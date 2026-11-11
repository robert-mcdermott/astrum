//! Configuration.
//!
//! `config.toml` is the file the user edits. Choices made inside Astrum
//! (`/model`, `/theme`, autonomy) are saved to `state.toml` next to it so the
//! user's file is never rewritten.

use crate::paths;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Autonomy {
    /// Ask before every command and every file change.
    #[default]
    Ask,
    /// Run read-only commands without asking; ask for everything else.
    Safe,
    /// Autopilot: only ask for commands that look destructive.
    Auto,
}

impl Autonomy {
    pub fn next(self) -> Self {
        match self {
            Autonomy::Ask => Autonomy::Safe,
            Autonomy::Safe => Autonomy::Auto,
            Autonomy::Auto => Autonomy::Ask,
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "ask" | "manual" => Some(Autonomy::Ask),
            "safe" | "readonly" | "read-only" => Some(Autonomy::Safe),
            "auto" | "autopilot" | "yolo" => Some(Autonomy::Auto),
            _ => None,
        }
    }
}

/// Where the agent panel goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Dock {
    /// Beside the shell when the terminal is wide enough, below it otherwise.
    #[default]
    Auto,
    Bottom,
    Side,
}

/// What of a session is kept on disk to bring it back after a restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Restore {
    /// Tabs, panes, directories, and what each pane shows and remembers.
    #[default]
    All,
    /// Tabs, panes and directories; nothing of what was on screen.
    Layout,
    Off,
}

/// From this many columns, `Dock::Auto` puts the panel beside the shell.
pub const WIDE_TERMINAL: u16 = 140;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    /// Any OpenAI-compatible chat-completions endpoint, local or hosted.
    Openai,
    Anthropic,
    Bedrock,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderConfig {
    pub kind: ProviderKind,
    #[serde(default)]
    pub base_url: Option<String>,
    /// Literal API key. Prefer `api_key_env`.
    #[serde(default)]
    pub api_key: Option<String>,
    /// Name of the environment variable holding the API key.
    #[serde(default)]
    pub api_key_env: Option<String>,
    /// AWS region (Bedrock).
    #[serde(default)]
    pub region: Option<String>,
    /// AWS profile name (Bedrock).
    #[serde(default)]
    pub profile: Option<String>,
    /// Models to offer in `/model` in addition to what the endpoint lists.
    #[serde(default)]
    pub models: Vec<String>,
    /// The key found for this provider in the system's password store.
    /// Filled in while running; never read from or written to a file.
    #[serde(skip)]
    pub stored_key: Option<String>,
    /// What `api_key_env` held in the shell `astrum reload` was last run
    /// from (empty: unset there). Takes the place of the server's own
    /// environment, which is fixed when the session starts.
    #[serde(skip)]
    pub env_key: Option<String>,
}

/// Where a provider's key comes from.
#[derive(Debug, Clone, PartialEq)]
pub enum KeySource {
    ConfigFile,
    Keychain,
    Environment(String),
    None,
}

impl ProviderConfig {
    /// The key to use and where it came from: the config file first, then
    /// one stored with `/key`, then the environment variable.
    pub fn key(&self) -> (Option<String>, KeySource) {
        if let Some(k) = self.api_key.as_ref().filter(|k| !k.is_empty()) {
            return (Some(k.clone()), KeySource::ConfigFile);
        }
        if let Some(k) = self.stored_key.as_ref().filter(|k| !k.is_empty()) {
            return (Some(k.clone()), KeySource::Keychain);
        }
        if let Some(var) = &self.api_key_env {
            let value = match &self.env_key {
                Some(reloaded) => Some(reloaded.clone()),
                None => std::env::var(var).ok(),
            };
            if let Some(k) = value.filter(|k| !k.is_empty()) {
                return (Some(k), KeySource::Environment(var.clone()));
            }
        }
        (None, KeySource::None)
    }

