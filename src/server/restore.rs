//! Keeping a session across a restart.
//!
//! While a session runs, what it would take to bring it back is kept on
//! disk: its tabs, how each is divided into panes, where each shell is, and
//! (unless turned off) what each pane shows and remembers. A session that is
//! stopped, or that goes down with the machine, comes back from this the
//! next time it is started; one whose shells were all closed is finished
//! and leaves nothing behind.
//!
//! One directory per session under the data directory: `session.json` for
//! the layout and a text file per pane for its screen and scrollback.

use super::panes::{Node, Panes};
use super::session::AgentSession;
use super::{Focus, Server, Tab, store};
use crate::config::Restore;
use crate::paths;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const VERSION: u32 = 1;

/// How long after something happens before the session is written out.
const SAVE_AFTER: Duration = Duration::from_secs(15);

fn save_after() -> Duration {
    // Tests of what survives a crash cannot wait that long.
    if cfg!(debug_assertions) {
        if let Some(ms) = std::env::var("ASTRUM_SAVE_AFTER_MS").ok().and_then(|v| v.parse().ok()) {
            return Duration::from_millis(ms);
        }
    }
    SAVE_AFTER
}

/// After a shell has ended, nothing is written for this long: if the whole
/// machine is going down, the shells go one by one, and what was saved
/// before the first of them went is the session to bring back.
const HOLD_AFTER_KILL: Duration = Duration::from_secs(10);

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SavedSession {
    pub version: u32,
    /// When this was written, in seconds since the Unix epoch.
    pub saved: u64,
    pub active: usize,
    pub tabs: Vec<SavedTab>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SavedTab {
    #[serde(default)]
    pub name: Option<String>,
    pub layout: Node,
    pub focus: u64,
    #[serde(default)]
    pub zoomed: bool,
    pub panes: Vec<SavedPane>,
    /// The agent conversation the tab had open, and whether its panel showed.
    #[serde(default)]
    pub conversation: Option<String>,
    #[serde(default)]
    pub panel: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SavedPane {
    pub id: u64,
    #[serde(default)]
    pub cwd: Option<String>,
}

fn root() -> PathBuf {
    paths::data_dir().join("sessions")
}

pub fn dir(session: &str) -> PathBuf {
    root().join(session)
}

fn pane_file(session: &str, id: u64) -> PathBuf {
    dir(session).join(format!("pane-{id}.txt"))
}

/// Write a file that only its owner can read, replacing it in one step.
fn write_private(path: &PathBuf, bytes: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    if let Some(parent) = path.parent() {
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(parent)?;
    }
    let tmp = path.with_extension("tmp");
    let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
    file.write_all(bytes)?;
    // On disk before it takes the old one's place: a power cut must not
    // leave an empty file where there was a good one.
    file.sync_all()?;
    drop(file);
    std::fs::rename(&tmp, path)
}

pub fn load(session: &str) -> Option<SavedSession> {
    let text = std::fs::read_to_string(dir(session).join("session.json")).ok()?;
    let saved: SavedSession = serde_json::from_str(&text).ok()?;
    (saved.version == VERSION && !saved.tabs.is_empty()).then_some(saved)
}

pub fn pane_text(session: &str, id: u64) -> Option<String> {
    std::fs::read_to_string(pane_file(session, id)).ok().filter(|t| !t.is_empty())
}

/// Throw away what is kept of a session. True if there was something.
pub fn forget(session: &str) -> bool {
    let _ = std::fs::remove_dir_all(aside(session));
    let dir = dir(session);
    dir.is_dir() && std::fs::remove_dir_all(dir).is_ok()
}

/// Where a session's saved state waits while it is not yet certain that
/// the session is over (see `set_aside`). The name is not one a session
/// can have, so it is never restored or listed.
fn aside(session: &str) -> PathBuf {
    root().join(format!("{session}.ending"))
}

/// The last shell of a session has ended, which normally means the session
/// is over and nothing of it is kept. But when the whole machine is going
/// down, shells that end by themselves when told to (fish does) look just
/// the same, a moment before the server is told to stop too. So the saved
/// session is put out of the way at once, where a session starting afresh
/// will not find it, and only thrown away once that moment has passed.
pub fn set_aside(session: &str) {
    let _ = std::fs::remove_dir_all(aside(session));
    let _ = std::fs::rename(dir(session), aside(session));
}

/// The server was told to stop after all: the session was not closed, it
/// went down with the machine. Keep it.
pub fn put_back(session: &str) {
    if dir(session).exists() {
        let _ = std::fs::remove_dir_all(aside(session));
    } else {
        let _ = std::fs::rename(aside(session), dir(session));
    }
}

/// The session really is over.
pub fn discard_aside(session: &str) {
    let _ = std::fs::remove_dir_all(aside(session));
}

/// Every session that has something saved, by name.
pub fn list() -> Vec<(String, SavedSession)> {
    let mut all: Vec<(String, SavedSession)> = std::fs::read_dir(root())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            load(&name).map(|saved| (name, saved))
        })
        .collect();
    all.sort_by(|a, b| a.0.cmp(&b.0));
    all
}

