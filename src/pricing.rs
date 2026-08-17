use crate::record::Usage;
use anyhow::{Context, Result};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
const MODELS_DEV_URL: &str = "https://models.dev/api.json";
const MINUTES_PER_DAY: u32 = 24 * 60;

/// Half-open UTC minute-of-day windows, written `"HH:MM-HH:MM"` (end `24:00`
/// allowed). A window that wraps midnight (`"22:00-02:00"`) is normalized on
/// load into its two non-wrapping halves, so membership is a flat scan and
/// re-writing the file is idempotent.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
#[serde(try_from = "Vec<String>", into = "Vec<String>")]
pub struct UtcRanges(Vec<(u32, u32)>);

impl UtcRanges {
    fn contains(&self, at: Timestamp) -> bool {
        // Unix time has no leap seconds, so minute-of-day in UTC is pure
        // arithmetic — no timezone lookup needed.
        let minute = (at.as_second().rem_euclid(86_400) / 60) as u32;
        self.0
            .iter()
            .any(|&(from, to)| minute >= from && minute < to)
    }
}

fn parse_hhmm(s: &str) -> Result<u32, String> {
    let (h, m) = s
        .split_once(':')
        .ok_or_else(|| format!("bad time {s:?}, expected HH:MM"))?;
    let h: u32 = h.parse().map_err(|_| format!("bad hour in {s:?}"))?;
    let m: u32 = m.parse().map_err(|_| format!("bad minute in {s:?}"))?;
    if m > 59 || h > 24 || (h == 24 && m > 0) {
        return Err(format!("{s:?} is not a valid UTC time of day"));
    }
    Ok(h * 60 + m)
}

impl TryFrom<Vec<String>> for UtcRanges {
    type Error = String;
    fn try_from(specs: Vec<String>) -> Result<Self, Self::Error> {
        let mut out = Vec::with_capacity(specs.len());
        for spec in &specs {
            let (a, b) = spec
                .split_once('-')
                .ok_or_else(|| format!("bad window {spec:?}, expected HH:MM-HH:MM"))?;
            let (from, to) = (parse_hhmm(a.trim())?, parse_hhmm(b.trim())?);
            match from.cmp(&to) {
                Ordering::Less => out.push((from, to)),
                Ordering::Greater => {
                    // Wraps midnight: keep the two halves instead.
                    out.push((from, MINUTES_PER_DAY));
                    out.push((0, to));
                }
                Ordering::Equal => return Err(format!("empty window {spec:?}")),
            }
        }
        Ok(Self(out))
    }
}

impl From<UtcRanges> for Vec<String> {
    fn from(r: UtcRanges) -> Self {
        fn hhmm(m: u32) -> String {
            format!("{:02}:{:02}", m / 60, m % 60)
        }
        r.0.iter()
            .map(|&(from, to)| format!("{}-{}", hhmm(from), hhmm(to)))
            .collect()
    }
}

/// A recurring time-of-day discount (DeepSeek's off-peak window). The base
/// rates always hold the *undiscounted* price, so a window that is missing or
/// mis-specified over-reports spend rather than under-reporting it — the same
/// bias as `call_cost` refusing to return a confident $0.
#[derive(Deserialize, Serialize, Clone, Debug)]
pub struct OffPeak {
    pub utc: UtcRanges,
    /// Factor applied to every rate inside the window (DeepSeek: `0.5`).
    pub multiplier: f64,
}

impl OffPeak {
    /// The factor in effect at `at`. A multiplier that is not a positive
    /// finite number is ignored: a malformed discount must over-report, never
    /// silently zero out spend.
    fn factor_at(&self, at: Timestamp) -> f64 {
        if self.multiplier > 0.0 && self.multiplier.is_finite() && self.utc.contains(at) {
            self.multiplier
        } else {
            1.0
        }
    }
}

#[derive(Deserialize, Serialize, Clone, Default)]
pub struct Rates {
    pub input_per_m: f64,
    pub output_per_m: f64,
    #[serde(default)]
    pub cache_read_per_m: f64,
    #[serde(default)]
    pub cache_creation_per_m: f64,
    /// Context-length pricing tiers (e.g. Gemini/Grok/Claude bill a higher
    /// rate once the prompt exceeds a threshold). When a request's input
    /// context exceeds a tier's `above_input_tokens`, that tier's rates
    /// replace the base rates for the *whole* request — providers reprice the
    /// entire call, not just the overflow.
    ///
    /// Absent (`None`) means *unstated*, not *none*: on a revision it inherits
    /// the model's base tiers, so a hand-authored revision that lists only the
    /// rates it knows about cannot silently delete tier pricing and
    /// under-report every large-context call after its date. Write `[]` to say
    /// a provider genuinely dropped its tiers. On the base entry there is
    /// nothing to inherit, so absent and empty mean the same thing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tiers: Option<Vec<Tier>>,
    /// Recurring time-of-day discount, if the provider prices by clock.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub off_peak: Option<OffPeak>,
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

/// One dated price change. Rates apply to calls at or after `effective_from`
/// until the next revision.
#[derive(Deserialize, Serialize, Clone)]
pub struct Revision {
    pub effective_from: Timestamp,
    #[serde(flatten)]
    pub rates: Rates,
}

/// Everything known about one model's price *over time*: the rates it launched
/// with plus every dated change since. A table with no revisions behaves
/// exactly as the old flat table did, so pre-existing `prices.json` files load
/// and price identically.
#[derive(Deserialize, Serialize, Clone, Default)]
pub struct ModelPrices {
    #[serde(flatten)]
    pub base: Rates,
    /// True when cache_read tokens are already included in input_tokens
    /// (OpenAI/DeepSeek/Gemini). False for Anthropic, where input_tokens
    /// is non-cached only and cache is reported additively.
    ///
    /// This is an accounting *shape*, not a price: it lives on the model, not
    /// on a revision, so a hand-authored revision cannot accidentally flip a
    /// provider's cache semantics by omitting the field.
    #[serde(default)]
    pub cache_in_input: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub revisions: Vec<Revision>,
    /// Excludes the model from `prices pull`. Set it when the local entry is
    /// known to be better than models.dev's — e.g. a peak/off-peak model where
    /// upstream publishes only one of the two numbers, which would otherwise
    /// append a bogus revision on every pull.
    #[serde(default, skip_serializing_if = "is_false")]
    pub pinned: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// One rate set with tier inheritance already resolved: the rates a revision
/// (or the base) states, plus the tiers actually in force for it.
#[derive(Clone, Copy)]
struct RatesAt<'a> {
    rates: &'a Rates,
    tiers: &'a [Tier],
}

impl ModelPrices {
    /// The tiers stated on the base entry — what a revision inherits when it
    /// says nothing about tiers.
    fn base_tiers(&self) -> &[Tier] {
        self.base.tiers.as_deref().unwrap_or(&[])
    }

