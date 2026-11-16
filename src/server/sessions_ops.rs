//! The sessions there are, from inside one: go to another in this
//! terminal, start a new one, stop one, or bring one back that was stopped.
//!
//! Going to another session is the terminal's own doing: the server tells
//! the client which (`ServerMsg::Switch`), and the client lets go of this
//! session and attaches to that one, in the same terminal. Only the
//! terminal that asked goes; the session it leaves keeps running.

use super::{Ev, Server};
use crate::proto::{ServerFrame, ServerMsg};
use std::path::Path;

/// The list, as one terminal is looking at it.
pub struct SessionsView {
    /// What was found; none until it has been looked for.
    pub rows: Option<Vec<Row>>,
    pub selected: usize,
    /// The row whose stopping (or forgetting) is being asked about.
    pub asking: Option<usize>,
    pub note: String,
    /// The session this terminal came from, to go back to.
    back: Option<String>,
}

pub enum Row {
    /// Back to the session this terminal was in before.
    Back(String),
    Session(Found),
    New,
}

/// A session, running or stopped.
#[derive(Clone, Debug)]
pub struct Found {
    pub name: String,
    /// The one this list is shown in.
    pub here: bool,
    pub running: bool,
    pub tabs: Vec<String>,
    /// Terminals attached, for one that is running.
    pub clients: usize,
    /// How long it has been up, or since it was saved (in seconds).
    pub age: u64,
    /// Its tabs are in the folder, or the git repository, you are in.
    pub near: bool,
    dirs: Vec<String>,
}

/// A duration as a person would say it, briefly.
fn ago(secs: u64) -> String {
    match secs {
        0..60 => "moments".into(),
        60..3600 => format!("{}m", secs / 60),
        3600..86400 => format!("{}h {}m", secs / 3600, secs % 3600 / 60),
        _ => format!("{}d", secs / 86400),
    }
}

impl Found {
    /// What the list says of it.
    pub fn detail(&self) -> String {
        let n = self.tabs.len();
        let tabs = format!("{n} tab{}", if n == 1 { "" } else { "s" });
        let names = self.tabs.join(", ");
        if self.running {
            let attached = match self.clients {
                0 => "detached".to_string(),
                1 => "attached".to_string(),
                n => format!("{n} terminals"),
            };
            format!("{tabs} · {attached} · up {} · {names}", ago(self.age))
        } else {
            format!("stopped · {tabs} · saved {} ago · {names}", ago(self.age))
        }
    }
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Every other session there is: running ones (asked, each), then stopped
/// ones that would come back if started. (Asking takes a moment for each,
/// so this is done away from the server's own work.)
fn gather(this: &str) -> Vec<Found> {
    let mut found = Vec::new();
    for name in crate::client::session_names().into_iter().filter(|n| n != this) {
        if let Some(info) = crate::client::info(&name) {
            found.push(Found { name, here: false, running: true, tabs: info.tabs, clients: info.clients, age: now().saturating_sub(info.started_unix), near: false, dirs: info.dirs });
        }
    }
    for (name, saved) in super::restore::list() {
        if name == this || found.iter().any(|f| f.name == name) || !crate::valid_session(&name) {
            continue;
        }
        let dirs: Vec<String> = saved.tabs.iter().filter_map(|t| t.panes.iter().find(|p| p.id == t.focus).or(t.panes.first()).and_then(|p| p.cwd.clone())).collect();
        let tabs = saved
            .tabs
            .iter()
            .zip(&dirs)
            .map(|(t, dir)| t.name.clone().unwrap_or_else(|| Path::new(dir).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "~".into())))
            .collect();
        found.push(Found { name, here: false, running: false, tabs, clients: 0, age: now().saturating_sub(saved.saved), near: false, dirs });
    }
    found
}

/// Is a folder in the folder or repository `near` names?
fn is_near(dir: &str, near: &[String]) -> bool {
    near.iter().any(|n| Path::new(dir).starts_with(n))
}

