//! The git branch, and what has changed, for the status bar. Looked up
//! away from the loop (a repository on a slow disk keeps nothing waiting),
//! when a shell's prompt comes back, when its directory changes, and now
//! and then for the shell in front.

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

use super::{Ev, Server};
use crate::git;

/// How old what is known of a directory may get, for a shell in front,
/// before it is looked up again: files are changed by other programs too.
const REFRESH: Duration = Duration::from_secs(10);

/// How long a repository whose `git status` took too long is shown with
/// its branch alone, before git is asked again.
const SLOW_PAUSE: Duration = Duration::from_secs(60);

#[derive(Default)]
pub struct GitState {
    by_dir: HashMap<String, Seen>,
}

struct Seen {
    /// The repository the directory is in; `None` while not known, or in none.
    info: Option<git::Info>,
    /// When it was last asked for.
    at: Instant,
    /// A lookup is under way.
    asking: bool,
    /// Something may have changed since that one began: look again after it.
    again: bool,
    /// `git status` gave no answer in time: not asked again until then.
    slow_until: Option<Instant>,
}

impl Server {
    /// What is known of the repository `dir` is in, if it is in one.
    pub fn git_for(&self, dir: &str) -> Option<&git::Info> {
        if !self.cfg.git_status {
            return None;
        }
        self.git.by_dir.get(dir)?.info.as_ref()
    }

    /// Look up the repository of `dir`: now if `fresh` (a command may just
    /// have changed it), or else if what is known is missing or old.
    pub(super) fn git_want(&mut self, dir: &str, fresh: bool) {
        if !self.cfg.git_status {
            return;
        }
        if let Some(seen) = self.git.by_dir.get_mut(dir) {
            if seen.asking {
                seen.again |= fresh;
                return;
            }
            if !fresh && seen.at.elapsed() < REFRESH {
                return;
            }
            seen.asking = true;
            seen.at = Instant::now();
        } else {
            self.git.by_dir.insert(dir.to_string(), Seen { info: None, at: Instant::now(), asking: true, again: false, slow_until: None });
        }
        let status = self.git.by_dir.get(dir).and_then(|seen| seen.slow_until).is_none_or(|until| Instant::now() >= until);
        let tx = self.tx.clone();
        let dir = dir.to_string();
        tokio::task::spawn_blocking(move || {
            // The branch first, which is quick; then what has changed.
            let quick = git::look(Path::new(&dir), false);
            let in_one = quick.is_some();
            let _ = tx.blocking_send(Ev::Git { dir: dir.clone(), info: quick, done: !in_one });
            if in_one {
                let info = git::look(Path::new(&dir), status);
                let _ = tx.blocking_send(Ev::Git { dir, info, done: true });
            }
        });
    }

    /// What a lookup found. `done` is false for the branch alone, sent ahead.
    pub(super) fn on_git(&mut self, dir: String, info: Option<git::Info>, done: bool) {
        let Some(seen) = self.git.by_dir.get_mut(&dir) else { return };
        let info = match (info, &seen.info) {
            // Until git has answered, what it said last time stands, for as
            // long as it is the same branch.
            (Some(mut quick), Some(old)) if !done && quick.branch == old.branch => {
                quick.status = old.status;
                Some(quick)
            }
            (info, _) => info,
        };
        if seen.info != info {
            seen.info = info;
            self.dirty = true;
        }
        if done {
            // In a repository, and git had no answer: it is slow here (or
            // not installed). Its branch alone, for a while.
            if seen.info.as_ref().is_some_and(|info| info.status.is_none()) {
                if seen.slow_until.is_none_or(|until| Instant::now() >= until) {
                    seen.slow_until = Some(Instant::now() + SLOW_PAUSE);
                }
            } else {
                seen.slow_until = None;
            }
            seen.asking = false;
            if std::mem::take(&mut seen.again) {
                self.git_want(&dir, true);
            }
        }
    }

    /// A shell's prompt has come back: whatever it ran may have changed
    /// the repository.
    pub(super) fn git_prompt(&mut self, tab: usize, pane: u64) {
        if let Some(dir) = self.tabs.get(tab).and_then(|t| t.panes.get(pane)).and_then(local_dir) {
            self.git_want(&dir, true);
        }
    }

