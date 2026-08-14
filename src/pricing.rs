use crate::record::Usage;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
const MODELS_DEV_URL: &str = "https://models.dev/api.json";

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
    /// Context-length pricing tiers (e.g. Gemini/Grok/Claude bill a higher
    /// rate once the prompt exceeds a threshold). When a request's input
    /// context exceeds a tier's `above_input_tokens`, that tier's rates
    /// replace the base rates for the *whole* request — providers reprice the
    /// entire call, not just the overflow. Empty for flat-priced models.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tiers: Vec<Tier>,
}

#[derive(Deserialize, Serialize, Clone)]
pub struct Tier {
    /// Applies when the request's input context exceeds this many tokens.
    pub above_input_tokens: u64,
    pub input_per_m: f64,
    pub output_per_m: f64,
    #[serde(default)]
    pub cache_read_per_m: f64,
    #[serde(default)]
    pub cache_creation_per_m: f64,
}

impl Rates {
    /// Per-million (input, output, cache_read, cache_creation) rates for a
    /// request whose input context is `context_tokens`. Selects the highest
    /// context tier the request exceeds, falling back to the base rates when
    /// no tier applies. Robust to tier ordering.
    fn effective_rates(&self, context_tokens: u64) -> (f64, f64, f64, f64) {
        self.tiers
            .iter()
            .filter(|t| context_tokens > t.above_input_tokens)
            .max_by_key(|t| t.above_input_tokens)
            .map(|t| {
                (
                    t.input_per_m,
                    t.output_per_m,
                    t.cache_read_per_m,
                    t.cache_creation_per_m,
                )
            })
            .unwrap_or((
                self.input_per_m,
                self.output_per_m,
                self.cache_read_per_m,
                self.cache_creation_per_m,
            ))
    }
}

pub struct PriceTable {
    map: HashMap<String, Rates>,
}

/// A listing that actually charges for tokens: nonzero input or output rate.
fn has_real_rates(r: &Rates) -> bool {
    r.input_per_m > 0.0 || r.output_per_m > 0.0
}

impl PriceTable {
    fn from_json(json: &str) -> Result<Self> {
        let raw: HashMap<String, Rates> = serde_json::from_str(json)?;
        // Lowercased keys can collide across listings ("Qwen/..." vs
        // "qwen/..." for the same model). HashMap iteration order is random
        // per process, so resolve collisions deterministically: a listing
        // with real (nonzero input or output) rates beats a free listing —
        // pricing token-bearing calls at a confident $0 would silently
        // underreport spend — and among equals the lexicographically
        // smaller original key wins.
        let mut keys: Vec<&String> = raw.keys().collect();
        keys.sort();
        let mut map = HashMap::with_capacity(raw.len());
        for k in keys {
            let rates = &raw[k];
            map.entry(k.to_ascii_lowercase())
                .and_modify(|cur: &mut Rates| {
                    if has_real_rates(rates) && !has_real_rates(cur) {
                        *cur = rates.clone();
                    }
                })
                .or_insert_with(|| rates.clone());
        }
        Ok(Self { map })
    }

