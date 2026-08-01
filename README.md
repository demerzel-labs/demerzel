# Demerzel

An open-source terminal coding agent in Rust. One lightweight binary, any model
provider, built for a rich extension ecosystem and a benchmark-backed
performance claim.

**Status: pre-alpha.** The product plan is [`PLAN.md`](PLAN.md); the benchmark
methodology is [`bench/README.md`](bench/README.md). Nothing is usable yet.

## Build

```bash
cargo build --release
./target/release/demerzel --version
```

## Principles

- **Provider-neutral**: OpenAI-compatible endpoints first (OpenAI, Ollama,
  vLLM, LM Studio), Anthropic next; no adapter is privileged.
- **Lightweight**: performance and memory numbers are product features and ship
  with reproducible receipts in [`bench/`](bench/).
- **Yours to extend**: markdown skills, subprocess hooks, MCP, subagents —
  and auto-discovery of the agent config already in your repos.

The previous contents of this repository (a Python pipeline review harness) are
preserved on the `archive/python-pipeline-harness-2026-07-31` branch.

## License

MIT
