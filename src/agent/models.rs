//! What Astrum knows about individual models: how much context they accept.
//! Providers do not report this in a common way, so it comes from the config
//! file's `[models."id"]` tables, with a cautious guess behind them.

use crate::config::{Config, ProviderConfig, ProviderKind};
use serde::{Deserialize, Serialize};

/// Per-model settings from the config file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ModelConfig {
    /// Tokens of context the model accepts.
    pub context_window: Option<u64>,
}

/// Tokens of context to assume for a model: what the config file says for
/// `provider/model` or `model`, else a size that is safe to assume for the
/// kind of provider. Guessing low only means compacting earlier than needed.
pub fn context_window(cfg: &Config, provider_name: &str, provider: &ProviderConfig, model: &str) -> u64 {
    let user = cfg.models.get(&format!("{provider_name}/{model}")).or_else(|| cfg.models.get(model));
    if let Some(w) = user.and_then(|m| m.context_window).filter(|w| *w > 0) {
        return w;
    }
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
        ProviderConfig { kind, base_url: None, api_key: None, api_key_env: None, region: None, profile: None, models: Vec::new() }
    }

    #[test]
    fn the_config_file_sets_a_models_context_window() {
        let cfg: Config =
            toml::from_str("[models.\"local:7b\"]\ncontext_window = 32000\n\n[models.\"work/big\"]\ncontext_window = 1000000\n").unwrap();
        let open = provider(ProviderKind::Openai);
        assert_eq!(context_window(&cfg, "ollama", &open, "local:7b"), 32_000);
        assert_eq!(context_window(&cfg, "work", &open, "big"), 1_000_000);
        // A provider-qualified entry applies to that provider only.
        assert_eq!(context_window(&cfg, "other", &open, "big"), 128_000);
        // Without an entry, a size that is safe for the kind of provider.
        assert_eq!(context_window(&cfg, "anthropic", &provider(ProviderKind::Anthropic), "claude-opus-5-5"), 200_000);
        assert_eq!(context_window(&cfg, "bedrock", &provider(ProviderKind::Bedrock), "us.anthropic.claude-opus-5-5-v1:0"), 200_000);
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
