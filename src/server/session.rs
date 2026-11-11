//! Per-tab agent state: the conversation, its transcript, the prompt editor,
//! pending approvals and commands the agent is waiting on.

use crate::agent::{Msg, ToolCall, ToolReply, Usage};
use crate::pane::Block;
use serde::{Deserialize, Serialize};
use std::time::Instant;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

pub const COMMANDS: &[(&str, &str)] = &[
    ("/model", "choose the AI provider and model"),
    ("/theme", "choose a color theme"),
    ("/autonomy", "ask first, auto-run read-only, or autopilot"),
    ("/clear", "start a new conversation"),
    ("/resume", "continue your most recent conversation"),
    ("/conversations", "browse and reopen saved conversations"),
    ("/explain", "explain the last command, or why it failed"),
    ("/fix", "work out why the last command failed and fix it"),
    ("/watch", "follow the command that is running and report when it ends"),
    ("/compact", "summarize the conversation so far to free up context"),
    ("/usage", "tokens this conversation has used"),
    ("/copy", "copy the last reply to the clipboard"),
    ("/providers", "show configured providers"),
    ("/key", "store a provider's API key in the system's password store"),
    ("/reload", "re-read providers from the config file and password store"),
    ("/rename", "rename this tab"),
    ("/config", "show where the config file lives"),
    ("/help", "keys and commands"),
];

/// A small multi-line text editor for the agent prompt.
#[derive(Default)]
pub struct Editor {
    pub text: String,
    /// Byte offset of the cursor, always on a char boundary.
    pub cursor: usize,
}

impl Editor {
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }
    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
    }
    pub fn set(&mut self, text: &str) {
        self.text = text.to_string();
        self.cursor = self.text.len();
    }
    pub fn take(&mut self) -> String {
        self.cursor = 0;
        std::mem::take(&mut self.text)
    }
    pub fn insert(&mut self, s: &str) {
        self.text.insert_str(self.cursor, s);
        self.cursor += s.len();
    }
    fn prev_boundary(&self) -> usize {
        self.text[..self.cursor].char_indices().next_back().map(|(i, _)| i).unwrap_or(0)
    }
    fn next_boundary(&self) -> usize {
        self.text[self.cursor..].chars().next().map(|c| self.cursor + c.len_utf8()).unwrap_or(self.text.len())
    }
    pub fn left(&mut self) {
        self.cursor = self.prev_boundary();
    }
    pub fn right(&mut self) {
        self.cursor = self.next_boundary();
    }
    pub fn backspace(&mut self) {
        let p = self.prev_boundary();
        self.text.drain(p..self.cursor);
        self.cursor = p;
    }
    pub fn delete(&mut self) {
        let n = self.next_boundary();
        self.text.drain(self.cursor..n);
    }
    fn line_start(&self) -> usize {
        self.text[..self.cursor].rfind('\n').map(|i| i + 1).unwrap_or(0)
    }
    fn line_end(&self) -> usize {
        self.text[self.cursor..].find('\n').map(|i| self.cursor + i).unwrap_or(self.text.len())
    }
    pub fn home(&mut self) {
        self.cursor = self.line_start();
    }
    pub fn end(&mut self) {
        self.cursor = self.line_end();
    }
    fn word_left_pos(&self) -> usize {
        let before = &self.text[..self.cursor];
        let trimmed = before.trim_end_matches(|c: char| !c.is_alphanumeric());
        trimmed.char_indices().rev().find(|(_, c)| !c.is_alphanumeric()).map(|(i, c)| i + c.len_utf8()).unwrap_or(0)
    }
    fn word_right_pos(&self) -> usize {
        let after = &self.text[self.cursor..];
        let skipped = after.len() - after.trim_start_matches(|c: char| !c.is_alphanumeric()).len();
        let rest = &after[skipped..];
        let word = rest.find(|c: char| !c.is_alphanumeric()).unwrap_or(rest.len());
        self.cursor + skipped + word
    }
    pub fn word_left(&mut self) {
        self.cursor = self.word_left_pos();
    }
    pub fn word_right(&mut self) {
        self.cursor = self.word_right_pos();
    }
    pub fn kill_word(&mut self) {
        let p = self.word_left_pos();
        self.text.drain(p..self.cursor);
        self.cursor = p;
    }
    pub fn kill_to_start(&mut self) {
        let p = self.line_start();
        self.text.drain(p..self.cursor);
        self.cursor = p;
    }
    pub fn kill_to_end(&mut self) {
        let e = self.line_end();
        self.text.drain(self.cursor..e);
    }
    /// Move one line up or down, keeping the column. False if there is no
    /// such line (the caller then falls back to history navigation).
    pub fn vertical(&mut self, down: bool) -> bool {
        let start = self.line_start();
        let col = self.text[start..self.cursor].chars().count();
        let target_start = if down {
            let end = self.line_end();
            if end >= self.text.len() {
                return false;
            }
            end + 1
        } else {
            if start == 0 {
                return false;
            }
            self.text[..start - 1].rfind('\n').map(|i| i + 1).unwrap_or(0)
        };
        let line = self.text[target_start..].split('\n').next().unwrap_or("");
        let offset: usize = line.chars().take(col).map(char::len_utf8).sum();
        self.cursor = target_start + offset;
        true
    }
}

