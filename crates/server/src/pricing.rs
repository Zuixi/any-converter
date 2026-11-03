//! Model pricing: USD per million tokens, resolved by model prefix.
//!
//! A call's cost is split the way providers bill it: input, output, cache
//! reads, and cache writes (5-minute and 1-hour TTLs). The table is a plain
//! `[pricing."model-prefix"]` map in the config, and the longest matching
//! prefix wins — the same rule routing uses.

use std::collections::BTreeMap;

use any_converter_core::ir::Usage;
use serde::Deserialize;

/// USD per 1,000,000 tokens, as magazines quote them.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
pub struct Price {
    pub input: f64,
    pub output: f64,
    #[serde(default)]
    pub cache_read: f64,
    #[serde(default)]
    pub cache_write: f64,
    /// Anthropic's 1-hour cache write; falls back to `cache_write` when 0.
    #[serde(default)]
    pub cache_write_1h: f64,
}

impl Price {
    fn per_million(tokens: u64, usd_per_million: f64) -> f64 {
        tokens as f64 / 1_000_000.0 * usd_per_million
    }

    /// What a call costs at this price, in USD.
    ///
    /// `cache_read_tokens` is billed at `cache_read`; `cache_write_tokens`
    /// splits into the 1-hour part (`cache_write_1h`) and the rest
    /// (`cache_write`). Reasoning tokens are already inside `output_tokens`.
    pub fn cost_split(&self, usage: &Usage) -> f64 {
        let mut cost = Self::per_million(usage.input_tokens, self.input)
            + Self::per_million(usage.output_tokens, self.output);

        if let Some(tokens) = usage.cache_read_tokens {
            cost += Self::per_million(tokens, self.cache_read);
        }

        if let Some(tokens) = usage.cache_write_tokens {
            let one_hour = usage.cache_write_1h.unwrap_or(0).min(tokens);
            let rest = tokens - one_hour;
            let one_hour_rate = if self.cache_write_1h == 0.0 {
                self.cache_write
            } else {
                self.cache_write_1h
            };
            cost += Self::per_million(one_hour, one_hour_rate)
                + Self::per_million(rest, self.cache_write);
        }

        cost
    }
}

/// Model-prefix → price. The longest matching prefix wins, as routing does.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(transparent)]
pub struct PriceTable {
    prices: BTreeMap<String, Price>,
}

impl PriceTable {
    pub fn resolve(&self, model: &str) -> Option<Price> {
        self.prices
            .iter()
            // A trailing `*` is accepted so keys can be written like the
            // routing globs (`"claude-*"`); a plain prefix works the same.
            .filter(|(pattern, _)| model.starts_with(pattern.strip_suffix('*').unwrap_or(pattern)))
            .max_by_key(|(pattern, _)| pattern.len())
            .map(|(_, price)| *price)
    }

    /// A stable fingerprint of the table's contents, stored beside each cost
    /// so a later price change can be told apart from the cost it produced.
    pub fn version(&self) -> String {
        // FNV-1a 64-bit, hand-rolled: one more dependency is not worth a hash
        // of a handful of config lines.
        const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
        const PRIME: u64 = 0x0000_0100_0000_01b3;

        let mut hash = OFFSET_BASIS;
        for (pattern, price) in &self.prices {
            let line = format!(
                "{pattern}={},{},{},{},{}\n",
                price.input,
                price.output,
                price.cache_read,
                price.cache_write,
                price.cache_write_1h
            );
            for byte in line.as_bytes() {
                hash ^= u64::from(*byte);
                hash = hash.wrapping_mul(PRIME);
            }
        }
        format!("{hash:016x}")
    }

