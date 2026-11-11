//! The thin client: puts the host terminal into raw mode, forwards input
//! events to the session server and writes the frames it sends back.

use crate::paths;
use crate::proto::{self, ClientMsg, Hello, ServerFrame, ServerMsg, SessionInfo};
use anyhow::{Context, Result, bail};
use base64::Engine;
use crossterm::event::{
    DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste, EnableFocusChange,
    EnableMouseCapture, KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{execute, terminal};
use std::io::{IsTerminal, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Connect to a running session server, if there is one.
pub fn connect(session: &str) -> Option<UnixStream> {
    let path = paths::socket_path(session).ok()?;
    UnixStream::connect(path).ok()
}

/// Start the session server as a detached background process.
fn spawn_server(session: &str) -> Result<()> {
    let exe = std::env::current_exe().context("locating the astrum executable")?;
    let log = std::fs::OpenOptions::new().create(true).append(true).open(paths::log_path(session)?)?;
    let mut cmd = Command::new(exe);
    cmd.arg("__server")
        .arg("--session")
        .arg(session)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    // Its own session: no controlling terminal, so closing this terminal
    // window cannot take the server (and your shells) down with it.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    cmd.spawn().context("starting the astrum session server")?;
    Ok(())
}

pub fn connect_or_spawn(session: &str) -> Result<UnixStream> {
    if let Some(s) = connect(session) {
        return Ok(s);
    }
    spawn_server(session)?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
        if let Some(s) = connect(session) {
            return Ok(s);
        }
    }
    bail!(
        "the session server did not come up; see {}",
        paths::log_path(session).map(|p| p.display().to_string()).unwrap_or_default()
    )
}

/// Set by the signal handler when the client is asked to go away.
static SIGNALLED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    SIGNALLED.store(true, Ordering::SeqCst);
}

/// Leave on SIGTERM / SIGHUP through the normal path, so the terminal is put
/// back the way it was instead of being left in raw mode with the mouse grabbed.
fn watch_signals() {
    let handler = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
    unsafe {
        libc::signal(libc::SIGTERM, handler);
        libc::signal(libc::SIGHUP, handler);
    }
}

struct TerminalGuard {
    keyboard: bool,
}

impl TerminalGuard {
    fn enter() -> Result<Self> {
        terminal::enable_raw_mode()?;
        let mut out = std::io::stdout();
        execute!(out, EnterAlternateScreen, EnableMouseCapture, EnableBracketedPaste, EnableFocusChange)?;
        // Where supported, ask for unambiguous key reports (lets us tell
        // Shift+Enter from Enter, Ctrl+I from Tab, and so on).
        let keyboard = matches!(terminal::supports_keyboard_enhancement(), Ok(true));
        if keyboard {
            execute!(out, PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES))?;
        }
        Ok(Self { keyboard })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal(self.keyboard);
    }
}

fn restore_terminal(keyboard: bool) {
    let mut out = std::io::stdout();
    if keyboard {
        let _ = execute!(out, PopKeyboardEnhancementFlags);
    }
    let _ = execute!(out, DisableFocusChange, DisableBracketedPaste, DisableMouseCapture, LeaveAlternateScreen);
    // Default cursor shape, visible cursor, default colors.
    let _ = out.write_all(b"\x1b[0 q\x1b[?25h\x1b[0m");
    let _ = out.flush();
    let _ = terminal::disable_raw_mode();
}

/// Is the terminal we were started in no longer there (its window closed,
/// the connection it came over dropped)?
fn terminal_gone() -> bool {
    let mut fds = libc::pollfd { fd: 0, events: libc::POLLIN, revents: 0 };
    // POLLNVAL is not a sign of anything: macOS reports it for /dev/tty,
    // which poll does not support there, on a terminal that is perfectly alive.
    let hung_up = unsafe { libc::poll(&mut fds, 1, 0) } > 0 && fds.revents & (libc::POLLHUP | libc::POLLERR) != 0;
    let mut settings: libc::termios = unsafe { std::mem::zeroed() };
    hung_up || unsafe { libc::tcgetattr(0, &mut settings) } != 0
}

