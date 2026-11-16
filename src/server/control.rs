//! A running session, from the command line: `astrum run`, `send`,
//! `capture`, `list` and `status` reach it over its socket, which only its
//! owner can open, and are answered here.
//!
//! What these can do is what a person at a pane could: start a program in
//! a pane of its own, type into a pane's program, read what a pane shows.
//! What they cannot: anything to do with the agent's panel. Text that is
//! sent goes to a pane's terminal and nowhere else, so a question the agent
//! is asking, or a command it wants approved, is never answered from here.

use super::session::AgentSession;
use super::{Focus, Server, Tab, panes};
use crate::pane::ShellState;
use crate::proto::{Control, Place, ServerFrame, ServerMsg};
use crate::signal::{Said, State, Status};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// What is said back: done or not, what to print, and an exit code.
type Answer = (bool, String, Option<i32>);

fn done(text: impl Into<String>) -> Option<Answer> {
    Some((true, text.into(), None))
}

fn refused(why: impl Into<String>) -> Option<Answer> {
    Some((false, why.into(), None))
}

impl Server {
    pub(super) fn control(&mut self, client: u64, what: Control) {
        // (None: the answer comes later, when a command waited for has ended.)
        if let Some((ok, text, code)) = self.control_do(client, what) {
            self.reply(client, ok, text, code);
        }
    }

    fn reply(&mut self, client: u64, ok: bool, text: String, code: Option<i32>) {
        if let Some(c) = self.clients.iter().find(|c| c.id == client) {
            let _ = c.tx.send(ServerFrame::Msg(ServerMsg::Reply { ok, text, code }));
        }
    }

    /// The tab and pane meant: the one named, or the one the keyboard is in.
    fn pane_meant(&self, pane: Option<u64>) -> Result<(usize, u64), String> {
        match pane {
            Some(id) => self.find_pane(id).map(|idx| (idx, id)).ok_or_else(|| format!("there is no pane {id} in session `{}` (`astrum list` shows them)", self.session)),
            None => self.tabs.get(self.active).map(|t| (self.active, t.panes.focus)).ok_or_else(|| "the session has no pane yet".to_string()),
        }
    }

