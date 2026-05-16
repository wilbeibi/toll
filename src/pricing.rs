use crate::record::Usage;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;

const LITELLM_URL: &str =
    "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json";

static TABLE: OnceLock<PriceTable> = OnceLock::new();

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

/// Built-in price table. Keys are lowercase model names or name prefixes;
/// the lookup uses longest-prefix match as a fallback, so a key like
/// "deepseek-v" covers "deepseek-v3", "deepseek-v4-pro", etc.
fn default_rates() -> HashMap<String, Rates> {
    macro_rules! r {
        ($inp:expr, $out:expr, $cr:expr, $cw:expr, $cii:expr) => {
            Rates {
                input_per_m: $inp,
                output_per_m: $out,
                cache_read_per_m: $cr,
                cache_creation_per_m: $cw,
                cache_in_input: $cii,
            }
        };
    }
    [
        // Anthropic — cache is additive (not included in input_tokens)
        ("claude-opus-4",     r!(15.0,  75.0,  1.5,    18.75, false)),
        ("claude-4-opus",     r!(15.0,  75.0,  1.5,    18.75, false)),
        ("claude-3-opus",     r!(15.0,  75.0,  1.5,    18.75, false)),
        ("claude-sonnet-4",   r!( 3.0,  15.0,  0.3,     3.75, false)),
        ("claude-4-sonnet",   r!( 3.0,  15.0,  0.3,     3.75, false)),
        ("claude-3-7-sonnet", r!( 3.0,  15.0,  0.3,     3.75, false)),
        ("claude-3-5-sonnet", r!( 3.0,  15.0,  0.3,     3.75, false)),
        ("claude-3-sonnet",   r!( 3.0,  15.0,  0.3,     3.75, false)),
        ("claude-haiku-4",    r!( 0.8,   4.0,  0.08,    1.0,  false)),
        ("claude-3-5-haiku",  r!( 0.8,   4.0,  0.08,    1.0,  false)),
        ("claude-3-haiku",    r!( 0.25,  1.25, 0.03,    0.3,  false)),
        // OpenAI — cache included in input_tokens
        ("gpt-4o-mini",       r!( 0.15,  0.6,  0.075,   0.0,  true)),
        ("gpt-4o",            r!( 2.5,  10.0,  1.25,    0.0,  true)),
        ("o1-mini",           r!( 1.1,   4.4,  0.55,    0.0,  true)),
        ("o1",                r!(15.0,  60.0,  7.5,     0.0,  true)),
        ("o3-mini",           r!( 1.1,   4.4,  0.55,    0.0,  true)),
        ("o3",                r!(10.0,  40.0,  2.5,     0.0,  true)),
        // DeepSeek — cache included in input_tokens
        ("deepseek-chat",     r!( 0.27,  1.10, 0.07,    0.0,  true)),
        ("deepseek-v",        r!( 0.27,  1.10, 0.07,    0.0,  true)),
        ("deepseek-reasoner", r!( 0.55,  2.19, 0.14,    0.0,  true)),
        ("deepseek-r1",       r!( 0.55,  2.19, 0.14,    0.0,  true)),
        // Gemini — cache included in input_tokens
        ("gemini-2.5-pro",    r!( 1.25, 10.0,  0.315,   0.0,  true)),
        ("gemini-2.0-flash",  r!( 0.10,  0.40, 0.025,   0.0,  true)),
        ("gemini-1.5-pro",    r!( 1.25,  5.0,  0.3125,  0.0,  true)),
        ("gemini-1.5-flash",  r!( 0.075, 0.30, 0.01875, 0.0,  true)),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect()
}

struct PriceTable {
    /// Keys are lowercase model names or prefixes. Local file entries overlay
    /// the built-in defaults so exact versioned names win over prefix fallbacks.
    map: HashMap<String, Rates>,
}

impl PriceTable {
    fn from_json(json: &str) -> Result<HashMap<String, Rates>> {
        let raw: HashMap<String, Rates> = serde_json::from_str(json)?;
        Ok(raw
            .into_iter()
            .map(|(k, v)| (k.to_ascii_lowercase(), v))
            .collect())
    }

    fn load(local_path: &Path) -> Self {
        let mut map = default_rates();

        if let Ok(src) = std::fs::read_to_string(local_path) {
            match Self::from_json(&src) {
                Ok(local) => {
                    // Local entries overlay built-in ones; prefix fallbacks from
                    // defaults still apply for any model absent from the local file.
                    map.extend(local);
                }
                Err(e) => {
                    log::warn!("toll: ignoring malformed {}: {e}", local_path.display());
                }
            }
        }

        Self { map }
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

    fn compute(&self, model: Option<&str>, usage: &Usage) -> Option<f64> {
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

/// Call once at proxy startup before any `compute_cost` calls.
pub fn init(local_path: &Path) {
    TABLE.get_or_init(|| PriceTable::load(local_path));
}

/// Compute cost in USD. Provider-reported cost (e.g. OpenRouter) takes
/// precedence; falls back to the loaded price table by model name.
pub fn compute_cost(model: Option<&str>, usage: &Usage) -> Option<f64> {
    TABLE.get()?.compute(model, usage)
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
        .map(|c| if c.is_ascii_control() && c != '\n' { ' ' } else { c })
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
    let n_default = default_rates().len();

    if local_path.exists() {
        match std::fs::read_to_string(local_path)
            .ok()
            .and_then(|s| serde_json::from_str::<HashMap<String, Rates>>(&s).ok())
        {
            Some(local) => {
                println!(
                    "source: {} ({} models) overlaid on built-in defaults ({n_default} models)",
                    local_path.display(),
                    local.len(),
                );
            }
            None => {
                println!(
                    "source: built-in defaults ({n_default} models) — {} is unreadable",
                    local_path.display()
                );
            }
        }
    } else {
        println!(
            "source: built-in defaults ({n_default} models) — run `toll prices pull` to fetch latest"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table() -> PriceTable {
        PriceTable { map: default_rates() }
    }

    fn usage(input: u64, output: u64, cache_read: u64, cache_creation: u64) -> Usage {
        Usage {
            input_tokens: Some(input),
            output_tokens: Some(output),
            cache_read_input_tokens: if cache_read > 0 { Some(cache_read) } else { None },
            cache_creation_input_tokens: if cache_creation > 0 {
                Some(cache_creation)
            } else {
                None
            },
            ..Default::default()
        }
    }

    #[test]
    fn deepseek_prefix_matches_v4_pro() {
        // deepseek-v4-pro is not an exact key; "deepseek-v" prefix covers it
        let t = table();
        assert!(t.lookup("deepseek-v4-pro").is_some());
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

    #[test]
    fn openrouter_reported_cost_wins() {
        let u = Usage {
            cost: Some(0.001234),
            ..Default::default()
        };
        assert_eq!(table().compute(Some("gpt-4o"), &u), Some(0.001234));
    }

    #[test]
    fn unknown_model_is_none() {
        let u = usage(100, 50, 0, 0);
        assert_eq!(table().compute(Some("unknown-xyz"), &u), None);
    }
}
