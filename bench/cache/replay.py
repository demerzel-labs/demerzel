#!/usr/bin/env python3
"""Prompt-cache hit rate on a replayed prompt script: Layer 4.

The script replays the user side of one session — a fixed list of
user turns — through the harness against one live provider, as two
fresh sessions one after the other. The model side is live, so the
two sessions differ in what the model reads and how many calls it
makes. The report gives `cache_read_tokens / input_tokens` per model
call, per turn, and per session. The turns read this repository and
nothing else.

The Rust side (`crates/demerzel/examples/cache_replay.rs`) drives
the real engine over the real adapter and prints the per-call usage;
this script builds it, runs it, and writes the report with the
provenance envelope. The engine advertises the full tool set in
`ask` mode with no approver: the prefix a real session sends, with
only the read tools able to run. Sessions land in a private temporary
data root that goes away with the run. Credentials resolve the way a
session resolves them: the environment (`ANTHROPIC_API_KEY`), then
the default auth store (`dmz login`).

What the numbers mean:
- `input_tokens` counts the whole prompt, cached parts included, for
  both providers. `cache_read_tokens` is the part served from the
  cache; `cache_creation_tokens` the part written into it (the Codex
  endpoint reports no write count).
- The comparable numbers across milestones are the per-call ones: a
  warm call that reads its predecessor's whole prompt shows a stable
  prefix. Session totals move with the model's tool choices.
- `first_call_hit_rate` is the first call of each session. In the
  second session it shares its whole prefix with the first session's
  first call, so a non-zero value there shows the prefix holds across
  sessions. A provider caches only prompts past a minimum length, so
  a zero can also mean the static prefix is still too short; the
  per-call column tells the two apart.
- `steady_state_hit_rate` is the ratio over every call after the
  first of each session.

A live run costs tokens and needs network access.
"""

import argparse
import json
import os
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import harness

REPO_ROOT = harness.REPO_ROOT
TARGET = os.environ.get("CARGO_TARGET_DIR") or os.path.join(REPO_ROOT, "target")
EXAMPLE = os.path.join(TARGET, "release", "examples", "cache_replay")

# The user side of the session: one read-only walk through this
# repository. Fixed across milestones.
PROMPTS = [
    "Read Cargo.toml at the repository root and list the workspace members, one per line.",
    "Read crates/demerzel-core/src/provider.rs. Name each method of the Provider trait "
    "and its return type.",
    "Use grep to find where the session file's record types are defined, then read that "
    "enum and list its variants.",
    "Read crates/demerzel-core/src/engine.rs. In three sentences, say what happens when "
    "the user aborts a turn at an approval prompt.",
    "Which crate owns the full-screen interface? Read its lib.rs and name the public "
    "items it exports.",
    "Without reading any more files, summarize what you learned about this repository "
    "in five bullets.",
]


def ratio(cached: int, total: int) -> float | None:
    return round(cached / total, 4) if total else None


