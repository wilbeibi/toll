# toll

A local LLM API usage meter. `toll` runs a reverse proxy on `127.0.0.1` for each supported provider, forwards your requests to the real upstream API unchanged, and records per-call usage, latency, cache hits, status, and errors to SQLite.

You point your clients at toll instead of the upstream URL. Authentication is unchanged — toll forwards your existing API key.

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
cargo build --release
target/release/toll start
```

In another shell, point a client at toll and make a request:

```zsh
export OPENAI_BASE_URL=http://127.0.0.1:4000/v1
# ...your existing OPENAI_API_KEY stays as-is
```

Then inspect what you used:

```zsh
target/release/toll tail -n 10
target/release/toll stats
```

## Configure Clients

Print ready-to-paste shell exports for every provider:

```zsh
target/release/toll config
target/release/toll config --provider openrouter
target/release/toll config --format json
```

OpenAI-compatible providers share `OPENAI_BASE_URL`, so set it per-process or per-profile rather than globally if you use more than one.

## Run as a User Service

```zsh
systemctl --user restart toll.service
systemctl --user status toll.service --no-pager --lines=20
```

Toll only ever binds to `127.0.0.1`.

## Usage Data

Records live at:

```text
${XDG_DATA_HOME:-$HOME/.local/share}/toll/calls.db
```

SQLite WAL sidecar files may appear next to the database while the service is running.

```zsh
target/release/toll stats             # totals per provider
target/release/toll stats --by-model  # totals per model
target/release/toll tail -n 20        # recent calls
```

## Development

```zsh
cargo fmt --all --check
cargo test
cargo build --release
```

Code map:

- `src/providers.rs` — provider ports, upstream URLs, config snippets.
- `src/proxy.rs` — reverse proxy request/response handling and record writes.
- `src/record.rs` — SQLite schema and usage record contract.
- `src/parsers/` — provider-specific token extraction.
- `src/stats.rs`, `src/tail.rs` — read-only reporting commands.

The `Record` schema is a compatibility contract. New fields must be optional, forward-migrated, and covered by tests.

See [`DESIGN.md`](DESIGN.md) for the philosophy, architecture, and invariants behind toll — read it before non-trivial changes.

## Status

`0.1.0`. Single Rust binary; no runtime dependencies beyond the system.
