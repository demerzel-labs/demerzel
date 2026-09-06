# Benchmark Platforms Plan — Linux and macOS

Pull request numbers recorded before the 2026-09-04 migration refer to the
[original private archive](history.md#historical-pull-requests).

This plan extends the benchmark suite to Linux and macOS.
Warm boot and Layer-2 benchmarks produce receipts on both platforms.
The macOS memory probe and system-cache eviction are implemented.
Binary size has a budget that requires receipts for exceptions.

**Status (2026-09-06):** steps 1 and 2 merged (PR #29, PR #30).
The M2 close-out is complete, with macOS receipts in PR #32 and
Linux receipts in PR #33 on 2026-08-24.
Step 3 is complete, with idle receipts from both reference machines and macOS idle gate calibration.
Step 4 verifies macOS system-cache eviction and adds an explicit `--cold-system` mode.
The [decision receipt](#macos-cold-boot-decision) records the evidence and limits.
See the [next-session handoff](m2.5-plan.md#next-session-handoff) for the work order.

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
| Cold boot | Per-file `posix_fadvise` eviction, `mincore` verification | System-cache `purge` eviction, binary `mincore` verification |
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
| boot/pty_probe.py --cold | yes | no: no per-file eviction |
| boot/pty_probe.py --cold-system | no | yes: administrator authorization required |
| memory/idle.py | yes (PSS, USS, anon) | yes (footprint) |
| memory/memory.py | yes (PSS, USS) | yes (current and lifetime peak footprint) |
| turn/run.sh | yes | yes |
| render/run.sh | yes | yes |
| cache/replay.py | yes | yes |
| metrics.py | yes | yes |

### macOS cold-boot decision

The verified eviction support does not yet provide a practical unattended batch.
The first full batch requested administrator authorization before each purge and stopped without a complete latency receipt.
Do not repeat that batch until the authorization path changes.
See the [attempt record](m2.5-measurements.md#system-cold-status).

The 2026-09-06 experiment at `6e00cec` verified binary eviction after each of three system-cache purges.
The release binary contains 7,256,144 bytes across 443 pages of 16,384 bytes each.
Every attempt started with all 443 pages resident and ended with zero resident pages.
Each `purge` command returned success.

The experiment ran on battery power with Low Power Mode off and nominal thermal state.
It verifies eviction, not latency or a performance gate.
The timing receipts still require the [macOS run conditions](#macos-run-conditions).

- [Eviction receipt](../bench/results/macos-eviction-2026-09-06-6e00cec-macos-aarch64.json).
- [Experiment machine record](../bench/results/machine-eviction-2026-09-06-6e00cec-macos-aarch64.json).

Use `python3 bench/boot/pty_probe.py --cold-system` for macOS system-cold timing.
The mode requires an explicit flag because `purge` clears the system disk cache.
macOS authorizes only the fixed `/usr/sbin/purge` command.
The benchmark process stays unprivileged.
Each sample follows one purge and a zero-residency check for the binary.
A failed purge or residency check stops the run.
The harness does not retry a system purge within one sample.

The receipt records `mode: cold-system` and `eviction_scope: system`.
Shared libraries and filesystem metadata can also become cold.
Their residency is not measured.
This scope differs from Linux per-file eviction, and neither method represents a fresh operating-system boot.
The warm `printf` calibration does not measure the system-cold loader cost.
The probe does not subtract that calibration from any sample.
Keep the 50 ms target for warm startup.
Treat system-cold results as a separate baseline until its gate is set.

Repeat the eviction experiment with `python3 bench/boot/macos_eviction.py --administrator`.
Its machine record has a separate filename, so later timing runs cannot overwrite its conditions.

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
- Size receipts are per platform. M2 baseline sizes: 7.9 MB Linux
  x86_64, 6.7 MB macOS aarch64.

`docs/plan.md` and `docs/m2.5-plan.md` change to this wording where
they state size gates.

## Gates after this plan

| Metric | Linux gate | macOS gate | Bar (jcode, Linux) |
|---|---|---|---|
| Boot to interactive | < 50 ms | < 50 ms, warm | 14 ms |
| Single idle session memory | PSS < 40 MB | footprint < 10 MB | 27.8 MB |
| Marginal session memory | PSS < 15 MB | footprint < 20 MB, provisional | ~9.9 MB |
| Stripped binary | 25 MB budget | 25 MB budget | — |

The first macOS receipt calibrates the idle startup gate to 10 MB.
The marginal-session gate remains provisional until a concurrent-session scenario measures it.
See the [calibration record](#macos-memory-calibration).

## macOS memory calibration

The 2026-09-05 receipt measures 10 idle TUI launches at `33f631d`, with a clean tree.
The driver waits for the first prompt and settles for one second before each sample.
The machine uses AC power, with Low Power Mode off and nominal thermal state.

| Metric | Median | Range | Standard deviation |
|---|---|---|---|
| Current physical footprint | 2,112 KiB | 2,096–2,112 KiB | 7.729 KiB |
| Lifetime peak physical footprint | 2,112 KiB | 2,096–2,112 KiB | 7.729 KiB |

The median is 2.16 MB, using decimal MB.
Set the idle startup gate to less than 10 MB, replacing the provisional 60 MB gate.
This leaves more than four times the observed maximum for future startup work.
It does not set a limit for loaded transcripts or active model turns.
The provisional marginal-session gate stays at 20 MB; sequential launches cannot calibrate it.

This is the first macOS memory baseline. M2 has no macOS memory receipt.
M2.5 step 9 delays HTTP client construction until the first model turn.
The new idle baseline excludes that client and does not establish an M2 memory trend.
Linux PSS and macOS footprint do not compare.

The probe reads `ri_phys_footprint` and `ri_lifetime_max_phys_footprint` through `proc_pid_rusage`, using `RUSAGE_INFO_V4`.
The full native structure prevents writes beyond the Python buffer.
Tests compare its size, alignment, and field offsets against the installed SDK.
The [Apple structure definition](https://github.com/apple-oss-distributions/xnu/blob/main/bsd/sys/resource.h) defines the fields.
The [Apple kernel implementation](https://github.com/apple-oss-distributions/xnu/blob/main/bsd/kern/kern_resource.c) supplies their values.
The PID command reads processes in sequence. It does not add lifetime peaks into a group peak.

Receipts:

- [Idle memory, macOS](../bench/results/memory-idle-2026-09-05-33f631d-macos-aarch64.json).
- [Machine specification, macOS](../bench/results/machine-2026-09-05-macos-aarch64.json).

## Linux idle memory comparison

The 2026-09-05 receipt measures 10 idle TUI launches at `8ef6851`, with a clean tree.
Each launch uses the synthetic login fixture and a 120x40 PTY.
The driver waits for the first prompt and settles for one second before each sample.
The reference machine uses the `performance` CPU governor.

The receipt uses these commands, in order:

1. `cargo build --release --locked`
2. `python3 bench/machine.py`
3. `python3 bench/memory/idle.py`

The comparison uses the previous Linux receipt from 2026-08-24 at `bd667b4`.
Memory values are in KiB. PSS means proportional set size. USS means unique set size.

| Metric | Previous median | Current median | Change | Current range | Current standard deviation |
|---|---|---|---|---|---|
| Anonymous memory | 1,004 | 1,080 | +7.6% | 1,076–1,084 | 2.271 |
| PSS | 4,921.5 | 4,798 | -2.5% | 4,698–4,998 | 83.342 |
| USS | 4,904 | 4,784 | -2.4% | 4,684–4,984 | 83.331 |

The PSS median is 4.91 decimal MB, below the 40 MB Linux idle gate.
These medians remain within the M2.5 10% band against the previous Linux receipt.
The Linux gate stays unchanged.

The CPU, governor, filesystem, and Rust compiler match the previous machine specification.
The kernel changed from `7.1.8-arch1-3` to `7.2.3-arch1-2`.
Step 9 also moved HTTP client construction from startup to the first model turn.
The new receipt excludes that client; the previous receipt includes it.
PSS and USS also vary with the page-cache state of mapped files.
These differences prevent a direct claim of improved memory use for the same workload.
This idle receipt does not measure loaded transcripts, active turns, or concurrent sessions.
The full measurement pass and milestone close-out remain open.

Receipts:

- [Idle memory, Linux](../bench/results/memory-idle-2026-09-05-8ef6851-linux-x86_64.json).
- [Machine specification, Linux](../bench/results/machine-2026-09-05-linux-x86_64.json).
- [Previous idle memory, Linux](../bench/results/memory-idle-2026-08-24-bd667b4-linux-x86_64.json).
- [Previous machine specification, Linux](../bench/results/machine-2026-08-24-linux-x86_64.json).

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
   `memory/idle.py` reports footprint on macOS. Runs alongside M2.5;
   must land before the M2.5 measurement pass. *Verify: idle receipts
   from both machines; the macOS gate calibration lands in
   `docs/plan.md`.* The implementation, both reference receipts, and macOS idle gate calibration
   are complete. See the [Linux comparison](#linux-idle-memory-comparison).
4. **macOS cold boot decision.** Try `purge` with `mincore`
   verification. Runs alongside M2.5; must land before the M2.5
   measurement pass. *Verify: a receipt either way; the support
   matrix updated.* Complete: the [decision](#macos-cold-boot-decision) verifies system-cache eviction.
   Per-file cold mode remains unavailable on macOS.
5. **M2 close-out.** The alignment review and the metrics receipts.
   The close-out definition in `AGENTS.md` gains the second machine.
   Done 2026-08-23, with macOS receipts in PR #32.
   The Linux receipts landed on 2026-08-24 in PR #33.

M2.5 starts after step 5. The M2.5 render-bench gates read
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
