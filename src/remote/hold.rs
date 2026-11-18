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

use super::{Answer, Ask, Down, Frame, LINK, Machine, Up, WORD, pack, policy, ports, read_frame_async, screen};
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
use std::sync::{Arc, Mutex};
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
/// A connection that has said nothing for this long has stopped.
const GONE_QUIET: Duration = Duration::from_secs(45);

/// The least time between two looks at a shell's git repository.
const GIT_EVERY: Duration = Duration::from_secs(2);
/// How long before git is asked again, when it was too slow to say what
/// has changed.
const GIT_AGAIN: Duration = Duration::from_secs(5);
/// With more than this waiting to be sent to a connection, the ports
/// carried for it read no more from their sockets: what its shells write
/// is not left far behind a download.
const PORTS_WAIT: usize = 256 * 1024;
/// How long a port here is given to answer.
const DIAL: Duration = Duration::from_secs(10);
/// A screen is sent in pieces of this size, each well within a frame.
const SCREEN_PIECE: usize = 1024 * 1024;

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
    said: (Option<String>, Option<Job>, Option<crate::git::Info>),
    /// The git repository its folder is in, as last looked up (away from
    /// the keeper's own work: `git status` can take a while), and for which
    /// folder, after how many commands, and when, that was asked.
    git: Arc<Mutex<(u64, Option<crate::git::Info>)>>,
    git_asked: Option<(Option<String>, u64, Instant)>,
    /// How many times it has been asked again because git gave the branch
    /// and not what has changed (it took too long, on a busy machine).
    git_again: u8,
}

#[derive(Clone)]
struct Conn {
    owner: Option<String>,
    tx: mpsc::UnboundedSender<Vec<u8>>,
    /// Bytes handed over that have not been written to the connection yet.
    waiting: Arc<AtomicUsize>,
    /// When anything last came from it. (The other side asks every ten
    /// seconds whether this one is still there.)
    heard: Instant,
    /// It wants the git branch of each shell's folder.
    git: bool,
    /// The connections to ports here that are carried for it.
    carried: ports::Streams,
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

    /// Connect to a port of this machine itself, or of a machine it can
    /// reach (`host`), and carry the connection: if whoever runs this
    /// machine allows it.
    fn dial(&self, id: u64, host: Option<String>, port: u16) {
        if let Some(why) = policy::refusal(policy::forward(), host.is_some()) {
            return self.say(Down::Drop { conn: id, why: Some(why) });
        }
        let to = self.clone();
        let say: ports::Say = Arc::new(move |said| match said {
            ports::Said::Bytes(conn, bytes) => to.send(&Frame::Conn(conn, bytes)),
            ports::Said::Took(conn, n) => to.say(Down::Took { conn, n }),
            ports::Said::End(conn) => to.say(Down::End { conn }),
            ports::Said::Drop(conn, why) => to.say(Down::Drop { conn, why }),
        });
        let told = self.clone();
        let socket = async move {
            match host {
                None => connect(port).await,
                Some(host) => {
                    let socket = connect_to(&host, port).await?;
                    // (Said before a byte of it: the one who asked may be
                    // waiting to tell someone else that it is there.)
                    told.say(Down::Reached { conn: id });
                    Ok(socket)
                }
            }
        };
        self.carried.carry(id, socket, say, Some((self.waiting.clone(), PORTS_WAIT)));
    }
}

/// A connection to a port of this machine itself, by whichever of its
/// two addresses something is listening on.
async fn connect(port: u16) -> std::io::Result<tokio::net::TcpStream> {
    use std::net::{Ipv4Addr, Ipv6Addr};
    use tokio::net::TcpStream;
    let slow = || std::io::Error::new(std::io::ErrorKind::TimedOut, "the port did not answer");
    match tokio::time::timeout(DIAL, TcpStream::connect((Ipv4Addr::LOCALHOST, port))).await.unwrap_or_else(|_| Err(slow())) {
        Ok(socket) => Ok(socket),
        Err(first) => tokio::time::timeout(DIAL, TcpStream::connect((Ipv6Addr::LOCALHOST, port))).await.unwrap_or_else(|_| Err(slow())).map_err(|_| first),
    }
}