    pub fn load(local_path: &Path) -> Self {
        match std::fs::read_to_string(local_path)
            .ok()
            .and_then(|s| Self::from_json(&s).ok())
        {
            Some(table) => table,
            None => {
                if local_path.exists() {
                    log::warn!("turnpike: ignoring malformed {}", local_path.display());
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

    /// Cache accounting shape for a model: true = cached tokens are a subset
    /// of input_tokens (OpenAI/DeepSeek style), false = additive (Anthropic).
    pub fn cache_in_input(&self, model: Option<&str>) -> Option<bool> {
        self.lookup(model?).map(|r| r.cache_in_input)
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

        // Context tiers threshold on total prompt size. For additive-cache
        // providers (Anthropic) input_tokens excludes cache, so add it back to
        // size the context; for cache-included providers input_tokens already
        // counts it.
        let context_tokens = if rates.cache_in_input {
            input
        } else {
            input + cache_read + cache_creation
        };
        let (input_per_m, output_per_m, cache_read_per_m, cache_creation_per_m) =
            rates.effective_rates(context_tokens);

        let non_cached_input = if rates.cache_in_input {
            input.saturating_sub(cache_read) as f64
        } else {
            input as f64
        };

        Some(
            non_cached_input / 1_000_000.0 * input_per_m
                + cache_read as f64 / 1_000_000.0 * cache_read_per_m
                + cache_creation as f64 / 1_000_000.0 * cache_creation_per_m
                + output / 1_000_000.0 * output_per_m,
        )
    }
}

/// Fetch prices from models.dev, transform to our format, and write to
/// `dest`. Prints a summary on success.
pub async fn pull(dest: &Path) -> Result<()> {
    println!("Fetching {MODELS_DEV_URL} ...");
    let body = reqwest::get(MODELS_DEV_URL).await?.text().await?;

    // Top-level: { provider_id: { models: { model_id: { cost: { input, output, ... } } } } }
    let raw: HashMap<String, serde_json::Value> = serde_json::from_str(&body)?;

    let mut out: HashMap<String, Rates> = HashMap::new();

    // Deterministic provider order: canonical providers first (their bare
    // model IDs win over reseller/aggregator re-listings that may carry
    // different prices), then the rest alphabetically — two pulls of the
    // same upstream yield the same table.
    for provider_id in ordered_provider_ids(&raw, &["anthropic", "openai", "google", "deepseek"]) {
        let provider_val = &raw[provider_id];
        let Some(models) = provider_val.get("models").and_then(|v| v.as_object()) else {
            continue;
        };
        // Anthropic native API: input_tokens is non-cached only; cache is additive.
        // All other providers (including OpenRouter): input_tokens includes cached tokens.
        let cache_in_input = provider_id != "anthropic";
        for (model_id, model_val) in models {
            let Some(cost) = model_val.get("cost").and_then(|v| v.as_object()) else {
                continue;
            };
            let Some(inp) = cost.get("input").and_then(|v| v.as_f64()) else {
                continue;
            };
            let Some(outp) = cost.get("output").and_then(|v| v.as_f64()) else {
                continue;
            };
            let cache_read = cost
                .get("cache_read")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let cache_creation = cost
                .get("cache_write")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            // Context-length tiers: models.dev lists them under `cost.tiers` as
            // `{input, output, cache_read, tier:{type:"context", size}}`. A tier
            // field it omits inherits the base rate. (`context_over_200k` is a
            // redundant convenience copy of the same data — ignored.)
            let tiers = cost
                .get("tiers")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    let mut ts: Vec<Tier> = arr
                        .iter()
                        .filter_map(|t| {
                            let info = t.get("tier")?;
                            if info.get("type").and_then(|v| v.as_str()) != Some("context") {
                                return None;
                            }
                            let above = info.get("size").and_then(|v| v.as_u64())?;
                            Some(Tier {
                                above_input_tokens: above,
                                input_per_m: t.get("input").and_then(|v| v.as_f64()).unwrap_or(inp),
                                output_per_m: t
                                    .get("output")
                                    .and_then(|v| v.as_f64())
                                    .unwrap_or(outp),
                                cache_read_per_m: t
                                    .get("cache_read")
                                    .and_then(|v| v.as_f64())
                                    .unwrap_or(cache_read),
                                cache_creation_per_m: t
                                    .get("cache_write")
                                    .and_then(|v| v.as_f64())
                                    .unwrap_or(cache_creation),
                            })
                        })
                        .collect();
                    ts.sort_by_key(|t| t.above_input_tokens);
                    ts
                })
                .unwrap_or_default();
            // Dedupe case-insensitively: the same model re-listed as
            // "Qwen/..." and "qwen/..." must collapse to one entry (the
            // earlier provider in the ordered pass wins).
            out.entry(model_id.to_ascii_lowercase()).or_insert(Rates {
                input_per_m: inp,
                output_per_m: outp,
                cache_read_per_m: cache_read,
                cache_creation_per_m: cache_creation,
                cache_in_input,
                tiers,
            });
        }
    }

    // Bare claude-* keys may be claimed by aggregators that set cache_in_input=true.
    // Anthropic's additive cache accounting applies to all Claude models regardless
    // of which listing won the insertion race.
    for (key, rates) in out.iter_mut() {
        if key.starts_with("claude-") {
            rates.cache_in_input = false;
        }
    }

    let n = out.len();
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Sorted keys: the file is byte-identical across pulls of the same data.
    let sorted: std::collections::BTreeMap<String, Rates> = out.into_iter().collect();
    let json = serde_json::to_string_pretty(&sorted)?;
    std::fs::write(dest, json)?;
    println!("Saved {n} models to {}", dest.display());
    Ok(())
}

/// Provider IDs in deterministic order: `priority` first (as listed), then
/// every other ID alphabetically.
fn ordered_provider_ids<'a>(
    raw: &'a HashMap<String, serde_json::Value>,
    priority: &[&str],
) -> Vec<&'a String> {
    let mut ids: Vec<&String> = raw.keys().collect();
    ids.sort_by(|a, b| {
        let pa = priority.iter().position(|p| *p == a.as_str());
        let pb = priority.iter().position(|p| *p == b.as_str());
        match (pa, pb) {
            (Some(x), Some(y)) => x.cmp(&y),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.cmp(b),
        }
    });
    ids
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
        println!("no price table found — run `turnpike prices pull` to fetch one");
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
        // Hand-derived: non-cached 20k * 0.27/M + cache_read 80k * 0.07/M
        // + output 1k * 1.10/M = 0.0054 + 0.0056 + 0.0011.
        assert!((cost - 0.0121).abs() < 1e-12);
    }

    #[test]
    fn anthropic_cache_additive() {
        // input_tokens = non-cached only; cache is additive
        let u = usage(80_000, 50_000, 20_000, 5_000);
        let cost = table().compute(Some("claude-sonnet-4-5"), &u).unwrap();
        // Hand-derived: 80k * 3.0/M + 20k * 0.3/M + 5k * 3.75/M + 50k * 15.0/M
        // = 0.24 + 0.006 + 0.01875 + 0.75.
        assert!((cost - 1.01475).abs() < 1e-12);
    }

    fn tiered_table() -> PriceTable {
        // grok-style context tier: base below 200k, double above (verbatim
        // models.dev shape for xai grok, transformed by `pull`).
        let json = r#"{
            "grok-4.3": {"input_per_m":1.25,"output_per_m":2.5,"cache_read_per_m":0.2,
                "cache_creation_per_m":0.0,"cache_in_input":true,
                "tiers":[{"above_input_tokens":200000,"input_per_m":2.5,"output_per_m":5.0,
                          "cache_read_per_m":0.4,"cache_creation_per_m":0.0}]}
        }"#;
        PriceTable::from_json(json).unwrap()
    }

    #[test]
    fn context_tier_below_threshold_uses_base_rates() {
        let u = usage(100_000, 1_000, 0, 0);
        let cost = tiered_table().compute(Some("grok-4.3"), &u).unwrap();
        // Hand-derived: 100k * 1.25/M + 1k * 2.5/M = 0.125 + 0.0025.
        assert!((cost - 0.1275).abs() < 1e-12);
    }

    #[test]
    fn load_resolves_case_collisions_deterministically() {
        // Two listings of one model differing only in case; the paid listing
        // must win over the free one on every load, not whichever HashMap
        // iteration order produces.
        let json = r#"{
            "MiniMax-M2.1":   {"input_per_m":0.0,"output_per_m":0.0,"cache_in_input":true},
            "minimax-m2.1":   {"input_per_m":0.3,"output_per_m":1.2,"cache_in_input":true}
        }"#;
        // Many loads on purpose: the bug was per-process HashMap
        // iteration order, so one load catches a regression only half the
        // time — twenty make it effectively certain.
        for _ in 0..20 {
            let t = PriceTable::from_json(json).unwrap();
            let u = usage(1_000, 100, 0, 0);
            let cost = t.compute(Some("minimax-m2.1"), &u).unwrap();
            // Hand-derived: 1k * 0.3/M + 100 * 1.2/M = 0.0003 + 0.00012.
            assert!((cost - 0.00042).abs() < 1e-12);
        }
    }

    #[test]
    fn collision_among_equal_listings_picks_smaller_key() {
        // Both variants are paid: the lexicographically smaller original key
        // wins, so two loads can never disagree.
        let json = r#"{
            "a-model": {"input_per_m":1.0,"output_per_m":1.0,"cache_in_input":true},
            "A-Model": {"input_per_m":2.0,"output_per_m":2.0,"cache_in_input":true}
        }"#;
        for _ in 0..20 {
            let t = PriceTable::from_json(json).unwrap();
            // "A-Model" < "a-model", so the uppercase listing's rates win.
            assert_eq!(t.lookup("a-model").unwrap().input_per_m, 2.0);
        }
    }

    #[test]
    fn ordered_provider_ids_put_priority_first_then_alphabetical() {
        let raw: HashMap<String, serde_json::Value> = ["z-prov", "a-prov", "openai", "deepseek"]
            .iter()
            .map(|id| (id.to_string(), serde_json::Value::Null))
            .collect();
        let ids = ordered_provider_ids(&raw, &["anthropic", "openai", "google", "deepseek"]);
        let names: Vec<&str> = ids.iter().map(|s| s.as_str()).collect();
        assert_eq!(names, ["openai", "deepseek", "a-prov", "z-prov"]);
    }

    #[test]
    fn context_tier_above_threshold_reprices_whole_request() {
        // 250k input > 200k: the entire request bills at the tier rate, not
        // just the 50k overflow. This is the bug the flat table mispriced.
        let u = usage(250_000, 1_000, 0, 0);
        let cost = tiered_table().compute(Some("grok-4.3"), &u).unwrap();
        // Hand-derived: 250k * 2.5/M + 1k * 5.0/M = 0.625 + 0.005.
        assert!((cost - 0.63).abs() < 1e-12);
    }
}
