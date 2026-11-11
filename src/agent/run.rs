//! One agent turn: call the model, carry out the tools it asks for (through
//! the server loop, which owns the terminal and the approval UI), feed the
//! results back, and repeat until the model is done.

use super::llm::{self, Request};
use super::tools;
use super::{AgentEv, Msg, Stop, StreamEv, ToolReply};
use crate::config::ProviderConfig;
use crate::server::Ev;
use tokio::sync::{mpsc, oneshot};

const MAX_STEPS: usize = 60;

pub struct Turn {
    pub tab: u64,
    pub turn: u64,
    pub tx: mpsc::Sender<Ev>,
    pub client: reqwest::Client,
    pub provider: ProviderConfig,
    pub model: String,
    pub system: String,
    pub history: Vec<Msg>,
    pub max_tokens: Option<u32>,
}

pub async fn run_turn(mut t: Turn) {
    let specs = tools::specs();
    let send = |tx: &mpsc::Sender<Ev>, tab: u64, turn: u64, ev: AgentEv| {
        let tx = tx.clone();
        async move {
            let _ = tx.send(Ev::Agent { tab, turn, ev }).await;
        }
    };

    for _ in 0..MAX_STEPS {
        send(&t.tx, t.tab, t.turn, AgentEv::Status("Thinking".into())).await;

        let delta_tx = t.tx.clone();
        let (tab, turn) = (t.tab, t.turn);
        // Deltas are best-effort: the final message carries the full text.
        let mut on = move |ev: StreamEv| {
            let ev = match ev {
                StreamEv::Text(s) => AgentEv::Delta(s),
                StreamEv::Thinking(s) => AgentEv::Thinking(s),
            };
            let _ = delta_tx.try_send(Ev::Agent { tab, turn, ev });
        };
        let req = Request {
            provider: &t.provider,
            model: &t.model,
            system: &t.system,
            messages: &t.history,
            tools: &specs,
            max_tokens: t.max_tokens,
        };
        let done = match llm::complete(&t.client, &req, &mut on).await {
            Ok(c) => c,
            Err(e) => {
                send(&t.tx, t.tab, t.turn, AgentEv::Failed(format!("{e:#}"))).await;
                return;
            }
        };

        // A cut-off or refused turn may contain half-written tool calls;
        // never run those.
        let runnable = matches!(done.stop, Stop::ToolUse | Stop::End) && !done.calls.is_empty();
        let calls = if runnable { done.calls.clone() } else { Vec::new() };
        let msg = Msg::Assistant { text: done.text.clone(), calls: calls.clone(), native: if runnable || done.calls.is_empty() { done.native } else { None } };
        t.history.push(msg.clone());
        send(&t.tx, t.tab, t.turn, AgentEv::Assistant { msg, usage: done.usage }).await;

        match &done.stop {
            Stop::MaxTokens => {
                send(&t.tx, t.tab, t.turn, AgentEv::Failed("The reply was cut off at the output limit. Raise agent.max_tokens or ask for a shorter answer.".into())).await;
                return;
            }
            Stop::Refusal(category) => {
                let why = category.as_deref().map(|c| format!(" (category: {c})")).unwrap_or_default();
                send(&t.tx, t.tab, t.turn, AgentEv::Failed(format!("The model declined this request{why}."))).await;
                return;
            }
            Stop::Other(reason) if calls.is_empty() && done.text.trim().is_empty() => {
                let why = match reason.as_str() {
                    "model_context_window_exceeded" => "the conversation no longer fits in the model's context window. Start a new one with /clear".to_string(),
                    other => format!("the model stopped without a reply ({other})"),
                };
                send(&t.tx, t.tab, t.turn, AgentEv::Failed(format!("Stopped: {why}."))).await;
                return;
            }
            _ => {}
        }
        if calls.is_empty() {
            send(&t.tx, t.tab, t.turn, AgentEv::Done).await;
            return;
        }

        for call in calls {
            let (content, is_error) = match tools::validate(&call) {
                Err(problem) => (problem, true),
                Ok(()) => {
                    let (reply_tx, reply_rx) = oneshot::channel();
                    send(&t.tx, t.tab, t.turn, AgentEv::ToolRequest { call: call.clone(), reply: reply_tx }).await;
                    match reply_rx.await {
                        // The server dropped the request: the turn was cancelled.
                        Err(_) => return,
                        Ok(ToolReply::Result { content, is_error }) => (content, is_error),
                        Ok(ToolReply::Local { cwd }) => tools::run_local(&call, &cwd).await,
                        Ok(ToolReply::Subprocess { cwd, timeout, note }) => {
                            tools::run_subprocess(call.str_arg("command").unwrap_or_default(), &cwd, timeout, &note).await
                        }
                    }
                }
            };
            t.history.push(Msg::ToolResult {
                id: call.id.clone(),
                name: call.name.clone(),
                content: content.clone(),
                is_error,
            });
            send(&t.tx, t.tab, t.turn, AgentEv::ToolResult { id: call.id, name: call.name, content, is_error }).await;
        }
    }
    send(&t.tx, t.tab, t.turn, AgentEv::Failed(format!("Stopped after {MAX_STEPS} steps. Say \"continue\" to keep going."))).await;
}

