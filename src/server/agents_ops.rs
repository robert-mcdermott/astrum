//! Everything that needs you, at a glance: what each tab's agent is doing,
//! and what each pane says (that it needs you, is at work, has finished or
//! failed; see `crate::signal`); a screen that lists them all; and word of
//! one that needs you or has finished while nobody can see it (a bell, or
//! the terminal's own notification).

use super::session::{Choice, Ended, Item, ToolState};
use super::{Focus, Hit, Overlay, Server};
use crate::pane::Block;
use crate::proto::ServerFrame;
use crate::signal::{From, Need, Said, Signal, State, Status};
use alacritty_terminal::term::TermMode;
use std::hash::{Hash, Hasher};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

/// The longest command that can be answered yes from the list of agents:
/// what the list shows of it in full. Anything longer, or on more than one
/// line, or a file change, or anything with a warning, is opened instead.
pub const WHOLE: usize = 70;

/// How long a question has to have been there before the list takes a yes
/// for it, so that a click or a key meant for the one before is not taken
/// for an answer to it.
pub const FRESH: Duration = Duration::from_millis(700);


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
    /// Waiting for an answer: the question, and its first line. `yes` is
    /// the choice that says yes, where it can be given from the list (the
    /// question is shown whole there); `key` tells this question from the
    /// next.
    Asks { question: String, first: String, yes: Option<usize>, key: u64 },
    /// At work: what it says it is doing, and the command or tool it is on.
    Working { status: String, on: String, since: Option<Instant> },
    /// Following a command it started, which outlived its wait.
    Following,
    /// Done, at rest.
    Ended { how: Ended, at: Instant },
    /// A conversation, nothing going on.
    Idle,
}

/// The least time between two notifications about the same pane.
const TOLD_GAP: Duration = Duration::from_secs(2);

/// One row of the overview: a tab's agent, or something one of its panes
/// is saying.
pub struct Row {
    /// The tab.
    pub idx: usize,
    pub title: String,
    pub what: What,
}

pub enum What {
    Agent {
        doing: Doing,
        /// The model, and the tokens the conversation has used.
        model: String,
        tokens: u64,
        unseen: bool,
    },
    Pane {
        id: u64,
        /// Its place among the tab's panes, from one; none if it is the only one.
        number: Option<usize>,
        signal: Signal,
    },
}

impl Row {
    /// It wants an answer.
    pub fn asks(&self) -> bool {
        match &self.what {
            What::Agent { doing, .. } => matches!(doing, Doing::Asks { .. }),
            What::Pane { signal, .. } => signal.need == Need::NeedsYou,
        }
    }

    /// It has news nobody has seen.
    pub fn news(&self) -> bool {
        match &self.what {
            What::Agent { unseen, .. } => *unseen,
            What::Pane { signal, .. } => signal.news,
        }
    }

    pub fn hit(&self) -> Hit {
        match &self.what {
            What::Agent { .. } => Hit::AgentRow(self.idx),
            What::Pane { id, .. } => Hit::PaneRow(self.idx, *id),
        }
    }
}

/// What a pane is saying, in a few words.
pub fn headline(s: &Signal) -> String {
    let cmd = |text: &str| if text.is_empty() { "A command".to_string() } else { format!("`{}`", crate::ui::text::truncate(text, 60)) };
    let who = if s.app.is_empty() { "A program".to_string() } else { s.app.clone() };
    let said = |text: &str| if text.is_empty() { String::new() } else { format!(": {text}") };
    match (s.from, s.need) {
        (From::Shell, Need::Working) => format!("{} running", cmd(&s.text)),
        (From::Shell, Need::Failed) => match s.exit {
            Some(code) => format!("{} failed (exit {code})", cmd(&s.text)),
            None => format!("{} failed", cmd(&s.text)),
        },
        (From::Shell, _) => format!("{} finished", cmd(&s.text)),
        (From::Bell, _) => "Rang the bell".into(),
        (From::Notice, _) => match (s.title.is_empty(), s.text.is_empty()) {
            (false, false) => format!("{}: {}", s.title, s.text),
            (false, true) => s.title.clone(),
            _ => s.text.clone(),
        },
        (From::Program, need) => {
            let what = match need {
                Need::NeedsYou => match s.kind.as_deref() {
                    Some("permission") => "needs your permission".to_string(),
                    Some("question") => "has a question".to_string(),
                    Some("auth") => "needs you to sign in".to_string(),
                    _ => "needs you".to_string(),
                },
                Need::Working => match s.progress {
                    Some(p) => format!("is at work, {p}%"),
                    None => "is at work".to_string(),
                },
                Need::Done => "is done".to_string(),
                Need::Failed => "failed".to_string(),
            };
            let task = if s.id.is_empty() { String::new() } else { format!(" ({})", s.id) };
            let text = if s.title.is_empty() { s.text.clone() } else { s.title.clone() };
            format!("{who}{task} {what}{}", said(&text))
        }
    }
}