/// Remove the pane files of panes that are no longer part of the session.
fn prune_panes(session: &str, keep: &[u64]) {
    for entry in std::fs::read_dir(dir(session)).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(id) = name.strip_prefix("pane-").and_then(|n| n.strip_suffix(".txt")).and_then(|n| n.parse::<u64>().ok()) else { continue };
        if !keep.contains(&id) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// A date and time as the clock on the wall shows it: `2026-11-11 10:42`.
pub fn wall_time(unix: u64) -> String {
    unsafe {
        let secs = unix as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&secs, &mut tm);
        format!("{:04}-{:02}-{:02} {:02}:{:02}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday, tm.tm_hour, tm.tm_min)
    }
}

/// What the server remembers about its own saving.
#[derive(Default)]
pub struct Saving {
    /// When the session is next to be written out, if anything has happened.
    pub due: Option<Instant>,
    /// The layout as it was last written, to write it only when it changes.
    layout: String,
    /// For each pane, the moment of the output its saved text goes up to.
    panes: HashMap<u64, Instant>,
    /// Nothing is written before this (see `HOLD_AFTER_KILL`).
    hold: Option<Instant>,
    /// The session ended because its last shell was closed: nothing of it
    /// is to be kept.
    pub finished: bool,
    /// The shell whose end is being handled right now was killed from
    /// outside, not closed.
    pub killed_now: bool,
}

impl Server {
    /// Something happened; write the session out before long.
    pub(super) fn schedule_save(&mut self) {
        if self.saving.due.is_none() && self.cfg.restore != Restore::Off {
            self.saving.due = Some(Instant::now() + save_after());
        }
    }

    /// A shell has ended, by itself or killed from outside.
    pub(super) fn hold_saves(&mut self) {
        self.saving.hold = Some(Instant::now() + HOLD_AFTER_KILL);
    }

    pub(super) fn saves_held(&self) -> bool {
        self.saving.hold.is_some_and(|until| Instant::now() < until)
    }

    fn snapshot(&self) -> SavedSession {
        let tabs = self
            .tabs
            .iter()
            .map(|tab| SavedTab {
                name: tab.name.clone(),
                layout: tab.panes.layout().clone(),
                focus: tab.panes.focus,
                zoomed: tab.panes.zoomed,
                panes: tab.panes.iter().map(|p| SavedPane { id: p.id, cwd: p.cwd.clone() }).collect(),
                conversation: tab.agent.saved.as_ref().map(|s| s.id.clone()),
                panel: tab.agent.visible,
            })
            .collect();
        SavedSession { version: VERSION, saved: store::now(), active: self.active, tabs }
    }

    /// Write out whatever has changed since the last time.
    pub(super) fn save_session(&mut self) {
        self.saving.due = None;
        if self.cfg.restore == Restore::Off || self.tabs.is_empty() {
            return;
        }
        if self.saves_held() {
            self.saving.due = self.saving.hold;
            return;
        }
        let session = self.session.clone();
        let snapshot = self.snapshot();
        // The time of saving aside, has the layout changed?
        let layout = serde_json::to_string(&SavedSession { saved: 0, ..snapshot.clone() }).unwrap_or_default();
        let mut failed = None;
        if layout != self.saving.layout {
            match serde_json::to_vec_pretty(&snapshot).map_err(std::io::Error::other).and_then(|bytes| write_private(&dir(&session).join("session.json"), &bytes)) {
                Ok(()) => self.saving.layout = layout,
                Err(e) => failed = Some(e),
            }
        }
        let keep: Vec<u64> = self.tabs.iter().flat_map(|t| t.panes.iter().map(|p| p.id)).collect();
        if self.cfg.restore == Restore::All {
            for pane in self.tabs.iter().flat_map(|t| t.panes.iter()) {
                if self.saving.panes.get(&pane.id) == Some(&pane.last_output) {
                    continue;
                }
                // (A full-screen program has the screen: keep what was saved before it.)
                let Some(text) = pane.dump() else { continue };
                match write_private(&pane_file(&session, pane.id), text.as_bytes()) {
                    Ok(()) => {
                        self.saving.panes.insert(pane.id, pane.last_output);
                    }
                    Err(e) => failed = Some(e),
                }
            }
            prune_panes(&session, &keep);
        } else {
            prune_panes(&session, &[]);
        }
        self.saving.panes.retain(|id, _| keep.contains(id));
        if let Some(e) = failed {
            eprintln!("astrum: could not save the session: {e}");
        }
    }

    /// The last thing a server does with its saved session, unless the
    /// session is over (see `set_aside`): write it one more time. Stopped
    /// on purpose (`astrum kill`), always; told to stop from outside while
    /// shells have just been ending, not: that is the machine going down,
    /// and what is on disk is from before it began.
    pub(super) fn save_at_exit(&mut self, on_purpose: bool) {
        if self.cfg.restore == Restore::Off {
            forget(&self.session);
        } else if on_purpose || !self.saves_held() {
            self.saving.hold = None;
            self.save_session();
        }
    }

    /// Has the session ended by its last shell being closed?
    pub(super) fn finished(&self) -> bool {
        self.saving.finished && self.cfg.restore != Restore::Off
    }

    /// Bring back the tabs of the session of this name that was running
    /// before, if there is one saved. False if there was nothing to restore.
    pub(super) fn restore_session(&mut self) -> bool {
        if self.cfg.restore == Restore::Off {
            forget(&self.session);
            return false;
        }
        // (What an earlier server set aside and never got to throw away.)
        discard_aside(&self.session);
        let Some(saved) = load(&self.session) else { return false };
        let (cols, rows) = self.pane_size(false);
        let note = format!("restored · as it was on {}", wall_time(saved.saved).replace(' ', " at "));
        let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
        let mut conversations = Vec::new();
        for tab in &saved.tabs {
            let mut ids = HashMap::new();
            let mut list = Vec::new();
            for was in &tab.panes {
                let Some(mut pane) = self.spawn_pane(Some(was.cwd.as_deref().unwrap_or(&home)), cols, rows) else { continue };
                if self.cfg.restore == Restore::All {
                    if let Some(text) = pane_text(&self.session, was.id) {
                        pane.preload(&text, &note);
                    }
                }
                ids.insert(was.id, pane.id);
                list.push(pane);
            }
            let Some(first) = list.first().map(|p| p.id) else { continue };
            let panes = Panes::restored(list, &tab.layout, &ids, tab.focus, tab.zoomed);
            let mut agent = AgentSession::new();
            agent.visible = tab.panel;
            self.tabs.push(Tab { id: first, panes, name: tab.name.clone(), agent, focus: Focus::Shell, activity: false, owner: None, sized: (0, 0) });
            if let Some(id) = &tab.conversation {
                conversations.push((self.tabs.len() - 1, id.clone(), tab.panel));
            }
        }
        if self.tabs.is_empty() {
            return false;
        }
        self.active = saved.active.min(self.tabs.len() - 1);
        // Each tab's conversation with the agent comes back with it.
        for (idx, id, panel) in conversations {
            if store::load(&id).is_ok() && self.load_conversation(idx, &id) {
                self.tabs[idx].agent.visible = panel;
            }
            // Wherever the keyboard was, a restored tab starts in its shell.
            self.tabs[idx].focus = Focus::Shell;
        }
        let n = self.tabs.len();
        self.toast(format!("Restored {n} tab{} from {}", if n == 1 { "" } else { "s" }, store::ago(saved.saved, store::now())));
        // The new panes have new numbers: write the session out under them.
        self.save_session();
        self.dirty = true;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_saved_session_reads_back_and_tolerates_older_files() {
        let saved = SavedSession {
            version: VERSION,
            saved: 1_700_000_000,
            active: 1,
            tabs: vec![SavedTab {
                name: Some("build".into()),
                layout: Node::Split { side_by_side: true, share: 400, a: Box::new(Node::Leaf(3)), b: Box::new(Node::Leaf(7)) },
                focus: 7,
                zoomed: false,
                panes: vec![SavedPane { id: 3, cwd: Some("/tmp".into()) }, SavedPane { id: 7, cwd: None }],
                conversation: Some("2026-11-11-abc".into()),
                panel: true,
            }],
        };
        let text = serde_json::to_string(&saved).unwrap();
        assert_eq!(serde_json::from_str::<SavedSession>(&text).unwrap(), saved);
        // Fields added later may be missing from a file written earlier.
        let old = r#"{"version":1,"saved":5,"active":0,"tabs":[{"layout":{"Leaf":1},"focus":1,"panes":[{"id":1}]}]}"#;
        let read: SavedSession = serde_json::from_str(old).unwrap();
        assert_eq!(read.tabs[0].panes[0].cwd, None);
        assert!(!read.tabs[0].panel && read.tabs[0].conversation.is_none());
    }

    #[test]
    fn wall_time_reads_as_a_date() {
        let text = wall_time(1_700_000_000);
        assert_eq!(text.len(), 16, "{text}");
        assert!(text.starts_with("2023-11-1"), "{text}");
    }
}
