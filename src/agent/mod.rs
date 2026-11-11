//! The built-in terminal agent: provider-neutral message types and the
//! events exchanged between an agent turn (a background task) and the server
//! loop that owns the terminal.

pub mod aws;
pub mod bedrock;
pub mod llm;
pub mod policy;
pub mod run;
pub mod tools;

use crate::config::ProviderKind;
use serde_json::Value;
use tokio::sync::oneshot;

#[derive(Clone, Debug)]
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

#[derive(Clone, Debug)]
pub enum Msg {
    User(String),
    Assistant {
        text: String,
        calls: Vec<ToolCall>,
        /// The provider's own content blocks, replayed unchanged to the same
        /// kind of provider (Anthropic requires thinking blocks to round-trip).
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

#[derive(Clone, Copy, Debug, Default)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
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
    /// The model wants to use a tool. The server decides (policy, approval
    /// card) and answers through `reply`; dropping it cancels the turn.
    ToolRequest { call: ToolCall, reply: oneshot::Sender<ToolReply> },
    ToolResult { id: String, name: String, content: String, is_error: bool },
    Done,
    Failed(String),
}

/// The server's answer to a tool request.
pub enum ToolReply {
    /// The server produced the result itself (ran it in the terminal, read
    /// the screen, or the user declined).
    Result { content: String, is_error: bool },
    /// Approved: run this file tool in the agent task, relative to `cwd`.
    Local { cwd: String },
    /// Approved: run the command in a background subshell because the
    /// terminal cannot take it right now.
    Subprocess { cwd: String, timeout: u64, note: String },
}