impl Server {
    /// Each tab's folder (the pane with the keyboard's), for one on this machine.
    pub(super) fn tab_dirs(&self) -> Vec<String> {
        self.tabs.iter().filter_map(|t| t.panes.focused().there().is_none().then(|| t.panes.focused().cwd.clone()).flatten()).collect()
    }

    /// Open the list of sessions; what is there is looked for meanwhile.
    pub fn open_sessions(&mut self) {
        let back = self.clients.iter().find(|c| Some(c.id) == self.loaded).and_then(|c| c.came_from.clone()).filter(|b| *b != self.session);
        self.ui.overlay = Some(super::Overlay::Sessions(SessionsView { rows: None, selected: 0, asking: None, note: String::new(), back }));
        self.look_for_sessions();
        self.dirty = true;
    }

    fn look_for_sessions(&self) {
        let tx = self.tx.clone();
        let this = self.session.clone();
        tokio::task::spawn_blocking(move || {
            let found = gather(&this);
            let _ = tx.blocking_send(Ev::Sessions(found));
        });
    }

    /// What was found: the list, in each terminal that has it open.
    pub(super) fn on_sessions(&mut self, mut found: Vec<Found>) {
        // This one, as it is.
        found.push(Found {
            name: self.session.clone(),
            here: true,
            running: true,
            tabs: self.tabs.iter().map(crate::ui::tab_title).collect(),
            clients: self.clients.iter().filter(|c| c.attached).count(),
            age: self.started.elapsed().map(|d| d.as_secs()).unwrap_or(0),
            near: false,
            dirs: self.tab_dirs(),
        });
        // Those whose tabs are in the folder, or the repository, you are in come first.
        let cwd = self.tab().filter(|t| t.panes.focused().there().is_none()).and_then(|t| t.panes.focused().cwd.clone());
        let near: Vec<String> = cwd.iter().flat_map(|c| [Some(c.clone()), crate::git::find(Path::new(c)).map(|r| r.work_tree.to_string_lossy().into_owned())]).flatten().collect();
        for f in &mut found {
            f.near = f.dirs.iter().any(|d| is_near(d, &near));
        }
        found.sort_by(|a, b| (!a.near, !a.running, &a.name).cmp(&(!b.near, !b.running, &b.name)));
        let views = std::iter::once(&mut self.ui).chain(self.clients.iter_mut().filter_map(|c| c.view.as_mut().map(|v| &mut v.ui)));
        for ui in views {
            let Some(super::Overlay::Sessions(view)) = &mut ui.overlay else { continue };
            let mut rows = Vec::new();
            if let Some(back) = view.back.clone().filter(|b| found.iter().any(|f| f.name == *b)) {
                rows.push(Row::Back(back));
            }
            rows.extend(found.iter().cloned().map(Row::Session));
            rows.push(Row::New);
            view.selected = view.selected.min(rows.len() - 1);
            if view.rows.is_none() {
                // The keyboard starts on the first that is not this one.
                view.selected = rows.iter().position(|r| !matches!(r, Row::Session(f) if f.here)).unwrap_or(0);
            }
            view.rows = Some(rows);
        }
        self.dirty = true;
    }

    fn sessions_view(&mut self) -> Option<&mut SessionsView> {
        match &mut self.ui.overlay {
            Some(super::Overlay::Sessions(view)) => Some(view),
            _ => None,
        }
    }

    /// A row was chosen: go there.
    pub(super) fn session_chosen(&mut self, row: usize) {
        let Some(view) = self.sessions_view() else { return };
        let Some(rows) = &view.rows else { return };
        let to = match rows.get(row) {
            Some(Row::Back(name)) => name.clone(),
            Some(Row::Session(f)) if f.here => {
                view.note = "This is the session you are in.".into();
                self.dirty = true;
                return;
            }
            Some(Row::Session(f)) => f.name.clone(),
            Some(Row::New) => {
                self.ui.overlay = Some(super::Overlay::NewSession { text: String::new() });
                self.dirty = true;
                return;
            }
            None => return,
        };
        self.switch_to(&to, None);
    }

