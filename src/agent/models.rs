//! What Astrum knows about individual models: how much context they take and
//! what their tokens cost. The config file's `[models."id"]` tables come
//! first; behind them is a small built-in table for Anthropic's API.

use super::Usage;
use crate::config::{Config, ProviderConfig, ProviderKind};
use serde::{Deserialize, Serialize};

/// Dollars per million tokens.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Price {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
    /// Some models charge more for long prompts: (prompt tokens above which
    /// it applies, multiplier on every rate).
    pub long_prompt: Option<(u64, f64)>,
}

impl Price {
    /// What one request cost.
    pub fn cost(&self, u: &Usage) -> f64 {
        let factor = match self.long_prompt {
            Some((above, factor)) if u.input > above => factor,
            _ => 1.0,
        };
        let plain = u.input.saturating_sub(u.cache_read + u.cache_write);
        let dollars = plain as f64 * self.input
            + u.cache_read as f64 * self.cache_read
            + u.cache_write as f64 * self.cache_write
            + u.output as f64 * self.output;
        dollars * factor / 1_000_000.0
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ModelInfo {
    /// Tokens of context the model accepts.
    pub window: u64,
    pub price: Option<Price>,
}

/// Anthropic's list prices and context windows, as published when this
/// version was released: (id prefix, window, input, output, cache read).
/// Cache writes are 1.25 times the input price.
const ANTHROPIC: &[(&str, u64, f64, f64, f64)] = &[
    ("claude-fable-5-2", 1_000_000, 8.0, 40.0, 0.20),
    ("claude-fable-5-1", 1_000_000, 10.0, 50.0, 0.25),
    ("claude-fable-5", 1_000_000, 10.0, 50.0, 1.0),
    ("claude-opus-5-5", 1_000_000, 4.0, 20.0, 0.20),
    ("claude-opus-5", 1_000_000, 5.0, 25.0, 0.50),
    ("claude-opus-4-8", 1_000_000, 5.0, 25.0, 0.50),
    ("claude-opus-4-7", 1_000_000, 5.0, 25.0, 0.50),
    ("claude-opus-4-6", 1_000_000, 5.0, 25.0, 0.50),
    ("claude-sonnet-5-5", 1_000_000, 2.0, 10.0, 0.10),
    ("claude-sonnet-5", 1_000_000, 2.0, 10.0, 0.20),
    ("claude-sonnet-4-6", 1_000_000, 3.0, 15.0, 0.30),
    ("claude-haiku-5-5", 1_000_000, 0.10, 0.50, 0.01),
    ("claude-haiku-4-5", 200_000, 1.0, 5.0, 0.10),
];

fn builtin(kind: ProviderKind, model: &str) -> ModelInfo {
    match kind {
        ProviderKind::Anthropic => {
            // The longest matching prefix, so `claude-opus-5-5` is not taken for `claude-opus-5`.
            let known = ANTHROPIC.iter().filter(|(id, ..)| model.starts_with(id)).max_by_key(|(id, ..)| id.len());
            match known {
                Some(&(id, window, input, output, cache_read)) => ModelInfo {
                    window,
                    price: Some(Price {
                        input,
                        output,
                        cache_read,
                        cache_write: input * 1.25,
                        // Prompts over 100K tokens are billed at five times the rate on this model.
                        long_prompt: (id == "claude-haiku-5-5").then_some((100_000, 5.0)),
                    }),
                },
                None => ModelInfo { window: 200_000, price: None },
            }
        }
        // Other services set their own prices and limits; these are cautious guesses.
        ProviderKind::Bedrock => ModelInfo { window: 200_000, price: None },
        ProviderKind::Openai => ModelInfo { window: 128_000, price: None },
    }
}

/// Per-model settings from the config file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ModelConfig {
    /// Tokens of context the model accepts.
    pub context_window: Option<u64>,
    /// Dollars per million tokens.
    pub input_price: Option<f64>,
    pub output_price: Option<f64>,
    /// Default: a tenth of the input price.
    pub cache_read_price: Option<f64>,
    /// Default: 1.25 times the input price.
    pub cache_write_price: Option<f64>,
}

pub fn info(cfg: &Config, provider_name: &str, provider: &ProviderConfig, model: &str) -> ModelInfo {
    let mut info = builtin(provider.kind, model);
    let user = cfg.models.get(&format!("{provider_name}/{model}")).or_else(|| cfg.models.get(model));
    if let Some(m) = user {
        if let Some(w) = m.context_window.filter(|w| *w > 0) {
            info.window = w;
        }
        if let (Some(input), Some(output)) = (m.input_price, m.output_price) {
            info.price = Some(Price {
                input,
                output,
                cache_read: m.cache_read_price.unwrap_or(input * 0.1),
                cache_write: m.cache_write_price.unwrap_or(input * 1.25),
                long_prompt: None,
            });
        }
    }
    info
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

/// `$1.23`, `$0.031`, `<$0.001`.
pub fn dollars(d: f64) -> String {
    if d >= 1.0 {
        format!("${d:.2}")
    } else if d >= 0.001 {
        format!("${d:.3}")
    } else {
        "<$0.001".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(kind: ProviderKind) -> ProviderConfig {
        ProviderConfig { kind, base_url: None, api_key: None, api_key_env: None, region: None, profile: None, models: Vec::new() }
    }

    #[test]
    fn known_models_are_matched_by_their_longest_prefix() {
        let cfg = Config::default();
        let p = provider(ProviderKind::Anthropic);
        assert_eq!(info(&cfg, "anthropic", &p, "claude-opus-5-5").price.unwrap().input, 4.0);
        assert_eq!(info(&cfg, "anthropic", &p, "claude-opus-5").price.unwrap().input, 5.0);
        assert_eq!(info(&cfg, "anthropic", &p, "claude-haiku-4-5-20251001").window, 200_000);
        let unknown = info(&cfg, "anthropic", &p, "claude-next");
        assert!(unknown.price.is_none() && unknown.window == 200_000);
        // Other services price the same models themselves.
        assert!(info(&cfg, "bedrock", &provider(ProviderKind::Bedrock), "us.anthropic.claude-opus-5-5-v1:0").price.is_none());
        assert_eq!(info(&cfg, "ollama", &provider(ProviderKind::Openai), "anything").window, 128_000);
    }

    #[test]
    fn the_config_file_overrides_and_fills_in() {
        let cfg: Config = toml::from_str(
            "[models.\"local:7b\"]\ncontext_window = 32000\n\n[models.\"work/big\"]\ninput_price = 1.0\noutput_price = 4.0\n\n[models.claude-opus-5-5]\ncontext_window = 200000\n",
        )
        .unwrap();
        let open = provider(ProviderKind::Openai);
        let local = info(&cfg, "ollama", &open, "local:7b");
        assert!(local.window == 32_000 && local.price.is_none());
        let big = info(&cfg, "work", &open, "big").price.unwrap();
        assert_eq!((big.cache_read, big.cache_write), (0.1, 1.25));
        assert!(info(&cfg, "other", &open, "big").price.is_none(), "a provider-qualified entry applies to that provider only");
        // Overriding the window keeps the built-in price.
        let opus = info(&cfg, "anthropic", &provider(ProviderKind::Anthropic), "claude-opus-5-5");
        assert!(opus.window == 200_000 && opus.price.is_some());
    }

    #[test]
    fn cost_counts_each_kind_of_token_at_its_own_rate() {
        let p = Price { input: 2.0, output: 10.0, cache_read: 0.1, cache_write: 2.5, long_prompt: None };
        // 1M prompt tokens: 600k from cache, 100k written to it, 300k plain; 100k out.
        let u = Usage { input: 1_000_000, output: 100_000, cache_read: 600_000, cache_write: 100_000 };
        let expect = 0.3 * 2.0 + 0.6 * 0.1 + 0.1 * 2.5 + 0.1 * 10.0;
        assert!((p.cost(&u) - expect).abs() < 1e-9);
        // Long prompts on a tiered model cost more.
        let tiered = Price { long_prompt: Some((100_000, 5.0)), ..p };
        let short = Usage { input: 50_000, output: 0, cache_read: 0, cache_write: 0 };
        let long = Usage { input: 200_000, output: 0, cache_read: 0, cache_write: 0 };
        assert!((tiered.cost(&short) - 0.1).abs() < 1e-9);
        assert!((tiered.cost(&long) - 2.0).abs() < 1e-9);
    }

    #[test]
    fn numbers_read_compactly() {
        assert_eq!(tokens(950), "950");
        assert_eq!(tokens(1234), "1.2k");
        assert_eq!(tokens(48_000), "48k");
        assert_eq!(tokens(1_000_000), "1M");
        assert_eq!(tokens(1_250_000), "1.2M");
        assert_eq!(dollars(1.234), "$1.23");
        assert_eq!(dollars(0.0314), "$0.031");
        assert_eq!(dollars(0.0002), "<$0.001");
    }
}