    fn resolve<'a>(&'a self, rates: &'a Rates) -> RatesAt<'a> {
        RatesAt {
            rates,
            tiers: rates.tiers.as_deref().unwrap_or_else(|| self.base_tiers()),
        }
    }

    /// The rates billing a call made at `at`: the newest revision that had
    /// taken effect by then, else the base rates. Robust to revision ordering.
    fn rates_at(&self, at: Timestamp) -> RatesAt<'_> {
        let rates = self
            .revisions
            .iter()
            .filter(|r| r.effective_from <= at)
            .max_by_key(|r| r.effective_from)
            .map_or(&self.base, |r| &r.rates);
        self.resolve(rates)
    }

    /// The rates currently in effect — what a fresh pull should be compared
    /// against.
    fn newest(&self) -> RatesAt<'_> {
        let rates = self
            .revisions
            .iter()
            .max_by_key(|r| r.effective_from)
            .map_or(&self.base, |r| &r.rates);
        self.resolve(rates)
    }
}

impl RatesAt<'_> {
    /// Per-million (input, output, cache_read, cache_creation) rates for a
    /// request whose input context is `context_tokens`, made at `at`. Selects
    /// the highest context tier the request exceeds, falling back to the base
    /// rates when no tier applies, then applies any time-of-day discount.
    /// Robust to tier ordering.
    fn effective_rates(&self, context_tokens: u64, at: Timestamp) -> (f64, f64, f64, f64) {
        let (i, o, cr, cc) = self
            .tiers
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
                self.rates.input_per_m,
                self.rates.output_per_m,
                self.rates.cache_read_per_m,
                self.rates.cache_creation_per_m,
            ));
        let f = self
            .rates
            .off_peak
            .as_ref()
            .map_or(1.0, |w| w.factor_at(at));
        (i * f, o * f, cr * f, cc * f)
    }
}

pub struct PriceTable {
    map: HashMap<String, ModelPrices>,
}

/// A key in the form `pull` writes: already lowercase, so it is the entry the
/// canonical-provider pass produced rather than a legacy mixed-case re-listing.
fn is_canonical_key(k: &str) -> bool {
    !k.bytes().any(|b| b.is_ascii_uppercase())
}

/// A listing that actually charges for tokens: nonzero input or output rate.
fn has_real_rates(m: &ModelPrices) -> bool {
    let r = m.newest().rates;
    r.input_per_m > 0.0 || r.output_per_m > 0.0
}

/// Fold case-variant keys onto one lowercase entry, keeping the best listing.
///
/// Lowercased keys can collide across listings ("Qwen/..." vs "qwen/..." for
/// the same model), and map iteration order is random per process, so
/// collisions are resolved by an explicit preference, best first, with the
/// first insertion winning:
///
///   1. A listing with real (nonzero input or output) rates beats a free one —
///      pricing token-bearing calls at a confident $0 would silently
///      under-report spend.
///   2. A key already in canonical form (equal to its own lowercase) beats a
///      mixed-case one. `pull` lowercases every key it writes and inserts
///      canonical providers first, so the canonical-form key *is* the entry a
///      current pull produced; a mixed-case sibling is a legacy re-listing
///      from a reseller, whose rates differ from the provider's own. Without
///      this, `DeepSeek-V4-Pro` (a reseller at 0.4286/0.8571, no cache rate)
///      outranked `deepseek-v4-pro` (DeepSeek's own 0.435/0.87/0.003625) on
///      nothing but `'D' < 'd'`.
///   3. Among equals, the lexicographically smaller key — so two loads can
///      never disagree.
///
/// Returns the collapsed table and how many entries lost a collision. `load`
/// and `pull` both go through here, so the entry `pull` merges into is the
/// same one `compute` will later price with — a `pinned` flag or a hand-authored
/// window on a mixed-case key would otherwise be invisible to the merge and
/// then lose the collision to the fresh entry `pull` inserted beside it.
fn canonicalize(entries: Vec<(String, ModelPrices)>) -> (BTreeMap<String, ModelPrices>, usize) {
    let mut entries = entries;
    entries.sort_by(|(ka, a), (kb, b)| {
        // false sorts first, so negate the "better" predicates.
        (!has_real_rates(a), !is_canonical_key(ka))
            .cmp(&(!has_real_rates(b), !is_canonical_key(kb)))
            .then_with(|| ka.cmp(kb))
    });
    let total = entries.len();
    let mut out = BTreeMap::new();
    for (k, e) in entries {
        out.entry(k.to_ascii_lowercase()).or_insert(e);
    }
    let collapsed = total - out.len();
    (out, collapsed)
}

