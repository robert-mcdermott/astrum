//! The other machine's side: the program that keeps the shells
//! (`astrum __hold`), and the one `ssh` runs to talk to it (`astrum
//! __link`).
//!
//! The keeper is started by the first link that finds none, as a program
//! of its own that the connection closing does not take with it, and
//! stops by itself a while after its last shell has ended. It keeps, for
//! each shell, a terminal of its own (so the screen can be shown to
//! someone who was not there to see it drawn) and the last few megabytes
//! the shell wrote (so a short absence is made up without a gap).

use super::{Answer, Ask, Down, Frame, LINK, Machine, Up, WORD, pack, read_frame_async, screen};
use crate::agent::tools;
use crate::image::Caps;
use crate::pane::{Pane, ShellState, SpawnOpts};
use crate::paths;
use crate::procinfo::Job;
use crate::server::Ev;
use anyhow::{Context, Result};
use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;
use tokio::net::UnixListener;
use tokio::sync::mpsc;

/// How much of what each shell wrote last is kept, to send to a
/// connection that was away.
const KEPT: usize = 4 * 1024 * 1024;
/// A connection with more than this waiting to be sent is not keeping up
/// (or has stopped): the shells stop sending to it, and go on by
/// themselves, until most of it has gone.
const TOO_FAR_BEHIND: usize = 8 * 1024 * 1024;
const CAUGHT_UP: usize = 1024 * 1024;
/// How long the keeper stays once it has no shells and nobody connected.
const LINGER: Duration = Duration::from_secs(20);

/// One of the sizes above, or what a test has set in its place. (A test
/// cannot write megabytes, or wait that long for a keeper to be gone.)
fn tuned(name: &str, usual: usize) -> usize {
    if cfg!(debug_assertions) {
        if let Some(set) = std::env::var(name).ok().and_then(|v| v.parse().ok()) {
            return set;
        }
    }
    usual
}

fn linger() -> Duration {
    Duration::from_millis(tuned("ASTRUM_HOLD_LINGER_MS", LINGER.as_millis() as usize) as u64)
}

fn kept() -> usize {
    tuned("ASTRUM_HOLD_KEPT", KEPT)
}

fn too_far_behind() -> usize {
    tuned("ASTRUM_HOLD_BEHIND", TOO_FAR_BEHIND)
}

fn caught_up() -> usize {
    too_far_behind() / (TOO_FAR_BEHIND / CAUGHT_UP)
}

/// Where the keeper listens. (Not a name a session can have: those end
/// in `.sock`. The number is in it so that two versions that do not speak
/// the same language each have their own.)
pub fn socket() -> Result<PathBuf> {
    Ok(paths::runtime_dir()?.join(format!("hold-{LINK}.link")))
}

fn log_path() -> Result<PathBuf> {
    Ok(paths::runtime_dir()?.join("hold.log"))
}

struct Kept {
    pane: Pane,
    owner: String,
    /// The last of what the shell wrote, and how much it has written in all.
    last: VecDeque<u8>,
    total: u64,
    /// The connection showing it, and how much has been sent there.
    conn: Option<u64>,
    sent: u64,
    /// That connection is not keeping up; nothing is being sent to it.
    behind: bool,
    /// What was last said of what the shell is doing.
    said: (Option<String>, Option<Job>),
}

#[derive(Clone)]
struct Conn {
    owner: Option<String>,
    tx: mpsc::UnboundedSender<Vec<u8>>,
    /// Bytes handed over that have not been written to the connection yet.
    waiting: Arc<AtomicUsize>,
}

impl Conn {
    fn send(&self, frame: &Frame<Down>) {
        let bytes = pack(frame);
        self.waiting.fetch_add(bytes.len(), Ordering::Relaxed);
        let _ = self.tx.send(bytes);
    }
    fn say(&self, msg: Down) {
        self.send(&Frame::Msg(msg));
    }
}

enum Came {
    Joined(u64, Conn),
    Frame(u64, Frame<Up>),
    Left(u64),
}

struct Keeper {
    kept: HashMap<u64, Kept>,
    conns: HashMap<u64, Conn>,
    shell_dir: PathBuf,
    pty_tx: mpsc::Sender<Ev>,
}

