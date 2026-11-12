//! The settings screen: the options most people want to change, each a row
//! to click. A change takes effect at once and is remembered in
//! `state.toml`, where it takes the place of the config file's value.
//! Everything else is in the config file, which the screen points to.

use super::{Overlay, Server};
use crate::config::{Autonomy, Dock, Restore};
use crate::paths;

/// One row of the settings screen.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Setting {
    Theme,
    Recolor,
    Panel,
    CopyOnSelect,
    Model,
    Autonomy,
    AskFromShell,
    PlainLanguage,
    Redact,
    AutoCompact,
    SaveConversations,
    Mcp,
    Restore,
    Walkthrough,
    Forget,
}

/// The rows, in order, under their headings.
pub const SECTIONS: &[(&str, &[Setting])] = &[
    ("Looks", &[Setting::Theme, Setting::Recolor, Setting::Panel, Setting::CopyOnSelect]),
    (
        "Agent",
        &[Setting::Model, Setting::Autonomy, Setting::AskFromShell, Setting::PlainLanguage, Setting::Redact, Setting::AutoCompact, Setting::SaveConversations, Setting::Mcp],
    ),
    ("Sessions", &[Setting::Restore]),
    ("More", &[Setting::Walkthrough, Setting::Forget]),
];

/// Every row, top to bottom.
pub fn rows() -> Vec<Setting> {
    SECTIONS.iter().flat_map(|(_, rows)| rows.iter().copied()).collect()
}

/// The settings screen while it is open.
pub struct SettingsView {
    /// Which row the keyboard is on.
    pub selected: usize,
}

fn on_off(on: bool) -> String {
    if on { "on" } else { "off" }.to_string()
}

impl Setting {
    pub fn label(self) -> &'static str {
        match self {
            Setting::Theme => "Theme",
            Setting::Recolor => "Programs use the theme's colors too",
            Setting::Panel => "Agent panel sits",
            Setting::CopyOnSelect => "Copy text as soon as it is selected",
            Setting::Model => "Model",
            Setting::Autonomy => "The agent may",
            Setting::AskFromShell => "Ask from the shell prompt with \"# question\"",
            Setting::PlainLanguage => "Plain-language lines at the prompt go to the agent",
            Setting::Redact => "Hide keys and passwords from the model",
            Setting::AutoCompact => "Summarize long conversations when they fill up",
            Setting::SaveConversations => "Keep conversations so they can be resumed",
            Setting::Mcp => "MCP servers",
            Setting::Restore => "A stopped session comes back with",
            Setting::Walkthrough => "Show the walkthrough",
            Setting::Forget => "Forget the choices made here",
        }
    }

    /// Does choosing the row open something, rather than change a value?
    pub fn opens(self) -> bool {
        matches!(self, Setting::Theme | Setting::Model | Setting::Mcp | Setting::Walkthrough | Setting::Forget)
    }
}

impl Server {
    pub fn open_settings(&mut self) {
        self.ui.overlay = Some(Overlay::Settings(SettingsView { selected: 0 }));
        self.dirty = true;
    }

    /// What a row shows as its value now.
    pub fn setting_value(&self, setting: Setting) -> String {
        let cfg = &self.cfg;
        match setting {
            Setting::Theme => self.theme.label.to_string(),
            Setting::Recolor => on_off(cfg.theme_terminal),
            Setting::Panel => match cfg.agent_panel {
                Dock::Auto => "beside the shell when there is room".into(),
                Dock::Bottom => "below the shell".into(),
                Dock::Side => "beside the shell".into(),
            },
            Setting::CopyOnSelect => on_off(cfg.copy_on_select),
            Setting::Model if cfg.agent.model.is_empty() => "choose one".into(),
            Setting::Model => cfg.agent.model.clone(),
            Setting::Autonomy => match cfg.agent.autonomy {
                Autonomy::Ask => "ask before everything".into(),
                Autonomy::Safe => "read by itself, ask before changes".into(),
                Autonomy::Auto => "do everything by itself (autopilot)".into(),
            },
            Setting::AskFromShell => on_off(!cfg.agent.ask_prefix.is_empty()),
            Setting::PlainLanguage => on_off(cfg.agent.natural_language),
            Setting::Redact => on_off(cfg.agent.redact_secrets),
            Setting::AutoCompact => on_off(cfg.agent.auto_compact),
            Setting::SaveConversations => on_off(cfg.agent.save_conversations),
            Setting::Restore => match cfg.restore {
                Restore::All => "its tabs and what was on screen".into(),
                Restore::Layout => "its tabs only".into(),
                Restore::Off => "nothing".into(),
            },
            Setting::Mcp => self.mcp_summary(),
            Setting::Walkthrough | Setting::Forget => String::new(),
        }
    }

