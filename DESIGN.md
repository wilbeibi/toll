# DESIGN.md — toll

The philosophy, architecture, and invariants behind toll. Read this before
any non-trivial change. `AGENTS.md` holds the operational contract; this
document explains *why* the code is shaped the way it is. When a change
fights an invariant here, the change is wrong until this document is
deliberately revised.

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

1. **No request mutation** except `stream_options.include_usage` for
   providers flagged `inject_stream_options`. The request body is
   otherwise forwarded byte-for-byte.
2. **Observation never affects the proxied response.** The forward path
   does not await the observer. The observer channel is bounded and drops
   on full (`try_observe`). `spawn_record_write` is detached.
3. **Every buffer is bounded.** Request-body inspection is gated by
   `Content-Length` ≤ `MAX_MODEL_INSPECT_BYTES`; the usage scanner caps
   the matched object and disables itself past its limit; SSE and channel
   buffers are bounded. New code that buffers unbounded input is a bug.
4. **`Record` is append-only.** New fields: optional, appended, with a
   forward-compatible `ALTER` that tolerates pre-existing databases, plus
   a roundtrip test. Renames/removals require a version bump and a
   changelog entry.
5. **Record only inference.** `is_inference_endpoint` is an allowlist of
   token-bearing endpoints, biased toward inclusion (anything that can
   carry `usage` must match, or cost data is silently lost). It is never
   an ever-growing denylist of probes.
6. **Faithful identity.** Store the model exactly as the provider bills
   it, including the `vendor/` prefix for routers. No transformation that
   destroys billing identity.
7. **Provider-correct cost.** Cached tokens are a *subset* of input for
   OpenAI/DeepSeek/Gemini and *additive* for Anthropic; never charge a
   token twice. Prefer provider-reported `cost` over inferred prices.
8. **SQLite is a write-optimized embedded log.** WAL,
   `synchronous=NORMAL`, `busy_timeout`, one long-lived writer behind a
   `Mutex`. Compaction (`wal_checkpoint(TRUNCATE)` + `optimize`) happens
   only on clean shutdown — no checkpoint daemon for a non-problem.
9. **Security.** Bind `127.0.0.1` only. Never log API keys, bearer
   tokens, or credential-bearing request bodies. Never delete user data
   unasked.

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
