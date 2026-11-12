//! A headless client for testing: attaches to a session without a terminal,
//! plays scripted input, and prints what the screen would show. It runs the
//! server's output through a terminal emulator of its own, so it sees exactly
//! what a real terminal would.

use crate::client;
use crate::proto::{self, ClientMsg, Hello, ServerFrame, ServerMsg};
use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config as TermConfig, Term};
use alacritty_terminal::vte::ansi::{Color, NamedColor, Processor};
use anyhow::{Result, bail};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex};
use std::time::Duration;

struct Dims(usize, usize);

impl Dimensions for Dims {
    fn total_lines(&self) -> usize {
        self.1
    }
    fn screen_lines(&self) -> usize {
        self.1
    }
    fn columns(&self) -> usize {
        self.0
    }
}

struct Screen {
    term: Term<VoidListener>,
    parser: Processor,
    clipboard: Vec<String>,
    /// Links the server asked this client to open.
    opened: Vec<String>,
    bye: Option<String>,
}

fn parse_key(spec: &str) -> Result<KeyEvent> {
    let mut mods = KeyModifiers::NONE;
    let mut code = None;
    for part in spec.split('+') {
        match part.to_ascii_lowercase().as_str() {
            "ctrl" => mods |= KeyModifiers::CONTROL,
            "alt" => mods |= KeyModifiers::ALT,
            "shift" => mods |= KeyModifiers::SHIFT,
            "enter" => code = Some(KeyCode::Enter),
            "esc" => code = Some(KeyCode::Esc),
            "tab" => code = Some(KeyCode::Tab),
            "backtab" => code = Some(KeyCode::BackTab),
            "backspace" => code = Some(KeyCode::Backspace),
            "delete" => code = Some(KeyCode::Delete),
            "up" => code = Some(KeyCode::Up),
            "down" => code = Some(KeyCode::Down),
            "left" => code = Some(KeyCode::Left),
            "right" => code = Some(KeyCode::Right),
            "home" => code = Some(KeyCode::Home),
            "end" => code = Some(KeyCode::End),
            "pageup" => code = Some(KeyCode::PageUp),
            "pagedown" => code = Some(KeyCode::PageDown),
            "space" => code = Some(KeyCode::Char(' ')),
            "plus" => code = Some(KeyCode::Char('+')),
            p if p.chars().count() == 1 => code = Some(KeyCode::Char(part.chars().next().unwrap())),
            other => bail!("unknown key `{other}`"),
        }
    }
    match code {
        Some(code) => Ok(KeyEvent::new(code, mods)),
        None => bail!("no key in `{spec}`"),
    }
}

fn send(stream: &mut UnixStream, ev: Event) -> Result<()> {
    proto::send_client_msg(stream, &ClientMsg::Event(ev))?;
    Ok(())
}

fn mouse(kind: MouseEventKind, col: u16, row: u16) -> Event {
    Event::Mouse(MouseEvent { kind, column: col, row, modifiers: KeyModifiers::NONE })
}

fn coords(arg: &str) -> Result<(u16, u16)> {
    let (c, r) = arg.split_once(',').ok_or_else(|| anyhow::anyhow!("expected COL,ROW"))?;
    Ok((c.trim().parse()?, r.trim().parse()?))
}