    /// Send the terminal that asked to another session.
    fn switch_to(&mut self, session: &str, cwd: Option<String>) {
        let Some(client) = self.clients.iter().find(|c| Some(c.id) == self.loaded && c.attached) else { return };
        if !client.switches {
            self.toast(format!("This terminal's astrum is from before sessions could be switched: run `astrum -s {session}` there"));
            return;
        }
        let _ = client.tx.send(ServerFrame::Msg(ServerMsg::Switch { session: session.to_string(), cwd }));
        self.ui.overlay = None;
        self.dirty = true;
    }

    /// "New session…": a name, and a folder if not the one you are in.
    pub(super) fn new_session(&mut self, asked: &str) {
        let mut words = asked.splitn(2, char::is_whitespace);
        let name = words.next().unwrap_or("").trim();
        let folder = words.next().map(str::trim).filter(|f| !f.is_empty());
        if name.is_empty() {
            return;
        }
        if !crate::valid_session(name) {
            self.toast("A session's name may only have letters, digits, '-' and '_'");
            return;
        }
        if name == self.session {
            self.toast("This is the session you are in");
            return;
        }
        let here = self.tab().filter(|t| t.panes.focused().there().is_none()).and_then(|t| t.panes.focused().cwd.clone());
        let cwd = match folder {
            Some(folder) => {
                let dir = crate::paths::resolve(folder, here.as_deref().unwrap_or("/"));
                if !dir.is_dir() {
                    self.toast(format!("There is no folder {folder}"));
                    return;
                }
                Some(dir.to_string_lossy().into_owned())
            }
            None => here,
        };
        self.switch_to(name, cwd);
    }

    /// Stop (or, for one that is stopped, forget) a session: asked first.
    pub(super) fn session_stop_asked(&mut self, row: usize) {
        let Some(view) = self.sessions_view() else { return };
        let Some(Row::Session(f)) = view.rows.as_ref().and_then(|r| r.get(row)) else { return };
        if f.here {
            view.note = "To stop the session you are in, close its tabs, or run `astrum kill` from outside it.".into();
        } else {
            view.asking = Some(row);
            view.selected = row;
        }
        self.dirty = true;
    }

    pub(super) fn session_answer(&mut self, yes: bool) {
        let Some(view) = self.sessions_view() else { return };
        let Some(row) = view.asking.take() else { return };
        let Some(Row::Session(f)) = view.rows.as_ref().and_then(|r| r.get(row)).filter(|_| yes) else {
            self.dirty = true;
            return;
        };
        let (name, running) = (f.name.clone(), f.running);
        if running {
            view.note = format!("Stopping {name}…");
            let tx = self.tx.clone();
            let this = self.session.clone();
            tokio::task::spawn_blocking(move || {
                crate::client::kill(&name);
                let _ = tx.blocking_send(Ev::Sessions(gather(&this)));
            });
        } else {
            super::restore::forget(&name);
            view.note = format!("Forgot {name}.");
            self.look_for_sessions();
        }
        self.dirty = true;
    }

    pub(super) fn sessions_key(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::KeyCode;
        let Some(view) = self.sessions_view() else { return };
        let n = view.rows.as_ref().map_or(1, |r| r.len().max(1));
        if view.asking.is_some() {
            match key.code {
                KeyCode::Char('y') | KeyCode::Enter => self.session_answer(true),
                _ => self.session_answer(false),
            }
            return;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.ui.overlay = None,
            KeyCode::Up | KeyCode::Char('k') => view.selected = (view.selected + n - 1) % n,
            KeyCode::Down | KeyCode::Char('j') => view.selected = (view.selected + 1) % n,
            KeyCode::Enter => {
                let row = view.selected;
                self.session_chosen(row);
            }
            KeyCode::Delete | KeyCode::Char('x') => {
                let row = view.selected;
                self.session_stop_asked(row);
            }
            _ => {}
        }
        self.dirty = true;
    }
}
