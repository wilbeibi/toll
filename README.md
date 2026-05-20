# toll

`toll` is a personal usage meter for LLM APIs. It gives you a local, searchable record of what your AI tools are using across OpenAI, Anthropic, Gemini, OpenRouter, DeepSeek, and other providers.

Run it on your machine, point your clients at `localhost`, and keep using your normal provider API keys. `toll` forwards each request to the real provider and records the useful parts locally: tokens, cost when reported, latency, cache hits, status, model, and errors.

Use it when you want to know:

- What did I call recently?
- Which provider or model is using the most tokens or money?
- Did a failed request reach the provider?
- Are cache hits actually happening?
- Which tools are quietly sending traffic to which APIs?

No hosted dashboard, no new API keys, no account to create. `toll` only listens on `127.0.0.1` and stores usage on your machine.

## How It Works

Your client sends requests to toll instead of the provider's base URL. Toll forwards the request upstream, streams the response back, and records what happened locally.

```text
your client -> http://127.0.0.1:<provider-port> -> provider API
                         |
                         +-> local SQLite usage log
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

## Quick Start

```zsh
cargo install --path .
toll start
```

This installs `toll` to Cargo's default binary directory, usually `~/.cargo/bin`. Make sure that directory is on your `PATH`.

Leave that running. In another shell, point a client at toll and make a normal request:

```zsh
export OPENAI_BASE_URL=http://127.0.0.1:4000/v1
# Keep your existing OPENAI_API_KEY unchanged.
```

Then inspect what you used:

```zsh
toll tail -n 10
toll stats
```

## Configure Clients

Print ready-to-paste shell exports:

```zsh
toll config
toll config --provider openrouter
toll config --format json
```

OpenAI-compatible providers share `OPENAI_BASE_URL`, so set it per-process or per-profile rather than globally if you use more than one.

## Running Long-Term

The simplest way to try toll is `toll start` in a terminal. For daily use, run the installed binary with your usual user-level process manager.

If you have already set up a user systemd unit named `toll.service`, these commands are useful:

```zsh
systemctl --user restart toll.service
systemctl --user status toll.service --no-pager --lines=20
```

Toll is designed to bind only to `127.0.0.1`.

## Usage Data

Usage records live at:

```text
${XDG_DATA_HOME:-$HOME/.local/share}/toll/calls.db
```

SQLite WAL sidecar files may appear next to the database while the service is running.

```zsh
toll stats             # totals per provider
toll stats --by-model  # totals per model
toll tail -n 20        # recent calls
```

## Status

`0.1.0`. Single Rust binary. Local-first by design. See [`DESIGN.md`](DESIGN.md) for architecture and contribution notes.