    /// The shells in front of each tab: anything not yet known, or known a
    /// while ago, is looked up. What no shell is in any longer is forgotten.
    pub(super) fn git_tick(&mut self) {
        if !self.cfg.git_status {
            return;
        }
        let dirs: Vec<String> = self.tabs.iter().filter_map(|t| local_dir(t.panes.focused())).collect();
        for dir in &dirs {
            self.git_want(dir, false);
        }
        if self.git.by_dir.len() > 64 {
            let every: Vec<String> = self.tabs.iter().flat_map(|t| t.panes.iter()).filter_map(local_dir).collect();
            self.git.by_dir.retain(|dir, seen| seen.asking || every.contains(dir));
        }
    }
}

/// The directory of a shell on this machine. (One elsewhere, or reached
/// from this one over `ssh`, has its files there, where they cannot be read.)
fn local_dir(pane: &crate::pane::Pane) -> Option<String> {
    if pane.there().is_some() || pane.inner().is_some() {
        return None;
    }
    pane.cwd.clone()
}

// ---- the chip's menu ------------------------------------------------------------

/// What the agent is asked to do with the changes.
const REVIEW: &str = "Review the changes in this git repository, staged and not: read them with `git diff` and `git diff --staged` (and `git status` for new files). Point out bugs, risks, and anything unclear or unfinished, file by file, most important first. Do not change anything.";

const COMMIT: &str = "Draft a commit message for the changes staged in this git repository: read them with `git diff --staged`, and the last ten commit messages with `git log -n 10 --format=%B%n---` to write in the same style. If nothing is staged, use all the changes (`git diff`, and `git status` for new files), say so, and include `git add` for them in the command. Then propose the commit as one command to run (`git commit -F - <<'EOF'` with the message, for one of several lines). It will be shown to me before it runs: do not run anything else that changes the repository.";

impl Server {
    /// The repository the pane with the keyboard is in, as the status bar
    /// shows it: what is known of it, and its folder on this machine (none
    /// for one on another machine).
    fn chip(&self) -> Option<(git::Info, Option<String>)> {
        let pane = self.tab()?.panes.focused();
        if pane.inner().is_some() || !self.cfg.git_status {
            return None;
        }
        match pane.there() {
            Some(there) => Some((there.git.clone()?, None)),
            None => {
                let cwd = pane.cwd.clone()?;
                let info = self.git_for(&cwd)?.clone();
                let root = git::find(Path::new(&cwd)).map(|r| r.work_tree.to_string_lossy().into_owned());
                Some((info, root))
            }
        }
    }

    /// The git chip's menu, opened at `col` above the status bar.
    pub fn open_git_menu(&mut self, col: u16) {
        let Some((_, root)) = self.chip() else {
            self.toast("This folder is not in a git repository");
            return;
        };
        let mut items = Vec::new();
        // (The files of a shell on another machine are there.)
        if root.is_some() {
            items.push(("Show the changes".to_string(), super::MenuAction::GitDiff));
        }
        items.push(("Ask the agent to review my changes".to_string(), super::MenuAction::GitReview));
        items.push(("Draft a commit message".to_string(), super::MenuAction::GitCommit));
        let height = items.len() as u16 + 2;
        let at = (col, self.rows.saturating_sub(height + 2));
        self.ui.overlay = Some(super::Overlay::Menu(super::Menu { at, items, selected: 0 }));
        self.dirty = true;
    }

    /// `git diff` (and what is staged, where something is) in a new pane
    /// beside this one, in the repository's folder, through git's own pager.
    /// The pane stays until Enter, so that a short diff is not gone at once.
    pub(super) fn show_changes(&mut self) {
        let Some((info, Some(root))) = self.chip() else { return };
        let counts = info.status.unwrap_or_default();
        let diff = match (counts.staged > 0, counts.changed > 0) {
            (false, _) => "git diff",
            (true, false) => "git diff --staged",
            // Both: everything since the last commit, in one.
            (true, true) => "git diff HEAD",
        };
        let script = format!("{diff}; printf '\\n── the end of the changes · Enter closes this pane ──'; read _");
        self.run_beside_in(vec!["sh".into(), "-c".into(), script], false, Some(root));
    }

    /// Ask the tab's agent about the changes: to review them, or to draft
    /// their commit message and propose the commit (which is then always
    /// asked about, autopilot or not).
    pub(super) fn ask_about_changes(&mut self, commit: bool) {
        let idx = self.active;
        if self.tabs[idx].agent.busy {
            self.toast("The agent is still working · Esc to stop it");
            return;
        }
        let (shown, sent) = if commit { ("Draft a commit message", COMMIT) } else { ("Review my changes", REVIEW) };
        let a = &mut self.tabs[idx].agent;
        a.visible = true;
        a.commit_asked = commit;
        self.start_turn_as(idx, shown.to_string(), sent.to_string());
    }
}

