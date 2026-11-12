//! End-to-end tests: run the real `astrum` binary on a pseudo-terminal, type
//! at it the way a terminal app would (raw key bytes, SGR mouse reports,
//! window resizes), and check what ends up on screen.
//!
//! Every test gets its own HOME, config directory and runtime directory, so
//! nothing touches the developer's sessions or settings. The shell is bash
//! with an empty HOME, which gives a predictable prompt everywhere.

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config as TermConfig, Term};
use alacritty_terminal::vte::ansi::Processor;
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_astrum");
static COUNTER: AtomicUsize = AtomicUsize::new(0);

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

/// An isolated place for one test's sessions and settings.
struct Sandbox {
    root: PathBuf,
    runtime: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let tag = format!("{}-{n}", std::process::id());
        let root = std::env::temp_dir().join(format!("astrum-e2e-{tag}"));
        // Socket paths are short-limited, so the runtime dir lives in /tmp.
        let runtime = PathBuf::from(format!("/tmp/astrum-e2e-{tag}"));
        std::fs::create_dir_all(root.join("home")).unwrap();
        std::fs::create_dir_all(root.join("cfg")).unwrap();
        // A fixed prompt, whatever the machine's own bash setup looks like.
        std::fs::write(root.join("home").join(".bash_profile"), "PS1='ready$ '\n").unwrap();
        // Not a first run: the walkthrough is for the test that asks for it.
        std::fs::write(root.join("cfg").join("state.toml"), "welcomed = true\n").unwrap();
        Sandbox { root, runtime }
    }

    fn env(&self) -> Vec<(&'static str, String)> {
        vec![
            ("HOME", self.root.join("home").to_string_lossy().into_owned()),
            ("ASTRUM_CONFIG_DIR", self.root.join("cfg").to_string_lossy().into_owned()),
            ("ASTRUM_RUNTIME_DIR", self.runtime.to_string_lossy().into_owned()),
            // Stored keys go to a file here, never to the developer's real keychain.
            ("ASTRUM_KEYCHAIN_FILE", self.root.join("keychain.json").to_string_lossy().into_owned()),
            // What a test copies stays off the developer's real clipboard,
            // and a link a test follows does not open their browser.
            ("ASTRUM_CLIPBOARD_TOOL", "none".to_string()),
            ("ASTRUM_OPEN_TOOL", "none".to_string()),
            // And nothing a test does reaches the real service manager.
            ("ASTRUM_SERVICE_MANAGER", "none".to_string()),
            ("SHELL", "/bin/bash".to_string()),
            ("BASH_SILENCE_DEPRECATION_WARNING", "1".to_string()),
            ("TERM", "xterm-256color".to_string()),
        ]
    }

    fn write_config(&self, text: &str) {
        std::fs::write(self.root.join("cfg").join("config.toml"), text).unwrap();
    }

    /// Run a non-interactive astrum subcommand and return its output.
    fn run(&self, args: &[&str]) -> String {
        self.run_with(args, &[])
    }

    /// The same, with extra environment, as if run from a shell that has it.
    fn run_with(&self, args: &[&str], extra: &[(&str, &str)]) -> String {
        let mut cmd = Command::new(BIN);
        cmd.args(args).env_remove("ASTRUM").env_remove("XDG_CONFIG_HOME").env_remove("XDG_DATA_HOME");
        for (k, v) in self.env() {
            cmd.env(k, v);
        }
        // (Last, so that a test can set what is cleared above. The sandbox's
        // own settings are not for overriding: they keep tests off the real machine.)
        for (k, v) in extra.iter().copied().filter(|(k, _)| !["HOME", "ASTRUM_SERVICE_MANAGER", "ASTRUM_RUNTIME_DIR", "ASTRUM_CONFIG_DIR"].contains(k)) {
            cmd.env(k, v);
        }
        let out = cmd.output().unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr)
    }

    fn attach(&self, session: &str, cols: u16, rows: u16) -> Client {
        self.attach_env(session, cols, rows, &[])
    }

    /// Attach with extra environment for the client, e.g. what a particular
    /// terminal app would set.
    fn attach_env(&self, session: &str, cols: u16, rows: u16, extra: &[(&str, &str)]) -> Client {
        // Cells of 10 by 20 pixels, as far as anything that asks is told.
        let pair = native_pty_system().openpty(PtySize { rows, cols, pixel_width: cols * CELL_PX.0, pixel_height: rows * CELL_PX.1 }).unwrap();
        let mut cmd = CommandBuilder::new(BIN);
        cmd.arg("-s");
        cmd.arg(session);
        // Start from a plain 256-color terminal, whatever runs the tests.
        for var in ["ASTRUM", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "ZDOTDIR", "PROMPT_COMMAND", "COLORTERM", "TERM_PROGRAM", "LC_TERMINAL", "ASTRUM_IMAGES"] {
            cmd.env_remove(var);
        }
        for (k, v) in self.env() {
            cmd.env(k, v);
        }
        for (k, v) in extra {
            cmd.env(k, v);
        }
        cmd.cwd(self.root.join("home"));
        let child = pair.slave.spawn_command(cmd).unwrap();
        drop(pair.slave);

        let screen = Arc::new(Mutex::new(Screen {
            term: Term::new(TermConfig::default(), &Dims(cols as usize, rows as usize), VoidListener),
            parser: Processor::new(),
            raw: Vec::new(),
        }));
        let writer = Arc::new(Mutex::new(pair.master.take_writer().unwrap()));
        let mut reader = pair.master.try_clone_reader().unwrap();
        let (shared, answer) = (screen.clone(), writer.clone());
        // The background this "terminal" says it has: pale, unless the test
        // asks for another with TEST_TERMINAL_BG.
        let background = extra.iter().find(|(k, _)| *k == "TEST_TERMINAL_BG").map_or("fafa/f0f0/e6e6", |(_, v)| *v).to_string();
        // What this "terminal" says of pictures when it is asked: with
        // TEST_TERMINAL_SHOWS naming `kitty`, it answers a question put in
        // kitty's graphics; naming `sixel`, it lists that among what it is.
        let shows = extra.iter().find(|(k, _)| *k == "TEST_TERMINAL_SHOWS").map_or("", |(_, v)| *v).to_string();
        std::thread::spawn(move || {
            let mut buf = [0u8; 16384];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                let chunk = &buf[..n];
                // Answer the device-attributes query like a terminal would,
                // so the client's capability probe returns immediately.
                // (And say what colors this "terminal" has: dark text on a pale background.)
                if chunk.windows(6).any(|w| w == b"\x1b]10;?") {
                    let _ = answer.lock().unwrap().write_all(b"\x1b]10;rgb:1010/2020/3030\x07");
                }
                if chunk.windows(6).any(|w| w == b"\x1b]11;?") {
                    let _ = answer.lock().unwrap().write_all(format!("\x1b]11;rgb:{background}\x07").as_bytes());
                }
                if shows.contains("kitty") && chunk.windows(8).any(|w| w == b"\x1b_Gi=31,") {
                    let _ = answer.lock().unwrap().write_all(b"\x1b_Gi=31;OK\x1b\\");
                }
                if chunk.windows(3).any(|w| w == b"\x1b[c") {
                    let kind: &[u8] = if shows.contains("sixel") { b"\x1b[?62;4;22c" } else { b"\x1b[?62;c" };
                    let _ = answer.lock().unwrap().write_all(kind);
                }
                let mut s = shared.lock().unwrap();
                let s = &mut *s;
                s.raw.extend_from_slice(chunk);
                s.parser.advance(&mut s.term, chunk);
            }
        });
        Client { master: pair.master, writer, child, screen }
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = self.run(&["kill", "--all"]);
        let _ = std::fs::remove_dir_all(&self.root);
        let _ = std::fs::remove_dir_all(&self.runtime);
    }
}

struct Screen {
    term: Term<VoidListener>,
    parser: Processor,
    raw: Vec<u8>,
}

struct Client {
    master: Box<dyn MasterPty + Send>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    child: Box<dyn Child + Send + Sync>,
    screen: Arc<Mutex<Screen>>,
}

impl Client {
    fn send(&self, bytes: &[u8]) {
        let mut w = self.writer.lock().unwrap();
        w.write_all(bytes).unwrap();
        w.flush().unwrap();
    }

    fn lines(&self) -> Vec<String> {
        let s = self.screen.lock().unwrap();
        let (cols, rows) = (s.term.columns(), s.term.screen_lines());
        let mut grid = vec![vec![' '; cols]; rows];
        for cell in s.term.grid().display_iter() {
            let (r, c) = (cell.point.line.0, cell.point.column.0);
            if r >= 0 && (r as usize) < rows && c < cols && !cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                grid[r as usize][c] = if cell.c == '\0' { ' ' } else { cell.c };
            }
        }
        grid.into_iter().map(|row| row.into_iter().collect::<String>().trim_end().to_string()).collect()
    }

    fn text(&self) -> String {
        self.lines().join("\n")
    }

    /// Everything the client has written, including after it left the
    /// alternate screen (its goodbye line).
    fn raw(&self) -> String {
        String::from_utf8_lossy(&self.screen.lock().unwrap().raw).into_owned()
    }

    #[track_caller]
    fn wait_until(&self, what: &str, mut ok: impl FnMut(&Client) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if ok(self) {
                return;
            }
            std::thread::sleep(Duration::from_millis(40));
        }
        panic!("timed out waiting for {what}.\n--- screen ---\n{}\n--------------", self.text());
    }

    #[track_caller]
    fn wait_for(&self, needle: &str) {
        self.wait_until(&format!("`{needle}` on screen"), |c| c.text().contains(needle));
    }

    /// Click the left button on a cell (zero-based), as an SGR mouse report.
    fn click(&self, col: usize, row: usize) {
        self.send(format!("\x1b[<0;{};{}M\x1b[<0;{};{}m", col + 1, row + 1, col + 1, row + 1).as_bytes());
    }

    /// Click the right button on a cell.
    /// Move the pointer to a cell, no button held.
    fn hover(&self, col: usize, row: usize) {
        self.send(format!("\x1b[<35;{};{}M", col + 1, row + 1).as_bytes());
    }

    fn ctrl_click(&self, col: usize, row: usize) {
        self.send(format!("\x1b[<16;{};{}M", col + 1, row + 1).as_bytes());
        self.send(format!("\x1b[<16;{};{}m", col + 1, row + 1).as_bytes());
    }

    fn right_click(&self, col: usize, row: usize) {
        self.send(format!("\x1b[<2;{};{}M\x1b[<2;{};{}m", col + 1, row + 1, col + 1, row + 1).as_bytes());
    }

    /// Where `needle` first appears on screen: zero-based column and row.
    fn position_of(&self, needle: &str) -> Option<(usize, usize)> {
        self.lines().iter().enumerate().find_map(|(row, line)| line.find(needle).map(|byte| (line[..byte].chars().count(), row)))
    }

    /// Press the left button on one cell, move to another, and let go.
    fn drag(&self, from: (usize, usize), to: (usize, usize)) {
        self.send(format!("\x1b[<0;{};{}M", from.0 + 1, from.1 + 1).as_bytes());
        self.send(format!("\x1b[<32;{};{}M", to.0 + 1, to.1 + 1).as_bytes());
        self.send(format!("\x1b[<0;{};{}m", to.0 + 1, to.1 + 1).as_bytes());
    }

    /// The rightmost zero-based column at which `needle` appears on screen, on whichever row.
    fn column_of(&self, needle: &str) -> Option<usize> {
        self.lines().iter().filter_map(|line| line.rfind(needle).map(|byte| line[..byte].chars().count())).max()
    }

    /// The leftmost such column.
    fn first_column_of(&self, needle: &str) -> Option<usize> {
        self.lines().iter().filter_map(|line| line.find(needle).map(|byte| line[..byte].chars().count())).min()
    }

    /// Zero-based column of the first occurrence of `needle` on a row.
    fn find_on_row(&self, row: usize, needle: &str) -> Option<usize> {
        let line = self.lines().get(row)?.clone();
        line.find(needle).map(|byte| line[..byte].chars().count())
    }

    fn resize(&self, cols: u16, rows: u16) {
        self.screen.lock().unwrap().term.resize(Dims(cols as usize, rows as usize));
        self.master.resize(PtySize { rows, cols, pixel_width: cols * CELL_PX.0, pixel_height: rows * CELL_PX.1 }).unwrap();
    }

    /// Type `exit` at each shell until the session has ended and this
    /// client with it. Each one goes to whichever shell has the keyboard,
    /// and one typed while a shell is still on its way out is lost with it:
    /// so they are sent until there is nothing left, however long this
    /// machine takes over each.
    #[track_caller]
    fn close_every_shell(&mut self) {
        for _ in 0..40 {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                break;
            }
            let _ = self.writer.lock().unwrap().write_all(b"exit\r");
            std::thread::sleep(Duration::from_millis(400));
        }
        self.wait_exit();
    }

    #[track_caller]
    fn wait_exit(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                // Let the reader thread drain the goodbye line.
                std::thread::sleep(Duration::from_millis(150));
                return;
            }
            std::thread::sleep(Duration::from_millis(40));
        }
        panic!("the client did not exit.\n--- screen ---\n{}", self.text());
    }
}

/// The size of a cell of the test terminal, in pixels.
const CELL_PX: (u16, u16) = (10, 20);

const CTRL_B: &[u8] = b"\x02";
const CTRL_SPACE: &[u8] = b"\x00";

#[test]
fn shell_tabs_mouse_and_detach() {
    let sb = Sandbox::new();
    let mut c = sb.attach("main", 90, 24);
    c.wait_for("ready$");
    // (Not a marker that could turn up in the scratch directory's name,
    // `astrum-e2e-<pid>-<n>`, which the status bar shows.)
    c.send(b"echo first-tab-$((20+22))\r");
    c.wait_for("first-tab-42");
    assert!(c.lines()[0].contains(" 1 "), "tab bar on the first row: {:?}", c.lines()[0]);
    assert!(c.lines()[23].contains("SHELL"), "status bar on the last row: {:?}", c.lines()[23]);

    // Prefix + c opens a second tab with a fresh shell.
    c.send(CTRL_B);
    c.wait_for("PREFIX");
    c.send(b"c");
    c.wait_until("a second tab", |c| c.lines()[0].contains(" 2 "));
    c.wait_until("the new tab's empty screen", |c| !c.text().contains("first-tab-42"));
    c.wait_for("ready$");

    // Clicking the "+" opens a third; clicking tab 1 goes back to the first.
    let plus = c.find_on_row(0, " + ").expect("a + button") + 1;
    c.click(plus, 0);
    c.wait_until("a third tab", |c| c.lines()[0].contains(" 3 "));
    let tab1 = c.find_on_row(0, " 1 ").expect("tab 1") + 1;
    c.click(tab1, 0);
    c.wait_for("first-tab-42");

    // Prefix + d detaches: the client exits, the session stays.
    c.send(CTRL_B);
    c.send(b"d");
    c.wait_exit();
    assert!(c.raw().contains("detached"), "goodbye line: {:?}", c.raw().chars().rev().take(120).collect::<String>());
    let listing = sb.run(&["ls"]);
    assert!(listing.contains("main") && listing.contains("3 tabs") && listing.contains("detached"), "{listing}");

    // Reattach: same tabs, same screen contents.
    let mut c = sb.attach("main", 90, 24);
    c.wait_for("first-tab-42");
    assert!(c.lines()[0].contains(" 3 "));

    // Closing every shell ends the session.
    c.close_every_shell();
    assert!(c.raw().contains("session ended"));
    assert!(sb.run(&["ls"]).contains("No sessions"));
}

#[test]
fn agent_key_toggles_and_layout_follows_resize() {
    let sb = Sandbox::new();
    let c = sb.attach("main", 80, 24);
    c.wait_for("ready$");

    c.send(CTRL_SPACE);
    c.wait_for("AGENT");
    c.wait_for("Astrum agent");
    c.send(CTRL_SPACE);
    c.wait_until("focus back on the shell", |c| c.lines()[23].contains("SHELL"));
    // The panel stays open until dismissed; Escape from the agent closes it.
    c.send(CTRL_SPACE);
    c.wait_for("AGENT");
    c.send(b"\x1b");
    c.wait_until("the panel to close", |c| !c.text().contains("Astrum agent"));

    // A smaller window: the status bar moves to the new last row and the
    // shell is told the new size.
    c.resize(60, 14);
    c.wait_until("the status bar on row 13", |c| c.lines().len() == 14 && c.lines()[13].contains("SHELL"));
    c.send(b"echo size=$(tput cols)x$(tput lines)\r");
    c.wait_for("size=60x12");
}

#[test]
fn session_survives_the_terminal_going_away() {
    let sb = Sandbox::new();
    let mut c = sb.attach("keep", 80, 20);
    c.wait_for("ready$");
    c.send(b"echo still-here-$((1+1))\r");
    c.wait_for("still-here-2");

    // Closing a terminal window hangs up whatever runs in it. The client
    // goes away; the server, in a session of its own, must not.
    let pid = c.child.process_id().expect("client pid") as libc::pid_t;
    unsafe { libc::kill(pid, libc::SIGHUP) };
    c.wait_exit();
    drop(c);
    std::thread::sleep(Duration::from_millis(300));
    let listing = sb.run(&["ls"]);
    assert!(listing.contains("keep") && listing.contains("detached"), "{listing}");

    let c = sb.attach("keep", 80, 20);
    c.wait_for("still-here-2");

    assert!(sb.run(&["kill", "-s", "keep"]).contains("Stopped"));
    assert!(sb.run(&["ls"]).contains("No sessions"));
}

#[test]
fn the_client_exits_when_its_terminal_disappears() {
    use std::os::unix::process::CommandExt;
    let sb = Sandbox::new();

    // A pseudo-terminal held by this test alone, so that closing our end is
    // what closing a terminal window is to the client: its terminal is gone.
    let (mut master, mut slave) = (0, 0);
    assert_eq!(unsafe { libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut()) }, 0);
    let size = libc::winsize { ws_row: 24, ws_col: 80, ws_xpixel: 0, ws_ypixel: 0 };
    unsafe { libc::ioctl(master, libc::TIOCSWINSZ as _, &size) };
    let mut cmd = Command::new(BIN);
    cmd.args(["-s", "gone"]).current_dir(sb.root.join("home"));
    for var in ["ASTRUM", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "ZDOTDIR", "PROMPT_COMMAND", "COLORTERM", "TERM_PROGRAM"] {
        cmd.env_remove(var);
    }
    cmd.envs(sb.env());
    unsafe {
        cmd.pre_exec(move || {
            // The client's own session, with the pseudo-terminal as its terminal.
            libc::setsid();
            libc::ioctl(slave, libc::TIOCSCTTY as _, 0);
            for fd in 0..3 {
                libc::dup2(slave, fd);
            }
            libc::close(slave);
            libc::close(master);
            Ok(())
        });
    }
    let mut child = cmd.spawn().unwrap();
    unsafe { libc::close(slave) };

    // Play the terminal until the shell's prompt has been drawn.
    let mut seen = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(20);
    while !String::from_utf8_lossy(&seen).contains("ready$") {
        assert!(Instant::now() < deadline, "no prompt: {}", String::from_utf8_lossy(&seen));
        let mut fds = libc::pollfd { fd: master, events: libc::POLLIN, revents: 0 };
        if unsafe { libc::poll(&mut fds, 1, 100) } <= 0 {
            continue;
        }
        let mut buf = [0u8; 8192];
        let n = unsafe { libc::read(master, buf.as_mut_ptr().cast(), buf.len()) };
        assert!(n > 0, "the client went away early");
        let chunk = &buf[..n as usize];
        if chunk.windows(3).any(|w| w == b"\x1b[c") {
            let answer = b"\x1b[?62;c";
            unsafe { libc::write(master, answer.as_ptr().cast(), answer.len()) };
        }
        seen.extend_from_slice(chunk);
    }
    assert!(sb.run(&["ls"]).contains("attached"));

    // The terminal goes away. The client must notice and leave promptly,
    // not stay behind reading from a terminal that is no longer there.
    unsafe { libc::close(master) };
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut exited = false;
    while Instant::now() < deadline && !exited {
        exited = matches!(child.try_wait(), Ok(Some(_)));
        std::thread::sleep(Duration::from_millis(50));
    }
    if !exited {
        let _ = child.kill();
        let _ = child.wait();
    }
    assert!(exited, "the client was still running 5 seconds after its terminal closed");

    // The session itself carries on, as it should.
    let listing = sb.run(&["ls"]);
    assert!(listing.contains("gone") && listing.contains("detached"), "{listing}");
}

#[test]
fn panes_split_take_focus_zoom_resize_and_close() {
    let sb = Sandbox::new();
    // 101 columns: two panes of 50 with a divider between. Rows 1 to 22 are the panes.
    let mut c = sb.attach("main", 101, 24);
    c.wait_for("ready$");
    c.send(b"echo left-$((1+1))\r");
    c.wait_for("left-2");

    // Split side by side: a divider appears, and a new shell in the right half has the keyboard.
    c.send(CTRL_B);
    c.wait_for("PREFIX");
    c.send(b"|");
    c.wait_until("a divider down the middle", |c| c.lines()[10].chars().nth(50) == Some('│'));
    c.wait_until("a prompt in the new pane", |c| c.column_of("ready$").is_some_and(|col| col > 50));
    c.send(b"echo right-$(tput cols)x$(tput lines)\r");
    c.wait_for("right-50x22");
    assert!(c.column_of("right-50x22").unwrap() > 50 && c.text().contains("left-2"));

    // Prefix and an arrow moves the keyboard back; a click moves it too.
    c.send(CTRL_B);
    c.send(b"\x1b[D");
    c.send(b"echo back-$(tput cols)\r");
    c.wait_for("back-50");
    assert!(c.first_column_of("back-50").unwrap() < 50);
    c.click(75, 15);
    c.send(b"echo clicked-$((2+2))\r");
    c.wait_for("clicked-4");
    assert!(c.column_of("clicked-4").unwrap() > 50);

    // Split the right pane again, this time one above the other.
    c.send(CTRL_B);
    c.send(b"-");
    c.wait_until("a divider across the right half", |c| c.lines()[12].chars().nth(75) == Some('─'));
    c.send(b"echo lower-$(tput cols)x$(tput lines)\r");
    c.wait_for("lower-50x10");

    // Zoom gives the pane the whole tab; again puts the others back.
    c.send(CTRL_B);
    c.send(b"z");
    c.wait_for("zoomed · 1 of 3 panes");
    c.send(b"echo zoomed-$(tput cols)x$(tput lines)\r");
    c.wait_for("zoomed-101x22");
    assert!(!c.text().contains('│'));
    c.send(CTRL_B);
    c.send(b"z");
    c.wait_until("the other panes back", |c| c.lines()[5].chars().nth(50) == Some('│') && !c.text().contains("zoomed ·"));

    // Dragging the divider resizes the panes on both sides of it.
    c.drag((50, 5), (30, 5));
    c.wait_until("the divider where it was dropped", |c| c.lines()[5].chars().nth(30) == Some('│'));
    c.send(b"echo wider-$(tput cols)\r");
    c.wait_for("wider-70");

    // Closing a pane hands its space to its neighbour; so does its shell ending.
    c.send(CTRL_B);
    c.send(b"x");
    c.wait_until("two panes left", |c| !c.text().contains('─'));
    c.send(b"echo tall-$(tput lines)\r");
    c.wait_for("tall-22");
    c.send(b"exit\r");
    c.wait_until("one pane left", |c| !c.text().contains('│'));
    c.send(b"echo alone-$(tput cols)\r");
    c.wait_for("alone-101");
    assert!(c.text().contains("left-2"), "the first pane is the one that remains");
    // The last pane takes the tab, and here the session, with it.
    c.send(b"exit\r");
    c.wait_exit();
    assert!(c.raw().contains("session ended"));
}

#[test]
fn tabs_move_by_drag_and_key_and_right_click_opens_a_menu() {
    let sb = Sandbox::new();
    let c = sb.attach("main", 100, 24);
    c.wait_for("ready$");
    // Three named tabs: alpha, beta, gamma.
    for (i, name) in ["alpha", "beta", "gamma"].iter().enumerate() {
        if i > 0 {
            c.send(CTRL_B);
            c.send(b"c");
            c.wait_until("the new tab", |c| c.lines()[0].contains(&format!(" {} ", i + 1)));
        }
        c.send(CTRL_B);
        c.send(b",");
        c.wait_for("Rename tab");
        c.send(format!("{name}\r").as_bytes());
        c.wait_until("the name on the tab", |c| c.lines()[0].contains(name));
    }
    let order = |c: &Client| {
        let bar = c.lines()[0].clone();
        let mut names = vec!["alpha", "beta", "gamma"];
        names.retain(|n| bar.contains(n));
        names.sort_by_key(|n| bar.find(n).unwrap());
        names.join(" ")
    };
    assert_eq!(order(&c), "alpha beta gamma");

    // The keyboard moves the tab in front; it stays in front.
    c.send(CTRL_B);
    c.send(b"<");
    c.wait_until("gamma before beta", |c| order(c) == "alpha gamma beta");
    // Dragging carries a tab along the bar.
    let (from, to) = (c.find_on_row(0, "alpha").unwrap(), c.find_on_row(0, "beta").unwrap());
    c.drag((from + 1, 0), (to + 1, 0));
    c.wait_until("alpha at the end", |c| order(c) == "gamma beta alpha");

    // Right-click on a tab: a menu for that tab. Clicking an entry does it.
    let beta = c.find_on_row(0, "beta").unwrap();
    c.right_click(beta + 1, 0);
    c.wait_for("Move left");
    let menu = c.text();
    assert!(menu.contains("Rename") && menu.contains("Move right") && menu.contains("New tab") && menu.contains("Close tab"), "{menu}");
    let (col, row) = c.position_of("Close tab").unwrap();
    c.click(col + 1, row);
    c.wait_until("beta closed", |c| order(c) == "gamma alpha");
    assert!(!c.text().contains("Move left"), "the menu is gone");

    // Right-click in a pane: split from the menu with the mouse, then with the keyboard.
    c.right_click(40, 10);
    c.wait_for("Split right");
    let (col, row) = c.position_of("Split right").unwrap();
    c.click(col + 1, row);
    c.wait_until("a divider down the middle", |c| c.lines()[10].contains('│'));
    c.right_click(20, 10);
    c.wait_for("Split down");
    c.send(b"\x1b[B\r");
    c.wait_until("a divider across the left pane", |c| c.text().contains('─'));
    assert!(c.column_of("─").unwrap() < 50, "the pane that was right-clicked is the one that split");

    // Escape closes a menu without doing anything.
    c.right_click(20, 5);
    c.wait_for("Close pane");
    c.send(b"\x1b");
    c.wait_until("the menu to close", |c| !c.text().contains("Close pane"));
}

#[test]
fn what_the_prefix_key_offers_can_be_clicked() {
    let sb = Sandbox::new();
    // Wide enough for the whole hint line.
    let mut c = sb.attach("main", 256, 24);
    c.wait_for("ready$");

    // The hint line outlasts a slow reader, and each entry is a button.
    c.send(CTRL_B);
    c.wait_for("PREFIX");
    let hints = c.lines()[23].clone();
    for entry in ["c new tab", "R remote tab", "| split right", "- split down", "z zoom", "x close", ", rename", "d detach", "? help", "1-9 tab · arrows pane"] {
        assert!(hints.contains(entry), "{entry:?} in {hints:?}");
    }
    std::thread::sleep(Duration::from_secs(4));
    assert!(c.lines()[23].contains("PREFIX"), "the hint line is still there after four seconds");
    let (col, row) = c.position_of("split right").unwrap();
    c.click(col + 2, row);
    c.wait_until("a divider down the middle", |c| c.lines()[10].contains('│'));
    c.wait_until("the hint line to go", |c| !c.text().contains("PREFIX"));

    // A click somewhere else lets go of the prefix: the next key goes to the shell.
    c.send(CTRL_B);
    c.wait_for("PREFIX");
    c.click(120, 10);
    c.wait_until("the hint line to go", |c| !c.text().contains("PREFIX"));
    c.send(b"echo typed-$((2+3))\r");
    c.wait_for("typed-5");
    assert!(c.lines()[0].contains(" 1 ") && !c.lines()[0].contains(" 2 "), "no tab was opened: {:?}", c.lines()[0]);

    // The menu button on the status bar lists the same things, without the prefix key.
    let (col, row) = c.position_of("≡ menu").unwrap();
    assert_eq!(row, 23);
    c.click(col + 2, row);
    c.wait_for("Split down");
    let menu = c.text();
    for entry in ["New tab", "Split right", "Zoom pane", "Close pane", "Rename tab", "Talk to the agent", "Find in scrollback", "Choose a theme", "Detach", "^B c", "^B ?"] {
        assert!(menu.contains(entry), "{entry:?} in the menu:\n{menu}");
    }
    assert!(c.lines()[23].contains("SHELL"), "the menu sits above the status bar");
    let (col, row) = c.position_of("Zoom pane").unwrap();
    c.click(col + 2, row);
    c.wait_for("zoomed · 1 of 2 panes");
    assert!(!c.text().contains("Split down"), "the menu is gone");

    // Open again: the entry now undoes the zoom, and a key picks an entry too.
    let (col, row) = c.position_of("≡ menu").unwrap();
    c.click(col + 2, row);
    c.wait_for("Unzoom pane");
    c.send(b"z");
    c.wait_until("the zoom to end", |c| !c.text().contains("zoomed"));
    let (col, row) = c.position_of("≡ menu").unwrap();
    c.click(col + 2, row);
    c.wait_for("New tab");
    let (col, row) = c.position_of("New tab").unwrap();
    c.click(col + 2, row);
    c.wait_until("a second tab", |c| c.lines()[0].contains(" 2 "));

    // On a narrow terminal the hint line shows what fits and offers the rest.
    c.resize(80, 24);
    c.wait_until("the status bar at the new width", |c| c.lines()[23].trim_end().ends_with("≡ menu") && c.lines()[23].chars().count() <= 80);
    c.send(CTRL_B);
    c.wait_for("≡ more");
    assert!(c.lines()[23].contains("c new tab") && !c.lines()[23].contains("? help"), "{:?}", c.lines()[23]);
    let (col, row) = c.position_of("≡ more").unwrap();
    c.click(col + 2, row);
    c.wait_for("Choose a model");
    assert!(!c.text().contains("PREFIX"));
    let (col, row) = c.position_of("Detach").unwrap();
    c.click(col + 2, row);
    c.wait_exit();
    let listing = sb.run(&["ls"]);
    assert!(listing.contains("main") && listing.contains("2 tabs") && listing.contains("detached"), "{listing}");
}

/// Was this text put on the clipboard (as the client asks its terminal to)?
fn on_clipboard(c: &Client, text: &str) -> bool {
    use base64::Engine;
    c.raw().contains(&format!("52;c;{}", base64::engine::general_purpose::STANDARD.encode(text)))
}

#[test]
fn scrollback_can_be_searched_and_copied_from_with_the_keyboard() {
    let sb = Sandbox::new();
    let c = sb.attach("main", 100, 24);
    c.wait_for("ready$");
    c.send(b"for i in $(seq 1 120); do echo \"row $i of Apples\"; done\r");
    c.wait_for("row 120 of Apples");

    // Prefix then / opens the search. It finds as you type, newest first:
    // row 1, rows 10 to 19 and rows 100 to 120 make 32.
    c.send(CTRL_B);
    c.wait_for("PREFIX");
    c.send(b"/");
    c.wait_for("find:");
    c.send(b"row 1");
    c.wait_for("32 of 32");
    // Enter goes to the next older match, bringing it into view.
    for _ in 0..25 {
        c.send(b"\r");
    }
    c.wait_for("7 of 32");
    c.wait_for("row 15 of Apples");
    assert!(c.lines()[1].contains("find: row 1"), "the bar is on the pane's first row: {:?}", c.lines()[1]);
    assert!(!c.lines()[1].contains("row 15"), "the match is not hidden under the bar");
    // The arrows on the bar do the same with the mouse, and the arrow keys too.
    let newer = c.find_on_row(1, " ↓ ").unwrap() + 1;
    c.click(newer, 1);
    c.wait_for("8 of 32");
    let older = c.find_on_row(1, " ↑ ").unwrap() + 1;
    c.click(older, 1);
    c.wait_for("7 of 32");
    c.send(b"\x1b[B");
    c.wait_for("8 of 32");
    // Past the newest it starts again from the oldest.
    for _ in 0..25 {
        c.send(b"\x1b[B");
    }
    c.wait_for("1 of 32");

    // Small letters find capitals too; a capital in the text makes it exact.
    c.send(b"\x15apples");
    c.wait_for("of 121");
    c.send(b"\x15APPLES");
    c.wait_for("no match");
    // Escape closes the search and goes back to the newest output.
    c.send(b"\x1b");
    c.wait_until("the search to close", |c| !c.text().contains("find:"));
    c.wait_for("row 120 of Apples");
    c.send(b"echo back-$((3+4))\r");
    c.wait_for("back-7");

    // The same from the menu, with the mouse.
    let (col, row) = c.position_of("≡ menu").unwrap();
    c.click(col + 2, row);
    c.wait_for("Find in scrollback");
    let (col, row) = c.position_of("Find in scrollback").unwrap();
    c.click(col + 2, row);
    c.wait_for("find:");
    c.send(b"row 77");
    c.wait_for("1 of 1");
    c.wait_for("row 77 of Apples");
    let close = c.find_on_row(1, "✕").unwrap();
    c.click(close, 1);
    c.wait_until("the search to close", |c| !c.text().contains("find:"));
    // And from the pane's own menu.
    c.right_click(40, 10);
    c.wait_for("Find…");
    let (col, row) = c.position_of("Find…").unwrap();
    c.click(col + 1, row);
    c.wait_for("find:");
    c.send(b"\x1b");
    c.wait_until("the search to close", |c| !c.text().contains("find:"));

    // Copy mode: a cursor moved with the keyboard. Up one line from the
    // prompt, select the line and copy it.
    // (The word searched for further down is kept out of the command line
    // itself, so it is on screen exactly twice whatever `clear` leaves behind.)
    c.send(b"w=wo; clear; echo one t$w three; echo four five t$w\r");
    c.wait_for("four five two");
    c.send(CTRL_B);
    c.wait_for("PREFIX");
    c.send(b"[");
    c.wait_for("COPY");
    c.send(b"k0v$y");
    c.wait_for("Copied 13 characters");
    assert!(on_clipboard(&c, "four five two"), "the line was copied");
    assert!(!c.text().contains("COPY"), "copying ends copy mode");

    // By words, with a search from inside copy mode: Enter keeps the
    // cursor on the match, and the selection runs from there.
    c.send(CTRL_B);
    c.wait_for("PREFIX");
    c.send(b"[");
    c.wait_for("COPY");
    c.send(b"/two");
    c.wait_for("2 of 2");
    c.send(b"\rn");
    c.wait_for("1 of 2");
    c.send(b"n");
    c.wait_for("2 of 2");
    c.send(b"vey");
    c.wait_for("Copied 3 characters");
    assert!(on_clipboard(&c, "two"));

    // The keys of copy mode never reach the shell, and q leaves it.
    c.send(CTRL_B);
    c.wait_for("PREFIX");
    c.send(b"[");
    c.wait_for("COPY");
    c.send(b"hjklwq");
    c.wait_until("copy mode to end", |c| !c.text().contains("COPY"));
    c.send(b"echo typed-$((4+5))\r");
    c.wait_for("typed-9");
    assert!(!c.text().contains("hjkl"), "{}", c.text());
}

/// The saved session of `name` in a sandbox: its directory.
fn saved_session(sb: &Sandbox, name: &str) -> PathBuf {
    sb.root.join("home/.local/share/astrum/sessions").join(name)
}