    pub fn resolved_key(&self) -> Option<String> {
        self.key().0
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentConfig {
    pub provider: String,
    pub model: String,
    pub autonomy: Autonomy,
    /// Output-token limit per reply; unset uses each provider's own default.
    pub max_tokens: Option<u32>,
    /// Seconds to wait for a command before handing control back to the model.
    pub command_timeout: u64,
    /// Extra instructions appended to the system prompt.
    pub instructions: String,
    /// A line typed at the shell prompt that starts with this and a space
    /// goes to the agent instead of the shell; empty turns that off.
    pub ask_prefix: String,
    /// Also send lines that read as plain language to the agent.
    pub natural_language: bool,
    /// Replace keys, tokens and passwords in what the agent collects with a
    /// note before it is sent to the model.
    pub redact_secrets: bool,
    /// Commands the agent may run without asking, by their opening words.
    pub allow: Vec<String>,
    /// Commands the agent may never run.
    pub deny: Vec<String>,
    /// Keep conversations on disk so they can be resumed later.
    pub save_conversations: bool,
    /// Delete saved conversations that have not been used for this many
    /// days; 0 keeps them forever.
    pub keep_conversations_days: u64,
    /// Compact the conversation by itself when the context is nearly full.
    pub auto_compact: bool,
    /// Compact once the conversation reaches this many tokens, even when the
    /// model's context window is larger; 0 means no such cap.
    pub compact_limit: u64,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            provider: String::new(),
            model: String::new(),
            autonomy: Autonomy::Ask,
            max_tokens: None,
            command_timeout: 60,
            instructions: String::new(),
            ask_prefix: "#".into(),
            natural_language: false,
            redact_secrets: true,
            allow: Vec::new(),
            deny: Vec::new(),
            save_conversations: true,
            keep_conversations_days: 90,
            auto_compact: true,
            compact_limit: 200_000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub theme: String,
    /// Recolor terminal content with the theme's palette instead of using
    /// the host terminal's colors.
    pub theme_terminal: bool,
    pub prefix: String,
    pub agent_key: String,
    /// Where the agent panel is docked.
    pub agent_panel: Dock,
    pub scrollback: usize,
    /// What to bring back when a session that was stopped starts again.
    pub restore: Restore,
    pub shell: Option<String>,
    pub status_bar: bool,
    pub copy_on_select: bool,
    /// Force 24-bit color on or off; unset detects it from each terminal.
    pub truecolor: Option<bool>,
    pub agent: AgentConfig,
    /// Keys for Astrum's own actions, where they differ from the defaults:
    /// action name to key or keys (see `keys.rs`).
    pub keys: BTreeMap<String, crate::keys::KeySpec>,
    pub providers: BTreeMap<String, ProviderConfig>,
    /// Context sizes for individual models, by model id.
    pub models: BTreeMap<String, crate::agent::models::ModelConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            // Out of the box nothing looks different: the terminal's own
            // colors, for Astrum's bars and for what programs draw. Choosing
            // a theme then changes both together.
            theme: "terminal".into(),
            theme_terminal: true,
            prefix: "ctrl+b".into(),
            agent_key: "ctrl+space".into(),
            agent_panel: Dock::Auto,
            scrollback: 10_000,
            restore: Restore::All,
            shell: None,
            status_bar: true,
            copy_on_select: true,
            truecolor: None,
            agent: AgentConfig::default(),
            keys: BTreeMap::new(),
            providers: BTreeMap::new(),
            models: BTreeMap::new(),
        }
    }
}

/// Settings Astrum writes itself.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct State {
    pub theme: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub autonomy: Option<Autonomy>,
}

fn builtin_providers() -> BTreeMap<String, ProviderConfig> {
    let p = |kind, base_url: Option<&str>, env: Option<&str>| ProviderConfig {
        kind,
        base_url: base_url.map(str::to_string),
        api_key: None,
        api_key_env: env.map(str::to_string),
        region: None,
        profile: None,
        models: Vec::new(),
        stored_key: None,
        env_key: None,
    };
    let mut m = BTreeMap::new();
    m.insert("ollama".into(), p(ProviderKind::Openai, Some("http://localhost:11434/v1"), Some("OLLAMA_API_KEY")));
    m.insert("openai".into(), p(ProviderKind::Openai, Some("https://api.openai.com/v1"), Some("OPENAI_API_KEY")));
    m.insert(
        "anthropic".into(),
        p(ProviderKind::Anthropic, Some("https://api.anthropic.com"), Some("ANTHROPIC_API_KEY")),
    );
    m.insert("bedrock".into(), p(ProviderKind::Bedrock, None, Some("AWS_BEARER_TOKEN_BEDROCK")));
    m
}

impl Config {
    /// Load config.toml (if present), layer state.toml on top, and fill in the
    /// built-in providers. Returns warnings for anything that could not be read.
    pub fn load() -> (Config, Vec<String>) {
        let mut warnings = Vec::new();
        let path = paths::config_file();
        let mut cfg = match std::fs::read_to_string(&path) {
            Ok(text) => match toml::from_str::<Config>(&text) {
                Ok(c) => c,
                Err(e) => {
                    warnings.push(format!("{}: {e}", path.display()));
                    Config::default()
                }
            },
            Err(_) => Config::default(),
        };
        for (name, builtin) in builtin_providers() {
            cfg.providers.entry(name).or_insert(builtin);
        }
        let state = State::load();
        if let Some(t) = state.theme {
            cfg.theme = t;
        }
        if let (Some(p), Some(m)) = (state.provider, state.model) {
            if cfg.providers.contains_key(&p) {
                cfg.agent.provider = p;
                cfg.agent.model = m;
            }
        }
        if let Some(a) = state.autonomy {
            cfg.agent.autonomy = a;
        }
        (cfg, warnings)
    }

