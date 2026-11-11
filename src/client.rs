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

/// Put text on the clipboard of the machine this client runs on: OSC 52 for
/// the host terminal (works over SSH), plus the local clipboard tool if any.
fn set_clipboard(text: &str) {
    let encoded = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    let mut out = std::io::stdout();
    let _ = out.write_all(format!("\x1b]52;c;{encoded}\x07").as_bytes());
    let _ = out.flush();
    let tools: &[(&str, &[&str])] =
        if cfg!(target_os = "macos") { &[("pbcopy", &[])] } else { &[("wl-copy", &[]), ("xclip", &["-selection", "clipboard"])] };
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
    proto::send_client_msg(&mut stream, &ClientMsg::Hello(Hello { version: proto::VERSION, cols, rows, cwd, control: false }))?;

    let guard = TerminalGuard::enter()?;
    let keyboard = guard.keyboard;
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
    while !done.load(Ordering::SeqCst) {
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
    let _ = stream.shutdown(std::net::Shutdown::Both);
    let _ = reader_thread.join();
    drop(guard);
    let _ = std::panic::take_hook();

    let reason = reason.lock().unwrap().clone();
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
    proto::send_client_msg(&mut stream, &ClientMsg::Hello(Hello { version: proto::VERSION, cols: 80, rows: 24, cwd: None, control: true }))
        .ok()?;
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
    let hello = ClientMsg::Hello(Hello { version: proto::VERSION, cols: 80, rows: 24, cwd: None, control: true });
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