/// The stable part of the prompt. Anything that changes between turns (cwd,
/// time, recent commands) goes into the user message instead so the cached
/// prefix stays valid.
pub fn system_prompt(shell: &str, extra: &str) -> String {
    let os = os_description();
    let user = std::env::var("USER").unwrap_or_default();
    let home = std::env::var("HOME").unwrap_or_default();
    let mut s = format!(
        r#"You are Astrum, an expert terminal agent built into the user's terminal. You share one live shell session with the user: commands you run with run_command execute in their real interactive shell, in the terminal pane they are looking at, so directory changes, environment variables and activated environments persist, and they see everything you do.

You are an expert in {shell}, bash, zsh, POSIX tools, {os_family} system administration, networking, git, Python and general software work. Help the user investigate, troubleshoot, automate and get things done on this machine.

Environment
- Operating system: {os}
- Shell: {shell}
- User: {user} (home: {home})
Each user message starts with a <context> block giving the current working directory, the time, and commands the user ran since your last turn.

How to work
- Act, don't lecture. When the user asks for something you can check or do with a command, run it and report what you found rather than telling them what to type. For pure questions, just answer.
- Look before you change things: inspect the current state first, then make the smallest change that does the job.
- Use commands that finish on their own and print plain output. Avoid pagers and interactive programs: pass --no-pager or pipe to cat (git --no-pager log, systemctl --no-pager), use -y style flags only when the user has clearly asked for the action, and never open an editor; use write_file and edit_file to change files.
- If a command is still running when run_command returns, decide deliberately: keep waiting with command_status, interact with send_input, or interrupt it with send_input key ctrl-c.
- Never type passwords, tokens or other secrets. If a command asks for one (sudo, ssh), tell the user to type it in the terminal and wait with command_status.
- Use the platform's own tools ({os_family} here); do not assume GNU options on macOS or BSD options on Linux.
- Be careful with anything destructive or hard to undo (deleting, overwriting, force flags, system configuration, package removal). Say what it will affect before doing it, and prefer a reversible route.
- If a command fails, read the error, work out why, and try a better approach instead of repeating it. If you are blocked, say exactly what is blocking you.
- The user may decline a command. Treat that as direction: adjust or ask what they would prefer; do not retry the same thing.

Replies
- Be brief and concrete. Lead with the answer or the result, then only the detail that matters. Use short Markdown: backticks for commands, paths and values, and a fenced block for multi-line commands or file content.
- Don't repeat command output the user has just watched scroll by; summarize what it means.
- When you finish a task, say what changed and anything the user should check."#,
        os_family = if cfg!(target_os = "macos") { "macOS" } else { "Linux" },
    );
    if !extra.trim().is_empty() {
        s.push_str("\n\nAdditional instructions from the user:\n");
        s.push_str(extra.trim());
    }
    s
}

fn os_description() -> String {
    let mut uts: libc::utsname = unsafe { std::mem::zeroed() };
    let field = |f: &[libc::c_char]| {
        let bytes: Vec<u8> = f.iter().take_while(|&&c| c != 0).map(|&c| c as u8).collect();
        String::from_utf8_lossy(&bytes).into_owned()
    };
    if unsafe { libc::uname(&mut uts) } != 0 {
        return std::env::consts::OS.to_string();
    }
    let (sys, release, machine) = (field(&uts.sysname), field(&uts.release), field(&uts.machine));
    if cfg!(target_os = "macos") {
        format!("macOS ({sys} {release}, {machine})")
    } else {
        format!("{sys} {release} ({machine})")
    }
}