/// The machine, as it describes itself.
fn machine() -> Machine {
    let env = |name: &str| std::env::var(name).unwrap_or_default();
    let mut host = [0u8; 256];
    let named = unsafe { libc::gethostname(host.as_mut_ptr().cast(), host.len() - 1) } == 0;
    let host = if named { String::from_utf8_lossy(&host[..host.iter().position(|b| *b == 0).unwrap_or(0)]).into_owned() } else { String::new() };
    let shell = env("SHELL");
    Machine {
        user: env("USER"),
        host,
        os: crate::agent::run::os_description(),
        home: env("HOME"),
        shell: shell.rsplit('/').next().unwrap_or("sh").to_string(),
        outlasts_logout: outlasts_logout(),
    }
}

/// Does the system leave a user's programs running when they have logged
/// out everywhere? `None`: nothing says.
fn outlasts_logout() -> Option<bool> {
    if !cfg!(target_os = "linux") {
        return Some(true);
    }
    if crate::service::lingering() == Some(true) {
        return Some(true);
    }
    // Without lingering it is the login manager's choice, and its default
    // is to leave them.
    let mut kills = false;
    let mut files = vec![PathBuf::from("/etc/systemd/logind.conf")];
    for dir in ["/usr/lib/systemd/logind.conf.d", "/etc/systemd/logind.conf.d"] {
        let mut more: Vec<PathBuf> = std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "conf")).collect();
        more.sort();
        files.extend(more);
    }
    for file in files {
        for line in std::fs::read_to_string(file).unwrap_or_default().lines() {
            if let Some(value) = line.trim().strip_prefix("KillUserProcesses=") {
                kills = matches!(value.trim().to_ascii_lowercase().as_str(), "yes" | "true" | "1" | "on");
            }
        }
    }
    Some(!kills)
}

impl Keeper {
    fn conn_of(&self, key: u64) -> Option<&Conn> {
        self.kept.get(&key).and_then(|k| k.conn).and_then(|id| self.conns.get(&id))
    }

    fn open(&mut self, from: u64, key: u64, size: (u16, u16, (u16, u16)), cwd: Option<String>, session: &str, scrollback: usize) {
        let Some(conn) = self.conns.get(&from).cloned() else { return };
        let Some(owner) = conn.owner.clone() else { return };
        if self.kept.contains_key(&key) {
            return conn.say(Down::Gone { key, why: Some("that name is taken".into()) });
        }
        let session: String = session.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').take(40).collect();
        let opts = SpawnOpts {
            id: key,
            cols: size.0,
            rows: size.1,
            cwd: cwd.as_deref(),
            shell: None,
            shell_dir: &self.shell_dir,
            session: if session.is_empty() { "default" } else { &session },
            scrollback: scrollback.clamp(100, 100_000),
        };
        match Pane::spawn(opts, self.pty_tx.clone()) {
            Ok(mut pane) => {
                pane.set_image_support(Caps::default(), size.2);
                // The one showing it answers what programs ask of the terminal.
                pane.mute = true;
                conn.say(Down::Shell { key, nonce: pane.nonce().to_string(), shell: pane.shell_name.clone(), cwd: pane.cwd.clone(), at: 0, screen: false, replay: 0, at_prompt: None });
                self.kept.insert(key, Kept { pane, owner, last: VecDeque::new(), total: 0, conn: Some(from), sent: 0, behind: false, said: (None, None) });
            }
            Err(e) => conn.say(Down::Gone { key, why: Some(format!("{e:#}")) }),
        }
    }