/// What tells one question from another in the list: the tab, and the
/// call the question is about.
pub fn question_key(tab: u64, call: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (tab, call).hash(&mut h);
    h.finish()
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
            // What is to be done, without the line that says why.
            let shown: Vec<&String> = ap.preview.iter().filter(|l| !l.trim().is_empty() && !l.starts_with("# ")).collect();
            let first = shown.first().map(|l| l.to_string()).unwrap_or_default();
            let whole = ap.warning.is_none() && ap.edit.is_none() && shown.len() == 1 && crate::ui::text::width(&first) <= WHOLE;
            let yes = ap.choices.iter().position(|(c, _)| *c == Choice::Yes).filter(|_| whole);
            return Doing::Asks { question: ap.question.clone(), first, yes, key: question_key(tab.id, &ap.call.id) };
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

    /// The overview's rows: each tab's agent, and under it whatever the
    /// tab's panes are saying.
    pub fn agent_rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        for (idx, tab) in self.tabs.iter().enumerate() {
            let a = &tab.agent;
            let title = crate::ui::tab_title(tab);
            let model = if a.turn_model.1.is_empty() { self.cfg.agent.model.clone() } else { a.turn_model.1.clone() };
            let mut panes = Vec::new();
            let several = tab.panes.len() > 1;
            for (at, pane) in tab.panes.iter().enumerate() {
                let number = several.then_some(at + 1);
                let mut said: Vec<&Signal> = pane.signals.list.iter().collect();
                said.sort_by_key(|s| std::cmp::Reverse((s.need, s.at)));
                for signal in said {
                    panes.push(Row { idx, title: title.clone(), what: What::Pane { id: pane.id, number, signal: signal.clone() } });
                }
                if let Some(signal) = self.working(idx, pane.id) {
                    panes.push(Row { idx, title: title.clone(), what: What::Pane { id: pane.id, number, signal } });
                }
            }
            // Every tab has a row: its agent's, unless there is no
            // conversation and its panes have something to say.
            let doing = self.doing(idx);
            if doing != Doing::Nothing || panes.is_empty() {
                rows.push(Row { idx, title: title.clone(), what: What::Agent { doing, model, tokens: a.totals.input + a.totals.output, unseen: a.unseen } });
            }
            rows.extend(panes);
        }
        rows
    }

    /// A command that has been running in a pane for a while, where nobody
    /// can see it: at work. (Not a full-screen program, which is used, not
    /// waited for; nor a command of the agent's, which says for itself.)
    pub fn working(&self, idx: usize, pane_id: u64) -> Option<Signal> {
        let tab = self.tabs.get(idx)?;
        let pane = tab.panes.get(pane_id)?;
        let after = self.cfg.done_after_secs;
        let crate::pane::ShellState::Running { cmd, since } = &pane.state else { return None };
        let agents = tab.panes.agent == pane_id && (tab.agent.busy || tab.agent.detached);
        let programs = pane.signals.list.iter().any(|s| s.from == From::Program);
        if after == 0 || since.elapsed() < Duration::from_secs(after) || pane.signals.quiet || agents || programs || pane.mode().contains(TermMode::ALT_SCREEN) || self.pane_seen(idx, pane_id) {
            return None;
        }
        let mut signal = Signal::new(From::Shell, Need::Working, crate::signal::clean(cmd.as_deref().unwrap_or(""), 200));
        signal.at = *since;
        Some(signal)
    }

    /// For the status bar: how many agents in tabs other than the one in
    /// front are at work, and how many things there need you (agents
    /// asking, and panes).
    pub fn agents_elsewhere(&self) -> (usize, usize) {
        let mut busy = 0;
        let mut waiting = 0;
        for idx in (0..self.tabs.len()).filter(|i| *i != self.active) {
            match self.doing(idx) {
                Doing::Asks { .. } => waiting += 1,
                Doing::Working { .. } | Doing::Following => busy += 1,
                _ => {}
            }
            waiting += self.tabs[idx].panes.iter().flat_map(|p| p.signals.list.iter()).filter(|s| s.need == Need::NeedsYou).count();
        }
        (busy, waiting)
    }

    /// The most pressing thing the panes of tab `idx` are saying.
    pub fn panes_say(&self, idx: usize) -> Option<&Signal> {
        self.tabs.get(idx)?.panes.iter().filter_map(|p| p.signals.top()).max_by_key(|s| (s.need, s.at))
    }

    /// Can anyone see this pane? Its tab is in front in some terminal, and
    /// it is not hidden behind another that is zoomed.
    pub(super) fn pane_seen(&self, idx: usize, pane: u64) -> bool {
        self.shown(idx) && self.tabs.get(idx).is_some_and(|t| !t.panes.zoomed || t.panes.focus == pane)
    }

    // ---- what panes say -------------------------------------------------------------

    /// The bell rang in a pane.
    pub(super) fn pane_bell(&mut self, idx: usize, pane: u64) {
        self.pane_signal(idx, pane, Signal::new(From::Bell, Need::NeedsYou, String::new()));
    }

    /// A command ended in a pane: if it ran long enough to have been left
    /// to run, that is news.
    pub(super) fn pane_finished(&mut self, idx: usize, pane: u64, block: &Block) {
        let after = self.cfg.done_after_secs;
        let Some(tab) = self.tabs.get(idx) else { return };
        // (The agent says for itself how its own commands went.)
        let agents = tab.panes.agent == pane && (tab.agent.busy || tab.agent.detached);
        if after == 0 || block.took < Duration::from_secs(after) || agents {
            return;
        }
        // Nor is it news twice: what ran has said for itself, while it
        // ran, what there was to say (a bell, a notification, that it is
        // done or waiting).
        let began = Instant::now().checked_sub(block.took);
        let spoke = tab.panes.get(pane).is_some_and(|p| p.signals.list.iter().any(|s| s.from != From::Shell && s.need != Need::Working && began.is_some_and(|b| s.at >= b)));
        if spoke {
            return;
        }
        let failed = block.exit.is_some_and(|code| code != 0);
        let mut signal = Signal::new(From::Shell, if failed { Need::Failed } else { Need::Done }, crate::signal::clean(&block.cmd, 200));
        signal.exit = block.exit;
        self.pane_signal(idx, pane, signal);
    }

    /// A program in a pane said something of itself.
    pub(super) fn pane_said(&mut self, idx: usize, pane: u64, said: Said) {
        match said {
            Said::Notice { title, body } => {
                let mut signal = Signal::new(From::Notice, Need::NeedsYou, body);
                signal.title = title;
                self.pane_signal(idx, pane, signal);
            }
            Said::Status(Status { id, state, kind, progress, msg, title, app }) => {
                let need = match state {
                    State::Working => Need::Working,
                    State::Done => Need::Done,
                    State::Blocked => Need::NeedsYou,
                    State::Error => Need::Failed,
                    State::Idle | State::Clear => {
                        if let Some(p) = self.tabs.get_mut(idx).and_then(|t| t.panes.get_mut(pane)) {
                            p.signals.clear_task(&id);
                        }
                        self.dirty = true;
                        return;
                    }
                };
                let signal = Signal { id, kind: kind.filter(|_| need == Need::NeedsYou), progress, title, app, ..Signal::new(From::Program, need, msg) };
                self.pane_signal(idx, pane, signal);
            }
            // (Answered by the pane itself.)
            Said::Asks => {}
        }
    }

    /// A pane has something to say. It is kept, and if nobody can see the
    /// pane, it is news: marked, and told to each terminal that does not
    /// have the tab in front. Nothing is kept for a pane in front of you,
    /// except a state a program holds (at work, waiting for you), which
    /// still holds when you look away.
    fn pane_signal(&mut self, idx: usize, pane_id: u64, mut signal: Signal) {
        let seen = self.pane_seen(idx, pane_id);
        let Some(tab) = self.tabs.get_mut(idx) else { return };
        let Some(pane) = tab.panes.get_mut(pane_id) else { return };
        if pane.signals.quiet {
            return;
        }
        self.dirty = true;
        if seen && !signal.lasting() {
            // A task done or failed in front of you has nothing left to say.
            if signal.from == From::Program {
                pane.signals.clear_task(&signal.id);
            }
            return;
        }
        // The same again (a bell that keeps ringing, a state said twice) is
        // not news twice.
        let before = pane.signals.list.iter().find(|s| s.from == signal.from && s.id == signal.id);
        if let Some(before) = before.filter(|b| b.need == signal.need && b.text == signal.text && b.kind == signal.kind) {
            signal.at = before.at;
            signal.news = before.news;
            pane.signals.set(signal);
            return;
        }
        signal.news = !seen && signal.need != Need::Working;
        let now = Instant::now();
        let tell = signal.news && pane.signals.told.is_none_or(|at| now.duration_since(at) >= TOLD_GAP);
        // (A bell has been rung already, where it is the way of telling.)
        let ring = signal.from != From::Bell;
        let text = headline(&signal);
        pane.signals.set(signal);
        if tell {
            pane.signals.told = Some(now);
            let title = crate::ui::tab_title(tab);
            self.announce(idx, &format!("Tab {} ({title}): {text}", idx + 1), ring);
        }
    }

    /// Someone can see these panes now: what they said is no longer news.
    /// (Called as a view is drawn, for the panes it shows.)
    pub(super) fn look(&mut self) {
        let Some(tab) = self.tabs.get_mut(self.active) else { return };
        let (zoomed, focus) = (tab.panes.zoomed, tab.panes.focus);
        for pane in tab.panes.iter_mut().filter(|p| !zoomed || p.id == focus) {
            pane.signals.look();
        }
    }

    /// Go to a pane the overview lists: its tab in front, the keyboard in it.
    pub(super) fn go_to_pane(&mut self, idx: usize, pane: u64) {
        self.ui.overlay = None;
        if idx >= self.tabs.len() {
            return;
        }
        self.select_tab(idx);
        if let Some(tab) = self.tabs.get_mut(idx) {
            tab.focus = Focus::Shell;
            if tab.panes.zoomed && tab.panes.focus != pane {
                tab.panes.zoomed = false;
            }
        }
        self.focus_pane(pane);
        self.dirty = true;
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
        if news == News::Asks {
            self.tabs[idx].agent.asked_at = Some(Instant::now());
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
        self.announce(idx, &format!("The agent in tab {} ({title}) {what}", idx + 1), true);
        self.dirty = true;
    }

    /// Tell each terminal that does not have tab `idx` in front, in its own
    /// way of notifying; or with the bell, where it has none (unless `ring`
    /// is false: the bell has been rung already).
    fn announce(&mut self, idx: usize, text: &str, ring: bool) {
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
                _ if ring => "\x07".to_string(),
                _ => continue,
            };
            c.unsent.fetch_add(1, Ordering::SeqCst);
            let _ = c.tx.send(ServerFrame::Output(bytes.into_bytes()));
        }
    }

    // ---- the overview -------------------------------------------------------------

    pub fn open_agents(&mut self) {
        // The keyboard starts on what most needs looking at: a question,
        // then news, then the agent of the tab in front.
        let rows = self.agent_rows();
        let selected = rows
            .iter()
            .position(Row::asks)
            .or_else(|| rows.iter().position(Row::news))
            .or_else(|| rows.iter().position(|r| r.idx == self.active))
            .unwrap_or(0);
        self.ui.overlay = Some(Overlay::Agents { selected });
        self.dirty = true;
    }

    /// Go to what a row of the overview is about.
    fn go_to_row(&mut self, row: &Row) {
        match row.what {
            What::Agent { .. } => self.go_to_agent(row.idx),
            What::Pane { id, .. } => self.go_to_pane(row.idx, id),
        }
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

    /// Answer yes, from the overview, to the question a tab's agent is
    /// asking: only the very question the yes was drawn for, once it has
    /// been there long enough to have been seen.
    pub(super) fn yes_from_overview(&mut self, idx: usize, key: u64) {
        let fresh = self.tabs.get(idx).and_then(|t| t.agent.asked_at).is_some_and(|at| at.elapsed() < FRESH);
        if let Doing::Asks { yes: Some(choice), key: now, .. } = self.doing(idx) {
            if now == key && !fresh {
                self.choose_approval_in(idx, choice);
                if let Some(tab) = self.tabs.get_mut(idx) {
                    tab.agent.unseen = false;
                }
            }
        }
        self.dirty = true;
    }

    pub(super) fn agents_key(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::KeyCode;
        let rows = self.agent_rows();
        let Some(Overlay::Agents { selected }) = &mut self.ui.overlay else { return };
        let n = rows.len().max(1);
        *selected = (*selected).min(n - 1);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.ui.overlay = None,
            KeyCode::Up | KeyCode::Char('k') => *selected = (*selected + n - 1) % n,
            KeyCode::Down | KeyCode::Char('j') => *selected = (*selected + 1) % n,
            KeyCode::Enter => {
                if let Some(row) = rows.get(*selected) {
                    self.go_to_row(row);
                }
            }
            KeyCode::Char('y') => {
                // Only for an agent's row, where the list shows a yes for
                // that question now.
                let Some(row) = rows.get(*selected).filter(|r| matches!(r.what, What::Agent { .. })) else { return };
                let idx = row.idx;
                let drawn = self.ui.hits.iter().find_map(|(_, h)| match h {
                    Hit::AgentYes(i, key) if *i == idx => Some(*key),
                    _ => None,
                });
                if let Some(key) = drawn {
                    self.yes_from_overview(idx, key);
                }
            }
            _ => {}
        }
        self.dirty = true;
    }
}
