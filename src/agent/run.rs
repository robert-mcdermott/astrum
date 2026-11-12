//! One agent turn: call the model, carry out the tools it asks for (through
//! the server loop, which owns the terminal and the approval UI), feed the
//! results back, and repeat until the model is done.

use super::compact;
use super::diff;
use super::llm::{self, Request};
use super::redact;
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
    /// The agent is working in a shell on another machine: the tools that
    /// read and write this machine's files are not offered.
    pub elsewhere: bool,
    /// The tab is on another machine: the way to the Astrum there, which
    /// reads the project's rules as it does the rest of the file work.
    pub at: Option<crate::remote::Way>,
    /// What the MCP servers offer, as it becomes known.
    pub mcp: tokio::sync::watch::Receiver<std::sync::Arc<crate::server::mcp_ops::Offer>>,
    /// The tab's shell is not on this machine, where MCP servers run.
    pub mcp_away: bool,
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
    /// Hide secrets in tool results before the model sees them; the list is
    /// the secrets Astrum holds itself. `None` turns this off.
    pub redact: Option<Vec<String>>,
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
async fn review_change(t: &Turn, call: &super::ToolCall, cwd: &str, at: &Option<crate::remote::Way>) -> Option<(String, bool)> {
    let change = match tools::plan_at(at, call, cwd).await {
        Ok(change) => change,
        Err(problem) => return Some((problem, true)),
    };
    let old = change.old.clone().unwrap_or_default();
    let hunks = diff::hunks(&old, &change.new, 3);
    let exists = change.existed;
    if hunks.is_empty() && exists {
        return Some((format!("{} already has exactly this content; nothing was changed.", change.path.display()), false));
    }
    // A new file with nothing in it has no lines to show, but is still a change to approve.
    let shown: Vec<Vec<String>> = if hunks.is_empty() { vec![vec!["(a new, empty file)".to_string()]] } else { hunks.iter().map(diff::render).collect() };
    let (reply_tx, reply_rx) = oneshot::channel();
    let ev = AgentEv::EditReview {
        call: call.clone(),
        path: crate::paths::tildify(&change.path.to_string_lossy()),
        new_file: !exists,
        hunks: shown.clone(),
        reply: reply_tx,
    };
    let _ = t.tx.send(Ev::Agent { tab: t.tab, turn: t.turn, ev }).await;
    let accept = match reply_rx.await.ok()? {
        ToolReply::Apply { accept } if accept.len() == shown.len() => accept,
        ToolReply::Result { content, is_error } => return Some((content, is_error)),
        _ => return Some(("That answer does not fit this request; nothing was changed.".into(), true)),
    };
    let all = accept.iter().all(|a| *a);
    let content = if all { change.new.clone() } else { diff::apply(&old, &hunks, &accept) };
    if let Err(problem) = tools::commit_at(at, &change, &content).await {
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

/// How long a turn waits for MCP servers that are still starting.
const MCP_WAIT: std::time::Duration = std::time::Duration::from_secs(20);

pub async fn run_turn(mut t: Turn) {
    let mut specs: Vec<_> = tools::specs().into_iter().filter(|spec| !(t.elsewhere && tools::is_local(&spec.name))).collect();
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

    // The MCP servers' tools, once those still starting have had a while.
    if t.compact_only.is_none() {
        if t.mcp.borrow().starting {
            let _ = t.tx.send(Ev::Agent { tab: t.tab, turn: t.turn, ev: AgentEv::Status("Starting MCP servers".into()) }).await;
            let _ = tokio::time::timeout(MCP_WAIT, t.mcp.wait_for(|offer| !offer.starting)).await;
        }
        let offer = t.mcp.borrow().clone();
        if !offer.specs.is_empty() {
            specs.extend(offer.specs.iter().map(|s| tools::ToolSpec { name: s.name.clone(), description: s.description.clone(), schema: s.schema.clone() }));
            t.system.push_str(&offer.note);
            if t.mcp_away {
                t.system.push_str("\nThe shell of this tab is not on the user's own machine, but the MCP servers are: what their tools read and change is on the user's own machine.");
            }
        }
    }
    let mcp_schemas: std::collections::HashMap<String, serde_json::Value> = specs.iter().filter(|s| s.name.contains("__")).map(|s| (s.name.to_string(), s.schema.clone())).collect();

    let found = match (&t.at, &t.cwd) {
        // The user's own rules are here; the project's are where the project is.
        (Some(way), Some(cwd)) => {
            let project = match way.ask(crate::remote::Ask::Rules { cwd: cwd.clone() }).await {
                Ok(crate::remote::Answer::Rules { text, sources }) => rules::Rules { text, sources },
                _ => rules::Rules::default(),
            };
            rules::load_beside(project).await
        }
        (Some(_), None) => rules::load(None).await,
        (None, cwd) => rules::load(cwd.as_deref()).await,
    };
    t.system.push_str(&found.text);
    send(&t.tx, t.tab, t.turn, AgentEv::Rules(found.sources)).await;

    // One retry, after compacting, if the provider says the request is too large.
    let mut may_retry_overflow = t.compact_limit.is_some();
    // Set after compacting, until the next reply shows whether it helped.
    let mut just_compacted = false;
    for _ in 0..MAX_STEPS {
        // Make room before the conversation outgrows the model's context.
        let compact_at = t.compact_limit.map(|cap| (t.window / 10 * 8).min(cap));
        if compact_at.is_some_and(|limit| t.context >= limit) && compact::split(&t.history).is_some() {
            match compact_history(&mut t, "").await {
                Ok(()) => {
                    t.context = 0;
                    just_compacted = true;
                }
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

        // If the conversation is over the limit straight after compacting,
        // it is the fixed part (instructions, tools, the summary itself)
        // that is large, and compacting again at every step would not help.
        if std::mem::take(&mut just_compacted) && t.compact_limit.is_some_and(|cap| t.context >= (t.window / 10 * 8).min(cap)) {
            t.compact_limit = None;
            let note = "The conversation is still close to the model's limit after compacting, so it will not be compacted again in this turn. If the model's context is larger than Astrum assumes, set `context_window` for it in the config file.";
            send(&t.tx, t.tab, t.turn, AgentEv::Note(note.into())).await;
        }

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
            let (content, is_error) = match tools::validate(&call, &mcp_schemas) {
                Err(problem) => (problem, true),
                Ok(()) => {
                    let (reply_tx, reply_rx) = oneshot::channel();
                    send(&t.tx, t.tab, t.turn, AgentEv::ToolRequest { call: call.clone(), reply: reply_tx }).await;
                    match reply_rx.await {
                        // The server dropped the request: the turn was cancelled.
                        Err(_) => return,
                        Ok(ToolReply::Result { content, is_error }) => (content, is_error),
                        Ok(ToolReply::Local { cwd, at }) => tools::run_at(&at, &call, &cwd).await,
                        Ok(ToolReply::Review { cwd, at }) => match review_change(&t, &call, &cwd, &at).await {
                            Some(result) => result,
                            None => return,
                        },
                        Ok(ToolReply::Apply { .. }) => ("That answer does not fit this request; nothing was changed.".into(), true),
                        Ok(ToolReply::Mcp { client, tool, within }) => {
                            let (text, failed) = client.call(&tool, call.args.clone(), within).await;
                            (tools::clip_for_model(&text), failed)
                        }
                        Ok(ToolReply::Subprocess { cwd, timeout, note, at }) => {
                            tools::subprocess_at(&at, call.str_arg("command").unwrap_or_default(), &cwd, timeout, &note).await
                        }
                    }
                }
            };
            // Whatever the tool brought back, the model gets it without the secrets.
            let (content, redacted) = match &t.redact {
                Some(known) => redact::scrub(&content, known),
                None => (content, 0),
            };
            t.history.push(Msg::ToolResult {
                id: call.id.clone(),
                name: call.name.clone(),
                content: content.clone(),
                is_error,
            });
            send(&t.tx, t.tab, t.turn, AgentEv::ToolResult { id: call.id, name: call.name, content, is_error, redacted }).await;
        }
        // Steering: what the user typed during this step goes in with its results.
        checkpoint(&mut t).await;
    }
    send(&t.tx, t.tab, t.turn, AgentEv::Failed(format!("Stopped after {MAX_STEPS} steps. Say \"continue\" to keep going."))).await;
}

/// The stable part of the prompt. Anything that changes between turns (cwd,
/// time, recent commands) goes into the user message instead so the cached
/// prefix stays valid.
/// `there`: the machine the tab is on, when it is not this one.
pub fn system_prompt(shell: &str, extra: &str, there: Option<&crate::remote::Machine>) -> String {
    let (os, user, home) = match there {
        Some(m) => (m.os.clone(), m.user.clone(), m.home.clone()),
        None => (os_description(), std::env::var("USER").unwrap_or_default(), std::env::var("HOME").unwrap_or_default()),
    };
    let os_family = if os.starts_with("macOS") {
        "macOS"
    } else if os.contains("Linux") || there.is_none() && cfg!(not(target_os = "macos")) {
        "Linux"
    } else {
        os.split_whitespace().next().unwrap_or("Unix")
    };
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
- Keys, tokens and passwords in what you read may have been replaced with a note such as [redacted: secret] before reaching you. That is deliberate: work with where the secret lives (a file, a variable) rather than its value, and do not try to recover it or ask the user to paste it.
- Never type passwords, tokens or other secrets. If a command asks for one (sudo, ssh), tell the user to type it in the terminal and wait with command_status.
- Use the platform's own tools ({os_family} here); do not assume GNU options on macOS or BSD options on Linux.
- Be careful with anything destructive or hard to undo (deleting, overwriting, force flags, system configuration, package removal). Say what it will affect before doing it, and prefer a reversible route.
- If a command fails, read the error, work out why, and try a better approach instead of repeating it. If you are blocked, say exactly what is blocking you.
- The user may decline a command. Treat that as direction: adjust or ask what they would prefer; do not retry the same thing.

Replies
- Be brief and concrete. Lead with the answer or the result, then only the detail that matters. Use short Markdown: backticks for commands, paths and values, and a fenced block for multi-line commands or file content.
- Don't repeat command output the user has just watched scroll by; summarize what it means.
- When you finish a task, say what changed and anything the user should check."#,
    );
    if let Some(m) = there {
        s.push_str(&format!(
            "\n\nThis tab is on another machine\n- The terminal is a shell on {user}@{host}, which the user reached from their own machine over ssh. Astrum's own program runs there, so everything you do happens there: run_command runs in that shell, and read_file, find_files, search_files, write_file and edit_file read and change that machine's files. Paths are that machine's, and the environment above describes it.\n- You cannot see or change the user's own machine from this tab.\n- If the connection to it drops, the shell there goes on running and Astrum reconnects by itself. A tool may come back saying the connection was lost before it was answered: then nothing says whether it was done, so look before doing it again.",
            user = m.user,
            host = m.host,
        ));
    }
    if !extra.trim().is_empty() {
        s.push_str("\n\nAdditional instructions from the user:\n");
        s.push_str(extra.trim());
    }
    s
}

pub fn os_description() -> String {
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