fn dump_text(screen: &Screen) -> String {
    let term = &screen.term;
    let cols = term.columns();
    let lines = term.screen_lines();
    let mut rows = vec![vec![' '; cols]; lines];
    for cell in term.grid().display_iter() {
        let (r, c) = (cell.point.line.0, cell.point.column.0);
        if r >= 0 && (r as usize) < lines && c < cols && !cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
            rows[r as usize][c] = if cell.c == '\0' { ' ' } else { cell.c };
        }
    }
    let mut out = String::new();
    for row in rows {
        let line: String = row.into_iter().collect();
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

fn css(c: Color, default: &str) -> String {
    const BASE: [&str; 16] = [
        "#1d1f21", "#cc6666", "#b5bd68", "#f0c674", "#81a2be", "#b294bb", "#8abeb7", "#c5c8c6", "#666666", "#d54e53",
        "#b9ca4a", "#e7c547", "#7aa6da", "#c397d8", "#70c0b1", "#eaeaea",
    ];
    let indexed = |i: u8| -> String {
        match i {
            0..=15 => BASE[i as usize].to_string(),
            16..=231 => {
                let i = i - 16;
                let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
                format!("#{:02x}{:02x}{:02x}", level(i / 36), level((i / 6) % 6), level(i % 6))
            }
            _ => {
                let v = 8 + (i - 232) * 10;
                format!("#{v:02x}{v:02x}{v:02x}")
            }
        }
    };
    match c {
        Color::Spec(rgb) => format!("#{:02x}{:02x}{:02x}", rgb.r, rgb.g, rgb.b),
        Color::Indexed(i) => indexed(i),
        Color::Named(n) => match n {
            NamedColor::Foreground | NamedColor::Background | NamedColor::Cursor => default.to_string(),
            other => {
                let i = other as usize;
                if i < 16 { BASE[i].to_string() } else { default.to_string() }
            }
        },
    }
}

/// Render the screen as an HTML page so colors and layout can be inspected.
fn dump_html(screen: &Screen) -> String {
    let term = &screen.term;
    let cols = term.columns();
    let lines = term.screen_lines();
    let (def_fg, def_bg) = ("#d0d0d8", "#14141c");
    let mut rows: Vec<String> = vec![String::new(); lines];
    for cell in term.grid().display_iter() {
        let (r, c) = (cell.point.line.0, cell.point.column.0);
        if r < 0 || r as usize >= lines || c >= cols || cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
            continue;
        }
        let (mut fg, mut bg) = (css(cell.fg, def_fg), css(cell.bg, def_bg));
        if cell.flags.contains(Flags::INVERSE) {
            std::mem::swap(&mut fg, &mut bg);
        }
        let mut style = format!("color:{fg};background:{bg};");
        if cell.flags.contains(Flags::BOLD) {
            style.push_str("font-weight:700;");
        }
        if cell.flags.contains(Flags::ITALIC) {
            style.push_str("font-style:italic;");
        }
        if cell.flags.intersects(Flags::ALL_UNDERLINES) {
            style.push_str("text-decoration:underline;");
        }
        if cell.flags.contains(Flags::DIM) {
            style.push_str("opacity:.6;");
        }
        let ch = match cell.c {
            '\0' | ' ' => "&nbsp;".to_string(),
            '<' => "&lt;".to_string(),
            '>' => "&gt;".to_string(),
            '&' => "&amp;".to_string(),
            c => c.to_string(),
        };
        rows[r as usize].push_str(&format!("<span style=\"{style}\">{ch}</span>"));
    }
    let cursor = term.grid().cursor.point;
    format!(
        "<!doctype html><meta charset=utf-8><title>astrum snapshot</title><style>body{{background:#000;margin:16px}}\
         pre{{font:14px/1.25 Menlo,'SF Mono',monospace;background:{def_bg};color:{def_fg};display:inline-block;margin:0}}\
         span{{display:inline-block;width:1ch;height:1.25em;vertical-align:top;overflow:visible;white-space:pre}}</style>\
         <pre>{}</pre><p style=\"color:#888;font:12px monospace\">{}x{} · cursor at col {}, row {}</p>",
        rows.join("\n"),
        cols,
        lines,
        cursor.column.0,
        cursor.line.0
    )
}

pub struct Opts {
    pub session: String,
    pub cols: u16,
    pub rows: u16,
    pub actions: Vec<String>,
    pub wait_ms: u64,
    pub html: Option<String>,
}

