# Demerzel benchmark suite

Demerzel treats performance claims as product features. The scripts in this
directory produce every performance number that Demerzel publishes. The raw
data for each number goes into `results/`. The "Benchmark program" section of
[`docs/plan.md`](../docs/plan.md) describes the full program.

## Rules

1. **Reproducible.** The suite lives in this repository, next to the code. You
   can run the same scripts and compare your numbers with ours.
2. **Pinned.** Each report records the exact version of each competitor.
3. **Disclosed.** Each report records the machine data: CPU, RAM, kernel, and
   filesystem.
4. **Statistical.** Timing benchmarks use warmup runs and 50 or more measured
   runs. Each report keeps the confidence intervals that hyperfine calculates.
   The cache-hit measurement is the exception: it runs two live sessions and
   keeps no interval. Its claim is the per-call ratio, not a timing.
5. **Comparable.** When a report compares Demerzel with another agent, it uses
   the same metric as that agent, on the same machine and OS. On Linux the
   memory metric is PSS from `/proc/<pid>/smaps_rollup`, the metric the
   comparison targets publish. On macOS it is physical footprint, the metric
   Apple's tools report. Numbers never compare across operating systems.
   [`docs/plan.md`](../docs/plan.md) names the comparison targets.
6. **Raw data included.** We commit the raw JSON output next to each published
   summary.

## Layers

| Layer | Measures | Method | Status |
|---|---|---|---|
| 1 | Boot time, memory footprint, binary size | hyperfine; on Linux, PSS and USS from `smaps_rollup` and cgroup-v2 `memory.peak`; cargo-bloat | The boot runner, the PTY probe, the PSS sampler, and the idle-session driver are here. |
| 2 | Harness overhead per turn and per tool call; render throughput | A mock OpenAI-compatible server replays recorded transcripts for the turn benches. The render bench drives the transcript and the renderer in process. criterion runs micro-benchmarks. iai-callgrind gates CI on instruction counts. | The criterion turn benchmark and the render benchmark are here. iai-callgrind arrives with CI in M5. |
| 3 | Agentic capability of the harness plus a model | Terminal-Bench 2.0; fixed-model A/B benchmarks against other harnesses; the Coding Agent Index as the external target | Comes with M3/M4 |
| 4 | Loop efficiency | Prompt-cache hit rate on replayed sessions; tokens per completed task; memory recall precision | The cache-hit measurement is here. The rest comes with M4. |

## How to run

```bash
cargo build --release

# Machine spec. Each published number references one of these files.
bench/machine.py

# Layer 1: boot time, pure binary start.
bench/boot/run.sh

# Layer 1: time to interactive, warm and cold.
bench/boot/pty_probe.py
bench/boot/pty_probe.py --cold

# Layer 1: memory of an idle REPL session, over 10 launches.
bench/memory/idle.py

# Layer 1: PSS and USS of running agent processes.
bench/memory/pss.py <pid> [pid...]

# Layer 2: harness-added latency per turn, against the mock server.
bench/turn/run.sh

# Layer 2: frame cost and streamed-text throughput of the full screen.
bench/render/run.sh

# Layer 4: prompt-cache hit rate on a replayed session, live provider.
# Needs credentials and network access, and it costs tokens.
bench/cache/replay.py --provider anthropic
bench/cache/replay.py --provider openai-codex
```

The suite runs on Linux and macOS, the two reference platforms
([`docs/bench-platforms.md`](../docs/bench-platforms.md)). Result files
carry an OS-arch slug, for example `boot-2026-08-23-abc1234-macos-aarch64.json`.
Three gaps remain on macOS. The idle memory driver and `pss.py` run
only on Linux until the footprint probe lands (step 3). Cold mode runs
only on Linux until the eviction decision lands (step 4).

The boot benchmark needs `hyperfine`. On Arch, install it with
`pacman -S hyperfine`. On macOS, install it with
`brew install hyperfine`. The probes need Python 3.13 or newer. On
macOS, install Python from Homebrew or python.org. `bench/harness.py`
holds the shared probe machinery. `platform_linux.py` and
`platform_macos.py` hold the native probes.

Method notes:

- The probe gives each run a private data home and HOME with a synthetic
  auth store. A run needs no login and makes no network call.
- The probe records its own spawn floor, measured through the identical
  PTY path with `printf`. The floor sits inside every sample.
- Cold mode, on Linux, evicts the binary from the page cache with `posix_fadvise`
  and verifies the eviction with `mincore`. Shared libraries and
  metadata caches stay warm, so the cold number tracks the binary read
  and moves with binary size. A true cold start needs a fresh boot.
- The memory driver samples N launches. Anonymous memory is the
  headline: it is the harness's own heap and it is stable. PSS and USS
  also count clean mapped binary text, which moves with page-cache
  state; reports keep them because cross-agent comparisons use PSS.
- The turn suite reports `transport_floor` next to the turn benches.
  Harness-added latency per turn is `turn_text` minus
  `transport_floor`. Session files land under the cargo target
  directory, so appends hit a real filesystem.
- The render suite draws into a headless frame buffer. The layout and
  the buffer diff are the live interface's work; the terminal write is
  not. Three benches:
  1. `frame` is one redraw with no new content, the floor of a frame.
  2. `ingest` pushes 64 KiB of text in 16-byte deltas with no frames.
  3. `stream` pushes the same text with a frame every 64 deltas. It is
     frame-bound, so its throughput is frames per second.
- The cache-hit measurement replays the user side of one session: a
  fixed list of user turns. It runs them through the real engine
  against one live provider, as two fresh sessions one after the
  other. The model side is live, so the two sessions differ in what
  the model reads. The report gives `cache_read_tokens /
  input_tokens` per model call, per turn, and per session.
  `input_tokens` counts the cached parts for both providers. The
  engine advertises the full tool set in `ask` mode with no approver,
  so only the read tools run. Sessions land in a private temporary
  data root that goes away with the run. Credentials resolve the way
  a session resolves them. What to compare across milestones:
  1. The per-call ratio. A warm call that reads its predecessor's
     whole prompt shows a stable prefix.
  2. The first call of the second session. It shares its whole prefix
     with the first session's first call. A non-zero rate shows the
     prefix holds across sessions; a zero can also mean the prefix is
     under the provider's minimum cacheable length.
  Session totals move with the model's tool choices, so they do not
  compare.
