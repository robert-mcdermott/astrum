//! A pane: one shell running on a PTY plus the terminal emulation state that
//! mirrors its screen.

use crate::osc::{self, Mark, Scanner};
use crate::server::Ev;
use alacritty_terminal::event::{Event as TermEvent, EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Direction, Line, Point};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config as TermConfig, Term, TermMode, viewport_to_point};
use alacritty_terminal::vte::ansi::{Processor, Rgb};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::path::Path;
use std::sync::mpsc as std_mpsc;
use std::time::Instant;
use tokio::sync::mpsc;

const MAX_BLOCKS: usize = 40;

struct Dims {
    cols: usize,
    lines: usize,
}

impl Dimensions for Dims {
    fn total_lines(&self) -> usize {
        self.lines
    }
    fn screen_lines(&self) -> usize {
        self.lines
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

#[derive(Clone)]
pub struct Listener(std_mpsc::Sender<TermEvent>);

impl EventListener for Listener {
    fn send_event(&self, event: TermEvent) {
        let _ = self.0.send(event);
    }
}

#[derive(Debug, Clone)]
pub enum ShellState {
    /// No shell integration seen (yet); we cannot tell what the shell is doing.
    Unknown,
    /// Sitting at a prompt.
    Prompt,
    /// Running a command.
    Running { cmd: Option<String>, since: Instant },
}

/// A finished command with its output, as recorded by shell integration.
#[derive(Debug, Clone)]
pub struct Block {
    pub cmd: String,
    pub exit: Option<i32>,
    pub output: String,
    pub cwd: Option<String>,
}

pub enum PaneEvent {
    Bell,
    Clipboard(String),
    CommandStarted,
    CommandFinished(Block),
    PromptReady,
}

pub struct Pane {
    pub term: Term<Listener>,
    parser: Processor,
    scanner: Scanner,
    master: Box<dyn MasterPty + Send>,
    writer: std_mpsc::Sender<Vec<u8>>,
    killer: Box<dyn ChildKiller + Send + Sync>,
    term_events: std_mpsc::Receiver<TermEvent>,
    pub shell_name: String,
    pub osc_title: Option<String>,
    pub cwd: Option<String>,
    pub state: ShellState,
    pub integrated: bool,
    pub blocks: VecDeque<Block>,
    /// Total number of commands finished in this pane.
    pub block_seq: u64,
    pub cols: u16,
    pub rows: u16,
    pub default_fg: Rgb,
    pub default_bg: Rgb,
    /// What the cursor's line held when the user began typing at the current
    /// prompt, i.e. the prompt itself. Forgotten at each new prompt.
    prompt_text: Option<String>,
    /// When the shell or a program in it last wrote anything.
    pub last_output: Instant,
    /// The line typed at the current prompt and where the cursor is in it
    /// (a byte offset), for as long as that can be followed from the keys
    /// alone: typing, deleting and moving the cursor. `None` once something
    /// only the shell knows the effect of was pressed (history, completion).
    typed: Option<(String, usize)>,
    /// A line was sent off and the next prompt has not appeared yet.
    awaiting_prompt: bool,
    /// Keys were sent before the current prompt appeared; the shell may
    /// have put them on its line.
    typeahead: bool,
}

pub struct SpawnOpts<'a> {
    pub id: u64,
    pub cols: u16,
    pub rows: u16,
    pub cwd: Option<&'a str>,
    pub shell: Option<&'a str>,
    pub shell_dir: &'a Path,
    pub session: &'a str,
    pub scrollback: usize,
}

impl Pane {
    pub fn spawn(o: SpawnOpts, tx: mpsc::Sender<Ev>) -> anyhow::Result<Pane> {
        let (cols, rows) = (o.cols.max(2), o.rows.max(1));
        let pair = native_pty_system().openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })?;

