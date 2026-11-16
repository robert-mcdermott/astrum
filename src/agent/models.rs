//! What Astrum knows about individual models: how much context they accept.
//! The config file's `[models."id"]` tables come first; then what the
//! provider itself reports, where it does (see `llm::context_window`); then
//! a cautious guess.

use crate::config::{Config, ProviderConfig, ProviderKind};
use serde::{Deserialize, Serialize};

/// Per-model settings from the config file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ModelConfig {
    /// Tokens of context the model accepts.
    pub context_window: Option<u64>,
    /// How hard the model is asked to think, until another level is
    /// chosen inside Astrum.
    pub effort: Option<Effort>,
}

/// How hard a model is asked to think. `Default` asks for nothing, and
/// the model runs at its provider's own level.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    #[default]
    Default,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl Effort {
    pub fn name(self) -> &'static str {
        match self {
            Effort::Default => "default",
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
            Effort::Xhigh => "xhigh",
            Effort::Max => "max",
        }
    }

    pub fn named(name: &str) -> Option<Effort> {
        [Effort::Default, Effort::Low, Effort::Medium, Effort::High, Effort::Xhigh, Effort::Max].into_iter().find(|e| e.name() == name.trim().to_lowercase())
    }

    /// What goes in a request: nothing for `Default`.
    pub fn sent(self) -> Option<&'static str> {
        (self != Effort::Default).then(|| self.name())
    }

    /// What each level is for, in a few words.
    pub fn about(self) -> &'static str {
        match self {
            Effort::Default => "the model's own level",
            Effort::Low => "quickest, for simple work",
            Effort::Medium => "a balance of speed and care",
            Effort::High => "careful, for hard work",
            Effort::Xhigh => "more careful, slower, dearer",
            Effort::Max => "all the model has, at any cost",
        }
    }
}

/// Is this a Claude model that decides for itself how much to think (and
/// so takes a level)? Those before it do not.
pub fn adaptive_claude(model: &str) -> bool {
    // (`-4-2025…` is how the first 4 models are named: a date follows the 4.)
    !(model.contains("-4-5") || model.contains("-4-1") || model.contains("-4-2025") || model.contains("claude-3"))
}

/// The levels a model can be asked for, `Default` aside. None: the model
/// has no levels, as far as Astrum knows.
pub fn levels(kind: ProviderKind, model: &str) -> &'static [Effort] {
    use Effort::*;
    let claude = |model: &str| -> &'static [Effort] {
        if !adaptive_claude(model) {
            &[]
        } else if model.contains("-4-6") {
            &[Low, Medium, High, Max]
        } else {
            &[Low, Medium, High, Xhigh, Max]
        }
    };
    match kind {
        ProviderKind::Anthropic => claude(model),
        ProviderKind::Bedrock if model.contains("anthropic.claude") => claude(model),
        ProviderKind::Bedrock => &[],
        ProviderKind::Openai => &[Low, Medium, High],
    }
}

/// The level in force for a model: what was chosen inside Astrum, or else
/// what the config file says, where the model takes that level.
pub fn effort(cfg: &Config, provider_name: &str, model: &str) -> Effort {
    let key = format!("{provider_name}/{model}");
    let wanted = cfg.state.effort.get(&key).copied().or_else(|| cfg.models.get(&key).or_else(|| cfg.models.get(model)).and_then(|m| m.effort)).unwrap_or_default();
    let offered = cfg.providers.get(provider_name).map(|p| levels(p.kind, model)).unwrap_or(&[]);
    if offered.contains(&wanted) { wanted } else { Effort::Default }
}

/// What the config file says a model's context window is, under
/// `provider/model` or `model`.
pub fn configured_window(cfg: &Config, provider_name: &str, model: &str) -> Option<u64> {
    let user = cfg.models.get(&format!("{provider_name}/{model}")).or_else(|| cfg.models.get(model));
    user.and_then(|m| m.context_window).filter(|w| *w > 0)
}

/// A size that is safe to assume when nothing says otherwise. Guessing low
/// only means compacting earlier than needed.
pub fn default_window(provider: &ProviderConfig) -> u64 {
    match provider.kind {
        ProviderKind::Anthropic | ProviderKind::Bedrock => 200_000,
        ProviderKind::Openai => 128_000,
    }
}