    /// Show a shell to a connection that has (`had`) so much of what it wrote.
    fn show(&mut self, from: u64, key: u64, had: Option<u64>) {
        let Some(conn) = self.conns.get(&from).cloned() else { return };
        let Some(k) = self.kept.get_mut(&key).filter(|k| Some(&k.owner) == conn.owner.as_ref()) else {
            return conn.say(Down::Gone { key, why: None });
        };
        k.conn = Some(from);
        k.behind = false;
        k.pane.mute = true;
        k.said = (None, None);
        let (nonce, shell, cwd) = (k.pane.nonce().to_string(), k.pane.shell_name.clone(), k.pane.cwd.clone());
        // From where it left off, if everything since is still kept: then
        // nothing is missing, and nothing has to be drawn again. (For
        // someone who has nothing, that is everything from the start.)
        let from_here = had.unwrap_or(0);
        if from_here <= k.total && k.total - from_here <= k.last.len() as u64 {
            let missed = (k.total - from_here) as usize;
            conn.say(Down::Shell { key, nonce, shell, cwd, at: from_here, screen: false, replay: missed as u64, at_prompt: None });
            let start = k.last.len() - missed;
            let bytes: Vec<u8> = k.last.range(start..).copied().collect();
            for piece in bytes.chunks(64 * 1024) {
                conn.send(&Frame::Bytes(key, piece.to_vec()));
            }
        } else {
            let at_prompt = k.pane.integrated.then(|| matches!(k.pane.state, ShellState::Prompt));
            conn.say(Down::Shell { key, nonce, shell, cwd, at: k.total, screen: true, replay: 0, at_prompt });
            let title = k.pane.osc_title.clone();
            conn.send(&Frame::Screen(key, screen::of(&mut k.pane.term, title.as_deref())));
        }
        k.sent = k.total;
    }

    fn wrote(&mut self, key: u64, data: Vec<u8>) {
        let Some(k) = self.kept.get_mut(&key) else { return };
        k.last.extend(&data);
        let over = k.last.len().saturating_sub(kept());
        k.last.drain(..over);
        k.total += data.len() as u64;
        k.pane.feed_plain(&data, &mut Vec::new());
        let Some(conn) = k.conn.and_then(|id| self.conns.get(&id)) else { return };
        if k.behind {
            return;
        }
        if conn.waiting.load(Ordering::Relaxed) > too_far_behind() {
            k.behind = true;
            return;
        }
        conn.send(&Frame::Bytes(key, data));
        k.sent = k.total;
    }

    /// Now and then: bring connections that fell behind up to date, and
    /// say what each shell is doing where that has changed.
    fn look_round(&mut self) {
        let behind: Vec<(u64, u64, u64)> = self.kept.iter().filter(|(_, k)| k.behind).filter_map(|(key, k)| Some((*key, k.conn?, k.sent))).collect();
        for (key, conn, sent) in behind {
            if self.conns.get(&conn).is_some_and(|c| c.waiting.load(Ordering::Relaxed) < caught_up()) {
                self.show(conn, key, Some(sent));
            }
        }
        for (key, k) in self.kept.iter_mut() {
            let Some(conn) = k.conn.and_then(|id| self.conns.get(&id)) else { continue };
            if k.behind {
                continue;
            }
            if k.pane.cwd_stale {
                k.pane.refresh_cwd();
            }
            let now = (k.pane.cwd.clone(), k.pane.job());
            if now != k.said {
                conn.say(Down::Doing { key: *key, cwd: now.0.clone(), job: now.1.clone() });
                k.said = now;
            }
        }
    }

    fn came(&mut self, from: u64, frame: Frame<Up>) {
        let Some(conn) = self.conns.get(&from).cloned() else { return };
        let mine = |keeper: &Keeper, key: u64| keeper.kept.get(&key).is_some_and(|k| Some(&k.owner) == conn.owner.as_ref());
        match frame {
            Frame::Bytes(key, bytes) if mine(self, key) => {
                if let Some(k) = self.kept.get(&key) {
                    k.pane.write(bytes);
                }
            }
            Frame::Bytes(..) | Frame::Screen(..) => {}
            Frame::Msg(Up::Hello { owner, .. }) => {
                let shells = self.kept.iter().filter(|(_, k)| k.owner == owner).map(|(key, _)| *key).collect();
                if let Some(conn) = self.conns.get_mut(&from) {
                    conn.owner = Some(owner);
                }
                conn.say(Down::Hello { link: LINK, version: env!("CARGO_PKG_VERSION").into(), there: machine(), shells });
            }
            Frame::Msg(Up::Open { key, cols, rows, px, cwd, session, scrollback }) => self.open(from, key, (cols, rows, px), cwd, &session, scrollback),
            Frame::Msg(Up::Attach { key, had, cols, rows, px }) => {
                if mine(self, key) {
                    if let Some(k) = self.kept.get_mut(&key) {
                        k.pane.set_image_support(Caps::default(), px);
                        k.pane.resize(cols, rows);
                    }
                }
                self.show(from, key, had);
            }
            Frame::Msg(Up::Resize { key, cols, rows, px }) if mine(self, key) => {
                if let Some(k) = self.kept.get_mut(&key) {
                    k.pane.set_image_support(Caps::default(), px);
                    k.pane.resize(cols, rows);
                }
            }
            Frame::Msg(Up::Close { key }) if mine(self, key) => {
                self.kept.remove(&key);
            }
            Frame::Msg(Up::Resize { .. } | Up::Close { .. }) => {}
            Frame::Msg(Up::Ping) => conn.say(Down::Pong),
            Frame::Msg(Up::Ask { n, ask }) => {
                if conn.owner.is_some() {
                    tokio::spawn(async move {
                        let answer = carry_out(ask).await;
                        conn.say(Down::Answer { n, answer });
                    });
                }
            }
        }
    }