    /// Choose a row: change its value to the next one (or the one before,
    /// going `back`), or open what it opens.
    pub fn change_setting(&mut self, setting: Setting, back: bool) {
        match setting {
            Setting::Theme => return self.open_theme_picker(),
            Setting::Model => return self.open_model_picker(),
            Setting::Mcp => return self.open_mcp_picker(),
            Setting::Walkthrough => return self.open_walkthrough(),
            Setting::Forget => {
                // Back to what the config file says, for everything.
                self.cfg.state = crate::config::State { welcomed: self.cfg.state.welcomed, ..Default::default() };
                self.cfg.save_state_alone();
                // Autopilot chosen here is one of those choices.
                self.autopilot_here = false;
                match self.reload_config() {
                    Ok(_) => self.toast(format!("Settings are as {} has them", paths::tildify(&paths::config_file().to_string_lossy()))),
                    Err(problem) => self.toast(format!("Config problem: {problem}")),
                }
                return;
            }
            Setting::Autonomy => {
                let order = [Autonomy::Ask, Autonomy::Safe, Autonomy::Auto];
                let at = order.iter().position(|a| *a == self.cfg.agent.autonomy).unwrap_or(0);
                let next = order[(at + if back { order.len() - 1 } else { 1 }) % order.len()];
                // (Says what the level means, and remembers it unless it is autopilot.)
                self.set_autonomy(next);
                self.ui.toast = None;
                return;
            }
            _ => {}
        }
        let cfg = &mut self.cfg;
        match setting {
            Setting::Recolor => {
                cfg.theme_terminal = !cfg.theme_terminal;
                cfg.state.theme_terminal = Some(cfg.theme_terminal);
            }
            Setting::Panel => {
                let order = [Dock::Auto, Dock::Bottom, Dock::Side];
                let at = order.iter().position(|d| *d == cfg.agent_panel).unwrap_or(0);
                cfg.agent_panel = order[(at + if back { order.len() - 1 } else { 1 }) % order.len()];
                cfg.state.agent_panel = Some(cfg.agent_panel);
            }
            Setting::CopyOnSelect => {
                cfg.copy_on_select = !cfg.copy_on_select;
                cfg.state.copy_on_select = Some(cfg.copy_on_select);
            }
            Setting::AskFromShell => {
                // Off is no prefix. On is the prefix the config file
                // gives, if it gives one, and `#` otherwise.
                let on = cfg.agent.ask_prefix.is_empty();
                let own = crate::config::Config::ask_prefix_in_file().filter(|prefix| !prefix.is_empty());
                cfg.agent.ask_prefix = if on { own.unwrap_or_else(|| "#".into()) } else { String::new() };
                cfg.state.ask_prefix = Some(cfg.agent.ask_prefix.clone());
            }
            Setting::PlainLanguage => {
                cfg.agent.natural_language = !cfg.agent.natural_language;
                cfg.state.natural_language = Some(cfg.agent.natural_language);
            }
            Setting::Redact => {
                cfg.agent.redact_secrets = !cfg.agent.redact_secrets;
                cfg.state.redact_secrets = Some(cfg.agent.redact_secrets);
            }
            Setting::AutoCompact => {
                cfg.agent.auto_compact = !cfg.agent.auto_compact;
                cfg.state.auto_compact = Some(cfg.agent.auto_compact);
            }
            Setting::SaveConversations => {
                cfg.agent.save_conversations = !cfg.agent.save_conversations;
                cfg.state.save_conversations = Some(cfg.agent.save_conversations);
            }
            Setting::Restore => {
                let order = [Restore::All, Restore::Layout, Restore::Off];
                let at = order.iter().position(|r| *r == cfg.restore).unwrap_or(0);
                cfg.restore = order[(at + if back { order.len() - 1 } else { 1 }) % order.len()];
                cfg.state.restore = Some(cfg.restore);
            }
            _ => {}
        }
        self.cfg.save_state();
        // The session on disk follows what is to be kept of it.
        if setting == Setting::Restore {
            self.schedule_save();
        }
        self.force_redraw();
    }

    /// A key while the settings are open.
    pub(super) fn settings_key(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::KeyCode;
        let all = rows();
        let Some(Overlay::Settings(view)) = &mut self.ui.overlay else { return };
        match key.code {
            KeyCode::Up | KeyCode::BackTab | KeyCode::Char('k') => view.selected = (view.selected + all.len() - 1) % all.len(),
            KeyCode::Down | KeyCode::Tab | KeyCode::Char('j') => view.selected = (view.selected + 1) % all.len(),
            KeyCode::Enter | KeyCode::Char(' ') | KeyCode::Right => {
                let setting = all[view.selected.min(all.len() - 1)];
                self.change_setting(setting, false);
            }
            KeyCode::Left => {
                let setting = all[view.selected.min(all.len() - 1)];
                if !setting.opens() {
                    self.change_setting(setting, true);
                }
            }
            KeyCode::Esc | KeyCode::Char('q') => self.ui.overlay = None,
            _ => {}
        }
    }

    /// A click on a row of the settings.
    pub(super) fn settings_click(&mut self, row: usize) {
        let all = rows();
        let Some(setting) = all.get(row).copied() else { return };
        if let Some(Overlay::Settings(view)) = &mut self.ui.overlay {
            view.selected = row;
        }
        self.change_setting(setting, false);
    }
}
