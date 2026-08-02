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
| 1 | Boot time, memory footprint, binary size | hyperfine; PSS and USS from `smaps_rollup`; cgroup-v2 `memory.peak`; cargo-bloat | The boot runner and the PSS sampler are here. The PTY scenarios come with the M1 interactive mode. |
| 2 | Harness overhead per turn and per tool call; render throughput | A mock OpenAI-compatible server replays recorded transcripts. criterion runs micro-benchmarks. iai-callgrind gates CI on instruction counts. | Comes with M1 |
| 3 | Agentic capability of the harness plus a model | Terminal-Bench 2.0; fixed-model A/B benchmarks against other harnesses; the Coding Agent Index as the external target | Comes with M3/M4 |
| 4 | Loop efficiency | Prompt-cache hit rate on replayed sessions; tokens per completed task; memory recall precision | Comes with M4 |

## How to run

```bash
cargo build --release

# Layer 1: boot time (pure binary start; the PTY probe comes with M1)
bench/boot/run.sh

# Layer 1: memory. Give the PIDs of running agent processes.
bench/memory/pss.py <pid> [pid...]
```

The boot benchmark needs `hyperfine`. On Arch Linux, install it with
`pacman -S hyperfine`. The memory benchmark works only on Linux, because it
reads `/proc/<pid>/smaps_rollup`.