    pub fn is_empty(&self) -> bool {
        self.prices.is_empty()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn price(input: f64, output: f64) -> Price {
        Price {
            input,
            output,
            cache_read: 0.0,
            cache_write: 0.0,
            cache_write_1h: 0.0,
        }
    }

    fn table(toml: &str) -> PriceTable {
        toml::from_str(toml).unwrap()
    }

    #[test]
    fn input_tokens_bill_at_the_input_rate() {
        let price = price(3.0, 15.0);
        let usage = Usage {
            input_tokens: 1_000_000,
            ..Default::default()
        };
        assert_eq!(price.cost_split(&usage), 3.0);
    }

    #[test]
    fn cache_write_splits_the_one_hour_part() {
        let price = Price {
            cache_write: 3.75,
            cache_write_1h: 6.0,
            ..price(3.0, 15.0)
        };
        let usage = Usage {
            input_tokens: 0,
            output_tokens: 0,
            cache_write_tokens: Some(150),
            cache_write_1h: Some(100),
            ..Default::default()
        };
        let expected = 100.0 * 6.0 / 1_000_000.0 + 50.0 * 3.75 / 1_000_000.0;
        assert_eq!(expected, 0.000_787_5);
        assert!((price.cost_split(&usage) - expected).abs() < 1e-12);
    }

    #[test]
    fn cache_write_without_one_hour_bills_entirely_at_cache_write() {
        let price = Price {
            cache_write: 3.75,
            cache_write_1h: 6.0,
            ..price(3.0, 15.0)
        };
        let usage = Usage {
            cache_write_tokens: Some(150),
            cache_write_1h: None,
            ..Default::default()
        };
        let expected = 150.0 * 3.75 / 1_000_000.0;
        assert!((price.cost_split(&usage) - expected).abs() < 1e-12);
    }

    #[test]
    fn resolve_matches_a_model_prefix() {
        let table = table(
            r#"
["claude-sonnet-4"]
input = 3.0
output = 15.0
"#,
        );
        let resolved = table.resolve("claude-sonnet-4-20250514").unwrap();
        assert_eq!(resolved.input, 3.0);
        assert_eq!(resolved.output, 15.0);
    }

    #[test]
    fn resolve_prefers_the_longest_prefix() {
        let table = table(
            r#"
["claude"]
input = 1.0
output = 2.0

["claude-sonnet"]
input = 3.0
output = 15.0
"#,
        );
        assert_eq!(
            table.resolve("claude-sonnet-4-20250514").unwrap().input,
            3.0
        );
        assert_eq!(table.resolve("claude-opus-4").unwrap().input, 1.0);
    }

    #[test]
    fn resolve_unlisted_model_is_none() {
        let table = table(
            r#"
["claude-sonnet-4"]
input = 3.0
output = 15.0
"#,
        );
        assert!(table.resolve("gpt-4.1").is_none());
    }

    #[test]
    fn version_is_stable_and_tracks_price_changes() {
        let same_a = table(
            r#"
["claude-sonnet-4"]
input = 3.0
output = 15.0

["gpt-4.1"]
input = 2.0
output = 8.0
"#,
        );
        // Same contents, keys written in the other order.
        let same_b = table(
            r#"
["gpt-4.1"]
input = 2.0
output = 8.0

["claude-sonnet-4"]
input = 3.0
output = 15.0
"#,
        );
        assert_eq!(same_a.version(), same_b.version());

        let changed = table(
            r#"
["claude-sonnet-4"]
input = 3.5
output = 15.0

["gpt-4.1"]
input = 2.0
output = 8.0
"#,
        );
        assert_ne!(same_a.version(), changed.version());
    }

    #[test]
    fn config_toml_parses_into_the_price_table() {
        let config = crate::config::ServerConfig::from_toml(
            r#"
[server]
host = "127.0.0.1"
port = 8080
api_key = "sk-test"

[pricing."claude-sonnet-4"]
input = 3.0
output = 15.0
cache_read = 0.30
cache_write = 3.75
cache_write_1h = 6.0
"#,
        )
        .unwrap();

        assert!(!config.pricing.is_empty());
        let resolved = config.pricing.resolve("claude-sonnet-4-20250514").unwrap();
        assert_eq!(resolved.cache_read, 0.30);
        assert_eq!(resolved.cache_write, 3.75);
        assert_eq!(resolved.cache_write_1h, 6.0);

        let no_pricing = crate::config::ServerConfig::from_toml(
            r#"
[server]
host = "127.0.0.1"
port = 8080
api_key = "sk-test"
"#,
        )
        .unwrap();
        assert!(no_pricing.pricing.is_empty());
    }
}