        let shell = o
            .shell
            .map(str::to_string)
            .or_else(|| std::env::var("SHELL").ok().filter(|s| !s.is_empty()))
            .unwrap_or_else(|| "/bin/sh".to_string());
        let shell_name =
            Path::new(&shell).file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "sh".into());

        let mut cmd = CommandBuilder::new(&shell);
        match shell_name.as_str() {
            "zsh" => {
                if let Ok(orig) = std::env::var("ZDOTDIR") {
                    cmd.env("ASTRUM_ORIG_ZDOTDIR", orig);
                }
                cmd.env("ZDOTDIR", o.shell_dir.join("zsh"));
                cmd.arg("-l");
            }
            "bash" => {
                cmd.arg("--rcfile");
                cmd.arg(o.shell_dir.join("astrum.bash"));
            }
            "fish" => {
                cmd.arg("-l");
                cmd.arg("--init-command");
                cmd.arg(format!("source '{}'", o.shell_dir.join("astrum.fish").display()));
            }
            _ => cmd.arg("-l"),
        }
        cmd.env("ASTRUM_SHELL_DIR", o.shell_dir);
        let nonce = random_hex();
        cmd.env("ASTRUM_NONCE", &nonce);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("TERM_PROGRAM", "astrum");
        cmd.env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));
        cmd.env("ASTRUM", "1");
        cmd.env("ASTRUM_SESSION", o.session);
        cmd.env("ASTRUM_TAB", o.id.to_string());
        // Don't let programs believe they talk to the host terminal directly.
        for var in [
            "ITERM_SESSION_ID",
            "LC_TERMINAL",
            "LC_TERMINAL_VERSION",
            "TERM_SESSION_ID",
            "WEZTERM_PANE",
            "KITTY_WINDOW_ID",
            "KITTY_PID",
            "TMUX",
            "TMUX_PANE",
            "STY",
            "ZELLIJ",
            "ZELLIJ_SESSION_NAME",
            "WT_SESSION",
            "COLUMNS",
            "LINES",
        ] {
            cmd.env_remove(var);
        }
        let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
        let cwd = o.cwd.filter(|d| Path::new(d).is_dir()).unwrap_or(&home);
        cmd.cwd(cwd);
        // Keep the logical path ($PWD) the shell shows consistent with where we start it.
        cmd.env("PWD", cwd);

        let mut child = pair.slave.spawn_command(cmd)?;
        drop(pair.slave);
        let killer = child.clone_killer();
        let master = pair.master;
        let id = o.id;

        // Reader: PTY -> server loop. The bounded channel gives backpressure,
        // so a flood of output cannot outrun parsing.
        let mut reader = master.try_clone_reader()?;
        let tx_out = tx.clone();
        std::thread::Builder::new().name(format!("pty-read-{id}")).spawn(move || {
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if tx_out.blocking_send(Ev::Pty { pane: id, data: buf[..n].to_vec() }).is_err() {
                            break;
                        }
                    }
                }
            }
        })?;

        // Writer: a blocked child must never stall the server loop.
        let mut pty_writer = master.take_writer()?;
        let (wtx, wrx) = std_mpsc::channel::<Vec<u8>>();
        std::thread::Builder::new().name(format!("pty-write-{id}")).spawn(move || {
            while let Ok(bytes) = wrx.recv() {
                if pty_writer.write_all(&bytes).is_err() || pty_writer.flush().is_err() {
                    break;
                }
            }
        })?;

        let tx_exit = tx;
        std::thread::Builder::new().name(format!("pty-wait-{id}")).spawn(move || {
            let _ = child.wait();
            let _ = tx_exit.blocking_send(Ev::PtyExit { pane: id });
        })?;

        let (etx, erx) = std_mpsc::channel();
        let config = TermConfig { scrolling_history: o.scrollback, ..TermConfig::default() };
        let term = Term::new(config, &Dims { cols: cols as usize, lines: rows as usize }, Listener(etx));

        Ok(Pane {
            term,
            parser: Processor::new(),
            scanner: Scanner::with_nonce(nonce),
            master,
            writer: wtx,
            killer,
            term_events: erx,
            shell_name,
            osc_title: None,
            cwd: Some(cwd.to_string()),
            state: ShellState::Unknown,
            integrated: false,
            blocks: VecDeque::new(),
            block_seq: 0,
            cols,
            rows,
            default_fg: Rgb { r: 0xd8, g: 0xd8, b: 0xe0 },
            default_bg: Rgb { r: 0x12, g: 0x12, b: 0x1a },
            prompt_text: None,
            last_output: Instant::now(),
            typed: None,
            awaiting_prompt: false,
            typeahead: false,
        })
    }

    pub fn write(&self, bytes: impl Into<Vec<u8>>) {
        let _ = self.writer.send(bytes.into());
    }

    pub fn mode(&self) -> TermMode {
        *self.term.mode()
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        let (cols, rows) = (cols.max(2), rows.max(1));
        if cols == self.cols && rows == self.rows {
            return;
        }
        self.cols = cols;
        self.rows = rows;
        self.term.resize(Dims { cols: cols as usize, lines: rows as usize });
        let _ = self.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
    }

    /// Feed PTY output through the emulator and the shell-integration scanner.
    pub fn feed(&mut self, data: &[u8], out: &mut Vec<PaneEvent>) {
        self.last_output = Instant::now();
        self.parser.advance(&mut self.term, data);

        let mut marks = Vec::new();
        self.scanner.feed(data, &mut marks);
        for mark in marks {
            self.integrated = true;
            match mark {
                Mark::PromptStart => {
                    self.prompt_text = None;
                    self.typed = (!std::mem::take(&mut self.typeahead)).then(|| (String::new(), 0));
                    self.awaiting_prompt = false;
                    self.state = ShellState::Prompt;
                    out.push(PaneEvent::PromptReady);
                }
                Mark::OutputStart { cmd } => {
                    self.prompt_text = None;
                    self.state = ShellState::Running { cmd, since: Instant::now() };
                    out.push(PaneEvent::CommandStarted);
                }
                Mark::CmdEnd { exit, output } => {
                    if let ShellState::Running { cmd, .. } = &self.state {
                        let block = Block {
                            cmd: cmd.clone().unwrap_or_default(),
                            exit,
                            output: output.map(|c| c.text()).unwrap_or_default(),
                            cwd: self.cwd.clone(),
                        };
                        let mut stored = block.clone();
                        stored.output = osc::truncate_lines(&stored.output, 30, 30);
                        if self.blocks.len() >= MAX_BLOCKS {
                            self.blocks.pop_front();
                        }
                        self.blocks.push_back(stored);
                        self.block_seq += 1;
                        out.push(PaneEvent::CommandFinished(block));
                    }
                    self.state = ShellState::Prompt;
                }
                Mark::Cwd(path) => self.cwd = Some(path),
            }
        }

        while let Ok(ev) = self.term_events.try_recv() {
            match ev {
                TermEvent::PtyWrite(text) => self.write(text.into_bytes()),
                TermEvent::Title(t) => self.osc_title = Some(t),
                TermEvent::ResetTitle => self.osc_title = None,
                TermEvent::ClipboardStore(_, text) => out.push(PaneEvent::Clipboard(text)),
                TermEvent::ColorRequest(index, fmt) => {
                    let color = self.term.colors()[index].unwrap_or(match index {
                        256 | 258 => self.default_fg,
                        257 => self.default_bg,
                        _ => self.default_fg,
                    });
                    self.write(fmt(color).into_bytes());
                }
                TermEvent::TextAreaSizeRequest(fmt) => {
                    let size = WindowSize {
                        num_lines: self.rows,
                        num_cols: self.cols,
                        cell_width: 8,
                        cell_height: 16,
                    };
                    self.write(fmt(size).into_bytes());
                }
                TermEvent::Bell => out.push(PaneEvent::Bell),
                _ => {}
            }
        }
    }

    /// A program can open a synchronized update and never close it; flush the
    /// buffered bytes once the emulator's timeout has passed.
    pub fn flush_stale_sync(&mut self) -> bool {
        if self.parser.sync_bytes_count() == 0 {
            return false;
        }
        match self.parser.sync_timeout().sync_timeout() {
            Some(deadline) if deadline <= Instant::now() => {
                self.parser.stop_sync(&mut self.term);
                true
            }
            _ => false,
        }
    }

    pub fn sync_pending(&self) -> bool {
        self.parser.sync_bytes_count() > 0
    }

    /// Output captured so far from the command that is still running.
    pub fn partial_output(&self) -> String {
        if self.mode().contains(TermMode::ALT_SCREEN) {
            return self.recent_text(self.rows as usize);
        }
        self.scanner.capture.as_ref().map(|c| c.text()).unwrap_or_default()
    }

    pub fn is_busy(&self) -> bool {
        matches!(self.state, ShellState::Running { .. })
    }

    pub fn running_cmd(&self) -> Option<&str> {
        match &self.state {
            ShellState::Running { cmd, .. } => cmd.as_deref(),
            _ => None,
        }
    }

    // ---- scrollback ---------------------------------------------------------

    pub fn scroll(&mut self, delta: i32) {
        self.term.scroll_display(Scroll::Delta(delta));
    }

    pub fn scroll_to_bottom(&mut self) {
        if self.term.grid().display_offset() != 0 {
            self.term.scroll_display(Scroll::Bottom);
        }
    }

    pub fn display_offset(&self) -> usize {
        self.term.grid().display_offset()
    }

    // ---- selection ----------------------------------------------------------

    fn point_at(&self, col: u16, row: u16) -> Point {
        let col = (col as usize).min(self.cols as usize - 1);
        let row = (row as usize).min(self.rows as usize - 1);
        viewport_to_point(self.display_offset(), Point::new(row, Column(col)))
    }

    pub fn start_selection(&mut self, col: u16, row: u16, ty: SelectionType) {
        let p = self.point_at(col, row);
        self.term.selection = Some(Selection::new(ty, p, Direction::Left));
    }

    pub fn update_selection(&mut self, col: u16, row: u16) {
        let p = self.point_at(col, row);
        if let Some(sel) = &mut self.term.selection {
            sel.update(p, Direction::Right);
        }
    }

    pub fn clear_selection(&mut self) -> bool {
        self.term.selection.take().is_some()
    }

    pub fn selection_text(&self) -> Option<String> {
        self.term.selection_to_string().filter(|s| !s.is_empty())
    }

    // ---- the line being typed -------------------------------------------------

    /// The cursor's line from its start to the cursor, following it back
    /// over rows it has wrapped onto.
    fn line_to_cursor(&self) -> String {
        let grid = self.term.grid();
        let cursor = grid.cursor.point;
        let last = grid.last_column();
        let mut first = cursor.line;
        while first > grid.topmost_line() && grid[Line(first.0 - 1)][last].flags.contains(Flags::WRAPLINE) {
            first = Line(first.0 - 1);
        }
        let mut text = String::new();
        let mut line = first;
        while line <= cursor.line {
            // A cursor waiting to wrap sits on the last character, not after it.
            let end = if line < cursor.line || grid.cursor.input_needs_wrap { last.0 + 1 } else { cursor.column.0 };
            for col in 0..end {
                let cell = &grid[line][Column(col)];
                if !cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                    text.push(if cell.c == '\0' { ' ' } else { cell.c });
                }
            }
            line = Line(line.0 + 1);
        }
        text
    }

    /// Is a pager (less, more, man) sitting on its prompt, waiting for a
    /// key? Told by what the cursor's line says, the way their prompts do.
    pub fn pager_waiting(&self) -> bool {
        let line = self.line_to_cursor();
        let line = line.trim();
        line == ":" || line.ends_with("(END)") || line.contains("--More--") || line.starts_with("Manual page ") || {
            // "lines 1-24", "lines 1-24/310 7%"
            line.strip_prefix("lines ").is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()) && rest.contains('-'))
        }
    }

    /// The process group in the terminal's foreground: the shell at a
    /// prompt, otherwise the job it is running.
    pub fn foreground_group(&self) -> Option<i32> {
        self.master.process_group_leader()
    }

    fn at_prompt(&self) -> bool {
        self.integrated && matches!(self.state, ShellState::Prompt) && !self.mode().contains(TermMode::ALT_SCREEN)
    }

    /// The first time something is typed at a prompt, the cursor's line
    /// still holds only the prompt: remember it, so what follows it on the
    /// screen can be told apart later.
    fn note_typing(&mut self) {
        if self.prompt_text.is_none() && self.display_offset() == 0 {
            self.prompt_text = Some(self.line_to_cursor());
        }
    }

    /// Call before passing a key to the shell, to keep track of the line
    /// being typed at the prompt.
    pub fn note_key(&mut self, key: &KeyEvent) {
        if !self.at_prompt() || self.awaiting_prompt {
            self.typeahead = true;
            return;
        }
        self.note_typing();
        let ctrl = key.modifiers == KeyModifiers::CONTROL;
        let plain = !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
        let Some((text, cursor)) = &mut self.typed else {
            // Ctrl+C leaves an empty line whatever was on it; Enter sends it off.
            match key.code {
                KeyCode::Char('c') if ctrl => self.typed = Some((String::new(), 0)),
                KeyCode::Enter => self.awaiting_prompt = true,
                _ => {}
            }
            return;
        };
        let before = |text: &str, at: usize| text[..at].chars().next_back().map(char::len_utf8).unwrap_or(0);
        let after = |text: &str, at: usize| text[at..].chars().next().map(char::len_utf8).unwrap_or(0);
        let at_end = *cursor == text.len();
        match key.code {
            KeyCode::Char(c) if plain => {
                text.insert(*cursor, c);
                *cursor += c.len_utf8();
            }
            KeyCode::Backspace if plain => {
                let n = before(text, *cursor);
                text.drain(*cursor - n..*cursor);
                *cursor -= n;
            }
            KeyCode::Delete if plain => {
                let n = after(text, *cursor);
                text.drain(*cursor..*cursor + n);
            }
            KeyCode::Left if key.modifiers.is_empty() => *cursor -= before(text, *cursor),
            KeyCode::Home if key.modifiers.is_empty() => *cursor = 0,
            KeyCode::Char('a') if ctrl => *cursor = 0,
            // At the end of the line these accept a suggestion in shells
            // that offer them, which changes the line in a way not known here.
            KeyCode::Right if key.modifiers.is_empty() && !at_end => *cursor += after(text, *cursor),
            KeyCode::End if key.modifiers.is_empty() && !at_end => *cursor = text.len(),
            KeyCode::Char('e') if ctrl && !at_end => *cursor = text.len(),
            // Leaves an empty line, whatever was on it.
            KeyCode::Char('c') if ctrl => self.typed = Some((String::new(), 0)),
            KeyCode::Enter => {
                self.typed = None;
                self.awaiting_prompt = true;
            }
            _ => self.typed = None,
        }
    }

    /// Call before passing pasted text to the shell.
    pub fn note_paste(&mut self, text: &str) {
        if !self.at_prompt() || self.awaiting_prompt {
            self.typeahead = true;
            return;
        }
        self.note_typing();
        match &mut self.typed {
            Some((typed, cursor)) if !text.contains(char::is_control) => {
                typed.insert_str(*cursor, text);
                *cursor += text.len();
            }
            _ => self.typed = None,
        }
    }

    /// The keys that make the shell's line editor erase the line being
    /// typed: Ctrl+U erases back to the start of the line, preceded by
    /// Ctrl+E (to the end of the line) when the cursor is known to be
    /// somewhere in the middle.
    pub fn erase_line_keys(&self) -> Vec<u8> {
        match &self.typed {
            Some((text, cursor)) if *cursor < text.len() => vec![0x05, 0x15],
            _ => vec![0x15],
        }
    }

    /// The line at the prompt was erased on the user's behalf.
    pub fn line_abandoned(&mut self) {
        self.typed = Some((String::new(), 0));
        self.awaiting_prompt = false;
    }

    /// Is there anything on the cursor's row after the cursor, or does the
    /// row run on to the next?
    fn text_follows_cursor(&self) -> bool {
        let grid = self.term.grid();
        let cursor = grid.cursor.point;
        let row = &grid[cursor.line];
        let last = grid.last_column();
        let from = if grid.cursor.input_needs_wrap { last.0 + 1 } else { cursor.column.0 };
        (from..=last.0).any(|col| !matches!(row[Column(col)].c, ' ' | '\0')) || row[last].flags.contains(Flags::WRAPLINE)
    }

    /// What the user has typed at the prompt. A line that was only typed,
    /// corrected and moved about in is known exactly from the keys. One the
    /// shell had a hand in (history, completion) is read from the screen,
    /// from the end of the prompt to the cursor, and only when the cursor is
    /// at the end of what is on its row: text after it could be more of the
    /// line, a suggestion or a right-hand prompt, and there is no telling
    /// which. `None` whenever the line cannot be told for certain: no shell
    /// integration, a program running, a prompt that has changed since they
    /// began typing, a continuation line.
    pub fn typed_line(&self) -> Option<String> {
        if !self.at_prompt() || self.awaiting_prompt {
            return None;
        }
        if let Some((typed, _)) = &self.typed {
            return Some(typed.clone());
        }
        if self.display_offset() != 0 || self.text_follows_cursor() {
            return None;
        }
        let line = self.line_to_cursor();
        line.strip_prefix(self.prompt_text.as_deref()?).map(|typed| typed.trim_end().to_string())
    }

    // ---- text extraction ----------------------------------------------------

    /// The last `max_lines` lines of screen + scrollback as plain text.
    pub fn recent_text(&self, max_lines: usize) -> String {
        let grid = self.term.grid();
        let bottom = grid.bottommost_line();
        let top = grid.topmost_line();
        let start_line = Line((bottom.0 - max_lines as i32 + 1).max(top.0));
        let start = Point::new(start_line, Column(0));
        let end = Point::new(bottom, grid.last_column());
        let text = self.term.bounds_to_string(start, end);
        let trimmed: Vec<&str> = text.lines().map(|l| l.trim_end()).collect();
        trimmed.join("\n").trim_end().to_string()
    }
}

fn random_hex() -> String {
    let mut bytes = [0u8; 12];
    let ok = std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut bytes)).is_ok();
    if !ok {
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        bytes[..8].copy_from_slice(&(t as u64 ^ std::process::id() as u64).to_le_bytes());
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

impl Drop for Pane {
    fn drop(&mut self) {
        let _ = self.killer.kill();
    }
}
