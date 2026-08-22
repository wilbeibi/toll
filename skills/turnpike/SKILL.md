---
name: turnpike
description: Meter local LLM API spend with the `turnpike` CLI — point a tool or script at the local proxy so its calls are recorded and attributed, then read the money back with `stats`, `tail`, and `check`. Use when writing or running anything that calls an OpenAI, Anthropic, Gemini, DeepSeek, OpenRouter, Kimi, MiniMax, GLM, xAI, or Groq API from this machine, when asked what a tool or model cost, when a budget gate is wanted in a hook or cron job, or when a call isn't showing up in turnpike. Do NOT use for a full spend teardown or cheaper-model advice (use turnpike-spend-review), and not for subscription tools like Claude Code or Codex, which never pass through turnpike.
---

# turnpike

A local meter, not a gateway. Point a client at it and every call that client makes
is written down — model, tokens, cost, latency, who called. What it never sees it can
never tell you about, so most of the work is getting the traffic through it and named.

## Send traffic through it

```zsh
eval $(turnpike config --provider openrouter)          # zsh/bash: exports the base URL
turnpike config --provider xai --format fish | source  # fish
turnpike config --provider gemini --format url         # bare URL, for code that takes base_url=
turnpike config                                        # all providers (OpenAI-shaped ones share one var)
```

Keep the same API key and auth header — turnpike passes them upstream untouched, and
the base URL is the only thing that changes. Always ask `turnpike config` for it; never
type one from memory or copy one out of a README. Ports, paths, and env var names all
differ per provider, and one with no base-URL convention (Gemini) prints nothing in
shell format — take `--format url` and pass it in code (`google-genai`:
`http_options={"base_url": ...}`).

Then say who is calling, or the spend lands under a runtime name:

```zsh
curl -H 'x-turnpike-client: myscript' ...
```
```python
OpenAI(base_url=..., default_headers={"x-turnpike-client": "worthit:score"})
```

Stored verbatim, first 128 bytes. `<tool>` or `<tool>:<task>` — the `:task` half is
convention, not syntax, but it's what makes `--by-tool` readable when one tool does
several jobs. With no header turnpike records the User-Agent instead, and
`--by-tool` prefers the observed process over it — both name a runtime (`node`,
`python`) rather than your tool.

## Read it back

```zsh
turnpike tail -n 20 --since 2h    # last calls, one line each; anomaly + error shown
turnpike stats --since 7d         # by provider
turnpike stats --by-tool          # best identity per call: header, else process, else UA
turnpike stats --by-model         # or --by-client, --by-day, --by-exe (Linux only; one at a time)
turnpike check --budget 50/day    # 0 under, 1 at/over, 2 error, 3 unknown
turnpike prices show              # rates in force for the models you actually call
```

`--since` takes `30m`, `12h`, `7d`, `today`, `2026-07-01`, or an RFC-3339 instant.
`--json` works on `stats`, `tail`, and `check`.

## Operation

- Nothing recorded is not the same as nothing spent. Only clients you pointed at
  turnpike appear; subscription tools (Claude Code, Codex) bypass it entirely, and
  each machine keeps its own `calls.db` — a query here is a one-host answer.
- A call that didn't show up, in order: is `turnpike start` alive (it runs under a
  supervisor — systemd user unit or launchd, no daemon mode of its own); does the
  *calling process* actually have the base URL (exporting a var doesn't reach a
  daemon that started before you did); is it an inference endpoint (model listings
  and probes are proxied but deliberately not logged); did the provider report usage
  at all (a `no_usage` row is recorded with no counts rather than guessed).
- `turnpike check` is a meter, not a notifier — branch on the exit code and send your
  own alert. Keep the four outcomes distinct: 3 is "can't vouch for the number yet"
  (no calls, or no price), not a pass and not a failure; 2 means the invocation or
  the data is broken.
- `turnpike prices pull` appends dated revisions, so old calls keep the rate they
  were billed at. `prices.json` is your price history, not a cache: never delete it
  or rebuild it from scratch, and hand-edit `effective_from` when you know the real
  date. `prices show` prints `NO PRICE` for a model it can't price — fix that before
  quoting a total.
- Read totals through `stats`/`tail`/`check`, not raw SQL: the stored `cost` column
  holds only costs the provider itself reported; those commands fill the rest in.
- If the ask is routing, fallback, retries, budget *enforcement*, or key storage,
  turnpike is the wrong tool — it only watches.

For a full spend teardown, waste hunting, or model-substitution advice, use the
**turnpike-spend-review** skill; direct SQL against `calls.db` lives there.
Run `turnpike <command> --help` for the complete flag list.
