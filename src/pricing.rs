use crate::record::Usage;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
const LITELLM_URL: &str =
    "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";

#[derive(Deserialize, Serialize, Clone)]
pub struct Rates {
    pub input_per_m: f64,
    pub output_per_m: f64,
    #[serde(default)]
    pub cache_read_per_m: f64,
    #[serde(default)]
    pub cache_creation_per_m: f64,
    /// True when cache_read tokens are already included in input_tokens
    /// (OpenAI/DeepSeek/Gemini). False for Anthropic, where input_tokens
    /// is non-cached only and cache is reported additively.
    #[serde(default)]
    pub cache_in_input: bool,
}

pub struct PriceTable {
    map: HashMap<String, Rates>,
}

impl PriceTable {
    fn from_json(json: &str) -> Result<Self> {
        let raw: HashMap<String, Rates> = serde_json::from_str(json)?;
        Ok(Self {
            map: raw
                .into_iter()
                .map(|(k, v)| (k.to_ascii_lowercase(), v))
                .collect(),
        })
    }

    pub fn load(local_path: &Path) -> Self {
        match std::fs::read_to_string(local_path)
            .ok()
            .and_then(|s| Self::from_json(&s).ok())
        {
            Some(table) => table,
            None => {
                if local_path.exists() {
                    log::warn!("toll: ignoring malformed {}", local_path.display());
                }
                Self {
                    map: HashMap::new(),
                }
            }
        }
    }

    /// Exact match first; then longest-prefix match (case-insensitive).
    fn lookup(&self, model: &str) -> Option<&Rates> {
        let m = model.to_ascii_lowercase();
        if let Some(r) = self.map.get(&m) {
            return Some(r);
        }
        self.map
            .iter()
            .filter(|(k, _)| m.starts_with(k.as_str()))
            .max_by_key(|(k, _)| k.len())
            .map(|(_, r)| r)
    }

    pub fn compute(&self, model: Option<&str>, usage: &Usage) -> Option<f64> {
        if let Some(c) = usage.cost {
            return Some(c);
        }
        let rates = self.lookup(model?)?;
        let input = usage.input_tokens.unwrap_or(0);
        let cache_read = usage.cache_read_input_tokens.unwrap_or(0);
        let cache_creation = usage.cache_creation_input_tokens.unwrap_or(0);
        let output = usage.output_tokens.unwrap_or(0) as f64;

        let non_cached_input = if rates.cache_in_input {
            input.saturating_sub(cache_read) as f64
        } else {
            input as f64
        };

        Some(
            non_cached_input / 1_000_000.0 * rates.input_per_m
                + cache_read as f64 / 1_000_000.0 * rates.cache_read_per_m
                + cache_creation as f64 / 1_000_000.0 * rates.cache_creation_per_m
                + output / 1_000_000.0 * rates.output_per_m,
        )
    }
}

/// Fetch the litellm prices JSON, transform to our format, and write to
/// `dest`. Prints a summary on success.
pub async fn pull(dest: &Path) -> Result<()> {
    println!("Fetching {LITELLM_URL} ...");
    let body = reqwest::get(LITELLM_URL).await?.text().await?;

    // litellm embeds literal tab characters inside strings in the sample_spec
    // entry; strip control characters so serde_json can parse it.
    let body: String = body
        .chars()
        .map(|c| {
            if c.is_ascii_control() && c != '\n' {
                ' '
            } else {
                c
            }
        })
        .collect();

    let raw: HashMap<String, serde_json::Value> = serde_json::from_str(&body)?;

    let mut out: HashMap<String, Rates> = HashMap::new();
    for (name, val) in &raw {
        let Some(obj) = val.as_object() else { continue };
        let Some(inp) = obj.get("input_cost_per_token").and_then(|v| v.as_f64()) else {
            continue;
        };
        if inp == 0.0 {
            continue;
        }
        let Some(outp) = obj.get("output_cost_per_token").and_then(|v| v.as_f64()) else {
            continue;
        };
        let cache_read = obj
            .get("cache_read_input_token_cost")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let cache_creation = obj
            .get("cache_creation_input_token_cost")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let provider = obj
            .get("litellm_provider")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        // Anthropic: input_tokens is non-cached only; cache is additive.
        let cache_in_input = provider != "anthropic";

        out.insert(
            name.clone(),
            Rates {
                input_per_m: inp * 1_000_000.0,
                output_per_m: outp * 1_000_000.0,
                cache_read_per_m: cache_read * 1_000_000.0,
                cache_creation_per_m: cache_creation * 1_000_000.0,
                cache_in_input,
            },
        );
    }

    let n = out.len();
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(&out)?;
    std::fs::write(dest, json)?;
    println!("Saved {n} models to {}", dest.display());
    Ok(())
}