#[test]
fn a_stopped_session_comes_back_with_its_tabs_panes_and_scrollback() {
    let sb = Sandbox::new();
    let work = sb.root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let mut c = sb.attach("main", 101, 24);
    c.wait_for("ready$");
    // Tab 1: two panes side by side, each somewhere of its own, with output.
    c.send(format!("cd {} && printf '\\033[31mcrimson-%s\\033[0m\\n' text && seq 1 60\r", work.display()).as_bytes());
    c.wait_for("60");
    c.send(CTRL_B);
    c.wait_for("PREFIX");
    c.send(b"|");
    c.wait_until("a prompt in the new pane", |c| c.column_of("ready$").is_some_and(|col| col > 50));
    c.send(b"cd / && echo right-$((5+5))\r");
    c.wait_for("right-10");
    // Tab 2, with a name.
    c.send(CTRL_B);
    c.wait_for("PREFIX");
    c.send(b"c");
    c.wait_until("a second tab", |c| c.lines()[0].contains(" 2 "));
    c.wait_for("ready$");
    c.send(b"echo second-$((1+1))\r");
    c.wait_for("second-2");
    c.send(CTRL_B);
    c.wait_for("PREFIX");
    c.send(b",");
    c.send(b"notes\r");
    c.wait_until("the tab's new name", |c| c.lines()[0].contains("notes"));

    // Stopping the session keeps it on disk, for its owner only, and says so in the list.
    assert!(sb.run(&["kill", "-s", "main"]).contains("Stopped session `main`."));
    c.wait_exit();
    let dir = saved_session(&sb, "main");
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
    assert_eq!(std::fs::metadata(dir.join("session.json")).unwrap().permissions().mode() & 0o777, 0o600);
    let listing = sb.run(&["ls"]);
    assert!(listing.contains("main") && listing.contains("2 tabs") && listing.contains("stopped") && listing.contains("brings them back"), "{listing}");

    // Started again, from somewhere else: the same tabs, the named one in front as it was.
    let mut c = sb.attach("main", 101, 24);
    c.wait_for("Restored 2 tabs");
    c.wait_until("both tabs", |c| c.lines()[0].contains(" 1 ") && c.lines()[0].contains("notes"));
    c.wait_for("second-2");
    c.wait_for("restored · as it was on");
    c.wait_for("ready$");
    // The first tab has its two panes, each where it was, with what it showed.
    let tab1 = c.find_on_row(0, " 1 ").unwrap() + 1;
    c.click(tab1, 0);
    c.wait_for("right-10");
    c.wait_until("a divider down the middle", |c| c.lines()[10].chars().nth(50) == Some('│'));
    assert!(c.column_of("right-10").unwrap() > 50, "the right pane is on the right");
    // The keyboard is in the right pane, as before, and its shell is in /.
    c.send(b"echo here-$PWD-$((2+2))\r");
    c.wait_for("here-/-4");
    // The left pane is in its directory with its scrollback: search finds
    // the first line, long since scrolled away, still in red.
    c.click(10, 10);
    c.send(b"echo left-$(basename $PWD)\r");
    c.wait_for("left-work");
    c.send(CTRL_B);
    c.wait_for("PREFIX");
    c.send(b"/");
    c.wait_for("find:");
    c.send(b"crimson-text");
    c.wait_for("1 of 1");
    assert!(c.text().contains("crimson-text\n1\n2") || c.lines().iter().any(|l| l.starts_with("crimson-text")), "{}", c.text());
    c.send(b"\x1b");

    // Closing a tab is remembered too; closing the last shell ends the
    // session for good, and nothing of it is kept.
    assert!(sb.run(&["kill", "-s", "main"]).contains("Stopped"));
    c.wait_exit();
    let mut c = sb.attach("main", 101, 24);
    c.wait_for("Restored 2 tabs");
    c.wait_until("the restored shells", |c| c.text().matches("ready$").count() >= 1 && c.text().contains("restored ·"));
    c.close_every_shell();
    assert!(c.raw().contains("session ended"));
    assert!(!dir.exists(), "a session whose shells were all closed leaves nothing saved");
    assert!(sb.run(&["ls"]).contains("No sessions are running. Start one"));
    let c = sb.attach("main", 101, 24);
    c.wait_for("ready$");
    assert!(!c.text().contains("Restored") && !c.lines()[0].contains(" 2 "), "{}", c.text());
}

#[test]
fn restoring_can_be_limited_or_turned_off_and_saved_tabs_discarded() {
    // Layout only: the tabs come back, what was on screen does not, and none of it is on disk.
    let sb = Sandbox::new();
    sb.write_config("restore = \"layout\"\n");
    let mut c = sb.attach("main", 90, 24);
    c.wait_for("ready$");
    c.send(b"echo secret-$((40+2))\r");
    c.wait_for("secret-42");
    c.send(CTRL_B);
    c.wait_for("PREFIX");
    c.send(b"c");
    c.wait_until("a second tab", |c| c.lines()[0].contains(" 2 "));
    assert!(sb.run(&["kill", "-s", "main"]).contains("Stopped"));
    c.wait_exit();
    let dir = saved_session(&sb, "main");
    let files: Vec<String> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(files, vec!["session.json".to_string()], "only the layout is kept");
    let mut c = sb.attach("main", 90, 24);
    c.wait_for("Restored 2 tabs");
    let tab1 = c.find_on_row(0, " 1 ").unwrap() + 1;
    c.click(tab1, 0);
    c.wait_for("ready$");
    assert!(!c.text().contains("secret-42") && !c.text().contains("restored ·"), "{}", c.text());

    // --forget stops the session and discards its tabs.
    let out = sb.run(&["kill", "-s", "main", "--forget"]);
    assert!(out.contains("Stopped session `main` and discarded its saved tabs."), "{out}");
    c.wait_exit();
    assert!(!dir.exists());
    // It also discards the tabs of a session that is not running.
    let mut c = sb.attach("main", 90, 24);
    c.wait_for("ready$");
    assert!(!c.text().contains("Restored"));
    assert!(sb.run(&["kill", "-s", "main"]).contains("Stopped"));
    c.wait_exit();
    assert!(dir.exists());
    let out = sb.run(&["kill", "-s", "main", "--forget"]);
    assert!(out.contains("Discarded the saved tabs of session `main`."), "{out}");
    assert!(!dir.exists());

    // Off: nothing is saved at all.
    let sb = Sandbox::new();
    sb.write_config("restore = \"off\"\n");
    let mut c = sb.attach("main", 90, 24);
    c.wait_for("ready$");
    c.send(CTRL_B);
    c.wait_for("PREFIX");
    c.send(b"c");
    c.wait_until("a second tab", |c| c.lines()[0].contains(" 2 "));
    assert!(sb.run(&["kill", "-s", "main"]).contains("Stopped"));
    c.wait_exit();
    assert!(!saved_session(&sb, "main").exists());
    let c = sb.attach("main", 90, 24);
    c.wait_for("ready$");
    assert!(!c.lines()[0].contains(" 2 ") && !c.text().contains("Restored"));
}

/// The number printed after `label` on screen, e.g. a process id. Waits
/// for it: the label is on screen as soon as the command is typed, the
/// number only once the command has run.
fn number_after(c: &Client, label: &str) -> i32 {
    let find = |text: &str| -> Option<i32> {
        text.rmatch_indices(label).find_map(|(at, _)| {
            let digits: String = text[at + label.len()..].chars().take_while(|ch| ch.is_ascii_digit()).collect();
            digits.parse().ok()
        })
    };
    c.wait_until(&format!("a number after {label}"), |c| find(&c.text()).is_some());
    find(&c.text()).unwrap()
}

#[test]
fn a_session_that_crashes_or_is_killed_from_outside_can_still_be_restored() {
    // The session is written out as it goes, so a server that dies without
    // warning loses little.
    let sb = Sandbox::new();
    let mut c = sb.attach_env("main", 90, 24, &[("ASTRUM_SAVE_AFTER_MS", "300")]);
    c.wait_for("ready$");
    c.send(b"echo before-the-crash-$((6*7)); echo server-$PPID\r");
    c.wait_for("before-the-crash-42");
    let server = number_after(&c, "server-");
    let dir = saved_session(&sb, "main");
    c.wait_until("the session to be written out", |_| {
        std::fs::read_dir(&dir).is_ok_and(|d| d.flatten().filter(|e| e.path().extension().is_some_and(|x| x == "txt")).any(|e| std::fs::read_to_string(e.path()).is_ok_and(|t| t.contains("before-the-crash-42"))))
    });
    unsafe { libc::kill(server, libc::SIGKILL) };
    c.wait_exit();
    let mut c = sb.attach("main", 90, 24);
    c.wait_for("Restored 1 tab");
    c.wait_for("before-the-crash-42");
    c.wait_for("ready$");

    // Shells killed from outside, as when the machine shuts down, end the
    // session; but it was not closed, so it is kept.
    c.send(b"echo shell-$$\r");
    c.wait_for("shell-");
    c.wait_for("ready$");
    let shell = number_after(&c, "shell-");
    unsafe { libc::kill(shell, libc::SIGKILL) };
    c.wait_exit();
    assert!(dir.join("session.json").exists(), "the session is still saved");
    let c = sb.attach("main", 90, 24);
    c.wait_for("Restored 1 tab");
    c.wait_for("before-the-crash-42");
}

#[test]
fn a_restored_tab_has_its_conversation_with_the_agent() {
    let (sb, bodies) = agent_sandbox("safe");
    let mut c = sb.attach("main", 100, 30);
    c.wait_for("ready$");
    c.send(CTRL_SPACE);
    c.wait_for("AGENT");
    c.send(b"what is six times seven?\r");
    c.wait_for("Verdict: the shell printed forty-two.");
    assert!(sb.run(&["kill", "-s", "main"]).contains("Stopped"));
    c.wait_exit();

    // The panel is open as it was, with the conversation in it, and the
    // model is sent the earlier messages when it goes on.
    let c = sb.attach("main", 100, 30);
    c.wait_for("Restored 1 tab");
    c.wait_for("Verdict: the shell printed forty-two.");
    assert!(c.text().contains("what is six times seven?"), "{}", c.text());
    c.wait_for("ready$");
    c.send(CTRL_SPACE);
    c.wait_for("AGENT");
    let before = bodies.lock().unwrap().len();
    c.send(b"and again?\r");
    c.wait_until("another request", |_| bodies.lock().unwrap().len() > before);
    let sent = bodies.lock().unwrap();
    assert!(sent.last().unwrap().contains("what is six times seven?") && sent.last().unwrap().contains("and again?"));
    drop(sent);
    assert_eq!(saved_conversations(&sb).len(), 1, "it carries on in the same file");
}

#[test]
fn a_session_can_be_run_by_a_service_manager() {
    let sb = Sandbox::new();
    // What would be installed, without installing anything.
    let shown = sb.run(&["-s", "work", "service", "install", "--print"]);
    let home = sb.root.join("home");
    if cfg!(target_os = "macos") {
        assert!(shown.contains("Library/LaunchAgents/io.github.robert-mcdermott.astrum.work.plist"), "{shown}");
        assert!(shown.contains("<string>__server</string>") && shown.contains("<string>--service</string>") && shown.contains("<key>KeepAlive</key>"), "{shown}");
    } else {
        assert!(shown.contains("systemd/user/astrum-work.service"), "{shown}");
        assert!(shown.contains("--session work __server --service") && shown.contains("Restart=always"), "{shown}");
    }
    assert!(shown.contains(&*home.to_string_lossy()), "it is written under the home directory: {shown}");
    assert!(shown.contains("ASTRUM_CONFIG_DIR") && shown.contains("/bin/bash"), "the server is told where its files are and which shell to run: {shown}");
    assert!(sb.run(&["-s", "work", "service", "status"]).contains("No service is installed for session `work`"));
    assert!(sb.run(&["-s", "work", "service", "uninstall"]).contains("No service is installed"));

    // A server started as a service does not fight a session that is
    // already running under the same name: it waits, and takes over when
    // that one stops, with the tabs.
    let mut c = sb.attach("main", 90, 24);
    c.wait_for("ready$");
    c.send(b"echo kept-$((8*8))\r");
    c.wait_for("kept-64");
    let mut service = Command::new(BIN).args(["-s", "main", "__server", "--service"]).envs(sb.env()).spawn().unwrap();
    std::thread::sleep(Duration::from_millis(600));
    assert!(service.try_wait().unwrap().is_none(), "it waits rather than giving up");
    assert!(sb.run(&["kill", "-s", "main"]).contains("Stopped"));
    c.wait_exit();
    c.wait_until("the service's server to be up", |_| sb.run(&["ls"]).contains("0 tabs"));
    assert!(service.try_wait().unwrap().is_none());
    let c = sb.attach("main", 90, 24);
    c.wait_for("Restored 1 tab");
    c.wait_for("kept-64");
    drop(c);
    let _ = service.kill();
    let _ = service.wait();
}

#[test]
fn a_shell_that_does_not_report_its_directory_is_asked_through_its_process() {
    // A plain sh has no integration: nothing tells Astrum where it is.
    let sb = Sandbox::new();
    std::fs::write(sb.root.join("home/.profile"), "PS1='plain$ '\n").unwrap();
    let deep = sb.root.join("deep/down");
    std::fs::create_dir_all(&deep).unwrap();
    let c = sb.attach_env("main", 100, 24, &[("SHELL", "/bin/sh")]);
    c.wait_for("plain$");
    c.send(format!("cd {}\r", deep.display()).as_bytes());
    c.wait_until("the status bar to follow", |c| c.lines()[23].contains("deep/down"));
    // A new tab opens where the shell is, as it would with the integration.
    c.send(CTRL_B);
    c.wait_for("PREFIX");
    c.send(b"c");
    c.wait_until("a second tab", |c| c.lines()[0].contains(" 2 "));
    c.wait_for("plain$");
    c.send(b"echo in-$(basename $PWD)\r");
    c.wait_for("in-down");

    // A shell started inside an integrated one is followed too, and the
    // outer shell's own report takes over again when it ends.
    let sb = Sandbox::new();
    let other = sb.root.join("other-place");
    std::fs::create_dir_all(&other).unwrap();
    let c = sb.attach("main", 100, 24);
    c.wait_for("ready$");
    c.send(b"PS1='inner> ' sh\r");
    c.wait_for("inner>");
    c.send(format!("cd {}\r", other.display()).as_bytes());
    c.wait_until("the status bar to follow the inner shell", |c| c.lines()[23].contains("other-place"));
    c.send(b"exit\r");
    c.wait_for("ready$");
    c.wait_until("the outer shell's directory", |c| !c.lines()[23].contains("other-place"));
}

#[test]
fn programs_are_told_the_real_colors_of_the_terminal() {
    // A program asks its terminal for the background color to tell a dark
    // screen from a light one. The answer is the outer terminal's own.
    let ask = "printf '\\033]11;?\\007'; IFS= read -rs -d $'\\a' -t 2 r; echo \"bg=${r#*;}=\"; printf '\\033]10;?\\007'; IFS= read -rs -d $'\\a' -t 2 r; echo \"fg=${r#*;}=\"\r";
    let sb = Sandbox::new();
    let c = sb.attach("main", 100, 24);
    c.wait_for("ready$");
    c.send(ask.as_bytes());
    c.wait_for("bg=rgb:fafa/f0f0/e6e6=");
    c.wait_for("fg=rgb:1010/2020/3030=");

    // When the theme recolors what programs draw, they are told the theme's colors.
    let sb = Sandbox::new();
    sb.write_config("theme = \"dracula\"\ntheme_terminal = true\n");
    let c = sb.attach("main", 100, 24);
    c.wait_for("ready$");
    c.send(ask.as_bytes());
    c.wait_for("fg=rgb:");
    let text = c.text();
    assert!(text.contains("bg=rgb:") && !text.contains("fafa/f0f0/e6e6"), "{text}");
}

#[test]
fn a_program_that_asks_for_it_gets_keys_spelled_out() {
    let sb = Sandbox::new();
    let c = sb.attach("main", 100, 24);
    c.wait_for("ready$");
    // The outer terminal here reports Ctrl+I and Shift+Enter unambiguously,
    // as modern ones do. A program that has not asked for more gets what it
    // always got: a tab and a carriage return.
    c.send(b"head -n 1 | cat -v\r");
    std::thread::sleep(Duration::from_millis(300));
    c.send(b"plain\x1b[105;5u\x1b[13;2u");
    c.wait_until("the line to come back from cat", |c| c.text().matches("plain").count() == 2);
    c.wait_until("the prompt", |c| c.text().matches("ready$").count() == 2);
    assert!(!c.text().contains("^[["), "{}", c.text());

    // A program asks for enhanced reports, checks that it got them, and is
    // then sent the keys as they really were.
    c.send(b"clear; printf '\\033[>1u\\033[?u'; head -n 2 | cat -v; printf '\\033[<u'; echo popped\r");
    c.wait_for("^[[?1u");
    c.send(b"\x1b[105;5u\x1b[13;2uq\r");
    c.wait_for("^[[105;5u^[[13;2uq");
    // Ctrl+C is a key like any other to such a program, not an interrupt.
    c.send(b"\x03\r");
    c.wait_for("^[[99;5u");
    c.wait_for("popped");
    // Once the program has let go, the keys mean what they used to.
    c.send(b"sleep 30\r");
    std::thread::sleep(Duration::from_millis(300));
    c.send(b"\x03");
    c.send(b"echo status-$?\r");
    c.wait_for("status-130");
}

#[test]
fn links_can_be_seen_followed_and_copied() {
    let sb = Sandbox::new();
    // Stand in for the system's way of opening links: note what was asked for.
    let (opener, opened) = (sb.root.join("opener.sh"), sb.root.join("opened.txt"));
    std::fs::write(&opener, format!("#!/bin/sh\necho \"$1\" >> '{}'\n", opened.display())).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&opener, std::fs::Permissions::from_mode(0o755)).unwrap();
    let asked = || std::fs::read_to_string(&opened).unwrap_or_default();
    let c = sb.attach_env("main", 110, 24, &[("ASTRUM_OPEN_TOOL", &opener.to_string_lossy())]);
    c.wait_for("ready$");

    // An address in plain text. Pointing at it says where it goes, without
    // the full stop that ends the sentence; Ctrl+click opens it.
    c.send(b"clear; echo see https://example.com/docs?q=1. and more\r");
    // (The line as printed, not as typed: the address is found by where it is.)
    c.wait_until("the line printed", |c| c.lines().iter().any(|l| l.starts_with("see https://")));
    let (col, row) = c.position_of("https://example.com").unwrap();
    c.hover(col + 4, row);
    c.wait_until("the status bar to show the link", |c| c.lines()[23].contains("https://example.com/docs?q=1 ") && c.lines()[23].contains("to open"));
    c.ctrl_click(col + 4, row);
    c.wait_until("the link to be opened", |_| asked() == "https://example.com/docs?q=1\n");
    // Away from it, the status bar is itself again, and Ctrl+click is just a click.
    c.hover(col - 3, row);
    c.wait_until("the status bar to return", |c| !c.lines()[23].contains("to open") && c.lines()[23].contains("SHELL"));
    c.ctrl_click(col - 3, row);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(asked().lines().count(), 1);

    // A link a program marked: the text says one thing, the target is
    // another, and the status bar shows the target.
    c.send(b"clear; printf '\\033]8;;https://hidden.example/page\\033\\\\click here\\033]8;;\\033\\\\ plain\\n'\r");
    c.wait_for("click here plain");
    let (col, row) = c.position_of("click here").unwrap();
    c.hover(col + 2, row);
    c.wait_until("the target in the status bar", |c| c.lines()[23].contains("https://hidden.example/page"));
    // The right-click menu opens it, or copies it.
    c.right_click(col + 2, row);
    c.wait_for("Open link");
    let (mc, mr) = c.position_of("Open link").unwrap();
    c.click(mc + 1, mr);
    c.wait_until("the marked link to be opened", |_| asked().ends_with("https://hidden.example/page\n"));
    // (The first menu gone before the next is asked for: what is waited
    // for below must be the new one.)
    c.wait_until("the menu closed", |c| !c.text().contains("Open link"));
    c.right_click(col + 2, row);
    c.wait_for("Copy link");
    let (mc, mr) = c.position_of("Copy link").unwrap();
    c.click(mc + 1, mr);
    c.wait_for("Copied the link");
    assert!(on_clipboard(&c, "https://hidden.example/page"));
    // Elsewhere the menu has no link entries.
    c.right_click(col + 14, row);
    c.wait_for("Split right");
    assert!(!c.text().contains("Open link"));
    c.send(b"\x1b");
    c.wait_until("the menu to close", |c| !c.text().contains("Split right"));

    // A link that is not to a page, mail or a file is not opened.
    c.send(b"clear; printf '\\033]8;;javascript:alert(1)\\033\\\\odd link\\033]8;;\\033\\\\\\n'\r");
    c.wait_until("the odd link", |c| c.lines()[1].starts_with("odd link"));
    // (The pointer has not moved, and what was under it has gone.)
    c.wait_until("the status bar to drop the old link", |c| !c.lines()[23].contains("hidden.example"));
    c.ctrl_click(2, 1);
    c.wait_for("not one Astrum opens");
    assert_eq!(asked().lines().count(), 2);
}

#[test]
fn what_one_terminal_does_cannot_misdirect_another() {
    let sb = Sandbox::new();
    let a = sb.attach("main", 90, 24);
    a.wait_for("ready$");
    for name in ["alpha", "beta", "gamma"] {
        a.send(format!("echo this-is-{name}\r").as_bytes());
        a.wait_for(&format!("this-is-{name}"));
        a.send(CTRL_B);
        a.wait_for("PREFIX");
        a.send(b",");
        a.send(format!("{name}\r").as_bytes());
        a.wait_until("the tab's name", |c| c.lines()[0].contains(name));
        if name != "gamma" {
            a.send(CTRL_B);
            a.wait_for("PREFIX");
            a.send(b"c");
            a.wait_until("a new tab's prompt", |c| !c.text().contains(&format!("this-is-{name}")) && c.text().contains("ready$"));
        }
    }
    // A second terminal of the same size. The first opens the menu of the
    // third tab; meanwhile the second closes the first tab, so every tab
    // has moved one place by the time "Close tab" is picked.
    let b = sb.attach("main", 90, 24);
    b.wait_until("three tabs on the second terminal", |c| c.lines()[0].contains("gamma"));
    let gamma = a.find_on_row(0, "gamma").unwrap();
    a.right_click(gamma + 1, 0);
    a.wait_for("Close tab");
    let alpha_close = b.find_on_row(0, "alpha").unwrap() + "alpha ".len();
    b.click(alpha_close, 0);
    b.wait_until("alpha closed", |c| !c.lines()[0].contains("alpha"));
    a.wait_until("alpha gone on the first terminal too, menu still open", |c| !c.lines()[0].contains("alpha") && c.text().contains("Close tab"));
    let (col, row) = a.position_of("Close tab").unwrap();
    a.click(col + 1, row);
    a.wait_until("the tab the menu was opened on to close", |c| !c.lines()[0].contains("gamma"));
    assert!(a.lines()[0].contains("beta"), "the tab that slid into its place is untouched: {:?}", a.lines()[0]);

    // Both terminals on the same tab, at the same size: nothing marks it as
    // the other's, and a click in the second lands where it was aimed.
    a.send(b"clear\r");
    a.send(CTRL_B);
    a.wait_for("PREFIX");
    a.send(b"|");
    a.wait_until("a prompt in the new pane", |c| c.column_of("ready$").is_some_and(|col| col > 44));
    a.send(b"echo right-side\r");
    a.wait_for("right-side");
    b.wait_for("right-side");
    assert!(!b.text().contains("in use in another window"), "{}", b.text());
    b.click(10, 10);
    b.send(b"echo typed-on-the-left\r");
    b.wait_for("typed-on-the-left");
    assert!(b.column_of("typed-on-the-left").unwrap() < 44, "the click moved the keyboard to the left pane: {}", b.text());
}

#[test]
fn shells_that_end_by_themselves_at_shutdown_do_not_erase_the_session() {
    // When the machine goes down, some shells end by themselves on being
    // told to, a moment before the server is told to stop as well. That
    // must not count as the session being closed.
    let sb = Sandbox::new();
    let mut c = sb.attach_env("main", 90, 24, &[("ASTRUM_SAVE_AFTER_MS", "300")]);
    c.wait_for("ready$");
    c.send(b"echo keep-me-$((9*9)); echo server-$PPID\r");
    c.wait_for("keep-me-81");
    let server = number_after(&c, "server-");
    let dir = saved_session(&sb, "main");
    c.wait_until("the session to be written out", |_| {
        std::fs::read_dir(&dir).is_ok_and(|d| d.flatten().filter(|e| e.path().extension().is_some_and(|x| x == "txt")).any(|e| std::fs::read_to_string(e.path()).is_ok_and(|t| t.contains("keep-me-81"))))
    });
    c.send(format!("nohup sh -c 'sleep 0.15; kill -TERM {server}' >/dev/null 2>&1 & exit\r").as_bytes());
    c.wait_exit();
    c.wait_until("the session to be kept", |_| dir.join("session.json").exists());
    std::thread::sleep(Duration::from_millis(700));
    assert!(dir.join("session.json").exists());
    let c = sb.attach("main", 90, 24);
    c.wait_for("Restored 1 tab");
    c.wait_for("keep-me-81");
}

#[test]
fn the_service_is_not_removed_from_inside_the_session_it_runs() {
    // Stopping the service stops the session, and a command run in that
    // session would be cut off before it had finished the job.
    let sb = Sandbox::new();
    let shown = sb.run(&["-s", "work", "service", "install", "--print"]);
    let (path, text) = shown.split_once('\n').unwrap();
    let path = PathBuf::from(path.trim_start_matches("# "));
    assert!(path.starts_with(&sb.root), "{path:?}");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, text).unwrap();
    for action in ["uninstall", "install"] {
        // (The definition differs from what would be written now, so an install would replace it.)
        std::fs::write(&path, format!("{text}\n")).unwrap();
        let out = sb.run_with(&["-s", "work", "service", action], &[("ASTRUM", "1"), ("ASTRUM_SESSION", "work")]);
        assert!(out.contains("which this command is running in"), "{action}: {out}");
        assert!(path.exists(), "{action} left the definition alone");
    }
}

#[test]
fn search_and_paging_hold_up_when_the_pane_changes() {
    let sb = Sandbox::new();
    let c = sb.attach("main", 120, 24);
    c.wait_for("ready$");
    // Something to find far to the right, then a search that lands on it.
    c.send(b"m=marker; printf '%110s\\n' far-right-$m\r");
    c.wait_until("the marker at the right", |c| c.column_of("far-right-marker").is_some_and(|col| col > 80));
    c.send(CTRL_B);
    c.wait_for("PREFIX");
    c.send(b"/");
    c.wait_for("find:");
    c.send(b"far-right-marker");
    c.wait_for("1 of 1");
    // The window gets much narrower: the column the search stands on is
    // gone. Carrying on typing in the search must not bring the session down.
    c.resize(60, 24);
    c.wait_until("the narrower window", |c| c.lines()[23].trim_end().ends_with("menu") && c.lines()[23].chars().count() <= 60);
    c.send(b"\x7f");
    c.send(b"r");
    c.wait_until("the search still at work", |c| c.lines()[1].contains("find:") && c.lines()[1].contains("of 1"));
    c.send(b"\x1b");
    c.wait_until("the search to close", |c| !c.text().contains("find:"));
    c.send(b"echo alive-$((2+2))\r");
    c.wait_for("alive-4");

    // Paging in copy mode moves one page at a time, and comes back to
    // where it was. (22 rows of pane: lines 80 to 100 and the prompt.)
    c.send(b"clear; seq 1 100\r");
    c.wait_until("the numbers", |c| c.lines()[21].trim() == "100" && c.lines()[22].contains("ready$"));
    c.send(CTRL_B);
    c.wait_for("PREFIX");
    c.send(b"[");
    c.wait_for("COPY");
    assert_eq!(c.lines()[2].trim(), "81");
    c.send(b"\x1b[5~");
    c.wait_until("one page back", |c| c.lines()[2].trim() == "60");
    c.send(b"\x1b[5~");
    c.wait_until("two pages back", |c| c.lines()[2].trim() == "39");
    c.send(b"\x1b[6~");
    c.wait_until("forward a page", |c| c.lines()[2].trim() == "60");
    c.send(b"\x1b[6~");
    c.wait_until("back where it started", |c| c.lines()[2].trim() == "81");
    c.send(b"q");
    c.wait_until("copy mode to end", |c| !c.text().contains("COPY"));
}

#[test]
fn keys_can_be_given_to_other_things_in_the_config() {
    let sb = Sandbox::new();
    sb.write_config(
        "[keys]\nnew_tab = [\"ctrl+t\", \"y\"]\nsplit_right = \"v\"\nnext_tab = \"alt+right\"\nprev_tab = \"alt+left\"\ndetach_all = \"none\"\nzoom = \"not a key\"\n",
    );
    // The listing says what the keys are now, and what could not be read.
    let listing = sb.run(&["keys"]);
    assert!(listing.lines().any(|l| l.starts_with("new_tab") && l.contains("ctrl+t, y")), "{listing}");
    assert!(listing.lines().any(|l| l.starts_with("detach_all") && l.contains("none")), "{listing}");
    assert!(listing.lines().any(|l| l.starts_with("close") && l.contains("x, w")), "an action left alone keeps its keys: {listing}");
    assert!(listing.contains("`not a key` is not a key"), "{listing}");

    let c = sb.attach("main", 200, 24);
    c.wait_for("ready$");
    // The session says so too, once, when it starts.
    c.wait_for("Config problem");
    c.send(b"echo first-tab\r");
    c.wait_for("first-tab");

    // A key with Ctrl works by itself, without the prefix.
    c.send(b"\x14");
    c.wait_until("a second tab from Ctrl+T", |c| c.lines()[0].contains(" 2 "));
    // Its other key is pressed after the prefix; the key it used to have does nothing.
    c.send(CTRL_B);
    c.wait_for("PREFIX");
    assert!(c.lines()[23].contains("y new tab") && c.lines()[23].contains("v split right"), "the hint line shows the keys as they are: {:?}", c.lines()[23]);
    c.send(b"y");
    c.wait_until("a third tab from the prefix and y", |c| c.lines()[0].contains(" 3 "));
    c.send(CTRL_B);
    c.wait_for("PREFIX");
    c.send(b"c");
    c.wait_until("the prefix to be let go", |c| !c.text().contains("PREFIX"));
    assert!(!c.lines()[0].contains(" 4 "), "c no longer opens a tab: {:?}", c.lines()[0]);

    // Alt with the arrows moves between tabs, as asked.
    c.send(b"\x1b[1;3D");
    c.send(b"\x1b[1;3D");
    c.wait_for("first-tab");
    c.send(b"\x1b[1;3C");
    c.wait_until("the next tab", |c| !c.text().contains("first-tab"));
    c.send(b"\x1b[1;3D");
    c.wait_for("first-tab");

    // The new split key, and the menu showing the keys as they are.
    c.send(CTRL_B);
    c.wait_for("PREFIX");
    c.send(b"v");
    c.wait_until("a divider down the middle", |c| c.lines()[10].contains('│'));
    let (col, row) = c.position_of("≡ menu").unwrap();
    c.click(col + 2, row);
    c.wait_for("Split down");
    let menu = c.text();
    assert!(menu.lines().any(|l| l.contains("New tab") && l.contains("^T")), "{menu}");
    assert!(menu.lines().any(|l| l.contains("Split right") && l.contains("^B v")), "{menu}");
    c.send(b"\x1b");
}

#[test]
fn a_change_to_the_config_file_takes_effect_in_the_running_session() {
    let sb = Sandbox::new();
    let c = sb.attach_env("main", 100, 24, &[("COLORTERM", "truecolor")]);
    c.wait_for("ready$");
    assert!(c.lines()[23].contains("^B ? help"));
    assert!(!c.raw().contains("48;2;"), "no theme yet");

    // A theme, a key and another prefix, written while the session runs.
    sb.write_config("theme = \"mcdermott\"\nprefix = \"ctrl+a\"\n\n[keys]\nnew_tab = \"y\"\n");
    c.wait_for("Settings reloaded");
    c.wait_until("the theme", |c| c.raw().contains("48;2;4;1;2"));
    c.wait_until("the new prefix in the status bar", |c| c.lines()[23].contains("^A ? help"));
    c.send(b"\x01");
    c.wait_for("PREFIX");
    c.send(b"y");
    c.wait_until("a second tab from the new key", |c| c.lines()[0].contains(" 2 "));
    // The old prefix is an ordinary key again: it goes to the shell.
    c.wait_for("ready$");
    c.send(CTRL_B);
    std::thread::sleep(Duration::from_millis(300));
    assert!(!c.text().contains("PREFIX"));

    // A file that cannot be read changes nothing, and says why.
    sb.write_config("theme = \"mcdermott\"\nprefix = [unfinished\n");
    c.wait_for("nothing was changed");
    c.send(b"\x01");
    c.wait_for("PREFIX");
    c.send(b"y");
    c.wait_until("a third tab: the keys are as they were", |c| c.lines()[0].contains(" 3 "));

    // Something odd that does not stop the file from being read is reported, and the rest applies.
    sb.write_config("theme = \"nord\"\n\n[keys]\nzoom = \"not a key\"\n");
    c.wait_for("Config problem: [keys] zoom");
    c.wait_until("the prefix back to its default", |c| c.lines()[23].contains("^B ? help"));
}

#[test]
fn the_settings_screen_changes_things_at_once_and_remembers_them() {
    let sb = Sandbox::new();
    let state = || std::fs::read_to_string(sb.root.join("cfg/state.toml")).unwrap_or_default();
    let mut c = sb.attach("main", 110, 30);
    c.wait_for("ready$");

    // From the menu, with the mouse.
    let (col, row) = c.position_of("≡ menu").unwrap();
    c.click(col + 2, row);
    c.wait_for("Settings");
    let (col, row) = c.position_of("Settings").unwrap();
    c.click(col + 1, row);
    c.wait_for("Agent panel sits");
    assert!(c.text().contains("beside the shell when there is room"));
    // Clicking a row changes its value, there and then, and it is written down.
    let (col, row) = c.position_of("Agent panel sits").unwrap();
    c.click(col + 2, row);
    c.wait_for("below the shell");
    c.wait_until("the choice to be saved", |_| state().contains("agent_panel = \"bottom\""));
    let (col, row) = c.position_of("Hide keys and passwords").unwrap();
    c.click(col + 2, row);
    c.wait_until("redaction off", |_| state().contains("redact_secrets = false"));
    // The keyboard works too: down to a row, Enter to change it, Left to go back.
    let (col, row) = c.position_of("A stopped session comes back with").unwrap();
    c.click(col + 2, row);
    c.wait_for("its tabs only");
    c.send(b"\x1b[D");
    c.wait_for("its tabs and what was on screen");
    c.send(b"\x1b[D");
    c.wait_until("nothing kept", |_| state().contains("restore = \"off\""));
    c.send(b"\r");
    c.wait_until("everything kept again", |_| state().contains("restore = \"all\""));
    // A row that opens something opens it.
    let (col, row) = c.position_of("Theme").unwrap();
    c.click(col + 2, row);
    c.wait_for("Previewing as you move");
    c.send(b"\x1b");
    c.wait_until("the list to close", |c| !c.text().contains("Previewing as you move"));

    // The choices outlast the session, and a change to the config file does not undo them.
    assert!(sb.run(&["kill", "-s", "main", "--forget"]).contains("Stopped"));
    c.wait_exit();
    let c = sb.attach("main", 110, 30);
    c.wait_for("ready$");
    sb.write_config("agent_panel = \"side\"\n");
    c.wait_for("Settings reloaded");
    c.send(CTRL_SPACE);
    c.wait_for("AGENT");
    c.send(b"/settings\r");
    c.wait_for("Agent panel sits");
    assert!(c.text().contains("below the shell"), "what was chosen here still stands: {}", c.text());
    assert!(c.lines().iter().any(|l| l.contains("Hide keys and passwords") && l.contains("off")));
    // Until they are forgotten: then the config file decides again.
    let (col, row) = c.position_of("Forget the choices made here").unwrap();
    c.click(col + 2, row);
    c.wait_until("the config file's value", |c| c.lines().iter().any(|l| l.contains("Agent panel sits") && l.contains("beside the shell") && !l.contains("room")));
    assert!(c.lines().iter().any(|l| l.contains("Hide keys and passwords") && l.contains("on")));
    assert!(!state().contains("agent_panel") && state().contains("welcomed = true"), "{}", state());
    // The close button closes.
    let top = c.lines().iter().position(|l| l.contains("╭ Settings")).unwrap();
    let close = c.find_on_row(top, "✕").unwrap();
    c.click(close, top);
    c.wait_until("the settings to close", |c| !c.text().contains("Agent panel sits"));
}