/// Make sure the client leaves when it should, whatever the input loop is
/// doing. Reading keys from a terminal that has gone away returns at once,
/// every time, and the input library keeps reading: its call never comes
/// back, the loop around it never gets to check whether to stop, and the
/// process would sit there using a whole processor. So a second thread
/// watches for the reasons to leave (asked to by a signal, the server gone,
/// the terminal gone) and, if the loop has not left by itself shortly
/// after, ends the process.
fn watch_for_exit(done: Arc<AtomicBool>, left: Arc<AtomicBool>, keyboard: bool) {
    std::thread::spawn(move || {
        let leaving = || done.load(Ordering::SeqCst) || SIGNALLED.load(Ordering::SeqCst) || terminal_gone();
        while !leaving() {
            if left.load(Ordering::SeqCst) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        // The ordinary way out restores the terminal and says goodbye; give it a moment.
        for _ in 0..8 {
            std::thread::sleep(Duration::from_millis(50));
            if left.load(Ordering::SeqCst) {
                return;
            }
        }
        restore_terminal(keyboard);
        std::process::exit(0);
    });
}

/// Put text on the clipboard of the machine this client runs on: OSC 52 for
/// the host terminal (works over SSH), plus the local clipboard tool if any.
fn set_clipboard(text: &str) {
    let encoded = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    let mut out = std::io::stdout();
    let _ = out.write_all(format!("\x1b]52;c;{encoded}\x07").as_bytes());
    let _ = out.flush();
    // ASTRUM_CLIPBOARD_TOOL names another program to hand the text to, or
    // "none" to leave it to the terminal.
    let chosen = std::env::var("ASTRUM_CLIPBOARD_TOOL").ok().filter(|t| !t.is_empty());
    if chosen.as_deref() == Some("none") {
        return;
    }
    let chosen: Vec<(&str, &[&str])> = chosen.as_deref().map(|t| vec![(t, &[][..])]).unwrap_or_default();
    let usual: &[(&str, &[&str])] =
        if cfg!(target_os = "macos") { &[("pbcopy", &[])] } else { &[("wl-copy", &[]), ("xclip", &["-selection", "clipboard"])] };
    let tools = if chosen.is_empty() { usual } else { &chosen[..] };
    for (tool, args) in tools {
        let child = Command::new(tool).args(*args).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
        if let Ok(mut child) = child {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(text.as_bytes());
            }
            // Reap in the background; some tools linger to serve the selection.
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            break;
        }
    }
}

/// Does the terminal we are running in understand 24-bit color? Terminals
/// that do announce it; one that does not would show garbled colors if sent
/// such sequences, so the server falls back to the 256-color palette for it.
pub fn terminal_has_truecolor() -> bool {
    let var = |name: &str| std::env::var(name).unwrap_or_default().to_ascii_lowercase();
    let colorterm = var("COLORTERM");
    if colorterm.contains("truecolor") || colorterm.contains("24bit") {
        return true;
    }
    let term = var("TERM");
    if ["direct", "kitty", "ghostty", "alacritty", "wezterm", "foot", "contour"].iter().any(|t| term.contains(t)) {
        return true;
    }
    matches!(var("TERM_PROGRAM").as_str(), "iterm.app" | "wezterm" | "ghostty" | "vscode" | "hyper" | "rio" | "tabby")
}

/// One color out of a terminal's answer to "what is your color number
/// `which`?": `ESC ] which ; rgb:RRRR/GGGG/BBBB` and a terminator.
fn color_in(reply: &[u8], which: u8) -> Option<[u8; 3]> {
    let text = String::from_utf8_lossy(reply);
    let rest = &text[text.find(&format!("\x1b]{which};rgb:"))? + format!("\x1b]{which};rgb:").len()..];
    let end = rest.find(['\x07', '\x1b']).unwrap_or(rest.len());
    let mut parts = rest[..end].split('/');
    let mut channel = || {
        let hex = parts.next()?;
        // One to four hex digits, each a fraction of full.
        let value = u32::from_str_radix(hex, 16).ok()?;
        let full = (1u32 << (4 * hex.len().clamp(1, 4) as u32)) - 1;
        Some((value * 255 / full) as u8)
    };
    Some([channel()?, channel()?, channel()?])
}