    pub fn save_state(&self) {
        let state = State {
            theme: Some(self.theme.clone()),
            provider: Some(self.agent.provider.clone()).filter(|s| !s.is_empty()),
            model: Some(self.agent.model.clone()).filter(|s| !s.is_empty()),
            // Autopilot is never remembered implicitly: a new session starts
            // cautious unless config.toml asks for it.
            autonomy: Some(self.agent.autonomy).filter(|a| *a != Autonomy::Auto),
        };
        if let Ok(text) = toml::to_string_pretty(&state) {
            let dir = paths::config_dir();
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::write(dir.join("state.toml"), text);
        }
    }
}

impl State {
    fn load() -> State {
        std::fs::read_to_string(paths::config_dir().join("state.toml"))
            .ok()
            .and_then(|t| toml::from_str(&t).ok())
            .unwrap_or_default()
    }
}

/// Terminals send the same control byte for Ctrl+\ and Ctrl+4 (likewise
/// ] and 5, ^ and 6, _ and 7), and which name the event carries depends on
/// the terminal. Treat each pair as one key.
fn ctrl_alias(c: char, ctrl: bool) -> char {
    match c {
        '4' if ctrl => '\\',
        '5' if ctrl => ']',
        '6' if ctrl => '^',
        '7' if ctrl => '_',
        other => other,
    }
}

/// A key chord such as `ctrl+b` or `ctrl+space`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chord {
    pub code: KeyCode,
    pub mods: KeyModifiers,
}

impl Chord {
    pub fn parse(spec: &str) -> Option<Chord> {
        let mut mods = KeyModifiers::NONE;
        let mut code = None;
        for part in spec.split('+') {
            let part = part.trim().to_ascii_lowercase();
            match part.as_str() {
                "ctrl" | "control" => mods |= KeyModifiers::CONTROL,
                "alt" | "option" | "meta" => mods |= KeyModifiers::ALT,
                "shift" => mods |= KeyModifiers::SHIFT,
                "space" => code = Some(KeyCode::Char(' ')),
                "enter" | "return" => code = Some(KeyCode::Enter),
                "tab" => code = Some(KeyCode::Tab),
                "esc" | "escape" => code = Some(KeyCode::Esc),
                "backslash" => code = Some(KeyCode::Char('\\')),
                p if p.len() > 1 && p.starts_with('f') => {
                    code = Some(KeyCode::F(p[1..].parse().ok()?));
                }
                p => {
                    let mut chars = p.chars();
                    let c = chars.next()?;
                    if chars.next().is_some() {
                        return None;
                    }
                    code = Some(KeyCode::Char(c));
                }
            }
        }
        Some(Chord { code: code?, mods })
    }

    pub fn matches(&self, key: &KeyEvent) -> bool {
        let relevant = KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT;
        let mut mods = key.modifiers & relevant;
        let ctrl = mods.contains(KeyModifiers::CONTROL);
        let code = match key.code {
            KeyCode::Char(c) => {
                // Shift is already folded into the character itself.
                if !self.mods.contains(KeyModifiers::SHIFT) {
                    mods.remove(KeyModifiers::SHIFT);
                }
                KeyCode::Char(ctrl_alias(c.to_ascii_lowercase(), ctrl))
            }
            other => other,
        };
        let wanted = match self.code {
            KeyCode::Char(c) => KeyCode::Char(ctrl_alias(c, self.mods.contains(KeyModifiers::CONTROL))),
            other => other,
        };
        code == wanted && mods == self.mods
    }

    /// Short human label, e.g. `^B` or `^Space`.
    pub fn label(&self) -> String {
        let mut s = String::new();
        if self.mods.contains(KeyModifiers::CONTROL) {
            s.push('^');
        }
        if self.mods.contains(KeyModifiers::ALT) {
            s.push_str("Alt+");
        }
        if self.mods.contains(KeyModifiers::SHIFT) {
            s.push_str("Shift+");
        }
        match self.code {
            KeyCode::Char(' ') => s.push_str("Space"),
            KeyCode::Char(c) => s.push(c.to_ascii_uppercase()),
            KeyCode::F(n) => s.push_str(&format!("F{n}")),
            KeyCode::Enter => s.push_str("Enter"),
            KeyCode::Tab => s.push_str("Tab"),
            KeyCode::Esc => s.push_str("Esc"),
            _ => s.push('?'),
        }
        s
    }
}

pub const SAMPLE: &str = r##"# Astrum configuration. Every key is optional; these are the defaults.

