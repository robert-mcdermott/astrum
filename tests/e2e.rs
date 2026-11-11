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
        Sandbox { root, runtime }
    }

    fn env(&self) -> Vec<(&'static str, String)> {
        vec![
            ("HOME", self.root.join("home").to_string_lossy().into_owned()),
            ("ASTRUM_CONFIG_DIR", self.root.join("cfg").to_string_lossy().into_owned()),
            ("ASTRUM_RUNTIME_DIR", self.runtime.to_string_lossy().into_owned()),
            // Stored keys go to a file here, never to the developer's real keychain.
            ("ASTRUM_KEYCHAIN_FILE", self.root.join("keychain.json").to_string_lossy().into_owned()),
            // What a test copies stays off the developer's real clipboard.
            ("ASTRUM_CLIPBOARD_TOOL", "none".to_string()),
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
        cmd.envs(extra.iter().copied());
        cmd.args(args).env_remove("ASTRUM").env_remove("XDG_CONFIG_HOME").env_remove("XDG_DATA_HOME");
        for (k, v) in self.env() {
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
        let pair = native_pty_system().openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 }).unwrap();
        let mut cmd = CommandBuilder::new(BIN);
        cmd.arg("-s");
        cmd.arg(session);
        // Start from a plain 256-color terminal, whatever runs the tests.
        for var in ["ASTRUM", "XDG_CONFIG_HOME", "XDG_DATA_HOME", "ZDOTDIR", "PROMPT_COMMAND", "COLORTERM", "TERM_PROGRAM"] {
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
        std::thread::spawn(move || {
            let mut buf = [0u8; 16384];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                let chunk = &buf[..n];
                // Answer the device-attributes query like a terminal would,
                // so the client's capability probe returns immediately.
                if chunk.windows(3).any(|w| w == b"\x1b[c") {
                    let _ = answer.lock().unwrap().write_all(b"\x1b[?62;c");
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
        self.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 }).unwrap();
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

const CTRL_B: &[u8] = b"\x02";
const CTRL_SPACE: &[u8] = b"\x00";

#[test]
fn shell_tabs_mouse_and_detach() {
    let sb = Sandbox::new();
    let mut c = sb.attach("main", 90, 24);
    c.wait_for("ready$");
    c.send(b"echo e2e-$((20+22))\r");
    c.wait_for("e2e-42");
    assert!(c.lines()[0].contains(" 1 "), "tab bar on the first row: {:?}", c.lines()[0]);
    assert!(c.lines()[23].contains("SHELL"), "status bar on the last row: {:?}", c.lines()[23]);

    // Prefix + c opens a second tab with a fresh shell.
    c.send(CTRL_B);
    c.wait_for("PREFIX");
    c.send(b"c");
    c.wait_until("a second tab", |c| c.lines()[0].contains(" 2 "));
    c.wait_until("the new tab's empty screen", |c| !c.text().contains("e2e-42"));
    c.wait_for("ready$");

    // Clicking the "+" opens a third; clicking tab 1 goes back to the first.
    let plus = c.find_on_row(0, " + ").expect("a + button") + 1;
    c.click(plus, 0);
    c.wait_until("a third tab", |c| c.lines()[0].contains(" 3 "));
    let tab1 = c.find_on_row(0, " 1 ").expect("tab 1") + 1;
    c.click(tab1, 0);
    c.wait_for("e2e-42");

    // Prefix + d detaches: the client exits, the session stays.
    c.send(CTRL_B);
    c.send(b"d");
    c.wait_exit();
    assert!(c.raw().contains("detached"), "goodbye line: {:?}", c.raw().chars().rev().take(120).collect::<String>());
    let listing = sb.run(&["ls"]);
    assert!(listing.contains("main") && listing.contains("3 tabs") && listing.contains("detached"), "{listing}");

    // Reattach: same tabs, same screen contents.
    let mut c = sb.attach("main", 90, 24);
    c.wait_for("e2e-42");
    assert!(c.lines()[0].contains(" 3 "));

    // Closing every shell ends the session.
    for _ in 0..3 {
        c.wait_for("ready$");
        c.send(b"exit\r");
        std::thread::sleep(Duration::from_millis(400));
    }
    c.wait_exit();
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
    let mut c = sb.attach("main", 221, 24);
    c.wait_for("ready$");

    // The hint line outlasts a slow reader, and each entry is a button.
    c.send(CTRL_B);
    c.wait_for("PREFIX");
    let hints = c.lines()[23].clone();
    for entry in ["c new tab", "| split right", "- split down", "z zoom", "x close", ", rename", "d detach", "? help", "1-9 tab · arrows pane"] {
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
    for _ in 0..3 {
        c.wait_for("ready$");
        c.send(b"exit\r");
        std::thread::sleep(Duration::from_millis(500));
    }
    c.wait_exit();
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

/// The number printed after `label` on screen, e.g. a process id.
fn number_after(c: &Client, label: &str) -> i32 {
    let text = c.text();
    let at = text.rfind(label).unwrap_or_else(|| panic!("no {label} in:\n{text}"));
    text[at + label.len()..].chars().take_while(|ch| ch.is_ascii_digit()).collect::<String>().parse().unwrap()
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
        std::fs::read_dir(&dir).is_ok_and(|d| d.flatten().any(|e| std::fs::read_to_string(e.path()).is_ok_and(|t| t.contains("before-the-crash-42"))))
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
    c.wait_until("the status bar at full width", |c| c.lines().len() == 30 && c.lines()[29].contains("SHELL"));
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
    c.wait_for("command not found");
    assert_eq!(bodies.lock().unwrap().len(), 2);
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
    assert!(!c.text().contains("command not found"), "{}", c.text());
    assert!(bodies.lock().unwrap()[0].contains("how do I list files here"));
    // Commands are still commands, and a leading space forces a line to the shell.
    c.wait_until("a prompt after the agent's command", |c| c.lines().iter().rev().find(|l| l.contains("ready$")).is_some_and(|l| l.trim_end().ends_with("ready$")));
    c.send(b"echo still-a-shell-$((3+4))\r");
    c.wait_for("still-a-shell-7");
    c.wait_until("the next prompt", |c| c.lines().iter().rev().find(|l| l.contains("ready$")).is_some_and(|l| l.trim_end().ends_with("ready$")));
    c.send(b" how do I list files here\r");
    c.wait_for("command not found");
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
    // Told to wait two seconds for a command that takes far longer.
    ask_for_tool(&c, "run_command", serde_json::json!({"command": "sleep 30", "timeout_secs": 2}));
    c.wait_until("the report on the running command", |_| bodies.lock().unwrap().len() == 2);
    let report = tool_result_in(&bodies.lock().unwrap()[1]);
    assert!(report.contains("status: still running after 2s"), "{report}");
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
    // user is typing, and "y" is the key that would approve it.
    c.send(b"go SLOWLY\r");
    c.wait_until("the request", |_| bodies.lock().unwrap().len() == 1);
    std::thread::sleep(Duration::from_millis(900));
    for _ in 0..14 {
        c.send(b"y");
        std::thread::sleep(Duration::from_millis(50));
    }
    c.wait_for("Run this command?");
    std::thread::sleep(Duration::from_millis(300));
    assert!(!c.text().contains("agent-ran-42"), "typing must not approve the command: {}", c.text());
    assert!(c.text().contains("Run this command?"));
    // A deliberate answer, once the question has been up for a moment, counts.
    std::thread::sleep(Duration::from_millis(1300));
    c.send(b"y");
    c.wait_for("agent-ran-42");
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
fn colors_fall_back_to_the_256_palette_without_truecolor() {
    let sb = Sandbox::new();
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
fn the_view_follows_the_terminal_in_use() {
    let sb = Sandbox::new();
    let small = sb.attach("shared", 70, 16);
    small.wait_for("ready$");
    small.wait_until("the status bar on the small terminal's last row", |c| c.lines()[15].contains("SHELL"));

    // A second, larger terminal opens the same session: it is filled edge to
    // edge, and the shell is resized to it.
    let large = sb.attach("shared", 100, 30);
    large.wait_until("the status bar on the large terminal's last row", |c| c.lines()[29].contains("SHELL"));
    large.send(b"echo cols=$(tput cols)\r");
    large.wait_for("cols=100");
    // Meanwhile the small one shows the top-left part of that view.
    small.wait_until("a cropped view on the small terminal", |c| c.lines()[0].contains(" 1 ") && !c.lines()[15].contains("SHELL"));

    // Using the small terminal again brings the session back to its size.
    small.send(b" ");
    small.wait_until("the session to follow the small terminal", |c| c.lines()[15].contains("SHELL"));
    small.send(b"echo cols=$(tput cols)\r");
    small.wait_for("cols=70");
    large.wait_until("the large terminal to show the smaller view, padded", |c| {
        c.lines()[15].contains("SHELL") && c.lines()[29].trim().is_empty()
    });
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
