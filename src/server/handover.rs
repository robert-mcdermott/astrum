//! Moving a running session onto a new Astrum without stopping anything.
//!
//! The server replaces itself with the new program (`exec`: the same
//! process, so every shell stays its child and never notices). Before it
//! does, it writes down everything it holds, in a file only its owner can
//! read: the tabs and how each is laid out, and for each pane its shell's
//! terminal (left open across the change, by number), what the terminal
//! shows, and what shell integration knows. The new program builds each
//! pane around the terminal it was handed, instead of starting a shell.
//!
//! Nothing is lost on the way: reading from the shells is held first (what
//! a program writes meanwhile waits in its terminal), terminals attached
//! are told to come back to the session's socket (which stays open, so
//! they wait on it until the new server answers), and the new program is
//! asked whether it can read what was written before anything is given up.
//! If any step fails, the session carries on as it was.

use super::panes::{Node, Panes};
use super::session::AgentSession;
use super::{Ev, Focus, Server, Tab};
use crate::pane::{HANDING_OVER, Handed, Pane};
use crate::proto::{ServerFrame, ServerMsg};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use tokio::net::UnixListener;
use tokio::sync::mpsc;

/// The number of the form the state is written in. A program that reads
/// another number does not take the session.
pub const FORMAT: u32 = 1;

/// How long after reading is held before the state is written: time for
/// what was already being read to arrive, and for terminals to be told.
const SETTLE: Duration = Duration::from_millis(250);

#[derive(Serialize, Deserialize, Debug)]
pub struct State {
    pub format: u32,
    /// The version that wrote it.
    pub from: String,
    /// The session's socket, still listening, by number.
    pub listener: i32,
    pub active: usize,
    pub next_id: u64,
    /// How many times the session had been moved before this.
    #[serde(default)]
    pub moves: u32,
    pub tabs: Vec<TabState>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct TabState {
    pub id: u64,
    #[serde(default)]
    pub name: Option<String>,
    pub layout: Node,
    pub focus: u64,
    /// The pane the agent works in.
    pub agent: u64,
    #[serde(default)]
    pub zoomed: bool,
    #[serde(default)]
    pub conversation: Option<String>,
    #[serde(default)]
    pub panel: bool,
    pub panes: Vec<PaneState>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct PaneState {
    pub id: u64,
    /// A shell on this machine, handed over.
    #[serde(default)]
    pub here: Option<Handed>,
    /// Or one on another machine, which its keeper there has: the machine,
    /// and the name the shell goes by.
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub key: Option<u64>,
    #[serde(default)]
    pub cwd: Option<String>,
}

/// What a server keeps for handing over.
#[derive(Default)]
pub struct Handing {
    /// When the state is to be written and the program replaced, and by which.
    pub due: Option<Instant>,
    pub exe: Option<PathBuf>,
    /// The offer to move onto a newer Astrum was turned down.
    pub declined: bool,
    /// The session's socket, and the task taking connections from it.
    pub listener: Option<Arc<UnixListener>>,
    pub accepting: Option<tokio::task::JoinHandle<()>>,
    pub in_tx: Option<mpsc::Sender<Ev>>,
    /// Started by a service manager (said again to the next program).
    pub service: bool,
    /// How many times the session has been moved onto a new Astrum.
    pub moves: u32,
}

/// A version as numbers, for telling a newer from an older.
fn numbers(version: &str) -> Vec<u64> {
    version.split(['.', '-', '+']).map_while(|part| part.parse().ok()).collect()
}

pub fn newer(this: &str, than: &str) -> bool {
    numbers(this) > numbers(than)
}

/// What `astrum __takes` prints: that this program can take a session
/// over, in which form it reads the state, and its version.
pub fn takes_line() -> String {
    format!("astrum-takes {FORMAT} {}", installed_version())
}

/// The version this program says it is to a session that might move onto
/// it. (A test can have it say another, to stand for a newer one installed.)
pub fn installed_version() -> String {
    if cfg!(debug_assertions) {
        if let Ok(version) = std::env::var("ASTRUM_TEST_INSTALLED_VERSION") {
            return version;
        }
    }
    env!("CARGO_PKG_VERSION").to_string()
}

/// `astrum __takes [FILE]`: say what this program takes, or read a
/// state file through and say whether it could be taken.
pub fn takes(file: Option<&str>) -> anyhow::Result<()> {
    match file {
        None => println!("{}", takes_line()),
        Some(file) => {
            read(Path::new(file))?;
            println!("ok");
        }
    }
    Ok(())
}

pub fn read(file: &Path) -> anyhow::Result<State> {
    let state: State = serde_json::from_slice(&std::fs::read(file)?)?;
    if state.format != FORMAT {
        anyhow::bail!("the state is in form {}, and this Astrum reads form {FORMAT}", state.format);
    }
    Ok(state)
}

/// Run a program briefly and give what it printed, if it ended well in time.
fn ask(exe: &Path, args: &[&str]) -> Option<String> {
    use std::process::{Command, Stdio};
    let mut child = Command::new(exe).args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) | Err(_) => return None,
            Ok(None) if Instant::now() >= until => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    let mut out = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take()?, &mut out).ok()?;
    Some(out.trim().to_string())
}

