//! Agent orchestration on the server loop: prompt editing, slash commands,
//! approvals, running the agent's commands in the live shell, and applying
//! the events an agent turn sends back.

use super::session::{Approval, Choice, EditReview, InputMode, Item, Saved, ToolItem, ToolState, Wait};
use super::store;
use super::{Ev, Focus, Overlay, PickItem, Picker, PickerKind, Server};
use crate::agent::policy;
use crate::agent::run::{self, Turn};
use crate::agent::tools;
use crate::agent::{AgentEv, Msg, ToolCall, ToolReply, llm, models};
use crate::config::{Autonomy, KeySource, ProviderKind};
use crate::input;
use crate::osc;
use crate::pane::{Block, ShellState};
use crate::paths;
use crate::theme;
use alacritty_terminal::term::TermMode;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::time::{Duration, Instant};
use tokio::sync::oneshot;

fn local_time() -> String {
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&now, &mut tm);
        let mut buf = [0u8; 64];
        let fmt = b"%Y-%m-%d %H:%M %Z (%A)\0";
        let n = libc::strftime(buf.as_mut_ptr() as *mut libc::c_char, buf.len(), fmt.as_ptr() as *const libc::c_char, &tm);
        String::from_utf8_lossy(&buf[..n]).into_owned()
    }
}

fn first_line(text: &str, max: usize) -> String {
    let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    if line.chars().count() > max { line.chars().take(max.saturating_sub(1)).collect::<String>() + "…" } else { line.to_string() }
}

fn tool_target(call: &ToolCall) -> String {
    match call.name.as_str() {
        "run_command" => call.str_arg("command").unwrap_or_default().to_string(),
        "read_file" | "write_file" | "edit_file" => call.str_arg("path").unwrap_or_default().to_string(),
        "find_files" | "search_files" => {
            let pattern = call.str_arg("pattern").unwrap_or_default();
            match call.str_arg("path").filter(|p| !p.is_empty() && *p != ".") {
                Some(path) => format!("{pattern} in {path}"),
                None => pattern.to_string(),
            }
        }
        "send_input" => {
            let text = call.str_arg("text").unwrap_or_default();
            match call.str_arg("key") {
                Some(key) if text.is_empty() => format!("<{key}>"),
                Some(key) => format!("{text}<{key}>"),
                None => text.to_string(),
            }
        }
        _ => String::new(),
    }
}

/// What can be told about a command that is still running: which program
/// it is, when it last printed anything, and whether it has been using the
/// processor since the agent last looked. `seen` carries the previous
/// reading from one look to the next.
fn liveness(pane: &crate::pane::Pane, seen: &mut Option<(i32, f64, Instant)>, now: Instant) -> String {
    let quiet = pane.last_output.elapsed().as_secs();
    let mut lines = String::new();
    let job = pane.foreground_group().and_then(crate::procinfo::job);
    if let Some(job) = &job {
        let others = match job.processes {
            1 => String::new(),
            n => format!(", with {} other process{} in its job", n - 1, if n == 2 { "" } else { "es" }),
        };
        lines.push_str(&format!("process: `{}` (pid {}{others})\n", job.name, job.pid));
    }
    let mut activity = if quiet < 2 { "it printed something just now".to_string() } else { format!("no new output for {quiet}s") };
    if let Some(job) = &job {
        match seen.filter(|(group, ..)| *group == job.pid) {
            Some((_, before, at)) if now.duration_since(at).as_secs_f64() >= 1.0 => {
                let (used, over) = ((job.cpu_secs - before).max(0.0), now.duration_since(at).as_secs_f64());
                let verdict = if used / over < 0.02 {
                    "idle: it is probably waiting (for input, the network, a timer or a lock)"
                } else {
                    "working"
                };
                activity.push_str(&format!("; {used:.1}s of processor time used in the last {over:.0}s ({verdict})"));
            }
            _ => activity.push_str(&format!("; {:.1}s of processor time used so far", job.cpu_secs)),
        }
        *seen = Some((job.pid, job.cpu_secs, now));
    }
    lines.push_str(&format!("activity: {activity}\n"));
    lines
}

fn input_bytes(call: &ToolCall) -> Vec<u8> {
    let mut bytes = call.str_arg("text").unwrap_or_default().as_bytes().to_vec();
    let key: &[u8] = match call.str_arg("key") {
        Some("enter") => b"\r",
        Some("ctrl-c") => b"\x03",
        Some("ctrl-d") => b"\x04",
        Some("ctrl-z") => b"\x1a",
        Some("esc") => b"\x1b",
        Some("tab") => b"\t",
        Some("up") => b"\x1b[A",
        Some("down") => b"\x1b[B",
        Some("space") => b" ",
        _ => b"",
    };
    bytes.extend_from_slice(key);
    bytes
}

impl Server {
    fn autonomy_for(&self, idx: usize) -> Autonomy {
        if self.tabs[idx].agent.session_auto { Autonomy::Auto } else { self.cfg.agent.autonomy }
    }

    pub fn cycle_autonomy(&mut self) {
        let next = self.cfg.agent.autonomy.next();
        self.set_autonomy(next);
    }

    fn set_autonomy(&mut self, a: Autonomy) {
        self.cfg.agent.autonomy = a;
        for tab in &mut self.tabs {
            tab.agent.session_auto = false;
        }
        // Autopilot is never remembered: a new session starts cautious
        // unless the config file asks for it.
        self.cfg.state.autonomy = Some(a).filter(|a| *a != Autonomy::Auto);
        self.cfg.save_state();
        let what = match a {
            Autonomy::Ask => "Ask first: confirms every command and change",
            Autonomy::Safe => "Auto read-only: look-only commands run unasked",
            Autonomy::Auto => "Autopilot on until this session ends",
        };
        self.toast(what);
    }

    // ---- model picker -------------------------------------------------------

