# Demerzel

![Demerzel: a geometric archive with cyan and gold linework.](docs/assets/readme-header.png)

[![CI on main](https://github.com/demerzel-labs/demerzel/actions/workflows/ci.yml/badge.svg?branch=main&event=push)](https://github.com/demerzel-labs/demerzel/actions/workflows/ci.yml?query=branch%3Amain)

Demerzel is an open-source terminal coding agent written in Rust. One
small binary runs an agent loop in your terminal. It connects to the
provider that you choose.

**Status: pre-alpha.** Demerzel is usable from source. It has no
packaged release yet. The current build has a full-screen interface,
a plain fallback, two providers, layered config, login, resume,
slash commands, and the core file and shell tools. M5 is the planned
packaged launch.

## Prerequisites

- Git.
- A Rust toolchain with `rustfmt` and `clippy`.
- A native linker for your platform.
- Linux or macOS.

If you use `rustup`, `rust-toolchain.toml` selects stable Rust and
the required components.

## Build

```bash
git clone https://github.com/demerzel-labs/demerzel.git
cd demerzel
cargo build --release --locked
./target/release/demerzel --version
```

The source build creates a binary named `demerzel`. Installed
releases will also provide `dmz` as the short command. To work in
another project, run this binary by its absolute path.

## Log in

Use one of the two shipped providers:

```bash
./target/release/demerzel login
./target/release/demerzel login anthropic
```

`login` with no provider opens the browser flow for a ChatGPT
subscription. `login anthropic` asks for an Anthropic API key,
checks it, and stores it. A login stores credentials only. It does
not change the provider for later runs.

Use Anthropic with a flag:

```bash
./target/release/demerzel --provider anthropic doctor
./target/release/demerzel --provider anthropic
```

Or set the provider in your user config file (see [Configuration](#configuration)):

```toml
provider = "anthropic"
```

You can also set `ANTHROPIC_API_KEY` for Anthropic.

Run `doctor` before the first session:

```bash
./target/release/demerzel doctor
```

`doctor` shows config, credentials, and tool checks. It reports
whether a session can start. It does not print credential values.

## First session

```bash
./target/release/demerzel
```

With a real terminal, Demerzel opens the full-screen interface. Use
`--plain` for the line interface:

```bash
./target/release/demerzel --plain
```

Pass a quoted prompt for one-shot mode:

```bash
./target/release/demerzel "explain this error"
```

One-shot mode turns `ask` into `read-only` because no user can
answer a prompt. A flag or config can still set the effective mode
to `auto`. In an interactive session, the default approval mode is
`ask`.

Useful commands:

```text
/help              list commands
/model [name]      show or switch the model
/effort [level]    show or set reasoning effort
/approval [mode]   show or set approval mode
/session           show the session id and file
/new               start a fresh session at the next prompt
/exit              end the session
```

Resume the latest session in the current project:

```bash
./target/release/demerzel -c
```

Resume one session by id. Replace `SESSION_ID` with the id from `/session`:

```bash
./target/release/demerzel --session SESSION_ID
```

## Configuration

See [`docs/config.md`](docs/config.md) for all keys. The main
layers are command-line flags, environment variables, a project
file at `.demerzel/config.toml`, and a user file at
`$XDG_CONFIG_HOME/demerzel/config.toml`.

## Safety notes

Demerzel stores sessions, history, credentials, and optional debug
logs under the XDG config and data directories. Session files and
logs can include prompts, tool inputs, tool output, file contents,
and model text. Do not attach raw session files or debug logs to a
public issue without reviewing them first.

Approval mode controls which tools Demerzel advertises and asks to
run. It is not an operating-system sandbox. The `bash` tool runs a
real shell command with a constructed environment and a timeout.
Write tools stay inside the project root, but `bash` in `auto` mode
can write wherever the process can write.

## Principles

- **Provider-neutral.** The current build ships `openai-codex` and
  `anthropic`. Each adapter translates into Demerzel's provider
  contract.
- **Lightweight.** Speed and memory use are product features. Each published
  number comes from a reproducible benchmark in [`bench/`](bench/).
- **Yours to extend.** Skills, hooks, MCP, subagents, memory, and
  compatible local endpoints are planned work. The current extension
  surface is config plus the built-in slash commands.

## Current limits

- Linux and macOS are the supported targets. Windows is out of scope.
- There is no packaged release or installer yet.
- `openai-compat`, managed local models, skills, hooks, MCP,
  subagents, memory, personality, and compaction are planned.
- Long-running background commands are not supported. The shell tool
  ends its process group after each call.
- The ChatGPT-subscription provider uses an endpoint that is not a
  public API. It can change outside a Demerzel release.

## Docs

[Repository history](docs/history.md) explains commit IDs, benchmark receipts, and historical pull requests.

[`docs/plan.md`](docs/plan.md) contains the product plan.
[`docs/m2.5-plan.md`](docs/m2.5-plan.md) contains the current
milestone state. [`bench/README.md`](bench/README.md) contains the
benchmark method.

## Contributing

We welcome focused contributions, including work made with AI assistance.
You are responsible for understanding and verifying every change you submit.
See the [contribution guide](docs/contributing.md) for evidence and review requirements.

## Prior art

Demerzel builds on ideas from coding agents that came before it.
[`docs/prior-art.md`](docs/prior-art.md) names each source and the
license that applies. Demerzel does not copy code from other projects.

## License

[MIT](LICENSE).