fn cloexec(fd: i32, on: bool) {
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFD);
        libc::fcntl(fd, libc::F_SETFD, if on { flags | libc::FD_CLOEXEC } else { flags & !libc::FD_CLOEXEC });
    }
}

/// The socket a server before this one left listening, taken up again.
pub fn listener_from(fd: i32) -> anyhow::Result<UnixListener> {
    use std::os::fd::FromRawFd;
    let listener = unsafe { std::os::unix::net::UnixListener::from_raw_fd(fd) };
    cloexec(fd, true);
    listener.set_nonblocking(true)?;
    Ok(UnixListener::from_std(listener)?)
}

impl Server {
    /// The Astrum a terminal attached here runs, if it is newer than this
    /// server: the session could be moved onto it.
    pub fn newer_installed(&self) -> Option<(String, String)> {
        if self.handover.declined || self.handover.due.is_some() {
            return None;
        }
        let lead = self.clients.iter().find(|c| Some(c.id) == self.lead && c.attached)?;
        let (version, exe) = (lead.astrum.clone()?, lead.exe.clone()?);
        newer(&version, env!("CARGO_PKG_VERSION")).then_some((version, exe))
    }

    /// Asked to move this session onto the Astrum at `exe`. Says why not,
    /// if not; otherwise the move is under way, and happens in a moment.
    pub(super) fn upgrade_begin(&mut self, exe: &str, force: bool) -> Result<String, String> {
        if self.handover.due.is_some() {
            return Err("This session is already moving onto a new Astrum".into());
        }
        let exe = PathBuf::from(exe);
        let said = ask(&exe, &["__takes"]).unwrap_or_default();
        let mut words = said.split_whitespace();
        let (Some("astrum-takes"), Some(format), Some(version)) = (words.next(), words.next(), words.next()) else {
            return Err(format!("{} is not an Astrum that can take a running session over (that needs 0.8.0 or later)", exe.display()));
        };
        if format.parse() != Ok(FORMAT) {
            return Err(format!("Astrum {version} reads a session's state in another form than this one ({}) writes it: stop the session to move it (`astrum kill`, then `astrum`)", env!("CARGO_PKG_VERSION")));
        }
        if version == env!("CARGO_PKG_VERSION") && !force {
            return Err(format!("This session already runs Astrum {version}"));
        }
        // An agent in the middle of something would be cut off.
        if let Some(idx) = self.tabs.iter().position(|t| t.agent.busy || t.agent.detached || t.agent.approval.is_some()) {
            return Err(format!("The agent in tab {} is at work: move the session when it has finished, or stop it first (Esc)", idx + 1));
        }
        // From here nothing more is read from the shells, and no terminal
        // is taken on: those that come wait at the socket for the next server.
        HANDING_OVER.store(true, Ordering::SeqCst);
        if let Some(accepting) = self.handover.accepting.take() {
            accepting.abort();
        }
        let session = self.session.clone();
        for c in self.clients.iter().filter(|c| c.attached) {
            let msg = if c.switches {
                ServerMsg::Switch { session: session.clone(), cwd: None }
            } else {
                ServerMsg::Bye { reason: format!("this session is moving onto Astrum {version}, with everything in it still running; run `astrum` to come back") }
            };
            let _ = c.tx.send(ServerFrame::Msg(msg));
        }
        self.handover.exe = Some(exe);
        self.handover.due = Some(Instant::now() + SETTLE);
        Ok(version.to_string())
    }

    pub fn handover_due(&self) -> bool {
        self.handover.due.is_some_and(|due| Instant::now() >= due)
    }

    /// Write everything down and become the new program. Returns only if
    /// that could not be done, with the session as it was.
    pub fn hand_over(&mut self) {
        self.handover.due = None;
        let Some(exe) = self.handover.exe.take() else { return };
        let why = self.hand_over_to(&exe).unwrap_err();
        // Still here: carry on as before.
        for pane in self.tabs.iter().flat_map(|t| t.panes.iter()) {
            pane.keep_after_all();
        }
        if let Some(listener) = &self.handover.listener {
            use std::os::fd::AsRawFd;
            cloexec(listener.as_raw_fd(), true);
        }
        HANDING_OVER.store(false, Ordering::SeqCst);
        if let (Some(listener), Some(in_tx)) = (self.handover.listener.clone(), self.handover.in_tx.clone()) {
            self.handover.accepting = Some(super::accept(listener, in_tx));
        }
        eprintln!("astrum: the session could not be moved onto {}: {why:#}", exe.display());
        self.toast(format!("The session could not be moved onto the new Astrum, and carries on as it was: {why:#}"));
    }

