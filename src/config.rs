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

/// An MCP server: a program on this machine, talked to over its input and
/// output (`command`), or an address on the network (`url`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct McpServer {
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    /// More of the environment for the program, on top of the little it
    /// is given anyway. `${NAME}` in a value is that variable of Astrum's.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Where the program runs; the home folder unless set.
    pub cwd: Option<String>,
    pub url: Option<String>,
    /// Sent with every request to a server on the network (`${NAME}` as
    /// for `env`).
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// For a program: the environment variable a token stored with
    /// `/mcp key` is given to it in.
    pub token_env: Option<String>,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Which of its tools to offer the agent (`*` for anything); all of
    /// them unless set.
    #[serde(default)]
    pub tools: Vec<String>,
    /// Which of its tools may be used without asking.
    #[serde(default)]
    pub allow: Vec<String>,
    /// How long a tool may take, in seconds (default 120).
    pub timeout: Option<u64>,
    /// For signing in with OAuth: a client id the server has given you,
    /// where it does not register one by itself, and the scope to ask for.
    pub client_id: Option<String>,
    pub scope: Option<String>,
}

fn yes() -> bool {
    true
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
    /// Folders of skills, beside the user's own (`skills/` in the config
    /// directory) and a project's.
    pub skill_dirs: Vec<String>,
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
            skill_dirs: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub theme: String,
    /// The theme to use instead when the terminal's own background is
    /// light, for a terminal that changes with the time of day.
    pub theme_light: Option<String>,
    /// Themes of the user's own, by name.
    pub themes: BTreeMap<String, crate::theme::UserTheme>,
    /// What has been chosen inside Astrum, as it is kept in `state.toml`.
    #[serde(skip)]
    pub state: State,
    /// Nothing of Astrum's was found when the settings were read: this is
    /// the first time it is used here.
    #[serde(skip)]
    pub first_run: bool,
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
    /// Show the pictures programs print, on terminals that can.
    pub images: bool,
    /// Where the release files are, in place of GitHub, when Astrum is to
    /// be copied to another machine: an https:// address, or file:// and a
    /// directory on this machine.
    pub download_base: Option<String>,
    /// Force 24-bit color on or off; unset detects it from each terminal.
    pub truecolor: Option<bool>,
    pub agent: AgentConfig,
    /// Keys for Astrum's own actions, where they differ from the defaults:
    /// action name to key or keys (see `keys.rs`).
    pub keys: BTreeMap<String, crate::keys::KeySpec>,
    pub providers: BTreeMap<String, ProviderConfig>,
    /// Context sizes for individual models, by model id.
    pub models: BTreeMap<String, crate::agent::models::ModelConfig>,
    /// MCP servers, by the name the agent knows their tools by.
    pub mcp: BTreeMap<String, McpServer>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            // Out of the box nothing looks different: the terminal's own
            // colors, for Astrum's bars and for what programs draw. Choosing
            // a theme then changes both together.
            theme: "terminal".into(),
            theme_light: None,
            themes: BTreeMap::new(),
            state: State::default(),
            first_run: false,
            theme_terminal: true,
            prefix: "ctrl+b".into(),
            agent_key: "ctrl+space".into(),
            agent_panel: Dock::Auto,
            scrollback: 10_000,
            restore: Restore::All,
            shell: None,
            status_bar: true,
            copy_on_select: true,
            images: true,
            download_base: None,
            truecolor: None,
            agent: AgentConfig::default(),
            keys: BTreeMap::new(),
            providers: BTreeMap::new(),
            models: BTreeMap::new(),
            mcp: BTreeMap::new(),
        }
    }
}

/// Settings Astrum writes itself: what was chosen inside it, and only
/// that. Each takes the place of the config file's value.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct State {
    pub theme: Option<String>,
    pub theme_light: Option<String>,
    pub theme_terminal: Option<bool>,
    pub agent_panel: Option<Dock>,
    pub copy_on_select: Option<bool>,
    pub restore: Option<Restore>,
    pub ask_prefix: Option<String>,
    pub natural_language: Option<bool>,
    pub redact_secrets: Option<bool>,
    pub auto_compact: Option<bool>,
    pub save_conversations: Option<bool>,
    /// The walkthrough has been seen (or skipped).
    pub welcomed: bool,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub autonomy: Option<Autonomy>,
    /// Skills turned off (by name). `None` until something is turned off
    /// here: then another session's choice is not undone by this one's.
    pub skills_off: Option<Vec<String>>,
}

