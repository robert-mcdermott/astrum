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
}

impl ProviderConfig {
    pub fn resolved_key(&self) -> Option<String> {
        if let Some(k) = &self.api_key {
            if !k.is_empty() {
                return Some(k.clone());
            }
        }
        self.api_key_env.as_ref().and_then(|v| std::env::var(v).ok()).filter(|k| !k.is_empty())
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
    /// Keep conversations on disk so they can be resumed later.
    pub save_conversations: bool,
    /// Compact the conversation by itself when the context is nearly full.
    pub auto_compact: bool,
    /// Compact once the conversation reaches this many tokens, even when the
    /// model's context window is larger.
    pub compact_limit: Option<u64>,
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
            save_conversations: true,
            auto_compact: true,
            compact_limit: None,
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
    pub scrollback: usize,
    pub shell: Option<String>,
    pub status_bar: bool,
    pub copy_on_select: bool,
    /// Force 24-bit color on or off; unset detects it from each terminal.
    pub truecolor: Option<bool>,
    pub agent: AgentConfig,
    pub providers: BTreeMap<String, ProviderConfig>,
    /// Context sizes for individual models, by model id.
    pub models: BTreeMap<String, crate::agent::models::ModelConfig>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            theme: "astrum".into(),
            theme_terminal: false,
            prefix: "ctrl+b".into(),
            agent_key: "ctrl+space".into(),
            scrollback: 10_000,
            shell: None,
            status_bar: true,
            copy_on_select: true,
            truecolor: None,
            agent: AgentConfig::default(),
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

pub const SAMPLE: &str = r#"# Astrum configuration. Every key is optional; these are the defaults.

theme = "astrum"          # run /theme inside Astrum to browse the built-in themes
theme_terminal = false    # true: recolor terminal content with the theme palette
prefix = "ctrl+b"         # then: c new tab, n/p switch, 1-9 go to, x close, d detach, ? help
agent_key = "ctrl+space"  # switch between the shell and the agent
scrollback = 10000
status_bar = true
copy_on_select = true
# truecolor = true        # default: detected per terminal; set it if colors look reduced over SSH
# shell = "/bin/zsh"      # defaults to $SHELL

[agent]
# provider = "ollama"
# model = "deepseek-v4.1-flash:cloud"
autonomy = "ask"          # ask | safe (auto-run read-only commands) | auto (autopilot)
# max_tokens = 16000   # default: the provider's own limit
command_timeout = 60
instructions = ""
save_conversations = true # keep conversations on disk for /resume and /conversations
auto_compact = true       # summarize the conversation when the model's context is nearly full
# compact_limit = 150000  # ...or once it reaches this many tokens, whichever comes first

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
"#;

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