theme = "terminal"        # your terminal's own colors; run /theme inside Astrum to browse the built-in themes
theme_terminal = true     # a chosen theme also recolors what programs draw; false: only Astrum's own bars and panel
prefix = "ctrl+b"         # then: c new tab, n/p switch, 1-9 go to, x close, d detach, ? help
agent_key = "ctrl+space"  # switch between the shell and the agent
agent_panel = "auto"      # auto: beside the shell on a wide terminal, below it otherwise | bottom | side
scrollback = 10000
restore = "all"           # what a stopped session brings back when it starts again:
                          # all: tabs, panes, directories and what was on screen |
                          # layout: not what was on screen (none of it is kept on disk) | off
status_bar = true
copy_on_select = true
# truecolor = true        # default: detected per terminal; set it if colors look reduced over SSH

# Keys for the things Astrum does. A plain key is pressed after the prefix;
# one with ctrl or alt works by itself. A list gives several keys; "none"
# takes the key away. `astrum keys` lists every action and its keys.
# [keys]
# new_tab = ["c", "ctrl+t"]
# split_right = "v"
# next_tab = "alt+right"
# prev_tab = "alt+left"
# shell = "/bin/zsh"      # defaults to $SHELL

[agent]
# provider = "ollama"
# model = "deepseek-v4.1-flash:cloud"
autonomy = "ask"          # ask | safe (auto-run read-only commands) | auto (autopilot)
# max_tokens = 16000   # default: the provider's own limit
command_timeout = 60
instructions = ""
redact_secrets = true     # hide keys, tokens and passwords in output and files from the model
# Commands the agent may run without asking, and commands it may never run,
# by their opening words ("git push" covers "git push origin main"; * stands
# for anything). Destructive commands still ask, and deny wins over allow.
# allow = ["cargo test", "git commit", "npm run *test*"]
# deny = ["git push", "terraform apply"]
ask_prefix = "#"          # at the shell prompt, "# question" asks the agent; "" turns that off
natural_language = false  # true: lines that read as plain language ("how do I ...") go to the agent too
save_conversations = true # keep conversations on disk for /resume and /conversations
keep_conversations_days = 90 # delete saved conversations unused for this long; 0 keeps them forever
auto_compact = true       # summarize the conversation when the model's context is nearly full
compact_limit = 200000    # ...or once it reaches this many tokens, whichever comes first; 0 for no cap

# Built-in providers: ollama, openai, anthropic, bedrock. Override them or add
# your own. Any OpenAI-compatible endpoint works with kind = "openai".
#
# [providers.ollama]
# kind = "openai"
# base_url = "http://localhost:11434/v1"
#
# [providers.anthropic]
# kind = "anthropic"
# api_key_env = "ANTHROPIC_API_KEY"
#
# [providers.work]
# kind = "openai"
# base_url = "https://llm.example.com/v1"
# api_key_env = "WORK_LLM_KEY"
# models = ["my-model"]
#
# [providers.bedrock]
# kind = "bedrock"
# region = "us-west-2"
# profile = "default"      # an AWS profile from ~/.aws; or set AWS_BEARER_TOKEN_BEDROCK

# How much context a model accepts, in tokens. Astrum compacts a conversation
# before it gets there. Unless set here it assumes 128000 for OpenAI-compatible
# models and 200000 for Anthropic and Amazon Bedrock. Use "provider/model" as
# the name to cover one provider only.
#
# [models."deepseek-v4.1-flash:cloud"]
# context_window = 128000
"##;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chords() {
        let c = Chord::parse("ctrl+b").unwrap();
        assert!(c.matches(&KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL)));
        assert!(!c.matches(&KeyEvent::new(KeyCode::Char('b'), KeyModifiers::NONE)));
        let s = Chord::parse("ctrl+space").unwrap();
        assert!(s.matches(&KeyEvent::new(KeyCode::Char(' '), KeyModifiers::CONTROL)));
        assert_eq!(s.label(), "^Space");
        assert!(Chord::parse("f2").unwrap().matches(&KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE)));
        // Ctrl+\ arrives as Ctrl+4 from terminals without enhanced key reporting.
        let b = Chord::parse("ctrl+backslash").unwrap();
        assert!(b.matches(&KeyEvent::new(KeyCode::Char('4'), KeyModifiers::CONTROL)));
        assert!(b.matches(&KeyEvent::new(KeyCode::Char('\\'), KeyModifiers::CONTROL)));
        assert!(!b.matches(&KeyEvent::new(KeyCode::Char('4'), KeyModifiers::NONE)));
    }

    #[test]
    fn sample_config_parses() {
        let cfg: Config = toml::from_str(SAMPLE).unwrap();
        assert_eq!(cfg.prefix, "ctrl+b");
        assert_eq!(cfg.agent.autonomy, Autonomy::Ask);
    }
}
