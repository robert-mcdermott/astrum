//! The built-in terminal agent: provider-neutral message types and the
//! events exchanged between an agent turn (a background task) and the server
//! loop that owns the terminal.

pub mod ask;
pub mod aws;
pub mod bedrock;
pub mod compact;
pub mod diff;
pub mod llm;
pub mod models;
pub mod policy;
pub mod redact;
pub mod rules;
pub mod run;
pub mod search;
pub mod tools;

use crate::config::ProviderKind;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::oneshot;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub args: Value,
}

impl ToolCall {
    pub fn str_arg(&self, key: &str) -> Option<&str> {
        self.args.get(key).and_then(Value::as_str)
    }
    pub fn u64_arg(&self, key: &str) -> Option<u64> {
        self.args.get(key).and_then(Value::as_u64)
    }
    pub fn bool_arg(&self, key: &str) -> bool {
        self.args.get(key).and_then(Value::as_bool).unwrap_or(false)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Msg {
    User(String),
    Assistant {
        text: String,
        calls: Vec<ToolCall>,
        /// The provider's own content blocks, replayed unchanged to the same
        /// kind of provider (Anthropic requires thinking blocks to round-trip).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        native: Option<(ProviderKind, Value)>,
    },
    ToolResult {
        id: String,
        /// Kept for providers that key results by tool name rather than call id.
        #[allow(dead_code)]
        name: String,
        content: String,
        is_error: bool,
    },
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct Usage {
    /// Every prompt token of the request, cached or not.
    pub input: u64,
    pub output: u64,
    /// The part of `input` that was read from the provider's prompt cache.
    #[serde(default)]
    pub cache_read: u64,
    /// The part of `input` that was written to it.
    #[serde(default)]
    pub cache_write: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Stop {
    End,
    ToolUse,
    MaxTokens,
    Refusal(Option<String>),
    Other(String),
}

pub struct Completion {
    pub text: String,
    pub calls: Vec<ToolCall>,
    pub native: Option<(ProviderKind, Value)>,
    pub stop: Stop,
    pub usage: Usage,
}

pub enum StreamEv {
    Text(String),
    Thinking(String),
}

/// Events an agent turn sends to the server loop.
pub enum AgentEv {
    Status(String),
    Delta(String),
    Thinking(String),
    Assistant { msg: Msg, usage: Usage },
    /// The rules files in effect for this turn (possibly none).
    Rules(Vec<String>),
    /// The conversation was compacted: `history` replaces what the server
    /// holds, `summary` is what the model wrote, `replaced` how many messages
    /// it stands in for, and `usage` what writing it used.
    Compacted { history: Vec<Msg>, summary: String, replaced: usize, usage: Usage },
    /// Something the user should know that is not part of the conversation.
    Note(String),
    /// A change to a file, worked out and waiting for approval: each hunk
    /// as lines of text. The server shows them and answers with
    /// `ToolReply::Apply`, or with a result if the user declines.
    EditReview { call: ToolCall, path: String, new_file: bool, hunks: Vec<Vec<String>>, reply: oneshot::Sender<ToolReply> },
    /// A pause between steps: has the user said anything in the meantime?
    /// The server answers with the messages to add to the conversation, as
    /// they should be sent (usually none).
    Checkpoint { reply: oneshot::Sender<Vec<String>> },
    /// What the provider says the model's context window is (`None`: it has
    /// no answer), to be remembered for later turns.
    Window(Option<u64>),
    /// The model wants to use a tool. The server decides (policy, approval
    /// card) and answers through `reply`; dropping it cancels the turn.
    ToolRequest { call: ToolCall, reply: oneshot::Sender<ToolReply> },
    /// `redacted`: how many secrets were hidden in `content` on its way here.
    ToolResult { id: String, name: String, content: String, is_error: bool, redacted: usize },
    Done,
    Failed(String),
}

/// The server's answer to a tool request.
pub enum ToolReply {
    /// The server produced the result itself (ran it in the terminal, read
    /// the screen, or the user declined).
    Result { content: String, is_error: bool },
    /// Approved: run this file tool in the agent task, relative to `cwd`.
    /// `at`: on the machine the tab is on, when that is another one.
    Local { cwd: String, at: Option<crate::remote::Way> },
    /// A change to a file needs the user's say: work out what it would
    /// change, relative to `cwd`, and come back with it (`EditReview`).
    Review { cwd: String, at: Option<crate::remote::Way> },
    /// The user's answer to an `EditReview`: which of its hunks to apply.
    Apply { accept: Vec<bool> },
    /// Approved: run the command in a background subshell because the
    /// terminal cannot take it right now.
    Subprocess { cwd: String, timeout: u64, note: String, at: Option<crate::remote::Way> },
    /// Approved: call this tool of an MCP server, waiting this long for it.
    Mcp { client: crate::mcp::Client, tool: String, within: std::time::Duration },
}
