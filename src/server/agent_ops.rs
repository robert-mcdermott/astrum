//! Agent orchestration on the server loop: prompt editing, slash commands,
//! approvals, running the agent's commands in the live shell, and applying
//! the events an agent turn sends back.

use super::session::{Approval, Choice, InputMode, Item, Saved, ToolItem, ToolState, Wait};
use super::store;
use super::{Ev, Focus, Overlay, PickItem, Picker, PickerKind, Server};
use crate::agent::policy::{self, Risk};
use crate::agent::run::{self, Turn};
use crate::agent::tools;
use crate::agent::{AgentEv, Msg, ToolCall, ToolReply, llm, models};
use crate::config::{Autonomy, ProviderKind};
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
                KeyCode::PageUp => a.scroll += 5,
                KeyCode::PageDown => a.scroll = a.scroll.saturating_sub(5),
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
                if a.busy {
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
                let pane = &mut tab.pane;
                pane.scroll_to_bottom();
                let mut bytes = input::encode_paste(&cmd, pane.mode());
                bytes.push(b'\r');
                pane.write(bytes);
                tab.agent.prompts.push(text);
            }
            return;
        }
        if a.busy {
            a.input.set(&text);
            self.toast("The agent is still working · Esc to stop it");
            return;
        }
        self.start_turn(idx, text);
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
                    match theme::by_name(arg) {
                        Some(t) => self.set_theme(t),
                        None => {
                            let names: Vec<&str> = theme::all().iter().map(|t| t.name).collect();
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
                self.cancel_turn(idx, "");
                let tab = &mut self.tabs[idx];
                let a = &mut tab.agent;
                a.items.clear();
                a.history.clear();
                a.totals = Default::default();
                a.session_auto = false;
                a.seen_seq = tab.pane.block_seq;
                a.scroll = 0;
                // The old conversation stays on disk; the next prompt starts a new one.
                a.saved = None;
                a.save_failed = false;
                a.rules.clear();
            }
            "usage" | "cost" | "tokens" => self.show_usage(idx),
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

    /// `/usage`: what this conversation has used and cost so far.
    fn show_usage(&mut self, idx: usize) {
        let provider_name = self.cfg.agent.provider.clone();
        let model = self.cfg.agent.model.clone();
        let info = self.cfg.providers.get(&provider_name).map(|p| models::info(&self.cfg, &provider_name, p, &model));
        let t = self.tabs[idx].agent.totals;
        if t.requests == 0 {
            self.notice(idx, "Nothing has been sent to a model in this conversation yet.");
            return;
        }
        let n = models::tokens;
        let mut lines = vec![format!("**This conversation** · `{model}` ({provider_name})")];
        match info.filter(|_| t.context > 0) {
            Some(i) => lines.push(format!(
                "- Context: {} of {} tokens ({}%)",
                n(t.context),
                n(i.window),
                (t.context * 100).checked_div(i.window).unwrap_or(0)
            )),
            None => lines.push(format!("- Context: {} tokens", n(t.context))),
        }
        let mut sent = format!("- Sent: {} tokens in {} request{}", n(t.input), t.requests, if t.requests == 1 { "" } else { "s" });
        if t.cache_read + t.cache_write > 0 {
            sent.push_str(&format!(" ({} read from the provider's cache, {} written to it)", n(t.cache_read), n(t.cache_write)));
        }
        lines.push(sent);
        lines.push(format!("- Received: {} tokens", n(t.output)));
        match (t.cost_label(), info.and_then(|i| i.price)) {
            (Some(cost), price) => {
                let rate = price.map(|p| format!(" · this model: ${:.2} in, ${:.2} out per million tokens", p.input, p.output)).unwrap_or_default();
                let partial = if t.unpriced > 0 { format!(", not counting {} request{} on models without a known price", t.unpriced, if t.unpriced == 1 { "" } else { "s" }) } else { String::new() };
                lines.push(format!("- Cost: {cost}{partial}{rate}"));
            }
            (None, _) => lines.push(format!(
                "- Cost: not known for this model. To see it, add its prices under `[models.\"{model}\"]` in the config file (`/config` shows where)."
            )),
        }
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
        let saved = a.saved.get_or_insert_with(|| Saved { id: store::new_id(), created: store::now() });
        let mut meta = store::new_meta(&saved.id, saved.created);
        let mut prompts = a.items.iter().filter_map(|i| match i {
            Item::User(t) => Some(t),
            _ => None,
        });
        meta.title = prompts.next().map(|t| first_line(t, 80)).unwrap_or_default();
        meta.prompts = 1 + prompts.count();
        meta.cwd = tab.pane.cwd.clone();
        meta.session = self.session.clone();
        meta.model = self.cfg.agent.model.clone();
        meta.totals = a.totals;
        if let Err(e) = store::save(&meta, &a.history, &a.items) {
            if !a.save_failed {
                a.save_failed = true;
                self.toast(format!("Could not save this conversation: {}", first_line(&format!("{e:#}"), 80)));
            }
        }
    }

    fn open_conversation_ids(&self) -> Vec<(usize, String)> {
        self.tabs.iter().enumerate().filter_map(|(i, t)| t.agent.saved.as_ref().map(|s| (i, s.id.clone()))).collect()
    }

    /// `/resume`: reopen the most recent conversation that is not already
    /// open in a tab, or the most recent one matching `arg`.
    fn resume(&mut self, idx: usize, arg: &str) {
        let open = self.open_conversation_ids();
        let needle = arg.to_lowercase();
        let all = store::list();
        let found = all.iter().find(|m| {
            if needle.is_empty() {
                !open.iter().any(|(_, id)| *id == m.id)
            } else {
                m.id.starts_with(&needle) || m.title.to_lowercase().contains(&needle)
            }
        });
        match found {
            Some(m) => {
                let id = m.id.clone();
                self.load_conversation(idx, &id);
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

    /// Replace the tab's conversation with a saved one.
    pub(super) fn load_conversation(&mut self, idx: usize, id: &str) {
        if let Some((other, _)) = self.open_conversation_ids().into_iter().find(|(_, open)| open == id) {
            let msg = if other == idx { "That conversation is the one open here.".to_string() } else { format!("That conversation is open in tab {}.", other + 1) };
            self.notice(idx, msg);
            return;
        }
        let (meta, body) = match store::load(id) {
            Ok(found) => found,
            Err(e) => {
                self.notice(idx, format!("Could not open that conversation: {e:#}"));
                return;
            }
        };
        // The conversation being replaced is already on disk.
        self.cancel_turn(idx, "");
        let active = idx == self.active;
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
        a.seen_seq = tab.pane.block_seq;
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
    }

    // ---- turns --------------------------------------------------------------

    fn build_context(&self, idx: usize) -> String {
        let tab = &self.tabs[idx];
        let pane = &tab.pane;
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
        let fresh = (pane.block_seq - tab.agent.seen_seq.min(pane.block_seq)) as usize;
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
        s
    }

    fn start_turn(&mut self, idx: usize, text: String) {
        let provider_name = self.cfg.agent.provider.clone();
        let model = self.cfg.agent.model.clone();
        let provider = self.cfg.providers.get(&provider_name).cloned();
        let Some(provider) = provider.filter(|_| !model.is_empty()) else {
            self.tabs[idx].agent.input.set(&text);
            self.open_model_picker();
            self.toast("Pick a model first, then press Enter again to send");
            return;
        };
        let info = models::info(&self.cfg, &provider_name, &provider, &model);
        let context = self.build_context(idx);
        let system = run::system_prompt(&self.tabs[idx].pane.shell_name, &self.cfg.agent.instructions);
        let tab = &mut self.tabs[idx];
        let a = &mut tab.agent;
        a.history.push(Msg::User(format!("{context}\n\n{text}")));
        a.items.push(Item::User(text.clone()));
        if a.prompts.last() != Some(&text) {
            a.prompts.push(text);
        }
        a.window = info.window;
        a.price = info.price;
        a.turn += 1;
        a.busy = true;
        a.streaming = false;
        a.status = "Thinking".into();
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
            cwd: tab.pane.cwd.clone(),
            history: a.history.clone(),
            max_tokens: self.cfg.agent.max_tokens,
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
        a.mode = InputMode::Prompt;
        a.status.clear();
        a.thinking_tail.clear();
        a.seen_seq = tab.pane.block_seq;
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
            // Interrupt the command the agent was running for us.
            if tab.pane.is_busy() {
                tab.pane.write(vec![0x03]);
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
        self.finish_turn(idx);
        self.save_conversation(idx);
        self.mark_dirty();
    }

    pub(super) fn on_agent_ev(&mut self, tab_id: u64, turn: u64, ev: AgentEv) {
        let Some(idx) = self.tab_index(tab_id) else { return };
        if self.tabs[idx].agent.turn != turn {
            return;
        }
        if idx != self.active {
            self.tabs[idx].activity = true;
        }
        self.mark_dirty();
        // Anything that changes the conversation is written to disk as it happens.
        let lasting = matches!(ev, AgentEv::Assistant { .. } | AgentEv::ToolResult { .. } | AgentEv::Done | AgentEv::Failed(_));
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
                a.totals.add(&usage, a.price.as_ref());
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
            AgentEv::Rules(sources) => {
                // Say which rules apply, once, and again only if that changes.
                if a.rules != sources && !sources.is_empty() {
                    let list: Vec<String> = sources.iter().map(|s| format!("`{s}`")).collect();
                    a.items.push(Item::Notice(format!("Following the rules in {}.", list.join(" and "))));
                }
                a.rules = sources;
            }
            AgentEv::ToolRequest { call, reply } => self.on_tool_request(idx, call, reply),
            AgentEv::ToolResult { id, name, content, is_error } => {
                if let Some(item) = a.tool_mut(&id) {
                    if item.state != ToolState::Declined {
                        item.state = if is_error { ToolState::Failed } else { ToolState::Done };
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
            AgentEv::Done => self.finish_turn(idx),
            AgentEv::Failed(err) => {
                a.items.push(Item::Error(err));
                a.repair_history();
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
                let text = tab.pane.recent_text(lines);
                let content = if text.trim().is_empty() { "(the terminal is empty)".to_string() } else { tools::clip_for_model(&text) };
                let _ = reply.send(ToolReply::Result { content, is_error: false });
            }
            "read_file" => {
                self.set_tool_state(idx, &call.id, ToolState::Running);
                let cwd = self.tabs[idx].pane.cwd.clone().unwrap_or_else(|| "/".into());
                let _ = reply.send(ToolReply::Local { cwd });
            }
            "command_status" => self.command_status(idx, call, reply),
            "run_command" => {
                let cmd = call.str_arg("command").unwrap_or_default().to_string();
                let risk = policy::classify(&cmd);
                if policy::needs_approval(autonomy, &risk) {
                    let warning = match &risk {
                        Risk::Dangerous(why) => Some(format!("Careful: this {why}.")),
                        _ => None,
                    };
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
                    self.ask(idx, Approval { call, reply, question: "Run this command?".into(), warning, preview, choices, selected: 0 });
                } else {
                    self.run_command(idx, call, reply, cmd, String::new());
                }
            }
            "write_file" | "edit_file" => {
                if autonomy == Autonomy::Auto {
                    self.set_tool_state(idx, &call.id, ToolState::Running);
                    let cwd = self.tabs[idx].pane.cwd.clone().unwrap_or_else(|| "/".into());
                    let _ = reply.send(ToolReply::Local { cwd });
                    return;
                }
                let path = call.str_arg("path").unwrap_or_default().to_string();
                let mut preview = Vec::new();
                let question = if call.name == "write_file" {
                    let content = call.str_arg("content").unwrap_or_default();
                    let total = content.lines().count();
                    preview.extend(content.lines().take(14).map(|l| format!("+ {l}")));
                    if total > 14 {
                        preview.push(format!("  … {} more lines", total - 14));
                    }
                    format!("Write {path}?")
                } else {
                    let old = call.str_arg("old_text").unwrap_or_default();
                    let new = call.str_arg("new_text").unwrap_or_default();
                    preview.extend(old.lines().take(8).map(|l| format!("- {l}")));
                    if old.lines().count() > 8 {
                        preview.push(format!("  … {} more removed lines", old.lines().count() - 8));
                    }
                    preview.extend(new.lines().take(8).map(|l| format!("+ {l}")));
                    if new.lines().count() > 8 {
                        preview.push(format!("  … {} more added lines", new.lines().count() - 8));
                    }
                    format!("Edit {path}?")
                };
                let choices = vec![
                    (Choice::Yes, "Yes"),
                    (Choice::Always, "Yes, don't ask again this session"),
                    (Choice::No, "No, do something else"),
                ];
                self.ask(idx, Approval { call, reply, question, warning: None, preview, choices, selected: 0 });
            }
            "send_input" => {
                if autonomy == Autonomy::Auto {
                    self.send_input(idx, call, reply);
                    return;
                }
                let preview = vec![tool_target(&call)];
                let choices = vec![
                    (Choice::Yes, "Yes"),
                    (Choice::Always, "Yes, don't ask again this session"),
                    (Choice::No, "No, do something else"),
                ];
                let running = tab.pane.running_cmd().map(|c| format!(" into `{}`", first_line(c, 40))).unwrap_or_default();
                self.ask(
                    idx,
                    Approval { call, reply, question: format!("Type this{running}?"), warning: None, preview, choices, selected: 0 },
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

    fn ask(&mut self, idx: usize, approval: Approval) {
        let tab = &mut self.tabs[idx];
        tab.agent.approval = Some(approval);
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
                    _ => {
                        self.set_tool_state(idx, &ap.call.id, ToolState::Running);
                        let cwd = self.tabs[idx].pane.cwd.clone().unwrap_or_else(|| "/".into());
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
        let pane = &mut tab.pane;
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
            tab.agent.wait = Some(Wait::Command {
                reply,
                deadline: Instant::now() + Duration::from_secs(timeout),
                call_id: call.id,
                started: false,
                note,
                timeout,
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
        if tab.pane.is_busy() {
            let wait = call.u64_arg("wait_secs").unwrap_or(15).clamp(1, 300);
            a.wait = Some(Wait::Command {
                reply,
                deadline: Instant::now() + Duration::from_secs(wait),
                call_id: call.id,
                started: true,
                note: String::new(),
                timeout: wait,
            });
            a.status = "Waiting on command".into();
            return;
        }
        let _ = reply.send(ToolReply::Result {
            content: "No command is running; the terminal is at a prompt. Recent terminal content:\n".to_string()
                + &tools::clip_for_model(&tab.pane.recent_text(20)),
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
        tab.pane.scroll_to_bottom();
        tab.pane.write(bytes);
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
        match a.wait.take() {
            Some(Wait::Command { reply, call_id, started: true, note, .. }) => {
                a.detached = false;
                if let Some(item) = a.tool_mut(&call_id) {
                    item.exit = block.exit;
                    item.state = if block.exit == Some(0) { ToolState::Done } else { ToolState::Failed };
                }
                let content = tools::format_command_result(&note, block.exit, tab.pane.cwd.as_deref(), &block.output);
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
                    a.last_finished = Some(block);
                }
            }
        }
    }

    pub(super) fn agent_tick(&mut self, now: Instant) {
        for idx in 0..self.tabs.len() {
            let tab = &mut self.tabs[idx];
            let a = &mut tab.agent;
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
                        let so_far = tools::clip_for_model(&tab.pane.partial_output());
                        format!(
                            "{note}status: still running after {timeout}s\noutput so far:\n{}\n\nThe command is still running in the terminal. Use command_status to keep waiting, send_input to interact with it (key ctrl-c interrupts), or tell the user if it needs them.",
                            if so_far.trim().is_empty() { "(nothing yet)" } else { &so_far }
                        )
                    } else {
                        if let Some(item) = a.tool_mut(&call_id) {
                            item.state = ToolState::Failed;
                        }
                        format!(
                            "{note}The shell did not start the command within {timeout}s; it may be waiting for more input (an unclosed quote?) or showing a prompt. The terminal shows:\n{}",
                            tools::clip_for_model(&tab.pane.recent_text(15))
                        )
                    };
                    let _ = reply.send(ToolReply::Result { content, is_error: !started });
                    a.status = "Thinking".into();
                }
                Some(Wait::Snapshot { reply, call_id, .. }) => {
                    if let Some(item) = a.tool_mut(&call_id) {
                        item.state = ToolState::Done;
                    }
                    let state = if tab.pane.is_busy() { "a program is still running" } else { "the shell is at a prompt" };
                    let content = format!(
                        "Typed. Now {state}. The terminal shows:\n{}",
                        tools::clip_for_model(&tab.pane.recent_text(25))
                    );
                    let _ = reply.send(ToolReply::Result { content, is_error: false });
                }
                None => {}
            }
            self.mark_dirty();
        }
    }
}