impl PriceTable {
    fn from_json(json: &str) -> Result<Self> {
        let raw: HashMap<String, ModelPrices> = serde_json::from_str(json)?;
        let (table, _) = canonicalize(raw.into_iter().collect());
        Ok(Self {
            map: table.into_iter().collect(),
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
                    // Loud, and not through `log`: env_logger defaults to
                    // error-only, so a warning here would never be seen, and
                    // the symptom — every computed cost silently missing — is
                    // easy to misread as "these calls were free".
                    eprintln!(
                        "turnpike: {} is malformed; every computed cost will be missing                          until it parses",
                        local_path.display()
                    );
                }
                Self {
                    map: HashMap::new(),
                }
            }
        }
    }

    /// Exact match first; then longest-prefix match (case-insensitive).
    fn lookup(&self, model: &str) -> Option<&ModelPrices> {
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
    /// Time-independent — a price change never changes how tokens are counted.
    pub fn cache_in_input(&self, model: Option<&str>) -> Option<bool> {
        self.lookup(model?).map(|r| r.cache_in_input)
    }

    /// Price one call as it was billed *at the time it was made*. `at` selects
    /// the price revision in force and resolves any time-of-day discount, so
    /// re-running `stats` over old rows after a provider raises prices does not
    /// retroactively reprice history.
    pub fn compute(&self, model: Option<&str>, usage: &Usage, at: Timestamp) -> Option<f64> {
        if let Some(c) = usage.cost {
            return Some(c);
        }
        let prices = self.lookup(model?)?;
        let rates = prices.rates_at(at);
        let input = usage.input_tokens.unwrap_or(0);
        let cache_read = usage.cache_read_input_tokens.unwrap_or(0);
        let cache_creation = usage.cache_creation_input_tokens.unwrap_or(0);
        let output = usage.output_tokens.unwrap_or(0) as f64;

        // Context tiers threshold on total prompt size. For additive-cache
        // providers (Anthropic) input_tokens excludes cache, so add it back to
        // size the context; for cache-included providers input_tokens already
        // counts it.
        let context_tokens = if prices.cache_in_input {
            input
        } else {
            input + cache_read + cache_creation
        };
        let (input_per_m, output_per_m, cache_read_per_m, cache_creation_per_m) =
            rates.effective_rates(context_tokens, at);

        let non_cached_input = if prices.cache_in_input {
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

/// True when two rate sets would bill differently. Tiers are compared *after*
/// inheritance is resolved, so a stored revision that inherits its tiers reads
/// as equal to an upstream entry that states the same ones. `off_peak` is
/// deliberately not compared: models.dev never supplies it, so a fetched rate
/// set always has `None` and comparing it would append a spurious revision on
/// every pull for any model carrying a hand-authored window.
fn rates_differ(a: RatesAt<'_>, b: RatesAt<'_>) -> bool {
    // models.dev round-trips f64 exactly, so any real difference is a price
    // change; the epsilon only absorbs hand-edited values.
    fn ne(x: f64, y: f64) -> bool {
        (x - y).abs() > 1e-12 * x.abs().max(y.abs()).max(1.0)
    }
    let (ra, rb) = (a.rates, b.rates);
    ne(ra.input_per_m, rb.input_per_m)
        || ne(ra.output_per_m, rb.output_per_m)
        || ne(ra.cache_read_per_m, rb.cache_read_per_m)
        || ne(ra.cache_creation_per_m, rb.cache_creation_per_m)
        || a.tiers.len() != b.tiers.len()
        || a.tiers.iter().zip(b.tiers).any(|(x, y)| {
            x.above_input_tokens != y.above_input_tokens
                || ne(x.input_per_m, y.input_per_m)
                || ne(x.output_per_m, y.output_per_m)
                || ne(x.cache_read_per_m, y.cache_read_per_m)
                || ne(x.cache_creation_per_m, y.cache_creation_per_m)
        })
}

#[derive(Default)]
struct MergeSummary {
    added: usize,
    revised: usize,
    unchanged: usize,
    pinned: usize,
    /// Entries whose stored `cache_in_input` disagreed with upstream's
    /// accounting shape and was corrected. Rates are versioned; the shape is
    /// not, so this is the one field a pull still overwrites in place.
    reshaped: usize,
    /// Case-variant keys folded onto their canonical lowercase entry. They
    /// were already unreachable — `load` resolved the same collision — so this
    /// makes the file agree with what pricing actually used.
    collapsed: usize,
    /// False when there was no table before this pull, so the counts below
    /// would be noise.
    had_table: bool,
    /// Models that gained a revision while their current rates carried a
    /// hand-authored `off_peak` window. models.dev cannot supply a window, so
    /// the new revision has none and the discount stops applying from that
    /// date on. Over-reporting, not under — but silent, so `pull` names them.
    window_conflicts: Vec<String>,
}

/// Fold a fresh models.dev fetch into the stored table, **appending** dated
/// revisions rather than overwriting.
///
/// This is what makes historical rows stay correct across a price change: a
/// model whose upstream price moved gains a revision stamped `now`, and calls
/// recorded before that instant keep pricing at the rates they were billed at.
/// Nothing is ever removed — a model that vanishes upstream keeps its history,
/// and hand-authored `off_peak` windows and revisions are never touched.
///
/// `effective_from = now` is an approximation: the provider changed prices
/// some time before you pulled. Correct it by editing the timestamp in
/// `prices.json`; the next pull sees the rates already match and appends
/// nothing.
impl MergeSummary {
    fn print(&self, dest: &Path) {
        if !self.had_table {
            return;
        }
        println!(
            "  {} new, {} repriced (revision effective now), {} unchanged, {} pinned",
            self.added, self.revised, self.unchanged, self.pinned
        );
        if self.revised > 0 {
            println!(
                "  note: revisions are stamped with the pull time; if you know when a \
                 price actually changed, edit `effective_from` in {}",
                dest.display()
            );
        }
        if self.reshaped > 0 {
            println!(
                "  note: {} entries had their cache accounting (`cache_in_input`) \
                 corrected from upstream",
                self.reshaped
            );
        }
        if self.collapsed > 0 {
            println!(
                "  note: {} case-variant keys folded onto their canonical entry \
                 (they were already unreachable); previous file kept at {}",
                self.collapsed,
                dest.with_extension("json.bak").display()
            );
        }
        if !self.window_conflicts.is_empty() {
            // The discount silently stops applying from the new revision on.
            println!(
                "  warning: repriced with a time-of-day discount upstream cannot know about; \
                 re-declare `off_peak` on the new revision or set \"pinned\": true — {}",
                self.window_conflicts.join(", ")
            );
        }
    }
}

fn merge(
    mut table: BTreeMap<String, ModelPrices>,
    fetched: BTreeMap<String, ModelPrices>,
    now: Timestamp,
) -> (BTreeMap<String, ModelPrices>, MergeSummary) {
    let mut s = MergeSummary::default();
    for (key, incoming) in fetched {
        match table.get_mut(&key) {
            None => {
                table.insert(key, incoming);
                s.added += 1;
            }
            Some(stored) if stored.pinned => s.pinned += 1,
            Some(stored) => {
                // Rates are versioned; the cache-accounting *shape* is not — it
                // is a property of the provider's API, so upstream owns it and
                // a pull must still be able to correct it in place. Leaving it
                // frozen would mean a table pulled before a shape fix keeps
                // mispricing forever.
                if stored.cache_in_input != incoming.cache_in_input {
                    stored.cache_in_input = incoming.cache_in_input;
                    s.reshaped += 1;
                }
                if rates_differ(stored.newest(), incoming.newest()) {
                    if stored.newest().rates.off_peak.is_some() {
                        s.window_conflicts.push(key.clone());
                    }
                    // State tiers explicitly on an appended revision: inheritance
                    // exists for hand-authored revisions, and upstream dropping a
                    // tier must not read as "keep the old one".
                    let mut rates = incoming.base;
                    if rates.tiers.is_none() && !stored.base_tiers().is_empty() {
                        rates.tiers = Some(Vec::new());
                    }
                    stored.revisions.push(Revision {
                        effective_from: now,
                        rates,
                    });
                    s.revised += 1;
                } else {
                    s.unchanged += 1;
                }
            }
        }
    }
    (table, s)
}

/// Transform a models.dev `api.json` body into flat, revision-less entries.
fn parse_models_dev(body: &str) -> Result<BTreeMap<String, ModelPrices>> {
    // Top-level: { provider_id: { models: { model_id: { cost: { input, output, ... } } } } }
    let raw: HashMap<String, serde_json::Value> = serde_json::from_str(body)?;
    let mut out: BTreeMap<String, ModelPrices> = BTreeMap::new();

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
                .filter(|ts| !ts.is_empty());
            // Dedupe case-insensitively: the same model re-listed as
            // "Qwen/..." and "qwen/..." must collapse to one entry (the
            // earlier provider in the ordered pass wins).
            out.entry(model_id.to_ascii_lowercase())
                .or_insert(ModelPrices {
                    base: Rates {
                        input_per_m: inp,
                        output_per_m: outp,
                        cache_read_per_m: cache_read,
                        cache_creation_per_m: cache_creation,
                        tiers,
                        off_peak: None,
                    },
                    cache_in_input,
                    revisions: Vec::new(),
                    pinned: false,
                });
        }
    }

    // Bare claude-* keys may be claimed by aggregators that set cache_in_input=true.
    // Anthropic's additive cache accounting applies to all Claude models regardless
    // of which listing won the insertion race.
    for (key, entry) in out.iter_mut() {
        if key.starts_with("claude-") {
            entry.cache_in_input = false;
        }
    }
    Ok(out)
}

/// Read the stored table, resolving case-variant keys the same way `load`
/// does. A file that will not parse is an **error**, never an empty table: the
/// entry point for hand-editing is this file, so a typo'd `effective_from`
/// must not let the caller rebuild it from upstream and drop every revision,
/// window, and `pinned` flag along the way.
fn read_table(path: &Path) -> Result<(BTreeMap<String, ModelPrices>, usize, bool)> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok((BTreeMap::new(), 0, false))
        }
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let raw: HashMap<String, ModelPrices> = serde_json::from_str(&text).with_context(|| {
        format!(
            "{} is malformed — refusing to overwrite it, since it is the only \
             record of your price history. Fix the JSON (or move it aside) and pull again",
            path.display()
        )
    })?;
    let (table, collapsed) = canonicalize(raw.into_iter().collect());
    Ok((table, collapsed, true))
}

