//! Every tab's agent at a glance: what each is doing, a screen that lists
//! them all, and word of one that needs you or has finished while its tab
//! is not in front (a bell, or the terminal's own notification).

use super::session::{Choice, Ended, Item, ToolState};
use super::{Focus, Overlay, Server};
use crate::proto::ServerFrame;
use std::sync::atomic::Ordering;
use std::time::Instant;


/// What happened that the user may want to hear of.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum News {
    /// A question for the user.
    Asks,
    Finished,
    Failed,
}

/// What a tab's agent is doing, as the overview and the tab bar show it.
#[derive(Clone, Debug, PartialEq)]
pub enum Doing {
    /// There is no conversation in this tab.
    Nothing,
    /// Waiting for an answer: the question, and its first line.
    Asks { question: String, first: String, yes: Option<usize> },
    /// At work: what it says it is doing, and the command or tool it is on.
    Working { status: String, on: String, since: Option<Instant> },
    /// Following a command it started, which outlived its wait.
    Following,
    /// Done, at rest.
    Ended { how: Ended, at: Instant },
    /// A conversation, nothing going on.
    Idle,
}

/// One row of the overview.
pub struct Row {
    pub idx: usize,
    pub title: String,
    pub doing: Doing,
    /// The model, and the tokens the conversation has used.
    pub model: String,
    pub tokens: u64,
    pub unseen: bool,
}

/// A duration as a person would say it.
pub fn ago(secs: u64) -> String {
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m {}s", secs / 60, secs % 60),
        _ => format!("{}h {}m", secs / 3600, secs % 3600 / 60),
    }
}

impl Server {
    /// What the agent of tab `idx` is doing.
    pub fn doing(&self, idx: usize) -> Doing {
        let Some(tab) = self.tabs.get(idx) else { return Doing::Nothing };
        let a = &tab.agent;
        if let Some(ap) = &a.approval {
            let first = ap.preview.iter().find(|l| !l.trim().is_empty()).cloned().unwrap_or_default();
            let yes = ap.choices.iter().position(|(c, _)| *c == Choice::Yes).filter(|_| ap.edit.as_ref().is_none_or(|e| e.decided.is_none()));
            return Doing::Asks { question: ap.question.clone(), first, yes };
        }
        if a.busy {
            let on = a
                .items
                .iter()
                .rev()
                .find_map(|i| match i {
                    Item::Tool(t) if t.state == ToolState::Running => Some(t.target.lines().next().unwrap_or("").to_string()),
                    _ => None,
                })
                .unwrap_or_default();
            return Doing::Working { status: if a.status.is_empty() { "Working".into() } else { a.status.clone() }, on, since: a.started };
        }
        if a.detached {
            return Doing::Following;
        }
        if let Some((at, how)) = &a.ended {
            return Doing::Ended { how: how.clone(), at: *at };
        }
        if a.history.is_empty() { Doing::Nothing } else { Doing::Idle }
    }

    /// The overview's rows, a tab apiece.
    pub fn agent_rows(&self) -> Vec<Row> {
        (0..self.tabs.len())
            .map(|idx| {
                let tab = &self.tabs[idx];
                let a = &tab.agent;
                let model = if a.turn_model.1.is_empty() { self.cfg.agent.model.clone() } else { a.turn_model.1.clone() };
                Row { idx, title: crate::ui::tab_title(tab), doing: self.doing(idx), model, tokens: a.totals.input + a.totals.output, unseen: a.unseen }
            })
            .collect()
    }

    /// How many agents in tabs other than the one in front are busy or
    /// waiting, or have news; for the status bar.
    pub fn agents_elsewhere(&self) -> (usize, usize) {
        let mut busy = 0;
        let mut waiting = 0;
        for idx in (0..self.tabs.len()).filter(|i| *i != self.active) {
            match self.doing(idx) {
                Doing::Asks { .. } => waiting += 1,
                Doing::Working { .. } | Doing::Following => busy += 1,
                _ => {}
            }
        }
        (busy, waiting)
    }