    fn left(&mut self, id: u64) {
        self.conns.remove(&id);
        for k in self.kept.values_mut().filter(|k| k.conn == Some(id)) {
            k.conn = None;
            k.behind = false;
            // Nobody is showing it: the keeper's own terminal answers
            // what programs ask, so that none waits for an answer.
            k.pane.mute = false;
        }
    }
}

/// Do a piece of the agent's work on this machine.
async fn carry_out(ask: Ask) -> Answer {
    let text = |(text, failed): (String, bool)| Answer::Text { text, failed };
    match ask {
        Ask::Tool { call, cwd } => text(tools::run_local(&call, &cwd).await),
        Ask::Plan { call, cwd } => match tools::plan_change(&call, &cwd).await {
            Ok(change) => Answer::Plan { existed: change.existed, path: change.path.to_string_lossy().into_owned(), old: change.old, new: change.new, done: change.done },
            Err(why) => text((why, true)),
        },
        Ask::Commit { path, old, content } => {
            let change = tools::Change { path: path.into(), existed: old.is_some(), old, new: String::new(), done: String::new() };
            match tools::commit_change(&change, &content).await {
                Ok(()) => text((String::new(), false)),
                Err(why) => text((why, true)),
            }
        }
        Ask::Run { command, cwd, timeout, note } => text(tools::run_subprocess(&command, &cwd, timeout.clamp(1, 3600), &note).await),
        Ask::Rules { cwd } => {
            let found = crate::agent::rules::load_project(&cwd).await;
            Answer::Rules { text: found.text, sources: found.sources }
        }
    }
}

/// Keep shells until there are none left to keep. (`astrum __hold`.)
pub async fn hold() -> Result<()> {
    let sock = socket()?;
    if tokio::net::UnixStream::connect(&sock).await.is_ok() {
        // One is running already.
        return Ok(());
    }
    let _ = std::fs::remove_file(&sock);
    let listener = UnixListener::bind(&sock).with_context(|| format!("binding {}", sock.display()))?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(0o600))?;
    }
    let shell_dir = crate::server::install_shell_files()?;
    let (pty_tx, mut pty_rx) = mpsc::channel::<Ev>(128);
    let (came_tx, mut came_rx) = mpsc::channel::<Came>(256);

    tokio::spawn(async move {
        let mut next = 1u64;
        while let Ok((stream, _)) = listener.accept().await {
            let id = next;
            next += 1;
            let (mut read, mut write) = stream.into_split();
            let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
            let waiting = Arc::new(AtomicUsize::new(0));
            let conn = Conn { owner: None, tx, waiting: waiting.clone() };
            if came_tx.send(Came::Joined(id, conn)).await.is_err() {
                break;
            }
            tokio::spawn(async move {
                while let Some(bytes) = rx.recv().await {
                    let sent = write.write_all(&bytes).await.is_ok();
                    waiting.fetch_sub(bytes.len(), Ordering::Relaxed);
                    if !sent {
                        break;
                    }
                }
            });
            let came_tx = came_tx.clone();
            tokio::spawn(async move {
                loop {
                    match read_frame_async::<Up>(&mut read).await {
                        Ok(Some(frame)) => {
                            if came_tx.send(Came::Frame(id, frame)).await.is_err() {
                                break;
                            }
                        }
                        Ok(None) => {}
                        Err(_) => break,
                    }
                }
                let _ = came_tx.send(Came::Left(id)).await;
            });
        }
    });

    let mut keeper = Keeper { kept: HashMap::new(), conns: HashMap::new(), shell_dir, pty_tx };
    let mut tick = tokio::time::interval(Duration::from_millis(500));
    let mut empty_since: Option<Instant> = Some(Instant::now());
    loop {
        tokio::select! {
            Some(ev) = pty_rx.recv() => match ev {
                Ev::Pty { pane, data } => keeper.wrote(pane, data),
                Ev::PtyExit { pane, .. } => {
                    if let Some(conn) = keeper.conn_of(pane) {
                        conn.say(Down::Exited { key: pane });
                    }
                    keeper.kept.remove(&pane);
                }
                _ => {}
            },
            Some(came) = came_rx.recv() => match came {
                Came::Joined(id, conn) => {
                    keeper.conns.insert(id, conn);
                }
                Came::Frame(id, frame) => keeper.came(id, frame),
                Came::Left(id) => keeper.left(id),
            },
            _ = tick.tick() => {
                keeper.look_round();
                if keeper.kept.is_empty() && keeper.conns.is_empty() {
                    if empty_since.get_or_insert_with(Instant::now).elapsed() > linger() {
                        break;
                    }
                } else {
                    empty_since = None;
                }
            }
        }
    }
    let _ = std::fs::remove_file(&sock);
    Ok(())
}