#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolState {
    Proposed,
    Running,
    Done,
    Failed,
    Declined,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ToolItem {
    pub id: String,
    pub name: String,
    /// What the tool acts on: the command line, a path, the text typed.
    pub target: String,
    pub state: ToolState,
    pub exit: Option<i32>,
    /// Short result shown under the row (e.g. an error, or subshell output).
    pub summary: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Item {
    User(String),
    Assistant(String),
    Tool(ToolItem),
    Notice(String),
    Error(String),
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Choice {
    Yes,
    Always,
    Edit,
    /// Go through a file change one hunk at a time.
    Review,
    /// Leave this hunk out.
    Skip,
    No,
}

impl Choice {
    pub fn key(self) -> char {
        match self {
            Choice::Yes => 'y',
            Choice::Always => 'a',
            Choice::Edit => 'e',
            Choice::Review => 'r',
            Choice::Skip => 's',
            Choice::No => 'n',
        }
    }
}

pub struct Approval {
    pub call: ToolCall,
    pub reply: oneshot::Sender<ToolReply>,
    pub question: String,
    /// Why this needs a confirmation even on autopilot.
    pub warning: Option<String>,
    pub preview: Vec<String>,
    pub choices: Vec<(Choice, &'static str)>,
    pub selected: usize,
    /// Lines of the preview scrolled past.
    pub scroll: usize,
    /// Keys are not taken as an answer before this moment: set when the
    /// question appeared while the user was in the middle of typing.
    pub guard_until: Option<Instant>,
    /// Set when this is a file change shown as a diff.
    pub edit: Option<EditReview>,
}

/// A file change under review: its hunks, and how far a one-at-a-time
/// review has got.
pub struct EditReview {
    pub path: String,
    pub hunks: Vec<Vec<String>>,
    /// The verdict on each hunk looked at so far, once the user has chosen
    /// to go through them one at a time.
    pub decided: Option<Vec<bool>>,
}

/// Something the agent turn is blocked on that the server loop resolves.
pub enum Wait {
    /// A command typed into the terminal (or an earlier one being polled).
    Command {
        reply: oneshot::Sender<ToolReply>,
        deadline: Instant,
        call_id: String,
        started: bool,
        note: String,
        timeout: u64,
        /// The command stopped in a pager, which was closed for it: what the
        /// pager's first screen showed.
        paged: Option<String>,
    },
    /// A short pause after typing into a running program, before reporting
    /// what the screen shows.
    Snapshot { reply: oneshot::Sender<ToolReply>, at: Instant, call_id: String },
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum InputMode {
    Prompt,
    /// Editing a proposed command before running it.
    EditCommand,
    /// Typing what the agent should do instead of the declined action.
    Guidance,
    /// Typing an API key to store (`/key`): shown as dots, kept out of the
    /// conversation and the prompt history.
    Secret,
}

pub struct AgentSession {
    pub visible: bool,
    pub items: Vec<Item>,
    pub history: Vec<Msg>,
    pub input: Editor,
    pub mode: InputMode,
    pub busy: bool,
    pub streaming: bool,
    pub status: String,
    pub thinking_tail: String,
    pub started: Option<Instant>,
    pub approval: Option<Approval>,
    pub wait: Option<Wait>,
    /// A command the agent started that outlived its wait and is still running.
    pub detached: bool,
    pub last_finished: Option<Block>,
    pub task: Option<JoinHandle<()>>,
    pub turn: u64,
    /// Lines scrolled up from the bottom of the transcript.
    pub scroll: usize,
    pub totals: Totals,
    /// The context window of the model the current turn runs on.
    pub window: u64,
    /// That model, as (provider, model).
    pub turn_model: (String, String),
    pub prompts: Vec<String>,
    pub prompt_idx: Option<usize>,
    pub draft: String,
    pub seen_seq: u64,
    pub slash_sel: usize,
    /// "Stop asking for this session" was chosen on an approval card.
    pub session_auto: bool,
    /// Where this conversation is saved, once it has something worth saving.
    pub saved: Option<Saved>,
    /// A failed save was already reported; don't repeat it on every step.
    pub save_failed: bool,
    /// The rules files the user has been told are in effect.
    pub rules: Vec<String>,
    /// The turn in progress only compacts (`/compact`); the model is not
    /// being shown anything new.
    pub compacting: bool,
    /// Messages typed while the agent was working, waiting for its next step.
    pub queued: Vec<String>,
    /// The agent is following a command the user started (`/watch`), which
    /// is not the agent's to interrupt.
    pub watching: bool,
    /// When the user last typed into the prompt.
    pub last_typed: Option<Instant>,
    /// The provider whose key is being typed in `InputMode::Secret`.
    pub secret_for: String,
    /// Processor time the running command's job had used when the agent
    /// last looked: (process group, seconds, when).
    pub cpu_seen: Option<(i32, f64, Instant)>,
}

/// Running token totals for one conversation.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Totals {
    /// Prompt tokens of the latest request: how full the context is.
    pub context: u64,
    /// Prompt tokens over every request, including those read from cache.
    pub input: u64,
    /// The part of `input` the provider served from its prompt cache.
    pub cache_read: u64,
    /// The part of `input` the provider wrote to it.
    pub cache_write: u64,
    pub output: u64,
    pub requests: u64,
}

impl Totals {
    pub fn add(&mut self, u: &Usage) {
        self.input += u.input;
        self.cache_read += u.cache_read;
        self.cache_write += u.cache_write;
        self.output += u.output;
        self.requests += 1;
    }

    pub fn total(&self) -> u64 {
        self.input + self.output
    }
}

/// The identity of a conversation on disk.
#[derive(Clone)]
pub struct Saved {
    pub id: String,
    pub created: u64,
}

impl AgentSession {
    pub fn new() -> Self {
        Self {
            visible: false,
            items: Vec::new(),
            history: Vec::new(),
            input: Editor::default(),
            mode: InputMode::Prompt,
            busy: false,
            streaming: false,
            status: String::new(),
            thinking_tail: String::new(),
            started: None,
            approval: None,
            wait: None,
            detached: false,
            last_finished: None,
            task: None,
            turn: 0,
            scroll: 0,
            totals: Totals::default(),
            window: 0,
            turn_model: Default::default(),
            prompts: Vec::new(),
            prompt_idx: None,
            draft: String::new(),
            seen_seq: 0,
            slash_sel: 0,
            session_auto: false,
            saved: None,
            save_failed: false,
            rules: Vec::new(),
            compacting: false,
            queued: Vec::new(),
            watching: false,
            last_typed: None,
            secret_for: String::new(),
            cpu_seen: None,
        }
    }

    pub fn tool_mut(&mut self, id: &str) -> Option<&mut ToolItem> {
        self.items.iter_mut().rev().find_map(|i| match i {
            Item::Tool(t) if t.id == id => Some(t),
            _ => None,
        })
    }

    /// Slash commands matching what is being typed, if the menu should show.
    pub fn slash_matches(&self) -> Vec<(&'static str, &'static str)> {
        if self.mode != InputMode::Prompt || self.approval.is_some() {
            return Vec::new();
        }
        let t = &self.input.text;
        if !t.starts_with('/') || t.contains(char::is_whitespace) {
            return Vec::new();
        }
        COMMANDS.iter().filter(|(name, _)| name.starts_with(t.as_str())).copied().collect()
    }

    /// Every assistant tool call must be answered before the next request;
    /// after a cancellation, answer the ones left hanging.
    pub fn repair_history(&mut self) {
        self.repair_history_with("Cancelled by the user before it completed.");
    }

    pub fn repair_history_with(&mut self, why: &str) {
        let Some(pos) = self.history.iter().rposition(|m| matches!(m, Msg::Assistant { .. })) else { return };
        let Msg::Assistant { calls, .. } = &self.history[pos] else { return };
        let answered: Vec<&str> = self.history[pos + 1..]
            .iter()
            .filter_map(|m| match m {
                Msg::ToolResult { id, .. } => Some(id.as_str()),
                _ => None,
            })
            .collect();
        let missing: Vec<Msg> = calls
            .iter()
            .filter(|c| !answered.contains(&c.id.as_str()))
            .map(|c| Msg::ToolResult {
                id: c.id.clone(),
                name: c.name.clone(),
                content: why.into(),
                is_error: true,
            })
            .collect();
        self.history.extend(missing);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_basics() {
        let mut e = Editor::default();
        e.insert("hello wörld");
        e.word_left();
        assert_eq!(&e.text[e.cursor..], "wörld");
        e.kill_word();
        assert_eq!(e.text, "wörld");
        e.end();
        e.backspace();
        assert_eq!(e.text, "wörl");
        e.insert("\nsecond line");
        assert!(e.vertical(false));
        assert_eq!(e.cursor, 5);
        assert!(!e.vertical(false));
        assert!(e.vertical(true));
        e.kill_to_start();
        assert_eq!(e.text, "wörl\nnd line");
    }

    #[test]
    fn history_repair() {
        let mut a = AgentSession::new();
        a.history.push(Msg::User("x".into()));
        a.history.push(Msg::Assistant {
            text: String::new(),
            calls: vec![
                ToolCall { id: "1".into(), name: "run_command".into(), args: serde_json::json!({}) },
                ToolCall { id: "2".into(), name: "run_command".into(), args: serde_json::json!({}) },
            ],
            native: None,
        });
        a.history.push(Msg::ToolResult { id: "1".into(), name: "run_command".into(), content: "ok".into(), is_error: false });
        a.repair_history();
        assert_eq!(a.history.len(), 4);
        assert!(matches!(&a.history[3], Msg::ToolResult { id, is_error: true, .. } if id == "2"));
    }
}
