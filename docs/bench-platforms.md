# Benchmark Platforms Plan — Linux and macOS

This plan makes the benchmark suite produce receipts on both release
targets: Linux and macOS. Today the suite runs only on Linux. macOS is
the platform most day-to-day developers use, so its numbers matter at
least as much. The plan also restates the binary-size posture: size is
a budget with receipts, not a wall.

Status (2026-08-23): draft, in review. The build order runs before the
M2 close-out, because the close-out receipts must come from both
platforms.

## Goals

1. Every Layer-1 and Layer-2 number has a receipt from each reference
   machine: one Linux, one macOS.
2. One script tree serves both platforms. A bench either runs on both,
   or the support matrix names the gap and the reason.
3. The fixtures stay identical across platforms, so a difference in a
   number traces to the platform, not the scenario.
4. Numbers never compare across operating systems. Each comparison
   holds one machine, one OS, and one metric constant.

## Principles

- **Same metric, same OS.** A cross-agent comparison uses the metric
  the competitor publishes, measured on the same machine and OS. The
  jcode bar (14 ms boot, 27.8 MB PSS) is a Linux-PSS claim; it gates
  the Linux track only. The macOS track gets its own baseline:
  competitors measured by us, on the macOS reference machine, with the
  macOS metric.
- **Per-machine trend lines.** A receipt compares against the previous
  receipt from the same machine. The trend is the claim; the absolute
  transfers only with the machine spec attached.
- **Portable fixtures, native probes.** Scenarios, markers, transcripts,
  and caps live in shared code and never fork per platform. Only the
  probe layer — how the harness reads memory or evicts a cache — is
  native per OS.
- **Honest gaps.** A measurement one platform cannot support is marked
  unsupported in the support matrix, with the reason. No emulated
  numbers.

## Reference machines

| Role | Machine | OS | Notes |
|---|---|---|---|
| Linux reference | Ryzen 9 5900X, 64 GB | Arch Linux | The machine behind every receipt through M2. |
| macOS reference | Apple M2 Pro, 16 GB | macOS 26 | New with this plan. |

The machine spec file names the exact hardware and toolchain per
receipt, as today. Machine hostnames stay out of the repository.

## Metric map

| Measurement | Linux | macOS |
|---|---|---|
| Boot, binary start | hyperfine over `--version` | same |
| Boot, time to interactive | PTY probe, `posix_spawn` | same |
| Cold boot | `posix_fadvise` eviction, `mincore` verify | `purge` eviction, `mincore` verify; step 4 decides |
| Memory, per process | PSS and USS from `/proc/<pid>/smaps_rollup` | physical footprint from `proc_pid_rusage` |
| Memory, peak | cgroup-v2 `memory.peak` | `ri_lifetime_max_phys_footprint` |
| Page-cache residency | `mincore` via libc | `mincore` via libSystem |
| Machine spec | `/proc`, `/sys`, `/etc/os-release`, `findmnt` | `sysctl`, `sw_vers`, `mount` |
| Layer 2 (turn, render) | criterion, in process | same |
| Cache-hit replay | live provider, engine replay | same |
| Binary size | stripped release binary, `cargo-bloat` | same, per-platform numbers |

macOS publishes no PSS. Physical footprint is the metric Apple's own
tools report (`footprint`, Activity Monitor), so it is the honest
macOS memory number and the one a competitor comparison on macOS must
also use. Footprint counts more than PSS; the two never compare.

### macOS run conditions

The macOS receipt records what Linux's CPU governor line records on
Arch: the conditions that move numbers.

- Mains power, Low Power Mode off. The spec records both.
- Thermal state nominal at run start (`pmset -g therm`).
- No pinning: macOS assigns P-cores and E-cores. Benches run from a
  normal interactive session, so they inherit the default QoS.
- The binary is locally built, so Gatekeeper assessment does not sit
  in the timed span. A competitor binary must run once before its
  timed runs, so its assessment cost drops out too.

## Fixture rules

These already hold and become explicit requirements:

1. A run gets a private data home and HOME with the synthetic auth
   store. No login, no network.
2. The ready and prompt markers are the interface contract for probes
   on both platforms.
3. The PTY window is fixed at 120x40.
4. The probe measures its own spawn floor through the identical spawn
   path and reports it beside the samples.
5. Layer-2 benches drive recorded transcripts and a headless screen —
   no wall-clock I/O in the measured span.
6. Result files carry the machine spec reference, the git revision,
   and the dirty flag.

## Harness changes

1. **Platform split.** `harness.py` keeps the shared logic: markers,
   spawn, shutdown, stats, the report envelope, and the one
   result-name template. The native probes move to a platform module
   loaded at import: `platform_linux.py` and `platform_macos.py`,
   each exporting `resident_pages`, `evict_once`, `filesystem`,
   `machine_fields`, and the support flags (`SUPPORTS_EVICT`,
   `SUPPORTS_PROC_MEMORY`); the per-process memory probe joins at
   step 3. Callers reach the seam only through `harness` wrappers.
   The eager `ctypes.CDLL("libc.so.6")` load becomes lazy; today it
   breaks every import on macOS.