/// Start the keeper, as a program the connection closing will not end.
fn start_keeper() -> Result<()> {
    let exe = std::env::current_exe().context("locating the astrum executable")?;
    // Where the user's programs are kept running after they log out
    // (lingering, which the login service of `astrum service` also relies
    // on), the user's own service manager runs it: a program merely left
    // behind by a login could still be ended with it.
    if cfg!(target_os = "linux") && crate::service::lingering() == Some(true) {
        let mut args = vec!["--user".to_string(), "--quiet".into(), "--collect".into(), format!("--unit=astrum-hold-{LINK}-{}", std::process::id())];
        for name in crate::service::KEEP_ENV.iter().chain(&["HOME", "USER", "LOGNAME", "PATH", "TERM"]) {
            if let Some(value) = std::env::var(name).ok().filter(|v| !v.is_empty()) {
                args.push(format!("--setenv={name}={value}"));
            }
        }
        args.extend(["--".to_string(), exe.to_string_lossy().into_owned(), "__hold".into()]);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        if crate::service::run("systemd-run", &args).is_ok() {
            return Ok(());
        }
    }
    let log = std::fs::OpenOptions::new().create(true).append(true).open(log_path()?)?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("__hold").stdin(std::process::Stdio::null()).stdout(log.try_clone()?).stderr(log);
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    cmd.spawn().context("starting the program that keeps the shells")?;
    Ok(())
}

/// Join whoever ran this (`ssh`, from the user's own machine) to the
/// keeper, starting it if need be: what comes in is passed to it, and
/// what it says is passed back. (`astrum __link`.)
pub fn link() -> Result<()> {
    use std::os::fd::FromRawFd;
    let sock = socket()?;
    let mut started = false;
    let began = Instant::now();
    let stream = loop {
        match std::os::unix::net::UnixStream::connect(&sock) {
            Ok(stream) => break stream,
            Err(e) if began.elapsed() > Duration::from_secs(8) => return Err(e).context("the program that keeps the shells did not start"),
            Err(_) => {
                if !std::mem::replace(&mut started, true) {
                    start_keeper()?;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    };
    // (Not `std::io::stdout`, which holds a line back until it is whole.)
    let mut out = std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(1) });
    out.write_all(format!("{WORD} ready\n").as_bytes())?;
    let mut up = stream.try_clone()?;
    std::thread::spawn(move || {
        let mut input = std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(0) });
        let _ = std::io::copy(&mut *input, &mut up);
        // The connection has closed. The shells stay.
        std::process::exit(0);
    });
    let mut stream = stream;
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if out.write_all(&buf[..n]).is_err() {
                    break;
                }
            }
        }
    }
    Ok(())
}
