# Demerzel

Demerzel is an open-source terminal coding agent written in Rust. It runs as
one small binary. It connects to the model provider that you choose. Its three
goals are a rich extension ecosystem, deep customization, and measured
performance.

**Status: pre-alpha.** You cannot use Demerzel yet. [`PLAN.md`](PLAN.md)
contains the product plan. [`bench/README.md`](bench/README.md) contains the
benchmark method.

## Build

```bash
cargo build --release
./target/release/demerzel --version
```

## Principles

- **Provider-neutral.** No provider adapter has a special position. The first
  adapter speaks the OpenAI-compatible API and covers OpenAI, Ollama, vLLM,
  and LM Studio. The Anthropic adapter comes next.
- **Lightweight.** Speed and memory use are product features. Each published
  number comes from a reproducible benchmark in [`bench/`](bench/).
- **Yours to extend.** You can add markdown skills, subprocess hooks, MCP
  servers, and subagents. Demerzel also finds the agent configuration that
  other tools already put in your repository.

The branch `archive/python-pipeline-harness-2026-07-31` holds the previous
contents of this repository: a Python review harness for CI pipelines.

## License

MIT
