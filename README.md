# toll

**See exactly where your LLM API spend goes — without routing your keys through anyone's cloud.**

`toll` is a localhost reverse proxy that logs every LLM API call your tools make —
model, tokens, cost, latency, cache hits, status, errors, and which tool made the
call — into a local SQLite database. Point any OpenAI-, Anthropic-, or
Gemini-compatible client (10 providers built in) at `127.0.0.1`, keep using your
existing API keys, and query usage from your terminal.

It is a **meter, not a gateway**: it forwards every request byte-for-byte and gets
out of the way. No routing, no rate limiting, no key vault, no account, no hosted
dashboard. If toll's recording breaks, your request still succeeds.

Use it to answer:

- Which tool spent the most money today? Which model ate the most tokens?
- Did a failed request still reach the provider (and bill me)?
- Are my cache hits actually landing?
- What did my agent call in the last 10 minutes?
- Which local tools are quietly sending traffic to which APIs?

```text
your client ──▶ http://127.0.0.1:<provider-port> ──▶ provider API   (forwarded verbatim)
                            │
                            └──▶ local SQLite usage log              (out-of-band)
```

## Quick start

```zsh
git clone https://github.com/wilbeibi/toll
cd toll
cargo install --path .
toll start                 # runs the listeners in the foreground
toll prices pull           # optional: fetch a price table so costs are filled in
```

In another shell, point a client at toll and start using it as normal:

```zsh
eval $(toll config --provider openrouter)   # export OPENAI_BASE_URL=http://127.0.0.1:4004/api/v1
# fish:  toll config --provider xai --format fish | source
```

`toll config` with no provider prints an annotated list for every provider (OpenAI-shaped
ones share `OPENAI_BASE_URL`, so pick one); `toll config --format url` prints the bare base
URLs. Then inspect what you used:

```zsh
toll tail -n 10 --since 2h
toll stats --since 7d
toll stats --by-model
toll stats --by-client     # which tool spent it (x-toll-client / User-Agent)
toll stats --by-day        # daily trend
```

## Example output

```text
$ toll tail -n 4

[2026-07-19T10:42:18Z] anthropic claude-sonnet-4-5 200 1243ms tokens=16200→2450 cache_read=45200 $0.1132 client=opencode/0.4
[2026-07-19T10:50:02Z] anthropic claude-sonnet-4-5 429 820ms tokens=? $0.0000 ERROR=rate_limit client=opencode/0.4
[2026-07-19T10:43:22Z] openai gpt-4.1-mini 200 312ms tokens=1205→88 cache_read=980 $0.0003 client=hermes/1.2
[2026-07-19T10:45:11Z] openrouter qwen/qwen3-coder-480b 200 2100ms tokens=44021→12134 $0.0700 client=opencode/0.4
```

```text
$ toll stats --by-model

model                  calls  input    output   cache_read  cache_write  cache%  errors  p50_ms  p95_ms  cost_usd
---------------------  -----  -------  -------  ----------  -----------  ------  ------  ------  ------  ----------
claude-sonnet-4-5      3      24300    3650     57400       3800         67%     1       980     1243    0.1591
gpt-4.1-mini           2      2047     207      980         0            48%     0       312     640     0.0009
qwen/qwen3-coder-480b  1      44021    12134    0           0            -       0       2100    2100    0.0700
```

Add `--json` to either command for machine-readable output (`toll stats --json`,
`toll tail --json`), including the computed cost and, for `tail`, whether that cost
was `provider`-reported or `computed`.

## Supported providers

| Provider | Local base URL | Upstream |
| --- | --- | --- |
| OpenAI | `http://127.0.0.1:4000/v1` | `https://api.openai.com` |
| Anthropic | `http://127.0.0.1:4001` | `https://api.anthropic.com` |
| Gemini | `http://127.0.0.1:4002` | `https://generativelanguage.googleapis.com` |
| DeepSeek | `http://127.0.0.1:4003/v1` | `https://api.deepseek.com` |
| OpenRouter | `http://127.0.0.1:4004/api/v1` | `https://openrouter.ai` |
| Kimi | `http://127.0.0.1:4005/v1` | `https://api.moonshot.ai` |
| MiniMax | `http://127.0.0.1:4006/v1` | `https://api.minimaxi.com` |
| GLM | `http://127.0.0.1:4007/api/paas/v4` | `https://open.bigmodel.cn` |
| xAI | `http://127.0.0.1:4008/v1` | `https://api.x.ai` |
| Groq | `http://127.0.0.1:4009/openai/v1` | `https://api.groq.com` |

Prefer names over ports: every listener also routes by Host, so
`http://<provider>.localhost:4000<path>` works from any toll port — one port to
remember, e.g. `http://openrouter.localhost:4000/api/v1`. Print them all with
`toll config --format url`. A mistyped name (`typo.localhost`) is refused with 421
rather than forwarded to the wrong provider with the wrong credentials.

## Cost

Per-call cost prefers what the provider itself reports (OpenRouter's `usage.cost`,
xAI's cost ticks, Groq's `x_groq`, …). When the provider reports no cost, toll
computes it from a local price table — pulled from [models.dev](https://models.dev)
with `toll prices pull` — honoring each model's cache-read/creation rates and
context-length tiers (once a prompt crosses a provider's threshold, the whole call
reprices at the higher tier, the way Gemini/Grok/Claude bill it).

```zsh
toll prices pull    # refresh the local table from models.dev
toll prices show    # which table is active and how many models it covers
```

toll never *estimates* tokens a provider did not report — it runs no local
tokenizer. A successful call that returns no usage is stored with null tokens and a
`no_usage` marker, never a guess, so silent loss is visible rather than mistaken for
a free call. `stats` warns when token-bearing calls went unpriced and how stale the
price table is.

> `stats` and `tail` (table and `--json`) include computed costs. Raw SQL over the
> `cost` column sees only provider-reported costs and will undercount.

## What toll records

Usage metadata only, one SQLite row per call: timestamp, provider, model (the exact
billing slug, `vendor/` prefix included), endpoint path, status, latency (and
time-to-first-token), token counts (input / output / cache read / cache creation /
reasoning), cost, the calling tool (the request `User-Agent`, or an `x-toll-client`
header if your tool sets one), and an `anomaly` marker when toll's own observation
was degraded. The provider's verbatim `usage` object is kept in `raw_usage` for
audit. **Request and response bodies are never stored.** API keys and credentials in
error text are redacted before anything is written.

Records live at:

```text
${XDG_DATA_HOME:-$HOME/.local/share}/toll/calls.db     # usage log
${XDG_DATA_HOME:-$HOME/.local/share}/toll/prices.json  # price table
```

## What toll does not do

- **Not a gateway.** No routing, load balancing, rate limiting, caching, retries,
  budgets, or key vault. The only edit toll makes to a request is injecting
  `stream_options.include_usage` on OpenAI-style streams, so the final chunk carries
  token counts; everything else is forwarded byte-for-byte.
- **Not multi-tenant or network-exposed.** Listeners bind `127.0.0.1` only, and all
  data stays on your machine.
- **Not a telemetry pipeline.** No spans or metrics to an external collector — just a
  local SQLite log you own and can query with `stats`, `tail`, or plain SQL.

Recording is out-of-band and fire-and-forget: the forward path never waits on it, and
under backpressure toll drops observations rather than stall your stream.

## Status

`0.1.0`. Single Rust binary, MIT-licensed. See [`DESIGN.md`](DESIGN.md) for the
architecture, invariants, and the record of what was deliberately left out.