    fn control_do(&mut self, client: u64, what: Control) -> Option<Answer> {
        if self.tabs.is_empty() {
            return refused(format!("session `{}` has no tabs yet: attach to it first", self.session));
        }
        match what {
            Control::List => done(self.listing().to_string()),
            Control::Capture { pane, history } => {
                let (idx, id) = match self.pane_meant(pane) {
                    Ok(found) => found,
                    Err(why) => return refused(why),
                };
                let pane = self.tabs[idx].panes.get(id)?;
                let text = if history { pane.history_text().0 } else { pane.recent_text(pane.rows as usize) + "\n" };
                done(text)
            }
            Control::Send { pane, text, enter } => {
                let (idx, id) = match self.pane_meant(pane) {
                    Ok(found) => found,
                    Err(why) => return refused(why),
                };
                let pane = self.tabs[idx].panes.get_mut(id)?;
                if pane.there().is_some_and(|t| !t.up) {
                    return refused("that pane's shell is on a machine that is not reached just now: nothing was sent");
                }
                pane.scroll_to_bottom();
                pane.note_paste(&text);
                let mut bytes = text.into_bytes();
                if enter {
                    pane.note_key(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
                    bytes.push(b'\r');
                }
                pane.write(bytes);
                self.dirty = true;
                done(String::new())
            }
            Control::Status { pane, state, message, progress, kind } => {
                let (idx, id) = match self.pane_meant(pane) {
                    Ok(found) => found,
                    Err(why) => return refused(why),
                };
                let state = match state.as_str() {
                    "idle" => State::Idle,
                    "working" => State::Working,
                    "done" => State::Done,
                    "blocked" => State::Blocked,
                    "error" => State::Error,
                    "clear" => State::Clear,
                    other => return refused(format!("`{other}` is not a state: idle, working, done, blocked, error or clear")),
                };
                let kind = kind.filter(|k| matches!(k.as_str(), "permission" | "question" | "auth"));
                let status = Status { id: String::new(), state, kind, progress: progress.map(|p| p.min(100)), msg: crate::signal::clean(&message, 2048), title: String::new(), app: String::new() };
                self.pane_said(idx, id, Said::Status(status));
                done(String::new())
            }
            Control::Run { command, cwd, place, pane, wait, keep, focus } => {
                if command.is_empty() {
                    return refused("there is no command to run");
                }
                // Left up when it ends, if asked: the command is run by a
                // shell that then waits for Enter, and ends as it ended.
                let run = if keep {
                    let script = r#""$@"; code=$?; printf '\n── ended (exit %s) · Enter closes this pane ──' "$code"; read _; exit "$code""#;
                    ["sh".to_string(), "-c".to_string(), script.to_string(), "astrum-run".to_string()].into_iter().chain(command).collect()
                } else {
                    command
                };
                let id = match place {
                    Place::NewTab => {
                        let (cols, rows) = self.pane_size(false);
                        let new = self.spawn_running(cwd.as_deref(), cols, rows, Some(&run))?;
                        let id = new.id;
                        self.tabs.push(Tab { id, panes: panes::Panes::new(new), name: None, agent: AgentSession::new(), focus: Focus::Shell, activity: false, owner: None, sized: (0, 0) });
                        if focus {
                            self.select_tab(self.tabs.len() - 1);
                        }
                        id
                    }
                    Place::Right | Place::Down => {
                        let (idx, beside) = match self.pane_meant(pane) {
                            Ok(found) => found,
                            Err(why) => return refused(why),
                        };
                        let side_by_side = place == Place::Right;
                        let area = self.panes_area();
                        let tab = &mut self.tabs[idx];
                        let was = tab.panes.focus;
                        tab.panes.focus = beside;
                        if !tab.panes.room_to_split(area, side_by_side) {
                            tab.panes.focus = was;
                            return refused("there is not enough room to split that pane; `--new-tab` opens a tab");
                        }
                        let (cols, rows) = tab.panes.get(beside).map_or((80, 24), |p| ((p.cols / 2).max(2), (p.rows / 2).max(1)));
                        tab.panes.focus = was;
                        let new = self.spawn_running(cwd.as_deref(), cols, rows, Some(&run))?;
                        let id = new.id;
                        let tab = &mut self.tabs[idx];
                        // (Split where it was asked for; the keyboard, and
                        // the agent, stay where they were unless told.)
                        let zoomed = tab.panes.zoomed;
                        tab.panes.focus = beside;
                        tab.panes.add(new, side_by_side);
                        if focus {
                            tab.focus = Focus::Shell;
                            self.select_tab(idx);
                        } else {
                            tab.panes.focus = was;
                            tab.panes.zoomed = zoomed;
                        }
                        id
                    }
                };
                self.dirty = true;
                if wait {
                    self.waiting.push((id, client));
                    return None;
                }
                done(id.to_string())
            }
        }
    }

    /// A pane's program has ended: whoever waited for it is told how.
    pub(super) fn pane_ended(&mut self, pane: u64, code: Option<i32>) {
        let waiting: Vec<u64> = self.waiting.iter().filter(|(p, _)| *p == pane).map(|(_, c)| *c).collect();
        self.waiting.retain(|(p, _)| *p != pane);
        for client in waiting {
            // (Ended by a signal: as a shell would say it.)
            self.reply(client, true, pane.to_string(), Some(code.unwrap_or(137)));
        }
    }

    /// The tabs and their panes, for `astrum list`.
    fn listing(&self) -> serde_json::Value {
        let tabs: Vec<serde_json::Value> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(idx, tab)| {
                let panes: Vec<serde_json::Value> = tab
                    .panes
                    .iter()
                    .map(|p| {
                        let (state, command) = match &p.state {
                            ShellState::Prompt => ("prompt", None),
                            ShellState::Running { cmd, .. } => ("running", cmd.clone().filter(|c| !c.is_empty()).or_else(|| p.job().map(|j| j.name))),
                            ShellState::Unknown => ("unknown", p.job().map(|j| j.name).filter(|n| !crate::procinfo::is_shell(n))),
                        };
                        let says = p.signals.top().map(|s| {
                            let need = match s.need {
                                crate::signal::Need::Working => "working",
                                crate::signal::Need::Done => "done",
                                crate::signal::Need::Failed => "failed",
                                crate::signal::Need::NeedsYou => "needs-you",
                            };
                            serde_json::json!({ "need": need, "text": super::agents_ops::headline(s) })
                        });
                        serde_json::json!({
                            "id": p.id,
                            "focused": p.id == tab.panes.focus,
                            "cwd": p.cwd_shown(),
                            "state": state,
                            "command": command,
                            "machine": p.there().map(|t| t.host.clone()),
                            "says": says,
                        })
                    })
                    .collect();
                serde_json::json!({ "tab": idx + 1, "title": crate::ui::tab_title(tab), "front": idx == self.active, "panes": panes })
            })
            .collect();
        serde_json::json!({ "session": self.session, "tabs": tabs })
    }
}