/// Write the table durably. This file stopped being a disposable cache of
/// models.dev the moment it started carrying hand-authored revisions and
/// windows — nothing upstream can reconstruct those — so the previous copy is
/// kept and the new one is swapped in by rename, which an interrupted write
/// cannot truncate.
fn write_table(dest: &Path, table: &BTreeMap<String, ModelPrices>) -> Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Sorted keys: the file is byte-identical across pulls of the same data.
    let json = serde_json::to_string_pretty(table)?;
    if dest.exists() {
        std::fs::copy(dest, dest.with_extension("json.bak"))
            .with_context(|| format!("backing up {}", dest.display()))?;
    }
    let tmp = dest.with_extension("json.tmp");
    std::fs::write(&tmp, json).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, dest).with_context(|| format!("replacing {}", dest.display()))?;
    Ok(())
}

/// Fold a fetch into the table at `dest` and write it back. Split from `pull`
/// so the file handling is testable without the network.
fn apply_pull(
    dest: &Path,
    fetched: BTreeMap<String, ModelPrices>,
    now: Timestamp,
) -> Result<(usize, MergeSummary)> {
    let (stored, collapsed, had_table) = read_table(dest)?;
    let (table, mut s) = merge(stored, fetched, now);
    s.collapsed = collapsed;
    s.had_table = had_table;
    write_table(dest, &table)?;
    Ok((table.len(), s))
}

