# Demerzel benchmark suite

Performance claims are a product feature and ship with receipts. Every number
Demerzel publishes is produced by scripts in this directory, with raw data
committed under `results/`. Full context: [`PLAN.md`](../PLAN.md), "Benchmark
program".

## Rules

1. **Reproducible**: the suite is versioned with the code; anyone can rerun it.
2. **Pinned**: competitor versions are recorded in every report.
3. **Disclosed**: the machine spec (CPU, RAM, kernel, filesystem) accompanies
   every published table.
4. **Statistical**: warmups and ≥50 runs for timing; hyperfine's confidence
   intervals are part of the result, not trimmed off.
5. **Comparable**: where a comparison with another agent is claimed, the metric
   matches theirs — memory is PSS from `/proc/<pid>/smaps_rollup`, as jcode
   measures.
6. **Raw data or it didn't happen**: JSON output is committed next to any
   published summary.

## Layers

| Layer | What | How | Status |
|---|---|---|---|
| 1 | Boot time, memory footprint, binary size | hyperfine; smaps_rollup PSS/USS; cgroup-v2 `memory.peak`; cargo-bloat | boot + PSS primitive here; PTY scenarios land with M1 interactive mode |
| 2 | Harness overhead per turn/tool-call, render throughput | mock OpenAI-compatible server replaying canned transcripts; criterion; iai-callgrind CI gate | lands in M1 |
| 3 | Agentic capability (harness + model) | Terminal-Bench 2.0; fixed-model A/B vs other harnesses; Coding Agent Index as external target | lands in M3/M4 |
| 4 | Loop efficiency | prompt-cache hit rate on replays; tokens per completed task | lands in M4 |

## Running

```bash
cargo build --release

# Layer 1: boot (pure binary start; PTY time-to-interactive comes with M1)
bench/boot/run.sh

# Layer 1: memory primitive — PSS/USS for running agent PIDs
bench/memory/pss.py <pid> [pid...]
```

Requires `hyperfine` (Arch: `pacman -S hyperfine`). Linux-only for memory
(smaps_rollup).