pub fn run(o: Opts) -> Result<()> {
    let mut stream = client::connect_or_spawn(&o.session)?;
    let cwd = std::env::current_dir().ok().map(|p| p.to_string_lossy().into_owned());
    proto::send_client_msg(
        &mut stream,
        &ClientMsg::Hello(Hello { version: proto::VERSION, cols: o.cols, rows: o.rows, cwd, control: false, truecolor: Some(true), fg: None, bg: None, opens_links: true, scripted: true, images: Default::default(), cell_px: (0, 0), notify: None }),
    )?;
    let screen = Arc::new(Mutex::new(Screen {
        term: Term::new(TermConfig::default(), &Dims(o.cols as usize, o.rows as usize), VoidListener),
        parser: Processor::new(),
        clipboard: Vec::new(),
        opened: Vec::new(),
        bye: None,
    }));
    let mut reader = stream.try_clone()?;
    let shared = screen.clone();
    std::thread::spawn(move || {
        loop {
            match proto::read_server_frame(&mut reader) {
                Ok(ServerFrame::Output(bytes)) => {
                    let mut s = shared.lock().unwrap();
                    let s = &mut *s;
                    s.parser.advance(&mut s.term, &bytes);
                }
                Ok(ServerFrame::Msg(ServerMsg::Clipboard(t))) => shared.lock().unwrap().clipboard.push(t),
                Ok(ServerFrame::Msg(ServerMsg::OpenUrl(url))) => shared.lock().unwrap().opened.push(url),
                Ok(ServerFrame::Msg(ServerMsg::Bye { reason })) => {
                    shared.lock().unwrap().bye = Some(reason);
                    break;
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
    });

    let pause = |ms: u64| std::thread::sleep(Duration::from_millis(ms));
    pause(o.wait_ms);
    for action in &o.actions {
        let (kind, arg) = action.split_once(':').unwrap_or((action.as_str(), ""));
        match kind {
            "text" => {
                for c in arg.chars() {
                    send(&mut stream, Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)))?;
                }
            }
            "line" => {
                for c in arg.chars() {
                    send(&mut stream, Event::Key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE)))?;
                }
                send(&mut stream, Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)))?;
            }
            "key" => send(&mut stream, Event::Key(parse_key(arg)?))?,
            "paste" => send(&mut stream, Event::Paste(arg.to_string()))?,
            "click" => {
                let (c, r) = coords(arg)?;
                send(&mut stream, mouse(MouseEventKind::Down(MouseButton::Left), c, r))?;
                send(&mut stream, mouse(MouseEventKind::Up(MouseButton::Left), c, r))?;
            }
            "drag" => {
                let (from, to) = arg.split_once('-').ok_or_else(|| anyhow::anyhow!("expected C,R-C,R"))?;
                let ((c1, r1), (c2, r2)) = (coords(from)?, coords(to)?);
                send(&mut stream, mouse(MouseEventKind::Down(MouseButton::Left), c1, r1))?;
                send(&mut stream, mouse(MouseEventKind::Drag(MouseButton::Left), c2, r2))?;
                send(&mut stream, mouse(MouseEventKind::Up(MouseButton::Left), c2, r2))?;
            }
            "move" => {
                let (c, r) = coords(arg)?;
                send(&mut stream, mouse(MouseEventKind::Moved, c, r))?;
            }
            "scrollup" | "scrolldown" => {
                let (c, r) = coords(arg)?;
                let k = if kind == "scrollup" { MouseEventKind::ScrollUp } else { MouseEventKind::ScrollDown };
                send(&mut stream, mouse(k, c, r))?;
            }
            "resize" => {
                let (c, r) = arg.split_once('x').ok_or_else(|| anyhow::anyhow!("expected COLSxROWS"))?;
                let (c, r): (u16, u16) = (c.parse()?, r.parse()?);
                screen.lock().unwrap().term.resize(Dims(c as usize, r as usize));
                send(&mut stream, Event::Resize(c, r))?;
            }
            "wait" => pause(arg.parse()?),
            // Wait until the screen contains some text (or give up after 60s).
            "until" => {
                let deadline = std::time::Instant::now() + Duration::from_secs(60);
                while !dump_text(&screen.lock().unwrap()).contains(arg) {
                    if std::time::Instant::now() > deadline {
                        println!("--- gave up waiting for `{arg}` ---");
                        break;
                    }
                    pause(100);
                }
            }
            "snap" => {
                pause(150);
                println!("--- {arg} ---\n{}", dump_text(&screen.lock().unwrap()));
            }
            other => bail!("unknown action `{other}`"),
        }
        pause(30);
    }
    pause(o.wait_ms);
    let s = screen.lock().unwrap();
    print!("{}", dump_text(&s));
    for url in &s.opened {
        println!("--- open: {url:?}");
    }
    for c in &s.clipboard {
        println!("--- clipboard: {c:?}");
    }
    if let Some(b) = &s.bye {
        println!("--- server said goodbye: {b}");
    }
    if let Some(path) = &o.html {
        std::fs::write(path, dump_html(&s))?;
    }
    Ok(())
}
