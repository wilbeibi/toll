# toll

### See which of your tools is spending your LLM API money

`toll` is for the moment your API bill is bigger than you expected and you can't
tell which tool ran it up. I had a handful of LLM tools on this laptop — a coding
agent, a couple of chat CLIs, a script that summarizes pages — all billing to the
same keys, with no way to split the cost. So I built this, and it's been running
ever since.

toll sits on your machine, between your tools and the provider APIs. Point a tool
at toll instead of the provider, keep the same API key, and it writes down every
call: which model, how many tokens, what it cost, how long it took, and which
tool made it. You read it back from the terminal.

It only watches. Each request goes to the provider unchanged; toll records what
happened on the side. Nothing leaves your machine, and if the logging ever
breaks, your request still goes through.

Use it when you want to know:

- Which tool spent the most today, and which model ate the most tokens.
- Whether that failed request still reached the provider and billed you.
- If your cache hits are actually landing.
- Which local tools are quietly calling which APIs.

Works with **OpenAI**, **Anthropic**, **Gemini**, **DeepSeek**, **OpenRouter**,
**Kimi**, **MiniMax**, **GLM**, **xAI**, and **Groq**.

## What it looks like

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

## Install

```zsh
git clone https://github.com/wilbeibi/toll
cd toll
cargo install --path .
toll start                 # start the listeners (runs in the foreground)
toll prices pull           # optional: pull a price table so costs are filled in
```

## Usage

Point a tool at toll and use it exactly as before:

```zsh
eval $(toll config --provider openrouter)   # sets OPENAI_BASE_URL to http://127.0.0.1:4004/api/v1
# fish:  toll config --provider xai --format fish | source
```

`toll config` with no provider lists every provider (the OpenAI-shaped ones share
one base URL, so pick any); `toll config --format url` prints just the URLs.

Then read back what you used:

```zsh
toll tail -n 10 --since 2h
toll stats --since 7d
toll stats --by-model
toll stats --by-client     # which tool spent it
toll stats --by-day        # daily trend
```

Add `--json` to `stats` or `tail` for machine-readable output.

### Providers and ports

Each provider has its own local port (below). You can also skip the ports and use
names: `http://<provider>.localhost:4000` routes by name from any toll port, so
there's one to remember instead of ten. That needs a client that resolves
`*.localhost` to your own machine — most browsers and Linux do, macOS and slim
containers sometimes don't, so if a name won't connect, use the `127.0.0.1` port.
A mistyped name is refused either way, never sent to the wrong provider with the
wrong key.

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

## Cost

Costs come from what the provider reports. When a provider doesn't report one,
toll works it out from a local price table you can refresh:

```zsh
toll prices pull    # refresh the table from models.dev
toll prices show    # what's loaded and how many models it covers
```

toll never guesses tokens a provider didn't report. A call that comes back with
no usage is saved as exactly that — no counts, marked `no_usage` — so nothing
quietly reads as free.

## What it records, and what it doesn't

One row per call, usage only: time, provider, model, endpoint, status, latency,
token counts, cost, and the tool that made the call.

**Your prompts and responses are never stored.** Keys or credentials that show up
in error text are scrubbed before anything is written. It all lives in a local
SQLite file you own:

```text
${XDG_DATA_HOME:-$HOME/.local/share}/toll/calls.db
```

Read it with `stats` and `tail`, or open it with any SQLite tool. (The stored
`cost` column holds only costs the provider itself reported; `stats` and `tail`
add the computed ones, so use those for a full total.)

## Boundaries

- A meter, not a gateway. It doesn't route, balance, cache, retry, hold budgets,
  or store your keys.
- Local only. Listeners bind `127.0.0.1`; nothing is network-exposed.
- Usage metadata only. No prompt or response bodies, ever.
- No external telemetry. Just the local file.

## Status

`0.1.0`. One Rust binary, MIT-licensed.
