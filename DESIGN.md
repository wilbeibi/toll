# DESIGN.md — toll

The architecture and invariants behind toll. `AGENTS.md` holds the
operational contract; this document is the rule set for changing the
code. When a change fights an invariant here, the change is wrong until
this document is deliberately revised.

## Working procedure

Before changing code:

1. Identify which invariant(s) below the change touches — each names its
   code locus and the test that proves it.
2. If the change fights an invariant, stop and surface the conflict. Do
   not code around it.
3. Adding a `Record` field: follow invariant 4's checklist exactly.
4. Verify: `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test`.

## What toll is

A localhost reverse proxy that sits between your LLM clients and the real
provider APIs, forwards every request untouched, and records per-call
usage, cost, latency, and errors to SQLite. It is a *meter*, not a gateway:
it measures, it does not manage. It has no opinion about your traffic and
adds no behavior to it.

## Philosophy

**The proxy is invisible.** A request through toll must be
byte-indistinguishable from a request to the upstream, except for the one
documented mutation (`stream_options.include_usage` injection for
OpenAI-compatible streaming, so the final chunk carries token counts).
Measurement is out-of-band and must never change the proxied response's
bytes, status, latency, or failure mode. If toll's recording path breaks,
the user's request still succeeds. Recording is fire-and-forget; under
backpressure the observer drops data rather than stall the stream.

**Record what happened, faithfully.** toll's value is a truthful log.
Never normalize away meaning for cosmetics: the full `vendor/model` slug is
the billing identity and is stored verbatim. Prefer provider-reported
truth (`usage.cost`) over anything toll infers. Record inference calls;
do not pollute the log with non-inference noise (discovery probes,
model listings).

**Bounded under hostile input.** Every buffer is capped. A 10 GB response,
a chunked request with no `Content-Length`, a never-terminating SSE
stream, a usage object the size of the moon — none of these may OOM toll
or wedge the client. Limits are explicit constants, not hopes.

**The schema is a wire contract.** `Record` is consumed by `stats`,
`tail`, and ad-hoc SQL. It is append-only: new fields are optional,
appended, and forward-migrated against existing databases. Old rows stay
readable forever.

**Deletion is the default.** Maintenance cost dominates implementation
cost. An abstraction that has collapsed to one uniform case is deleted,
not kept "just in case." An allowlist of what we want beats an
open-ended denylist of what we don't. Features are not added without a
demonstrated, concrete need. Optimize for the engineer reading this in
two years, not the one writing it today.

## Architecture

```
client ──▶ 127.0.0.1:<port>  ──┐
                               │  handle_request (proxy.rs)
                               │   • pick Provider from the bound port
                               │   • model from path (Gemini) or body
                               │   • optional stream_options injection
                               ▼
                          upstream provider API
                               │
        ┌──────────────────────┴───────────────────────┐
        │ forward task: stream bytes back to client     │  (hot path —
        │ verbatim, never blocked by observation        │   never blocked)
        └──────────────────────┬───────────────────────┘
                               │ tee (bounded channel, drop-on-full)
                               ▼
                     spawn_observer (proxy.rs)
                       • SSE merge / JSON usage scan
                       • backfill model from response
                               │ fire-and-forget
                               ▼
                     Store::insert  ──▶  SQLite (WAL)
```

The bound port selects the `Provider`. Providers are **data, not code**:
a static `PROVIDERS` table (`providers.rs`) whose only per-provider
variation is a small set of function pointers for the *genuine*
differences — usage JSON shape (`parse_json`), SSE accounting
(`merge_sse`), model-in-path (`model_from_path`), and streaming quirks.
There is no `if provider == "x"` anywhere, and there must not be.

Token extraction is provider-specific and isolated in `parsers/`. SSE
framing (`sse.rs`) and the bounded streaming JSON usage scanner
(`json_usage.rs`) are deliberately narrower than general parsers: they
watch for the one object they need and ignore the rest, so a giant
response body is never fully buffered.

## Invariants

These are load-bearing. Changing one is a design decision, not a patch.
Each states the rule, its code locus, and the test that proves it.

1. **No request mutation.** The only edit is `stream_options.include_usage`
   for providers flagged `inject_stream_options` (`providers.rs`), applied
   by `maybe_inject_stream_options` (`proxy.rs`) — forced on, not deferred
   to a client value; the body is otherwise forwarded byte-for-byte. A
   second mutation site is the violation. Proof:
   `inject_overwrites_existing_include_usage`,
   `inject_falls_back_on_invalid_json` (`proxy.rs`).
2. **Observation never affects the proxied response.** The forward path
   never `.await`s the observer; the tee channels are bounded
   (`BODY_CHANNEL_CAP`, `OBSERVER_CHANNEL_CAP` = 4, `proxy.rs`) and drop
   on full (`try_observe`); `spawn_record_write` is detached. A forward
   path that awaits observation is the violation.
3. **Every buffer is bounded.** Request-body inspection gated by
   `Content-Length` ≤ `MAX_MODEL_INSPECT_BYTES` (`proxy.rs`); the JSON
   usage scan caps at `MAX_USAGE_OBJECT_BYTES` and self-disables past it
   (`JsonUsageExtractor`, `json_usage.rs`); SSE framing caps at
   `MAX_SSE_EVENT_BYTES` and drops on overflow (`SseSplitter`, `sse.rs`).
   Unbounded input buffering is the violation. Proof:
   `overflow_is_reported_and_buffer_is_cleared` (`sse.rs`),
   `oversized_model_does_not_block_usage_extraction` (`json_usage.rs`).