/// Ask the terminal we are in for its text and background colors. Called
/// in raw mode, before anything else reads the keyboard. A terminal that
/// does not answer such questions still answers the last one asked (what
/// kind of terminal are you), which is how we know not to wait.
fn terminal_colors() -> (Option<[u8; 3]>, Option<[u8; 3]>) {
    let mut out = std::io::stdout();
    if out.write_all(b"\x1b]10;?\x07\x1b]11;?\x07\x1b[c").is_err() || out.flush().is_err() {
        return (None, None);
    }
    let mut reply: Vec<u8> = Vec::new();
    let deadline = Instant::now() + Duration::from_millis(400);
    loop {
        // The answer to the last question: ESC [ ? ... c
        if let Some(at) = reply.windows(3).position(|w| w == b"\x1b[?") {
            if reply[at..].contains(&b'c') {
                break;
            }
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        let mut fds = libc::pollfd { fd: 0, events: libc::POLLIN, revents: 0 };
        if unsafe { libc::poll(&mut fds, 1, left.as_millis() as libc::c_int) } <= 0 {
            break;
        }
        let mut buf = [0u8; 256];
        let n = unsafe { libc::read(0, buf.as_mut_ptr().cast(), buf.len()) };
        if n <= 0 {
            break;
        }
        reply.extend_from_slice(&buf[..n as usize]);
    }
    (color_in(&reply, 10), color_in(&reply, 11))
}

/// Attach to (or create) a session. Returns the message to show on exit.
pub fn attach(session: &str) -> Result<String> {
    if std::env::var_os("ASTRUM").is_some() && std::env::var_os("ASTRUM_ALLOW_NESTED").is_none() {
        bail!("you are already inside Astrum (use the prefix key, then c, for a new tab). Set ASTRUM_ALLOW_NESTED=1 to nest anyway.");
    }
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        bail!("astrum needs to run in a terminal");
    }
    let mut stream = connect_or_spawn(session)?;
    let (cols, rows) = terminal::size().unwrap_or((80, 24));
    let cwd = std::env::current_dir().ok().map(|p| p.to_string_lossy().into_owned());
    let guard = TerminalGuard::enter()?;
    let keyboard = guard.keyboard;
    let (fg, bg) = terminal_colors();
    let hello = Hello { version: proto::VERSION, cols, rows, cwd, control: false, truecolor: Some(terminal_has_truecolor()), fg, bg };
    proto::send_client_msg(&mut stream, &ClientMsg::Hello(hello))?;

    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal(keyboard);
        default_hook(info);
    }));

    let done = Arc::new(AtomicBool::new(false));
    let reason = Arc::new(Mutex::new(String::from("lost the connection to the session server")));

    // Server -> terminal.
    let mut reader = stream.try_clone()?;
    let (done_r, reason_r) = (done.clone(), reason.clone());
    let reader_thread = std::thread::spawn(move || {
        let mut out = std::io::stdout();
        loop {
            match proto::read_server_frame(&mut reader) {
                Ok(ServerFrame::Output(bytes)) => {
                    if out.write_all(&bytes).is_err() || out.flush().is_err() {
                        break;
                    }
                }
                Ok(ServerFrame::Msg(ServerMsg::Clipboard(text))) => set_clipboard(&text),
                Ok(ServerFrame::Msg(ServerMsg::Bye { reason })) => {
                    *reason_r.lock().unwrap() = reason;
                    break;
                }
                Ok(ServerFrame::Msg(_)) => {}
                Err(_) => break,
            }
        }
        done_r.store(true, Ordering::SeqCst);
    });

    // Terminal -> server.
    watch_signals();
    let left = Arc::new(AtomicBool::new(false));
    watch_for_exit(done.clone(), left.clone(), keyboard);
    while !done.load(Ordering::SeqCst) && !SIGNALLED.load(Ordering::SeqCst) {
        match crossterm::event::poll(Duration::from_millis(40)) {
            Ok(true) => {
                let Ok(ev) = crossterm::event::read() else { break };
                if proto::send_client_msg(&mut stream, &ClientMsg::Event(ev)).is_err() {
                    break;
                }
            }
            Ok(false) => {}
            Err(_) => break,
        }
    }
    left.store(true, Ordering::SeqCst);
    let _ = stream.shutdown(std::net::Shutdown::Both);
    let _ = reader_thread.join();
    drop(guard);
    let _ = std::panic::take_hook();

    let reason = if SIGNALLED.load(Ordering::SeqCst) { "detached".to_string() } else { reason.lock().unwrap().clone() };
    Ok(match reason.as_str() {
        "detached" => format!("[astrum: detached · session `{session}` keeps running · run `astrum` to return]"),
        "exited" => "[astrum: session ended]".to_string(),
        other => format!("[astrum: {other}]"),
    })
}

