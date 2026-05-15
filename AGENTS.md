# AGENTS.md — toll

Project-specific guidance for toll.

## Repo at a glance
- Rust 2021 single-binary project. Binary name: `toll`.
- A localhost reverse proxy and usage meter for LLM API calls. Binds only to `127.0.0.1`.
- Expected to run as a user-level systemd service (`toll.service`).
- Data path: `${XDG_DATA_HOME:-$HOME/.local/share}/toll/calls.db` (SQLite, with WAL sidecars).

## Code map
- `src/main.rs` — entrypoint.
- `src/cli.rs` — clap subcommands (`start`, `stats`, `tail`, `config`).
- `src/providers.rs` — provider table: ports, upstream URLs, config snippets, streaming quirks.
- `src/proxy.rs` — reverse proxy request/response handling and record writes.
- `src/record.rs` — SQLite schema and the `Record` wire contract.
- `src/parsers/`, `src/sse.rs`, `src/json_usage.rs` — token extraction and SSE merging.
- `src/stats.rs`, `src/tail.rs` — read-only reporting.
- `tests/` — integration tests and fixtures.

## Verification commands
Run from repo root. Prefix with `rtk` per machine policy.

```zsh
rtk cargo fmt --all --check
rtk cargo test
rtk cargo build --release
```

Smoke checks against the built binary:

```zsh
rtk target/release/toll config --format json
rtk target/release/toll stats
rtk target/release/toll tail -n 20
```

If asked to restart the running service:

```zsh
rtk systemctl --user restart toll.service
rtk systemctl --user status toll.service --no-pager --lines=20
```

Do not use `sudo` from an agent session — ask the user to run elevated commands in a separate terminal.

## Hard rules for changes
- **`Record` schema is locked.** New fields must be optional, appended, forward-migrated, and covered by tests. Renames or removals require an explicit version bump and a changelog entry.
- **Don't change client app source.** If a provider can be redirected by config (`OPENAI_BASE_URL` etc.), do that instead.
- **Preserve streaming behavior.** Keep `stream_options.include_usage` injection for OpenAI-compatible providers unless the upstream genuinely cannot support it. Anthropic and Gemini report usage via their own SSE events — don't break those paths.
- **Provider edits live in `src/providers.rs`.** Update the provider table in the README when ports, base URLs, or routing change.
- **Never delete user data** under `$XDG_DATA_HOME/toll` or `$HOME/.local/share/toll` unless explicitly requested.
- **Never print or log API keys, bearer tokens, or copied request bodies that may contain credentials.**
- Add parser tests whenever you change token extraction for Anthropic, Gemini, OpenAI-compatible APIs, or SSE merging.

## Style
- Keep edits small and local to the relevant module.
- zsh-compatible shell examples only.