    fn hand_over_to(&mut self, exe: &Path) -> anyhow::Result<std::convert::Infallible> {
        use std::io::Write;
        use std::os::fd::AsRawFd;
        use std::os::unix::fs::OpenOptionsExt;
        use std::os::unix::process::CommandExt;
        let listener = self.handover.listener.clone().ok_or_else(|| anyhow::anyhow!("the session's socket is not this server's to hand over"))?;
        // What is on disk is as good as it can be, should the next
        // program fail after all: the session then comes back from it.
        for idx in 0..self.tabs.len() {
            self.save_conversation(idx);
        }
        self.save_session();
        let tabs = self
            .tabs
            .iter_mut()
            .map(|tab| TabState {
                id: tab.id,
                name: tab.name.clone(),
                layout: tab.panes.layout().clone(),
                focus: tab.panes.focus,
                agent: tab.panes.agent,
                zoomed: tab.panes.zoomed,
                conversation: tab.agent.saved.as_ref().map(|s| s.id.clone()),
                panel: tab.agent.visible,
                panes: tab
                    .panes
                    .iter_mut()
                    .map(|p| PaneState { id: p.id, host: p.there().map(|t| t.host.clone()), key: p.there().map(|t| t.key), cwd: p.cwd.clone(), here: p.hand_over() })
                    .collect(),
            })
            .collect();
        let state = State { format: FORMAT, from: env!("CARGO_PKG_VERSION").into(), listener: listener.as_raw_fd(), active: self.active, next_id: self.next_id, moves: self.handover.moves, tabs };
        let file = crate::paths::runtime_dir()?.join(format!("{}.handover", self.session));
        let _ = std::fs::remove_file(&file);
        let written = (|| {
            let mut out = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&file)?;
            out.write_all(&serde_json::to_vec(&state)?)?;
            out.sync_all()?;
            anyhow::Ok(())
        })();
        let ready = written.and_then(|()| match ask(exe, &["__takes", &file.to_string_lossy()]).as_deref() {
            Some("ok") => Ok(()),
            _ => Err(anyhow::anyhow!("the new Astrum could not read this session's state")),
        });
        if let Err(why) = ready {
            let _ = std::fs::remove_file(&file);
            return Err(why);
        }
        // Past here there is no going back that is worth more than going on.
        self.keep_far_shells();
        self.mcp_close_all();
        cloexec(listener.as_raw_fd(), false);
        let mut next = std::process::Command::new(exe);
        next.arg("__server").arg("--session").arg(&self.session);
        if self.handover.service {
            next.arg("--service");
        }
        next.arg("--take").arg(&file);
        let failed = next.exec();
        let _ = std::fs::remove_file(&file);
        Err(anyhow::Error::from(failed).context("starting the new Astrum"))
    }

    /// In the new program: build the session from what the one before
    /// wrote down, around the shells it left running.
    pub fn take(&mut self, state: State) {
        let scrollback = self.cfg.scrollback;
        self.next_id = state.next_id;
        self.handover.moves = state.moves + 1;
        let mut conversations = Vec::new();
        for tab in state.tabs {
            let mut ids = HashMap::new();
            let mut list = Vec::new();
            for was in tab.panes {
                let pane = match (was.here, was.host, was.key) {
                    (Some(handed), ..) => match Pane::taken(was.id, scrollback, handed, self.tx.clone()) {
                        Ok(pane) => pane,
                        Err(why) => {
                            eprintln!("astrum: a shell could not be taken over: {why:#}");
                            continue;
                        }
                    },
                    // A shell on another machine is its keeper's: found again there.
                    (None, Some(host), Some(key)) => self.held_again(&host, key, 80, 24, was.cwd),
                    _ => continue,
                };
                ids.insert(was.id, pane.id);
                list.push(pane);
            }
            if list.is_empty() {
                continue;
            }
            let agent_pane = ids.get(&tab.agent).copied();
            let mut panes = Panes::restored(list, &tab.layout, &ids, tab.focus, tab.zoomed);
            if let Some(id) = agent_pane {
                panes.agent = id;
            }
            let mut agent = AgentSession::new();
            agent.visible = tab.panel;
            self.tabs.push(Tab { id: tab.id, panes, name: tab.name, agent, focus: Focus::Shell, activity: false, owner: None, sized: (0, 0) });
            if let Some(id) = tab.conversation {
                conversations.push((self.tabs.len() - 1, id, tab.panel));
            }
        }
        self.active = state.active.min(self.tabs.len().saturating_sub(1));
        for (idx, id, panel) in conversations {
            if super::store::load(&id).is_ok() && self.load_conversation(idx, &id) {
                self.tabs[idx].agent.visible = panel;
            }
            self.tabs[idx].focus = Focus::Shell;
        }
        // A full-screen program draws its screen again, as after a resize.
        for pane in self.tabs.iter().flat_map(|t| t.panes.iter()) {
            pane.ask_redraw();
        }
        self.toast(format!("This session now runs Astrum {} (it ran {}). Everything in it kept running.", env!("CARGO_PKG_VERSION"), state.from));
        self.schedule_save();
        self.dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_newer_version_is_told_from_an_older() {
        assert!(newer("0.8.1", "0.8.0") && newer("0.10.0", "0.9.3") && newer("1.0.0", "0.99.9"));
        assert!(!newer("0.8.0", "0.8.0") && !newer("0.7.9", "0.8.0") && !newer("0.8.0-dev", "0.8.0"));
    }
}
