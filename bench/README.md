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
5. **Comparable.** When a report compares Demerzel with another agent, it uses
   the same metric as that agent. For memory, the metric is PSS from
   `/proc/<pid>/smaps_rollup`, because jcode publishes PSS.
6. **Raw data included.** We commit the raw JSON output next to each published
   summary.

## Layers

| Layer | Measures | Method | Status |
|---|---|---|---|
| 1 | Boot time, memory footprint, binary size | hyperfine; PSS and USS from `smaps_rollup`; cgroup-v2 `memory.peak`; cargo-bloat | The boot runner, the PTY probe, the PSS sampler, and the idle-session driver are here. |
| 2 | Harness overhead per turn and per tool call; render throughput | A mock OpenAI-compatible server replays recorded transcripts. criterion runs micro-benchmarks. iai-callgrind gates CI on instruction counts. | The criterion turn benchmark is here. iai-callgrind arrives with CI in M5. |
| 3 | Agentic capability of the harness plus a model | Terminal-Bench 2.0; fixed-model A/B benchmarks against other harnesses; the Coding Agent Index as the external target | Comes with M3/M4 |
| 4 | Loop efficiency | Prompt-cache hit rate on replayed sessions; tokens per completed task; memory recall precision | Comes with M4 |

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
```

The boot benchmark needs `hyperfine`. On Arch Linux, install it with
`pacman -S hyperfine`. The PTY probe and the memory benchmarks work only on
Linux and need Python 3.13 or newer. `bench/harness.py` holds the shared
probe machinery.

Method notes:

- The probe gives each run a private data home and HOME with a synthetic
  auth store. A run needs no login and makes no network call.
- The probe records its own spawn floor, measured through the identical
  PTY path with `printf`. The floor sits inside every sample.
- Cold mode evicts the binary from the page cache with `posix_fadvise`
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