#[test]
fn someone_new_is_shown_around_once() {
    let sb = Sandbox::new();
    // Nothing of Astrum's here yet: a first run.
    std::fs::remove_file(sb.root.join("cfg/state.toml")).unwrap();
    let mut c = sb.attach("main", 100, 26);
    c.wait_for("Welcome to Astrum");
    c.wait_for("ready$");
    // Keys do not fall through to the shell while it is up.
    c.send(b"zz");
    std::thread::sleep(Duration::from_millis(200));
    assert!(!c.text().contains("ready$ zz"));
    // Through the cards with the mouse, and back with the keyboard.
    let (col, row) = c.position_of(" Next ").unwrap();
    c.click(col + 2, row);
    c.wait_for("Tabs and panes");
    c.click(col + 2, row);
    c.wait_for("The agent");
    assert!(c.text().contains("Choose a model"), "no model yet, so the card offers to choose one");
    c.send(b"\x1b[D");
    c.wait_for("Tabs and panes");
    c.send(b"\r");
    c.send(b"\r");
    c.wait_for("Coming back");
    let (col, row) = c.position_of(" Done ").unwrap();
    c.click(col + 2, row);
    c.wait_until("the walkthrough to close", |c| !c.text().contains("Coming back"));
    assert!(std::fs::read_to_string(sb.root.join("cfg/state.toml")).unwrap().contains("welcomed = true"));
    c.send(b"echo typing-works-$((1+1))\r");
    c.wait_for("typing-works-2");

    // Not again the next time; but the settings can bring it back.
    assert!(sb.run(&["kill", "-s", "main", "--forget"]).contains("Stopped"));
    c.wait_exit();
    let c = sb.attach("main", 100, 26);
    c.wait_for("ready$");
    std::thread::sleep(Duration::from_millis(300));
    assert!(!c.text().contains("Welcome to Astrum"));
    c.send(CTRL_B);
    c.wait_for("PREFIX");
    c.send(b".");
    c.wait_for("Show the walkthrough");
    let (col, row) = c.position_of("Show the walkthrough").unwrap();
    c.click(col + 2, row);
    c.wait_for("Welcome to Astrum");
    c.send(b"\x1b");
    c.wait_until("skipped", |c| !c.text().contains("Welcome to Astrum"));

    // Someone who has a config file already is not new.
    let sb = Sandbox::new();
    std::fs::remove_file(sb.root.join("cfg/state.toml")).unwrap();
    sb.write_config("scrollback = 5000\n");
    let c = sb.attach("main", 100, 26);
    c.wait_for("ready$");
    std::thread::sleep(Duration::from_millis(300));
    assert!(!c.text().contains("Welcome to Astrum"));
}

/// Whether the session is drawn at the width this terminal has: its name
/// sits at the right-hand end of the top bar, which a session drawn wider
/// would have off the edge.
fn drawn_at_width(c: &Client, cols: usize) -> bool {
    let top = c.lines()[0].trim_end().to_string();
    let end = top.chars().count();
    top.ends_with("main") && end <= cols && end + 3 > cols
}