2. **Machine slug in result names.** Result files gain an OS-arch
   slug: `boot-<date>-<rev>-linux-x86_64.json`,
   `machine-<date>-macos-aarch64.json`. Existing files keep their
   names; they are all Linux.
3. **`machine.py` on macOS.** CPU via `sysctl machdep.cpu.brand_string`,
   memory via `hw.memsize`, OS via `sw_vers`, filesystem via `mount`,
   plus power source, Low Power Mode, and thermal state.
4. **`turn/run.sh`.** Replace `findmnt` with a portable filesystem
   probe.
5. **README.** Install notes per platform: `pacman -S hyperfine` and
   `brew install hyperfine`; Python 3.13 from Homebrew or python.org
   on macOS.

## Support matrix after this plan

| Bench | Linux | macOS |
|---|---|---|
| boot/run.sh | yes | yes |
| boot/pty_probe.py warm | yes | yes |
| boot/pty_probe.py --cold | yes | step 4 decides |
| memory/idle.py | yes (PSS, USS, anon) | yes (footprint) |
| memory/pss.py | yes | yes, renamed probe |
| turn/run.sh | yes | yes |
| render/run.sh | yes | yes |
| cache/replay.py | yes | yes |
| metrics.py | yes | yes |

Cold boot on macOS: `purge` flushes the whole disk cache and needs
root. Step 4 tries it, verifies the eviction with `mincore`, and
decides with a receipt. If `purge` cannot give a verified eviction,
macOS cold is marked unsupported and the warm number stands alone.

## Binary size: budget, not wall

The size posture, decided 2026-08-23:

- The stripped binary has a **budget of 25 MB** per platform. The
  M2.5 working budget of 15 MB stands unchanged.
- A budget is not a wall. Function and measured performance never get
  cut only to stay under a size number.
- Size spending needs receipts. A change that grows the binary more
  than 10% lands with a `cargo-bloat` receipt naming what the bytes
  buy. Crossing a budget needs the receipt plus a note in the active
  plan doc.
- The direction stays lightweight: prefer the smaller dependency at
  equal function, prefer lazy loading, and keep the M0 discipline of
  measuring before adding.
- Size receipts are per platform. Current points: 7.9 MB Linux
  x86_64, 6.7 MB macOS aarch64.

`docs/plan.md` and `docs/m2.5-plan.md` change to this wording where
they state size gates.

## Gates after this plan

| Metric | Linux gate | macOS gate | Bar (jcode, Linux) |
|---|---|---|---|
| Boot to interactive | < 50 ms | < 50 ms | 14 ms |
| Single session memory | PSS < 40 MB | footprint < 60 MB, provisional | 27.8 MB |
| Marginal session memory | PSS < 15 MB | footprint < 20 MB, provisional | ~9.9 MB |
| Stripped binary | 25 MB budget | 25 MB budget | — |

The macOS memory gates are provisional: footprint counts shared and
compressed pages differently from PSS. The first macOS receipt
calibrates them, and the calibrated numbers replace these in
`docs/plan.md` with a note.

## Build order

Each step is one PR.

1. **This plan.** Amends `docs/plan.md` (rules, targets, size
   posture) and `docs/m2.5-plan.md` (size wording) in the same PR.
   *Verify: review.*
2. **Harness platform split.** The platform modules, the lazy probe
   load, the result-name slug, `machine.py` and `turn/run.sh`
   portability, README install notes. *Verify: `machine.py`,
   `boot/run.sh`, `pty_probe.py` (warm), `turn/run.sh`,
   `render/run.sh`, and `metrics.py` produce receipts on both
   reference machines.*
3. **macOS memory probe.** `process_memory` via `proc_pid_rusage`;
   `memory/idle.py` reports footprint on macOS. *Verify: idle receipts
   from both machines; the macOS gate calibration lands in
   `docs/plan.md`.*
4. **macOS cold boot decision.** Try `purge` with `mincore`
   verification. *Verify: a receipt either way; the support matrix
   updated.*
5. **M2 close-out.** The alignment review and the metrics receipts,
   now from both machines. The close-out definition in `AGENTS.md`
   gains the second machine.

M2.5 starts after step 5. The M2.5 render-bench gates then read
per-machine: each machine compares against its own M2-era baseline,
and the first macOS baseline comes from step 2's receipts.

## Risks

- **Two machines drift apart.** A regression can hide on the machine
  that did not run. Mitigation: the close-out requires both receipts;
  a PR-level bench runs on the developer's machine and the trend line
  catches the other at close-out.
- **Footprint noise from memory compression.** macOS compresses idle
  pages, so footprint can move between samples. Mitigation: the idle
  driver samples over 10 launches and reports the spread; the gate
  calibration uses the observed spread.
- **`purge` needs root and flushes everything.** Mitigation: step 4
  decides with a receipt; warm-only is an acceptable outcome.
- **Toolchain skew.** Different rustc versions across machines blur a
  comparison. Mitigation: the machine spec records rustc; a close-out
  updates both machines to the same toolchain first.
