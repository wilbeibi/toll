# toll

**See exactly where your LLM API tokens are going.**

`toll` is a local proxy that records every LLM API call your tools make — model, tokens, cost, latency, cache hits, status, and errors — into a searchable SQLite database.

Point your OpenAI, Anthropic, Gemini, OpenRouter, DeepSeek, or Kimi clients at `localhost`, keep using your existing API keys, and inspect usage from your terminal.

Use it when you want to answer:

- Which tool spent the most money today?
- Which model is eating the most tokens?
- Did a failed request still reach the provider?
- Are cache hits actually happening?
- What did my agent call in the last 10 minutes?
- Which local tools are quietly sending traffic to which APIs?

No hosted dashboard. No new account. No new API keys. Usage is stored locally in SQLite.

```text
your client -> http://127.0.0.1:<provider-port> -> provider API
                         |
                         +-> local SQLite usage log
```

## Quick Start

```zsh
git clone https://github.com/wilbeibi/toll
cd toll
cargo install --path .
toll start
```

In another shell, print ready-to-paste exports and point your client at toll:

```zsh
eval $(toll config)                        # all providers
eval $(toll config --provider openrouter)  # one provider
```

Then inspect what you used:

```zsh
toll tail -n 10
toll stats
toll stats --by-model
```

## Example Output

```text
$ toll tail -n 3

[2026-05-20T10:42:18Z] openai gpt-4.1-mini 200 1243ms tokens=842→119 $0.0003
[2026-05-20T10:43:01Z] anthropic claude-sonnet-4-5 429 823ms tokens=? ERROR=rate_limit
[2026-05-20T10:43:22Z] openai gpt-4.1-mini 200 312ms tokens=1205→88 cache_read=980 $0.0001
```

```text
$ toll stats --by-model

model                  calls  input    output   cache_read  errors  cost_usd
---------------------  -----  -------  -------  ----------  ------  ----------
claude-sonnet-4-5      13     210523   31812    45200       1       1.9200
gpt-4.1-mini           42     81243    9412     0           0       0.1800
qwen/qwen3-coder-480b  8      44021    12134    0           0       0.0700
```

## Supported Providers

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

## Usage Data

Records live at:

```text
${XDG_DATA_HOME:-$HOME/.local/share}/toll/calls.db
```

`toll` records usage metadata only — model, tokens, cost, latency, status, and errors. Request and response bodies are not stored.

## Local-First

`toll` binds to `127.0.0.1` by default and stores all data on your machine.

## Status

`0.1.0`. Single Rust binary. See [`DESIGN.md`](DESIGN.md) for architecture and contribution notes.