    pub fn open_model_picker(&mut self) {
        self.ui.overlay = Some(Overlay::Picker(Picker {
            kind: PickerKind::Model,
            title: "Model".into(),
            items: Vec::new(),
            filter: String::new(),
            selected: 0,
            loading: true,
            note: "Asking your providers which models they offer…".into(),
            original_theme: None,
            pending_delete: None,
        }));
        self.mark_dirty();
        let providers: Vec<_> = self.cfg.providers.iter().map(|(n, p)| (n.clone(), p.clone())).collect();
        let client = self.http.clone();
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let jobs = providers.into_iter().map(|(name, p)| {
                let client = client.clone();
                async move {
                    // Hosted OpenAI-style endpoints are useless without a key; don't probe them.
                    let hosted = p.base_url.as_deref().is_some_and(|u| !u.contains("localhost") && !u.contains("127.0.0.1"));
                    if p.kind == ProviderKind::Openai && hosted && p.resolved_key().is_none() && p.models.is_empty() {
                        return (name, Ok(Vec::new()));
                    }
                    let r = llm::list_models(&client, &p).await;
                    (name, r)
                }
            });
            let mut result = Vec::new();
            let mut errors = Vec::new();
            for (name, r) in futures_util::future::join_all(jobs).await {
                match r {
                    Ok(models) => result.extend(models.into_iter().map(|m| (name.clone(), m))),
                    Err(e) => errors.push(format!("{name}: {}", first_line(&format!("{e:#}"), 60))),
                }
            }
            let _ = tx.send(Ev::Models { result, errors }).await;
        });
    }

    pub(super) fn on_models(&mut self, result: Vec<(String, String)>, errors: Vec<String>) {
        let current = (self.cfg.agent.provider.clone(), self.cfg.agent.model.clone());
        let keyless: Vec<String> = self
            .cfg
            .providers
            .iter()
            .filter(|(_, p)| p.kind == ProviderKind::Anthropic && p.resolved_key().is_none())
            .map(|(n, _)| n.clone())
            .collect();
        let Some(Overlay::Picker(p)) = &mut self.ui.overlay else { return };
        if p.kind != PickerKind::Model {
            return;
        }
        p.loading = false;
        p.items = result
            .iter()
            .map(|(provider, model)| PickItem {
                label: model.clone(),
                detail: if keyless.contains(provider) { format!("{provider} · needs an API key") } else { provider.clone() },
                value: format!("{provider}\u{1f}{model}"),
            })
            .collect();
        p.selected = result.iter().position(|(pr, m)| *pr == current.0 && *m == current.1).unwrap_or(0);
        p.note = if p.items.is_empty() {
            if errors.is_empty() {
                "No models found. Start Ollama, or set OPENAI_API_KEY / ANTHROPIC_API_KEY, or add a provider to config.toml.".into()
            } else {
                format!("No models found ({})", errors.join("; "))
            }
        } else if errors.is_empty() {
            "Type to filter · Enter to select · Esc to cancel".into()
        } else {
            format!("Type to filter · unavailable: {}", errors.join("; "))
        };
        self.mark_dirty();
    }

    // ---- prompt input -------------------------------------------------------

    pub(super) fn agent_paste(&mut self, text: &str) {
        let Some(tab) = self.tab_mut() else { return };
        tab.agent.last_typed = Some(Instant::now());
        if tab.agent.approval.is_some() && tab.agent.mode == InputMode::Prompt {
            return;
        }
        tab.agent.input.insert(&text.replace("\r\n", "\n").replace('\r', "\n"));
    }

    pub(super) fn agent_key_input(&mut self, key: KeyEvent) {
        let idx = self.active;
        let Some(tab) = self.tabs.get_mut(idx) else { return };
        let a = &mut tab.agent;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);

        // An approval card is waiting for an answer.
        if a.approval.is_some() && a.mode == InputMode::Prompt {
            if a.approval.as_ref().and_then(|ap| ap.guard_until).is_some_and(|until| Instant::now() < until) {
                self.toast("The agent is asking you something · your typing was not taken as an answer");
                return;
            }
            let count = a.approval.as_ref().map(|ap| ap.choices.len()).unwrap_or(1);
            match key.code {
                KeyCode::Char('c') if ctrl => self.cancel_turn(idx, "Stopped."),
                KeyCode::Up | KeyCode::BackTab | KeyCode::Left => {
                    if let Some(ap) = &mut a.approval {
                        ap.selected = (ap.selected + count - 1) % count;
                    }
                }
                KeyCode::Down | KeyCode::Tab | KeyCode::Right => {
                    if let Some(ap) = &mut a.approval {
                        ap.selected = (ap.selected + 1) % count;
                    }
                }
                KeyCode::Enter => {
                    let sel = a.approval.as_ref().map(|ap| ap.selected).unwrap_or(0);
                    self.choose_approval(sel);
                }
                KeyCode::Esc => self.decline(idx, ""),
                KeyCode::Char(c) => {
                    let c = c.to_ascii_lowercase();
                    let pos = a.approval.as_ref().and_then(|ap| ap.choices.iter().position(|(ch, _)| ch.key() == c));
                    if let Some(pos) = pos {
                        self.choose_approval(pos);
                    }
                }
                // A long diff scrolls inside its card; otherwise the transcript does.
                KeyCode::PageUp | KeyCode::PageDown => match a.approval.as_mut().filter(|ap| ap.edit.is_some()) {
                    Some(ap) if key.code == KeyCode::PageUp => ap.scroll = ap.scroll.saturating_sub(5),
                    Some(ap) => ap.scroll = (ap.scroll + 5).min(ap.preview.len().saturating_sub(1)),
                    None if key.code == KeyCode::PageUp => a.scroll += 5,
                    None => a.scroll = a.scroll.saturating_sub(5),
                },
                _ => {}
            }
            return;
        }

        let menu = a.slash_matches();
        match key.code {
            KeyCode::Enter if shift || alt => a.input.insert("\n"),
            KeyCode::Char('j') if ctrl => a.input.insert("\n"),
            KeyCode::Enter => self.agent_submit(),
            KeyCode::Esc => match a.mode {
                InputMode::EditCommand => {
                    a.mode = InputMode::Prompt;
                    a.input.clear();
                }
                InputMode::Guidance => self.decline(idx, ""),
                InputMode::Secret => {
                    a.mode = InputMode::Prompt;
                    a.input.clear();
                }
                InputMode::Prompt => {
                    if a.busy {
                        self.cancel_turn(idx, "Stopped.");
                    } else if !a.input.is_empty() {
                        a.input.clear();
                    } else {
                        self.hide_panel();
                    }
                }
            },
            KeyCode::Char('c') if ctrl => {
                if a.mode == InputMode::Secret {
                    a.leave_secret();
                } else if a.busy {
                    self.cancel_turn(idx, "Stopped.");
                } else if !a.input.is_empty() {
                    a.input.clear();
                    a.mode = InputMode::Prompt;
                } else {
                    self.hide_panel();
                }
            }
            KeyCode::Char('d') if ctrl && a.input.is_empty() => self.hide_panel(),
            KeyCode::BackTab => self.cycle_autonomy(),
            KeyCode::Tab => {
                if let Some((name, _)) = menu.get(a.slash_sel.min(menu.len().saturating_sub(1))) {
                    a.input.set(&format!("{name} "));
                }
            }
            // A key being typed is not mixed up with the prompt history.
            KeyCode::Up | KeyCode::Down if a.mode == InputMode::Secret => {}
            KeyCode::Up if !menu.is_empty() => a.slash_sel = (a.slash_sel + menu.len() - 1) % menu.len(),
            KeyCode::Down if !menu.is_empty() => a.slash_sel = (a.slash_sel + 1) % menu.len(),
            KeyCode::Up if shift => a.scroll += 1,
            KeyCode::Down if shift => a.scroll = a.scroll.saturating_sub(1),
            KeyCode::Up => {
                if !a.input.vertical(false) && !a.prompts.is_empty() {
                    let next = match a.prompt_idx {
                        None => {
                            a.draft = a.input.text.clone();
                            a.prompts.len() - 1
                        }
                        Some(i) => i.saturating_sub(1),
                    };
                    a.prompt_idx = Some(next);
                    let text = a.prompts[next].clone();
                    a.input.set(&text);
                }
            }
            KeyCode::Down => {
                if !a.input.vertical(true) {
                    if let Some(i) = a.prompt_idx {
                        if i + 1 < a.prompts.len() {
                            a.prompt_idx = Some(i + 1);
                            let text = a.prompts[i + 1].clone();
                            a.input.set(&text);
                        } else {
                            a.prompt_idx = None;
                            let draft = std::mem::take(&mut a.draft);
                            a.input.set(&draft);
                        }
                    }
                }
            }
            KeyCode::PageUp => a.scroll += 8,
            KeyCode::PageDown => a.scroll = a.scroll.saturating_sub(8),
            KeyCode::Left if ctrl || alt => a.input.word_left(),
            KeyCode::Right if ctrl || alt => a.input.word_right(),
            KeyCode::Left => a.input.left(),
            KeyCode::Right => a.input.right(),
            KeyCode::Home => a.input.home(),
            KeyCode::End => a.input.end(),
            // Take the last waiting message back to change it.
            KeyCode::Backspace if a.input.is_empty() && !a.queued.is_empty() => {
                let text = a.queued.pop().unwrap_or_default();
                a.input.set(&text);
            }
            KeyCode::Backspace if alt || ctrl => a.input.kill_word(),
            KeyCode::Backspace => a.input.backspace(),
            KeyCode::Delete => a.input.delete(),
            KeyCode::Char('a') if ctrl => a.input.home(),
            KeyCode::Char('e') if ctrl => a.input.end(),
            KeyCode::Char('u') if ctrl => a.input.kill_to_start(),
            KeyCode::Char('k') if ctrl => a.input.kill_to_end(),
            KeyCode::Char('w') if ctrl => a.input.kill_word(),
            KeyCode::Char('b') if alt => a.input.word_left(),
            KeyCode::Char('f') if alt => a.input.word_right(),
            KeyCode::Char(c) if !ctrl && !alt => {
                a.last_typed = Some(Instant::now());
                let mut buf = [0u8; 4];
                a.input.insert(c.encode_utf8(&mut buf));
                a.slash_sel = 0;
                a.prompt_idx = None;
            }
            _ => {}
        }
    }

    pub(super) fn agent_submit(&mut self) {
        let idx = self.active;
        let Some(tab) = self.tabs.get_mut(idx) else { return };
        let a = &mut tab.agent;
        // Enter ends a stretch of typing.
        a.last_typed = None;
        match a.mode {
            InputMode::EditCommand => {
                let cmd = a.input.take().trim().to_string();
                a.mode = InputMode::Prompt;
                if cmd.is_empty() {
                    return;
                }
                if let Some(ap) = a.approval.take() {
                    let note = "note: the user edited the command before approving it; what actually ran is shown below.\n".to_string();
                    if let Some(item) = a.tool_mut(&ap.call.id) {
                        item.target = cmd.clone();
                    }
                    self.run_command(idx, ap.call, ap.reply, cmd, note);
                }
                return;
            }
            InputMode::Guidance => {
                let text = a.input.take();
                self.decline(idx, text.trim());
                return;
            }
            InputMode::Secret => {
                let key = a.input.take().trim().to_string();
                let provider = std::mem::take(&mut a.secret_for);
                a.mode = InputMode::Prompt;
                if !key.is_empty() {
                    self.store_key(idx, provider, Some(key));
                }
                return;
            }
            InputMode::Prompt => {}
        }

        let menu = a.slash_matches();
        let text = if let Some((name, _)) = menu.get(a.slash_sel.min(menu.len().saturating_sub(1))) {
            a.input.clear();
            name.to_string()
        } else {
            a.input.take().trim().to_string()
        };
        a.slash_sel = 0;
        a.prompt_idx = None;
        if text.is_empty() {
            return;
        }
        if let Some(rest) = text.strip_prefix('/') {
            let (cmd, arg) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
            self.slash(idx, cmd, arg.trim());
            return;
        }
        if let Some(cmd) = text.strip_prefix('!') {
            // `!command` runs it in the shell directly, no model involved.
            let cmd = cmd.trim().to_string();
            if !cmd.is_empty() {
                let pane = &mut tab.panes.agent_mut();
                pane.scroll_to_bottom();
                let mut bytes = input::encode_paste(&cmd, pane.mode());
                bytes.push(b'\r');
                pane.write(bytes);
                tab.agent.prompts.push(text);
            }
            return;
        }
        if a.busy {
            // Typed while the agent works: it gets this at its next step.
            if a.prompts.last() != Some(&text) {
                a.prompts.push(text.clone());
            }
            a.queued.push(text);
            a.scroll = 0;
            return;
        }
        self.start_turn(idx, text);
    }

    /// A question typed at the shell prompt: abandon the line there and put
    /// it to the agent, leaving the keyboard with the shell.
    pub(super) fn ask_from_shell(&mut self, idx: usize, question: String) {
        let key = self.agent_key.label();
        let tab = &mut self.tabs[idx];
        // Take the line back off the shell's prompt with its own editing
        // keys: Ctrl+E to the end of the line if the cursor is not there,
        // then Ctrl+U, which every shell's line editor takes to mean "erase
        // what is before the cursor". Not Ctrl+C: an interrupt arriving
        // while the shell is still digesting the keys just typed can leave
        // an old bash stuck for good.
        // (The line is in the pane the user is typing in, wherever the agent is working.)
        let keys = tab.panes.focused().erase_line_keys();
        tab.panes.focused_mut().write(keys);
        tab.panes.focused_mut().line_abandoned();
        tab.agent.visible = true;
        if tab.agent.busy {
            tab.agent.queued.push(question);
        } else {
            self.start_turn(idx, question);
        }
        self.toast(format!("Asked the agent · {key} to go to it"));
    }

    /// Put messages that were waiting for the agent back in the prompt, so
    /// a turn that ends early does not lose them.
    fn unqueue(&mut self, idx: usize) {
        let a = &mut self.tabs[idx].agent;
        if a.queued.is_empty() {
            return;
        }
        a.leave_secret();
        let mut text = std::mem::take(&mut a.queued).join("\n");
        if !a.input.is_empty() {
            text = format!("{text}\n{}", a.input.text);
        }
        a.input.set(&text);
    }

    fn notice(&mut self, idx: usize, text: impl Into<String>) {
        let a = &mut self.tabs[idx].agent;
        a.items.push(Item::Notice(text.into()));
        a.scroll = 0;
        self.mark_dirty();
    }

    fn slash(&mut self, idx: usize, cmd: &str, arg: &str) {
        match cmd {
            "model" | "models" | "provider" => {
                if arg.is_empty() {
                    self.open_model_picker();
                } else if let Some((provider, model)) = arg.split_once('/').filter(|(p, _)| self.cfg.providers.contains_key(*p)) {
                    let (provider, model) = (provider.to_string(), model.to_string());
                    self.set_model(&provider, &model);
                } else if !self.cfg.agent.provider.is_empty() {
                    let provider = self.cfg.agent.provider.clone();
                    self.set_model(&provider, arg);
                } else {
                    self.open_model_picker();
                }
            }
            "theme" | "themes" => {
                if arg.is_empty() {
                    self.open_theme_picker();
                } else {
                    match theme::find(&self.themes, arg) {
                        Some(t) => self.set_theme(t),
                        None => {
                            let names: Vec<&str> = self.themes.iter().map(|t| t.name.as_ref()).collect();
                            self.notice(idx, format!("No theme called `{arg}`. Available: {}", names.join(", ")));
                        }
                    }
                }
            }
            "autonomy" | "mode" => match Autonomy::parse(arg) {
                Some(a) => self.set_autonomy(a),
                None if arg.is_empty() => self.cycle_autonomy(),
                None => self.notice(idx, "Use /autonomy ask, /autonomy safe, or /autonomy auto."),
            },
            "ask" => self.set_autonomy(Autonomy::Ask),
            "safe" => self.set_autonomy(Autonomy::Safe),
            "auto" | "autopilot" => self.set_autonomy(Autonomy::Auto),
            "clear" | "new" | "reset" => {
                self.tabs[idx].agent.queued.clear();
                self.cancel_turn(idx, "");
                let tab = &mut self.tabs[idx];
                let a = &mut tab.agent;
                a.items.clear();
                a.history.clear();
                a.totals = Default::default();
                a.session_auto = false;
                tab.panes.agent_mut().seen_seq = tab.panes.agent().block_seq;
                a.scroll = 0;
                // The old conversation stays on disk; the next prompt starts a new one.
                a.saved = None;
                a.save_failed = false;
                a.rules.clear();
            }
            "usage" | "tokens" => self.show_usage(idx),
            "explain" => self.explain_last(idx, false),
            "fix" => self.explain_last(idx, true),
            "watch" | "attach" | "follow" => self.watch_command(idx, arg),
            "summary" => {
                let text = match crate::agent::compact::summary_in(&self.tabs[idx].agent.history) {
                    Some(summary) => format!("**The summary this conversation continues from:**\n\n{summary}"),
                    None => "This conversation has not been compacted, so there is no summary: the agent still has every message.".to_string(),
                };
                self.notice(idx, text);
            }
            "compact" | "summarize" => {
                let a = &self.tabs[idx].agent;
                if a.busy {
                    self.toast("The agent is still working · Esc to stop it");
                } else if crate::agent::compact::split(&a.history).is_none() {
                    self.notice(idx, "There is not enough conversation to compact yet.");
                } else {
                    self.launch_turn(idx, Some(arg.to_string()));
                }
            }
            "resume" | "continue" => self.resume(idx, arg),
            "conversations" | "history" => self.open_conversation_picker(idx),
            "copy" => {
                let last = self.tabs[idx].agent.items.iter().rev().find_map(|i| match i {
                    Item::Assistant(t) if !t.trim().is_empty() => Some(t.clone()),
                    _ => None,
                });
                match last {
                    Some(text) => {
                        self.copy_to_clipboard(text);
                        self.toast("Copied the last reply");
                    }
                    None => self.toast("Nothing to copy yet"),
                }
            }
            "providers" => {
                let mut lines = vec!["Configured providers:".to_string()];
                for (name, p) in &self.cfg.providers {
                    let kind = match p.kind {
                        ProviderKind::Openai => "OpenAI-compatible",
                        ProviderKind::Anthropic => "Anthropic",
                        ProviderKind::Bedrock => "Amazon Bedrock",
                    };
                    let local = p.base_url.as_deref().is_some_and(|u| u.contains("localhost") || u.contains("127.0.0.1"));
                    if p.kind == ProviderKind::Bedrock {
                        let auth = if p.resolved_key().is_some() {
                            "API key set"
                        } else if crate::agent::aws::has_any_credentials(p) {
                            "AWS credentials from your environment or ~/.aws"
                        } else {
                            "no AWS credentials found"
                        };
                        let current = if *name == self.cfg.agent.provider { "  ← in use" } else { "" };
                        lines.push(format!("- `{name}` · {kind} · region {} · {auth}{current}", crate::agent::aws::region(p)));
                        continue;
                    }
                    let key = match (p.resolved_key().is_some(), &p.api_key_env) {
                        (true, _) => "key set".to_string(),
                        (false, _) if local => "local, no key needed".to_string(),
                        (false, Some(env)) => format!("no key (${env} is unset)"),
                        (false, None) => "no key".to_string(),
                    };
                    let url = p.base_url.clone().unwrap_or_default();
                    let current = if *name == self.cfg.agent.provider { "  ← in use" } else { "" };
                    lines.push(format!("- `{name}` · {kind} · {url} · {key}{current}"));
                }
                lines.push(format!("Edit `{}` to add or change providers.", paths::tildify(&paths::config_file().to_string_lossy())));
                self.notice(idx, lines.join("\n"));
            }
            "key" | "keys" => self.key_command(idx, arg),
            "reload" => {
                let report = self.reload_providers(None);
                self.notice(idx, report);
            }
            "rename" => {
                let tab = &mut self.tabs[idx];
                tab.name = if arg.is_empty() { None } else { Some(arg.to_string()) };
            }
            "config" => {
                let path = paths::config_file();
                let mut msg = format!("Config file: `{}`", paths::tildify(&path.to_string_lossy()));
                if !path.exists() {
                    let _ = std::fs::create_dir_all(paths::config_dir());
                    match std::fs::write(&path, crate::config::SAMPLE) {
                        Ok(()) => msg.push_str("\nIt did not exist, so a commented starter file was written there. Changes apply the next time the session starts (`astrum kill`, then `astrum`)."),
                        Err(e) => msg.push_str(&format!("\nCould not create it: {e}")),
                    }
                } else {
                    msg.push_str("\nChanges apply the next time the session starts (`astrum kill`, then `astrum`).");
                }
                self.notice(idx, msg);
            }
            "help" | "?" => self.ui.overlay = Some(Overlay::Help),
            other => self.notice(idx, format!("Unknown command `/{other}`. Type `/` to see what is available.")),
        }
    }

    // ---- provider keys and reloading -----------------------------------------

    /// Read each provider's key from the system's password store, off the
    /// server loop: the store can be slow, or stop to ask the user something.
    pub(super) fn load_stored_keys(&self) {
        let providers: Vec<String> = self.cfg.providers.keys().cloned().collect();
        let tx = self.tx.clone();
        tokio::task::spawn_blocking(move || {
            let keys = providers.into_iter().map(|p| {
                let key = crate::keychain::get(&p);
                (p, key)
            });
            let _ = tx.blocking_send(Ev::KeysLoaded(keys.collect()));
        });
    }

    fn key_source_text(p: &crate::config::ProviderConfig) -> String {
        let local = p.base_url.as_deref().is_some_and(|u| u.contains("localhost") || u.contains("127.0.0.1"));
        match p.key().1 {
            KeySource::ConfigFile => "key set in the config file".into(),
            KeySource::Keychain => "key from the password store".into(),
            KeySource::Environment(var) => format!("key from ${var}"),
            KeySource::None if p.kind == ProviderKind::Bedrock => "no API key (AWS credentials are used instead)".into(),
            KeySource::None if local => "no key (local, none needed)".into(),
            KeySource::None => match &p.api_key_env {
                Some(var) => format!("no key (${var} is unset)"),
                None => "no key".into(),
            },
        }
    }

    /// `/key`: show where each provider's key comes from; `/key name` asks
    /// for one to store; `/key name remove` forgets the stored one.
    fn key_command(&mut self, idx: usize, arg: &str) {
        let store = crate::keychain::name();
        let mut words = arg.split_whitespace();
        let Some(provider) = words.next() else {
            let mut lines = vec!["Where each provider's API key comes from:".to_string()];
            lines.extend(self.cfg.providers.iter().map(|(name, p)| format!("- `{name}` · {}", Self::key_source_text(p))));
            lines.push(match &store {
                Ok(store) => format!("`/key name` stores a key in {store}, where it takes the place of the environment variable. `/key name remove` forgets it."),
                Err(why) => format!("Keys cannot be stored here: {why}."),
            });
            self.notice(idx, lines.join("\n"));
            return;
        };
        // Nothing typed after /key is repeated back: it may be a key.
        if !self.cfg.providers.contains_key(provider) {
            let names: Vec<&str> = self.cfg.providers.keys().map(String::as_str).collect();
            self.notice(idx, format!("`/key` takes the name of a provider, and that is not one of yours: {}.", names.join(", ")));
            return;
        }
        if let Err(why) = &store {
            self.notice(idx, format!("Keys cannot be stored here: {why}."));
            return;
        }
        match words.next() {
            Some("remove" | "delete" | "forget" | "rm") => self.store_key(idx, provider.to_string(), None),
            // A key given on the line itself: store it. It was visible while
            // being typed, which `/key name` alone avoids.
            Some(key) => {
                self.store_key(idx, provider.to_string(), Some(key.to_string()));
                self.toast(format!("Next time, `/key {provider}` alone hides the key while you type it"));
            }
            None => {
                let a = &mut self.tabs[idx].agent;
                a.input.clear();
                a.secret_for = provider.to_string();
                a.mode = InputMode::Secret;
                self.toast(format!("Type or paste the key for {provider} · it is shown as dots"));
            }
        }
    }

    /// Store a key for a provider (`Some`), or remove the stored one.
    fn store_key(&mut self, idx: usize, provider: String, key: Option<String>) {
        let (tab, tx) = (self.tabs[idx].id, self.tx.clone());
        tokio::task::spawn_blocking(move || {
            let result = match &key {
                Some(key) => crate::keychain::set(&provider, key).map(|()| Some(key.clone())),
                None => crate::keychain::delete(&provider).map(|()| None),
            };
            let _ = tx.blocking_send(Ev::KeyChanged { tab, provider, result });
        });
    }

    pub(super) fn on_key_changed(&mut self, tab: u64, provider: String, result: Result<Option<String>, String>) {
        let store = crate::keychain::name().unwrap_or("the password store");
        let text = match result {
            Ok(key) => {
                let stored = key.is_some();
                if let Some(p) = self.cfg.providers.get_mut(&provider) {
                    p.stored_key = key;
                }
                // What the provider said about its models may differ with another key.
                self.windows.retain(|(name, _), _| *name != provider);
                let now = self.cfg.providers.get(&provider).map(Self::key_source_text).unwrap_or_default();
                if stored {
                    format!("The key for `{provider}` is stored in {store} and in use from now on.")
                } else {
                    format!("The stored key for `{provider}` is removed. Now: {now}.")
                }
            }
            Err(why) => format!("Could not change the key for `{provider}`: {why}."),
        };
        match self.tab_index(tab) {
            Some(idx) => self.notice(idx, text),
            None => self.toast(text),
        }
    }

    /// Read the providers and model settings again from the config file,
    /// and the stored keys from the password store. `env`, from
    /// `astrum reload`, is what each key variable holds in the shell the
    /// request came from. Returns a report for the user.
    /// Take the config file as it is now: every setting in it, in the
    /// running session. A file that cannot be read changes nothing, and the
    /// reason comes back. Otherwise, what was odd about it but did not stop
    /// it (a key that is not a key, a color that is not a color).
    pub(super) fn reload_config(&mut self) -> Result<Vec<String>, String> {
        let (mut fresh, warnings) = crate::config::Config::load();
        if let Some(problem) = warnings.first() {
            return Err(problem.clone());
        }
        // What is only known while running stays: the keys found in the
        // password store and in the shells `astrum reload` was run from.
        for (name, p) in fresh.providers.iter_mut() {
            if let Some(before) = self.cfg.providers.get(name) {
                p.stored_key = before.stored_key.clone();
                p.env_key = before.env_key.clone();
            }
        }
        // A session put on autopilot stays on it; that is never written down.
        if self.cfg.agent.autonomy == Autonomy::Auto {
            fresh.agent.autonomy = Autonomy::Auto;
        }
        self.cfg = fresh;

        let mut notes = Vec::new();
        let (themes, odd) = theme::all_with(&self.cfg.themes);
        self.themes = themes;
        notes.extend(odd);
        let (keymap, odd) = crate::keys::Keymap::new(&self.cfg.keys);
        self.keymap = keymap;
        notes.extend(odd);
        use crate::config::Chord;
        use crossterm::event::{KeyCode, KeyModifiers};
        self.prefix = Chord::parse(&self.cfg.prefix).unwrap_or(Chord { code: KeyCode::Char('b'), mods: KeyModifiers::CONTROL });
        self.agent_key = Chord::parse(&self.cfg.agent_key).unwrap_or(Chord { code: KeyCode::Char(' '), mods: KeyModifiers::CONTROL });
        self.retheme();
        self.windows.clear();
        self.load_stored_keys();
        self.force_redraw();
        Ok(notes)
    }

    pub(super) fn reload_providers(&mut self, env: Option<Vec<(String, String)>>) -> String {
        if let Err(problem) = self.reload_config() {
            return format!("Nothing was changed: the config file has a problem. {problem}");
        }
        for p in self.cfg.providers.values_mut() {
            if let (Some(env), Some(var)) = (&env, &p.api_key_env) {
                if let Some((_, value)) = env.iter().find(|(name, _)| name == var) {
                    p.env_key = Some(value.clone());
                }
            }
        }
        self.mark_dirty();

        let mut lines = vec![format!("Reloaded {} providers:", self.cfg.providers.len())];
        lines.extend(self.cfg.providers.iter().map(|(name, p)| format!("- `{name}` · {}", Self::key_source_text(p))));
        let current = &self.cfg.agent.provider;
        if !current.is_empty() && !self.cfg.providers.contains_key(current) {
            lines.push(format!("The provider in use, `{current}`, is no longer configured: pick a model with `/model`."));
        }
        if env.is_none() {
            lines.push("Environment variables are as they were when the session started; run `astrum reload` in a shell to pass along new ones.".into());
        }
        lines.join("\n")
    }

    /// The secrets Astrum holds itself (its providers' keys, AWS credentials
    /// from the environment), for removing from what goes to the model;
    /// `None` when the user has turned redaction off.
    fn known_secrets(&self) -> Option<Vec<String>> {
        if !self.cfg.agent.redact_secrets {
            return None;
        }
        let mut known: Vec<String> = self.cfg.providers.values().filter_map(|p| p.resolved_key()).collect();
        known.extend(["AWS_SECRET_ACCESS_KEY", "AWS_SESSION_TOKEN"].iter().filter_map(|name| std::env::var(name).ok()));
        Some(known)
    }

    /// Text collected for the model, without the secrets in it.
    fn scrubbed(&self, text: &str) -> String {
        match self.known_secrets() {
            Some(known) => crate::agent::redact::scrub(text, &known).0,
            None => text.to_string(),
        }
    }

    /// The context window to assume for a model, and whether the provider
    /// should still be asked: the config file's figure, else what the
    /// provider said earlier, else a safe default until it has been asked.
    fn window_for(&self, provider_name: &str, provider: &crate::config::ProviderConfig, model: &str) -> (u64, bool) {
        if let Some(window) = models::configured_window(&self.cfg, provider_name, model) {
            return (window, false);
        }
        let default = models::default_window(provider);
        match self.windows.get(&(provider_name.to_string(), model.to_string())) {
            Some(found) => (found.unwrap_or(default), false),
            None => (default, true),
        }
    }

    /// `/usage`: the tokens this conversation has used so far.
    fn show_usage(&mut self, idx: usize) {
        let provider_name = self.cfg.agent.provider.clone();
        let model = self.cfg.agent.model.clone();
        let window = self.cfg.providers.get(&provider_name).map(|p| self.window_for(&provider_name, p, &model).0);
        let t = self.tabs[idx].agent.totals;
        if t.requests == 0 {
            self.notice(idx, "Nothing has been sent to a model in this conversation yet.");
            return;
        }
        let n = models::tokens;
        let mut lines = vec![format!("**This conversation** · `{model}` ({provider_name})")];
        match window.filter(|_| t.context > 0) {
            Some(w) => lines.push(format!("- Context: {} of {} tokens ({}%)", n(t.context), n(w), (t.context * 100).checked_div(w).unwrap_or(0))),
            None if t.context > 0 => lines.push(format!("- Context: {} tokens", n(t.context))),
            None => lines.push("- Context: just compacted; the next reply will say how large it is now".into()),
        }
        lines.push(format!("- Input: {} tokens over {} request{}", n(t.input), t.requests, if t.requests == 1 { "" } else { "s" }));
        lines.push(format!("- Cached: {} of that input read from the provider's cache, {} written to it", n(t.cache_read), n(t.cache_write)));
        lines.push(format!("- Output: {} tokens", n(t.output)));
        lines.push(format!("- Total: {} tokens", n(t.total())));
        self.notice(idx, lines.join("\n"));
    }

    // ---- saved conversations ------------------------------------------------

    /// Write the tab's conversation to disk, if there is one and saving is on.
    pub(super) fn save_conversation(&mut self, idx: usize) {
        if !self.cfg.agent.save_conversations {
            return;
        }
        let Some(tab) = self.tabs.get_mut(idx) else { return };
        let a = &mut tab.agent;
        if a.history.is_empty() {
            return;
        }
        let first_save = a.saved.is_none();
        let saved = a.saved.get_or_insert_with(|| Saved { id: store::new_id(), created: store::now() });
        let mut meta = store::new_meta(&saved.id, saved.created);
        let mut prompts = a.items.iter().filter_map(|i| match i {
            Item::User(t) => Some(t),
            _ => None,
        });
        meta.title = prompts.next().map(|t| first_line(t, 80)).unwrap_or_default();
        meta.prompts = 1 + prompts.count();
        meta.cwd = tab.panes.agent_mut().cwd.clone();
        meta.session = self.session.clone();
        meta.model = self.cfg.agent.model.clone();
        meta.totals = a.totals;
        if let Err(e) = store::save(&meta, &a.history, &a.items) {
            if !a.save_failed {
                a.save_failed = true;
                self.toast(format!("Could not save this conversation: {}", first_line(&format!("{e:#}"), 80)));
            }
        }
        // A long-running server clears out old conversations as new ones begin.
        let keep_days = self.cfg.agent.keep_conversations_days;
        if first_save && keep_days > 0 {
            let open: Vec<String> = self.open_conversation_ids().into_iter().map(|(_, id)| id).collect();
            tokio::task::spawn_blocking(move || store::prune(keep_days, &open));
        }
    }

    fn open_conversation_ids(&self) -> Vec<(usize, String)> {
        self.tabs.iter().enumerate().filter_map(|(i, t)| t.agent.saved.as_ref().map(|s| (i, s.id.clone()))).collect()
    }

    /// `/resume`: reopen the most recent conversation from the directory the
    /// shell is in, or failing that the most recent from anywhere. With
    /// `arg`, only conversations matching it are considered. Ones already
    /// open in a tab are skipped.
    fn resume(&mut self, idx: usize, arg: &str) {
        let open = self.open_conversation_ids();
        let needle = arg.to_lowercase();
        let here = self.tabs[idx].panes.agent_mut().cwd.clone();
        let all = store::list();
        let candidates: Vec<&store::Meta> = all
            .iter()
            .filter(|m| !open.iter().any(|(_, id)| *id == m.id))
            .filter(|m| needle.is_empty() || m.id.starts_with(&needle) || m.title.to_lowercase().contains(&needle))
            .collect();
        // The same directory can be spelled two ways (a symlinked home or
        // temporary directory), so compare where the paths really lead.
        let real = |p: &str| std::fs::canonicalize(p).unwrap_or_else(|_| p.into());
        let here_real = here.as_deref().map(real);
        let local = candidates.iter().find(|m| here_real.is_some() && m.cwd.as_deref().map(real) == here_real);
        match local.or(candidates.first()) {
            Some(m) => {
                let (id, elsewhere) = (m.id.clone(), local.is_none());
                if self.load_conversation(idx, &id) && elsewhere {
                    if let Some(here) = here {
                        let what = if needle.is_empty() { "No conversation was saved".to_string() } else { format!("No conversation matching `{arg}` was saved") };
                        self.notice(idx, format!("{what} from `{}`, so this is the most recent one from elsewhere.", paths::tildify(&here)));
                    }
                }
            }
            None if all.is_empty() => self.notice(idx, "No saved conversations yet."),
            None if needle.is_empty() => self.notice(idx, "No other saved conversation to resume. `/conversations` lists them all."),
            None => self.notice(idx, format!("No saved conversation matches `{arg}`. `/conversations` lists them all.")),
        }
    }

    pub(super) fn open_conversation_picker(&mut self, idx: usize) {
        let now = store::now();
        let open = self.open_conversation_ids();
        let items: Vec<PickItem> = store::list()
            .into_iter()
            .map(|m| {
                let mut detail = match open.iter().find(|(_, id)| *id == m.id) {
                    Some((tab, _)) => format!("open in tab {}", tab + 1),
                    None => store::ago(m.updated, now),
                };
                if let Some(cwd) = &m.cwd {
                    detail.push_str(&format!(" · {}", paths::tildify(cwd)));
                }
                PickItem { label: if m.title.is_empty() { "(no prompt)".into() } else { m.title }, detail, value: m.id }
            })
            .collect();
        if items.is_empty() {
            self.notice(idx, "No saved conversations yet.");
            return;
        }
        self.ui.overlay = Some(Overlay::Picker(Picker {
            kind: PickerKind::Conversation,
            title: "Conversations".into(),
            items,
            filter: String::new(),
            selected: 0,
            loading: false,
            note: super::CONVERSATIONS_NOTE.into(),
            original_theme: None,
            pending_delete: None,
        }));
        self.mark_dirty();
    }

    /// Replace the tab's conversation with a saved one. False if it could
    /// not be opened (the panel says why).
    pub(super) fn load_conversation(&mut self, idx: usize, id: &str) -> bool {
        if let Some((other, _)) = self.open_conversation_ids().into_iter().find(|(_, open)| open == id) {
            let msg = if other == idx { "That conversation is the one open here.".to_string() } else { format!("That conversation is open in tab {}.", other + 1) };
            self.notice(idx, msg);
            return false;
        }
        let (meta, body) = match store::load(id) {
            Ok(found) => found,
            Err(e) => {
                self.notice(idx, format!("Could not open that conversation: {e:#}"));
                return false;
            }
        };
        // The conversation being replaced is already on disk.
        self.cancel_turn(idx, "");
        let active = self.shown(idx);
        let tab = &mut self.tabs[idx];
        let a = &mut tab.agent;
        a.items = body.items;
        a.history = body.history;
        // Whatever was in flight when it was saved never finished.
        for item in a.items.iter_mut() {
            if let Item::Tool(t) = item {
                match t.state {
                    ToolState::Proposed => {
                        t.state = ToolState::Declined;
                        t.summary = "not run: the session ended first".into();
                    }
                    ToolState::Running => {
                        t.state = ToolState::Failed;
                        t.summary = "interrupted: the session ended".into();
                    }
                    _ => {}
                }
            }
        }
        a.repair_history_with("Interrupted: the session ended before this completed.");
        a.totals = meta.totals;
        a.saved = Some(Saved { id: meta.id.clone(), created: meta.created });
        a.save_failed = false;
        a.rules.clear();
        a.session_auto = false;
        a.detached = false;
        a.last_finished = None;
        tab.panes.agent_mut().seen_seq = tab.panes.agent().block_seq;
        a.scroll = 0;
        a.prompt_idx = None;
        for item in &a.items {
            if let Item::User(text) = item {
                if a.prompts.last() != Some(text) {
                    a.prompts.push(text.clone());
                }
            }
        }
        let place = meta.cwd.as_deref().map(|c| format!(", in `{}`", paths::tildify(c))).unwrap_or_default();
        a.items.push(Item::Notice(format!("Resumed this conversation (last active {}{place}).", store::ago(meta.updated, store::now()))));
        a.visible = true;
        if active {
            tab.focus = Focus::Agent;
        }
        self.mark_dirty();
        true
    }

    // ---- turns --------------------------------------------------------------

    fn build_context(&self, idx: usize) -> String {
        let tab = &self.tabs[idx];
        let pane = &tab.panes.agent();
        let mut s = String::from("<context>\n");
        s.push_str(&format!("cwd: {}\n", pane.cwd.as_deref().unwrap_or("unknown")));
        s.push_str(&format!("time: {}\n", local_time()));
        let state = match &pane.state {
            ShellState::Prompt => "idle at a shell prompt".to_string(),
            ShellState::Running { cmd, since } => format!(
                "busy running `{}` (started {}s ago by the user)",
                cmd.as_deref().unwrap_or("a command"),
                since.elapsed().as_secs()
            ),
            ShellState::Unknown => "unknown (shell integration is not active, so commands will run in a separate background shell)".to_string(),
        };
        s.push_str(&format!("terminal: {state}\n"));
        let fresh = (pane.block_seq - pane.seen_seq.min(pane.block_seq)) as usize;
        let take = fresh.min(5).min(pane.blocks.len());
        if take > 0 {
            s.push_str("commands the user ran since your last turn (oldest first):\n");
            for b in pane.blocks.iter().skip(pane.blocks.len() - take) {
                let exit = b.exit.map(|e| e.to_string()).unwrap_or_else(|| "?".into());
                s.push_str(&format!("$ {}\n[exit {exit}]\n", b.cmd));
                let out = osc::truncate_lines(&b.output, 12, 12);
                if !out.trim().is_empty() {
                    s.push_str(&out);
                    s.push('\n');
                }
            }
        }
        s.push_str("</context>");
        self.scrubbed(&s)
    }

    fn start_turn(&mut self, idx: usize, text: String) {
        if self.cfg.providers.get(&self.cfg.agent.provider).filter(|_| !self.cfg.agent.model.is_empty()).is_none() {
            self.tabs[idx].agent.input.set(&text);
            self.open_model_picker();
            self.toast("Pick a model first, then press Enter again to send");
            return;
        }
        {
            let a = &mut self.tabs[idx].agent;
            if a.prompts.last() != Some(&text) {
                a.prompts.push(text.clone());
            }
        }
        self.start_turn_as(idx, text.clone(), text);
    }

    /// Start a turn whose message to the model (`sent`) says more than the
    /// line shown for it in the panel (`shown`).
    fn start_turn_as(&mut self, idx: usize, shown: String, sent: String) {
        if self.cfg.providers.get(&self.cfg.agent.provider).filter(|_| !self.cfg.agent.model.is_empty()).is_none() {
            self.open_model_picker();
            self.toast("Pick a model first, then try again");
            return;
        }
        let context = self.build_context(idx);
        let a = &mut self.tabs[idx].agent;
        a.history.push(Msg::User(format!("{context}\n\n{sent}")));
        a.items.push(Item::User(shown));
        self.launch_turn(idx, None);
    }

    /// The exit code of the last command in the tab's shell, if it failed
    /// and the agent has not been shown it yet: what "explain" and "fix"
    /// are offered for.
    pub fn unexplained_failure(&self, idx: usize) -> Option<i32> {
        let tab = self.tabs.get(idx)?;
        if tab.agent.busy || tab.panes.agent().is_busy() || tab.panes.agent().block_seq <= tab.panes.agent().seen_seq {
            return None;
        }
        // 130 is the user's own Ctrl+C, which needs no explaining.
        tab.panes.agent().blocks.back()?.exit.filter(|code| *code != 0 && *code != 130)
    }

    /// Ask the agent about the last command that ran in the shell: to
    /// explain it, or to fix what went wrong.
    pub fn explain_last(&mut self, idx: usize, fix: bool) {
        let Some(block) = self.tabs[idx].panes.agent_mut().blocks.back().cloned() else {
            self.toast("No command has run in this tab yet");
            return;
        };
        if self.tabs[idx].agent.busy {
            self.toast("The agent is still working · Esc to stop it");
            return;
        }
        let failed = block.exit.is_some_and(|code| code != 0);
        let ask = match (fix, failed) {
            (false, true) => "Explain why this command failed and what would put it right. Do not change anything yet.",
            (false, false) => "Explain what this command did and what its output means.",
            (true, true) => "This command failed. Work out why, fix the cause, and run it again to confirm that it works.",
            (true, false) => "This command did not do what I wanted. Work out what went wrong and put it right.",
        };
        let exit = block.exit.map(|e| e.to_string()).unwrap_or_else(|| "unknown".into());
        let output = tools::clip_for_model(&block.output);
        let sent = self.scrubbed(&format!("{ask}\n\n$ {}\n[exit {exit}]\n{}", block.cmd, if output.trim().is_empty() { "(no output)" } else { &output }));
        let shown = format!("{} `{}`", if fix { "Fix" } else { "Explain" }, first_line(&block.cmd, 80));
        self.tabs[idx].agent.visible = true;
        self.start_turn_as(idx, shown, sent);
    }

    /// Have the agent follow a command the user started themselves and
    /// report when it ends.
    pub fn watch_command(&mut self, idx: usize, extra: &str) {
        let tab = &self.tabs[idx];
        let ShellState::Running { cmd, since } = &tab.panes.agent().state else {
            let why = if tab.panes.agent().integrated { "Nothing is running in this tab's terminal" } else { "This needs the shell integration, which is not active here" };
            self.toast(why);
            return;
        };
        if tab.agent.busy {
            self.toast("The agent is still working · Esc to stop it");
            return;
        }
        let cmd = cmd.clone().unwrap_or_else(|| "a command".into());
        let so_far = tools::clip_for_model(&tab.panes.agent().partial_output());
        let mut sent = format!(
            "I started this command myself {}s ago and it is still running:\n\n$ {cmd}\n\nFollow it for me: wait with command_status until it finishes, then tell me briefly how it went and, if it failed, why. Use send_input only if it is plainly waiting for an answer that is safe to give; otherwise tell me what it is asking. Do not interrupt it.",
            since.elapsed().as_secs()
        );
        if !extra.is_empty() {
            sent.push_str(&format!("\n\nWhen it is done: {extra}"));
        }
        sent.push_str(&format!("\n\nOutput so far:\n{}", if so_far.trim().is_empty() { "(nothing yet)" } else { &so_far }));
        let sent = self.scrubbed(&sent);
        let shown = format!("Watch `{}`{}", first_line(&cmd, 80), if extra.is_empty() { String::new() } else { format!(": {extra}") });
        let a = &mut self.tabs[idx].agent;
        a.visible = true;
        // If it ends before the agent first looks, its result is kept for it.
        a.detached = true;
        a.last_finished = None;
        a.own_command = false;
        self.start_turn_as(idx, shown, sent);
    }

    /// Start the background task for a turn: the model's reply to the
    /// conversation as it stands, or (for `/compact`) only its compaction.
    fn launch_turn(&mut self, idx: usize, compact_only: Option<String>) {
        let provider_name = self.cfg.agent.provider.clone();
        let model = self.cfg.agent.model.clone();
        let Some(provider) = self.cfg.providers.get(&provider_name).cloned().filter(|_| !model.is_empty()) else {
            self.open_model_picker();
            return;
        };
        let (window, lookup_window) = self.window_for(&provider_name, &provider, &model);
        let cap = Some(self.cfg.agent.compact_limit).filter(|cap| *cap > 0).unwrap_or(u64::MAX);
        let compact_limit = self.cfg.agent.auto_compact.then_some(cap);
        let mut system = run::system_prompt(&self.tabs[idx].panes.agent_mut().shell_name, &self.cfg.agent.instructions);
        if !self.cfg.agent.deny.is_empty() {
            let list: Vec<String> = self.cfg.agent.deny.iter().map(|d| format!("`{d}`")).collect();
            system.push_str(&format!(
                "\n\nCommands the user has ruled out. Never run them, and never reach the same result another way; say what you would have done and let the user decide: {}",
                list.join(", ")
            ));
        }
        let redact = self.known_secrets();
        let tab = &mut self.tabs[idx];
        let a = &mut tab.agent;
        a.window = window;
        a.turn_model = (provider_name, model.clone());
        a.compacting = compact_only.is_some();
        a.turn += 1;
        a.busy = true;
        a.streaming = false;
        a.status = if compact_only.is_some() { "Compacting the conversation" } else { "Thinking" }.into();
        a.thinking_tail.clear();
        a.started = Some(Instant::now());
        a.scroll = 0;
        let turn = Turn {
            tab: tab.id,
            turn: a.turn,
            tx: self.tx.clone(),
            client: self.http.clone(),
            provider,
            model,
            system,
            cwd: tab.panes.agent_mut().cwd.clone(),
            history: a.history.clone(),
            max_tokens: self.cfg.agent.max_tokens,
            window,
            lookup_window,
            compact_limit,
            context: a.totals.context,
            compact_only,
            redact,
        };
        a.task = Some(tokio::spawn(run::run_turn(turn)));
        self.save_conversation(idx);
        self.mark_dirty();
    }

    fn finish_turn(&mut self, idx: usize) {
        let tab = &mut self.tabs[idx];
        let a = &mut tab.agent;
        a.busy = false;
        a.streaming = false;
        a.task = None;
        a.approval = None;
        // A key being typed for /key has nothing to do with the turn: it
        // stays as it is, still hidden.
        if a.mode != InputMode::Secret {
            a.mode = InputMode::Prompt;
        }
        a.status.clear();
        a.thinking_tail.clear();
        // A turn that only compacted showed the model nothing new, so the
        // commands run since its last real turn are still news to it.
        if !std::mem::take(&mut a.compacting) {
            tab.panes.agent_mut().seen_seq = tab.panes.agent().block_seq;
        }
        // With the turn over, the agent is wherever the keyboard is again
        // (unless a command of its own is still running where it was).
        if !a.detached {
            tab.panes.agent = tab.panes.focus;
        }
    }

    pub(super) fn cancel_turn(&mut self, idx: usize, why: &str) {
        let tab = &mut self.tabs[idx];
        let a = &mut tab.agent;
        if !a.busy && a.task.is_none() {
            return;
        }
        if let Some(task) = a.task.take() {
            task.abort();
        }
        // Stale events from the aborted task are ignored by turn number.
        a.turn += 1;
        if let Some(Wait::Command { .. }) = a.wait.take() {
            // Interrupt the command the agent was running for us, but never
            // one the user started and the agent was only waiting on.
            if tab.panes.agent_mut().is_busy() && a.own_command {
                tab.panes.agent_mut().write(vec![0x03]);
            }
        }
        a.detached = false;
        for item in a.items.iter_mut() {
            if let Item::Tool(t) = item {
                match t.state {
                    ToolState::Proposed => {
                        t.state = ToolState::Declined;
                        t.summary = "cancelled".into();
                    }
                    ToolState::Running => {
                        t.state = ToolState::Failed;
                        t.summary = "stopped by you".into();
                    }
                    _ => {}
                }
            }
        }
        a.repair_history();
        if !why.is_empty() {
            a.items.push(Item::Notice(why.to_string()));
        }
        self.unqueue(idx);
        self.finish_turn(idx);
        self.save_conversation(idx);
        self.mark_dirty();
    }

    pub(super) fn on_agent_ev(&mut self, tab_id: u64, turn: u64, ev: AgentEv) {
        let Some(idx) = self.tab_index(tab_id) else { return };
        if self.tabs[idx].agent.turn != turn {
            return;
        }
        if !self.shown(idx) {
            self.tabs[idx].activity = true;
        }
        self.mark_dirty();
        // Anything that changes the conversation is written to disk as it happens.
        let lasting = matches!(
            ev,
            AgentEv::Assistant { .. } | AgentEv::ToolResult { .. } | AgentEv::Compacted { .. } | AgentEv::Done | AgentEv::Failed(_)
        ) || matches!(&ev, AgentEv::Checkpoint { .. } if !self.tabs[idx].agent.queued.is_empty());
        self.apply_agent_ev(idx, ev);
        if lasting {
            self.save_conversation(idx);
        }
    }

    fn apply_agent_ev(&mut self, idx: usize, ev: AgentEv) {
        let a = &mut self.tabs[idx].agent;
        match ev {
            AgentEv::Status(s) => {
                a.status = s;
                a.thinking_tail.clear();
            }
            AgentEv::Thinking(piece) => {
                a.status = "Reasoning".into();
                a.thinking_tail.push_str(&piece);
                // Only the tail is shown; keep it small.
                if a.thinking_tail.len() > 400 {
                    let cut = a.thinking_tail.len() - 200;
                    let cut = (cut..a.thinking_tail.len()).find(|&i| a.thinking_tail.is_char_boundary(i)).unwrap_or(0);
                    a.thinking_tail.drain(..cut);
                }
            }
            AgentEv::Delta(piece) => {
                a.status = "Writing".into();
                a.thinking_tail.clear();
                match a.items.last_mut() {
                    Some(Item::Assistant(text)) if a.streaming => text.push_str(&piece),
                    _ => {
                        a.items.push(Item::Assistant(piece));
                        a.streaming = true;
                    }
                }
            }
            AgentEv::Assistant { msg, usage } => {
                // The reasoning preview belongs to the reply that just ended.
                a.thinking_tail.clear();
                a.totals.add(&usage);
                if usage.input > 0 {
                    a.totals.context = usage.input;
                }
                if let Msg::Assistant { text, calls, .. } = &msg {
                    // The final text is authoritative; streamed pieces may have been dropped.
                    match a.items.last_mut() {
                        Some(Item::Assistant(shown)) if a.streaming => {
                            if !text.is_empty() {
                                *shown = text.clone();
                            }
                        }
                        _ if !text.trim().is_empty() => a.items.push(Item::Assistant(text.clone())),
                        _ => {}
                    }
                    for call in calls {
                        a.items.push(Item::Tool(ToolItem {
                            id: call.id.clone(),
                            name: call.name.clone(),
                            target: tool_target(call),
                            state: ToolState::Proposed,
                            exit: None,
                            summary: String::new(),
                        }));
                    }
                }
                a.streaming = false;
                a.history.push(msg);
            }
            AgentEv::Compacted { history, summary, replaced, usage } => {
                let was = a.totals.context;
                a.history = history;
                a.totals.add(&usage);
                // Unknown until the next request reports it.
                a.totals.context = 0;
                a.streaming = false;
                let size = if was > 0 { format!(" The conversation had reached {} tokens.", models::tokens(was)) } else { String::new() };
                // Asked for, the summary is shown. When it happens by itself
                // in the middle of the work, one line says so and the summary
                // is a command away, instead of pushing the work off screen.
                a.items.push(Item::Notice(if a.compacting {
                    format!("**Compacted.** {replaced} messages were replaced by the summary below, which is what the agent now works from.{size}\n\n{summary}")
                } else {
                    format!("**Compacted automatically.** {replaced} messages were replaced by a summary, which is what the agent now works from.{size} `/summary` shows it.")
                }));
                a.status = "Thinking".into();
            }
            AgentEv::Note(text) => a.items.push(Item::Notice(text)),
            AgentEv::Window(found) => {
                if let Some(window) = found {
                    a.window = window;
                }
                self.windows.insert(a.turn_model.clone(), found);
            }
            AgentEv::Rules(sources) => {
                // Say which rules apply, once, and again only if that changes.
                if a.rules != sources && !sources.is_empty() {
                    let list: Vec<String> = sources.iter().map(|s| format!("`{s}`")).collect();
                    a.items.push(Item::Notice(format!("Following the rules in {}.", list.join(" and "))));
                }
                a.rules = sources;
            }
            AgentEv::ToolRequest { call, reply } => self.on_tool_request(idx, call, reply),
            AgentEv::EditReview { call, path, new_file, hunks, reply } => {
                let question = if new_file {
                    format!("Create {path}?")
                } else if call.name == "write_file" {
                    format!("Rewrite {path}?")
                } else {
                    format!("Edit {path}?")
                };
                let mut choices = vec![(Choice::Yes, "Yes"), (Choice::Always, "Yes, don't ask again this session")];
                if hunks.len() > 1 {
                    choices.push((Choice::Review, "Review each change"));
                }
                choices.push((Choice::No, "No, do something else"));
                let preview = hunks.concat();
                let edit = Some(EditReview { path, hunks, decided: None });
                self.ask(idx, Approval { call, reply, question, warning: None, preview, choices, selected: 0, scroll: 0, guard_until: None, edit });
            }
            AgentEv::ToolResult { id, name, content, is_error, redacted } => {
                if let Some(item) = a.tool_mut(&id) {
                    if item.state != ToolState::Declined {
                        item.state = if is_error { ToolState::Failed } else { ToolState::Done };
                    }
                    if redacted > 0 && item.summary.is_empty() {
                        item.summary = format!("{redacted} secret{} hidden from the model", if redacted == 1 { "" } else { "s" });
                    }
                    if item.summary.is_empty() {
                        // Successes speak for themselves; failures get a line of explanation
                        // (unless the exit code already says it).
                        let show = is_error && item.exit.is_none();
                        let _ = &name;
                        if show {
                            item.summary = first_line(&content, 160);
                        }
                    }
                }
                a.history.push(Msg::ToolResult { id, name, content, is_error });
                a.status = "Thinking".into();
            }
            AgentEv::Checkpoint { reply } => {
                let texts = std::mem::take(&mut a.queued);
                let mut added = Vec::new();
                for text in texts {
                    let sent = run::interjection(&text);
                    a.items.push(Item::User(text));
                    a.history.push(Msg::User(sent.clone()));
                    added.push(sent);
                }
                if !added.is_empty() {
                    a.streaming = false;
                    a.scroll = 0;
                }
                let _ = reply.send(added);
            }
            AgentEv::Done => {
                self.finish_turn(idx);
                // Something typed in the turn's last moment starts the next one.
                let waiting = std::mem::take(&mut self.tabs[idx].agent.queued);
                if !waiting.is_empty() {
                    self.start_turn(idx, waiting.join("\n\n"));
                }
            }
            AgentEv::Failed(err) => {
                a.items.push(Item::Error(err));
                a.repair_history();
                self.unqueue(idx);
                self.finish_turn(idx);
            }
        }
    }

    // ---- tools --------------------------------------------------------------

    fn on_tool_request(&mut self, idx: usize, call: ToolCall, reply: oneshot::Sender<ToolReply>) {
        let autonomy = self.autonomy_for(idx);
        let tab = &mut self.tabs[idx];
        match call.name.as_str() {
            "read_terminal" => {
                let lines = call.u64_arg("lines").unwrap_or(80).clamp(1, 1000) as usize;
                let text = tab.panes.agent_mut().recent_text(lines);
                let content = if text.trim().is_empty() { "(the terminal is empty)".to_string() } else { tools::clip_for_model(&text) };
                let _ = reply.send(ToolReply::Result { content, is_error: false });
            }
            // Looking never needs permission.
            "read_file" | "find_files" | "search_files" => {
                self.set_tool_state(idx, &call.id, ToolState::Running);
                let cwd = self.tabs[idx].panes.agent_mut().cwd.clone().unwrap_or_else(|| "/".into());
                let _ = reply.send(ToolReply::Local { cwd });
            }
            "command_status" => self.command_status(idx, call, reply),
            "run_command" => {
                let cmd = call.str_arg("command").unwrap_or_default().to_string();
                let rules = &self.cfg.agent;
                match policy::decide(&cmd, autonomy, &rules.allow, &rules.deny) {
                    policy::Decision::Run => self.run_command(idx, call, reply, cmd, String::new()),
                    policy::Decision::Blocked(entry) => {
                        if let Some(item) = self.tabs[idx].agent.tool_mut(&call.id) {
                            item.state = ToolState::Declined;
                            item.summary = format!("not allowed: your deny list has `{entry}`");
                        }
                        let content = format!(
                            "The user's settings do not allow this command: it matches `{entry}` on their deny list. It was not run. Do not get the same effect another way; say what you wanted to do and let the user decide."
                        );
                        let _ = reply.send(ToolReply::Result { content, is_error: true });
                    }
                    policy::Decision::Ask(danger) => {
                        let warning = danger.map(|why| format!("Careful: this {why}."));
                        let mut choices = vec![(Choice::Yes, "Yes")];
                        if warning.is_none() {
                            choices.push((Choice::Always, "Yes, don't ask again this session"));
                        }
                        choices.push((Choice::Edit, "Edit first"));
                        choices.push((Choice::No, "No, do something else"));
                        let mut preview: Vec<String> = cmd.lines().map(str::to_string).collect();
                        if let Some(why) = call.str_arg("explanation").filter(|w| !w.trim().is_empty()) {
                            preview.push(format!("# {}", why.trim()));
                        }
                        self.ask(idx, Approval { call, reply, question: "Run this command?".into(), warning, preview, choices, selected: 0, scroll: 0, guard_until: None, edit: None });
                    }
                }
            }
            "write_file" | "edit_file" => {
                let cwd = self.tabs[idx].panes.agent_mut().cwd.clone().unwrap_or_else(|| "/".into());
                if autonomy == Autonomy::Auto {
                    self.set_tool_state(idx, &call.id, ToolState::Running);
                    let _ = reply.send(ToolReply::Local { cwd });
                } else {
                    // The agent task works out what would change and comes
                    // back with it as a diff to approve (`on_edit_review`).
                    let _ = reply.send(ToolReply::Review { cwd });
                }
            }
            "send_input" => {
                // Typing is for a program that is running. At the shell's
                // prompt it would be a way to run a command (or to send off
                // whatever the user has half typed there) around every check
                // run_command goes through.
                if tab.panes.agent_mut().integrated && matches!(tab.panes.agent_mut().state, ShellState::Prompt) {
                    let content = "Nothing is running in the terminal to type into: the shell is at its prompt. To run a command, use run_command.".to_string();
                    let _ = reply.send(ToolReply::Result { content, is_error: true });
                    return;
                }
                // What is typed may itself be a command (into a remote shell,
                // or a shell Astrum cannot see into): the deny list and the
                // destructive-command check apply to it as well.
                let text = call.str_arg("text").unwrap_or_default().to_string();
                let verdict = policy::decide(&text, autonomy, &[], &self.cfg.agent.deny);
                if let policy::Decision::Blocked(entry) = &verdict {
                    if let Some(item) = self.tabs[idx].agent.tool_mut(&call.id) {
                        item.state = ToolState::Declined;
                        item.summary = format!("not allowed: your deny list has `{entry}`");
                    }
                    let content = format!("The user's settings do not allow this: it matches `{entry}` on their deny list. Nothing was typed. Do not get the same effect another way; say what you wanted to do and let the user decide.");
                    let _ = reply.send(ToolReply::Result { content, is_error: true });
                    return;
                }
                let danger = match verdict {
                    policy::Decision::Ask(Some(why)) => Some(format!("Careful: this {why}.")),
                    _ => None,
                };
                if autonomy == Autonomy::Auto && danger.is_none() {
                    self.send_input(idx, call, reply);
                    return;
                }
                let tab = &mut self.tabs[idx];
                let preview = vec![tool_target(&call)];
                let choices = vec![
                    (Choice::Yes, "Yes"),
                    (Choice::Always, "Yes, don't ask again this session"),
                    (Choice::No, "No, do something else"),
                ];
                let running = tab.panes.agent_mut().running_cmd().map(|c| format!(" into `{}`", first_line(c, 40))).unwrap_or_default();
                self.ask(
                    idx,
                    Approval { call, reply, question: format!("Type this{running}?"), warning: danger, preview, choices, selected: 0, scroll: 0, guard_until: None, edit: None },
                );
            }
            other => {
                let _ = reply.send(ToolReply::Result { content: format!("Unknown tool `{other}`."), is_error: true });
            }
        }
    }

    fn set_tool_state(&mut self, idx: usize, id: &str, state: ToolState) {
        if let Some(item) = self.tabs[idx].agent.tool_mut(id) {
            item.state = state;
        }
    }

    fn ask(&mut self, idx: usize, mut approval: Approval) {
        let tab = &mut self.tabs[idx];
        // Someone in the middle of typing would answer with whatever key
        // came next. Give them a moment to see the question first.
        if tab.agent.last_typed.is_some_and(|at| at.elapsed() < Duration::from_millis(1500)) {
            approval.guard_until = Some(Instant::now() + Duration::from_millis(1200));
        }
        tab.agent.approval = Some(approval);
        // The question takes over the prompt; a key half typed there is dropped, never shown.
        tab.agent.leave_secret();
        tab.agent.mode = InputMode::Prompt;
        tab.agent.status = "Waiting for you".into();
        tab.agent.visible = true;
        tab.agent.scroll = 0;
        if idx == self.active {
            // Bring the question to the user; a stray Enter in the shell must not answer it.
            tab.focus = Focus::Agent;
        }
        self.mark_dirty();
    }

    pub(super) fn choose_approval(&mut self, choice_idx: usize) {
        let idx = self.active;
        let Some(tab) = self.tabs.get_mut(idx) else { return };
        let a = &mut tab.agent;
        let Some(choice) = a.approval.as_ref().and_then(|ap| ap.choices.get(choice_idx)).map(|(c, _)| *c) else { return };
        match choice {
            Choice::Edit => {
                let cmd = a.approval.as_ref().and_then(|ap| ap.call.str_arg("command")).unwrap_or_default().to_string();
                a.input.set(&cmd);
                a.mode = InputMode::EditCommand;
            }
            Choice::No => {
                a.input.clear();
                a.mode = InputMode::Guidance;
            }
            // Start going through a file change one hunk at a time.
            Choice::Review => {
                if let Some(ap) = &mut a.approval {
                    if let Some(edit) = &mut ap.edit {
                        edit.decided = Some(Vec::new());
                        ap.question = format!("Change 1 of {} to {}", edit.hunks.len(), edit.path);
                        ap.preview = edit.hunks[0].clone();
                        ap.choices = vec![(Choice::Yes, "Apply this change"), (Choice::Skip, "Skip it"), (Choice::No, "Cancel the whole edit")];
                        ap.selected = 0;
                        ap.scroll = 0;
                    }
                }
            }
            // A verdict on one hunk: move to the next, or finish.
            Choice::Yes | Choice::Skip if a.approval.as_ref().is_some_and(|ap| ap.edit.as_ref().is_some_and(|e| e.decided.is_some())) => {
                let Some(ap) = &mut a.approval else { return };
                let Some(edit) = &mut ap.edit else { return };
                let Some(decided) = &mut edit.decided else { return };
                decided.push(choice == Choice::Yes);
                if let Some(next) = edit.hunks.get(decided.len()) {
                    ap.question = format!("Change {} of {} to {}", decided.len() + 1, edit.hunks.len(), edit.path);
                    ap.preview = next.clone();
                    ap.selected = 0;
                    ap.scroll = 0;
                } else {
                    let accept = decided.clone();
                    let Some(ap) = a.approval.take() else { return };
                    a.status = "Working".into();
                    if accept.iter().any(|a| *a) {
                        if let Some(item) = a.tool_mut(&ap.call.id) {
                            item.state = ToolState::Running;
                        }
                        let _ = ap.reply.send(ToolReply::Apply { accept });
                    } else {
                        if let Some(item) = a.tool_mut(&ap.call.id) {
                            item.state = ToolState::Declined;
                        }
                        let content = "The user looked at each change and declined all of them, so the file is unchanged. Ask what they would like instead.".to_string();
                        let _ = ap.reply.send(ToolReply::Result { content, is_error: true });
                    }
                }
            }
            Choice::Skip => {}
            Choice::Yes | Choice::Always => {
                if choice == Choice::Always {
                    a.session_auto = true;
                }
                let Some(ap) = a.approval.take() else { return };
                a.status = "Working".into();
                match ap.call.name.as_str() {
                    "run_command" => {
                        let cmd = ap.call.str_arg("command").unwrap_or_default().to_string();
                        self.run_command(idx, ap.call, ap.reply, cmd, String::new());
                    }
                    "send_input" => self.send_input(idx, ap.call, ap.reply),
                    // A file change approved as a whole.
                    _ if ap.edit.is_some() => {
                        self.set_tool_state(idx, &ap.call.id, ToolState::Running);
                        let hunks = ap.edit.map(|e| e.hunks.len()).unwrap_or(0);
                        let _ = ap.reply.send(ToolReply::Apply { accept: vec![true; hunks] });
                    }
                    _ => {
                        self.set_tool_state(idx, &ap.call.id, ToolState::Running);
                        let cwd = self.tabs[idx].panes.agent_mut().cwd.clone().unwrap_or_else(|| "/".into());
                        let _ = ap.reply.send(ToolReply::Local { cwd });
                    }
                }
            }
        }
        self.mark_dirty();
    }

    /// Decline the pending action, optionally telling the agent what to do instead.
    fn decline(&mut self, idx: usize, guidance: &str) {
        let a = &mut self.tabs[idx].agent;
        a.mode = InputMode::Prompt;
        a.input.clear();
        let Some(ap) = a.approval.take() else { return };
        if let Some(item) = a.tool_mut(&ap.call.id) {
            item.state = ToolState::Declined;
        }
        let content = if guidance.is_empty() {
            "The user declined this. Do not retry it; ask what they would like instead or take a different approach.".to_string()
        } else {
            a.items.push(Item::User(guidance.to_string()));
            format!("The user declined this and said: {guidance}")
        };
        a.status = "Thinking".into();
        let _ = ap.reply.send(ToolReply::Result { content, is_error: true });
        self.mark_dirty();
    }

    /// Run an approved command: in the live shell when it is sitting at a
    /// prompt, otherwise in a background subshell.
    fn run_command(&mut self, idx: usize, call: ToolCall, reply: oneshot::Sender<ToolReply>, cmd: String, note: String) {
        let default_timeout = self.cfg.agent.command_timeout;
        let timeout = call.u64_arg("timeout_secs").unwrap_or(default_timeout).clamp(1, 600);
        let tab = &mut self.tabs[idx];
        let pane = &mut tab.panes.agent_mut();
        let mode = pane.mode();
        let at_prompt = pane.integrated && matches!(pane.state, ShellState::Prompt) && !mode.contains(TermMode::ALT_SCREEN);
        let pasteable = !cmd.contains('\n') || mode.contains(TermMode::BRACKETED_PASTE);
        if let Some(item) = tab.agent.tool_mut(&call.id) {
            item.state = ToolState::Running;
        }
        tab.agent.status = "Running command".into();
        if at_prompt && pasteable {
            pane.scroll_to_bottom();
            pane.clear_selection();
            // Clear anything half-typed at the prompt, then type the command.
            let mut bytes = vec![0x15];
            bytes.extend(input::encode_paste(&cmd, mode));
            bytes.push(b'\r');
            pane.write(bytes);
            tab.agent.detached = false;
            tab.agent.last_finished = None;
            tab.agent.own_command = true;
            tab.agent.wait = Some(Wait::Command {
                reply,
                deadline: Instant::now() + Duration::from_secs(timeout),
                call_id: call.id,
                started: false,
                note,
                timeout,
                paged: None,
            });
        } else {
            let why = if !pane.integrated {
                "shell integration is not active in this terminal".to_string()
            } else if let Some(running) = pane.running_cmd() {
                format!("the terminal is busy running `{}`", first_line(running, 60))
            } else {
                "the terminal cannot take a command right now".to_string()
            };
            let note = format!(
                "{note}note: {why}, so this ran in a separate background shell; directory and environment changes from it do not persist.\n"
            );
            if let Some(item) = tab.agent.tool_mut(&call.id) {
                item.summary = "ran in a background shell".into();
            }
            let cwd = pane.cwd.clone().unwrap_or_else(|| "/".into());
            let _ = reply.send(ToolReply::Subprocess { cwd, timeout, note });
        }
        self.mark_dirty();
    }

    fn command_status(&mut self, idx: usize, call: ToolCall, reply: oneshot::Sender<ToolReply>) {
        let tab = &mut self.tabs[idx];
        let a = &mut tab.agent;
        if let Some(block) = a.last_finished.take() {
            let content = tools::format_command_result("status: finished\n", block.exit, block.cwd.as_deref(), &block.output);
            let _ = reply.send(ToolReply::Result { content, is_error: block.exit != Some(0) });
            return;
        }
        if tab.panes.agent_mut().is_busy() {
            if let Some(item) = a.tool_mut(&call.id) {
                item.state = ToolState::Running;
            }
            let wait = call.u64_arg("wait_secs").unwrap_or(15).clamp(1, 300);
            a.wait = Some(Wait::Command {
                reply,
                deadline: Instant::now() + Duration::from_secs(wait),
                call_id: call.id,
                started: true,
                note: String::new(),
                timeout: wait,
                paged: None,
            });
            a.status = "Waiting on command".into();
            return;
        }
        let _ = reply.send(ToolReply::Result {
            content: "No command is running; the terminal is at a prompt. Recent terminal content:\n".to_string()
                + &tools::clip_for_model(&tab.panes.agent_mut().recent_text(20)),
            is_error: false,
        });
    }

    fn send_input(&mut self, idx: usize, call: ToolCall, reply: oneshot::Sender<ToolReply>) {
        let tab = &mut self.tabs[idx];
        let bytes = input_bytes(&call);
        if bytes.is_empty() {
            let _ = reply.send(ToolReply::Result { content: "Nothing to type: give `text`, `key`, or both.".into(), is_error: true });
            return;
        }
        tab.panes.agent_mut().scroll_to_bottom();
        tab.panes.agent_mut().write(bytes);
        if let Some(item) = tab.agent.tool_mut(&call.id) {
            item.state = ToolState::Running;
        }
        // Let the program react before reporting what the screen shows.
        tab.agent.wait = Some(Wait::Snapshot { reply, at: Instant::now() + Duration::from_millis(700), call_id: call.id });
        self.mark_dirty();
    }

    pub(super) fn agent_command_started(&mut self, idx: usize) {
        if let Some(Wait::Command { started, .. }) = &mut self.tabs[idx].agent.wait {
            *started = true;
        }
    }

    pub(super) fn agent_command_finished(&mut self, idx: usize, block: Block) {
        let tab = &mut self.tabs[idx];
        let a = &mut tab.agent;
        // Whatever runs next in this shell is not the agent's until it starts one.
        a.own_command = false;
        match a.wait.take() {
            Some(Wait::Command { reply, call_id, started: true, note, paged, .. }) => {
                a.detached = false;
                a.cpu_seen = None;
                if let Some(item) = a.tool_mut(&call_id) {
                    item.exit = block.exit;
                    item.state = if block.exit == Some(0) { ToolState::Done } else { ToolState::Failed };
                    if paged.is_some() {
                        item.summary = "it opened a pager, closed after the first screen".into();
                    }
                }
                // What a pager drew is screen-painting, not output; its first screen was kept as text.
                let (note, output) = match &paged {
                    Some(page) => (
                        format!("{note}note: the command opened a pager and waited there for a key. Astrum closed it after its first screen, which is all that is shown below. To see everything, run it again with the pager off (a --no-pager option, or piped through cat).\n"),
                        page.as_str(),
                    ),
                    None => (note, block.output.as_str()),
                };
                let content = tools::format_command_result(&note, block.exit, tab.panes.agent_mut().cwd.as_deref(), output);
                let _ = reply.send(ToolReply::Result { content, is_error: block.exit != Some(0) });
                a.status = "Thinking".into();
            }
            other => {
                a.wait = other;
                if a.detached {
                    a.detached = false;
                    // Mark the row for the command that outlived its wait.
                    let pending = a.items.iter_mut().rev().find_map(|i| match i {
                        Item::Tool(t) if t.name == "run_command" && t.state == ToolState::Running => Some(t),
                        _ => None,
                    });
                    if let Some(item) = pending {
                        item.exit = block.exit;
                        item.state = if block.exit == Some(0) { ToolState::Done } else { ToolState::Failed };
                    }
                    a.cpu_seen = None;
                    a.last_finished = Some(block);
                }
            }
        }
    }

    pub(super) fn agent_tick(&mut self, now: Instant) {
        for idx in 0..self.tabs.len() {
            let tab = &mut self.tabs[idx];
            let a = &mut tab.agent;
            // A pager the agent's own command wandered into would wait for a
            // key forever. Once it has sat on its prompt for a moment, keep
            // what it shows and close it.
            if let Some(Wait::Command { started: true, paged: paged @ None, .. }) = &mut a.wait {
                let quiet = tab.panes.agent_mut().last_output.elapsed() >= Duration::from_millis(1200);
                if a.own_command && quiet && tab.panes.agent_mut().is_busy() && tab.panes.agent_mut().pager_waiting() {
                    let screen = tab.panes.agent().recent_text(tab.panes.agent().rows as usize);
                    // Without the pager's own prompt line.
                    let page = screen.rsplit_once('\n').map(|(above, _)| above).unwrap_or("").to_string();
                    *paged = Some(page);
                    tab.panes.agent_mut().write(&b"q"[..]);
                    self.dirty = true;
                }
            }
            let due = match &a.wait {
                Some(Wait::Command { deadline, .. }) => now >= *deadline,
                Some(Wait::Snapshot { at, .. }) => now >= *at,
                None => false,
            };
            if !due {
                continue;
            }
            match a.wait.take() {
                Some(Wait::Command { reply, started, timeout, note, call_id, .. }) => {
                    let content = if started {
                        a.detached = true;
                        let so_far = tools::clip_for_model(&tab.panes.agent_mut().partial_output());
                        let alive = liveness(&tab.panes.agent_mut(), &mut a.cpu_seen, now);
                        format!(
                            "{note}status: still running after {timeout}s\n{alive}output so far:\n{}\n\nThe command is still running in the terminal. Use command_status to keep waiting, send_input to interact with it (key ctrl-c interrupts), or tell the user if it needs them.",
                            if so_far.trim().is_empty() { "(nothing yet)" } else { &so_far }
                        )
                    } else {
                        if let Some(item) = a.tool_mut(&call_id) {
                            item.state = ToolState::Failed;
                        }
                        format!(
                            "{note}The shell did not start the command within {timeout}s; it may be waiting for more input (an unclosed quote?) or showing a prompt. The terminal shows:\n{}",
                            tools::clip_for_model(&tab.panes.agent_mut().recent_text(15))
                        )
                    };
                    let _ = reply.send(ToolReply::Result { content, is_error: !started });
                    a.status = "Thinking".into();
                }
                Some(Wait::Snapshot { reply, call_id, .. }) => {
                    if let Some(item) = a.tool_mut(&call_id) {
                        item.state = ToolState::Done;
                    }
                    let state = if tab.panes.agent_mut().is_busy() { "a program is still running" } else { "the shell is at a prompt" };
                    let content = format!(
                        "Typed. Now {state}. The terminal shows:\n{}",
                        tools::clip_for_model(&tab.panes.agent_mut().recent_text(25))
                    );
                    let _ = reply.send(ToolReply::Result { content, is_error: false });
                }
                None => {}
            }
            self.mark_dirty();
        }
    }
}