4. **`Record` is append-only.** A new field is optional, appended last,
   with a forward-compatible `ALTER TABLE ... ADD COLUMN` in `Store::init`
   (`record.rs`) that ignores "duplicate column name" on pre-existing
   DBs. Renames/removals require a version bump and a changelog entry.
   Proof: `store_roundtrip` (`record.rs`).
5. **Record only inference.** `is_inference_endpoint` (`proxy.rs`) is an
   allowlist of token-bearing path markers, biased toward inclusion
   (anything that can carry `usage` must match or cost is silently lost);
   it is the single gate inside `spawn_record_write`. An ever-growing
   denylist of probes is the violation. Proof:
   `inference_endpoints_are_recorded` (`proxy.rs`).
6. **Faithful identity.** The model is stored exactly as the provider
   bills it, `vendor/` prefix included, and backfilled from the response
   (`JsonUsageExtractor::model`, `json_usage.rs`) only when the request
   omits it. Any per-provider model rewrite is the violation (see
   "Worked example"). Proof: `extracts_top_level_model_across_chunks`,
   `ignores_nested_or_content_model_strings` (`json_usage.rs`).
7. **Provider-correct token semantics.** Two shapes exist across
   providers: *cache-included* (OpenAI, DeepSeek, Gemini non-thinking —
   cached tokens are a subset of `input_tokens`, stored in
   `cache_read_input_tokens` as a breakdown) and *cache-additive*
   (Anthropic — `cache_read_input_tokens` and
   `cache_creation_input_tokens` are separate from `input_tokens`).
   Similarly, reasoning tokens are a *subset* of `output_tokens` for all
   providers except Gemini thinking models, where `thoughtsTokenCount` is
   additive to `candidatesTokenCount`; `parse_gemini` (`parsers/gemini.rs`)
   therefore adds them into `output_tokens` so aggregation is correct.
   Token fields are stored raw and displayed separately in `stats` — no
   cross-provider "total" is computed. Prefer provider-reported
   `usage.cost` (`parsers/openai_like.rs`) over any inferred price.
8. **SQLite is a write-optimized embedded log.** `Store` (`record.rs`):
   WAL, `synchronous=NORMAL`, `busy_timeout=5000`, one long-lived writer
   behind a `Mutex`. Compaction (`wal_checkpoint(TRUNCATE)` + `optimize`)
   runs only on clean shutdown via `Store::checkpoint`. A checkpoint
   daemon is the violation.
9. **Security.** The listener binds `SocketAddr::from(([127,0,0,1], port))`
   only (`proxy.rs`, `run_all`/`serve_on`). Never log API keys, bearer
   tokens, or credential-bearing bodies; error text is passed through
   `sanitize_error_message`/`redacted_url` (`proxy.rs`), which strip
   `user:pass`, query, and fragment before persistence. Never delete user
   data unasked. Proof: `error_url_is_redacted_before_persistence`
   (`proxy.rs`).

## Non-goals

toll is intentionally *not*: a rate limiter, a router/load balancer, a
caching layer, a key vault, a request transformer, a multi-tenant service,
or a network-exposed daemon. It captures usage and gets out of the way.
Capturing upstream error *bodies* is a deliberate non-goal until there is
a concrete need (size and credential-leakage risk outweigh speculative
value; failures are already diagnosable from status + timing + the
recorded model).

## Worked example: deleting `route_model`

The `Provider` struct once carried `route_model`, a per-provider hook to
rewrite the recorded model. Every provider eventually used `identity`
except OpenRouter, which used `slash_suffix` to strip the `vendor/`
prefix — which silently corrupted billing identity (invariant 6). The fix
was not to special-case OpenRouter differently; it was to recognize the
hook had collapsed to "identity, plus one wrong case," and **delete the
hook entirely**. The struct shrank, the proxy lost two indirections, and
the behavior became correct by construction. That is the default move
when an abstraction no longer earns its keep.

## Decision ledger — deliberately absent

Evaluated (LiteLLM, Langfuse, Helicone, OpenLLMetry, OpenLIT, Portkey,
RelayPlane) and kept out. This is negative space the code cannot tell
you. The answer is "no" until a concrete need overturns it.

- **Local price table — deferred.** Only provider-reported `usage.cost`
  is stored (invariant 7). A local price table would duplicate a derived
  value across nearly every row and inherit upstream-pricing staleness.
  If spend visibility is ever needed, the path is a *separate* price
  table joined at read time — never a wider `Record` (invariant 4).
- **Trace/session IDs, attribution headers — rejected.** Speculative
  request surface, no demonstrated need; grows request inspection
  against invariant 1.
- **OTLP/OpenTelemetry export — rejected.** SQLite is the source of
  truth; an export contract for a nonexistent consumer is bloat.
- **Real-time SSE log stream — rejected.** `tail` already gives local
  visibility; a network-facing stream contradicts invariant 9 and the
  localhost-only non-goal.
- **Writable-config resilience — rejected.** `toll` config is not
  writable; machinery for a problem we do not have.

Routing, virtual keys, spend writers, budgets, retries, caching,
guardrails, ClickHouse/Kafka ingestion, and SDK monkey-patching solve a
gateway/control-plane problem. `toll` is a local meter (see "What toll
is" and "Non-goals").