/// `1234` → `1.2k`, `1_000_000` → `1M`.
pub fn tokens(n: u64) -> String {
    let short = |v: f64, unit: &str| {
        let s = format!("{v:.1}");
        format!("{}{unit}", s.strip_suffix(".0").unwrap_or(&s))
    };
    match n {
        0..=999 => n.to_string(),
        1000..=999_949 => short(n as f64 / 1000.0, "k"),
        _ => short(n as f64 / 1_000_000.0, "M"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(kind: ProviderKind) -> ProviderConfig {
        ProviderConfig { kind, base_url: None, api: None, api_key: None, api_key_env: None, region: None, profile: None, models: Vec::new(), stored_key: None, env_key: None }
    }

    #[test]
    fn a_model_is_offered_the_levels_it_takes() {
        use Effort::*;
        assert_eq!(levels(ProviderKind::Anthropic, "claude-opus-5-5"), [Low, Medium, High, Xhigh, Max]);
        assert_eq!(levels(ProviderKind::Anthropic, "claude-sonnet-4-6"), [Low, Medium, High, Max]);
        assert_eq!(levels(ProviderKind::Anthropic, "claude-haiku-4-5"), []);
        assert_eq!(levels(ProviderKind::Bedrock, "us.anthropic.claude-fable-5-2-v1:0"), [Low, Medium, High, Xhigh, Max]);
        assert_eq!(levels(ProviderKind::Bedrock, "amazon.nova-pro-v1:0"), []);
        assert_eq!(levels(ProviderKind::Openai, "anything:7b"), [Low, Medium, High]);
        assert_eq!(Effort::named(" High "), Some(High));
        assert_eq!(Effort::named("harder"), None);
        assert_eq!((Default.sent(), Xhigh.sent()), (None, Some("xhigh")));
    }

    #[test]
    fn the_level_is_what_was_chosen_then_what_the_config_file_says() {
        let mut cfg: Config = toml::from_str(
            "[providers.work]\nkind = \"anthropic\"\n\n[providers.local]\nkind = \"openai\"\n\n[models.\"claude-opus-5-5\"]\neffort = \"high\"\n\n[models.\"local/small\"]\neffort = \"max\"\n",
        )
        .unwrap();
        assert_eq!(effort(&cfg, "work", "claude-opus-5-5"), Effort::High);
        assert_eq!(effort(&cfg, "work", "claude-sonnet-5-5"), Effort::Default);
        // A level the model is not offered is not sent.
        assert_eq!(effort(&cfg, "local", "small"), Effort::Default);
        cfg.state.effort.insert("work/claude-opus-5-5".into(), Effort::Xhigh);
        assert_eq!(effort(&cfg, "work", "claude-opus-5-5"), Effort::Xhigh);
        cfg.state.effort.insert("work/claude-opus-5-5".into(), Effort::Default);
        assert_eq!(effort(&cfg, "work", "claude-opus-5-5"), Effort::Default);
        // It is kept as it is read back.
        let kept = toml::to_string(&cfg.state).unwrap();
        let back: crate::config::State = toml::from_str(&kept).unwrap();
        assert_eq!(back.effort.get("work/claude-opus-5-5"), Some(&Effort::Default));
    }

    #[test]
    fn the_config_file_sets_a_models_context_window() {
        let cfg: Config =
            toml::from_str("[models.\"local:7b\"]\ncontext_window = 32000\n\n[models.\"work/big\"]\ncontext_window = 1000000\n").unwrap();
        assert_eq!(configured_window(&cfg, "ollama", "local:7b"), Some(32_000));
        assert_eq!(configured_window(&cfg, "work", "big"), Some(1_000_000));
        // A provider-qualified entry applies to that provider only.
        assert_eq!(configured_window(&cfg, "other", "big"), None);
        // Without an entry, a size that is safe for the kind of provider.
        assert_eq!(default_window(&provider(ProviderKind::Openai)), 128_000);
        assert_eq!(default_window(&provider(ProviderKind::Anthropic)), 200_000);
        assert_eq!(default_window(&provider(ProviderKind::Bedrock)), 200_000);
    }

    #[test]
    fn token_counts_read_compactly() {
        assert_eq!(tokens(950), "950");
        assert_eq!(tokens(1234), "1.2k");
        assert_eq!(tokens(48_000), "48k");
        assert_eq!(tokens(1_000_000), "1M");
        assert_eq!(tokens(1_250_000), "1.2M");
    }
}