#[test]
fn a_resize_that_arrives_together_with_input_is_not_missed() {
    let sb = Sandbox::new();
    let c = sb.attach("main", 120, 30);
    c.wait_for("ready$");
    assert!(drawn_at_width(&c, 120), "{}", c.text());
    // A terminal can change size and send something in the same instant:
    // a window that is split loses the focus as it shrinks, and says so.
    // Over and over, so that the two are sure to be found waiting together.
    for round in 0..150 {
        let cols = if round % 2 == 0 { 90 } else { 120 };
        c.resize(cols, 24);
        c.send(b"\x1b[O");
        let deadline = Instant::now() + Duration::from_secs(3);
        while !drawn_at_width(&c, cols as usize) {
            assert!(Instant::now() < deadline, "round {round}: the session never took the new width {cols}\n{}", c.text());
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    c.resize(90, 24);
    c.wait_until("the session drawn at the last size", |c| drawn_at_width(c, 90));
    c.send(b"echo now-$(tput cols)x$(tput lines)\r");
    c.wait_for("now-90x22");
}

#[test]
fn the_agent_panel_sits_beside_the_shell_on_a_wide_terminal() {
    let sb = Sandbox::new();
    let c = sb.attach("main", 160, 30);
    c.wait_for("ready$");
    c.send(CTRL_SPACE);
    c.wait_for("Astrum agent");
    // To the right of the shell, which keeps its full height.
    assert!(c.column_of("Astrum agent").unwrap() > 95, "{}", c.text());
    c.send(CTRL_SPACE);
    c.send(b"echo size-$(tput cols)x$(tput lines)\r");
    c.wait_for("size-95x28");
    // The line between them is the handle for resizing.
    assert_eq!(c.lines()[10].chars().nth(95), Some('│'));
    c.drag((95, 10), (110, 10));
    c.wait_until("the edge where it was dropped", |c| c.lines()[10].chars().nth(110) == Some('│'));
    c.send(b"echo wider-$(tput cols)\r");
    c.wait_for("wider-110");

    // In a narrower window the panel goes below the shell, as before.
    c.resize(100, 30);
    c.wait_until("the session drawn at the new width", |c| drawn_at_width(c, 100));
    c.send(b"clear; echo narrow-$(tput cols)x$(tput lines)\r");
    c.wait_for("narrow-100x15");

    // And it can be kept below whatever the width.
    let sb = Sandbox::new();
    sb.write_config("agent_panel = \"bottom\"\n");
    let c = sb.attach("main", 160, 30);
    c.wait_for("ready$");
    c.send(CTRL_SPACE);
    c.wait_for("Astrum agent");
    c.send(CTRL_SPACE);
    c.send(b"echo below-$(tput cols)x$(tput lines)\r");
    c.wait_for("below-160x15");
}

#[test]
fn the_agent_keeps_its_pane_while_you_work_in_another() {
    let (sb, bodies) = agent_sandbox("safe");
    let c = sb.attach("main", 101, 30);
    c.wait_for("ready$");
    c.send(CTRL_SPACE);
    c.wait_for("AGENT");
    // The agent starts on something in the only pane there is...
    c.send(b"go SLOWLY\r");
    c.wait_until("the request", |_| bodies.lock().unwrap().len() == 1);
    // ...and while it thinks, the user opens a second pane and works there.
    c.send(CTRL_B);
    c.send(b"|");
    c.wait_until("a prompt in the new pane", |c| c.column_of("ready$").is_some_and(|col| col > 50));
    c.send(b"echo mine-$((3+3))\r");
    c.wait_for("mine-6");
    // The agent's command runs in the pane it started in, not the one with the keyboard.
    c.wait_for("agent-ran-42");
    c.wait_for("Verdict: the shell printed forty-two.");
    assert!(c.first_column_of("agent-ran-42").unwrap() < 50 && c.column_of("agent-ran-42").unwrap() < 50, "{}", c.text());
    assert!(c.column_of("mine-6").unwrap() > 50, "{}", c.text());
    // Once it is done, it follows the keyboard: the next question is about the right-hand pane.
    // (Done, not only answered: the answer is on screen a moment before
    // the turn is over, and a line sent in that moment is added to it.)
    c.wait_for("Ask anything");
    c.send(CTRL_SPACE);
    c.send(b"and now?\r");
    c.wait_until("the next request", |_| bodies.lock().unwrap().len() == 3);
    assert!(bodies.lock().unwrap()[2].contains("$ echo mine-$((3+3))"), "{}", bodies.lock().unwrap()[2]);
}

/// One `application/vnd.amazon.eventstream` message (checksums zeroed).
fn event_frame(kind: &str, payload: &[u8]) -> Vec<u8> {
    let mut headers = Vec::new();
    for (k, v) in [(":message-type", "event"), (":event-type", kind), (":content-type", "application/json")] {
        headers.push(k.len() as u8);
        headers.extend_from_slice(k.as_bytes());
        headers.push(7);
        headers.extend_from_slice(&(v.len() as u16).to_be_bytes());
        headers.extend_from_slice(v.as_bytes());
    }
    let total = 12 + headers.len() + payload.len() + 4;
    let mut out = Vec::new();
    out.extend_from_slice(&(total as u32).to_be_bytes());
    out.extend_from_slice(&(headers.len() as u32).to_be_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(&headers);
    out.extend_from_slice(payload);
    out.extend_from_slice(&[0; 4]);
    out
}

/// A stand-in for a model endpoint (OpenAI-compatible, or Amazon Bedrock's
/// Converse stream when the request path asks for it). The first completion asks
/// to run a command; later ones answer according to whether the command's
/// output came back in the conversation. A request to summarize a
/// conversation gets a summary that mentions that output.
fn mock_model() -> (u16, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let bodies: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let seen = bodies.clone();
    std::thread::spawn(move || {
        let mut overflowed = false;
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut data = Vec::new();
            let mut buf = [0u8; 8192];
            let (mut header_end, mut length) = (None, 0usize);
            loop {
                let Ok(n) = stream.read(&mut buf) else { break };
                if n == 0 {
                    break;
                }
                data.extend_from_slice(&buf[..n]);
                if header_end.is_none() {
                    if let Some(pos) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                        header_end = Some(pos + 4);
                        let head = String::from_utf8_lossy(&data[..pos]).to_lowercase();
                        length = head
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length:").and_then(|v| v.trim().parse().ok()))
                            .unwrap_or(0);
                    }
                }
                if header_end.is_some_and(|h| data.len() >= h + length) {
                    break;
                }
            }
            let Some(h) = header_end else { continue };
            let head = String::from_utf8_lossy(&data[..h]).into_owned();
            let body = String::from_utf8_lossy(&data[h..]).into_owned();
            if head.contains("/converse-stream") {
                // Amazon Bedrock flavor: binary event-stream frames.
                let mut all = seen.lock().unwrap();
                let first = all.is_empty();
                all.push(format!("{}\n{body}", head.lines().next().unwrap_or("")));
                let events: Vec<(&str, String)> = if first {
                    vec![
                        ("messageStart", r#"{"role":"assistant"}"#.into()),
                        ("contentBlockStart", r#"{"contentBlockIndex":0,"start":{"toolUse":{"toolUseId":"tu_1","name":"run_command"}}}"#.into()),
                        ("contentBlockDelta", r#"{"contentBlockIndex":0,"delta":{"toolUse":{"input":"{\"command\":\"printf "}}}"#.into()),
                        ("contentBlockDelta", r#"{"contentBlockIndex":0,"delta":{"toolUse":{"input":"agent-ran-%d 42\"}"}}}"#.into()),
                        ("contentBlockStop", r#"{"contentBlockIndex":0}"#.into()),
                        ("messageStop", r#"{"stopReason":"tool_use"}"#.into()),
                        ("metadata", r#"{"usage":{"inputTokens":10,"outputTokens":5,"totalTokens":15}}"#.into()),
                    ]
                } else {
                    let verdict = if body.contains("agent-ran-42") { "Verdict: Bedrock saw forty-two." } else { "Verdict: nothing came back." };
                    vec![
                        ("contentBlockDelta", format!(r#"{{"contentBlockIndex":0,"delta":{{"text":"{verdict}"}}}}"#)),
                        ("contentBlockStop", r#"{"contentBlockIndex":0}"#.into()),
                        ("messageStop", r#"{"stopReason":"end_turn"}"#.into()),
                    ]
                };
                let mut wire = Vec::new();
                for (kind, payload) in events {
                    wire.extend(event_frame(kind, payload.as_bytes()));
                }
                let _ = stream.write_all(
                    format!("HTTP/1.1 200 OK\r\nContent-Type: application/vnd.amazon.eventstream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", wire.len())
                        .as_bytes(),
                );
                let _ = stream.write_all(&wire);
                continue;
            }
            let (ctype, payload) = if head.starts_with("GET /api/tags") {
                // What a local model runner says about its models: one it
                // forwards to a hosted service, one it runs itself.
                (
                    "application/json",
                    r#"{"models":[{"name":"mock-hosted","remote_host":"https://models.example","details":{"context_length":12}},{"name":"mock-local","details":{"context_length":12}}]}"#
                        .to_string(),
                )
            } else if head.starts_with("GET") {
                ("application/json", r#"{"data":[{"id":"mock-model"}]}"#.to_string())
            } else {
                let mut all = seen.lock().unwrap();
                let first = all.is_empty();
                all.push(body.clone());
                drop(all);
                // A prompt carrying this word is refused once, the way a
                // provider refuses a conversation that has grown too long.
                if body.contains("OVERFLOW-ONCE") && !overflowed {
                    overflowed = true;
                    let payload = r#"{"error":{"message":"This model's maximum context length is 12 tokens. Please reduce the length of the messages."}}"#;
                    let _ = stream.write_all(
                        format!("HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}", payload.len())
                            .as_bytes(),
                    );
                    continue;
                }
                // A conversation carrying this word is answered slowly, which
                // leaves time to type while the agent is "thinking".
                if body.contains("SLOWLY") {
                    std::thread::sleep(Duration::from_millis(1200));
                }
                // "TOOLCALL name {arguments}" in the latest prompt makes the
                // model ask for exactly that tool; once its result is back,
                // the model says so.
                let scripted = scripted_tool_call(&body);
                let summarizing = body.contains("handover summary");
                // Asked to follow a command the user started: wait for it with
                // command_status, then report what came back.
                let watching = body.contains("it is still running") && !body.contains("slept-2-done");
                // A request carrying this key is told so: how a test knows
                // which key a provider was called with.
                let keyed = head.to_lowercase().contains("authorization: bearer test-key-in-the-store");
                let chunk = if keyed {
                    r#"{"choices":[{"index":0,"delta":{"role":"assistant","content":"Verdict: the key arrived."},"finish_reason":null}]}"#.to_string()
                } else if let Some(scripted) = &scripted {
                    scripted.clone()
                } else if body.contains("TOOLCALL ") && !summarizing {
                    r#"{"choices":[{"index":0,"delta":{"role":"assistant","content":"Verdict: the tool answered."},"finish_reason":null}]}"#.to_string()
                } else if body.contains("slept-2-done") {
                    r#"{"choices":[{"index":0,"delta":{"role":"assistant","content":"Verdict: your command finished."},"finish_reason":null}]}"#.to_string()
                } else if watching {
                    r#"{"choices":[{"index":0,"delta":{"role":"assistant","content":"","tool_calls":[{"index":0,"id":"call_w","type":"function","function":{"name":"command_status","arguments":"{\"wait_secs\":20}"}}]},"finish_reason":null}]}"#
                        .to_string()
                } else if summarizing {
                    r#"{"choices":[{"index":0,"delta":{"role":"assistant","content":"Handover: the shell printed agent-ran-42, exit code: 0."},"finish_reason":null}]}"#.to_string()
                } else if first {
                    r#"{"choices":[{"index":0,"delta":{"role":"assistant","content":"","tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"run_command","arguments":"{\"command\":\"printf agent-ran-%d 42\",\"explanation\":\"Checking arithmetic.\"}"}}]},"finish_reason":null}]}"#
                        .to_string()
                } else if body.contains("agent-ran-42") && body.contains("exit code: 0") {
                    r#"{"choices":[{"index":0,"delta":{"role":"assistant","content":"Verdict: the shell printed **forty-two**."},"finish_reason":null}]}"#.to_string()
                } else {
                    r#"{"choices":[{"index":0,"delta":{"role":"assistant","content":"Verdict: no command output reached me."},"finish_reason":null}]}"#.to_string()
                };
                let finish = if !keyed && (scripted.is_some() || (!body.contains("TOOLCALL ") && (watching || (first && !summarizing)))) { "tool_calls" } else { "stop" };
                (
                    "text/event-stream",
                    format!(
                        "data: {chunk}\n\ndata: {{\"choices\":[{{\"index\":0,\"delta\":{{}},\"finish_reason\":\"{finish}\"}}]}}\n\ndata: {{\"choices\":[],\"usage\":{{\"prompt_tokens\":10,\"completion_tokens\":5,\"prompt_tokens_details\":{{\"cached_tokens\":4}}}}}}\n\ndata: [DONE]\n\n"
                    ),
                )
            };
            let _ = stream.write_all(
                format!("HTTP/1.1 200 OK\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}", payload.len())
                    .as_bytes(),
            );
        }
    });
    (port, bodies)
}

/// The tool call a prompt of the form `TOOLCALL name {json arguments}` asks
/// the stand-in model to make, as a streamed chunk, when that prompt is the
/// last thing in the conversation.
fn scripted_tool_call(body: &str) -> Option<String> {
    let request: serde_json::Value = serde_json::from_str(body).ok()?;
    let last = request["messages"].as_array()?.last()?;
    if last["role"] != "user" {
        return None;
    }
    let text = last["content"].as_str()?;
    let rest = &text[text.rfind("TOOLCALL ")? + "TOOLCALL ".len()..];
    let (name, arguments) = rest.split_once(' ')?;
    let arguments: serde_json::Value = serde_json::from_str(arguments.trim()).ok()?;
    Some(
        serde_json::json!({"choices": [{"index": 0, "delta": {"role": "assistant", "content": "", "tool_calls": [
            {"index": 0, "id": "call_s", "type": "function", "function": {"name": name, "arguments": arguments.to_string()}}
        ]}, "finish_reason": null}]})
        .to_string(),
    )
}

fn agent_sandbox(autonomy: &str) -> (Sandbox, Arc<Mutex<Vec<String>>>) {
    let (port, bodies) = mock_model();
    let sb = Sandbox::new();
    sb.write_config(&format!(
        "[agent]\nprovider = \"mock\"\nmodel = \"mock-model\"\nautonomy = \"{autonomy}\"\n\n[providers.mock]\nkind = \"openai\"\nbase_url = \"http://127.0.0.1:{port}/v1\"\n"
    ));
    (sb, bodies)
}

#[test]
fn agent_asks_then_runs_the_command_in_the_live_shell() {
    let (sb, bodies) = agent_sandbox("ask");
    let c = sb.attach("main", 100, 30);
    c.wait_for("ready$");
    c.send(CTRL_SPACE);
    c.wait_for("AGENT");
    c.send(b"what is six times seven?\r");

    // "Ask first": nothing runs until the user approves.
    c.wait_for("Run this command?");
    assert!(c.text().contains("printf agent-ran-%d 42"));
    assert!(!c.text().contains("agent-ran-42"), "the command must not run before approval");
    c.send(b"y");

    // The command runs in the shell pane, and its output goes back to the model.
    c.wait_for("agent-ran-42");
    c.wait_for("Verdict: the shell printed forty-two.");
    let sent = bodies.lock().unwrap();
    assert_eq!(sent.len(), 2, "one request per model step");
    assert!(sent[0].contains("what is six times seven?") && sent[0].contains("<context>"));
    assert!(sent[1].contains("\"role\":\"tool\"") && sent[1].contains("call_1"));
}

#[test]
fn declining_tells_the_model_instead_of_running() {
    let (sb, bodies) = agent_sandbox("ask");
    let c = sb.attach("main", 100, 30);
    c.wait_for("ready$");
    c.send(CTRL_SPACE);
    c.wait_for("AGENT");
    c.send(b"do the thing\r");
    c.wait_for("Run this command?");
    c.send(b"n");
    c.wait_for("What should the agent do instead?");
    c.send(b"never mind\r");

    c.wait_for("Verdict: no command output reached me.");
    assert!(!c.text().contains("agent-ran-42"), "a declined command must not run");
    let sent = bodies.lock().unwrap();
    assert!(sent[1].contains("declined") && sent[1].contains("never mind"), "{}", sent[1]);
}

#[test]
fn read_only_mode_does_not_ask_for_a_harmless_command() {
    let (sb, _bodies) = agent_sandbox("safe");
    let c = sb.attach("main", 100, 30);
    c.wait_for("ready$");
    c.send(CTRL_SPACE);
    c.wait_for("AGENT");
    c.send(b"go\r");
    // `printf` is read-only, so it runs without a prompt.
    c.wait_for("Verdict: the shell printed forty-two.");
    assert!(!c.text().contains("Run this command?"));
}

/// The conversation files a sandbox's server has written.
fn saved_conversations(sb: &Sandbox) -> Vec<PathBuf> {
    let dir = sb.root.join("home/.local/share/astrum/conversations");
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|x| x == "jsonl")).collect()
}

#[test]
fn conversations_survive_a_server_restart() {
    let (sb, bodies) = agent_sandbox("safe");
    let mut c = sb.attach("main", 100, 30);
    c.wait_for("ready$");
    c.send(CTRL_SPACE);
    c.wait_for("AGENT");
    c.send(b"what is six times seven?\r");
    c.wait_for("Verdict: the shell printed forty-two.");

    // Stop the server. Its conversation is on disk, readable only by the user.
    assert!(sb.run(&["kill", "-s", "main", "--forget"]).contains("Stopped"));
    c.wait_exit();
    let files = saved_conversations(&sb);
    assert_eq!(files.len(), 1, "{files:?}");
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(std::fs::metadata(&files[0]).unwrap().permissions().mode() & 0o777, 0o600);

    // A new server starts with an empty panel; /resume brings the conversation back.
    let c = sb.attach("main", 100, 30);
    c.wait_for("ready$");
    c.send(CTRL_SPACE);
    c.wait_for("AGENT");
    assert!(!c.text().contains("Verdict"));
    c.send(b"/resume\r");
    c.wait_for("Resumed this conversation");
    let shown = c.text();
    assert!(shown.contains("what is six times seven?") && shown.contains("Verdict: the shell printed forty-two."), "{shown}");

    // The model gets the earlier messages too, tool call and result included.
    c.send(b"and again?\r");
    c.wait_until("a third request", |_| bodies.lock().unwrap().len() == 3);
    let sent = bodies.lock().unwrap();
    assert!(sent[2].contains("what is six times seven?") && sent[2].contains("call_1") && sent[2].contains("agent-ran-42"), "{}", sent[2]);
    assert!(sent[2].contains("and again?"));
    drop(sent);
    // Still one file: the resumed conversation carries on in place.
    c.wait_until("the reply", |c| c.text().matches("Verdict:").count() == 2);
    assert_eq!(saved_conversations(&sb).len(), 1);
}

#[test]
fn the_conversation_list_reopens_an_interrupted_turn_and_deletes() {
    let (sb, bodies) = agent_sandbox("ask");
    let mut c = sb.attach("main", 100, 30);
    c.wait_for("ready$");
    c.send(CTRL_SPACE);
    c.wait_for("AGENT");
    c.send(b"first question\r");
    c.wait_for("Run this command?");
    // The server goes away while the question is still open.
    assert!(sb.run(&["kill", "-s", "main", "--forget"]).contains("Stopped"));
    c.wait_exit();

    let c = sb.attach("main", 100, 30);
    c.wait_for("ready$");
    c.send(CTRL_SPACE);
    c.wait_for("AGENT");
    c.send(b"/conversations\r");
    c.wait_for("Enter to open");
    assert!(c.text().contains("first question"), "{}", c.text());
    c.send(b"\r");
    c.wait_for("Resumed this conversation");
    assert!(c.text().contains("not run: the session ended first"), "{}", c.text());
    assert!(!c.text().contains("Run this command?"), "an old question must not come back to life");

    // The unanswered tool call is closed off, so the conversation can go on.
    c.send(b"carry on\r");
    c.wait_for("Verdict: no command output reached me.");
    {
        let sent = bodies.lock().unwrap();
        assert_eq!(sent.len(), 2);
        assert!(sent[1].contains("Interrupted: the session ended") && sent[1].contains("carry on"), "{}", sent[1]);
    }

    // A conversation that is open cannot be deleted; after /clear it can, with two presses.
    const DEL: &[u8] = b"\x1b[3~";
    c.send(b"/conversations\r");
    c.wait_for("open in tab 1");
    c.send(DEL);
    c.wait_for("open in a tab");
    c.send(b"\x1b");
    c.wait_until("the list to close", |c| !c.text().contains("open in a tab"));
    c.send(b"/clear\r");
    c.wait_until("an empty panel", |c| !c.text().contains("Verdict"));
    c.send(b"/conversations\r");
    c.wait_for("Enter to open");
    c.send(DEL);
    c.wait_for("Press Del again");
    assert_eq!(saved_conversations(&sb).len(), 1);
    c.send(DEL);
    c.wait_for("Deleted.");
    assert!(saved_conversations(&sb).is_empty());
    c.send(b"\x1b");
    c.wait_until("the list to close", |c| !c.text().contains("Deleted."));
    c.send(b"/resume\r");
    c.wait_for("No saved conversations yet.");
}

#[test]
fn rules_files_reach_the_model() {
    let (sb, bodies) = agent_sandbox("safe");
    let home = sb.root.join("home");
    std::fs::create_dir_all(home.join("proj/.git")).unwrap();
    std::fs::create_dir_all(home.join("proj/sub")).unwrap();
    std::fs::write(sb.root.join("cfg/AGENTS.md"), "Rule for me: user-rule-alpha.\n").unwrap();
    std::fs::write(home.join("proj/AGENTS.md"), "Rule for the project: project-rule-beta.\n").unwrap();
    std::fs::write(home.join("proj/sub/AGENTS.md"), "Rule for this folder: folder-rule-gamma.\n").unwrap();
    // Above the repository root, so it does not apply inside it.
    std::fs::write(home.join("AGENTS.md"), "outside-rule-delta\n").unwrap();

    let c = sb.attach("main", 110, 30);
    c.wait_for("ready$");
    c.send(b"cd proj/sub\r");
    c.wait_until("the shell to report its new directory", |c| c.lines()[29].contains("proj/sub"));
    c.send(CTRL_SPACE);
    c.wait_for("AGENT");
    c.send(b"go\r");
    c.wait_for("Following the rules in");
    c.wait_for("Verdict: the shell printed forty-two.");

    let sent = bodies.lock().unwrap();
    let first: serde_json::Value = serde_json::from_str(&sent[0]).unwrap();
    let system = first["messages"][0]["content"].as_str().unwrap();
    let at = |needle: &str| system.find(needle).unwrap_or_else(|| panic!("`{needle}` missing from the system prompt:\n{system}"));
    // Yours first, then the project's from its root down to where you are.
    assert!(at("user-rule-alpha") < at("project-rule-beta") && at("project-rule-beta") < at("folder-rule-gamma"));
    assert!(!system.contains("outside-rule-delta"));
    // Every step of the turn carries them, not just the first.
    assert!(sent[1].contains("folder-rule-gamma"));
}

#[test]
fn tokens_are_counted_per_conversation() {
    // Every request to the stand-in reports 10 prompt tokens (4 of them from cache) and 5 back.
    let (sb, _bodies) = agent_sandbox_with("safe", "[models.mock-model]\ncontext_window = 2000\n");
    let mut c = sb.attach("main", 100, 30);
    c.wait_for("ready$");
    c.send(CTRL_SPACE);
    c.wait_for("AGENT");
    c.send(b"go\r");
    c.wait_for("Verdict: the shell printed forty-two.");
    // Two requests so far.
    c.wait_for("10 of 2k context · 20 in · 8 cached · 10 out · 30 total");
    c.send(b"/usage\r");
    c.wait_for("Context: 10 of 2k tokens");
    c.wait_for("Input: 20 tokens over 2 requests");
    c.wait_for("Cached: 8 of that input read from the provider's cache");
    c.wait_for("Output: 10 tokens");
    c.wait_for("Total: 30 tokens");

    // The totals belong to the conversation: they come back with it, and a new one starts from zero.
    assert!(sb.run(&["kill", "-s", "main", "--forget"]).contains("Stopped"));
    c.wait_exit();
    let c = sb.attach("main", 100, 30);
    c.wait_for("ready$");
    c.send(CTRL_SPACE);
    c.wait_for("AGENT");
    c.send(b"/resume\r");
    c.wait_for("20 in · 8 cached · 10 out · 30 total");
    c.send(b"/clear\r");
    c.wait_until("the totals to clear", |c| !c.text().contains("30 total"));
    c.send(b"/usage\r");
    c.wait_for("Nothing has been sent to a model");
}

#[test]
fn resume_prefers_the_conversation_from_this_directory() {
    let (sb, _bodies) = agent_sandbox("safe");
    std::fs::create_dir_all(sb.root.join("home/proj")).unwrap();
    std::fs::create_dir_all(sb.root.join("home/empty")).unwrap();
    let c = sb.attach("main", 100, 30);
    c.wait_for("ready$");
    c.send(CTRL_SPACE);
    c.wait_for("AGENT");

    // One conversation in the home directory, then a later one in a project.
    c.send(b"alpha question\r");
    c.wait_for("Verdict: the shell printed forty-two.");
    c.send(b"/clear\r");
    c.wait_until("an empty panel", |c| !c.text().contains("alpha question"));
    std::thread::sleep(Duration::from_millis(1100));
    c.send(b"!cd proj\r");
    c.wait_until("the shell to be in the project", |c| c.lines()[29].contains("/proj"));
    c.send(b"beta question\r");
    c.wait_for("Verdict: no command output reached me.");
    c.send(b"/clear\r");
    c.wait_until("an empty panel", |c| !c.text().contains("beta question"));

    // Back home, /resume picks the one from here, though the other is newer.
    c.send(b"!cd ~\r");
    c.wait_until("the shell to be back home", |c| !c.lines()[29].contains("/proj"));
    c.send(b"/resume\r");
    c.wait_for("Resumed this conversation");
    assert!(c.text().contains("alpha question") && !c.text().contains("beta question"), "{}", c.text());
    c.send(b"/clear\r");
    c.wait_until("an empty panel", |c| !c.text().contains("alpha question"));

    // Somewhere with no conversation of its own, it falls back to the newest and says so.
    c.send(b"!cd empty\r");
    c.wait_until("the shell to be in the empty directory", |c| c.lines()[29].contains("/empty"));
    c.send(b"/resume\r");
    c.wait_for("the most recent one from elsewhere");
    assert!(c.text().contains("beta question") && !c.text().contains("alpha question"), "{}", c.text());
}

fn agent_sandbox_with(autonomy: &str, extra: &str) -> (Sandbox, Arc<Mutex<Vec<String>>>) {
    let (port, bodies) = mock_model();
    let sb = Sandbox::new();
    sb.write_config(&format!(
        "[agent]\nprovider = \"mock\"\nmodel = \"mock-model\"\nautonomy = \"{autonomy}\"\n\n[providers.mock]\nkind = \"openai\"\nbase_url = \"http://127.0.0.1:{port}/v1\"\n\n{extra}"
    ));
    (sb, bodies)
}

fn messages_of(body: &str) -> Vec<serde_json::Value> {
    let v: serde_json::Value = serde_json::from_str(body).unwrap();
    v["messages"].as_array().unwrap().clone()
}

#[test]
fn compact_replaces_the_conversation_with_a_summary() {
    let (sb, bodies) = agent_sandbox("safe");
    let c = sb.attach("main", 100, 36);
    c.wait_for("ready$");
    c.send(CTRL_SPACE);
    c.wait_for("AGENT");
    c.send(b"/compact\r");
    c.wait_for("not enough conversation to compact");
    c.send(b"/summary\r");
    c.wait_for("has not been compacted");
    c.send(b"go\r");
    c.wait_for("Verdict: the shell printed forty-two.");
    // The answer is on screen a moment before the turn is over, and until
    // it is, `/compact` only says the agent is still working.
    c.wait_for("Ask anything");

    c.send(b"/compact keep the numbers\r");
    c.wait_for("Compacted.");
    // The summary is shown, so the user can see what the agent kept.
    c.wait_for("Handover: the shell printed agent-ran-42");
    {
        let sent = bodies.lock().unwrap();
        assert_eq!(sent.len(), 3);
        // The request to summarize is the conversation as text, with no tools on offer.
        let ask = messages_of(&sent[2]);
        assert_eq!(ask.len(), 2);
        let text = ask[1]["content"].as_str().unwrap();
        assert!(text.contains("## Tool result") && text.contains("agent-ran-42") && text.contains("keep the numbers"), "{text}");
        assert!(!sent[2].contains("\"tools\""));
    }

    // From here on the model sees the summary, not the messages it replaced.
    c.send(b"what now?\r");
    c.wait_until("the reply to the next prompt", |c| c.text().matches("Verdict: the shell printed forty-two.").count() == 2 || bodies.lock().unwrap().len() == 4);
    c.wait_until("a fourth request", |_| bodies.lock().unwrap().len() == 4);
    let sent = bodies.lock().unwrap();
    let after = messages_of(&sent[3]);
    assert_eq!(after.len(), 3, "system prompt, summary, new prompt: {}", sent[3]);
    assert!(after[1]["content"].as_str().unwrap().contains("<summary>\nHandover: the shell printed"));
    assert!(after[2]["content"].as_str().unwrap().contains("what now?"));
    assert!(!sent[3].contains("call_1"));
}

#[test]
fn a_nearly_full_context_is_compacted_mid_turn() {
    // Every request reports 10 prompt tokens; with a 12-token window that is "nearly full".
    let (sb, bodies) = agent_sandbox_with("safe", "[models.mock-model]\ncontext_window = 12\n");
    let c = sb.attach("main", 100, 36);
    c.wait_for("ready$");
    c.send(CTRL_SPACE);
    c.wait_for("AGENT");
    c.send(b"go\r");
    // The turn runs its command, compacts before going back to the model, and still finishes.
    c.wait_for("Compacted automatically.");
    c.wait_for("Verdict: the shell printed forty-two.");
    // Unasked, it takes one line; the summary itself is a command away.
    assert!(!c.text().contains("Handover: the shell printed"), "{}", c.text());
    let sent = bodies.lock().unwrap();
    assert_eq!(sent.len(), 3, "tool call, summary, continuation");
    assert!(sent[1].contains("handover summary"));
    let cont = messages_of(&sent[2]);
    assert_eq!(cont.len(), 2);
    let text = cont[1]["content"].as_str().unwrap();
    assert!(text.contains("Handover: the shell printed") && text.contains("Carry on from where the summary leaves off"), "{text}");
    assert!(!sent[2].contains("call_1"));
    drop(sent);
    // Still over the limit afterwards (the stand-in always reports 10 tokens):
    // it says so once instead of compacting again at every step.
    c.wait_for("still close to the model's limit after compacting");
    assert_eq!(c.text().matches("Verdict: the shell printed forty-two.").count(), 1, "the reply is shown once: {}", c.text());
    c.send(b"/summary\r");
    c.wait_for("The summary this conversation continues from");
    c.wait_for("Handover: the shell printed agent-ran-42");
}

#[test]
fn a_refused_oversized_request_is_compacted_and_retried() {
    let (sb, bodies) = agent_sandbox("safe");
    let c = sb.attach("main", 100, 36);
    c.wait_for("ready$");
    c.send(CTRL_SPACE);
    c.wait_for("AGENT");
    c.send(b"go\r");
    c.wait_for("Verdict: the shell printed forty-two.");
    c.send(b"now the OVERFLOW-ONCE case\r");
    c.wait_for("Compacted automatically.");
    c.wait_until("the retried request to be answered", |c| c.text().matches("Verdict: the shell printed forty-two.").count() >= 1 && bodies.lock().unwrap().len() == 5);
    assert!(!c.text().contains("maximum context length"), "the provider's refusal is handled, not shown: {}", c.text());
    let sent = bodies.lock().unwrap();
    // Refused, summarized, sent again: the summary and the prompt that did not fit.
    assert!(sent[3].contains("handover summary") && !sent[3].contains("OVERFLOW-ONCE"));
    let retry = messages_of(&sent[4]);
    assert_eq!(retry.len(), 3);
    assert!(retry[1]["content"].as_str().unwrap().contains("Handover:") && retry[2]["content"].as_str().unwrap().contains("OVERFLOW-ONCE"));
}

#[test]
fn compaction_can_be_left_to_the_user() {
    let (port, bodies) = mock_model();
    let sb = Sandbox::new();
    sb.write_config(&format!(
        "[agent]\nprovider = \"mock\"\nmodel = \"mock-model\"\nautonomy = \"safe\"\nauto_compact = false\n\n[providers.mock]\nkind = \"openai\"\nbase_url = \"http://127.0.0.1:{port}/v1\"\n\n[models.mock-model]\ncontext_window = 12\n"
    ));
    let c = sb.attach("main", 100, 36);
    c.wait_for("ready$");
    c.send(CTRL_SPACE);
    c.wait_for("AGENT");
    c.send(b"go\r");
    c.wait_for("Verdict: the shell printed forty-two.");
    c.send(b"now the OVERFLOW-ONCE case\r");
    // Nothing is compacted, though the window is tiny and the provider refuses:
    // the refusal is shown, with what to do about it.
    c.wait_for("maximum context length");
    c.wait_for("/compact");
    assert!(!c.text().contains("Compacted."));
    assert_eq!(bodies.lock().unwrap().len(), 3, "no summary was asked for");
}

fn sandbox_for_model(model: &str) -> (Sandbox, Arc<Mutex<Vec<String>>>) {
    let (port, bodies) = mock_model();
    let sb = Sandbox::new();
    sb.write_config(&format!(
        "[agent]\nprovider = \"mock\"\nmodel = \"{model}\"\nautonomy = \"safe\"\n\n[providers.mock]\nkind = \"openai\"\nbase_url = \"http://127.0.0.1:{port}/v1\"\n"
    ));
    (sb, bodies)
}

#[test]
fn a_models_context_size_is_learned_from_the_provider() {
    // The provider says this model takes 12 tokens, and nothing in the config
    // says otherwise: the panel shows it, and the turn compacts to stay inside it.
    let (sb, bodies) = sandbox_for_model("mock-hosted");
    let c = sb.attach("main", 100, 36);
    c.wait_for("ready$");
    c.send(CTRL_SPACE);
    c.wait_for("AGENT");
    c.send(b"go\r");
    c.wait_for("Compacted automatically.");
    c.wait_for("Verdict: the shell printed forty-two.");
    c.wait_for("10 of 12 context");
    assert_eq!(bodies.lock().unwrap().len(), 3, "tool call, summary, continuation");

    // A model the runner serves itself may get less context than its own
    // maximum, so that figure is not believed and the default stands.
    let (sb, bodies) = sandbox_for_model("mock-local");
    let c = sb.attach("main", 100, 36);
    c.wait_for("ready$");
    c.send(CTRL_SPACE);
    c.wait_for("AGENT");
    c.send(b"go\r");
    c.wait_for("Verdict: the shell printed forty-two.");
    c.wait_for("10 of 128k context");
    assert!(!c.text().contains("Compacted"));
    assert_eq!(bodies.lock().unwrap().len(), 2);
}

/// Put a conversation file in a sandbox as an earlier server would have left it.
fn plant_conversation(sb: &Sandbox, id: &str, title: &str, updated: u64) -> PathBuf {
    let dir = sb.root.join("home/.local/share/astrum/conversations");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{id}.jsonl"));
    let header = format!(r#"{{"version":1,"id":"{id}","title":"{title}","created":{updated},"updated":{updated}}}"#);
    let body = r#"{"history":[{"user":"hello"},{"assistant":{"text":"hi","calls":[]}}],"items":[{"user":"hello"},{"assistant":"hi"}]}"#;
    std::fs::write(&path, format!("{header}\n{body}\n")).unwrap();
    path
}

#[test]
fn conversations_unused_for_ninety_days_are_cleared_out() {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let day = 86_400;

    let (sb, _bodies) = agent_sandbox("safe");
    let stale = plant_conversation(&sb, "20260101-000000-aaaaa", "long forgotten", now - 91 * day);
    let kept = plant_conversation(&sb, "20261001-000000-bbbbb", "last month", now - 30 * day);
    let c = sb.attach("main", 100, 30);
    c.wait_for("ready$");
    c.wait_until("the stale conversation to be deleted", |_| !stale.exists());
    assert!(kept.exists());
    c.send(CTRL_SPACE);
    c.wait_for("AGENT");
    c.send(b"/conversations\r");
    c.wait_for("last month");
    assert!(!c.text().contains("long forgotten"));
    // The planted file is a conversation like any other: it opens.
    c.send(b"\r");
    c.wait_for("Resumed this conversation");

    // With the limit turned off, nothing is deleted however old it is.
    let (port, _bodies) = mock_model();
    let sb = Sandbox::new();
    sb.write_config(&format!(
        "[agent]\nprovider = \"mock\"\nmodel = \"mock-model\"\nkeep_conversations_days = 0\n\n[providers.mock]\nkind = \"openai\"\nbase_url = \"http://127.0.0.1:{port}/v1\"\n"
    ));
    let ancient = plant_conversation(&sb, "20200101-000000-ccccc", "from years ago", now - 2000 * day);
    let c = sb.attach("main", 100, 30);
    c.wait_for("ready$");
    c.send(CTRL_SPACE);
    c.wait_for("AGENT");
    c.send(b"/conversations\r");
    c.wait_for("from years ago");
    assert!(ancient.exists());
}

#[test]
fn typing_while_the_agent_works_steers_the_turn() {
    let (sb, bodies) = agent_sandbox("safe");
    let c = sb.attach("main", 100, 36);
    c.wait_for("ready$");
    c.send(CTRL_SPACE);
    c.wait_for("AGENT");
    let requests = || bodies.lock().unwrap().len();

    // While the model is thinking about the first request, add to it.
    c.send(b"go SLOWLY\r");
    c.wait_until("the first request", |_| requests() == 1);
    c.send(b"also greet me\r");
    c.wait_for("sent at the next step");
    // The model asks for a command; once it has run, the addition goes in with its result.
    c.wait_until("the second request", |_| requests() == 2);
    {
        let sent = bodies.lock().unwrap();
        let (result, added) = (sent[1].find("\"role\":\"tool\"").unwrap(), sent[1].find("also greet me").unwrap());
        assert!(result < added, "the addition follows the command's result: {}", sent[1]);
        assert!(sent[1].contains("The user added this while you were working"));
    }
    c.wait_until("the waiting message to move into the conversation", |c| !c.text().contains("sent at the next step"));

    // Something typed while the model writes its final reply continues the turn instead of waiting for a new one.
    c.send(b"one more thing\r");
    c.wait_until("the third request", |_| requests() == 3);
    assert!(bodies.lock().unwrap()[2].contains("one more thing"));

    // Stopping the agent hands a message that was still waiting back to the prompt.
    c.send(b"never mind this\r");
    c.wait_for("sent at the next step");
    c.send(b"\x1b");
    c.wait_for("Stopped.");
    c.wait_until("the message to return to the prompt", |c| c.text().contains("never mind this") && !c.text().contains("sent at the next step"));
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(requests(), 3, "a stopped turn sends nothing more");
}

#[test]
fn a_hash_line_at_the_shell_prompt_asks_the_agent() {
    let (sb, bodies) = agent_sandbox("safe");
    let c = sb.attach("main", 100, 30);
    c.wait_for("ready$");
    c.send(b"echo plain-$((1+1))\r");
    c.wait_for("plain-2");
    c.wait_until("the next prompt", |c| c.text().matches("ready$").count() == 2);

    // Typed in the shell, with the keyboard staying there: the agent panel opens and answers.
    c.send(b"# what is six times seven?\r");
    c.wait_for("Verdict: the shell printed forty-two.");
    assert!(c.lines()[29].contains("SHELL"), "{:?}", c.lines()[29]);
    {
        let sent = bodies.lock().unwrap();
        assert_eq!(sent.len(), 2);
        assert!(sent[0].contains("what is six times seven?") && !sent[0].contains("# what is six"), "{}", sent[0]);
    }

    // The shell is none the worse for it, and other lines are still its own:
    // a command, a comment that is not a question, and plain language while
    // that detection is off.
    c.wait_until("a prompt after the agent's command", |c| c.lines().iter().rev().find(|l| l.contains("ready$")).is_some_and(|l| l.trim_end().ends_with("ready$")));
    c.send(b"echo after-$((2+3))\r");
    c.wait_for("after-5");
    c.send(b"#!not-a-question\r");
    c.send(b"how do I list files here\r");
    c.wait_until("the shell saying it knows no such command", not_a_command);
    assert_eq!(bodies.lock().unwrap().len(), 2);
}

/// Whether the shell has said it knows no such command. Its words depend on
/// the system: bash's own are `how: command not found`, and Ubuntu's helper
/// says `Command 'how' not found`.
fn not_a_command(c: &Client) -> bool {
    c.text().to_lowercase().contains("not found")
}

#[test]
fn plain_language_at_the_prompt_can_go_to_the_agent() {
    let (port, bodies) = mock_model();
    let sb = Sandbox::new();
    sb.write_config(&format!(
        "[agent]\nprovider = \"mock\"\nmodel = \"mock-model\"\nautonomy = \"safe\"\nnatural_language = true\n\n[providers.mock]\nkind = \"openai\"\nbase_url = \"http://127.0.0.1:{port}/v1\"\n"
    ));
    let c = sb.attach("main", 100, 30);
    c.wait_for("ready$");
    c.send(b"how do I list files here\r");
    c.wait_for("Verdict: the shell printed forty-two.");
    assert!(!not_a_command(&c), "{}", c.text());
    assert!(bodies.lock().unwrap()[0].contains("how do I list files here"));
    // Commands are still commands, and a leading space forces a line to the shell.
    c.wait_until("a prompt after the agent's command", |c| c.lines().iter().rev().find(|l| l.contains("ready$")).is_some_and(|l| l.trim_end().ends_with("ready$")));
    c.send(b"echo still-a-shell-$((3+4))\r");
    c.wait_for("still-a-shell-7");
    c.wait_until("the next prompt", |c| c.lines().iter().rev().find(|l| l.contains("ready$")).is_some_and(|l| l.trim_end().ends_with("ready$")));
    c.send(b" how do I list files here\r");
    c.wait_until("the shell saying it knows no such command", not_a_command);
    assert_eq!(bodies.lock().unwrap().len(), 2);
}

#[test]
fn a_failed_command_offers_explain_and_fix() {
    let (sb, bodies) = agent_sandbox("safe");
    let c = sb.attach("main", 110, 30);
    c.wait_for("ready$");
    assert!(!c.lines()[29].contains("explain"));
    c.send(b"ls /no-such-place-zzz\r");
    c.wait_until("the offer in the status bar", |c| c.lines()[29].contains("✗ exit") && c.lines()[29].contains("explain") && c.lines()[29].contains("fix"));

    // Clicking "explain" puts the command, its exit code and its output to the agent.
    let at = c.find_on_row(29, "explain").unwrap();
    c.click(at + 1, 29);
    c.wait_for("Explain `ls /no-such-place-zzz`");
    c.wait_for("Verdict: the shell printed forty-two.");
    {
        let sent = bodies.lock().unwrap();
        assert!(sent[0].contains("Explain why this command failed"), "{}", sent[0]);
        assert!(sent[0].contains("$ ls /no-such-place-zzz") && sent[0].contains("No such file or directory") && sent[0].contains("[exit "), "{}", sent[0]);
    }
    // The agent has seen that failure now; the offer goes away.
    c.wait_until("the offer to clear", |c| !c.lines()[29].contains("explain"));
    assert!(c.lines()[29].contains("SHELL"));

    // Enter on an empty line runs nothing, so it brings nothing back.
    c.send(b"\r\r");
    std::thread::sleep(Duration::from_millis(600));
    assert!(!c.lines()[29].contains("explain"), "an empty line is not a command: {:?}", c.lines()[29]);

    // The next failure brings it back; the keyboard route asks for a fix.
    c.send(b"cat /no-such-file-yyy\r");
    c.wait_until("the offer again", |c| c.lines()[29].contains("fix"));
    c.send(CTRL_B);
    c.wait_for("PREFIX");
    c.send(b"f");
    c.wait_for("Fix `cat /no-such-file-yyy`");
    c.wait_until("the request to fix", |_| bodies.lock().unwrap().len() >= 3);
    let sent = bodies.lock().unwrap();
    assert!(sent[2].contains("Work out why, fix the cause") && sent[2].contains("$ cat /no-such-file-yyy"), "{}", sent[2]);
}

#[test]
fn the_agent_can_watch_a_command_the_user_started() {
    let (sb, bodies) = agent_sandbox("safe");
    let c = sb.attach("main", 110, 30);
    c.wait_for("ready$");
    c.send(b"sleep 2; echo slept-$((1+1))-done\r");
    c.wait_until("the offer to watch", |c| c.lines()[29].contains("watch"));
    c.send(CTRL_B);
    c.wait_for("PREFIX");
    c.send(b"o");
    c.wait_for("Watch `sleep 2;");
    // The agent waits on the command instead of running anything, and reports when it ends.
    c.wait_for("Verdict: your command finished.");
    assert!(c.text().contains("slept-2-done"), "the user's command ran to the end: {}", c.text());
    let sent = bodies.lock().unwrap();
    assert_eq!(sent.len(), 2);
    assert!(sent[0].contains("I started this command myself") && sent[0].contains("$ sleep 2; echo slept-"), "{}", sent[0]);
    // What command_status handed back: the finished command's result.
    assert!(sent[1].contains("call_w") && sent[1].contains("exit code: 0") && sent[1].contains("slept-2-done"), "{}", sent[1]);
}

/// The text the last tool call handed back, as the model was sent it.
fn tool_result_in(body: &str) -> String {
    messages_of(body).iter().rev().find(|m| m["role"] == "tool").and_then(|m| m["content"].as_str()).unwrap_or_default().to_string()
}

#[test]
fn the_agent_finds_files_and_searches_text_without_asking() {
    // "Ask first" asks about commands and changes; looking is always allowed.
    let (sb, bodies) = agent_sandbox("ask");
    let home = sb.root.join("home");
    std::fs::create_dir_all(home.join("proj/src")).unwrap();
    std::fs::create_dir_all(home.join("proj/node_modules/dep")).unwrap();
    std::fs::write(home.join("proj/src/app.py"), "import os\n# FIXME handle errors\nprint('hi')\n").unwrap();
    std::fs::write(home.join("proj/README.md"), "Demo\nFIXME write this\n").unwrap();
    std::fs::write(home.join("proj/node_modules/dep/x.py"), "# FIXME upstream\n").unwrap();

    let c = sb.attach("main", 110, 30);
    c.wait_for("ready$");
    c.send(b"cd proj\r");
    c.wait_until("the shell to report its new directory", |c| c.lines()[29].contains("proj"));
    c.send(CTRL_SPACE);
    c.wait_for("AGENT");

    c.send(b"TOOLCALL find_files {\"pattern\":\"*.py\"}\r");
    c.wait_for("Verdict: the tool answered.");
    assert!(c.text().contains("Found files *.py") && !c.text().contains("Run this command?"), "{}", c.text());
    let found = tool_result_in(&bodies.lock().unwrap()[1]);
    assert!(found.starts_with("1 file under") && found.contains("src/app.py") && !found.contains("node_modules"), "{found}");

    c.send(b"TOOLCALL search_files {\"pattern\":\"FIXME\\\\s+\\\\w+\",\"glob\":\"*.{py,md}\"}\r");
    c.wait_until("the second answer", |c| c.text().matches("Verdict: the tool answered.").count() == 2);
    let hits = tool_result_in(&bodies.lock().unwrap()[3]);
    assert!(hits.starts_with("2 matching lines in 2 files"), "{hits}");
    assert!(hits.contains("README.md:2: FIXME write this") && hits.contains("src/app.py:2: # FIXME handle errors"), "{hits}");
}

/// Type a prompt that makes the stand-in model call `tool` with `args`.
fn ask_for_tool(c: &Client, tool: &str, args: serde_json::Value) {
    c.send(format!("TOOLCALL {tool} {args}\r").as_bytes());
}

#[test]
fn a_file_change_is_shown_as_a_diff_and_can_be_approved_in_parts() {
    let (sb, bodies) = agent_sandbox("ask");
    let notes = sb.root.join("home/notes.txt");
    let original: String = (1..=30).map(|i| format!("line {i}\n")).collect();
    std::fs::write(&notes, &original).unwrap();
    let wanted = original.replace("line 3\n", "line three\n").replace("line 27\n", "line twenty-seven\n");
    let on_disk = || std::fs::read_to_string(&notes).unwrap();
    // Each tool call is two requests: the call, then its result going back.
    let answers = |_: &Client| bodies.lock().unwrap().len() / 2;
    // What the approval card shows (its lines carry a bar down the left edge).
    let card_of = |c: &Client| c.lines().into_iter().filter(|l| l.starts_with('▌')).collect::<Vec<_>>().join("\n");

    let c = sb.attach("main", 110, 44);
    c.wait_for("ready$");
    c.send(CTRL_SPACE);
    c.wait_for("AGENT");

    // A rewrite that changes two places far apart: shown as two hunks, nothing written yet.
    ask_for_tool(&c, "write_file", serde_json::json!({"path": "notes.txt", "content": wanted}));
    c.wait_for("Review each change");
    let card = card_of(&c);
    assert!(card.contains("Rewrite ") && card.contains("@@ -1,6 +1,6 @@"), "{card}");
    assert!(card.contains("- line 3") && card.contains("+ line three"), "{card}");
    // The change is longer than the card; it scrolls.
    assert!(card.contains("more lines") && !card.contains("+ line twenty-seven"), "{card}");
    c.send(b"\x1b[6~");
    c.wait_until("the second hunk to scroll into view", |c| card_of(c).contains("+ line twenty-seven"));
    assert_eq!(on_disk(), original);

    // One at a time: take the first, leave the second.
    c.send(b"r");
    c.wait_for("Change 1 of 2");
    assert!(card_of(&c).contains("+ line three") && !card_of(&c).contains("twenty-seven"), "{}", c.text());
    c.send(b"y");
    c.wait_for("Change 2 of 2");
    assert!(card_of(&c).contains("+ line twenty-seven") && !card_of(&c).contains("line three"), "{}", c.text());
    assert_eq!(on_disk(), original, "nothing is written until every change has an answer");
    c.send(b"s");
    c.wait_until("the first answer", |c| answers(c) == 1);
    assert_eq!(on_disk(), original.replace("line 3\n", "line three\n"));
    let told = tool_result_in(&bodies.lock().unwrap()[1]);
    assert!(told.contains("Applied 1 of 2 changes") && told.contains("NOT made") && told.contains("+ line twenty-seven"), "{told}");

    // An edit approved whole.
    ask_for_tool(&c, "edit_file", serde_json::json!({"path": "notes.txt", "old_text": "line 10\n", "new_text": "line ten\n"}));
    c.wait_for("Edit ");
    c.wait_for("+ line ten");
    assert!(!c.text().contains("Review each change"), "one hunk needs no one-at-a-time review");
    c.send(b"y");
    c.wait_until("the second answer", |c| answers(c) == 2);
    assert!(on_disk().contains("line 9\nline ten\nline 11\n"));
    assert!(tool_result_in(&bodies.lock().unwrap()[3]).starts_with("Edited "));

    // A new file, declined with a word about what to do instead.
    ask_for_tool(&c, "write_file", serde_json::json!({"path": "fresh.txt", "content": "hello\nworld\n"}));
    c.wait_for("Create ");
    c.wait_for("+ world");
    c.send(b"n");
    c.wait_for("What should the agent do instead?");
    c.send(b"not now\r");
    c.wait_until("the third answer", |c| answers(c) == 3);
    assert!(!sb.root.join("home/fresh.txt").exists());
    assert!(tool_result_in(&bodies.lock().unwrap()[5]).contains("declined this and said: not now"));

    // A change that would change nothing says so without asking.
    ask_for_tool(&c, "write_file", serde_json::json!({"path": "notes.txt", "content": on_disk()}));
    c.wait_until("the fourth answer", |c| answers(c) == 4);
    assert!(tool_result_in(&bodies.lock().unwrap()[7]).contains("already has exactly this content"));
}

#[test]
fn autopilot_changes_files_without_a_review() {
    let (sb, bodies) = agent_sandbox("auto");
    let c = sb.attach("main", 110, 30);
    c.wait_for("ready$");
    c.send(CTRL_SPACE);
    c.wait_for("AGENT");
    ask_for_tool(&c, "write_file", serde_json::json!({"path": "made/by-agent.txt", "content": "one\ntwo\n"}));
    c.wait_for("Verdict: the tool answered.");
    assert_eq!(std::fs::read_to_string(sb.root.join("home/made/by-agent.txt")).unwrap(), "one\ntwo\n");
    assert!(tool_result_in(&bodies.lock().unwrap()[1]).starts_with("Created "));
    assert!(!c.text().contains("Create "));
}

/// A sandbox whose `[agent]` table carries extra settings.
fn sandbox_with_agent_settings(settings: &str) -> (Sandbox, Arc<Mutex<Vec<String>>>) {
    let (port, bodies) = mock_model();
    let sb = Sandbox::new();
    sb.write_config(&format!(
        "[agent]\nprovider = \"mock\"\nmodel = \"mock-model\"\n{settings}\n\n[providers.mock]\nkind = \"openai\"\nbase_url = \"http://127.0.0.1:{port}/v1\"\n"
    ));
    (sb, bodies)
}

#[test]
fn the_users_allow_and_deny_lists_decide_before_asking() {
    // "Ask first", but printf is on the allow list: it runs without a question.
    let (sb, _bodies) = sandbox_with_agent_settings("autonomy = \"ask\"\nallow = [\"printf\"]");
    let c = sb.attach("main", 100, 30);
    c.wait_for("ready$");
    c.send(CTRL_SPACE);
    c.wait_for("AGENT");
    c.send(b"go\r");
    c.wait_for("Verdict: the shell printed forty-two.");
    assert!(!c.text().contains("Run this command?"));

    // On the deny list it does not run even on autopilot, and the model is told why.
    let (sb, bodies) = sandbox_with_agent_settings("autonomy = \"auto\"\ndeny = [\"printf\"]");
    let c = sb.attach("main", 100, 30);
    c.wait_for("ready$");
    c.send(CTRL_SPACE);
    c.wait_for("AGENT");
    c.send(b"go\r");
    c.wait_for("Verdict: no command output reached me.");
    assert!(c.text().contains("not allowed: your deny list has `printf`"), "{}", c.text());
    assert!(!c.text().contains("agent-ran-42"), "a denied command must not run");
    let sent = bodies.lock().unwrap();
    // The model knows the rule from the start, and is told when it runs into it.
    assert!(sent[0].contains("Commands the user has ruled out") && sent[0].contains("`printf`"), "{}", sent[0]);
    assert!(tool_result_in(&sent[1]).contains("matches `printf` on their deny list"), "{}", sent[1]);
}

#[test]
fn secrets_are_hidden_from_the_model_but_not_from_the_user() {
    // Made-up values in the shape of real ones.
    let token = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
    let (sb, bodies) = agent_sandbox("safe");
    std::fs::write(sb.root.join("home/service.env"), format!("NAME=demo\nGITHUB_TOKEN={token}\nDB_PASSWORD=correct-horse-battery\n")).unwrap();
    let c = sb.attach("main", 110, 30);
    c.wait_for("ready$");
    // Something the user ran themselves, which the agent is shown as context.
    c.send(format!("echo leaked {token}\r").as_bytes());
    c.wait_until("the command to finish", |c| c.text().matches("leaked ghp_").count() == 2);
    c.send(CTRL_SPACE);
    c.wait_for("AGENT");

    // A file read by the agent.
    ask_for_tool(&c, "read_file", serde_json::json!({"path": "service.env"}));
    c.wait_for("Verdict: the tool answered.");
    c.wait_for("2 secrets hidden from the model");
    // A command run by the agent: the terminal shows the real output, the model does not get it.
    ask_for_tool(&c, "run_command", serde_json::json!({"command": "cat service.env"}));
    c.wait_until("the second answer", |_| bodies.lock().unwrap().len() == 4);
    c.wait_until("the real value in the terminal", |c| c.text().contains("DB_PASSWORD=correct-horse-battery"));

    let sent = bodies.lock().unwrap();
    for (i, body) in sent.iter().enumerate() {
        assert!(!body.contains("ghp_abcdef") && !body.contains("correct-horse"), "request {i} carries a secret: {body}");
    }
    assert!(sent[0].contains("echo leaked [redacted: GitHub token]"), "{}", sent[0]);
    let file = tool_result_in(&sent[1]);
    assert!(file.contains("NAME=demo") && file.contains("GITHUB_TOKEN=[redacted: GitHub token]") && file.contains("DB_PASSWORD=[redacted: secret]"), "{file}");
    assert!(tool_result_in(&sent[3]).contains("DB_PASSWORD=[redacted: secret]"));
}

#[test]
fn redaction_can_be_turned_off() {
    let (sb, bodies) = sandbox_with_agent_settings("autonomy = \"safe\"\nredact_secrets = false");
    std::fs::write(sb.root.join("home/service.env"), "DB_PASSWORD=correct-horse-battery\n").unwrap();
    let c = sb.attach("main", 110, 30);
    c.wait_for("ready$");
    c.send(CTRL_SPACE);
    c.wait_for("AGENT");
    ask_for_tool(&c, "read_file", serde_json::json!({"path": "service.env"}));
    c.wait_for("Verdict: the tool answered.");
    assert!(tool_result_in(&bodies.lock().unwrap()[1]).contains("DB_PASSWORD=correct-horse-battery"));
}

#[test]
fn a_command_that_outlives_its_wait_is_reported_with_signs_of_life() {
    let (sb, bodies) = agent_sandbox("safe");
    let c = sb.attach("main", 110, 30);
    c.wait_for("ready$");
    c.send(CTRL_SPACE);
    c.wait_for("AGENT");
    // Told to wait three seconds for a command that takes far longer. (Three,
    // so that "quiet for two seconds or more" holds with room to spare.)
    ask_for_tool(&c, "run_command", serde_json::json!({"command": "sleep 30", "timeout_secs": 3}));
    c.wait_until("the report on the running command", |_| bodies.lock().unwrap().len() == 2);
    let report = tool_result_in(&bodies.lock().unwrap()[1]);
    assert!(report.contains("status: still running after 3s"), "{report}");
    // Which program it is, and that it is neither printing nor computing.
    assert!(report.contains("process: `sleep` (pid "), "{report}");
    assert!(report.contains("activity: no new output for") && report.contains("of processor time used so far"), "{report}");
}

#[test]
fn a_pager_the_agent_walks_into_is_closed_for_it() {
    let (sb, bodies) = agent_sandbox("auto");
    let c = sb.attach("main", 110, 30);
    c.wait_for("ready$");
    c.send(CTRL_SPACE);
    c.wait_for("AGENT");
    // 300 lines through a pager: it shows a screenful and waits for a key that nobody will press.
    ask_for_tool(&c, "run_command", serde_json::json!({"command": "seq 1 300 | LESS= less"}));
    c.wait_until("the command's result", |_| bodies.lock().unwrap().len() == 2);
    let result = tool_result_in(&bodies.lock().unwrap()[1]);
    assert!(result.contains("opened a pager") && result.contains("exit code: 0"), "{result}");
    // The first screen came back as plain text, and the advice on how to get the rest.
    assert!(result.contains("\n1\n2\n3\n") && result.contains("--no-pager"), "{result}");
    assert!(!result.contains("\n300"), "{result}");
    c.wait_for("it opened a pager, closed after the first screen");
    // The shell is back at its prompt, not stuck in the pager.
    c.send(CTRL_SPACE);
    c.send(b"echo back-$((5+5))\r");
    c.wait_for("back-10");
}

#[test]
fn a_key_typed_into_astrum_is_stored_and_used() {
    let key = "test-key-in-the-store";
    let (sb, _bodies) = agent_sandbox("safe");
    let stored = || std::fs::read_to_string(sb.root.join("keychain.json")).unwrap_or_default();
    let mut c = sb.attach("main", 110, 30);
    c.wait_for("ready$");
    c.send(CTRL_SPACE);
    c.wait_for("AGENT");
    c.send(b"/key\r");
    c.wait_for("mock · no key (local, none needed)");

    // Typing a key: it is drawn as dots and never appears on screen.
    c.send(b"/key mock\r");
    c.wait_for("Paste or type the API key");
    c.send(key.as_bytes());
    c.wait_for("•••••••••••••••••••••");
    assert!(!c.text().contains(key));
    c.send(b"\r");
    c.wait_for("The key for mock is stored");
    assert!(stored().contains(key) && !c.text().contains(key));
    c.send(b"/key\r");
    c.wait_for("mock · key from the password store");

    // The provider is called with it, now and after the session is restarted.
    c.send(b"go\r");
    c.wait_for("Verdict: the key arrived.");
    assert!(sb.run(&["kill", "-s", "main", "--forget"]).contains("Stopped"));
    c.wait_exit();
    for file in saved_conversations(&sb) {
        assert!(!std::fs::read_to_string(file).unwrap().contains(key), "a key must not end up in a saved conversation");
    }
    let c = sb.attach("main", 110, 30);
    c.wait_for("ready$");
    c.send(CTRL_SPACE);
    c.wait_for("AGENT");
    c.send(b"go\r");
    c.wait_for("Verdict: the key arrived.");

    // Removing it.
    c.send(b"/key mock remove\r");
    c.wait_for("The stored key for mock is removed");
    assert!(!stored().contains(key));

    // A key half typed when key entry is cut short is dropped, never shown as text.
    let half = "half-typed-secret";
    c.send(b"/key mock\r");
    c.wait_for("Paste or type the API key");
    c.send(half.as_bytes());
    c.wait_for("•••••••••••••••••");
    c.send(b"\x03");
    c.wait_until("key entry to be abandoned", |c| !c.text().contains("•••"));
    assert!(!c.text().contains(half), "{}", c.text());
    // A turn that ends while a key is being typed leaves it hidden and still storable.
    c.send(b"/clear\r");
    c.send(b"tell me SLOWLY\r");
    c.send(b"/key mock\r");
    c.wait_for("Paste or type the API key");
    c.send(half.as_bytes());
    c.wait_for("Verdict");
    std::thread::sleep(Duration::from_millis(300));
    assert!(c.text().contains("•••••••••••••••••") && !c.text().contains(half), "{}", c.text());
    c.send(b"\r");
    c.wait_for("The key for mock is stored");
    assert!(stored().contains(half));

    // Whatever follows /key is never repeated back: it might be a key.
    c.send(b"/key sk-not-a-provider-name-123\r");
    c.wait_for("takes the name of a provider");
    assert!(!c.text().contains("sk-not-a-provider"), "{}", c.text());
    c.send(b"/key mock given-on-the-line-456\r");
    c.wait_until("the key given on the line to be stored", |_| stored().contains("given-on-the-line-456"));
    std::thread::sleep(Duration::from_millis(300));
    assert!(!c.text().contains("given-on-the-line"), "{}", c.text());
    for file in saved_conversations(&sb) {
        let text = std::fs::read_to_string(file).unwrap();
        assert!(!text.contains(half) && !text.contains("given-on-the-line") && !text.contains("sk-not-a-provider"), "a key reached a saved conversation");
    }
}

#[test]
fn reload_picks_up_new_providers_and_keys_without_a_restart() {
    let (port, _bodies) = mock_model();
    let sb = Sandbox::new();
    let config = |extra: &str| {
        format!(
            "[agent]\nprovider = \"mock\"\nmodel = \"mock-model\"\nautonomy = \"safe\"\n\n[providers.mock]\nkind = \"openai\"\nbase_url = \"http://127.0.0.1:{port}/v1\"\napi_key_env = \"MOCK_PROVIDER_KEY\"\n{extra}"
        )
    };
    sb.write_config(&config(""));
    let c = sb.attach("main", 110, 30);
    c.wait_for("ready$");
    c.send(CTRL_SPACE);
    c.wait_for("AGENT");
    // The session started without the key variable set.
    c.send(b"go\r");
    c.wait_for("Verdict: the shell printed forty-two.");

    // The config file gains a provider, and a shell now has the variable:
    // `astrum reload` from that shell brings both into the running session.
    sb.write_config(&config("\n[providers.second]\nkind = \"openai\"\nbase_url = \"http://127.0.0.1:1/v1\"\n"));
    let report = sb.run_with(&["reload", "-s", "main"], &[("MOCK_PROVIDER_KEY", "test-key-in-the-store")]);
    assert!(report.contains("`second`") && report.contains("`mock` · key from $MOCK_PROVIDER_KEY"), "{report}");
    c.send(b"/clear\r");
    c.wait_until("an empty panel", |c| !c.text().contains("Verdict"));
    c.send(b"go\r");
    c.wait_for("Verdict: the key arrived.");
    c.send(b"/providers\r");
    c.wait_for("second · OpenAI-compatible");

    // From inside the panel, the config file and the password store are read again.
    sb.write_config(&config(""));
    c.send(b"/reload\r");
    c.wait_for("Reloaded 5 providers");
    assert!(c.text().contains("Environment variables are as they were"), "{}", c.text());

    // A config file that cannot be read changes nothing.
    sb.write_config("this is [not toml");
    let report = sb.run(&["reload", "-s", "main"]);
    assert!(report.contains("Nothing was changed"), "{report}");
    assert!(sb.run(&["reload", "-s", "nosuch"]).contains("is not running"));
}

#[test]
fn typing_is_not_taken_as_the_answer_to_a_question_that_just_appeared() {
    let (sb, bodies) = agent_sandbox("ask");
    let c = sb.attach("main", 100, 30);
    c.wait_for("ready$");
    c.send(CTRL_SPACE);
    c.wait_for("AGENT");
    // The model takes over a second to ask to run a command. Meanwhile the
    // user is typing, and "y" is the key that would approve it. They type
    // until the question is on screen and a few keys past it, however long
    // this machine takes to get there: nothing here counts on its speed.
    c.send(b"go SLOWLY\r");
    c.wait_until("the request", |_| bodies.lock().unwrap().len() == 1);
    let mut past_the_question = 0;
    for _ in 0..400 {
        c.send(b"y");
        std::thread::sleep(Duration::from_millis(50));
        if c.text().contains("Run this command?") {
            past_the_question += 1;
            if past_the_question == 50 {
                break;
            }
        }
    }
    // They typed on for two and a half seconds with the question up, and
    // none of it answered: only a key after a pause does.
    assert_eq!(past_the_question, 50, "the question never appeared: {}", c.text());
    std::thread::sleep(Duration::from_millis(300));
    assert!(!c.text().contains("agent-ran-42"), "typing must not approve the command: {}", c.text());
    assert!(c.text().contains("Run this command?"));
    // A deliberate answer, once the typing has stopped for a moment, counts.
    std::thread::sleep(Duration::from_millis(1300));
    c.send(b"y");
    c.wait_for("agent-ran-42");
    c.wait_for("Verdict: the shell printed forty-two.");

    // Stopping is never held back: Ctrl+C in the middle of the same typing
    // ends the turn at once, and the command is not run.
    c.send(b"\x03");
    c.wait_until("the first question gone", |c| !c.text().contains("Run this command?"));
    c.send(b"SLOWLY TOOLCALL run_command {\"command\":\"printf never-%d 7\"}\r");
    c.wait_until("the next request", |_| bodies.lock().unwrap().len() == 3);
    c.wait_until("the second question", |c| {
        c.send(b"y");
        std::thread::sleep(Duration::from_millis(50));
        c.text().contains("Run this command?")
    });
    c.send(b"yy\x03");
    c.wait_for("Stopped.");
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(bodies.lock().unwrap().len(), 3, "nothing more went to the model");
    assert!(!c.text().contains("never-7") && !c.text().contains("Run this command?"), "{}", c.text());
}

#[test]
fn typing_into_the_terminal_is_no_way_around_the_checks() {
    let (sb, bodies) = sandbox_with_agent_settings("autonomy = \"auto\"\ndeny = [\"git push\"]");
    let c = sb.attach("main", 110, 30);
    c.wait_for("ready$");
    c.send(CTRL_SPACE);
    c.wait_for("AGENT");
    let result = |n: usize| tool_result_in(&bodies.lock().unwrap()[n]);

    // At the shell's prompt there is nothing to type into: a command goes through run_command.
    ask_for_tool(&c, "send_input", serde_json::json!({"text": "echo typed-by-$((40+2))", "key": "enter"}));
    c.wait_until("the first answer", |_| bodies.lock().unwrap().len() == 2);
    assert!(result(1).contains("Nothing is running in the terminal"), "{}", result(1));
    assert!(!c.text().contains("typed-by-42"));

    // With a program running that takes input, the deny list still applies to what is typed...
    c.send(CTRL_SPACE);
    c.send(b"cat\r");
    c.wait_until("cat to be running", |c| c.lines()[0].contains("cat"));
    c.send(CTRL_SPACE);
    ask_for_tool(&c, "send_input", serde_json::json!({"text": "git push origin main", "key": "enter"}));
    c.wait_until("the second answer", |_| bodies.lock().unwrap().len() == 4);
    assert!(result(3).contains("on their deny list") && result(3).contains("Nothing was typed"), "{}", result(3));
    // ...and something destructive asks, autopilot or not.
    ask_for_tool(&c, "send_input", serde_json::json!({"text": "sudo reboot", "key": "enter"}));
    c.wait_for("Careful: this runs with elevated privileges.");
    c.send(b"\x1b");
    c.wait_until("the third answer", |_| bodies.lock().unwrap().len() == 6);
    assert!(result(5).contains("declined"), "{}", result(5));
    // Harmless input is typed without asking, as autopilot allows.
    ask_for_tool(&c, "send_input", serde_json::json!({"text": "plain-line", "key": "enter"}));
    c.wait_until("the fourth answer", |_| bodies.lock().unwrap().len() == 8);
    assert!(result(7).contains("Typed.") && c.text().contains("plain-line"), "{}", result(7));
}

#[test]
fn a_question_is_not_answered_by_keys_meant_for_the_shell() {
    let (sb, _bodies) = agent_sandbox("ask");
    let c = sb.attach("main", 100, 30);
    c.wait_for("ready$");
    // Asked from the shell prompt; the user carries on typing there. "a" is
    // the key that would approve everything for the rest of the session.
    c.send(b"# go SLOWLY\r");
    std::thread::sleep(Duration::from_millis(900));
    for _ in 0..14 {
        c.send(b"a");
        std::thread::sleep(Duration::from_millis(50));
    }
    c.wait_for("Run this command?");
    std::thread::sleep(Duration::from_millis(300));
    assert!(c.text().contains("Run this command?") && !c.text().contains("agent-ran-42"), "{}", c.text());
    std::thread::sleep(Duration::from_millis(1300));
    c.send(b"y");
    c.wait_for("agent-ran-42");
}

#[test]
fn a_question_fixed_mid_line_arrives_whole() {
    let (sb, bodies) = agent_sandbox("safe");
    let c = sb.attach("main", 100, 30);
    c.wait_for("ready$");
    // A typo, the cursor moved back to mend it, and Enter pressed from the middle of the line.
    c.send(b"# what is six tmes seven");
    for _ in 0..9 {
        c.send(b"\x1b[D");
    }
    c.send(b"i");
    std::thread::sleep(Duration::from_millis(200));
    c.send(b"\r");
    c.wait_for("Verdict: the shell printed forty-two.");
    assert!(bodies.lock().unwrap()[0].contains("what is six times seven"), "{}", bodies.lock().unwrap()[0]);
}

#[test]
fn the_users_own_pager_is_left_alone() {
    let (sb, bodies) = agent_sandbox("safe");
    let c = sb.attach("main", 110, 30);
    c.wait_for("ready$");
    // The user is reading something in a pager and has the agent wait for it.
    c.send(b"seq 1 300 | LESS= less; echo slept-$((1+1))-done\r");
    c.wait_until("the offer to watch", |c| c.lines()[29].contains("watch"));
    c.send(CTRL_B);
    c.wait_for("PREFIX");
    c.send(b"o");
    c.wait_for("Watch `seq 1 300");
    // Well past the point where a pager of the agent's own would have been closed.
    std::thread::sleep(Duration::from_millis(2500));
    assert_eq!(bodies.lock().unwrap().len(), 1, "the agent is still waiting on the command");
    assert!(!c.text().contains("slept-2-done"), "the pager must still be open: {}", c.text());
    // The user closes it themselves; then the agent hears that it ended.
    c.send(b"q");
    c.wait_for("Verdict: your command finished.");
}

#[test]
fn an_empty_new_file_is_still_asked_about_and_created() {
    let (sb, bodies) = agent_sandbox("ask");
    let c = sb.attach("main", 110, 30);
    c.wait_for("ready$");
    c.send(CTRL_SPACE);
    c.wait_for("AGENT");
    ask_for_tool(&c, "write_file", serde_json::json!({"path": "pkg/__init__.py", "content": ""}));
    c.wait_for("Create ");
    c.wait_for("(a new, empty file)");
    assert!(!sb.root.join("home/pkg/__init__.py").exists());
    c.send(b"y");
    c.wait_until("the answer", |_| bodies.lock().unwrap().len() == 2);
    assert_eq!(std::fs::read_to_string(sb.root.join("home/pkg/__init__.py")).unwrap(), "");
    assert!(tool_result_in(&bodies.lock().unwrap()[1]).starts_with("Created "));
}

#[test]
fn bedrock_converse_stream_drives_the_same_flow() {
    let (port, bodies) = mock_model();
    let sb = Sandbox::new();
    sb.write_config(&format!(
        "[agent]\nprovider = \"aws\"\nmodel = \"vendor.mock-model-v1:0\"\nautonomy = \"safe\"\n\n[providers.aws]\nkind = \"bedrock\"\nbase_url = \"http://127.0.0.1:{port}\"\napi_key = \"test-key\"\nregion = \"us-test-1\"\n"
    ));
    let c = sb.attach("main", 100, 30);
    c.wait_for("ready$");
    c.send(CTRL_SPACE);
    c.wait_for("AGENT");
    c.send(b"go\r");
    c.wait_for("Verdict: Bedrock saw forty-two.");

    let sent = bodies.lock().unwrap();
    assert_eq!(sent.len(), 2);
    // The model id is percent-encoded in the path, and an API key goes out as a bearer token.
    assert!(sent[0].starts_with("POST /model/vendor.mock-model-v1%3A0/converse-stream"), "{}", sent[0]);
    assert!(sent[0].contains("\"toolSpec\"") && sent[0].contains("\"inputSchema\":{\"json\""), "{}", sent[0]);
    assert!(sent[1].contains("\"toolResult\"") && sent[1].contains("\"toolUseId\":\"tu_1\"") && sent[1].contains("\"status\":\"success\""), "{}", sent[1]);
}

#[test]
fn out_of_the_box_the_terminals_own_colors_are_kept() {
    // With nothing chosen, Astrum paints no background and no colors of
    // its own: the bars, the shell, and what programs print all stay in
    // the terminal's.
    let sb = Sandbox::new();
    let c = sb.attach_env("main", 90, 20, &[("COLORTERM", "truecolor")]);
    c.wait_for("ready$");
    c.wait_for("SHELL");
    c.send(b"printf '\\033[32mplain-green\\033[0m\\n'\r");
    c.wait_for("plain-green");
    let raw = c.raw();
    assert!(!raw.contains("48;2;") && !raw.contains("38;2;"), "no colors of Astrum's own");
    assert!(!raw.contains("48;5;"), "no background painted over the terminal's");

    // A chosen theme colors Astrum's bars and, with them, what programs draw.
    let sb = Sandbox::new();
    sb.write_config("theme = \"mcdermott\"\n");
    let c = sb.attach_env("main", 90, 20, &[("COLORTERM", "truecolor")]);
    c.wait_for("ready$");
    c.send(b"printf '\\033[32mthemed-green\\033[0m\\n'\r");
    c.wait_for("themed-green");
    let raw = c.raw();
    // The background, and green as this theme has it (#ff0068).
    assert!(raw.contains("48;2;4;1;2"), "the theme's background behind the shell");
    assert!(raw.contains("38;2;255;0;104"), "the theme's own green for a program's green text");

    // And it can be kept to Astrum's bars alone.
    let sb = Sandbox::new();
    sb.write_config("theme = \"mcdermott\"\ntheme_terminal = false\n");
    let c = sb.attach_env("main", 90, 20, &[("COLORTERM", "truecolor")]);
    c.wait_for("ready$");
    c.send(b"printf '\\033[32mhost-green\\033[0m\\n'\r");
    c.wait_for("host-green");
    let raw = c.raw();
    assert!(raw.contains("48;2;10;2;5"), "the status bar in the theme's color");
    assert!(!raw.contains("48;2;4;1;2"), "the shell's background left alone");
}

#[test]
fn themes_of_the_users_own_and_one_for_a_light_terminal() {
    let sb = Sandbox::new();
    sb.write_config(
        "theme = \"mine\"\ntheme_light = \"paper\"\n\n[themes.mine]\nlabel = \"Mine\"\nbased_on = \"matrix\"\nbg = \"#010203\"\n\n[themes.paper]\nlabel = \"Paper\"\nbased_on = \"catppuccin-latte\"\nbg = \"#fffef0\"\n\n[themes.odd]\naccent = \"pinkish\"\n",
    );
    let listing = sb.run(&["themes"]);
    assert!(listing.lines().any(|l| l.starts_with("mine") && l.contains("Mine") && l.contains("(yours)")), "{listing}");
    assert!(listing.lines().any(|l| l.starts_with("matrix")) && listing.contains("pinkish"), "{listing}");

    // A terminal with a dark background gets the one theme, a light one the other.
    let dark = sb.attach_env("dark", 90, 20, &[("COLORTERM", "truecolor"), ("TEST_TERMINAL_BG", "1010/1010/1818")]);
    dark.wait_for("ready$");
    dark.wait_for("SHELL");
    assert!(dark.raw().contains("48;2;1;2;3"), "the dark terminal is drawn in the user's dark theme");
    assert!(!dark.raw().contains("48;2;255;254;240"));
    let light = sb.attach_env("light", 90, 20, &[("COLORTERM", "truecolor")]);
    light.wait_for("ready$");
    light.wait_until("the light theme's background", |c| c.raw().contains("48;2;255;254;240"));

    // The theme list offers them by their labels; choosing one there, in the
    // light terminal, changes the light theme and leaves the dark one alone.
    light.send(CTRL_SPACE);
    light.wait_for("AGENT");
    light.send(b"/theme\r");
    light.wait_for("Previewing as you move");
    let list = light.text();
    assert!(list.contains("Mine") && list.contains("Paper"), "{list}");
    light.send(b"\x1b");
    light.wait_until("the list to close", |c| !c.text().contains("Previewing as you move"));
    light.send(b"/theme nord\r");
    light.wait_until("the state to be saved", |_| {
        std::fs::read_to_string(sb.root.join("cfg/state.toml")).is_ok_and(|t| t.contains("theme_light = \"nord\""))
    });
    let state = std::fs::read_to_string(sb.root.join("cfg/state.toml")).unwrap();
    assert!(!state.contains("theme = "), "only what was chosen is remembered: {state}");
}

#[test]
fn colors_fall_back_to_the_256_palette_without_truecolor() {
    let sb = Sandbox::new();
    sb.write_config("theme = \"astrum\"\n");
    // A terminal that does not announce 24-bit color (the built-in macOS
    // terminal on older systems) must never be sent 24-bit sequences.
    let plain = sb.attach("plain", 80, 20);
    plain.wait_for("ready$");
    plain.wait_for("SHELL");
    let raw = plain.raw();
    assert!(!raw.contains("38;2;") && !raw.contains("48;2;"), "24-bit color sent to a 256-color terminal");
    assert!(raw.contains("48;5;"), "the theme should still be drawn, from the 256-color palette");

    // One that does announce it gets the theme's exact colors.
    let rich = sb.attach_env("rich", 80, 20, &[("COLORTERM", "truecolor")]);
    rich.wait_for("SHELL");
    assert!(rich.raw().contains("48;2;"));
}

#[test]
fn each_terminal_has_its_own_view_of_the_session() {
    let sb = Sandbox::new();
    let mut small = sb.attach("shared", 70, 16);
    small.wait_for("ready$");
    small.wait_until("the status bar on the small terminal's last row", |c| c.lines()[15].contains("SHELL"));

    // A second, larger terminal opens the same session. Each is drawn edge
    // to edge at its own size. They are looking at the same tab, which is
    // sized for the one in use.
    let mut large = sb.attach("shared", 100, 30);
    large.wait_until("the status bar on the large terminal's last row", |c| c.lines()[29].contains("SHELL"));
    large.send(b"echo cols=$(tput cols)\r");
    large.wait_for("cols=100");
    // The small one keeps its own tab bar and status bar, and shows the tab
    // as it is laid out for the other, saying so.
    small.wait_until("the small terminal's own frame around the other's tab", |c| {
        c.lines()[15].contains("SHELL") && c.lines()[15].contains("menu") && c.text().contains("in use in another window") && c.text().contains("cols=100")
    });
    // Using the small terminal brings the tab to its size.
    small.send(b" ");
    small.wait_until("the note to go", |c| !c.text().contains("in use in another window"));
    small.send(b"echo cols=$(tput cols)\r");
    small.wait_for("cols=70");
    large.wait_until("the large terminal to show the tab at the smaller size, in its own frame", |c| {
        c.lines()[29].contains("SHELL") && c.text().contains("in use in another window") && c.text().contains("cols=70")
    });

    // Each terminal has its own tab in front. A tab opened in one is listed
    // in the other, which stays where it was.
    large.send(CTRL_B);
    large.wait_for("PREFIX");
    std::thread::sleep(Duration::from_millis(200));
    assert!(!small.text().contains("PREFIX"), "one terminal's prefix key is not the other's");
    large.send(b"c");
    large.wait_until("the new tab in front on the large terminal", |c| c.lines()[0].contains(" 2 ") && !c.text().contains("cols=70"));
    large.wait_for("ready$");
    large.send(b"echo only-here-$((3+3))\r");
    large.wait_for("only-here-6");
    small.wait_until("the new tab listed on the small terminal", |c| c.lines()[0].contains(" 2 "));
    assert!(small.text().contains("cols=70") && !small.text().contains("only-here"), "{}", small.text());
    // The first tab is the small terminal's alone now: its own size, no note.
    assert!(!small.text().contains("in use in another window"));
    // A menu opened in one is not shown in the other.
    let (col, row) = small.position_of("≡ menu").unwrap();
    small.click(col + 2, row);
    small.wait_for("Split right");
    std::thread::sleep(Duration::from_millis(200));
    assert!(!large.text().contains("Split right"));
    small.send(b"\x1b");
    small.wait_until("the menu to close", |c| !c.text().contains("Split right"));

    // The small terminal goes to the second tab as well: there it sees the
    // large one's layout until it uses the tab itself.
    let tab2 = small.find_on_row(0, " 2 ").unwrap() + 1;
    small.click(tab2, 0);
    small.wait_until("the second tab on the small terminal", |c| c.text().contains("only-here-6") && c.text().contains("in use in another window"));
    large.wait_until("the large terminal undisturbed", |c| c.text().contains("only-here-6") && !c.text().contains("in use in another window"));
    // A click inside it brings it over, and does nothing else.
    small.click(20, 8);
    small.wait_until("the tab at the small terminal's size", |c| !c.text().contains("in use in another window"));
    large.wait_until("the note on the large terminal", |c| c.text().contains("in use in another window"));
    large.send(b" ");
    large.wait_until("the tab back with the large terminal", |c| !c.text().contains("in use in another window"));

    // When that tab's shell ends, both fall back to the first tab.
    large.send(b"exit\r");
    large.wait_until("the large terminal back on the first tab", |c| !c.lines()[0].contains(" 2 ") && c.text().contains("cols=70"));
    small.wait_until("the small terminal back on the first tab", |c| !c.lines()[0].contains(" 2 ") && c.text().contains("cols=70"));

    // One terminal detaching leaves the other as it is.
    large.send(CTRL_B);
    large.send(b"d");
    large.wait_exit();
    small.send(b"echo still-$((4+4))\r");
    small.wait_for("still-8");
    small.wait_until("the tab at the small terminal's size", |c| !c.text().contains("in use in another window"));
    small.send(CTRL_B);
    small.send(b"d");
    small.wait_exit();

    // Coming back, a terminal lands on the tab that was in front.
    let c = sb.attach("shared", 70, 16);
    c.wait_for("still-8");
}

#[test]
fn the_agent_key_can_be_changed_in_the_config() {
    let sb = Sandbox::new();
    sb.write_config("agent_key = \"ctrl+backslash\"\n");
    let c = sb.attach("main", 80, 20);
    c.wait_for("ready$");
    assert!(c.lines()[19].contains("^\\ agent"), "the status bar names the configured key: {:?}", c.lines()[19]);
    // Ctrl+Space no longer switches; Ctrl+\ (the byte 0x1c) does.
    c.send(CTRL_SPACE);
    std::thread::sleep(Duration::from_millis(300));
    assert!(c.lines()[19].contains("SHELL"));
    c.send(b"\x1c");
    c.wait_for("AGENT");
}

// ---- pictures -----------------------------------------------------------------

/// A PNG file, 40 pixels square, in base64.
const PNG_40: &str = "iVBORw0KGgoAAAANSUhEUgAAACgAAAAoCAIAAAADnC86AAAALklEQVR42u3NMQ0AAAgDsMnFvwFmgoSnSf9mMy8iFovFYrFYLBaLxWKxWCwW3ykQ9cQ5FuAbUgAAAABJRU5ErkJggg==";

/// Where on the terminal's screen each thing matching `marker` was drawn:
/// the row and column (from zero) the cursor was last sent to before it.
fn drawn_at(raw: &str, marker: &str) -> Vec<(usize, usize)> {
    raw.match_indices(marker)
        .filter_map(|(at, _)| {
            raw[..at].rmatch_indices("\x1b[").find_map(|(start, _)| {
                let rest = &raw[start + 2..at];
                let end = rest.find(|c: char| !(c.is_ascii_digit() || c == ';'))?;
                let (row, col) = rest[..end].split_once(';').filter(|_| rest[end..].starts_with('H'))?;
                Some((row.parse::<usize>().ok()? - 1, col.parse::<usize>().ok()? - 1))
            })
        })
        .collect()
}

/// A sandbox with the test picture in the shells' home directory.
fn sandbox_with_a_picture() -> Sandbox {
    let sb = Sandbox::new();
    std::fs::write(sb.root.join("home/pic.b64"), PNG_40).unwrap();
    sb
}

/// The command that prints the test picture the way `imgcat` does.
const SHOW_FILE: &str = r#"printf '\033]1337;File=inline=1%s:%s\a\n' "$ARGS" "$(cat pic.b64)""#;

#[test]
fn a_picture_a_program_prints_is_shown_and_moves_with_the_text() {
    let sb = sandbox_with_a_picture();
    // A terminal known to take pictures the way iTerm2 does.
    let c = sb.attach_env("main", 100, 30, &[("TERM_PROGRAM", "iTerm.app")]);
    c.wait_for("ready$");
    c.send(format!("clear; echo top-1; echo top-2; echo top-3; {SHOW_FILE}; echo below-$((1+1))\r").as_bytes());
    c.wait_for("below-2");
    // 40 pixels square in cells of 10 by 20: four cells by two. Sent on to
    // the terminal as the file it is, to be drawn in exactly those cells.
    let file = format!("\x1b]1337;File=inline=1;width=4;height=2;preserveAspectRatio=1:{PNG_40}\x07");
    c.wait_until("the picture drawn", |c| c.raw().contains(&file));
    // Under the three lines (the tab bar is row 0), with the cells it
    // covers left empty and what was printed next below it.
    assert_eq!(drawn_at(&c.raw(), &file), [(4, 0)]);
    let lines = c.lines();
    assert!(lines[3].starts_with("top-3") && lines[4].trim().is_empty() && lines[5].trim().is_empty() && lines[6].starts_with("below-2"), "{}", c.text());

    // More output scrolls it up three rows: it is drawn again where it now is.
    c.send(b"for i in $(seq 23); do echo line-$i; done\r");
    c.wait_for("line-23");
    c.wait_until("the picture where it scrolled to", |c| drawn_at(&c.raw(), &file).last() == Some(&(1, 0)));
    assert!(c.lines()[1].trim().is_empty() && c.lines()[2].trim().is_empty() && c.lines()[3].starts_with("below-2"), "{}", c.text());

    // Scrolled partly off the top, it is not drawn: a terminal cannot be
    // asked for part of a picture. Its place is taken by text, and nothing
    // of it is left behind.
    let count = drawn_at(&c.raw(), &file).len();
    c.send(b"echo more-$((2+2))\r");
    c.wait_for("more-4");
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(drawn_at(&c.raw(), &file).len(), count);
    assert!(c.lines()[1].starts_with("below-2") && !c.text().contains('░') && !c.text().contains('\u{10FFFD}'), "{}", c.text());

    // Looking back through the pane brings it into view again, and it is drawn.
    c.send(CTRL_B);
    c.wait_for("PREFIX");
    c.send(b"[");
    c.wait_for("COPY");
    c.send(b"\x1b[5~");
    c.wait_until("the picture drawn in the history", |c| drawn_at(&c.raw(), &file).len() == count + 1);
    c.send(b"\x1b");
    c.wait_until("the latest output again", |c| c.text().contains("more-4"));

    // The same file sent in several sequences, as some programs do.
    c.send(b"clear; printf '\\033]1337;MultipartFile=inline=1\\a'; printf '\\033]1337;FilePart=%s\\a' \"$(cut -c1-80 pic.b64)\" \"$(cut -c81- pic.b64)\"; printf '\\033]1337;FileEnd\\a\\n'; echo below-$((1+1))\r");
    c.wait_until("the file drawn from its pieces", |c| drawn_at(&c.raw(), &file).len() == count + 2 && drawn_at(&c.raw(), &file).last() == Some(&(1, 0)));

    // What is saved of the pane has no trace of the marks the picture
    // leaves in the grid.
    assert!(sb.run(&["kill", "-s", "main"]).contains("Stopped"));
    let saved = saved_session(&sb, "main");
    let texts: Vec<String> = std::fs::read_dir(&saved).unwrap().flatten().filter(|e| e.path().extension().is_some_and(|x| x == "txt")).map(|e| std::fs::read_to_string(e.path()).unwrap()).collect();
    assert!(!texts.is_empty() && texts.iter().all(|t| t.contains("below-2") && !t.contains('\u{10FFFD}')), "{texts:?}");
}

#[test]
fn where_a_picture_cannot_be_shown_a_line_says_what_it_is() {
    let sb = sandbox_with_a_picture();
    // This terminal has no way to show pictures at all.
    let c = sb.attach("main", 100, 30);
    c.wait_for("ready$");
    c.send(format!("clear; ARGS=';width=44'; {SHOW_FILE}; echo below-$((1+1))\r").as_bytes());
    c.wait_for("below-2");
    // It still takes the room it would have: 44 cells wide, and as many
    // rows as keep it square (440 pixels, in rows of 20).
    c.wait_for("PNG 40×40 · this terminal cannot show it");
    let lines = c.lines();
    assert!(lines[1].starts_with("░ PNG 40×40 · this terminal cannot show it ░") && lines[22].starts_with(&"░".repeat(44)) && lines[23].starts_with("below-2"), "{}", c.text());
    assert_eq!(lines[10].trim_end().chars().count(), 44);
    assert!(!c.raw().contains("\x1b]1337;File"));

    // On a terminal that could, with pictures turned off in the config.
    let sb = sandbox_with_a_picture();
    sb.write_config("images = false\n");
    let c = sb.attach_env("main", 100, 30, &[("ASTRUM_IMAGES", "iterm,kitty,sixel")]);
    c.wait_for("ready$");
    c.send(format!("clear; ARGS=';width=44'; {SHOW_FILE}; echo below-$((1+1))\r").as_bytes());
    c.wait_for("PNG 40×40 · images are off");
    std::thread::sleep(Duration::from_millis(300));
    assert!(!c.raw().contains("\x1b]1337;File"));
    // A program that asks is not told pictures can be shown.
    c.send(b"clear; stty -echo; printf '\\033_Gi=5,s=1,v=1,a=q,t=d,f=24;AAAA\\033\\\\'; IFS= read -r -d '\\' -t 1 said; stty echo; echo \"said=[$said]\"\r");
    c.wait_for("said=[]");
}

#[test]
fn a_picture_gives_way_to_a_menu_and_each_terminal_gets_what_it_can_show() {
    let sb = sandbox_with_a_picture();
    let a = sb.attach_env("main", 100, 30, &[("ASTRUM_IMAGES", "iterm")]);
    a.wait_for("ready$");
    a.send(format!("clear; ARGS=';width=44'; {SHOW_FILE}; echo below-$((1+1))\r").as_bytes());
    a.wait_for("below-2");
    let file = format!("\x1b]1337;File=inline=1;width=44;height=22;preserveAspectRatio=1:{PNG_40}\x07");
    a.wait_until("the picture drawn", |c| drawn_at(&c.raw(), &file) == [(1, 0)]);

    // Something drawn over part of it: the picture is taken away for as
    // long as that is there, and comes back after.
    a.send(CTRL_B);
    a.wait_for("PREFIX");
    a.send(b"?");
    a.wait_until("the help over the picture", |c| c.text().contains('░'));
    assert_eq!(drawn_at(&a.raw(), &file).len(), 1);
    a.send(b"\x1b");
    a.wait_until("the picture drawn again", |c| drawn_at(&c.raw(), &file) == [(1, 0), (1, 0)]);
    assert!(!a.text().contains('░'), "{}", a.text());

    // A second terminal on the same tab, of the same size, that cannot
    // show pictures: it is told what is there, and the first keeps its picture.
    let b = sb.attach("main", 100, 30);
    b.wait_for("PNG 40×40 · this terminal cannot show it");
    assert!(!b.raw().contains("\x1b]1337;File"));
    std::thread::sleep(Duration::from_millis(400));
    assert!(!a.text().contains('░'), "{}", a.text());
}

#[test]
fn pictures_sent_the_way_kitty_does_are_answered_placed_and_removed() {
    let sb = sandbox_with_a_picture();
    // This terminal says, when asked, that it understands kitty's graphics.
    let c = sb.attach_env("main", 100, 30, &[("TEST_TERMINAL_SHOWS", "kitty")]);
    c.wait_for("ready$");
    // A program asking the same question of Astrum is told yes.
    c.send(b"stty -echo; printf '\\033_Gi=5,s=1,v=1,a=q,t=d,f=24;AAAA\\033\\\\'; IFS= read -r -d '\\' -t 3 said; stty echo; echo \"said=$(printf %s \"$said\" | tr -d '\\033')\"\r");
    c.wait_for("said=_Gi=5;OK");
    // And no about reading a file it names: only what is sent is taken.
    c.send(b"stty -echo; printf '\\033_Gi=6,a=q,t=f,f=100;L2V0Yy9wYXNzd2Q=\\033\\\\'; IFS= read -r -d '\\' -t 3 said; stty echo; echo \"said=$(printf %s \"$said\" | tr -d '\\033')\"\r");
    c.wait_for("said=_Gi=6;ENOTSUP");

    // A PNG, sent and placed in one go.
    c.send(b"clear; echo top-1; printf '\\033_Ga=T,f=100;%s\\033\\\\\\n' \"$(cat pic.b64)\"; echo below-$((1+1))\r");
    c.wait_for("below-2");
    c.wait_until("the picture handed to the terminal", |c| c.raw().contains(",f=100,q=2,m=0;"));
    let raw = c.raw();
    let id: String = raw.split("\x1b_Ga=t,i=").nth(1).unwrap().chars().take_while(char::is_ascii_digit).collect();
    assert!(raw.contains(&format!("\x1b_Ga=t,i={id},f=100,q=2,m=0;{PNG_40}\x1b\\")));
    let placed = |n: u32| format!("\x1b_Ga=p,i={id},p={n},c=4,r=2,C=1,q=2\x1b\\");
    c.wait_until("the picture placed", |c| drawn_at(&c.raw(), &placed(1)) == [(2, 0)]);
    assert!(c.lines()[2].trim().is_empty() && c.lines()[4].starts_with("below-2"), "{}", c.text());

    // When it moves, the terminal is told to move it: the picture itself
    // is not sent a second time.
    c.send(b"for i in $(seq 23); do echo line-$i; done\r");
    c.wait_for("line-23");
    c.wait_until("the picture where it scrolled to", |c| drawn_at(&c.raw(), &placed(2)) == [(1, 0)]);
    let raw = c.raw();
    assert!(raw.contains(&format!("\x1b_Ga=d,d=i,i={id},p=1,q=2\x1b\\")));
    assert_eq!(raw.matches("\x1b_Ga=t,").count(), 1);

    // The program takes its pictures away again.
    c.send(b"printf '\\033_Ga=d,d=A\\033\\\\'; echo gone-$((3+3))\r");
    c.wait_for("gone-6");
    c.wait_until("the picture taken off the terminal", |c| c.raw().contains(&format!("\x1b_Ga=d,d=i,i={id},p=2,q=2\x1b\\")));

    // Raw pixels, arriving in two pieces: two by two of them, one cell.
    c.send(b"clear; printf '\\033_Ga=T,f=24,s=2,v=2,m=1;/wBo/wBo\\033\\\\\\033_Gm=0;/wBo/wBo\\033\\\\\\n'; echo after-$((4+4))\r");
    c.wait_for("after-8");
    c.wait_until("the pixels handed on", |c| c.raw().contains(",f=24,s=2,v=2,q=2,m=0;/wBo/wBo/wBo/wBo\x1b\\"));
    c.wait_until("and placed", |c| c.raw().contains(",c=1,r=1,C=1,q=2\x1b\\"));
}

#[test]
fn raw_pixels_reach_a_terminal_that_only_takes_files_as_a_png() {
    let sb = Sandbox::new();
    let c = sb.attach_env("main", 100, 30, &[("ASTRUM_IMAGES", "iterm")]);
    c.wait_for("ready$");
    c.send(b"clear; printf '\\033_Ga=T,f=24,s=2,v=2;/wBo/wBo/wBo/wBo\\033\\\\\\n'; echo after-$((4+4))\r");
    c.wait_for("after-8");
    let head = "\x1b]1337;File=inline=1;width=1;height=1;preserveAspectRatio=1:";
    c.wait_until("a file sent", |c| c.raw().contains(head));
    let raw = c.raw();
    let b64: String = raw.split(head).nth(1).unwrap().chars().take_while(|c| *c != '\x07').collect();
    use base64::Engine;
    let png = base64::engine::general_purpose::STANDARD.decode(b64).unwrap();
    assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
    // Two pixels by two, and the pixels as they were sent.
    assert_eq!(&png[16..24], &[0, 0, 0, 2, 0, 0, 0, 2]);
    assert!(png.windows(7).any(|w| w == [0, 255, 0, 104, 255, 0, 104]));
}

#[test]
fn sixel_pictures_go_to_a_terminal_that_draws_sixel() {
    let sb = Sandbox::new();
    // This terminal lists sixel among what it is.
    let c = sb.attach_env("main", 100, 30, &[("TEST_TERMINAL_SHOWS", "sixel")]);
    c.wait_for("ready$");
    // A program that asks what kind of terminal this is finds sixel there too.
    c.send(b"stty -echo; printf '\\033[c'; IFS= read -r -d c -t 3 kind; stty echo; echo \"kind=$(printf %s \"$kind\" | tr -d '\\033')\"\r");
    c.wait_for("kind=[?62;4");
    // And one that asks how large a cell is, to work out how big to draw,
    // is told what this terminal said: 10 pixels by 20.
    c.send(b"stty -echo; printf '\\033[16t'; IFS= read -r -d t -t 3 cell; stty echo; echo \"cell=$(printf %s \"$cell\" | tr -d '\\033')\"\r");
    c.wait_for("cell=[6;20;10");
    // Twenty pixels square: two cells wide and one high. The program does
    // not end the line; the cursor is left below the picture all the same.
    let body = "q\"1;1;20;20#0;2;100;0;40#0!20~-!20~-!20~-!20B";
    c.send(format!("clear; printf '\\033P{body}\\033\\\\'; echo after-$((4+4))\r").as_bytes());
    c.wait_for("after-8");
    let sixel = format!("\x1bP{body}\x1b\\");
    c.wait_until("the picture drawn", |c| drawn_at(&c.raw(), &sixel) == [(1, 0)]);
    assert!(c.lines()[1].trim().is_empty() && c.lines()[2].starts_with("after-8"), "{}", c.text());

    // A terminal that takes pictures some other way cannot be given it.
    let sb = Sandbox::new();
    let c = sb.attach_env("main", 100, 30, &[("ASTRUM_IMAGES", "kitty,iterm")]);
    c.wait_for("ready$");
    let wide = "q\"1;1;600;20#0;2;100;0;40#0!600~-!600~-!600~-!600B";
    c.send(format!("clear; printf '\\033P{wide}\\033\\\\'; echo after-$((4+4))\r").as_bytes());
    c.wait_for("sixel image 600×20 · this terminal cannot show it");
    assert!(!c.raw().contains("\x1bPq"));
    c.send(b"stty -echo; printf '\\033[c'; IFS= read -r -d c -t 3 kind; stty echo; echo \"kind=$(printf %s \"$kind\" | tr -d '\\033')\"\r");
    c.wait_for("kind=[?6");
    assert!(!c.text().contains("kind=[?62;4"));
}

#[test]
fn the_agent_is_told_a_command_printed_a_picture_not_handed_its_bytes() {
    let (sb, bodies) = sandbox_with_agent_settings("autonomy = \"auto\"");
    std::fs::write(sb.root.join("home/pic.b64"), PNG_40).unwrap();
    std::fs::write(sb.root.join("home/show.sh"), format!("echo before; {SHOW_FILE}; echo after\n")).unwrap();
    let c = sb.attach_env("main", 100, 30, &[("ASTRUM_IMAGES", "iterm")]);
    c.wait_for("ready$");
    c.send(CTRL_SPACE);
    c.wait_for("AGENT");
    c.send(b"TOOLCALL run_command {\"command\":\"sh show.sh\"}\r");
    c.wait_for("Verdict: the tool answered.");
    let result = tool_result_in(&bodies.lock().unwrap()[1]);
    assert!(result.contains("before") && result.contains("[PNG 40×40]") && result.contains("after"), "{result}");
    assert!(!result.contains("iVBOR") && !result.contains("1337"), "{result}");
}

#[test]
fn a_terminal_is_sent_nothing_when_nothing_it_shows_has_changed() {
    let sb = Sandbox::new();
    let c = sb.attach("main", 90, 24);
    c.wait_for("ready$");
    // A shell that does not echo what is typed: a key changes nothing.
    c.send(b"stty -echo; echo quiet-$((1+1))\r");
    c.wait_for("quiet-2");
    std::thread::sleep(Duration::from_millis(400));
    let sent = c.screen.lock().unwrap().raw.len();
    for key in b"abcdef" {
        c.send(&[*key]);
        std::thread::sleep(Duration::from_millis(60));
    }
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(c.screen.lock().unwrap().raw.len(), sent, "frames were sent for keys that changed nothing");
    // The keys did arrive, and what they lead to is drawn as ever.
    c.send(b"\r");
    c.wait_until("the shell saying it knows no such command", not_a_command);
}

#[test]
fn a_terminal_that_stops_reading_is_not_sent_a_backlog() {
    let sb = Sandbox::new();
    let c = sb.attach("main", 120, 40);
    c.wait_for("ready$");
    // The terminal's side stops taking anything (a stalled connection),
    // while a program pours out text for a while.
    let pid = c.child.process_id().unwrap() as libc::pid_t;
    // (Repainting the whole screen in colors, over and over: every frame
    // is tens of kilobytes, and one is enough to fill the connection.)
    c.send(br#"awk 'BEGIN{for(f=0;;f++){printf "\033[H";for(r=0;r<36;r++){l="";for(c=0;c<58;c++)l=l sprintf("\033[38;5;%dm%c ",(f+r+c)%200+16,65+(f+c)%26);print l}}}'"#);
    c.send(b"\r");
    std::thread::sleep(Duration::from_millis(600));
    unsafe { libc::kill(pid, libc::SIGSTOP) };
    std::thread::sleep(Duration::from_millis(300));
    let before = work(&sb, "main");
    std::thread::sleep(Duration::from_millis(2500));
    let after = work(&sb, "main");
    unsafe { libc::kill(pid, libc::SIGCONT) };
    // Everything went on being read from the program. But once the
    // connection to a terminal that was not taking anything had filled,
    // no more was drawn for it: two and a half seconds of output would
    // otherwise be some hundreds of frames, all waiting in memory.
    let grew = |key: &str| after[key].as_u64().unwrap() - before[key].as_u64().unwrap();
    assert!(grew("read") > 50_000, "the program went on being read: {after}");
    assert!(grew("frames") <= 3, "drawn for a terminal that had stopped reading: {} frames, {} bytes", grew("frames"), grew("sent"));
    // When it reads again it is brought up to date, and is as usable as before.
    c.send(b"\x03");
    c.send(b"printf '\\033[0m'; clear; echo after-$((1+1))\r");
    c.wait_for("after-2");
}

// ---- what the review before 0.4.0 found ----------------------------------------

#[test]
fn the_config_file_has_the_last_word_on_what_it_sets() {
    let sb = Sandbox::new();
    // Autopilot asked for by the config file is the file's to take away
    // again. (Autopilot chosen in a session is kept through a reload.)
    sb.write_config("[agent]\nautonomy = \"auto\"\n");
    let c = sb.attach("main", 250, 24);
    c.wait_for("ready$");
    c.wait_until("autopilot in the status bar", |c| c.lines()[23].contains("autopilot"));
    sb.write_config("[agent]\nautonomy = \"ask\"\n");
    c.wait_until("asking first again", |c| c.lines()[23].contains("ask first") && !c.lines()[23].contains("autopilot"));

    // The status bar names the help key as it is set.
    sb.write_config("[keys]\nhelp = \"h\"\n");
    c.wait_until("the help key as set", |c| c.lines()[23].contains("^B h help"));
    c.send(CTRL_B);
    c.wait_for("PREFIX");
    // And says nothing of the number and arrow keys once they are other keys.
    assert!(c.lines()[23].contains("1-9 tab"), "{}", c.text());
    c.send(b"\x1b");
    c.wait_until("the prefix let go", |c| !c.lines()[23].contains("PREFIX"));
    sb.write_config("[keys]\ntab_1 = \"alt+1\"\n");
    c.wait_until("the new file taken (the help key is as it was)", |c| c.lines()[23].contains("^B ? help"));
    c.send(CTRL_B);
    c.wait_for("PREFIX");
    std::thread::sleep(Duration::from_millis(200));
    assert!(!c.lines()[23].contains("1-9 tab"), "{}", c.text());
    c.send(b"\x1b");
    c.wait_until("the prefix let go", |c| !c.lines()[23].contains("PREFIX"));

    // Ctrl held on from the prefix is not another key: Ctrl+B Ctrl+C is a new tab.
    c.send(CTRL_B);
    c.wait_for("PREFIX");
    c.send(b"\x03");
    c.wait_until("a second tab", |c| c.lines()[0].contains(" 2 "));
}

#[test]
fn panes_that_stayed_quiet_are_saved_again_when_keeping_them_is_turned_back_on() {
    let sb = Sandbox::new();
    let c = sb.attach_env("main", 90, 24, &[("ASTRUM_SAVE_AFTER_MS", "300")]);
    c.wait_for("ready$");
    c.send(b"echo keep-me-$((9*9))\r");
    c.wait_for("keep-me-81");
    let dir = saved_session(&sb, "main");
    let kept = |dir: &PathBuf| std::fs::read_dir(dir).is_ok_and(|d| d.flatten().filter(|e| e.path().extension().is_some_and(|x| x == "txt")).any(|e| std::fs::read_to_string(e.path()).is_ok_and(|t| t.contains("keep-me-81"))));
    c.wait_until("what the pane shows to be written out", |_| kept(&dir));
    // Only the tabs are to come back: what the pane showed is taken off the disk.
    sb.write_config("restore = \"layout\"\n");
    c.wait_until("the pane's text gone from the disk", |_| !kept(&dir));
    // And everything again. The pane has printed nothing in between.
    sb.write_config("restore = \"all\"\n");
    c.wait_until("the pane's text written out again", |_| kept(&dir));
}

#[test]
fn a_choice_made_in_one_session_is_not_undone_by_another() {
    let sb = Sandbox::new();
    let a = sb.attach("one", 100, 24);
    let b = sb.attach("two", 100, 24);
    a.wait_for("ready$");
    b.wait_for("ready$");
    // Both sessions read the same file of choices when they started. One
    // picks a theme; then the other, which knows nothing of that, changes
    // something else.
    a.send(CTRL_SPACE);
    a.wait_for("AGENT");
    a.send(b"/theme nord\r");
    let state = sb.root.join("cfg/state.toml");
    a.wait_until("the theme written down", |_| std::fs::read_to_string(&state).is_ok_and(|t| t.contains("nord")));
    b.send(CTRL_SPACE);
    b.wait_for("AGENT");
    b.send(b"/autonomy safe\r");
    b.wait_until("the other choice written down", |_| std::fs::read_to_string(&state).is_ok_and(|t| t.contains("safe")));
    let text = std::fs::read_to_string(&state).unwrap();
    assert!(text.contains("nord") && text.contains("safe") && text.contains("welcomed"), "{text}");
}

#[test]
fn the_walkthrough_put_away_with_a_click_has_been_seen_and_short_windows_reach_every_setting() {
    let sb = Sandbox::new();
    std::fs::remove_file(sb.root.join("cfg/state.toml")).unwrap();
    let c = sb.attach("main", 100, 26);
    c.wait_for("Welcome to Astrum");
    // A click outside the card puts it away, as it does any other.
    c.click(1, 24);
    c.wait_until("the walkthrough gone", |c| !c.text().contains("Welcome to Astrum"));
    let state = sb.root.join("cfg/state.toml");
    c.wait_until("that it was seen, written down", |_| std::fs::read_to_string(&state).is_ok_and(|t| t.contains("welcomed = true")));

    // In a window too short for all the settings, the wheel reaches the last of them.
    let c = sb.attach("short", 90, 18);
    c.wait_for("ready$");
    c.send(CTRL_B);
    c.wait_for("PREFIX");
    c.send(b".");
    c.wait_for("Settings");
    assert!(!c.text().contains("Forget the choices made here"), "{}", c.text());
    for _ in 0..30 {
        c.send(b"\x1b[<65;45;9M");
    }
    c.wait_for("Forget the choices made here");
}

#[test]
fn a_change_of_theme_reaches_rows_nothing_has_written_to_since() {
    let sb = Sandbox::new();
    let c = sb.attach_env("main", 90, 24, &[("COLORTERM", "truecolor")]);
    c.wait_for("ready$");
    c.send(b"echo quiet-$((1+1))\r");
    c.wait_for("quiet-2");
    // The background of an empty cell in the middle of the pane, as the terminal has it.
    let background = |c: &Client| color(c.screen.lock().unwrap().term.grid()[Line(12)][Column(40)].bg);
    c.send(CTRL_SPACE);
    c.wait_for("AGENT");
    c.send(b"/theme nord\r");
    c.wait_until("the first theme's background", |c| background(c) != u32::MAX);
    let nord = background(&c);
    // Drawn a few times as it is, then another theme: the pane's rows have
    // not been written to, and must change all the same.
    c.send(b"/theme dracula\r");
    c.wait_until("the second theme's background on a row nothing wrote to", |c| background(c) != nord && background(c) != u32::MAX);
}

#[test]
fn a_picture_as_wide_as_the_pane_is_not_written_into_by_what_follows() {
    let sb = sandbox_with_a_picture();
    let c = sb.attach_env("main", 60, 30, &[("ASTRUM_IMAGES", "iterm")]);
    c.wait_for("ready$");
    // Forty pixels square asked to fill the width: sixty cells by... as
    // many rows as fit; whatever it comes to, it reaches the last column.
    c.send(format!("clear; ARGS=';width=60;height=4'; printf '\\033]1337;File=inline=1%s:%s\\a' \"$ARGS\" \"$(cat pic.b64)\"; printf 'X-after\\n'\r").as_bytes());
    c.wait_for("X-after");
    let head = "\x1b]1337;File=inline=1;width=60;height=4;";
    c.wait_until("the picture drawn whole", |c| c.raw().contains(head));
    // The text went to the line below it, not onto its last cell.
    assert!(c.lines()[5].starts_with("X-after") && !c.text().contains('░'), "{}", c.text());
}

// ---- a shell inside the pane's own ---------------------------------------------

/// A plain shell started inside the pane's own, with a prompt of its own:
/// what the other end of `ssh` looks like to Astrum, with no server needed.
const INNER_BASH: &str = "PS1='inner$ ' bash --norc --noprofile";
// (These tests use a wide terminal: the status bar names the machine the
// shell is on, and on a test runner that name can be fifty characters.)

#[test]
fn the_agent_is_taken_into_a_shell_the_pane_has_reached() {
    let (sb, bodies) = sandbox_with_agent_settings("autonomy = \"auto\"");
    let c = sb.attach("main", 200, 30);
    c.wait_for("ready$");
    // At the pane's own prompt there is nowhere to take it.
    c.send(CTRL_B);
    c.wait_for("PREFIX");
    c.send(b"i");
    c.wait_for("already works in the shell at this prompt");

    c.send(format!("{INNER_BASH}\r").as_bytes());
    c.wait_until("the shell inside", |c| c.lines().iter().any(|l| l.starts_with("inner$")));
    // The status bar offers to take the agent in; a click does it.
    c.wait_for("take the agent in");
    let (col, row) = c.position_of("take the agent in").unwrap();
    c.click(col + 3, row);
    c.wait_for("The agent now works in this shell");
    // What was typed to do it is gone from the screen, and the status bar
    // says whose shell this is.
    c.wait_until("the typing tidied away", |c| !c.text().contains("printf") && !c.text().contains("stty"));
    assert!(c.lines()[29].contains("⇢ "), "{}", c.text());
    // The shell's prompt is back, once, where the typing began; and there
    // is nothing more to offer.
    c.wait_until("one prompt of the shell inside", |c| c.lines().iter().filter(|l| l.starts_with("inner$")).count() == 1);
    assert!(!c.lines()[29].contains("take the agent in"), "{}", c.text());

    // The agent's command runs there, and it is told how it went.
    c.send(CTRL_SPACE);
    c.wait_for("AGENT");
    c.send(b"TOOLCALL run_command {\"command\":\"echo from-inside-$((2+3)); cd /tmp; false\"}\r");
    c.wait_for("Verdict: the tool answered.");
    {
        let sent = bodies.lock().unwrap();
        let result = tool_result_in(&sent[1]);
        assert!(result.contains("from-inside-5") && result.contains("exit code: 1"), "{result}");
        // It was told where it is, and was not offered the tools that
        // work on this machine's files.
        assert!(sent[0].contains("You are working on another machine") && sent[0].contains("machine: "), "{}", sent[0]);
        assert!(sent[0].contains("\"name\":\"run_command\"") && !sent[0].contains("\"name\":\"read_file\"") && !sent[0].contains("\"name\":\"edit_file\""), "{}", sent[0]);
    }
    assert!(c.lines().iter().any(|l| l.starts_with("inner$ echo from-inside-")), "it ran in the shell inside: {}", c.text());
    // The directory in the status bar is that shell's.
    c.wait_until("the inner shell's directory", |c| c.lines()[29].contains("/tmp"));
    // A model that asks for a file tool all the same is told why not.
    c.send(b"/clear\r");
    c.wait_until("an empty panel", |c| !c.text().contains("Verdict"));
    c.send(b"TOOLCALL read_file {\"path\":\"/etc/hosts\"}\r");
    c.wait_for("Verdict: the tool answered.");
    assert!(tool_result_in(&bodies.lock().unwrap()[3]).contains("works on the user's own machine"));

    // Leaving that shell, the agent is back in the pane's own.
    c.send(CTRL_SPACE);
    c.send(b"exit\r");
    // (The panel is beside the shell at this width: the shell's part of a row is what is left of the line between them.)
    c.wait_until("the pane's own prompt", |c| c.lines().iter().rev().find(|l| l.contains("ready$")).is_some_and(|l| l.split('│').next().unwrap_or_default().trim_end().ends_with("ready$")));
    c.wait_until("the status bar back to this machine", |c| !c.lines()[29].contains("⇢ "));
    c.send(CTRL_SPACE);
    c.send(b"/clear\r");
    c.send(b"go\r");
    c.wait_until("the next request", |_| bodies.lock().unwrap().len() >= 5);
    let sent = bodies.lock().unwrap();
    assert!(!sent[4].contains("You are working on another machine") && sent[4].contains("\"name\":\"read_file\""), "{}", sent[4]);
}

#[test]
fn shells_within_shells_and_what_cannot_be_entered() {
    let sb = Sandbox::new();
    let c = sb.attach_env("main", 200, 30, &[("ASTRUM_CARRY_WAIT_MS", "1500")]);
    c.wait_for("ready$");
    let take_in = |c: &Client| {
        c.send(CTRL_B);
        c.wait_for("PREFIX");
        c.send(b"i");
    };
    let named = |c: &Client, prompt: &str| c.lines().iter().rev().find(|l| !l.trim().is_empty() && !l.contains("SHELL")).is_some_and(|l| l.starts_with(prompt));

    // One shell in, then another inside that.
    c.send(format!("{INNER_BASH}\r").as_bytes());
    c.wait_until("the first shell inside", |c| named(c, "inner$"));
    take_in(&c);
    c.wait_for("The agent now works in this shell");
    c.send(b"PS1='deeper$ ' bash --norc --noprofile\r");
    c.wait_until("the second", |c| named(c, "deeper$"));
    // (Offered again: the first one's marks say a shell was started in it.)
    c.wait_for("take the agent in");
    c.wait_until("the first message gone", |c| !c.text().contains("The agent now works"));
    take_in(&c);
    c.wait_for("The agent now works in this shell");
    c.send(b"cd /usr; echo deep-$((4+4))\r");
    c.wait_for("deep-8");
    c.wait_until("the innermost shell's directory", |c| c.lines()[29].contains("⇢ ") && c.lines()[29].contains("/usr"));
    // Out of the second: the first is followed again, with its own directory.
    c.send(b"exit\r");
    c.wait_until("back in the first", |c| named(c, "inner$"));
    c.wait_until("its directory", |c| c.lines()[29].contains("⇢ ") && !c.lines()[29].contains("/usr"));
    c.send(b"echo first-$((5+5))\r");
    c.wait_for("first-10");
    c.send(b"exit\r");
    c.wait_until("the pane's own shell", |c| named(c, "ready$") && !c.lines()[29].contains("⇢ "));

    // A shell that is none of the three says so, and is not entered.
    c.send(b"clear; dash\r");
    std::thread::sleep(Duration::from_millis(500));
    take_in(&c);
    c.wait_for("not bash, zsh or fish");
    c.send(b"exit\r");
    c.wait_until("the pane's own shell again", |c| named(c, "ready$"));
    // Something that is not a shell at all never answers, and that is said too.
    c.wait_until("the message gone", |c| !c.text().contains("not bash, zsh"));
    c.send(b"clear; cat\r");
    std::thread::sleep(Duration::from_millis(400));
    take_in(&c);
    c.wait_for("Nothing there answered");
    c.send(b"\x03");
    c.wait_until("the prompt after cat", |c| named(c, "ready$"));
}

#[test]
fn the_agent_is_taken_into_zsh_and_fish_where_they_are_installed() {
    // (bash is everywhere and has the tests above. These two are tried on
    // whatever machine has them.)
    let installed = |shell: &str| ["/bin", "/usr/bin", "/usr/local/bin", "/opt/homebrew/bin"].iter().any(|dir| std::path::Path::new(dir).join(shell).exists());
    for (shell, start, arithmetic, status) in [("zsh", "zsh -f", "$((6*7))", "$?"), ("fish", "fish --no-config", "(math 6 x 7)", "$status")] {
        if !installed(shell) {
            eprintln!("{shell} is not installed here: not tried");
            continue;
        }
        let sb = Sandbox::new();
        let c = sb.attach("main", 200, 30);
        c.wait_for("ready$");
        c.send(format!("{start}\r").as_bytes());
        c.wait_for("take the agent in");
        std::thread::sleep(Duration::from_millis(700));
        c.send(CTRL_B);
        c.wait_for("PREFIX");
        c.send(b"i");
        c.wait_for("The agent now works in this shell");
        c.wait_until("the typing tidied away", |c| !c.text().contains("printf") && !c.text().contains("stty"));
        // Its directory is followed, and a command's result is the shell's own.
        c.send(format!("cd /usr; echo in-{shell}-{arithmetic}; false\r").as_bytes());
        c.wait_for(&format!("in-{shell}-42"));
        c.wait_until("that shell's directory", |c| c.lines()[29].contains("⇢ ") && c.lines()[29].contains("/usr"));
        c.wait_until("the failure offered for explaining", |c| c.lines()[29].contains("exit 1"));
        c.send(format!("echo was-{status}\r").as_bytes());
        c.wait_for("was-1");
        c.send(b"exit\r");
        c.wait_until("the pane's own shell", |c| !c.lines()[29].contains("⇢ ") && c.lines().iter().rev().find(|l| !l.trim().is_empty() && !l.contains("SHELL")).is_some_and(|l| l.starts_with("ready$")));
    }
}

#[test]
fn a_command_for_a_shell_the_agent_was_not_taken_into_says_where_it_ran() {
    let (sb, bodies) = sandbox_with_agent_settings("autonomy = \"auto\"");
    let c = sb.attach("main", 200, 30);
    c.wait_for("ready$");
    c.send(format!("{INNER_BASH}\r").as_bytes());
    c.wait_until("the shell inside", |c| c.lines().iter().any(|l| l.starts_with("inner$")));
    c.send(CTRL_SPACE);
    c.wait_for("AGENT");
    c.send(b"TOOLCALL run_command {\"command\":\"echo where-$((1+1))\"}\r");
    c.wait_for("Verdict: the tool answered.");
    let result = tool_result_in(&bodies.lock().unwrap()[1]);
    assert!(result.contains("where-2") && result.contains("did NOT run there") && result.contains("take the agent in"), "{result}");
    // And nothing was typed into the shell inside.
    assert!(!c.text().contains("inner$ echo where"), "{}", c.text());
}

// ---- measuring ------------------------------------------------------------------
//
// Not tests of behavior: how much work drawing takes, and how long a key
// takes to show. Left out of an ordinary run; asked for with
//   cargo test --release --test e2e -- --ignored --nocapture --test-threads=1 measure_

/// What the session says it has spent so far (see `proto::Work`).
fn work(sb: &Sandbox, session: &str) -> serde_json::Value {
    let said = sb.run(&["-s", session, "debug-work"]);
    serde_json::from_str(said.trim()).unwrap_or_else(|_| panic!("debug-work said: {said}"))
}

/// One line of the report: what was done between two readings of `work`.
/// Returns what a frame cost, in microseconds.
fn report(name: &str, wall: Duration, before: &serde_json::Value, after: &serde_json::Value, extra: &str) -> f64 {
    let d = |key: &str| after[key].as_u64().unwrap_or(0).saturating_sub(before[key].as_u64().unwrap_or(0));
    let frames = d("frames").max(1);
    let read_mb = d("read") as f64 / 1e6;
    println!(
        "MEASURE {name:<22} wall {:>7.1} ms | {:>5} frames | draw {:>6.0} us/frame | send {:>6.0} us/frame | out {:>7.1} KB/frame | in {:>6.2} MB at {:>6.0} us/MB {extra}",
        wall.as_secs_f64() * 1000.0,
        d("frames"),
        d("draw_us") as f64 / frames as f64,
        d("send_us") as f64 / frames as f64,
        d("sent") as f64 / frames as f64 / 1000.0,
        read_mb,
        if read_mb > 0.0 { d("read_us") as f64 / read_mb } else { 0.0 },
    );
    (d("draw_us") + d("send_us")) as f64 / frames as f64
}

/// Fill the screen with text in many colors, as a listing or an editor would.
const FILL: &str = r#"awk 'BEGIN{for(r=0;r<200;r++){l="";for(c=0;c<36;c++)l=l sprintf("\033[38;5;%dm%s ",(r*7+c*3)%200+16,substr("lorem ipsum dolor sit amet",1+(r+c)%20,5));print l "\033[0m"}}'"#;

/// Type characters one at a time, each after the last has shown, and say
/// how long they took to show: the middle one, the slow ones, the slowest.
/// The characters are ones found neither in what the panes are showing nor
/// in the sequences a frame is made of, so that seeing one arrive is
/// seeing that key's echo and nothing else.
fn typing(c: &Client, keys: usize) -> (String, f64) {
    const KEYS: &[u8] = b"%+=@&*";
    let mut took = Vec::with_capacity(keys);
    for n in 0..keys {
        if n % 60 == 59 {
            c.send(b"\x15");
            std::thread::sleep(Duration::from_millis(30));
        }
        let key = KEYS[n % KEYS.len()];
        let seen = c.screen.lock().unwrap().raw.len();
        let began = Instant::now();
        c.send(&[key]);
        while !c.screen.lock().unwrap().raw[seen..].contains(&key) {
            assert!(began.elapsed() < Duration::from_secs(5), "a key never showed");
            std::thread::sleep(Duration::from_micros(50));
        }
        took.push(began.elapsed());
        // Let the frame finish arriving before the next key.
        std::thread::sleep(Duration::from_millis(4));
    }
    took.sort();
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    (format!("| key to screen: median {:.2} ms, 95th {:.2} ms, slowest {:.2} ms", ms(took[keys / 2]), ms(took[keys * 95 / 100]), ms(took[keys - 1])), ms(took[keys / 2]))
}

#[test]
#[ignore = "a measurement, not a test"]
fn measure_drawing_and_latency() {
    let sb = Sandbox::new();
    let c = sb.attach("main", 220, 60);
    c.wait_for("ready$");

    // Output pouring past: two megabytes of numbers.
    let (before, began) = (work(&sb, "main"), Instant::now());
    c.send(b"seq 1 300000; echo flood-$((1+1))\r");
    c.wait_for("flood-2");
    report("output pouring past", began.elapsed(), &before, &work(&sb, "main"), "");

    // A program repainting the whole screen in colors, 200 times.
    let (before, began) = (work(&sb, "main"), Instant::now());
    c.send(br#"clear; awk 'BEGIN{for(f=0;f<200;f++){printf "\033[H";for(r=0;r<56;r++){l="";for(c=0;c<108;c++)l=l sprintf("\033[38;5;%dm%c ",(f+r+c)%200+16,65+(f+c)%26);print l}}}'; printf '\033[0m'; clear; echo repaint-$((1+1))"#);
    c.send(b"\r");
    c.wait_for("repaint-2");
    report("whole screen repainted", began.elapsed(), &before, &work(&sb, "main"), "");

    // Typing at a prompt under a screen full of colored text: one
    // character changes, and the rest of the screen does not.
    c.send(format!("clear; {FILL}\r").as_bytes());
    c.wait_until("the screen filled", |c| c.lines()[30].contains("lorem") || c.lines()[30].contains("ipsum") || c.lines()[30].contains("dolor"));
    c.wait_until("the prompt", |c| c.lines()[58].starts_with("ready$"));
    std::thread::sleep(Duration::from_millis(300));
    let (before, began) = (work(&sb, "main"), Instant::now());
    let (keys, idle) = typing(&c, 300);
    let frame = report("typing, one pane", began.elapsed(), &before, &work(&sb, "main"), &keys);
    c.send(b"\x15");

    // The same with the window in four panes, each full.
    for split in [b"|", b"-", b"|"] {
        c.send(CTRL_B);
        c.send(split);
        std::thread::sleep(Duration::from_millis(500));
        c.send(format!("clear; {FILL}\r").as_bytes());
        std::thread::sleep(Duration::from_millis(900));
    }
    let (before, began) = (work(&sb, "main"), Instant::now());
    let (keys, _) = typing(&c, 300);
    report("typing, four panes", began.elapsed(), &before, &work(&sb, "main"), &keys);
    c.send(b"\x15");

    // And while another pane pours out text: this one is set going, and
    // the typing is done in the first pane, at the left.
    c.send(b"yes 'the quick brown fox jumps over the lazy dog'\r");
    std::thread::sleep(Duration::from_millis(500));
    c.click(5, 5);
    std::thread::sleep(Duration::from_millis(1000));
    let (before, began) = (work(&sb, "main"), Instant::now());
    let (keys, busy) = typing(&c, 300);
    report("typing beside a flood", began.elapsed(), &before, &work(&sb, "main"), &keys);

    // With ASTRUM_MEASURE_LIMITS set (as in CI), the run fails if any of
    // these is far out: several times what they are on a machine with
    // other things to do, which is what a mistake that brings back a
    // frame's wait, or a queue's, looks like. Not a fine comparison.
    if std::env::var_os("ASTRUM_MEASURE_LIMITS").is_some() {
        for (what, value, limit) in [("a key at an idle prompt, ms", idle, 40.0), ("a key beside a flood, ms", busy, 80.0), ("a frame while typing, us", frame, 6000.0)] {
            assert!(value <= limit, "{what}: {value:.2}, over the limit of {limit}");
        }
        println!("MEASURE within the limits");
    }
}

// ---- conformance ------------------------------------------------------------------
//
// What a program writes goes through a great deal before it is on the
// user's screen: the emulator, the frame Astrum draws from it, the changes
// worked out against the last frame, and the user's own terminal. These
// tests feed the same bytes to a pane and straight to an emulator of the
// same kind, and compare, cell by cell, what the "terminal" Astrum is
// drawing on ends up showing with what the program meant.

use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::TermMode;
use alacritty_terminal::term::cell::Cell;
use alacritty_terminal::vte::ansi::{Color as TermColor, NamedColor};

/// A color as far as a comparison goes: the default, one of the 256, or an exact one.
fn color(c: TermColor) -> u32 {
    match c {
        TermColor::Spec(rgb) => 0x0100_0000 | (rgb.r as u32) << 16 | (rgb.g as u32) << 8 | rgb.b as u32,
        TermColor::Indexed(i) => i as u32,
        TermColor::Named(n) => match n {
            NamedColor::Black | NamedColor::DimBlack => 0,
            NamedColor::Red | NamedColor::DimRed => 1,
            NamedColor::Green | NamedColor::DimGreen => 2,
            NamedColor::Yellow | NamedColor::DimYellow => 3,
            NamedColor::Blue | NamedColor::DimBlue => 4,
            NamedColor::Magenta | NamedColor::DimMagenta => 5,
            NamedColor::Cyan | NamedColor::DimCyan => 6,
            NamedColor::White | NamedColor::DimWhite => 7,
            NamedColor::BrightBlack => 8,
            NamedColor::BrightRed => 9,
            NamedColor::BrightGreen => 10,
            NamedColor::BrightYellow => 11,
            NamedColor::BrightBlue => 12,
            NamedColor::BrightMagenta => 13,
            NamedColor::BrightCyan => 14,
            NamedColor::BrightWhite => 15,
            _ => u32::MAX,
        },
    }
}

/// What can be seen of one cell: its character (with any marks combined
/// onto it), its colors, and how it is styled.
fn look(cell: &Cell) -> (String, u32, u32, String) {
    let mut text = String::new();
    text.push(if matches!(cell.c, '\0' | '\t') { ' ' } else { cell.c });
    text.extend(cell.zerowidth().into_iter().flatten());
    let flags = cell.flags;
    let mut style = String::new();
    for (flag, letter) in [
        (Flags::BOLD, 'b'),
        (Flags::DIM, 'd'),
        (Flags::ITALIC, 'i'),
        (Flags::ALL_UNDERLINES, 'u'),
        (Flags::INVERSE, 'r'),
        (Flags::HIDDEN, 'h'),
        (Flags::STRIKEOUT, 's'),
        (Flags::WIDE_CHAR, 'W'),
        (Flags::WIDE_CHAR_SPACER, 'w'),
    ] {
        if flags.intersects(flag) {
            style.push(letter);
        }
    }
    // Of an empty cell, only some of this can be seen: not whether it is
    // bold, slanted, faint or hidden, and not its text color unless a
    // line is drawn through or under it, or its colors are swapped.
    if text == " " {
        style.retain(|letter| matches!(letter, 'u' | 'r' | 's' | 'W' | 'w'));
    }
    let fg = if text == " " && !flags.intersects(Flags::ALL_UNDERLINES | Flags::STRIKEOUT | Flags::INVERSE) { u32::MAX } else { color(cell.fg) };
    (text, fg, color(cell.bg), style)
}

/// The cells of `rows` rows from row `top` of an emulator's screen, and
/// where its cursor is in them if it is showing.
type Seen = (Vec<Vec<(String, u32, u32, String)>>, Option<(usize, usize)>);

fn seen(term: &Term<VoidListener>, top: usize, rows: usize, cols: usize) -> Seen {
    let grid = term.grid();
    let mut cells: Vec<Vec<(String, u32, u32, String)>> = (0..rows).map(|row| (0..cols).map(|col| look(&grid[Line((top + row) as i32)][Column(col)])).collect()).collect();
    // A double-width character and the spacer after it can be parted in
    // the emulator's grid (by an insert or a delete, or at the last
    // column). There is no drawing half of one: each half without the
    // other is shown as a blank that keeps its colors.
    for row in &mut cells {
        for col in 0..cols {
            let wide = row[col].3.contains('W');
            let spacer = row[col].3.contains('w');
            let lone = (wide && !row.get(col + 1).is_some_and(|next| next.3.contains('w'))) || (spacer && !(col > 0 && row[col - 1].3.contains('W')));
            if lone {
                let cell = &mut row[col];
                cell.0 = " ".into();
                cell.3.retain(|letter| matches!(letter, 'u' | 'r' | 's'));
                if cell.3.is_empty() {
                    cell.1 = u32::MAX;
                }
            }
        }
    }
    // Where the emulator would draw its cursor: on the first half of a
    // double-width character, if it is on the second.
    let at = term.renderable_content().cursor.point;
    let shown = term.mode().contains(TermMode::SHOW_CURSOR) && at.line.0 >= top as i32 && (at.line.0 as usize) < top + rows;
    (cells, shown.then(|| (at.line.0 as usize - top, at.column.0)))
}

/// A pane being fed bytes, and an emulator fed the same.
struct Player {
    c: Client,
    /// The writing end of the pipe the pane reads. Kept open throughout:
    /// what is in a pipe that nobody has open is thrown away.
    feed: std::fs::File,
    reference: Term<VoidListener>,
    parser: Processor,
    cols: usize,
    rows: usize,
}

impl Player {
    /// A terminal of `cols` by `rows` with one pane, in which a loop copies
    /// whatever is written to a named pipe is copied straight to the screen.
    fn new(sb: &Sandbox, cols: u16, rows: u16) -> Player {
        let feed = sb.root.join("home/feed");
        let path = std::ffi::CString::new(feed.to_string_lossy().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        let c = sb.attach_env("main", cols, rows, &[("COLORTERM", "truecolor")]);
        c.wait_for("ready$");
        // Nothing typed is echoed, and nothing written is touched on its
        // way (no carriage return added to a line feed). Then a full reset,
        // so that the pane starts where a new emulator does.
        c.send(b"stty -echo -opost; printf '\\033c'; cat feed\r");
        let feed = std::fs::OpenOptions::new().write(true).open(&feed).unwrap();
        let (cols, rows) = (cols as usize, rows as usize - 2);
        let mut player = Player { c, feed, reference: Term::new(TermConfig::default(), &Dims(cols, rows), VoidListener), parser: Processor::new(), cols, rows };
        player.parser.advance(&mut player.reference, b"\x1bc");
        // (One thing a reset leaves as it was: which character was written
        // last, for "repeat the last character". A space, on both sides.)
        player.play(b" \r");
        player.check("a pane that has just been reset");
        player
    }

    /// Write bytes to the pane as a program would, and to the reference.
    fn play(&mut self, bytes: &[u8]) {
        self.feed.write_all(bytes).unwrap();
        self.parser.advance(&mut self.reference, bytes);
    }

    /// Wait for the terminal to show what the reference holds. The pane is
    /// below the tab bar, which is the terminal's first row.
    #[track_caller]
    fn check(&self, what: &str) {
        let want = seen(&self.reference, 0, self.rows, self.cols);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let got = {
                let screen = self.c.screen.lock().unwrap();
                let (cells, cursor) = seen(&screen.term, 1, self.rows, self.cols);
                // (The cursor is drawn by the terminal itself, where Astrum puts it.)
                let at = screen.term.grid().cursor.point;
                let shown = screen.term.mode().contains(TermMode::SHOW_CURSOR);
                let _ = cursor;
                (cells, shown.then(|| (at.line.0 as usize).checked_sub(1).map(|row| (row, at.column.0))).flatten())
            };
            if got == want {
                return;
            }
            if Instant::now() >= deadline {
                for (row, (a, b)) in got.0.iter().zip(&want.0).enumerate() {
                    if let Some(col) = (0..self.cols).find(|col| a[*col] != b[*col]) {
                        let text = |cells: &Vec<(String, u32, u32, String)>| cells.iter().map(|c| c.0.as_str()).collect::<String>();
                        let all = |rows: &Vec<Vec<(String, u32, u32, String)>>| rows.iter().map(|r| format!("  |{}|", text(r))).collect::<Vec<_>>().join("\n");
                        panic!("{what}: row {row}, column {col}: the terminal shows {:?} where the program meant {:?}\nshown:\n{}\nmeant:\n{}", a[col], b[col], all(&got.0), all(&want.0));
                    }
                }
                panic!("{what}: the cursor is at {:?} where the program left it at {:?}", got.1, want.1);
            }
            std::thread::sleep(Duration::from_millis(15));
        }
    }

    /// Play, check, and also hold the emulator itself to what the screen
    /// should say: `lines` are its first rows, without the blanks at their ends.
    #[track_caller]
    fn expect(&mut self, what: &str, bytes: &[u8], lines: &[&str]) {
        self.play(bytes);
        self.check(what);
        let grid = self.reference.grid();
        for (row, line) in lines.iter().enumerate() {
            let text: String = (0..self.cols).map(|col| &grid[Line(row as i32)][Column(col)]).filter(|c| !c.flags.contains(Flags::WIDE_CHAR_SPACER)).map(|c| if matches!(c.c, '\0' | '\t') { ' ' } else { c.c }).collect();
            assert_eq!(text.trim_end(), *line, "{what}: row {row} of the screen");
        }
    }
}

#[test]
fn what_a_program_writes_is_what_the_terminal_shows() {
    let sb = Sandbox::new();
    let mut p = Player::new(&sb, 60, 14);
    let home = b"\x1b[2J\x1b[H";

    // Text, and where lines end and begin.
    p.expect("plain text", b"hello\r\nworld", &["hello", "world"]);
    p.expect("a line feed alone keeps the column", b"\x1b[2J\x1b[Hab\ncd\rX", &["ab", "X cd"]);
    p.expect("backspace and overwrite", b"\x1b[2J\x1b[Habc\x08\x08XY\x08Z", &["aXZ"]);
    p.expect("a line longer than the screen wraps", &[home.as_slice(), "0123456789".repeat(7).as_bytes()].concat(), &["0123456789".repeat(6).as_str(), "0123456789"]);
    p.expect("with wrapping off it stays on the line", &[home.as_slice(), b"\x1b[?7l", "0123456789".repeat(7).as_bytes(), b"\x1b[?7h"].concat(), &["0123456789".repeat(6).as_str()]);
    p.expect("tabs, and tab stops of the program's own", b"\x1b[2J\x1b[Ha\tb\tc\x1b[2;1H\x1b[3g\x1b[5G\x1bH\x1b[1Gx\ty\x1b[2;20Hz", &["a       b       c", "x   y              z"]);
    p.play(b"\x1bc");

    // Moving about.
    p.expect("to a row and column", b"\x1b[2J\x1b[3;5Hx\x1b[1;1Hy\x1b[2;60Hz", &["y", "                                                           z", "    x"]);
    p.expect("by steps", b"\x1b[2J\x1b[5;10H\x1b[2Aa\x1b[3Bb\x1b[5Cc\x1b[9Dd\x1b[2Ee\x1b[3Ff\x1b[20Gg\x1b[2dh", &["", "                    h", "         a", "", "f                  g", "        d b     c", "", "e"]);
    p.expect("saved and brought back", b"\x1b[2J\x1b[4;4H\x1b7\x1b[1;1Hone\x1b8two\x1b[s\x1b[9;9H\x1b[uthree", &["one", "", "", "   twothree"]);
    p.expect("past the edges it stops", b"\x1b[2J\x1b[99;99Ha\x1b[99A\x1b[99Db\x1b[99Cc", &["b                                                          c", "", "", "", "", "", "", "", "", "", "", "                                                           a"]);

    // Erasing, inserting and deleting.
    let grid = "\x1b[2J\x1b[H".to_string() + &(0..6).map(|n| format!("line{n}-abcdefghij\r\n")).collect::<String>();
    p.expect("to the end of the line and of the screen", format!("{grid}\x1b[2;6H\x1b[K\x1b[4;6H\x1b[J").as_bytes(), &["line0-abcdefghij", "line1", "line2-abcdefghij", "line3"]);
    p.expect("from the start of the line and of the screen", format!("{grid}\x1b[5;6H\x1b[1K\x1b[3;6H\x1b[1J").as_bytes(), &["", "", "      abcdefghij", "line3-abcdefghij", "      abcdefghij", "line5-abcdefghij"]);
    p.expect("a whole line, and characters in it", format!("{grid}\x1b[1;1H\x1b[2K\x1b[2;3H\x1b[4X\x1b[3;3H\x1b[4P\x1b[4;3H\x1b[4@").as_bytes(), &["", "li    abcdefghij", "liabcdefghij", "li    ne3-abcdefghij"]);
    p.expect("lines put in and taken out", format!("{grid}\x1b[2;1H\x1b[2L\x1b[5;1H\x1b[1M").as_bytes(), &["line0-abcdefghij", "", "", "line1-abcdefghij", "line3-abcdefghij", "line4-abcdefghij", "line5-abcdefghij"]);
    p.expect("what is erased takes the background in force", b"\x1b[2J\x1b[H\x1b[44mblue\x1b[K\x1b[2;1H\x1b[41m\x1b[2K\x1b[0m\x1b[3;1Hplain", &["blue", "", "plain"]);
    p.expect("insert mode pushes what follows along", b"\x1b[2J\x1b[Habcdef\x1b[1;3H\x1b[4hXY\x1b[4lZ", &["abXYZdef"]);
    p.expect("the last character repeated", b"\x1b[2J\x1b[Hab\x1b[5bc", &["abbbbbbc"]);

    // Scrolling.
    let numbered = (1..=20).map(|n| format!("row {n}\r\n")).collect::<String>();
    p.expect("off the bottom", format!("\x1b[2J\x1b[H{numbered}end").as_bytes(), &["row 10", "row 11", "row 12", "row 13", "row 14", "row 15", "row 16", "row 17", "row 18", "row 19", "row 20", "end"]);
    p.expect("up and down by command", b"\x1b[2S\x1b[1T", &["", "row 12", "row 13"]);
    p.expect("back up off the top", b"\x1b[1;1H\x1bMtop", &["top", "", "row 12"]);
    p.expect("inside a region only", format!("{grid}\x1b[2;4r\x1b[4;1H\n\nin\x1b[r\x1b[10;1Hout").as_bytes(), &["line0-abcdefghij", "line3-abcdefghij", "", "in", "line4-abcdefghij", "line5-abcdefghij", "", "", "", "out"]);
    p.expect("rows counted from the region's top", format!("{grid}\x1b[3;5r\x1b[?6h\x1b[1;1HA\x1b[9;1HB\x1b[?6l\x1b[r\x1b[1;1HC").as_bytes(), &["Cine0-abcdefghij", "line1-abcdefghij", "Aine2-abcdefghij", "line3-abcdefghij", "Bine4-abcdefghij"]);

    // How text looks.
    p.play(b"\x1b[2J\x1b[H\x1b[1mbold\x1b[0m \x1b[2mdim\x1b[0m \x1b[3mitalic\x1b[0m \x1b[4munder\x1b[0m \x1b[7mreverse\x1b[0m \x1b[8mhidden\x1b[0m \x1b[9mstruck\x1b[0m\r\n");
    p.play(b"\x1b[1;3;4;9mall at once\x1b[22mno longer bold\x1b[23;24mnor slanted or underlined\x1b[29m\x1b[0m\r\n");
    p.check("each style, and styles taken away one at a time");
    for base in [30, 40, 90, 100] {
        let line: String = (0..8).map(|n| format!("\x1b[{}m {n} ", base + n)).collect();
        p.play(format!("{line}\x1b[0m\r\n").as_bytes());
    }
    p.play(b"\x1b[38;5;208;48;5;17m 256 \x1b[38;2;255;0;104;48;2;4;1;2m exact \x1b[39mdefault text\x1b[49m default both\x1b[0m\r\n");
    p.play(b"\x1b[4;31mred under\x1b[0;7;32mgreen reversed\x1b[27mnot\x1b[0m \x1b[41m   \x1b[0m|\r\n");
    p.check("the sixteen colors, the 256, exact ones, and going back to the default");
    p.play(b"\x1b[2J\x1b[H\x1b(0lqqk\r\nx  x\r\nmqqj\x1b(B box");
    p.expect("line-drawing characters", b"", &["┌──┐", "│  │", "└──┘ box"]);

    // Characters that are not one cell wide.
    p.expect("wide characters", "\x1b[2J\x1b[H日本語 text ✓ 🙂|\r\n한글|".as_bytes(), &["日本語 text ✓ 🙂|", "한글|"]);
    p.expect("a wide character that does not fit goes to the next line", format!("\x1b[2J\x1b[H{}語|", "x".repeat(59)).as_bytes(), &["x".repeat(59).as_str(), "語|"]);
    p.expect("half of a wide character written over", "\x1b[2J\x1b[H日本語\x1b[1;2Ha\x1b[1;5Hb".as_bytes(), &[" a本b"]);
    p.expect("marks combined onto a letter", "\x1b[2J\x1b[He\u{301}a\u{308}n\u{303}|".as_bytes(), &["e\u{301}a\u{308}n\u{303}|".chars().filter(|c| !('\u{300}'..'\u{370}').contains(c)).collect::<String>().as_str()]);

    // The other screen, the cursor, and starting over.
    p.expect("a full-screen program's own screen", b"\x1b[2J\x1b[Hbefore\x1b[?1049h\x1b[2J\x1b[Hinside", &["inside"]);
    p.expect("and the first one back as it was", b"\x1b[?1049l", &["before"]);
    p.play(b"\x1b[?25l");
    p.check("the cursor hidden");
    p.play(b" x\x1b[?25h");
    p.check("and showing again, where it now is");
    p.expect("a full reset", b"\x1b[44;1m\x1b[3;7r\x1b[?6h\x1b[4hjunk\x1bcclean", &["clean"]);
}

#[test]
fn where_the_emulator_is_known_to_differ_from_a_real_terminal() {
    // Kept here so that the day the library is put right, this says so and
    // the case can move to the table above.
    let sb = Sandbox::new();
    let mut p = Player::new(&sb, 40, 10);
    // "Erase from the start of the screen to the cursor", with the cursor
    // on the second row, should clear the first row too. The library
    // clears the rows above only from the third row down.
    p.expect("erase above, from the second row", b"\x1b[2J\x1b[Hfirst\r\nsecond\r\nthird\x1b[2;3H\x1b[1J", &["first", "   ond", "third"]);
    p.expect("from the third row it is right", b"\x1b[2J\x1b[Hfirst\r\nsecond\r\nthird\x1b[3;3H\x1b[1J", &["", "", "   rd"]);
}

/// A small generator of numbers that gives the same ones every time.
struct Dice(u64);

impl Dice {
    fn roll(&mut self, sides: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % sides
    }
}

/// One thing a program might write, chosen by the dice.
fn something(dice: &mut Dice, cols: u64, rows: u64) -> String {
    let n = |dice: &mut Dice, most: u64| dice.roll(most) + 1;
    match dice.roll(40) {
        0..=11 => {
            const WORDS: [&str; 12] = ["alpha", "beta ", "gamma", "δέλτα", "日本", "🙂", "x", "  ", "e\u{301}", "wörld", "0123456789", "한"];
            (0..n(dice, 6)).map(|_| WORDS[dice.roll(12) as usize]).collect()
        }
        12 | 13 => "\r\n".into(),
        14 => "\n".into(),
        15 => "\r".into(),
        16 => "\t".into(),
        17 => "\x08".into(),
        18..=21 => format!("\x1b[{};{}H", n(dice, rows), n(dice, cols)),
        22 => format!("\x1b[{}{}", n(dice, 5), ["A", "B", "C", "D", "E", "F"][dice.roll(6) as usize]),
        23 => format!("\x1b[{}G", n(dice, cols)),
        24..=27 => {
            let codes = ["0", "1", "2", "3", "4", "7", "9", "22", "23", "24", "27", "29", "39", "49"];
            let color = match dice.roll(5) {
                0 => format!("{}", 30 + dice.roll(8)),
                1 => format!("{}", 40 + dice.roll(8)),
                2 => format!("{}", 90 + dice.roll(8)),
                3 => format!("38;5;{}", dice.roll(256)),
                _ => format!("48;2;{};{};{}", dice.roll(256), dice.roll(256), dice.roll(256)),
            };
            format!("\x1b[{};{color}m", codes[dice.roll(14) as usize])
        }
        28 => format!("\x1b[{}J", dice.roll(3)),
        29 => format!("\x1b[{}K", dice.roll(3)),
        30 => format!("\x1b[{}{}", n(dice, 4), ["L", "M", "P", "@", "X", "S", "T"][dice.roll(7) as usize]),
        31 => {
            let top = n(dice, rows - 2);
            format!("\x1b[{top};{}r", top + n(dice, rows - top))
        }
        32 => "\x1b[r".into(),
        33 => ["\x1b[?7l", "\x1b[?7h", "\x1b[?6h", "\x1b[?6l", "\x1b[4h", "\x1b[4l"][dice.roll(6) as usize].into(),
        34 => ["\x1b7", "\x1b8", "\x1bM", "\x1bD", "\x1bE"][dice.roll(5) as usize].into(),
        35 => ["\x1b[?25l", "\x1b[?25h"][dice.roll(2) as usize].into(),
        36 => ["\x1b[?1049h", "\x1b[?1049l"][dice.roll(2) as usize].into(),
        37 => format!("\x1b[{}b", n(dice, 8)),
        38 => (0..n(dice, 30)).map(|k| format!("line {k}\r\n")).collect(),
        _ => "\x1b[?6l\x1b[4l\x1b[?7h\x1b[r\x1b[0m".into(),
    }
}

#[test]
fn a_long_run_of_anything_at_all_stays_in_step() {
    // The same runs every time; a failure names the run and the step, to go
    // straight back to it. The first is as good as any; the others each
    // found something once (half a double-width character at the edge of
    // the pane, one parted from its other half, and the second half of
    // one left behind in color when another was drawn over the first).
    // ASTRUM_TEST_SEED tries another run, when looking for trouble.
    let asked = std::env::var("ASTRUM_TEST_SEED").ok().and_then(|s| s.parse().ok());
    let seeds = asked.map_or(vec![0x5eed_1234_abcd_ef01, 5, 7, 10], |seed| vec![seed]);
    for seed in seeds {
        let sb = Sandbox::new();
        let mut p = Player::new(&sb, 72, 18);
        let mut dice = Dice(seed);
        let mut all = Vec::new();
        for step in 0..120 {
            let burst: String = (0..(dice.roll(40) + 1)).map(|_| something(&mut dice, 72, 16)).collect();
            all.extend_from_slice(burst.as_bytes());
            // (ASTRUM_TEST_DUMP keeps everything written so far, to play again.)
            if let Ok(path) = std::env::var("ASTRUM_TEST_DUMP") {
                std::fs::write(path, &all).unwrap();
            }
            p.play(burst.as_bytes());
            p.check(&format!("run {seed}, step {step}, after {burst:?}"));
        }
    }
}

/// Play the first ASTRUM_TEST_BYTES bytes of the file ASTRUM_TEST_REPLAY
/// in one go, then the next ASTRUM_TEST_THEN: for narrowing down a
/// difference the long run found.
#[test]
#[ignore = "a tool, not a test"]
fn replay_a_run() {
    let Ok(path) = std::env::var("ASTRUM_TEST_REPLAY") else { return };
    let bytes = std::fs::read(path).unwrap();
    let number = |name: &str| std::env::var(name).ok().and_then(|n| n.parse::<usize>().ok());
    let first = number("ASTRUM_TEST_BYTES").unwrap_or(bytes.len()).min(bytes.len());
    let then = number("ASTRUM_TEST_THEN").unwrap_or(0).min(bytes.len() - first);
    let sb = Sandbox::new();
    let mut p = Player::new(&sb, 72, 18);
    p.play(&bytes[..first]);
    p.check("the first part");
    // (ASTRUM_TEST_LOOK=row,col prints the cells around there, as raw as
    // the emulator has them, before and after the next part.)
    let look_at = std::env::var("ASTRUM_TEST_LOOK").ok().and_then(|at| at.split_once(',').map(|(r, c)| (r.parse::<usize>().unwrap(), c.parse::<usize>().unwrap())));
    let raw = |p: &Player, label: &str| {
        if let Some((row, col)) = look_at {
            let cells: Vec<String> = (col.saturating_sub(5)..(col + 5).min(p.cols)).map(|c| {
                let cell = &p.reference.grid()[Line(row as i32)][Column(c)];
                format!("{c}:{:?}/{:x}/{:?}", cell.c, color(cell.bg), cell.flags)
            }).collect();
            println!("LOOK {label}: {}", cells.join("  "));
        }
    };
    raw(&p, "before");
    if then > 0 {
        p.parser.advance(&mut p.reference, &bytes[first..first + then]);
        raw(&p, "after ");
        // (Put the reference back: `play` feeds it again.)
        p.reference = Term::new(TermConfig::default(), &Dims(p.cols, p.rows), VoidListener);
        p.parser = Processor::new();
        p.parser.advance(&mut p.reference, b"\x1bc \r");
        p.parser.advance(&mut p.reference, &bytes[..first]);
        p.play(&bytes[first..first + then]);
        p.check(&format!("the next part, {:?}", String::from_utf8_lossy(&bytes[first..first + then])));
    }
}

// ---- tabs on other machines -----------------------------------------------------

/// Another machine, as far as Astrum can tell: a home folder and a runtime
/// folder of its own on this one, reached through a stand-in for `ssh`
/// that runs what it is given there. (What `ssh` itself does, the real
/// thing is tried for; see the roadmap.)
struct FarBox {
    dir: PathBuf,
    runtime: PathBuf,
    ssh: PathBuf,
}

impl FarBox {
    fn new(sb: &Sandbox) -> FarBox {
        use std::os::unix::fs::PermissionsExt;
        let dir = sb.root.join("far");
        let runtime = PathBuf::from(format!("{}f", sb.runtime.display()));
        std::fs::create_dir_all(dir.join("home")).unwrap();
        std::fs::create_dir_all(dir.join("cfg")).unwrap();
        std::fs::write(dir.join("home").join(".bash_profile"), "PS1='far$ '\n").unwrap();
        let ssh = dir.join("ssh");
        // The command is the last word. Before it is run: whatever the
        // test has asked this "connection" to do first.
        let script = format!(
            r#"#!/bin/sh
for last; do :; done
far='{dir}'
echo "$@" >>"$far/calls"
if [ -e "$far/unreachable" ]; then echo "ssh: connect to host farbox port 22: Network is unreachable" >&2; exit 255; fi
if [ -e "$far/password" ]; then
  stty -echo </dev/tty; printf "tester@farbox's password: " >/dev/tty
  IFS= read -r given </dev/tty; stty echo </dev/tty; echo >/dev/tty
  if [ "$given" != "$(cat "$far/password")" ]; then echo "Permission denied, please try again." >&2; exit 255; fi
fi
if [ -e "$far/greeting" ]; then cat "$far/greeting"; fi
echo $$ >"$far/ssh.pid"
export HOME="$far/home" ASTRUM_RUNTIME_DIR='{runtime}' ASTRUM_CONFIG_DIR="$far/cfg" ASTRUM_KEYCHAIN_FILE="$far/keychain.json"
export ASTRUM_HOLD_LINGER_MS=300 PATH=/usr/bin:/bin:/usr/sbin:/sbin USER=tester
if [ -e "$far/env" ]; then . "$far/env"; fi
unset ASTRUM ASTRUM_SESSION ASTRUM_TAB ASTRUM_DOWNLOAD_BASE XDG_CONFIG_HOME XDG_DATA_HOME
cd "$HOME" && exec /bin/sh -c "$last"
"#,
            dir = dir.display(),
            runtime = runtime.display()
        );
        std::fs::write(&ssh, script).unwrap();
        std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o755)).unwrap();
        FarBox { dir, runtime, ssh }
    }

    /// With Astrum already there, where the installer would have put it.
    fn with_astrum(sb: &Sandbox) -> FarBox {
        let far = FarBox::new(sb);
        let bin = far.dir.join("home/.local/bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::os::unix::fs::symlink(BIN, bin.join("astrum")).unwrap();
        far
    }

    fn env(&self) -> Vec<(&'static str, String)> {
        vec![("ASTRUM_SSH", self.ssh.to_string_lossy().into_owned())]
    }

    fn attach(&self, sb: &Sandbox, cols: u16, rows: u16) -> Client {
        let env = self.env();
        let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
        sb.attach_env("main", cols, rows, &env)
    }

    /// The connection drops: whatever is at this end of it goes.
    fn drop_connection(&self) {
        let pid = std::fs::read_to_string(self.dir.join("ssh.pid")).unwrap();
        let _ = std::fs::remove_file(self.dir.join("ssh.pid"));
        Command::new("kill").arg("-9").arg(pid.trim()).status().unwrap();
    }

    fn calls(&self) -> usize {
        std::fs::read_to_string(self.dir.join("calls")).unwrap_or_default().lines().count()
    }
}

impl Drop for FarBox {
    fn drop(&mut self) {
        // The keeper there goes by itself once its shells have; a test
        // that failed may have left them.
        let _ = Command::new("pkill").arg("-f").arg(format!("ASTRUM_RUNTIME_DIR={}", self.runtime.display())).status();
        let _ = std::fs::remove_dir_all(&self.runtime);
    }
}

/// What is on the screen as running text, wherever its lines were broken.
fn flowed(c: &Client) -> String {
    c.text().split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Open a tab on the other machine, with the keyboard.
fn open_remote_tab(c: &Client) {
    c.send(CTRL_B);
    c.send(b"R");
    c.wait_for("New tab on another machine");
    c.send(b"farbox\r");
}

#[test]
fn a_tab_on_another_machine_has_its_shell_there() {
    let sb = Sandbox::new();
    let far = FarBox::with_astrum(&sb);
    let c = far.attach(&sb, 120, 30);
    c.wait_for("ready$");
    // With the mouse: the menu, its entry, and the machine's name.
    let (col, row) = c.position_of("≡ menu").unwrap();
    c.click(col + 2, row);
    c.wait_for("New tab on another machine…");
    let (col, row) = c.position_of("New tab on another machine…").unwrap();
    c.click(col + 2, row);
    c.wait_for("as you would name it to ssh");
    c.send(b"-p 2222 tester@farbox\r");
    c.wait_for("far$");
    c.send(b"echo over-$((6*7))-there; echo home is $HOME\r");
    c.wait_for("over-42-there");
    assert!(c.text().contains(&format!("home is {}", far.dir.join("home").display())), "{}", c.text());
    // `ssh` was run once, as the user would have run it, with one thing to do there.
    let calls = std::fs::read_to_string(far.dir.join("calls")).unwrap();
    assert!(calls.starts_with("-T -p 2222 tester@farbox sh -c ") && calls.lines().count() == 1, "{calls}");
    // The tab and the status bar say which machine this is.
    c.wait_until("the tab to be named for the machine", |c| c.lines()[0].contains(" 2 farbox: "));
    c.wait_until("the status bar to say whose shell it is", |c| c.lines()[29].contains("⇢ tester@") && c.lines()[29].contains(" ~ "));
    // The first tab is still this machine's.
    c.send(CTRL_B);
    c.send(b"1");
    c.wait_until("the first tab", |c| !c.lines()[29].contains("⇢") && c.text().contains("ready$"));
}

#[test]
fn a_shell_there_outlasts_the_connection_and_is_found_again() {
    let sb = Sandbox::new();
    let far = FarBox::with_astrum(&sb);
    let c = far.attach(&sb, 120, 30);
    c.wait_for("ready$");
    open_remote_tab(&c);
    c.wait_for("far$");
    // Something that will finish while nobody is connected, and something
    // only that shell knows.
    c.send(b"kept=in-this-shell; (sleep 1.5; echo finished-while-away) &\r");
    c.wait_until("the job to be started", |c| c.text().contains("[1]"));
    far.drop_connection();
    c.wait_for("was lost");
    assert!(c.text().contains("still running"), "{}", c.text());

    // It is tried again by itself, and what the shell wrote meanwhile is there.
    c.wait_for("finished-while-away");
    assert_eq!(far.calls(), 2);
    assert!(!c.text().contains("was lost"), "the note is gone once the shell is back:\n{}", c.text());
    c.send(b"echo still $kept\r");
    c.wait_for("still in-this-shell");
    // What was on the screen before is still above it, once.
    assert_eq!(c.text().matches("kept=in-this-shell").count(), 1, "{}", c.text());
}

#[test]
fn a_machine_that_cannot_be_reached_says_so_and_can_be_tried_again() {
    let sb = Sandbox::new();
    let far = FarBox::with_astrum(&sb);
    std::fs::write(far.dir.join("unreachable"), "").unwrap();
    let c = far.attach(&sb, 120, 30);
    c.wait_for("ready$");
    open_remote_tab(&c);
    c.wait_for("Network is unreachable");
    c.wait_for("Try again");
    // Never reached, so not tried again by itself: that would ask for a
    // password over and over.
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(far.calls(), 1);

    // The button, once the machine is back.
    std::fs::remove_file(far.dir.join("unreachable")).unwrap();
    let (col, row) = c.position_of("Try again").unwrap();
    c.click(col + 2, row);
    c.wait_for("far$");
}

#[test]
fn what_ssh_asks_is_shown_and_answered_in_the_pane() {
    let sb = Sandbox::new();
    let far = FarBox::with_astrum(&sb);
    std::fs::write(far.dir.join("password"), "open sesame").unwrap();
    // And a machine that greets every login, before anything is run.
    std::fs::write(far.dir.join("greeting"), "Welcome to farbox\nASTRUM-LINKS are not ours\n").unwrap();
    let c = far.attach(&sb, 120, 30);
    c.wait_for("ready$");
    open_remote_tab(&c);
    c.wait_for("tester@farbox's password:");
    c.send(b"wrong\r");
    c.wait_for("Permission denied");
    assert!(!c.text().contains("wrong"), "a password is not shown as it is typed:\n{}", c.text());
    c.send(b"\r");
    c.wait_until("the question again", |c| c.text().matches("password:").count() == 1 && !c.text().contains("Permission denied"));
    c.send(b"open sesame\r");
    c.wait_for("far$");
    assert!(!c.text().contains("sesame") && !c.text().contains("Welcome to farbox"), "{}", c.text());
}

impl FarBox {
    /// Is the program that keeps the shells running there?
    fn keeping(&self) -> bool {
        std::fs::read_dir(&self.runtime).is_ok_and(|d| d.flatten().any(|e| e.file_name().to_string_lossy().ends_with(".link")))
    }

    fn attach_saving(&self, sb: &Sandbox, cols: u16, rows: u16) -> Client {
        let mut env = self.env();
        env.push(("ASTRUM_SAVE_AFTER_MS", "200".into()));
        let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
        sb.attach_env("main", cols, rows, &env)
    }
}

#[test]
fn a_split_is_on_the_same_machine_and_closing_ends_the_shells_there() {
    let sb = Sandbox::new();
    let far = FarBox::with_astrum(&sb);
    let c = far.attach(&sb, 120, 30);
    c.wait_for("ready$");
    open_remote_tab(&c);
    c.wait_for("far$");
    c.send(b"cd /tmp && echo left-$((1+1))\r");
    c.wait_for("left-2");
    c.send(CTRL_B);
    c.wait_for("PREFIX");
    c.send(b"|");
    c.wait_until("a prompt in the new pane", |c| c.column_of("far$").is_some_and(|col| col > 50));
    // Over the connection that was there, and where the first pane was.
    assert_eq!(far.calls(), 1);
    c.send(b"echo right in $PWD\r");
    c.wait_until("the new shell to say where it is", |c| c.text().contains("right in /tmp") || c.text().contains("right in /private/tmp"));
    assert!(far.keeping());

    // Each pane closed ends its shell; with the last, the tab goes, and
    // the program that kept them has nothing left to stay for.
    c.send(b"exit\r");
    c.wait_until("one pane again", |c| !c.text().contains("right in"));
    c.send(CTRL_B);
    c.wait_for("PREFIX");
    c.send(b"x");
    c.wait_for("ready$");
    c.wait_until("the keeper there to go", |_| !far.keeping());
}

#[test]
fn a_session_that_dies_finds_its_shells_on_the_other_machine_again() {
    let sb = Sandbox::new();
    let far = FarBox::with_astrum(&sb);
    let mut c = far.attach_saving(&sb, 120, 30);
    c.wait_for("ready$");
    c.send(b"echo server-$PPID\r");
    c.wait_for("server-");
    c.wait_for("ready$");
    let server = number_after(&c, "server-");
    open_remote_tab(&c);
    c.wait_for("far$");
    c.send(b"kept=through-the-crash; echo it is $kept\r");
    c.wait_for("it is through-the-crash");
    let saved = saved_session(&sb, "main").join("session.json");
    c.wait_until("the tab to be written out", |_| std::fs::read_to_string(&saved).is_ok_and(|t| t.contains("\"host\": \"farbox\"")));

    // This machine's side goes without a word. The shell there stays.
    unsafe { libc::kill(server, libc::SIGKILL) };
    c.wait_exit();
    std::thread::sleep(Duration::from_millis(600));
    assert!(far.keeping(), "the shell there was kept");
    let c = far.attach_saving(&sb, 120, 30);
    c.wait_for("Restored 2 tabs");
    // The same shell, with what it had on its screen and what only it knew.
    c.wait_for("it is through-the-crash");
    c.send(b"echo still $kept\r");
    c.wait_for("still through-the-crash");
    assert!(!c.text().contains("restored ·"), "nothing was put back from disk: the shell itself is there\n{}", c.text());
}

#[test]
fn stopping_the_session_ends_its_shells_there_and_the_tab_still_comes_back() {
    let sb = Sandbox::new();
    let far = FarBox::with_astrum(&sb);
    let mut c = far.attach_saving(&sb, 120, 30);
    c.wait_for("ready$");
    open_remote_tab(&c);
    c.wait_for("far$");
    c.send(b"cd /usr && echo before-the-stop-$((6*7))\r");
    c.wait_for("before-the-stop-42");
    let dir = saved_session(&sb, "main");
    c.wait_until("the pane's text to be written out", |_| {
        std::fs::read_dir(&dir).is_ok_and(|d| d.flatten().any(|e| std::fs::read_to_string(e.path()).is_ok_and(|t| t.contains("before-the-stop-42"))))
    });
    sb.run(&["-s", "main", "kill"]);
    c.wait_exit();
    c.wait_until("the keeper there to go", |_| !far.keeping());

    // Started again: the tab is back, on that machine, in a new shell
    // where the old one was, under what the old one had shown.
    let c = far.attach_saving(&sb, 120, 30);
    c.wait_for("Restored 2 tabs");
    c.wait_for("before-the-stop-42");
    c.wait_for("restored ·");
    c.wait_for("far$");
    c.send(b"echo now in $PWD\r");
    c.wait_for("now in /usr");
}

/// A release laid out in a folder, holding this build under the name it
/// would have for this kind of machine. `None` where there is no build
/// for it (an Intel Mac).
fn release_folder(sb: &Sandbox) -> Option<(PathBuf, String)> {
    let uname = |flag: &str| String::from_utf8_lossy(&Command::new("uname").arg(flag).output().unwrap().stdout).trim().to_string();
    let os = match uname("-s").as_str() {
        "Linux" => "linux",
        "Darwin" => "darwin",
        _ => return None,
    };
    let arch = match uname("-m").as_str() {
        "x86_64" | "amd64" => "amd64",
        "aarch64" | "arm64" => "arm64",
        _ => return None,
    };
    if (os, arch) == ("darwin", "amd64") {
        return None;
    }
    let asset = format!("astrum-{os}-{arch}");
    let dir = sb.root.join("release");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(BIN, dir.join(&asset)).unwrap();
    let sum = Command::new("sh").arg("-c").arg(format!("cd '{}' && (sha256sum {asset} 2>/dev/null || shasum -a 256 {asset})", dir.display())).output().unwrap();
    std::fs::write(dir.join("SHA256SUMS"), sum.stdout).unwrap();
    Some((dir, asset))
}

impl FarBox {
    fn attach_with_release(&self, sb: &Sandbox, release: &std::path::Path) -> Client {
        let mut env = self.env();
        env.push(("ASTRUM_DOWNLOAD_BASE", format!("file://{}", release.display())));
        let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
        sb.attach_env("main", 120, 30, &env)
    }

    fn installed(&self) -> PathBuf {
        self.dir.join("home/.local/bin/astrum")
    }

    /// Something called `astrum` there that is not this version.
    fn with_other_astrum(sb: &Sandbox, body: &str) -> FarBox {
        use std::os::unix::fs::PermissionsExt;
        let far = FarBox::new(sb);
        std::fs::create_dir_all(far.installed().parent().unwrap()).unwrap();
        std::fs::write(far.installed(), format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(far.installed(), std::fs::Permissions::from_mode(0o755)).unwrap();
        far
    }
}

#[test]
fn astrum_is_copied_to_a_machine_that_lacks_it_once_you_say_so() {
    let sb = Sandbox::new();
    let Some((release, asset)) = release_folder(&sb) else { return };
    let far = FarBox::new(&sb);
    let c = far.attach_with_release(&sb, &release);
    c.wait_for("ready$");
    open_remote_tab(&c);
    c.wait_for("Astrum is not on that machine.");
    // Asked first: nothing is there until the answer.
    assert!(flowed(&c).contains("~/.local/bin/astrum") && flowed(&c).contains("needs no internet access"), "{}", c.text());
    std::thread::sleep(Duration::from_millis(300));
    assert!(!far.installed().exists());
    let (col, row) = c.position_of("Yes, copy it there").unwrap();
    c.click(col + 2, row);
    c.wait_for("far$");
    assert_eq!(std::fs::read(far.installed()).unwrap(), std::fs::read(release.join(&asset)).unwrap(), "what arrived is what was sent");
    let left: Vec<String> = std::fs::read_dir(far.installed().parent().unwrap()).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(left, ["astrum"], "nothing else is left beside it");
    c.send(b"echo over-$((6*7))-there\r");
    c.wait_for("over-42-there");
    // One connection for all of it: asked, copied and used.
    assert_eq!(far.calls(), 1);
}

#[test]
fn nothing_is_copied_when_you_say_no_or_the_checksum_is_wrong() {
    let sb = Sandbox::new();
    let Some((release, asset)) = release_folder(&sb) else { return };
    let far = FarBox::new(&sb);
    let c = far.attach_with_release(&sb, &release);
    c.wait_for("ready$");
    open_remote_tab(&c);
    c.wait_for("Astrum is not on that machine.");
    c.send(b"n");
    c.wait_for("Nothing was copied");
    assert!(!far.installed().parent().unwrap().exists());

    // A build that is not the one its checksum was made for.
    let mut bytes = std::fs::read(release.join(&asset)).unwrap();
    bytes.extend_from_slice(b"something added");
    std::fs::write(release.join(&asset), bytes).unwrap();
    c.send(b"\r");
    c.wait_until("the question to be asked afresh", |c| c.text().contains("Astrum is not on that machine.") && !c.text().contains("Nothing was copied"));
    c.send(b"y");
    c.wait_for("does not match its checksum");
    c.wait_for("Try again");
    std::thread::sleep(Duration::from_millis(300));
    assert!(!far.installed().exists(), "nothing was sent");
}

#[test]
fn an_astrum_from_before_this_is_replaced_only_when_you_say_so() {
    let sb = Sandbox::new();
    let Some((release, _)) = release_folder(&sb) else { return };
    // One that knows nothing of tabs on other machines.
    let old = "case \"$1\" in --version) echo astrum 0.4.0;; *) echo unknown >&2; exit 2;; esac";
    let far = FarBox::with_other_astrum(&sb, old);
    let c = far.attach_with_release(&sb, &release);
    c.wait_for("ready$");
    open_remote_tab(&c);
    c.wait_for("The Astrum there (0.4.0) is from before tabs on other machines.");
    assert!(flowed(&c).contains("put this version in ~/.local/bin/astrum"), "{}", c.text());
    assert!(std::fs::read_to_string(far.installed()).unwrap().contains("0.4.0"));
    c.send(b"y");
    c.wait_for("far$");
    assert!(std::fs::read(far.installed()).unwrap().len() > 100_000, "the new one is in its place");
}

#[test]
fn two_versions_that_cannot_talk_say_which_is_older() {
    let sb = Sandbox::new();
    let Some((release, _)) = release_folder(&sb) else { return };
    // Older, but new enough to say what it speaks.
    let older = "case \"$1 $2\" in '--version ') echo astrum 0.4.9;; '__link --speaks') echo 0;; *) exit 2;; esac";
    let far = FarBox::with_other_astrum(&sb, older);
    let c = far.attach_with_release(&sb, &release);
    c.wait_for("ready$");
    open_remote_tab(&c);
    c.wait_for("(0.4.9) is older than this one");
    assert!(flowed(&c).contains("do not speak the same language") && c.text().contains("Yes, copy it there"), "{}", c.text());
    c.send(b"n");
    c.wait_for("Nothing was copied");
    drop(c);

    // Newer: never replaced by an older one.
    let sb = Sandbox::new();
    let newer = "case \"$1 $2\" in '--version ') echo astrum 9.0.0;; '__link --speaks') echo 99;; *) exit 2;; esac";
    let far = FarBox::with_other_astrum(&sb, newer);
    let c = far.attach_with_release(&sb, &release);
    c.wait_for("ready$");
    open_remote_tab(&c);
    c.wait_for("The Astrum there (9.0.0) is newer than this one");
    assert!(flowed(&c).contains("Update Astrum on this machine") && !c.text().contains("Yes, copy it there"), "{}", c.text());
    assert!(std::fs::read_to_string(far.installed()).unwrap().contains("9.0.0"), "it is left as it was");
}

#[test]
fn the_agent_in_a_tab_on_another_machine_works_on_that_machine() {
    let (sb, bodies) = agent_sandbox("ask");
    let far = FarBox::with_astrum(&sb);
    // A project that is on the other machine, with rules of its own; and
    // a file of the same name on this one, which is not to be touched.
    let there = far.dir.join("home/proj");
    std::fs::create_dir_all(there.join(".git")).unwrap();
    std::fs::write(there.join("notes.txt"), "far line 1\nfar line 2\nfar line 3\n").unwrap();
    std::fs::write(there.join("AGENTS.md"), "Rule kept with the far project.\n").unwrap();
    let here = sb.root.join("home/proj");
    std::fs::create_dir_all(&here).unwrap();
    std::fs::write(here.join("notes.txt"), "this machine's notes\n").unwrap();
    std::fs::write(sb.root.join("cfg/AGENTS.md"), "Rule the user keeps for every project.\n").unwrap();
    // (Each tool call is two requests: the call, then its result going back.)
    let answers = |n: usize| {
        let bodies = bodies.clone();
        move |c: &Client| bodies.lock().unwrap().len() == 2 * n && c.text().contains("Ask anything")
    };
    let last_result = || tool_result_in(bodies.lock().unwrap().last().unwrap());

    let c = far.attach(&sb, 130, 40);
    c.wait_for("ready$");
    open_remote_tab(&c);
    c.wait_for("far$");
    c.send(b"cd proj\r");
    c.wait_until("the shell there to say where it is", |c| c.lines()[39].contains("proj"));
    // The status bar says whose shell this is.
    assert!(c.lines()[39].contains("⇢ tester@"), "{}", c.lines()[39]);
    c.send(CTRL_SPACE);
    c.wait_for("AGENT");

    // Reading: that machine's file, with no question asked.
    ask_for_tool(&c, "read_file", serde_json::json!({"path": "notes.txt"}));
    c.wait_until("the first answer", answers(1));
    let read = last_result();
    assert!(read.contains("far line 2") && !read.contains("this machine's"), "{read}");
    {
        let sent = bodies.lock().unwrap();
        // It is told which machine, and has that project's rules beside the user's own.
        assert!(sent[0].contains("This tab is on another machine") && sent[0].contains("machine: tester@"), "{}", sent[0]);
        assert!(sent[0].contains("Rule kept with the far project.") && sent[0].contains("Rule the user keeps for every project."), "{}", sent[0]);
        assert!(sent[0].contains(&format!("home: {}", far.dir.join("home").display())), "{}", sent[0]);
        assert!(sent[0].contains("\"name\":\"edit_file\""));
    }

    // A command: in the shell there, after the usual question.
    ask_for_tool(&c, "run_command", serde_json::json!({"command": "echo ran-$((4+4))-in $HOME"}));
    c.wait_for("Run this command?");
    c.send(b"y");
    c.wait_until("the second answer", answers(2));
    assert!(last_result().contains(&format!("ran-8-in {}", far.dir.join("home").display())), "{}", last_result());

    // A change: shown as a diff of that machine's file, and made there.
    ask_for_tool(&c, "edit_file", serde_json::json!({"path": "notes.txt", "old_text": "far line 2\n", "new_text": "changed over there\n"}));
    c.wait_for("Edit ");
    c.wait_for("+ changed over there");
    assert_eq!(std::fs::read_to_string(there.join("notes.txt")).unwrap(), "far line 1\nfar line 2\nfar line 3\n", "not before the answer");
    c.send(b"y");
    c.wait_until("the third answer", answers(3));
    assert!(last_result().starts_with("Edited "), "{}", last_result());
    assert_eq!(std::fs::read_to_string(there.join("notes.txt")).unwrap(), "far line 1\nchanged over there\nfar line 3\n");
    assert_eq!(std::fs::read_to_string(here.join("notes.txt")).unwrap(), "this machine's notes\n");

    // Looking for files and text: there too.
    ask_for_tool(&c, "search_files", serde_json::json!({"pattern": "changed over"}));
    c.wait_until("the fourth answer", answers(4));
    assert!(last_result().contains("notes.txt:2: changed over there"), "{}", last_result());

    // With the shell there busy, a command is run beside it: on that
    // machine too, and it is told that is what happened.
    c.send(CTRL_SPACE);
    c.send(b"cat\r");
    c.wait_until("the shell there to be busy", |c| c.lines()[39].contains("watch"));
    c.send(CTRL_SPACE);
    c.wait_for("Ask anything");
    ask_for_tool(&c, "run_command", serde_json::json!({"command": "echo beside-$((5+5))-in $HOME"}));
    c.wait_for("Run this command?");
    c.send(b"y");
    c.wait_until("the fifth answer", answers(5));
    assert!(last_result().contains(&format!("beside-10-in {}", far.dir.join("home").display())) && last_result().contains("separate background shell"), "{}", last_result());
    c.send(CTRL_SPACE);
    c.send(b"\x04");
    c.wait_until("the shell there to be free again", |c| !c.lines()[39].contains("watch"));
    c.send(CTRL_SPACE);
    c.wait_for("Ask anything");

    // With the connection down, it is told so rather than left waiting.
    std::fs::write(far.dir.join("unreachable"), "").unwrap();
    far.drop_connection();
    c.wait_for("was lost");
    ask_for_tool(&c, "read_file", serde_json::json!({"path": "notes.txt"}));
    c.wait_until("the sixth answer", answers(6));
    assert!(last_result().contains("is down right now") && last_result().contains("still running"), "{}", last_result());
    std::fs::remove_file(far.dir.join("unreachable")).unwrap();
    c.wait_until("the shell there to be back", |c| c.text().contains("far$") && !c.text().contains("was lost"));
    ask_for_tool(&c, "read_file", serde_json::json!({"path": "notes.txt"}));
    c.wait_until("the seventh answer", answers(7));
    assert!(last_result().contains("changed over there"), "{}", last_result());
}

#[test]
fn a_shell_that_wrote_too_much_while_away_is_shown_as_it_now_stands() {
    let sb = Sandbox::new();
    let far = FarBox::with_astrum(&sb);
    // Only the last few thousand bytes a shell wrote are kept for sending again.
    std::fs::write(far.dir.join("env"), "export ASTRUM_HOLD_KEPT=3000\n").unwrap();
    let c = far.attach(&sb, 100, 24);
    c.wait_for("ready$");
    open_remote_tab(&c);
    c.wait_for("far$");
    c.send(b"printf '\\033[32mgreen-before\\033[0m\\n'; (sleep 1; seq 1 3000; echo flood-is-over) &\r");
    c.wait_for("green-before");
    std::fs::write(far.dir.join("unreachable"), "").unwrap();
    far.drop_connection();
    c.wait_for("was lost");
    std::thread::sleep(Duration::from_millis(2500));
    std::fs::remove_file(far.dir.join("unreachable")).unwrap();
    // Far more was written than is kept: the screen comes as it is, with
    // the history the terminal there has.
    c.wait_for("flood-is-over");
    assert!(c.text().contains("3000") && !c.text().contains("was lost"), "{}", c.text());
    // The shell is where it was left (at its prompt), and is the same shell.
    c.send(b"\recho jobs-$(jobs | wc -l | tr -d ' ')-here\r");
    c.wait_for("-here");
    // Back through the history: what was there before the flood, in its color.
    c.send(CTRL_B);
    c.wait_for("PREFIX");
    c.send(b"/");
    c.send(b"green-before\r");
    c.wait_until("the line from before the flood", |c| c.lines().iter().any(|l| l.trim_end() == "green-before"));
}

#[test]
fn a_full_screen_program_there_is_found_as_it_was_left() {
    let sb = Sandbox::new();
    let far = FarBox::with_astrum(&sb);
    std::fs::write(far.dir.join("env"), "export ASTRUM_HOLD_KEPT=200\n").unwrap();
    let mut c = far.attach_saving(&sb, 100, 24);
    c.wait_for("ready$");
    c.send(b"echo server-$PPID\r");
    c.wait_for("server-");
    c.wait_for("ready$");
    let server = number_after(&c, "server-");
    open_remote_tab(&c);
    c.wait_for("far$");
    c.send(b"echo beneath-the-pager; seq 1 500 | less\r");
    c.wait_until("the pager's first page", |c| c.lines().iter().any(|l| l.trim_end() == "21") && c.text().contains(":"));
    let saved = saved_session(&sb, "main").join("session.json");
    c.wait_until("the tab to be written out", |_| std::fs::read_to_string(&saved).is_ok_and(|t| t.contains("\"host\": \"farbox\"")));
    unsafe { libc::kill(server, libc::SIGKILL) };
    c.wait_exit();

    // A new session knows nothing of the shell: it is shown the screen.
    let c = far.attach_saving(&sb, 100, 24);
    c.wait_for("Restored 2 tabs");
    c.wait_until("the pager as it was", |c| c.lines().iter().any(|l| l.trim_end() == "21") && !c.text().contains("beneath-the-pager"));
    // It is the pager itself: it still takes keys, and leaving it shows
    // the shell's own screen beneath.
    c.send(b"G");
    c.wait_until("the pager's last page", |c| c.lines().iter().any(|l| l.trim_end() == "500"));
    c.send(b"q");
    c.wait_for("beneath-the-pager");
    c.send(b"echo and-$((1+1))-more\r");
    c.wait_for("and-2-more");
}

#[test]
fn a_connection_that_stalls_does_not_hold_the_shell_up_and_catches_up_after() {
    let sb = Sandbox::new();
    let far = FarBox::with_astrum(&sb);
    // (A connection with this much waiting to go is not keeping up.)
    std::fs::write(far.dir.join("env"), "export ASTRUM_HOLD_BEHIND=20000\n").unwrap();
    let c = far.attach(&sb, 100, 24);
    c.wait_for("ready$");
    open_remote_tab(&c);
    c.wait_for("far$");
    let done = far.dir.join("home/done");
    c.send(b"(sleep 0.7; seq 1 400000; echo flood-is-over; touch ~/done) &\r");
    c.wait_until("the job to be started", |c| c.text().contains("[1]"));
    // The connection stops passing anything on, without closing.
    let link = std::fs::read_to_string(far.dir.join("ssh.pid")).unwrap();
    Command::new("kill").arg("-STOP").arg(link.trim()).status().unwrap();
    // The program there is not kept waiting for it: it runs to its end.
    let over = |c: &Client| c.lines().iter().any(|l| l.trim_end() == "flood-is-over");
    c.wait_until("the flood to finish there, unseen", |_| done.exists());
    assert!(!over(&c), "{}", c.text());
    Command::new("kill").arg("-CONT").arg(link.trim()).status().unwrap();
    // And what it wrote arrives, to the last line, in order.
    c.wait_until("the end of the flood", over);
    let lines = c.lines();
    let at = lines.iter().position(|l| l.trim_end() == "flood-is-over").unwrap();
    assert_eq!(lines[at - 3..at].iter().map(|l| l.trim_end()).collect::<Vec<_>>(), ["399998", "399999", "400000"], "{}", c.text());
    c.send(b"\recho typed-$((2+2))-after\r");
    c.wait_for("typed-4-after");
}

// ---- what the review before 0.5.0 found -----------------------------------------

#[test]
fn a_yes_given_after_the_connection_went_does_not_run_the_command_here() {
    let (sb, bodies) = agent_sandbox("ask");
    let far = FarBox::with_astrum(&sb);
    let c = far.attach(&sb, 130, 40);
    c.wait_for("ready$");
    open_remote_tab(&c);
    c.wait_for("far$");
    c.send(CTRL_SPACE);
    c.wait_for("AGENT");
    let here = sb.root.join("home/ran-on-the-wrong-machine");
    ask_for_tool(&c, "run_command", serde_json::json!({"command": "touch ~/ran-on-the-wrong-machine; echo ran in $HOME"}));
    c.wait_for("Run this command?");
    // The connection goes while the question is showing, and stays gone.
    std::fs::write(far.dir.join("unreachable"), "").unwrap();
    far.drop_connection();
    c.wait_for("was lost");
    c.send(b"y");
    c.wait_until("the answer", |c| bodies.lock().unwrap().len() == 2 && c.text().contains("Ask anything"));
    let told = tool_result_in(bodies.lock().unwrap().last().unwrap());
    assert!(told.contains("is down right now") && !told.contains("ran in"), "{told}");
    assert!(!here.exists(), "it was run on this machine");
    assert!(!far.dir.join("home/ran-on-the-wrong-machine").exists());

    // The same for a change to a file that was being looked over.
    std::fs::remove_file(far.dir.join("unreachable")).unwrap();
    c.wait_until("the shell there to be back", |c| c.text().contains("far$") && !c.text().contains("was lost"));
    std::fs::write(sb.root.join("home/notes.txt"), "this machine's\n").unwrap();
    std::fs::write(far.dir.join("home/notes.txt"), "that machine's\n").unwrap();
    ask_for_tool(&c, "write_file", serde_json::json!({"path": "notes.txt", "content": "rewritten\n"}));
    c.wait_for("+ rewritten");
    std::fs::write(far.dir.join("unreachable"), "").unwrap();
    far.drop_connection();
    c.wait_for("was lost");
    c.send(b"y");
    // It is answered (not left waiting for a machine that is not there), and nothing is written anywhere.
    c.wait_until("the second answer", |c| bodies.lock().unwrap().len() == 4 && c.text().contains("Ask anything"));
    let told = tool_result_in(bodies.lock().unwrap().last().unwrap());
    assert!(told.contains("connection") && !told.starts_with("Replaced"), "{told}");
    assert_eq!(std::fs::read_to_string(sb.root.join("home/notes.txt")).unwrap(), "this machine's\n");
    assert_eq!(std::fs::read_to_string(far.dir.join("home/notes.txt")).unwrap(), "that machine's\n");
}

#[test]
fn taking_the_agent_into_a_program_that_ends_at_once_leaves_this_shell_as_it_was() {
    let (sb, bodies) = agent_sandbox("ask");
    std::fs::write(sb.root.join("home/notes.txt"), "still readable\n").unwrap();
    let c = sb.attach("main", 200, 30);
    c.wait_for("ready$");
    // An `ssh` that is refused: it ends a moment after it is started,
    // and whatever was typed for the far side is read by this shell.
    c.send(b"ssh() { sleep 1.2; echo refused; return 255; }\r");
    c.wait_for("ready$");
    c.send(b"ssh nowhere\r");
    c.wait_until("the offer", |c| c.lines()[29].contains("take the agent in"));
    c.send(CTRL_B);
    c.send(b"i");
    c.wait_for("refused");
    c.wait_for("the agent stays in this one");
    std::thread::sleep(Duration::from_millis(1500));
    assert!(!c.lines()[29].contains("⇢"), "{}", c.lines()[29]);
    // This shell's own marks are its own still: its commands are followed,
    // and the agent has this machine's files.
    c.send(b"\x15cd /tmp\r");
    c.wait_until("the directory to be followed", |c| c.lines()[29].contains("/tmp"));
    c.send(CTRL_SPACE);
    c.wait_for("AGENT");
    ask_for_tool(&c, "read_file", serde_json::json!({"path": sb.root.join("home/notes.txt").to_string_lossy()}));
    c.wait_until("the answer", |c| bodies.lock().unwrap().len() == 2 && c.text().contains("Ask anything"));
    assert!(tool_result_in(bodies.lock().unwrap().last().unwrap()).contains("still readable"));
    assert!(!bodies.lock().unwrap()[0].contains("You are working on another machine"));
}

// ---- MCP servers ----------------------------------------------------------------

const MCP_STAND_IN: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/mcp/stand-in.sh");

/// The config of a stand-in MCP server called `name`, and the file it
/// writes what it is sent to.
fn mcp_server(sb: &Sandbox, name: &str, more: &str) -> (String, PathBuf) {
    let log = sb.root.join(format!("mcp-{name}.log"));
    let config = format!("[mcp.{name}]\ncommand = \"sh\"\nargs = [\"{MCP_STAND_IN}\"]\nenv = {{ MCP_LOG = \"{}\", MCP_NAME = \"{name}\" }}\n{more}\n", log.display());
    (config, log)
}

#[test]
fn the_agent_uses_an_mcp_servers_tools_after_asking() {
    let (sb, bodies) = agent_sandbox("ask");
    let (helper, log) = mcp_server(&sb, "helper", "");
    let config = std::fs::read_to_string(sb.root.join("cfg/config.toml")).unwrap();
    std::fs::write(sb.root.join("cfg/config.toml"), format!("{config}\n{helper}")).unwrap();
    let answers = |n: usize| {
        let bodies = bodies.clone();
        move |c: &Client| bodies.lock().unwrap().len() == 2 * n && c.text().contains("Ask anything")
    };
    let last_result = || tool_result_in(bodies.lock().unwrap().last().unwrap());

    let c = sb.attach_env("main", 130, 40, &[("ANTHROPIC_API_KEY", "sk-ant-not-for-servers")]);
    c.wait_for("ready$");
    c.send(CTRL_SPACE);
    c.wait_for("AGENT");
    ask_for_tool(&c, "helper__echo", serde_json::json!({"words": "over the pipe"}));
    c.wait_for("Use echo from helper?");
    assert!(c.text().contains("\"words\": \"over the pipe\""), "what it is given is shown first:\n{}", c.text());
    assert!(!std::fs::read_to_string(&log).unwrap_or_default().contains("tools/call"), "nothing is called before the answer");
    c.send(b"y");
    c.wait_until("the first answer", answers(1));
    assert_eq!(last_result(), "echo: over the pipe");
    assert!(c.text().contains("Used echo (helper)"), "{}", c.text());
    {
        let sent = bodies.lock().unwrap();
        // Offered beside the agent's own tools, and the model is told what they are.
        assert!(sent[0].contains("\"name\":\"helper__echo\"") && sent[0].contains("(A tool of the MCP server `helper`.)") && sent[0].contains("\"name\":\"run_command\""), "{}", sent[0]);
        assert!(sent[0].contains("Tools from MCP servers") && sent[0].contains("Stand-in instructions: be brief."), "{}", sent[0]);
    }
    // The program was given little of Astrum's environment: not the providers' keys.
    let env = std::fs::read_to_string(sb.root.join("mcp-helper.log.env")).unwrap();
    assert!(env.contains("MCP_NAME=helper") && env.lines().any(|l| l.starts_with("PATH=")), "{env}");
    assert!(!env.contains("sk-ant-not-for-servers") && !env.contains("ASTRUM_CONFIG_DIR"), "{env}");

    // A call without what the tool needs is not sent, and not asked about.
    ask_for_tool(&c, "helper__echo", serde_json::json!({}));
    c.wait_until("the second answer", answers(2));
    assert!(last_result().contains("Missing required argument `words`"), "{}", last_result());
    // A tool that says it failed.
    ask_for_tool(&c, "helper__boom", serde_json::json!({}));
    c.wait_for("Use boom from helper?");
    c.send(b"y");
    c.wait_until("the third answer", answers(3));
    assert_eq!(last_result(), "it went wrong");
    // Declined: the model is told, and the server is not called.
    ask_for_tool(&c, "helper__echo", serde_json::json!({"words": "never sent"}));
    c.wait_for("Use echo from helper?");
    c.send(b"n");
    c.wait_for("What should the agent do instead?");
    c.send(b"\r");
    c.wait_until("the fourth answer", answers(4));
    assert!(last_result().contains("declined"), "{}", last_result());
    assert!(!std::fs::read_to_string(&log).unwrap().contains("never sent"));

    // `/mcp` says how it is doing; off for this session, its tools are gone
    // and the program has been ended.
    c.send(b"/mcp\r");
    c.wait_for("helper · ready · 5 tools, helper 1.0");
    c.send(b"/mcp off helper\r");
    c.wait_for("helper is off for this session");
    ask_for_tool(&c, "run_command", serde_json::json!({"command": "true"}));
    c.wait_for("Run this command?");
    c.send(b"n");
    c.wait_for("What should the agent do instead?");
    c.send(b"\r");
    c.wait_until("the fifth answer", answers(5));
    assert!(!bodies.lock().unwrap().last().unwrap().contains("(A tool of the MCP server `helper`.)"), "no longer offered");
}

#[test]
fn an_mcp_tool_that_is_allowed_is_used_without_asking_and_a_broken_server_says_why() {
    let (sb, bodies) = agent_sandbox("ask");
    let (helper, _) = mcp_server(&sb, "helper", "allow = [\"ech*\"]");
    let (broken, _) = mcp_server(&sb, "broken", "");
    let broken = broken.replace("MCP_NAME = \"broken\"", "MCP_NAME = \"broken\", MCP_BROKEN = \"1\"");
    let (hidden, _) = mcp_server(&sb, "narrow", "tools = [\"echo\"]");
    let config = std::fs::read_to_string(sb.root.join("cfg/config.toml")).unwrap();
    std::fs::write(sb.root.join("cfg/config.toml"), format!("{config}\n{helper}\n{broken}\n{hidden}")).unwrap();
    let c = sb.attach("main", 130, 40);
    c.wait_for("ready$");
    c.send(CTRL_SPACE);
    c.wait_for("AGENT");
    ask_for_tool(&c, "helper__echo", serde_json::json!({"words": "unasked"}));
    c.wait_until("the answer", |c| bodies.lock().unwrap().len() == 2 && c.text().contains("Ask anything"));
    assert!(!c.text().contains("Use echo from helper?"));
    assert_eq!(tool_result_in(bodies.lock().unwrap().last().unwrap()), "echo: unasked");
    {
        let sent = bodies.lock().unwrap();
        // Only what `tools` names of `narrow` is offered; nothing of a server that failed.
        assert!(sent[0].contains("\"name\":\"narrow__echo\"") && !sent[0].contains("\"name\":\"narrow__boom\"") && !sent[0].contains("broken__"), "{}", sent[0]);
    }
    c.send(b"/mcp\r");
    c.wait_for("broken · failed:");
    assert!(flowed(&c).contains("no configuration found"), "what the program said is shown:\n{}", c.text());
    c.send(b"/mcp log broken\r");
    c.wait_for("broken: starting");
    c.send(b"/mcp tools narrow\r");
    c.wait_for("(not offered: not in tools)");
}

#[test]
fn mcp_servers_can_be_turned_off_and_on_from_the_settings_with_the_mouse() {
    let (sb, _) = agent_sandbox("ask");
    let (helper, log) = mcp_server(&sb, "helper", "");
    let config = std::fs::read_to_string(sb.root.join("cfg/config.toml")).unwrap();
    std::fs::write(sb.root.join("cfg/config.toml"), format!("{config}\n{helper}")).unwrap();
    let c = sb.attach("main", 120, 40);
    c.wait_for("ready$");
    // Nothing is started until something needs it.
    std::thread::sleep(Duration::from_millis(800));
    assert!(!log.exists(), "started before anything asked for it");
    let (col, row) = c.position_of("≡ menu").unwrap();
    c.click(col + 2, row);
    c.wait_for("Settings");
    let (col, row) = c.position_of("Settings").unwrap();
    c.click(col + 2, row);
    c.wait_for("MCP servers");
    // (Looking at the settings does not start them.)
    c.wait_for("1 not started");
    let (col, row) = c.position_of("MCP servers").unwrap();
    c.click(col + 2, row);
    c.wait_for("Enter turns it off for this session");
    c.wait_until("the program to be started", |_| log.with_extension("log.env").exists());
    let (col, row) = c.position_of("helper").unwrap();
    c.click(col + 1, row);
    c.wait_for("off for this session");
    let (col, row) = c.position_of("≡ menu").unwrap();
    c.click(col + 2, row);
    c.wait_for("Choose a theme");
    let (col, row) = c.position_of("Settings").unwrap();
    c.click(col + 2, row);
    c.wait_for("1 off");
}

/// Ask `/mcp` until it says `text` (a server being started says
/// "starting" until it is up).
fn mcp_until(c: &Client, text: &str) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        c.send(b"/mcp\r");
        std::thread::sleep(Duration::from_millis(400));
        if flowed(c).contains(text) {
            return;
        }
    }
    panic!("`/mcp` never said `{text}`.\n--- screen ---\n{}", c.text());
}

/// A stand-in for an MCP server on the network that wants its callers
/// signed in, with its own authorization server beside it: discovery,
/// registration, the browser's visit, and handing out and renewing tokens.
struct FarMcp {
    port: u16,
    seen: Arc<Mutex<Vec<String>>>,
}

/// One HTTP request, as the stand-ins read it: the request line, the
/// headers (names in lower case), and the body.
fn read_request(stream: &mut std::net::TcpStream) -> Option<(String, Vec<(String, String)>, String)> {
    let mut data = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = stream.read(&mut buf).ok()?;
        if n == 0 {
            return None;
        }
        data.extend_from_slice(&buf[..n]);
        if let Some(pos) = data.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&data[..pos]).to_string();
            let mut lines = head.lines();
            let first = lines.next()?.to_string();
            let headers: Vec<(String, String)> = lines.filter_map(|l| l.split_once(':').map(|(k, v)| (k.trim().to_lowercase(), v.trim().to_string()))).collect();
            let length: usize = headers.iter().find(|(k, _)| k == "content-length").and_then(|(_, v)| v.parse().ok()).unwrap_or(0);
            while data.len() < pos + 4 + length {
                let n = stream.read(&mut buf).ok()?;
                if n == 0 {
                    break;
                }
                data.extend_from_slice(&buf[..n]);
            }
            return Some((first, headers, String::from_utf8_lossy(&data[pos + 4..]).to_string()));
        }
    }
}

fn respond(stream: &mut std::net::TcpStream, status: &str, headers: &[(&str, String)], body: &str) {
    let mut out = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n", body.len());
    for (k, v) in headers {
        out.push_str(&format!("{k}: {v}\r\n"));
    }
    out.push_str("\r\n");
    out.push_str(body);
    let _ = stream.write_all(out.as_bytes());
}

fn query_param(target: &str, key: &str) -> Option<String> {
    let q = target.split_once('?')?.1;
    q.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == key).then(|| {
            let v = v.replace('+', " ");
            let mut out = Vec::new();
            let b = v.as_bytes();
            let mut i = 0;
            while i < b.len() {
                if b[i] == b'%' && i + 2 < b.len() + 1 && i + 2 <= b.len() - 1 + 1 {
                    if let Ok(n) = u8::from_str_radix(&v[i + 1..(i + 3).min(v.len())], 16) {
                        out.push(n);
                        i += 3;
                        continue;
                    }
                }
                out.push(b[i]);
                i += 1;
            }
            String::from_utf8_lossy(&out).to_string()
        })
    })
}

impl FarMcp {
    /// `token`: what a caller must bring, as a plain token (`Some`), or by
    /// signing in (`None`).
    fn start(token: Option<&'static str>) -> FarMcp {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen: Arc<Mutex<Vec<String>>> = Arc::default();
        let log = seen.clone();
        std::thread::spawn(move || {
            let base = format!("http://127.0.0.1:{port}");
            let mut challenge = String::new();
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let Some((first, headers, body)) = read_request(&mut stream) else { continue };
                let header = |name: &str| headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone()).unwrap_or_default();
                let target = first.split_whitespace().nth(1).unwrap_or("").to_string();
                log.lock().unwrap().push(format!("{first} auth={} session={} protocol={} {body}", header("authorization"), header("mcp-session-id"), header("mcp-protocol-version")));
                let path = target.split('?').next().unwrap_or("").to_string();
                let json = |s: &mut std::net::TcpStream, v: serde_json::Value| respond(s, "200 OK", &[("Content-Type", "application/json".into())], &v.to_string());
                match (first.split_whitespace().next().unwrap_or(""), path.as_str()) {
                    ("GET", "/.well-known/oauth-protected-resource/mcp") => json(&mut stream, serde_json::json!({"resource": format!("{base}/mcp"), "authorization_servers": [base], "scopes_supported": ["tools"]})),
                    ("GET", "/.well-known/oauth-authorization-server") => json(&mut stream, serde_json::json!({
                        "issuer": base, "authorization_endpoint": format!("{base}/authorize"), "token_endpoint": format!("{base}/token"),
                        "registration_endpoint": format!("{base}/register"), "code_challenge_methods_supported": ["S256"]
                    })),
                    ("POST", "/register") => json(&mut stream, serde_json::json!({"client_id": "astrum-test-client"})),
                    // The browser, sent to sign in: it is let in at once, and sent back.
                    ("GET", "/authorize") => {
                        challenge = query_param(&target, "code_challenge").unwrap_or_default();
                        let back = format!("{}?code=code-1&state={}", query_param(&target, "redirect_uri").unwrap_or_default(), query_param(&target, "state").unwrap_or_default());
                        respond(&mut stream, "302 Found", &[("Location", back)], "");
                    }
                    ("POST", "/token") => {
                        let field = |k: &str| query_param(&format!("?{body}"), k).unwrap_or_default();
                        let verifier = field("code_verifier");
                        // The verifier is the one the challenge was made from (PKCE).
                        let fits = {
                            use base64::Engine;
                            let hash = aws_lc_rs::digest::digest(&aws_lc_rs::digest::SHA256, verifier.as_bytes());
                            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(hash.as_ref()) == challenge
                        };
                        if field("grant_type") == "refresh_token" && field("refresh_token") == "refresh-1" {
                            // (No new refresh token: the old one stays good.)
                            json(&mut stream, serde_json::json!({"access_token": "signed-in-token", "expires_in": 3600, "token_type": "Bearer"}));
                        } else if field("grant_type") == "authorization_code" && field("code") == "code-1" && fits {
                            json(&mut stream, serde_json::json!({"access_token": "signed-in-token", "refresh_token": "refresh-1", "expires_in": 3600, "token_type": "Bearer"}));
                        } else {
                            respond(&mut stream, "400 Bad Request", &[("Content-Type", "application/json".into())], r#"{"error":"invalid_grant"}"#);
                        }
                    }
                    ("DELETE", "/mcp") => respond(&mut stream, "200 OK", &[], ""),
                    ("POST", "/mcp") => {
                        let wanted = format!("Bearer {}", token.unwrap_or("signed-in-token"));
                        if header("authorization") != wanted {
                            let asks = format!("Bearer resource_metadata=\"{base}/.well-known/oauth-protected-resource/mcp\", scope=\"tools\"");
                            respond(&mut stream, "401 Unauthorized", &[("WWW-Authenticate", asks)], "");
                            continue;
                        }
                        let msg: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
                        let id = msg["id"].clone();
                        let method = msg["method"].as_str().unwrap_or("");
                        let result = match method {
                            "initialize" => serde_json::json!({"protocolVersion": "2025-06-18", "capabilities": {"tools": {}}, "serverInfo": {"name": "far-mcp", "version": "2.0"}}),
                            "tools/list" => serde_json::json!({"tools": [{"name": "echo", "description": "Say it back, from far away.", "inputSchema": {"type": "object", "properties": {"words": {"type": "string"}}}}]}),
                            "tools/call" => serde_json::json!({"content": [{"type": "text", "text": format!("far echo: {}", msg["params"]["arguments"]["words"].as_str().unwrap_or(""))}]}),
                            _ => {
                                // A notification, or an answer: taken.
                                respond(&mut stream, "202 Accepted", &[], "");
                                continue;
                            }
                        };
                        let answer = serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result});
                        if method == "initialize" {
                            respond(&mut stream, "200 OK", &[("Content-Type", "application/json".into()), ("Mcp-Session-Id", "session-77".into())], &answer.to_string());
                        } else {
                            // As a stream of events, with something else first.
                            let events = format!(": warming up\n\nevent: message\ndata: {}\n\ndata: {}\n\n", serde_json::json!({"jsonrpc": "2.0", "method": "notifications/message", "params": {"level": "info", "data": "working"}}), answer);
                            respond(&mut stream, "200 OK", &[("Content-Type", "text/event-stream".into())], &events);
                        }
                    }
                    _ => respond(&mut stream, "404 Not Found", &[], ""),
                }
            }
        });
        FarMcp { port, seen }
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}/mcp", self.port)
    }

    fn seen(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }
}

#[test]
fn an_mcp_server_on_the_network_is_signed_in_to_and_used() {
    use std::os::unix::fs::PermissionsExt;
    let (sb, bodies) = agent_sandbox("ask");
    let far = FarMcp::start(None);
    let config = std::fs::read_to_string(sb.root.join("cfg/config.toml")).unwrap();
    std::fs::write(sb.root.join("cfg/config.toml"), format!("{config}\n[mcp.remote]\nurl = \"{}\"\n", far.url())).unwrap();
    // The browser: it follows where it is sent, as a browser would.
    let browser = sb.root.join("browser");
    std::fs::write(&browser, format!("#!/bin/sh\necho \"$1\" >> '{}'\ncurl -s -L -o /dev/null \"$1\"\n", sb.root.join("browser.log").display())).unwrap();
    std::fs::set_permissions(&browser, std::fs::Permissions::from_mode(0o755)).unwrap();
    // (The panel below the shell, where its lines are not run into the divider.)
    let c = sb.attach_env("main", 120, 40, &[("ASTRUM_OPEN_TOOL", &browser.display().to_string())]);
    c.wait_for("ready$");
    c.send(CTRL_SPACE);
    c.wait_for("AGENT");
    let says = |text: &'static str| move |c: &Client| flowed(c).contains(text);
    c.send(b"/mcp\r");
    c.wait_until("the server to ask for a sign-in", says("asks to be signed in to: /mcp signin remote"));
    c.send(b"/mcp\r");
    c.wait_until("the list to say so", says("remote · asks to be signed in to: /mcp signin remote"));
    c.send(b"/mcp signin remote\r");
    c.wait_until("the sign-in", says("Signed in to remote"));
    mcp_until(&c, "remote · ready · 1 tool, far-mcp 2.0");
    let visited = std::fs::read_to_string(sb.root.join("browser.log")).unwrap();
    assert!(visited.contains("/authorize?response_type=code&client_id=astrum-test-client") && visited.contains("code_challenge_method=S256") && visited.contains("scope=tools"), "{visited}");
    assert!(visited.contains(&format!("resource={}", far.url().replace(':', "%3A").replace('/', "%2F"))), "{visited}");
    // The sign-in is kept in the password store, with what renews it.
    let kept = std::fs::read_to_string(sb.root.join("keychain.json")).unwrap();
    assert!(kept.contains("mcp.remote") && kept.contains("signed-in-token") && kept.contains("refresh-1"), "{kept}");

    ask_for_tool(&c, "remote__echo", serde_json::json!({"words": "over the network"}));
    c.wait_for("Use echo from remote?");
    c.send(b"y");
    c.wait_until("the answer", |c| bodies.lock().unwrap().len() == 2 && c.text().contains("Ask anything"));
    assert_eq!(tool_result_in(bodies.lock().unwrap().last().unwrap()), "far echo: over the network");
    // The session it was given, and the version agreed on, went with what followed.
    let seen = far.seen();
    let call = seen.iter().find(|l| l.contains("tools/call")).unwrap();
    assert!(call.contains("auth=Bearer signed-in-token") && call.contains("session=session-77") && call.contains("protocol=2025-06-18"), "{call}");
    // The token is never the model's to see.
    assert!(bodies.lock().unwrap().iter().all(|b| !b.contains("signed-in-token") && !b.contains("refresh-1")));

    // A sign-in that has run out is renewed before the server is reached again.
    let file = sb.root.join("keychain.json");
    let mut keys: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    let mut grant: serde_json::Value = serde_json::from_str(keys["mcp.remote"].as_str().unwrap()).unwrap();
    grant["expires"] = serde_json::json!(1);
    keys["mcp.remote"] = serde_json::json!(grant.to_string());
    std::fs::write(&file, keys.to_string()).unwrap();
    c.send(b"/mcp restart remote\r");
    c.wait_until("the sign-in to be renewed", |_| far.seen().iter().any(|l| l.contains("grant_type=refresh_token&refresh_token=refresh-1")));
    c.wait_until("the server to be reached again", |_| far.seen().iter().filter(|l| l.contains("tools/list")).count() == 2);
    c.send(b"/clear\r");
    mcp_until(&c, "remote · ready · 1 tool");
    let kept = std::fs::read_to_string(&file).unwrap();
    assert!(kept.contains("refresh-1") && !kept.contains("\\\"expires\\\":1,"), "renewed and kept, with what renews it: {kept}");
}

#[test]
fn a_token_for_an_mcp_server_is_typed_hidden_and_kept() {
    let (sb, bodies) = agent_sandbox("ask");
    let far = FarMcp::start(Some("static-123"));
    let config = std::fs::read_to_string(sb.root.join("cfg/config.toml")).unwrap();
    std::fs::write(sb.root.join("cfg/config.toml"), format!("{config}\n[mcp.keyed]\nurl = \"{}\"\n", far.url())).unwrap();
    let c = sb.attach("main", 120, 40);
    c.wait_for("ready$");
    c.send(CTRL_SPACE);
    c.wait_for("AGENT");
    // Without the token it is turned away.
    c.send(b"/mcp\r");
    c.wait_until("it to be turned away", |c| flowed(c).contains("`keyed` asks to be signed in to"));
    c.send(b"/mcp key keyed\r");
    c.wait_for("Type or paste the token for keyed");
    c.send(b"static-123\r");
    c.wait_for("The token for keyed is stored");
    assert!(!c.text().contains("static-123"), "the token is not shown:\n{}", c.text());
    mcp_until(&c, "keyed · ready · 1 tool");
    ask_for_tool(&c, "keyed__echo", serde_json::json!({"words": "with a token"}));
    c.wait_for("Use echo from keyed?");
    c.send(b"y");
    c.wait_until("the answer", |c| bodies.lock().unwrap().len() == 2 && c.text().contains("Ask anything"));
    assert_eq!(tool_result_in(bodies.lock().unwrap().last().unwrap()), "far echo: with a token");
    assert!(bodies.lock().unwrap().iter().all(|b| !b.contains("static-123")));
}