def summarize(trace: dict) -> tuple[list[dict], dict]:
    """Per-session turn tables and the headline summary."""
    sessions = []
    for session in trace["sessions"]:
        turns = []
        s_input = s_cached = s_created = s_output = 0
        for turn in session["turns"]:
            calls = [
                {
                    **call,
                    "hit_rate": ratio(call["cache_read_tokens"], call["input_tokens"]),
                }
                for call in turn["calls"]
            ]
            t_input = sum(c["input_tokens"] for c in calls)
            t_cached = sum(c["cache_read_tokens"] for c in calls)
            t_created = sum(c.get("cache_creation_tokens", 0) for c in calls)
            t_output = sum(c["output_tokens"] for c in calls)
            s_input += t_input
            s_cached += t_cached
            s_created += t_created
            s_output += t_output
            turns.append(
                {
                    "prompt": turn["prompt"],
                    "outcome": turn["outcome"],
                    "wall_ms": turn["wall_ms"],
                    "tools": turn["tools"],
                    "model_calls": len(calls),
                    "input_tokens": t_input,
                    "cache_read_tokens": t_cached,
                    "cache_creation_tokens": t_created,
                    "output_tokens": t_output,
                    "hit_rate": ratio(t_cached, t_input),
                    "calls": calls,
                }
            )
        all_calls = [c for t in turns for c in t["calls"]]
        first = all_calls[0] if all_calls else None
        rest = all_calls[1:]
        rest_input = sum(c["input_tokens"] for c in rest)
        rest_cached = sum(c["cache_read_tokens"] for c in rest)
        sessions.append(
            {
                "session": session["session"],
                "model_calls": len(all_calls),
                "input_tokens": s_input,
                "cache_read_tokens": s_cached,
                "cache_creation_tokens": s_created,
                "output_tokens": s_output,
                "hit_rate": ratio(s_cached, s_input),
                "first_call_hit_rate": first["hit_rate"] if first else None,
                "steady_state_hit_rate": ratio(rest_cached, rest_input),
                "turns": turns,
            }
        )
    summary = {
        "session_hit_rates": [s["hit_rate"] for s in sessions],
        "first_call_hit_rates": [s["first_call_hit_rate"] for s in sessions],
        "steady_state_hit_rates": [s["steady_state_hit_rate"] for s in sessions],
        "turn_hit_rates": [[t["hit_rate"] for t in s["turns"]] for s in sessions],
        "outcomes": sorted({t["outcome"] for s in sessions for t in s["turns"]}),
    }
    return sessions, summary


def main() -> int:
    parser = argparse.ArgumentParser(description=(__doc__ or "").strip().splitlines()[0])
    parser.add_argument("--provider", required=True, help="openai-codex or anthropic")
    parser.add_argument("--model", help="model name (default: the provider's default)")
    parser.add_argument("--runs", type=int, default=2, help="fresh sessions to run (default: 2)")
    parser.add_argument("--out", help="output JSON path (default: under bench/results)")
    args = parser.parse_args()

    build = subprocess.run(
        ["cargo", "build", "--release", "-p", "demerzel", "--example", "cache_replay"],
        cwd=REPO_ROOT,
    )
    if build.returncode != 0:
        return build.returncode

    command = [EXAMPLE, "--provider", args.provider, "--runs", str(args.runs)]
    if args.model:
        command += ["--model", args.model]
    command += ["--", *PROMPTS]
    # Progress streams on stderr while the run spends tokens; only
    # the JSON trace on stdout is captured.
    run = subprocess.run(command, cwd=REPO_ROOT, stdout=subprocess.PIPE, text=True)
    if run.returncode != 0:
        print(f"cache_replay exited {run.returncode}", file=sys.stderr)
        return 1
    try:
        trace = json.loads(run.stdout)
    except json.JSONDecodeError as error:
        print(f"cache_replay printed no JSON trace: {error}", file=sys.stderr)
        return 1
    sessions, summary = summarize(trace)

    report = harness.base_report("prompt-cache-hit-rate", EXAMPLE)
    report.update(
        {
            "provider": trace["provider"],
            "model": trace["model"],
            "runs": trace["runs"],
            "definition": (
                "hit_rate = cache_read_tokens / input_tokens; input_tokens counts the "
                "cached parts for both providers. first_call is the first model call "
                "of the session: in the second session it can read the first "
                "session's prefix, but only once the prefix passes the provider's "
                "minimum cacheable length. steady_state is every call after the first."
            ),
            "project": "this repository; full tool set in ask mode, read tools run",
            "prompts": PROMPTS,
            "summary": summary,
            "sessions": sessions,
        }
    )
    out_path = args.out or os.path.join(
        REPO_ROOT,
        "bench/results",
        f"cache-hit-{trace['provider']}-{report['date']}-{report['git_rev']}-{report['os']}.json",
    )
    harness.write_report(out_path, report)
    for index, session in enumerate(sessions, 1):
        print(
            f"session {index}: hit rate {session['hit_rate']}, first call "
            f"{session['first_call_hit_rate']}, steady state "
            f"{session['steady_state_hit_rate']}, {session['model_calls']} calls, "
            f"{session['input_tokens']} in, {session['cache_read_tokens']} cached"
        )
    print(f"raw data: {os.path.relpath(out_path, REPO_ROOT)}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
