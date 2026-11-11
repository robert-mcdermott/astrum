//! One agent turn: call the model, carry out the tools it asks for (through
//! the server loop, which owns the terminal and the approval UI), feed the
//! results back, and repeat until the model is done.

use super::compact;
use super::diff;
use super::llm::{self, Request};
use super::rules;
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
    /// The terminal's working directory, for finding project rules.
    pub cwd: Option<String>,
    pub history: Vec<Msg>,
    pub max_tokens: Option<u32>,
    /// The model's context window, in tokens.
    pub window: u64,
    /// Nothing says how large the window is yet: ask the provider first.
    pub lookup_window: bool,
    /// Compact before a request once the prompt has reached 80% of the
    /// window or this many tokens, whichever is less; `None` leaves
    /// compaction to the user.
    pub compact_limit: Option<u64>,
    /// Prompt size of the conversation's latest request.
    pub context: u64,
    /// `/compact`: only compact, with this to focus on, and stop.
    pub compact_only: Option<String>,
}

/// How a message typed while the agent was working is put to the model.
pub fn interjection(text: &str) -> String {
    format!("[The user added this while you were working. Take it into account now: it may change, add to or stop what you are doing.]\n\n{text}")
}

/// Between steps, pick up anything the user typed meanwhile. True if there
/// was something, which is now the last message of the conversation.
async fn checkpoint(t: &mut Turn) -> bool {
    let (reply_tx, reply_rx) = oneshot::channel();
    let _ = t.tx.send(Ev::Agent { tab: t.tab, turn: t.turn, ev: AgentEv::Checkpoint { reply: reply_tx } }).await;
    let added = reply_rx.await.unwrap_or_default();
    let any = !added.is_empty();
    t.history.extend(added.into_iter().map(Msg::User));
    any
}

/// Work out a file change, show it to the user, and make the parts they
/// accept. `None` if the turn was cancelled while they were deciding.
async fn review_change(t: &Turn, call: &super::ToolCall, cwd: &str) -> Option<(String, bool)> {
    let change = match tools::plan_change(call, cwd).await {
        Ok(change) => change,
        Err(problem) => return Some((problem, true)),
    };
    let old = change.old.clone().unwrap_or_default();
    let hunks = diff::hunks(&old, &change.new, 3);
    if hunks.is_empty() {
        return Some((format!("{} already has exactly this content; nothing was changed.", change.path.display()), false));
    }
    let (reply_tx, reply_rx) = oneshot::channel();
    let ev = AgentEv::EditReview {
        call: call.clone(),
        path: crate::paths::tildify(&change.path.to_string_lossy()),
        new_file: !change.path.exists(),
        hunks: hunks.iter().map(diff::render).collect(),
        reply: reply_tx,
    };
    let _ = t.tx.send(Ev::Agent { tab: t.tab, turn: t.turn, ev }).await;
    let accept = match reply_rx.await.ok()? {
        ToolReply::Apply { accept } if accept.len() == hunks.len() => accept,
        ToolReply::Result { content, is_error } => return Some((content, is_error)),
        _ => return Some(("That answer does not fit this request; nothing was changed.".into(), true)),
    };
    let all = accept.iter().all(|a| *a);
    let content = if all { change.new.clone() } else { diff::apply(&old, &hunks, &accept) };
    if let Err(problem) = tools::commit_change(&change, &content).await {
        return Some((problem, true));
    }
    if all {
        return Some((change.done.clone(), false));
    }
    let declined: Vec<String> = hunks.iter().zip(&accept).filter(|(_, a)| !**a).map(|(h, _)| diff::render(h).join("\n")).collect();
    Some((
        format!(
            "Applied {} of {} changes to {}. The user looked at each one and declined the rest, which were NOT made:\n\n{}\n\nDo not make the declined changes some other way. If the task needs them, ask the user.",
            accept.iter().filter(|a| **a).count(),
            hunks.len(),
            change.path.display(),
            declined.join("\n\n")
        ),
        false,
    ))
}

