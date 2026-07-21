---
name: toll-spend-review
description: Analyze toll's call database to answer two questions — where did the LLM API money go, and would a cheaper or better model do the same work. Use when the user asks "where is my LLM spend going", "toll spend review", "is there a cheaper model for this task", "token usage analysis", or wants a cost teardown of API traffic metered by toll. Do NOT use for subscription traffic (Claude Code, Codex) — it never passes through toll and is not in the database.
---

# toll-spend-review

Turn toll's recorded facts into a spend report plus model-substitution suggestions. toll only records; all judgment (task type, quality tradeoffs, recommendations) happens here.

## Data sources

- `toll stats` / `toll tail` — first choice. Grouping: `--by-model | --by-client | --by-exe | --by-day`; filter `--since 30m|12h|7d|today|2026-07-01`; `--json` includes computed costs. Run `toll stats --help` for details.
- `~/.local/share/toll/calls.db` — SQLite (WAL; read-only queries are safe), table `calls`. Key columns: `ts, provider, model, client, endpoint, peer_exe, input_tokens, output_tokens, cache_read_input_tokens, cache_creation_input_tokens, reasoning_output_tokens, cost, raw_usage, anomaly`.
- `~/.local/share/toll/prices.json` — current rates. Refresh with `toll prices pull` before repricing anything.
- Quality benchmarks: fetch live (models.dev API, Artificial Analysis, LMArena) — never rank model quality from memory; it stales in weeks.

## Workflow

1. `toll prices pull`, then survey: `toll stats --since 30d --json`, then `--by-exe`, `--by-client`, `--by-model` to find the dominant source × model pairs.
2. Drill into the top pairs with SQL (below). Segment by provider before summing anything (see traps).
3. Identify the task behind each expensive source. `client` (`x-toll-client: <tool>[:<task>]`) and `peer_exe` say *who*; if they don't reveal *what kind of work* (OCR / distillation / chat / coding), ask the user — never guess task type from token shape alone.
4. Fetch current prices + benchmarks for candidate substitutes; recommend only same-task-class swaps, with estimated saving labeled "at current rates".

Canonical drill-down (spend by source × model):

```sql
SELECT COALESCE(client, peer_exe, 'unknown') src, model,
       COUNT(*) n, SUM(cost) spent,
       SUM(input_tokens) inp, SUM(cache_read_input_tokens) hit,
       SUM(output_tokens) out, SUM(reasoning_output_tokens) think
FROM calls WHERE ts >= ? GROUP BY src, model ORDER BY spent DESC;
```

Cache-hit decay vs idle gap (the "avoidable miss" lever — hit rate can fall 92%→31% past 1h idle, and a miss token can cost ~100× a hit):

```sql
WITH g AS (SELECT ts, model, input_tokens inp, cache_read_input_tokens hit,
  (julianday(ts)-julianday(LAG(ts) OVER (PARTITION BY provider ORDER BY ts)))*86400 gap
  FROM calls WHERE input_tokens > 0)
SELECT CASE WHEN gap<30 THEN '<30s' WHEN gap<300 THEN '30s-5m'
       WHEN gap<900 THEN '5-15m' WHEN gap<3600 THEN '15-60m' ELSE '>1h' END bucket,
       COUNT(*) n, ROUND(100.0*SUM(hit)/SUM(inp),1) hit_pct
FROM g WHERE gap IS NOT NULL GROUP BY bucket ORDER BY MIN(gap);
```

## Semantic traps (each one has produced a wrong number before)

- **`cost` is historical.** Computed at insert time with then-current rates. Repricing the same tokens with today's `prices.json` can differ by >2× (provider price cuts). Report stored `cost` as "what was paid"; label any repriced split "at current rates". Never mix the two in one total.
- **Cache accounting differs by provider.** OpenAI/DeepSeek/Gemini: `cache_read` is a *subset* of `input_tokens` (uncached = `input − cache_read`). Anthropic: cache fields are *additive* on top of `input_tokens`. Summing across providers without segmenting double-counts. `prices.json` records this per model as `cache_in_input`.
- **Absent traffic ≠ zero spend.** Subscription tools (Claude Code, Codex) bypass toll. Raw `SUM(cost)` also undercounts calls where the provider reported no cost — prefer `toll stats`, which fills from the price table.
- **Each machine has its own DB.** joi and mini run separate toll instances; a one-host query is a one-host answer. (mini: `ssh mini`, fish shell.)
- **Older rows are sparser.** `raw_usage`, `client`, `peer_exe` were added over time and are NULL on early rows; typed token columns are the complete series.
- **A runtime UA names a runtime, not a tool.** Bare `node` / `python-requests` could be any script or harness (an eval runner fanning equal call-counts across models looks nothing like an agent). Corroborate with `peer_exe`, timestamps vs known runs, and the actual binary's shebang before attributing spend to a named tool — and treat equal-count multi-model bursts as one-shot evals, not recurring workload to optimize.

## Output contract

Report three sections, numbers reconciled before presenting: (1) spend by source × model; (2) waste findings — avoidable cache misses, zero-cache high-thinking models, idle-gap losses; (3) substitution table: current model → candidate, task class, est. monthly saving at current rates.