/// Fetch prices from models.dev and fold them into the table at `dest`,
/// appending a dated revision wherever a price moved. Prints a summary.
pub async fn pull(dest: &Path) -> Result<()> {
    println!("Fetching {MODELS_DEV_URL} ...");
    let body = reqwest::get(MODELS_DEV_URL).await?.text().await?;
    let fetched = parse_models_dev(&body)?;
    let (models, s) = apply_pull(dest, fetched, Timestamp::now())?;
    println!("Saved {models} models to {}", dest.display());
    s.print(dest);
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

/// Print the active price table source, model count, and time-varying entries.
pub fn show(local_path: &Path) {
    if !local_path.exists() {
        println!("no price table found — run `turnpike prices pull` to fetch one");
        return;
    }
    let parsed: Option<BTreeMap<String, ModelPrices>> = std::fs::read_to_string(local_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok());
    let Some(table) = parsed else {
        println!("source: {} — unreadable or malformed", local_path.display());
        return;
    };
    println!("source: {} ({} models)", local_path.display(), table.len());

    let mut revised: Vec<(&String, &Revision)> = table
        .iter()
        .filter_map(|(k, m)| {
            m.revisions
                .iter()
                .max_by_key(|r| r.effective_from)
                .map(|r| (k, r))
        })
        .collect();
    revised.sort_by_key(|(_, r)| std::cmp::Reverse(r.effective_from));
    let pinned = table.values().filter(|m| m.pinned).count();
    let by_clock = table
        .values()
        .filter(|m| {
            m.base.off_peak.is_some() || m.revisions.iter().any(|r| r.rates.off_peak.is_some())
        })
        .count();

    if revised.is_empty() && pinned == 0 && by_clock == 0 {
        return;
    }
    println!(
        "  {} with dated revisions, {} priced by time of day, {} pinned",
        revised.len(),
        by_clock,
        pinned
    );
    for (model, rev) in revised.iter().take(5) {
        println!("    {model} — latest revision {}", rev.effective_from);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    /// Any instant: for tables with no revisions and no off-peak window,
    /// pricing is time-independent, so the choice cannot matter.
    fn whenever() -> Timestamp {
        at("2026-01-01T00:00:00Z")
    }

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
        assert!(mini.base.input_per_m < full.base.input_per_m);
    }

    #[test]
    fn deepseek_cache_in_input_true() {
        let u = usage(100_000, 1_000, 80_000, 0);
        let cost = table()
            .compute(Some("deepseek-v4-pro"), &u, whenever())
            .unwrap();
        // Hand-derived: non-cached 20k * 0.27/M + cache_read 80k * 0.07/M
        // + output 1k * 1.10/M = 0.0054 + 0.0056 + 0.0011.
        assert!((cost - 0.0121).abs() < 1e-12);
    }

    #[test]
    fn anthropic_cache_additive() {
        // input_tokens = non-cached only; cache is additive
        let u = usage(80_000, 50_000, 20_000, 5_000);
        let cost = table()
            .compute(Some("claude-sonnet-4-5"), &u, whenever())
            .unwrap();
        // Hand-derived: 80k * 3.0/M + 20k * 0.3/M + 5k * 3.75/M + 50k * 15.0/M
        // = 0.24 + 0.006 + 0.01875 + 0.75.
        assert!((cost - 1.01475).abs() < 1e-12);
    }

    #[test]
    fn flat_table_prices_identically_at_any_instant() {
        // A pre-existing prices.json carries no revisions and no windows, so
        // adding time to the lookup must not change a single number.
        let (t, u) = (table(), usage(100_000, 1_000, 80_000, 0));
        let a = t.compute(Some("gpt-4o"), &u, at("2020-01-01T00:00:00Z"));
        let b = t.compute(Some("gpt-4o"), &u, at("2099-06-30T13:45:00Z"));
        assert_eq!(a, b);
        assert!(a.unwrap() > 0.0);
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
        let cost = tiered_table()
            .compute(Some("grok-4.3"), &u, whenever())
            .unwrap();
        // Hand-derived: 100k * 1.25/M + 1k * 2.5/M = 0.125 + 0.0025.
        assert!((cost - 0.1275).abs() < 1e-12);
    }

    #[test]
    fn context_tier_above_threshold_reprices_whole_request() {
        // 250k input > 200k: the entire request bills at the tier rate, not
        // just the 50k overflow. This is the bug the flat table mispriced.
        let u = usage(250_000, 1_000, 0, 0);
        let cost = tiered_table()
            .compute(Some("grok-4.3"), &u, whenever())
            .unwrap();
        // Hand-derived: 250k * 2.5/M + 1k * 5.0/M = 0.625 + 0.005.
        assert!((cost - 0.63).abs() < 1e-12);
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
            let cost = t.compute(Some("minimax-m2.1"), &u, whenever()).unwrap();
            // Hand-derived: 1k * 0.3/M + 100 * 1.2/M = 0.0003 + 0.00012.
            assert!((cost - 0.00042).abs() < 1e-12);
        }
    }

    #[test]
    fn collision_prefers_the_canonical_lowercase_listing() {
        // Both variants are paid, so rule 1 cannot separate them. The
        // already-lowercase key is the one a current `pull` writes; the
        // mixed-case sibling is a legacy reseller re-listing. Picking by raw
        // lexicographic order instead made `DeepSeek-V4-Pro` (0.4286, no
        // cache rate) beat DeepSeek's own `deepseek-v4-pro` (0.435/0.003625)
        // on nothing but `'D' < 'd'`, mispricing a month of real calls.
        let json = r#"{
            "a-model": {"input_per_m":1.0,"output_per_m":1.0,"cache_in_input":true},
            "A-Model": {"input_per_m":2.0,"output_per_m":2.0,"cache_in_input":true}
        }"#;
        for _ in 0..20 {
            let t = PriceTable::from_json(json).unwrap();
            assert_eq!(t.lookup("a-model").unwrap().base.input_per_m, 1.0);
        }
    }

    #[test]
    fn collision_among_equally_canonical_listings_is_stable() {
        // Neither key is lowercase, so rule 2 cannot separate them either:
        // the lexicographically smaller one wins, and every load agrees.
        let json = r#"{
            "B-Model": {"input_per_m":1.0,"output_per_m":1.0,"cache_in_input":true},
            "B-MODEL": {"input_per_m":2.0,"output_per_m":2.0,"cache_in_input":true}
        }"#;
        for _ in 0..20 {
            let t = PriceTable::from_json(json).unwrap();
            // "B-MODEL" < "B-Model" ('O' < 'o').
            assert_eq!(t.lookup("b-model").unwrap().base.input_per_m, 2.0);
        }
    }

    #[test]
    fn free_listing_never_beats_a_paid_one_even_when_canonical() {
        // Rule 1 outranks rule 2: a confident $0 would silently under-report.
        let json = r#"{
            "c-model": {"input_per_m":0.0,"output_per_m":0.0,"cache_in_input":true},
            "C-Model": {"input_per_m":3.0,"output_per_m":3.0,"cache_in_input":true}
        }"#;
        for _ in 0..20 {
            let t = PriceTable::from_json(json).unwrap();
            assert_eq!(t.lookup("c-model").unwrap().base.input_per_m, 3.0);
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

    // ---- dated revisions and time-of-day windows -------------------------

    /// DeepSeek's real 2026-08-16 change, verbatim: a flat table that gained a
    /// revision with peak rates and a half-price off-peak window. Peak is UTC
    /// 01:00–04:00 and 06:00–10:00, so off-peak is everything else.
    fn deepseek() -> PriceTable {
        let json = r#"{
            "deepseek-v4-pro": {
                "input_per_m":0.435, "output_per_m":0.87, "cache_read_per_m":0.003625,
                "cache_in_input":true,
                "revisions":[{
                    "effective_from":"2026-08-16T16:00:00Z",
                    "input_per_m":1.32, "output_per_m":3.96, "cache_read_per_m":0.044,
                    "off_peak":{"utc":["10:00-01:00","04:00-06:00"],"multiplier":0.5}
                }]
            }
        }"#;
        PriceTable::from_json(json).unwrap()
    }

    #[test]
    fn call_before_the_change_keeps_the_old_price() {
        let u = usage(100_000, 1_000, 80_000, 0);
        let cost = deepseek()
            .compute(Some("deepseek-v4-pro"), &u, at("2026-08-01T02:00:00Z"))
            .unwrap();
        // Hand-derived at the pre-change flat rate: 20k * 0.435/M
        // + 80k * 0.003625/M + 1k * 0.87/M = 0.0087 + 0.00029 + 0.00087.
        assert!((cost - 0.00986).abs() < 1e-12);
    }

    #[test]
    fn call_after_the_change_pays_the_new_peak_price() {
        let u = usage(100_000, 1_000, 80_000, 0);
        // 02:00 UTC is inside DeepSeek's 01:00–04:00 peak block.
        let cost = deepseek()
            .compute(Some("deepseek-v4-pro"), &u, at("2026-08-17T02:00:00Z"))
            .unwrap();
        // Hand-derived at peak: 20k * 1.32/M + 80k * 0.044/M + 1k * 3.96/M
        // = 0.0264 + 0.00352 + 0.00396.
        assert!((cost - 0.03388).abs() < 1e-12);
    }

    #[test]
    fn off_peak_halves_the_same_call() {
        let u = usage(100_000, 1_000, 80_000, 0);
        // 12:00 UTC falls in the 10:00–01:00 off-peak window.
        let cost = deepseek()
            .compute(Some("deepseek-v4-pro"), &u, at("2026-08-17T12:00:00Z"))
            .unwrap();
        // Every rate halved: 0.03388 / 2.
        assert!((cost - 0.01694).abs() < 1e-12);
    }

    #[test]
    fn revision_boundary_is_inclusive_at_the_effective_instant() {
        let (t, u) = (deepseek(), usage(100_000, 1_000, 80_000, 0));
        let m = Some("deepseek-v4-pro");
        // One second before: old rates. At the instant itself: new rates.
        // 16:00 UTC is off-peak, so the new price here is the halved one.
        let before = t.compute(m, &u, at("2026-08-16T15:59:59Z")).unwrap();
        let on = t.compute(m, &u, at("2026-08-16T16:00:00Z")).unwrap();
        assert!((before - 0.00986).abs() < 1e-12);
        assert!((on - 0.01694).abs() < 1e-12);
    }

    #[test]
    fn off_peak_window_wraps_midnight() {
        // "10:00-01:00" must cover 23:00 and 00:30 but not 02:00.
        let (t, u) = (deepseek(), usage(100_000, 1_000, 80_000, 0));
        let m = Some("deepseek-v4-pro");
        let discounted = 0.01694;
        let full = 0.03388;
        for (when, want) in [
            ("2026-08-17T23:00:00Z", discounted),
            ("2026-08-18T00:30:00Z", discounted),
            ("2026-08-18T02:00:00Z", full),
            ("2026-08-18T05:00:00Z", discounted), // the 04:00-06:00 gap
            ("2026-08-18T07:00:00Z", full),
        ] {
            let got = t.compute(m, &u, at(when)).unwrap();
            assert!((got - want).abs() < 1e-12, "{when}: {got} != {want}");
        }
    }

    #[test]
    fn newest_applicable_revision_wins_regardless_of_file_order() {
        // Revisions written out of order must still resolve by date, the same
        // way `effective_rates` is robust to tier ordering.
        let json = r#"{
            "m": {"input_per_m":1.0,"output_per_m":0.0,"cache_in_input":true,
                  "revisions":[
                    {"effective_from":"2026-03-01T00:00:00Z","input_per_m":3.0,"output_per_m":0.0},
                    {"effective_from":"2026-01-01T00:00:00Z","input_per_m":2.0,"output_per_m":0.0}
                  ]}
        }"#;
        let t = PriceTable::from_json(json).unwrap();
        let u = usage(1_000_000, 0, 0, 0);
        for (when, want) in [
            ("2025-12-31T00:00:00Z", 1.0),
            ("2026-02-01T00:00:00Z", 2.0),
            ("2026-04-01T00:00:00Z", 3.0),
        ] {
            assert_eq!(t.compute(Some("m"), &u, at(when)).unwrap(), want, "{when}");
        }
    }

    #[test]
    fn malformed_multiplier_never_zeroes_spend() {
        // A discount that is zero, negative, or non-finite is ignored rather
        // than applied: mispricing must round toward over-reporting.
        for bad in ["0.0", "-1.0"] {
            let json = format!(
                r#"{{"m":{{"input_per_m":1.0,"output_per_m":0.0,"cache_in_input":true,
                     "off_peak":{{"utc":["00:00-24:00"],"multiplier":{bad}}}}}}}"#
            );
            let t = PriceTable::from_json(&json).unwrap();
            let u = usage(1_000_000, 0, 0, 0);
            assert_eq!(t.compute(Some("m"), &u, whenever()).unwrap(), 1.0, "{bad}");
        }
    }

    #[test]
    fn utc_ranges_normalize_and_round_trip() {
        let r = UtcRanges::try_from(vec!["10:00-01:00".to_string(), "04:00-06:00".into()]).unwrap();
        let back: Vec<String> = r.clone().into();
        // The wrapping window is stored as its two halves, and re-reading that
        // form is a fixed point — so rewriting the file is idempotent.
        assert_eq!(back, ["10:00-24:00", "00:00-01:00", "04:00-06:00"]);
        assert_eq!(UtcRanges::try_from(back).unwrap(), r);
    }

    #[test]
    fn utc_ranges_reject_garbage() {
        for bad in ["10:00", "25:00-26:00", "10:70-11:00", "03:00-03:00", "a-b"] {
            assert!(
                UtcRanges::try_from(vec![bad.to_string()]).is_err(),
                "{bad:?} should be rejected"
            );
        }
    }

    // ---- pull merges instead of overwriting -------------------------------

    fn entry(input: f64, output: f64) -> ModelPrices {
        ModelPrices {
            base: Rates {
                input_per_m: input,
                output_per_m: output,
                ..Default::default()
            },
            cache_in_input: true,
            ..Default::default()
        }
    }

    fn one(name: &str, e: ModelPrices) -> BTreeMap<String, ModelPrices> {
        [(name.to_string(), e)].into_iter().collect()
    }

    #[test]
    fn merge_appends_a_revision_when_the_upstream_price_moves() {
        let stored = one("m", entry(0.435, 0.87));
        let fetched = one("m", entry(1.32, 3.96));
        let now = at("2026-08-20T00:00:00Z");
        let (table, s) = merge(stored, fetched, now);
        let m = &table["m"];
        assert_eq!((s.added, s.revised, s.unchanged), (0, 1, 0));
        // The old rate is still the base, so calls recorded before the pull
        // keep pricing at what they actually cost.
        assert_eq!(m.base.input_per_m, 0.435);
        assert_eq!(m.revisions.len(), 1);
        assert_eq!(m.revisions[0].effective_from, now);
        assert_eq!(
            m.rates_at(at("2026-08-19T00:00:00Z")).rates.input_per_m,
            0.435
        );
        assert_eq!(
            m.rates_at(at("2026-08-21T00:00:00Z")).rates.input_per_m,
            1.32
        );
    }

    #[test]
    fn merge_appends_nothing_when_the_price_is_unchanged() {
        // The common case: pulling daily must not grow the file. It also means
        // a hand-corrected revision stays put once upstream catches up.
        let stored = one("m", entry(0.14, 0.28));
        let (table, s) = merge(
            stored,
            one("m", entry(0.14, 0.28)),
            at("2026-08-20T00:00:00Z"),
        );
        assert_eq!((s.revised, s.unchanged), (0, 1));
        assert!(table["m"].revisions.is_empty());
    }

    #[test]
    fn merge_compares_against_the_newest_revision_not_the_base() {
        // A model already carrying a revision must be diffed against that
        // revision; comparing to the base would re-append it on every pull.
        let mut stored = entry(0.435, 0.87);
        stored.revisions.push(Revision {
            effective_from: at("2026-08-16T16:00:00Z"),
            rates: Rates {
                input_per_m: 1.32,
                output_per_m: 3.96,
                ..Default::default()
            },
        });
        let (table, s) = merge(
            one("m", stored),
            one("m", entry(1.32, 3.96)),
            at("2026-08-20T00:00:00Z"),
        );
        assert_eq!((s.revised, s.unchanged), (0, 1));
        assert_eq!(table["m"].revisions.len(), 1);
    }

    #[test]
    fn merge_leaves_pinned_models_untouched() {
        // Pinning is the escape hatch for models where upstream publishes one
        // number (say off-peak) and the local entry holds the other.
        let mut stored = entry(1.32, 3.96);
        stored.pinned = true;
        let (table, s) = merge(
            one("m", stored),
            one("m", entry(0.66, 1.98)),
            at("2026-08-20T00:00:00Z"),
        );
        assert_eq!((s.pinned, s.revised), (1, 0));
        assert_eq!(table["m"].base.input_per_m, 1.32);
        assert!(table["m"].revisions.is_empty());
    }

    #[test]
    fn merge_preserves_hand_authored_windows_and_never_drops_models() {
        // A pull must not clobber an off_peak window it cannot know about,
        // and a model that disappears upstream keeps its history.
        let mut stored = entry(1.32, 3.96);
        stored.base.off_peak = Some(OffPeak {
            utc: UtcRanges::try_from(vec!["10:00-01:00".to_string()]).unwrap(),
            multiplier: 0.5,
        });
        let mut table = one("kept", entry(1.0, 1.0));
        table.insert("m".into(), stored);

        let (table, s) = merge(table, one("m", entry(2.0, 4.0)), at("2026-08-20T00:00:00Z"));
        assert_eq!(s.revised, 1);
        assert!(table.contains_key("kept"));
        assert!(table["m"].base.off_peak.is_some());
        // The window rides on the rates the revision replaces, and models.dev
        // cannot supply one — so the discount stops applying from the new
        // revision on. That is the safe direction (over-report), but silent,
        // so the merge reports it for `pull` to warn about.
        assert!(table["m"].revisions[0].rates.off_peak.is_none());
        assert_eq!(s.window_conflicts, ["m"]);
    }

    #[test]
    fn merge_adds_models_it_has_never_seen() {
        let (table, s) = merge(
            BTreeMap::new(),
            one("m", entry(1.0, 2.0)),
            at("2026-08-20T00:00:00Z"),
        );
        assert_eq!((s.added, s.revised), (1, 0));
        assert!(table["m"].revisions.is_empty());
    }

    // ---- tier inheritance on revisions -----------------------------------

    /// A model with a context tier that later gains a dated price change. The
    /// revision states only the rates it knows about, which is the shape the
    /// README documents and the shape `merge` writes for flat models.
    fn tiered_with_revision(revision_tiers: &str) -> PriceTable {
        let json = format!(
            r#"{{
            "g": {{"input_per_m":1.25,"output_per_m":2.5,"cache_in_input":true,
                  "tiers":[{{"above_input_tokens":200000,"input_per_m":2.5,"output_per_m":5.0}}],
                  "revisions":[{{"effective_from":"2026-08-16T00:00:00Z",
                                "input_per_m":1.50,"output_per_m":3.0{revision_tiers}}}]}}
        }}"#
        );
        PriceTable::from_json(&json).unwrap()
    }

    #[test]
    fn revision_without_tiers_inherits_them_instead_of_dropping_them() {
        // Unstated is not the same as none. Dropping the tier here would have
        // made a *price rise* read as 40% cheaper for every large-context call
        // after the revision date (250k billed at the 1.50 base, not the 2.5
        // tier) — under-reporting, the one direction this table must never go.
        let t = tiered_with_revision("");
        let big = usage(250_000, 0, 0, 0);
        let before = t
            .compute(Some("g"), &big, at("2026-08-01T00:00:00Z"))
            .unwrap();
        let after = t
            .compute(Some("g"), &big, at("2026-08-20T00:00:00Z"))
            .unwrap();
        // Hand-derived: 250k over the 200k threshold bills the whole request at
        // the tier rate, 250k * 2.5/M = 0.625, on both sides of the revision.
        assert!((before - 0.625).abs() < 1e-12);
        assert!((after - 0.625).abs() < 1e-12);

        // The revision still moves the base rate for requests under the tier.
        let small = usage(100_000, 0, 0, 0);
        let under_before = t
            .compute(Some("g"), &small, at("2026-08-01T00:00:00Z"))
            .unwrap();
        let under_after = t
            .compute(Some("g"), &small, at("2026-08-20T00:00:00Z"))
            .unwrap();
        assert!((under_before - 0.125).abs() < 1e-12);
        assert!((under_after - 0.15).abs() < 1e-12);
    }

    #[test]
    fn revision_with_explicit_empty_tiers_drops_them() {
        // The escape hatch: a provider that genuinely retired its tier is
        // written `"tiers": []`, and then the base rate bills the whole call.
        let t = tiered_with_revision(r#","tiers":[]"#);
        let big = usage(250_000, 0, 0, 0);
        let after = t
            .compute(Some("g"), &big, at("2026-08-20T00:00:00Z"))
            .unwrap();
        // Hand-derived: 250k * 1.50/M = 0.375, no tier in force.
        assert!((after - 0.375).abs() < 1e-12);
    }

    #[test]
    fn merge_states_tiers_explicitly_on_an_appended_revision() {
        // `merge` must not lean on inheritance: upstream dropping a tier has to
        // be recorded as a drop, not read back as "keep the old one".
        let mut stored = entry(1.25, 2.5);
        stored.base.tiers = Some(vec![Tier {
            above_input_tokens: 200_000,
            input_per_m: 2.5,
            output_per_m: 5.0,
            cache_read_per_m: 0.0,
            cache_creation_per_m: 0.0,
        }]);
        let (table, s) = merge(
            one("g", stored),
            one("g", entry(1.50, 3.0)),
            at("2026-08-20T00:00:00Z"),
        );
        assert_eq!(s.revised, 1);
        let recorded = table["g"].revisions[0].rates.tiers.as_deref();
        assert!(
            matches!(recorded, Some([])),
            "tiers must be stated, not inherited"
        );
    }

    #[test]
    fn merge_corrects_the_cache_accounting_shape_in_place() {
        // Rates are versioned; the accounting shape is a property of the
        // provider's API, so upstream owns it. A table pulled before a shape
        // fix must not keep mispricing forever.
        let mut stored = entry(3.0, 15.0);
        stored.cache_in_input = true; // wrong: Anthropic reports cache additively
        let mut incoming = entry(3.0, 15.0);
        incoming.cache_in_input = false;
        let (table, s) = merge(
            one("claude-sonnet-4", stored),
            one("claude-sonnet-4", incoming),
            at("2026-08-20T00:00:00Z"),
        );
        // The rates did not move, so no revision — but the shape is corrected.
        assert_eq!((s.revised, s.unchanged, s.reshaped), (0, 1, 1));
        assert!(!table["claude-sonnet-4"].cache_in_input);
    }

    // ---- pull: the table is user data, not a cache ------------------------

    struct TempFile(std::path::PathBuf);

    impl TempFile {
        fn new(name: &str) -> Self {
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let dir = std::env::temp_dir()
                .join(format!("turnpike-prices-test-{}-{n}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir.join(name))
        }
        fn write(&self, contents: &str) {
            std::fs::write(&self.0, contents).unwrap();
        }
        fn read(&self) -> String {
            std::fs::read_to_string(&self.0).unwrap()
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(self.0.parent().unwrap());
        }
    }

    #[test]
    fn pull_refuses_to_overwrite_a_malformed_table() {
        // `prices.json` is hand-edited by design — `pull` prints the
        // instruction to do it. Dropping the `Z` off an `effective_from` makes
        // the whole file unparseable, and treating that as "no table yet" would
        // silently rebuild it from upstream: every revision, window, and
        // `pinned` flag gone, and unreconstructable since upstream publishes
        // only today's price.
        let f = TempFile::new("prices.json");
        let broken = r#"{"m":{"input_per_m":1.0,"output_per_m":1.0,
            "revisions":[{"effective_from":"2026-08-16T16:00","input_per_m":2.0,"output_per_m":2.0}]}}"#;
        f.write(broken);

        let err = apply_pull(
            f.path(),
            one("m", entry(9.0, 9.0)),
            at("2026-08-20T00:00:00Z"),
        )
        .err()
        .expect("a malformed table must be an error, not an empty table");
        assert!(
            err.to_string().contains("malformed"),
            "unhelpful error: {err}"
        );
        assert_eq!(f.read(), broken, "the file must be left exactly as it was");
    }

    #[test]
    fn pull_keeps_the_previous_table_as_a_backup() {
        let f = TempFile::new("prices.json");
        f.write(r#"{"m":{"input_per_m":1.0,"output_per_m":1.0}}"#);
        let (_, s) = apply_pull(
            f.path(),
            one("m", entry(2.0, 2.0)),
            at("2026-08-20T00:00:00Z"),
        )
        .unwrap();
        assert_eq!(s.revised, 1);
        let bak = std::fs::read_to_string(f.path().with_extension("json.bak")).unwrap();
        assert!(bak.contains("1.0") && !bak.contains("2.0"));
        // And no scratch file is left behind by the atomic swap.
        assert!(!f.path().with_extension("json.tmp").exists());
    }

    #[test]
    fn pull_creates_the_table_when_none_exists() {
        let f = TempFile::new("prices.json");
        let (n, s) = apply_pull(
            f.path(),
            one("m", entry(1.0, 2.0)),
            at("2026-08-20T00:00:00Z"),
        )
        .unwrap();
        assert_eq!((n, s.added, s.had_table), (1, 1, false));
        assert!(!f.path().with_extension("json.bak").exists());
    }

    #[test]
    fn pull_honors_a_pinned_entry_stored_under_a_case_variant_key() {
        // models.dev keys are lowercased, but a table can hold mixed-case
        // re-listings (294 of them on the host this was found on). Matching
        // case-sensitively meant the fetch missed the pinned entry and inserted
        // a second, unpinned one beside it — which then *won* the load-time
        // collision on the canonical-key rule, so the pin protected nothing.
        let f = TempFile::new("prices.json");
        f.write(
            r#"{"Qwen/Qwen3-Max":{"input_per_m":1.32,"output_per_m":3.96,"pinned":true,
                "off_peak":{"utc":["10:00-01:00"],"multiplier":0.5}}}"#,
        );
        let (n, s) = apply_pull(
            f.path(),
            one("qwen/qwen3-max", entry(0.66, 1.98)),
            at("2026-08-20T00:00:00Z"),
        )
        .unwrap();
        assert_eq!((n, s.pinned, s.added, s.collapsed), (1, 1, 0, 0));

        let t = PriceTable::load(f.path());
        let m = t.lookup("qwen/qwen3-max").unwrap();
        assert!(m.pinned);
        assert_eq!(m.base.input_per_m, 1.32);
        assert!(m.base.off_peak.is_some());
    }

    #[test]
    fn pull_folds_case_variant_keys_onto_the_entry_pricing_already_used() {
        // Both listings are paid, so the canonical lowercase key wins — the
        // same entry `load` was already picking. Folding makes the file say so.
        let f = TempFile::new("prices.json");
        f.write(
            r#"{"a-model":{"input_per_m":1.0,"output_per_m":1.0},
                "A-Model":{"input_per_m":2.0,"output_per_m":2.0}}"#,
        );
        let (n, s) = apply_pull(f.path(), BTreeMap::new(), at("2026-08-20T00:00:00Z")).unwrap();
        assert_eq!((n, s.collapsed), (1, 1));
        assert_eq!(
            PriceTable::load(f.path())
                .lookup("a-model")
                .unwrap()
                .base
                .input_per_m,
            1.0
        );
    }
}
