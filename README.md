# toll

A local meter for LLM API calls. One JSONL record per call: tokens, cache, latency, errors.

**Status: under rewrite.** The Python + mitmproxy implementation has been removed.
The replacement is a single static Rust binary that runs as a reverse proxy per provider.
See [`PLAN.md`](PLAN.md) for context, motivation, and the build plan.