/// Ask a running session for its status.
pub fn info(session: &str) -> Option<SessionInfo> {
    let mut stream = connect(session)?;
    stream.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    let hello = Hello { version: proto::VERSION, cols: 80, rows: 24, cwd: None, control: true, truecolor: None, fg: None, bg: None };
    proto::send_client_msg(&mut stream, &ClientMsg::Hello(hello)).ok()?;
    proto::send_client_msg(&mut stream, &ClientMsg::Info).ok()?;
    loop {
        match proto::read_server_frame(&mut stream).ok()? {
            ServerFrame::Msg(ServerMsg::Info(info)) => return Some(info),
            ServerFrame::Msg(ServerMsg::Bye { .. }) => return None,
            _ => {}
        }
    }
}

/// Stop a session: its shells are closed. Returns false if it was not running.
pub fn kill(session: &str) -> bool {
    let Some(mut stream) = connect(session) else { return false };
    let hello = ClientMsg::Hello(Hello { version: proto::VERSION, cols: 80, rows: 24, cwd: None, control: true, truecolor: None, fg: None, bg: None });
    if proto::send_client_msg(&mut stream, &hello).is_err() || proto::send_client_msg(&mut stream, &ClientMsg::Kill).is_err() {
        return false;
    }
    // Wait for the server to go away.
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline && connect(session).is_some() {
        std::thread::sleep(Duration::from_millis(30));
    }
    true
}

/// Ask a running session to read its providers again, passing along the
/// key variables as this shell has them. Returns the session's report.
pub fn reload(session: &str) -> Result<String> {
    let Some(mut stream) = connect(session) else { bail!("session `{session}` is not running") };
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let (cfg, _) = crate::config::Config::load();
    let mut env: Vec<(String, String)> = Vec::new();
    for name in cfg.providers.values().filter_map(|p| p.api_key_env.clone()) {
        if !env.iter().any(|(known, _)| *known == name) {
            let value = std::env::var(&name).unwrap_or_default();
            env.push((name, value));
        }
    }
    let hello = Hello { version: proto::VERSION, cols: 80, rows: 24, cwd: None, control: true, truecolor: None, fg: None, bg: None };
    proto::send_client_msg(&mut stream, &ClientMsg::Hello(hello))?;
    proto::send_client_msg(&mut stream, &ClientMsg::Reload { env })?;
    loop {
        match proto::read_server_frame(&mut stream) {
            Ok(ServerFrame::Msg(ServerMsg::Bye { reason })) => return Ok(reason),
            Ok(_) => {}
            // A server from before this request existed drops the connection.
            Err(_) => bail!("session `{session}` did not answer: it was started by an older astrum. Restart it (`astrum kill -s {session}`, then `astrum -s {session}`)"),
        }
    }
}

/// Names of sessions that have a socket (running or stale).
pub fn session_names() -> Vec<String> {
    let Ok(dir) = paths::runtime_dir() else { return Vec::new() };
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().to_string_lossy().strip_suffix(".sock").map(str::to_string))
        .collect();
    names.sort();
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_terminals_color_answers_are_read() {
        // Four digits a channel, ended by a bell; two by ESC \; and both in one reply.
        assert_eq!(color_in(b"\x1b]11;rgb:ffff/8080/0000\x07", 11), Some([255, 128, 0]));
        assert_eq!(color_in(b"\x1b]10;rgb:ff/80/00\x1b\\", 10), Some([255, 128, 0]));
        assert_eq!(color_in(b"\x1b]10;rgb:f/0/8\x07", 10), Some([255, 0, 136]));
        let both = b"\x1b]10;rgb:1010/2020/3030\x07\x1b]11;rgb:fafa/f0f0/e6e6\x07\x1b[?62;c";
        assert_eq!(color_in(both, 10), Some([16, 32, 48]));
        assert_eq!(color_in(both, 11), Some([250, 240, 230]));
        // No answer, or one that is not a color.
        assert_eq!(color_in(b"\x1b[?62;c", 11), None);
        assert_eq!(color_in(b"\x1b]11;rgb:zz/00/00\x07", 11), None);
    }
}