    /// Something happened in tab `idx`: if no terminal has the tab in
    /// front, it is marked, and each terminal that does not is told.
    pub(super) fn agent_news(&mut self, idx: usize, news: News) {
        if idx >= self.tabs.len() {
            return;
        }
        if !self.shown(idx) {
            self.tabs[idx].agent.unseen = true;
        }
        let title = crate::ui::tab_title(&self.tabs[idx]);
        let what = match news {
            News::Asks => "is asking you something".to_string(),
            News::Finished => "has finished".to_string(),
            News::Failed => match &self.tabs[idx].agent.ended {
                Some((_, Ended::Failed(why))) => format!("stopped: {}", crate::ui::text::truncate(why, 80)),
                _ => "stopped".to_string(),
            },
        };
        let text = format!("The agent in tab {} ({title}) {what}", idx + 1);
        let text: String = text.chars().filter(|c| !c.is_control()).take(200).collect();
        let loaded = self.loaded;
        let active = self.active;
        for c in self.clients.iter().filter(|c| c.attached) {
            let in_front = if Some(c.id) == loaded { active } else { c.view.as_ref().map_or(active, |v| v.active) };
            if in_front == idx {
                continue;
            }
            let bytes = match c.notify.as_deref() {
                Some("osc9") => format!("\x1b]9;Astrum: {text}\x07"),
                Some("osc99") => format!("\x1b]99;;Astrum: {text}\x1b\\"),
                Some("osc777") => format!("\x1b]777;notify;Astrum;{text}\x07"),
                _ => "\x07".to_string(),
            };
            c.unsent.fetch_add(1, Ordering::SeqCst);
            let _ = c.tx.send(ServerFrame::Output(bytes.into_bytes()));
        }
        self.dirty = true;
    }

    // ---- the overview -------------------------------------------------------------

    pub fn open_agents(&mut self) {
        // The keyboard starts on what most needs looking at: a question,
        // then news, then the tab in front.
        let rows = self.agent_rows();
        let selected = rows.iter().position(|r| matches!(r.doing, Doing::Asks { .. })).or_else(|| rows.iter().position(|r| r.unseen)).unwrap_or(self.active);
        self.ui.overlay = Some(Overlay::Agents { selected });
        self.dirty = true;
    }

    /// Go to a tab's agent: the tab in front, the panel open, the keyboard in it.
    pub(super) fn go_to_agent(&mut self, idx: usize) {
        self.ui.overlay = None;
        if idx >= self.tabs.len() {
            return;
        }
        self.select_tab(idx);
        if let Some(tab) = self.tabs.get_mut(idx) {
            tab.agent.visible = true;
            tab.focus = Focus::Agent;
            tab.agent.unseen = false;
        }
        self.dirty = true;
    }

    /// Answer yes, from the overview, to the question a tab's agent is asking.
    pub(super) fn yes_from_overview(&mut self, idx: usize) {
        if let Doing::Asks { yes: Some(choice), .. } = self.doing(idx) {
            self.choose_approval_in(idx, choice);
            if let Some(tab) = self.tabs.get_mut(idx) {
                tab.agent.unseen = false;
            }
        }
        self.dirty = true;
    }

    pub(super) fn agents_key(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::KeyCode;
        let Some(Overlay::Agents { selected }) = &mut self.ui.overlay else { return };
        let n = self.tabs.len().max(1);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.ui.overlay = None,
            KeyCode::Up | KeyCode::Char('k') => *selected = (*selected + n - 1) % n,
            KeyCode::Down | KeyCode::Char('j') => *selected = (*selected + 1) % n,
            KeyCode::Enter => {
                let idx = *selected;
                self.go_to_agent(idx);
            }
            KeyCode::Char('y') => {
                let idx = *selected;
                self.yes_from_overview(idx);
            }
            _ => {}
        }
        self.dirty = true;
    }
}