impl State {
    /// The skills turned off.
    pub fn skills_off(&self) -> &[String] {
        self.skills_off.as_deref().unwrap_or(&[])
    }
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

/// What makes an MCP server's entry unusable, if anything.
fn mcp_problem(name: &str, server: &McpServer) -> Option<String> {
    if !crate::mcp::valid_name(name) {
        return Some("a server's name may have letters, digits, `-` and `_`, and at most 32 of them".into());
    }
    match (&server.command, &server.url) {
        (Some(_), Some(_)) => Some("it has both `command` and `url`: a server is one or the other".into()),
        (None, None) => Some("it needs a `command` to run, or the `url` of a server on the network".into()),
        (Some(command), None) if command.trim().is_empty() => Some("`command` is empty".into()),
        (None, Some(url)) if !(url.starts_with("https://") || url.starts_with("http://")) => Some("`url` must begin with https:// (or http:// for this machine)".into()),
        (None, Some(url)) if url.starts_with("http://") && !crate::mcp::http::is_this_machine(url) => {
            Some("a server elsewhere is only reached over https://, so that what is sent to it (a token, your data) cannot be read on the way".into())
        }
        _ => None,
    }
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
        // An MCP server that cannot be used as it is written is left out, with a word.
        cfg.mcp.retain(|name, server| match mcp_problem(name, server) {
            Some(problem) => {
                warnings.push(format!("[mcp.{name}]: {problem}; it is left out"));
                false
            }
            None => true,
        });
        // Neither file there: Astrum has not been used here before.
        cfg.first_run = !path.exists() && !paths::config_dir().join("state.toml").exists();
        cfg.state = State::load();
        cfg.apply_state();
        (cfg, warnings)
    }

    /// Put what was chosen inside Astrum over what the config file says.
    fn apply_state(&mut self) {
        let state = self.state.clone();
        if let Some(t) = state.theme {
            self.theme = t;
        }
        if let Some(t) = state.theme_light {
            self.theme_light = Some(t);
        }
        if let (Some(p), Some(m)) = (state.provider, state.model) {
            if self.providers.contains_key(&p) {
                self.agent.provider = p;
                self.agent.model = m;
            }
        }
        if let Some(a) = state.autonomy {
            self.agent.autonomy = a;
        }
        if let Some(v) = state.theme_terminal {
            self.theme_terminal = v;
        }
        if let Some(v) = state.agent_panel {
            self.agent_panel = v;
        }
        if let Some(v) = state.copy_on_select {
            self.copy_on_select = v;
        }
        if let Some(v) = state.restore {
            self.restore = v;
        }
        if let Some(v) = state.ask_prefix {
            self.agent.ask_prefix = v;
        }
        if let Some(v) = state.natural_language {
            self.agent.natural_language = v;
        }
        if let Some(v) = state.redact_secrets {
            self.agent.redact_secrets = v;
        }
        if let Some(v) = state.auto_compact {
            self.agent.auto_compact = v;
        }
        if let Some(v) = state.save_conversations {
            self.agent.save_conversations = v;
        }
    }

    /// Write out what has been chosen inside Astrum. Only what was chosen:
    /// a setting nobody touched here stays the config file's to decide.
    ///
    /// Every session writes the same file. What another session has chosen
    /// since this one read it is kept, wherever this one has made no
    /// choice of its own.
    pub fn save_state(&self) {
        self.write_state(true);
    }

    /// The `ask_prefix` the config file itself gives, whatever has been
    /// chosen since inside Astrum.
    pub fn ask_prefix_in_file() -> Option<String> {
        let text = std::fs::read_to_string(paths::config_file()).ok()?;
        let table = text.parse::<toml::Table>().ok()?;
        table.get("agent")?.get("ask_prefix")?.as_str().map(str::to_string)
    }

    /// Write out the choices as they are here and nothing else: for going
    /// back to what the config file says.
    pub fn save_state_alone(&self) {
        self.write_state(false);
    }