/// Print the active price table source and model count.
pub fn show(local_path: &Path) {
    if local_path.exists() {
        match std::fs::read_to_string(local_path)
            .ok()
            .and_then(|s| serde_json::from_str::<HashMap<String, Rates>>(&s).ok())
        {
            Some(local) => println!("source: {} ({} models)", local_path.display(), local.len()),
            None => println!("source: {} — unreadable or malformed", local_path.display()),
        }
    } else {
        println!("no price table found — run `toll prices pull` to fetch one");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table() -> PriceTable {
        let json = r#"{
            "claude-opus-4":     {"input_per_m":15.0,  "output_per_m":75.0,  "cache_read_per_m":1.5,    "cache_creation_per_m":18.75, "cache_in_input":false},
            "claude-sonnet-4":   {"input_per_m": 3.0,  "output_per_m":15.0,  "cache_read_per_m":0.3,    "cache_creation_per_m": 3.75, "cache_in_input":false},
            "claude-haiku-4":    {"input_per_m": 0.8,  "output_per_m": 4.0,  "cache_read_per_m":0.08,   "cache_creation_per_m": 1.0,  "cache_in_input":false},
            "gpt-4o-mini":       {"input_per_m": 0.15, "output_per_m": 0.6,  "cache_read_per_m":0.075,  "cache_creation_per_m": 0.0,  "cache_in_input":true},
            "gpt-4o":            {"input_per_m": 2.5,  "output_per_m":10.0,  "cache_read_per_m":1.25,   "cache_creation_per_m": 0.0,  "cache_in_input":true},
            "deepseek-v":        {"input_per_m": 0.27, "output_per_m": 1.10, "cache_read_per_m":0.07,   "cache_creation_per_m": 0.0,  "cache_in_input":true}
        }"#;
        PriceTable::from_json(json).unwrap()
    }

    fn usage(input: u64, output: u64, cache_read: u64, cache_creation: u64) -> Usage {
        Usage {
            input_tokens: Some(input),
            output_tokens: Some(output),
            cache_read_input_tokens: if cache_read > 0 {
                Some(cache_read)
            } else {
                None
            },
            cache_creation_input_tokens: if cache_creation > 0 {
                Some(cache_creation)
            } else {
                None
            },
            ..Default::default()
        }
    }

    #[test]
    fn exact_beats_prefix() {
        // "gpt-4o-mini" must not resolve to "gpt-4o" rates
        let t = table();
        let mini = t.lookup("gpt-4o-mini").unwrap();
        let full = t.lookup("gpt-4o").unwrap();
        assert!(mini.input_per_m < full.input_per_m);
    }

    #[test]
    fn deepseek_cache_in_input_true() {
        let u = usage(100_000, 1_000, 80_000, 0);
        let cost = table().compute(Some("deepseek-v4-pro"), &u).unwrap();
        // non-cached = 20k * 0.27/M + cache_read 80k * 0.07/M + output 1k * 1.10/M
        let expected = 20_000.0 / 1e6 * 0.27 + 80_000.0 / 1e6 * 0.07 + 1_000.0 / 1e6 * 1.10;
        assert!((cost - expected).abs() < 1e-9);
    }

    #[test]
    fn anthropic_cache_additive() {
        // input_tokens = non-cached only; cache is additive
        let u = usage(80_000, 50_000, 20_000, 5_000);
        let cost = table().compute(Some("claude-sonnet-4-5"), &u).unwrap();
        let expected = 80_000.0 / 1e6 * 3.0
            + 20_000.0 / 1e6 * 0.3
            + 5_000.0 / 1e6 * 3.75
            + 50_000.0 / 1e6 * 15.0;
        assert!((cost - expected).abs() < 1e-9);
    }
}
