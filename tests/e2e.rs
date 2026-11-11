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
        let mut cmd = Command::new(BIN);
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
            let (ctype, payload) = if head.starts_with("GET") {
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
                let summarizing = body.contains("handover summary");
                let chunk = if summarizing {
                    r#"{"choices":[{"index":0,"delta":{"role":"assistant","content":"Handover: the shell printed agent-ran-42, exit code: 0."},"finish_reason":null}]}"#.to_string()
                } else if first {
                    r#"{"choices":[{"index":0,"delta":{"role":"assistant","content":"","tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"run_command","arguments":"{\"command\":\"printf agent-ran-%d 42\",\"explanation\":\"Checking arithmetic.\"}"}}]},"finish_reason":null}]}"#
                        .to_string()
                } else if body.contains("agent-ran-42") && body.contains("exit code: 0") {
                    r#"{"choices":[{"index":0,"delta":{"role":"assistant","content":"Verdict: the shell printed **forty-two**."},"finish_reason":null}]}"#.to_string()
                } else {
                    r#"{"choices":[{"index":0,"delta":{"role":"assistant","content":"Verdict: no command output reached me."},"finish_reason":null}]}"#.to_string()
                };
                let finish = if first && !summarizing { "tool_calls" } else { "stop" };
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
    assert!(sb.run(&["kill", "-s", "main"]).contains("Stopped"));
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
    assert!(sb.run(&["kill", "-s", "main"]).contains("Stopped"));
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
    assert!(sb.run(&["kill", "-s", "main"]).contains("Stopped"));
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
    c.wait_until("the shell to be in the project", |c| c.lines()[29].contains("home/proj"));
    c.send(b"beta question\r");
    c.wait_for("Verdict: no command output reached me.");
    c.send(b"/clear\r");
    c.wait_until("an empty panel", |c| !c.text().contains("beta question"));

    // Back home, /resume picks the one from here, though the other is newer.
    c.send(b"!cd ~\r");
    c.wait_until("the shell to be back home", |c| !c.lines()[29].contains("home/proj"));
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
    c.wait_for("Compacted.");
    c.wait_for("Verdict: the shell printed forty-two.");
    let sent = bodies.lock().unwrap();
    assert_eq!(sent.len(), 3, "tool call, summary, continuation");
    assert!(sent[1].contains("handover summary"));
    let cont = messages_of(&sent[2]);
    assert_eq!(cont.len(), 2);
    let text = cont[1]["content"].as_str().unwrap();
    assert!(text.contains("Handover: the shell printed") && text.contains("Carry on from where the summary leaves off"), "{text}");
    assert!(!sent[2].contains("call_1"));
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
    c.wait_for("Compacted.");
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