    fn write_state(&self, keep_others: bool) {
        let dir = paths::config_dir();
        let file = dir.join("state.toml");
        let Ok(mut table) = toml::Table::try_from(&self.state) else { return };
        if keep_others {
            let theirs = std::fs::read_to_string(&file).ok().and_then(|text| text.parse::<toml::Table>().ok()).unwrap_or_default();
            for (key, value) in theirs {
                table.entry(key).or_insert(value);
            }
        }
        if let Ok(text) = toml::to_string_pretty(&table) {
            let _ = std::fs::create_dir_all(&dir);
            let _ = std::fs::write(file, text);
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
pub fn ctrl_alias(c: char, ctrl: bool) -> char {
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
# theme_light = "catppuccin-latte"   # the theme to use instead when the terminal's background is light
prefix = "ctrl+b"         # the key pressed before another: c new tab, n/p switch, 1-9 go to, x close, d detach, ? help
agent_key = "ctrl+space"  # switch between the shell and the agent
agent_panel = "auto"      # auto: beside the shell on a wide terminal, below it otherwise | bottom | side
scrollback = 10000        # lines of history kept for each pane
restore = "all"          # what a stopped session brings back when it starts again:
                          # all: tabs, panes, directories and what was on screen |
                          # layout: not what was on screen (none of it is kept on disk) | off
status_bar = true         # the bar along the bottom: the menu, what the agent is doing, hints
copy_on_select = true     # text selected with the mouse is copied when the button is let go
images = true            # show pictures that programs print, where the terminal can; false: a note in their place
# download_base = "file:///path/to/release"   # where Astrum's release files are, when it is copied to another machine (default: GitHub)
# truecolor = true        # default: detected per terminal; set it if colors look reduced over SSH
# shell = "/bin/zsh"      # the shell a new tab starts; default: $SHELL

# A theme of your own. Every color is optional; what is left out comes from
# the theme it is based on. `astrum themes` lists the names to build on.
# [themes.mine]
# label = "Mine"
# based_on = "matrix"
# bg = "#000000"
# text = "#33ff66"
# accent = "#33ff66"

# Keys for the things Astrum does. A plain key is pressed after the prefix;
# one with ctrl or alt works by itself. A list gives several keys; "none"
# takes the key away. `astrum keys` lists every action and its keys.
# [keys]
# new_tab = ["c", "ctrl+t"]
# split_right = "v"
# next_tab = "alt+right"
# prev_tab = "alt+left"

[agent]
# provider = "ollama"     # the provider the agent uses; default: the one chosen with /model
# model = "deepseek-v4.1-flash:cloud"   # the model it asks; default: the one chosen with /model
autonomy = "ask"         # ask | safe (auto-run read-only commands) | auto (autopilot)
# max_tokens = 16000      # the longest reply, in tokens; default: the provider's own limit
command_timeout = 60      # seconds the agent waits on a command before it hears that it is still running
instructions = ""         # more instructions for the agent, added to every conversation
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
# Skills are folders with a SKILL.md: yours go in skills/ beside this file, a
# project's in its .agents/skills/. These folders are read as well.
# skill_dirs = ["~/shared-skills"]

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

# MCP servers give the agent tools of their own. A program on this machine,
# talked to over its input and output; or a server on the network. Its tools
# are offered beside the agent's own, and using one is asked about as a
# command is, unless it is in `allow`. `/mcp` shows how each is doing.
#
# [mcp.files]
# command = "npx"
# args = ["-y", "@modelcontextprotocol/server-filesystem", "/Users/me/projects"]
# allow = ["read_*", "list_*", "search_*"]   # tools used without asking
#
# [mcp.github]
# command = "github-mcp-server"
# args = ["stdio"]
# token_env = "GITHUB_PERSONAL_ACCESS_TOKEN"   # given the token stored with /mcp key github
# tools = ["get_*", "list_*", "search_*"]       # only these are offered
#
# [mcp.docs]
# url = "https://mcp.example.com/mcp"   # signs in with OAuth if it asks; or /mcp key docs for a token
# timeout = 120                          # seconds a tool may take
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
        // Everything set at the top of the file comes before its first
        // table: a line uncommented below one would become part of it.
        let first_table = SAMPLE.lines().position(|l| l.trim_start_matches("# ").starts_with('[')).unwrap();
        let top_level = ["prefix", "agent_key", "scrollback", "restore", "shell", "status_bar", "images", "truecolor", "theme "];
        for (at, line) in SAMPLE.lines().enumerate().filter(|(_, l)| top_level.iter().any(|key| l.trim_start_matches("# ").starts_with(key))) {
            assert!(at < first_table, "`{line}` comes after a table in the sample config");
        }
        let cfg: Config = toml::from_str(SAMPLE).unwrap();
        assert_eq!(cfg.prefix, "ctrl+b");
        assert_eq!(cfg.agent.autonomy, Autonomy::Ask);
    }

    /// The website's list of settings is made from the sample, so every
    /// setting has to be in it, with a word on what it does.
    #[test]
    fn sample_config_has_every_setting() {
        let lines: Vec<&str> = SAMPLE.lines().map(|l| l.trim_start_matches("# ")).collect();
        let named = |key: &str| lines.iter().any(|l| l.starts_with(&format!("{key} ")) || l.starts_with(&format!("{key}=")));
        let table = |key: &str| lines.iter().any(|l| l.starts_with(&format!("[{key}]")) || l.starts_with(&format!("[{key}.")));
        let all = serde_json::to_value(Config::default()).unwrap();
        for (key, value) in all.as_object().unwrap() {
            if key == "agent" {
                for key in value.as_object().unwrap().keys() {
                    assert!(named(key), "`agent.{key}` is missing from the sample config");
                }
            } else if value.is_object() {
                assert!(table(key), "`[{key}]` is missing from the sample config");
            } else {
                assert!(named(key), "`{key}` is missing from the sample config");
            }
        }
    }
}