/// A connection to a port of a machine this one can reach, which is
/// found by its name here: the names this machine knows are the point.
async fn connect_to(host: &str, port: u16) -> std::io::Result<tokio::net::TcpStream> {
    use std::io::{Error, ErrorKind};
    use tokio::net::TcpStream;
    let named = !host.is_empty() && host.len() <= 253 && !host.chars().any(|c| c.is_whitespace() || c.is_control());
    if !named {
        return Err(Error::new(ErrorKind::InvalidInput, "that is not the name of a machine"));
    }
    // (An address with colons of its own comes in brackets.)
    let bare = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')).unwrap_or(host);
    let found: Vec<std::net::SocketAddr> = match tokio::time::timeout(DIAL, tokio::net::lookup_host((bare, port))).await {
        Ok(Ok(found)) => found.collect(),
        Ok(Err(_)) => return Err(Error::new(ErrorKind::NotFound, format!("the name {host} is not known there"))),
        Err(_) => return Err(Error::new(ErrorKind::TimedOut, format!("looking up {host} took too long there"))),
    };
    let mut last = Error::new(ErrorKind::NotFound, format!("the name {host} has no address there"));
    // (The first few it goes by: one that answers is enough.)
    for addr in found.into_iter().take(4) {
        match tokio::time::timeout(DIAL, TcpStream::connect(addr)).await {
            Ok(Ok(socket)) => return Ok(socket),
            Ok(Err(e)) => last = e,
            Err(_) => last = Error::new(ErrorKind::TimedOut, "it did not answer"),
        }
    }
    Err(last)
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
    // (A stand-in for another machine, in the tests and in the website's
    // pictures, is on this one and has no name of its own: it is given one.)
    let host = std::env::var("ASTRUM_HOLD_HOST").ok().filter(|h| !h.is_empty()).unwrap_or(host);
    let shell = env("SHELL");
    Machine {
        user: env("USER"),
        host,
        os: crate::agent::run::os_description(),
        home: env("HOME"),
        shell: shell.rsplit('/').next().unwrap_or("sh").to_string(),
        outlasts_logout: outlasts_logout(),
        mark: super::mark(),
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
            run: None,
        };
        match Pane::spawn(opts, self.pty_tx.clone()) {
            Ok(mut pane) => {
                pane.set_image_support(Caps::default(), size.2);
                // The one showing it answers what programs ask of the terminal.
                pane.mute = true;
                conn.say(Down::Shell { key, nonce: pane.nonce().to_string(), shell: pane.shell_name.clone(), cwd: pane.cwd.clone(), at: 0, screen: false, replay: 0, at_prompt: None });
                self.kept.insert(key, Kept { pane, owner, last: VecDeque::new(), total: 0, conn: Some(from), sent: 0, behind: false, said: (None, None, None), git: Arc::default(), git_asked: None, git_again: 0 });
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
        k.said = (None, None, None);
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
            for piece in screen::of(&mut k.pane.term, title.as_deref()).chunks(SCREEN_PIECE) {
                conn.send(&Frame::Screen(key, piece.to_vec()));
            }
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
            // Nobody is seeing this as it is written: what a program
            // asks of the terminal meanwhile is answered here.
            k.pane.mute = false;
            return;
        }
        conn.send(&Frame::Bytes(key, data));
        k.sent = k.total;
    }

    /// Now and then: bring connections that fell behind up to date, and
    /// say what each shell is doing where that has changed.
    fn look_round(&mut self) {
        // A connection nothing has come from for a long while has
        // stopped, whether or not anything has closed it: its shells are
        // nobody's for now.
        let silent: Vec<u64> = self.conns.iter().filter(|(_, c)| c.heard.elapsed() > GONE_QUIET).map(|(id, _)| *id).collect();
        for id in silent {
            self.left(id);
        }
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
            // The repository is looked up again when the folder changes, or
            // a command has finished there (it may have changed something),
            // if the other side shows it at all. Only the latest lookup's
            // answer is kept: an earlier one, slower, may come after it.
            let asked = (k.pane.cwd.clone(), k.pane.block_seq);
            let changed = k.git_asked.as_ref().is_none_or(|(cwd, seq, at)| (cwd.clone(), *seq) != asked && at.elapsed() >= GIT_EVERY);
            // (And a few times more where git was too slow to say what has
            // changed: the first time it is run on a machine can be.)
            let slow = k.git.lock().unwrap_or_else(|e| e.into_inner()).1.as_ref().is_some_and(|info| info.status.is_none());
            let again = !changed && slow && k.git_again < 4 && k.git_asked.as_ref().is_some_and(|(_, _, at)| at.elapsed() >= GIT_AGAIN);
            if changed {
                k.git_again = 0;
            } else if again {
                k.git_again += 1;
            }
            let due = conn.git && (changed || again);
            if let (true, Some(cwd)) = (due, asked.0.clone()) {
                k.git_asked = Some((asked.0.clone(), asked.1, Instant::now()));
                let found = k.git.clone();
                let ticket = {
                    let mut slot = found.lock().unwrap_or_else(|e| e.into_inner());
                    slot.0 += 1;
                    slot.0
                };
                // (A thread that cannot be had is a lookup that waits for the next time.)
                let _ = std::thread::Builder::new().name("git".into()).spawn(move || {
                    let info = crate::git::look(std::path::Path::new(&cwd), true);
                    let mut slot = found.lock().unwrap_or_else(|e| e.into_inner());
                    if slot.0 == ticket {
                        slot.1 = info;
                    }
                });
            }
            let git = if conn.git { k.git.lock().unwrap_or_else(|e| e.into_inner()).1.clone() } else { None };
            let now = (k.pane.cwd.clone(), k.pane.job(), git);
            if now != k.said {
                conn.say(Down::Doing { key: *key, cwd: now.0.clone(), job: now.1.clone(), git: now.2.clone() });
                k.said = now;
            }
        }
    }

    fn came(&mut self, from: u64, frame: Frame<Up>) {
        if let Some(conn) = self.conns.get_mut(&from) {
            conn.heard = Instant::now();
        }
        let Some(conn) = self.conns.get(&from).cloned() else { return };
        let mine = |keeper: &Keeper, key: u64| keeper.kept.get(&key).is_some_and(|k| Some(&k.owner) == conn.owner.as_ref());
        match frame {
            Frame::Bytes(key, bytes) if mine(self, key) => {
                if let Some(k) = self.kept.get(&key) {
                    k.pane.write(bytes);
                }
            }
            Frame::Bytes(..) | Frame::Screen(..) => {}
            Frame::Conn(id, bytes) => conn.carried.bytes(id, bytes),
            // (Only for someone who has said who they are, as with
            // everything else; and only to this machine itself.)
            Frame::Msg(Up::Dial { conn: id, port }) => match conn.owner {
                Some(_) => conn.dial(id, None, port),
                None => conn.say(Down::Drop { conn: id, why: None }),
            },
            // (To a machine this one can reach, if that is allowed here.)
            Frame::Msg(Up::Reach { conn: id, host, port }) => match conn.owner {
                Some(_) => conn.dial(id, Some(host), port),
                None => conn.say(Down::Drop { conn: id, why: None }),
            },
            Frame::Msg(Up::Took { conn: id, n }) => conn.carried.took(id, n),
            Frame::Msg(Up::End { conn: id }) => conn.carried.end(id),
            Frame::Msg(Up::Drop { conn: id }) => conn.carried.dropped(id),
            Frame::Msg(Up::Hello { owner, git, .. }) => {
                let shells = self.kept.iter().filter(|(_, k)| k.owner == owner).map(|(key, _)| *key).collect();
                if let Some(conn) = self.conns.get_mut(&from) {
                    conn.owner = Some(owner);
                    conn.git = git;
                }
                conn.say(Down::Hello { link: LINK, version: env!("CARGO_PKG_VERSION").into(), there: machine(), shells, ports: true, reach: true, forward: policy::forward().name().into(), files: true });
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
        if let Some(conn) = self.conns.remove(&id) {
            conn.carried.clear();
        }
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
/// Work on files, off the threads that pass the shells' bytes along.
async fn blocking<T: Send + 'static>(work: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, String> {
    tokio::task::spawn_blocking(work).await.unwrap_or_else(|e| Err(e.to_string()))
}

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
        Ask::Have { sum, name, size } => match blocking(move || super::files::have(&sum, &name, size)).await {
            // (Not there is an answer too: one with nothing in it.)
            Ok(found) => text((found.unwrap_or_default(), false)),
            Err(why) => text((why, true)),
        },
        Ask::Piece { sum, put, size, at, bytes } => {
            use base64::Engine;
            // (For the tests, which have no slow line to send over.)
            match tuned("ASTRUM_HOLD_SLOW_PIECE_MS", 0) {
                0 => {}
                ms => tokio::time::sleep(Duration::from_millis(ms as u64)).await,
            }
            let done = blocking(move || {
                let bytes = base64::engine::general_purpose::STANDARD.decode(bytes).map_err(|_| "That piece could not be read.".to_string())?;
                super::files::piece(&sum, &put, size, at, &bytes)
            });
            match done.await {
                Ok(()) => text((String::new(), false)),
                Err(why) => text((why, true)),
            }
        }
        Ask::Keep { sum, put, name, size } => match blocking(move || super::files::keep(&sum, &put, &name, size)).await {
            Ok(path) => text((path, false)),
            Err(why) => text((why, true)),
        },
        Ask::Forget { sum, put } => {
            super::files::forget(&sum, &put);
            text((String::new(), false))
        }
        Ask::Listening { port, host } => {
            if let Some(why) = policy::refusal(policy::forward(), host.is_some()) {
                return text((why, true));
            }
            let tried = match &host {
                Some(host) => connect_to(host, port).await,
                None => connect(port).await,
            };
            match tried {
                Ok(_) => text((String::new(), false)),
                Err(e) => text((e.to_string(), true)),
            }
        }
    }
}

/// Keep shells until there are none left to keep. (`astrum __hold`.)
pub async fn hold() -> Result<()> {
    let sock = socket()?;
    // One keeper at a time, settled by a lock that is held for as long as
    // this one lives: two started in the same moment would otherwise each
    // take the other's socket away.
    let lock = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(sock.with_extension("lock"))?;
    {
        use std::os::fd::AsRawFd;
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            // One is running already, or about to be.
            return Ok(());
        }
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
        loop {
            // (A refusal now, with too many files open say, is not the end
            // of listening.)
            let Ok((stream, _)) = listener.accept().await else {
                tokio::time::sleep(Duration::from_millis(200)).await;
                continue;
            };
            let id = next;
            next += 1;
            let (mut read, mut write) = stream.into_split();
            let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
            let waiting = Arc::new(AtomicUsize::new(0));
            let conn = Conn { owner: None, tx, waiting: waiting.clone(), heard: Instant::now(), git: false, carried: ports::Streams::default() };
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
    drop(lock);
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