/// Summarize the conversation and put the summary in its place, telling the
/// server. On failure the history is left as it was.
async fn compact_history(t: &mut Turn, focus: &str) -> Result<(), String> {
    let Some((body, pending)) = compact::split(&t.history) else {
        return Err("there is not enough conversation to compact yet".into());
    };
    let _ = t.tx.send(Ev::Agent { tab: t.tab, turn: t.turn, ev: AgentEv::Status("Compacting the conversation".into()) }).await;
    let replaced = body.len();
    let summary = compact::summarize(&t.client, &t.provider, &t.model, body, focus, t.window, t.max_tokens).await.map_err(|e| format!("{e:#}"))?;
    let mut history = vec![compact::summary_message(&summary.text, pending.is_none() && t.compact_only.is_none())];
    history.extend(pending.cloned());
    t.history = history.clone();
    let ev = AgentEv::Compacted { history, summary: summary.text, replaced, usage: summary.usage };
    let _ = t.tx.send(Ev::Agent { tab: t.tab, turn: t.turn, ev }).await;
    Ok(())
}

pub async fn run_turn(mut t: Turn) {
    let specs = tools::specs();
    let send = |tx: &mpsc::Sender<Ev>, tab: u64, turn: u64, ev: AgentEv| {
        let tx = tx.clone();
        async move {
            let _ = tx.send(Ev::Agent { tab, turn, ev }).await;
        }
    };

    if t.lookup_window {
        // A provider that cannot be reached is asked again next turn.
        if let Ok(found) = llm::context_window(&t.client, &t.provider, &t.model).await {
            t.window = found.unwrap_or(t.window);
            send(&t.tx, t.tab, t.turn, AgentEv::Window(found)).await;
        }
    }
    if let Some(focus) = t.compact_only.clone() {
        let ev = match compact_history(&mut t, &focus).await {
            Ok(()) => AgentEv::Done,
            Err(e) => AgentEv::Failed(format!("Could not compact the conversation: {e}.")),
        };
        send(&t.tx, t.tab, t.turn, ev).await;
        return;
    }

    let found = rules::load(t.cwd.as_deref()).await;
    t.system.push_str(&found.text);
    send(&t.tx, t.tab, t.turn, AgentEv::Rules(found.sources)).await;

    // One retry, after compacting, if the provider says the request is too large.
    let mut may_retry_overflow = t.compact_limit.is_some();
    for _ in 0..MAX_STEPS {
        // Make room before the conversation outgrows the model's context.
        let compact_at = t.compact_limit.map(|cap| (t.window / 10 * 8).min(cap));
        if compact_at.is_some_and(|limit| t.context >= limit) && compact::split(&t.history).is_some() {
            match compact_history(&mut t, "").await {
                Ok(()) => t.context = 0,
                Err(e) => {
                    // Carry on uncompacted; the provider will say if it no longer fits.
                    t.compact_limit = None;
                    send(&t.tx, t.tab, t.turn, AgentEv::Note(format!("Could not compact the conversation ({e}); carrying on without."))).await;
                }
            }
        }
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
        let result = llm::complete(&t.client, &req, &mut on).await;
        let too_large = match &result {
            Ok(c) => c.calls.is_empty() && c.text.trim().is_empty() && matches!(&c.stop, Stop::Other(r) if r == "model_context_window_exceeded"),
            Err(e) => compact::is_overflow(&format!("{e:#}")),
        };
        if too_large && may_retry_overflow && compact_history(&mut t, "").await.is_ok() {
            may_retry_overflow = false;
            t.context = 0;
            continue;
        }
        let done = match result {
            Ok(c) => c,
            Err(e) => {
                let hint = if too_large { " The conversation no longer fits in the model's context: `/compact` shortens it, `/clear` starts a new one." } else { "" };
                send(&t.tx, t.tab, t.turn, AgentEv::Failed(format!("{e:#}{hint}"))).await;
                return;
            }
        };
        if done.usage.input > 0 {
            t.context = done.usage.input;
        }

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
                    "model_context_window_exceeded" => "the conversation no longer fits in the model's context window. `/compact` shortens it, `/clear` starts a new one".to_string(),
                    other => format!("the model stopped without a reply ({other})"),
                };
                send(&t.tx, t.tab, t.turn, AgentEv::Failed(format!("Stopped: {why}."))).await;
                return;
            }
            _ => {}
        }
        if calls.is_empty() {
            // The model is finished, unless the user has added something.
            if checkpoint(&mut t).await {
                continue;
            }
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
                        Ok(ToolReply::Review { cwd }) => match review_change(&t, &call, &cwd).await {
                            Some(result) => result,
                            None => return,
                        },
                        Ok(ToolReply::Apply { .. }) => ("That answer does not fit this request; nothing was changed.".into(), true),
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
        // Steering: what the user typed during this step goes in with its results.
        checkpoint(&mut t).await;
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
- To locate files or text, use find_files and search_files rather than find and grep: they need no approval and skip dependency and build directories.
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
