"""Trace writer + manifest: event shape, resilience, crash-safe manifest."""

import json

from prototype.trace import RunManifest, TraceWriter, sha256_file, sha256_text


def _read_jsonl(path):
    return [json.loads(line) for line in path.read_text().splitlines() if line.strip()]


def test_trace_emits_jsonl(tmp_path):
    p = tmp_path / "trace.jsonl"
    tw = TraceWriter(p, run_id="run123")
    tw.emit("run_start", repo="acme/core", diff_size=100)
    tw.emit("model_decision", model_id="x", source="default:review")
    tw.close()

    rows = _read_jsonl(p)
    assert [r["event"] for r in rows] == ["run_start", "model_decision"]
    assert all(r["run_id"] == "run123" for r in rows)
    assert all("ts" in r for r in rows)
    assert rows[0]["repo"] == "acme/core"


def test_trace_none_path_is_noop():
    tw = TraceWriter(None)
    tw.emit("run_start")  # must not raise
    tw.close()


def test_trace_broken_sink_degrades(tmp_path, capsys):
    # Point at a path whose parent is a file -> mkdir/open fails.
    blocker = tmp_path / "afile"
    blocker.write_text("x")
    tw = TraceWriter(blocker / "trace.jsonl")
    tw.emit("run_start")  # must not raise
    tw.close()
    assert "trace unavailable" in capsys.readouterr().err


def test_sha_helpers(tmp_path):
    f = tmp_path / "a.txt"
    f.write_text("hello")
    assert sha256_file(f) == sha256_text("hello")
    assert sha256_file(tmp_path / "missing") is None


def test_manifest_describes_artifacts(tmp_path):
    review = tmp_path / "review.md"
    review.write_text("# Review\nlooks good")
    man = RunManifest(run_id="r1")
    man.set(repo="acme/core", task="review", cost_usd=0.12, model_id="sonnet")
    man.add_artifact("review", review)
    man.add_artifact("trace", tmp_path / "trace.jsonl")  # doesn't exist yet

    out = tmp_path / "run_manifest.json"
    data = man.write(out, outcome="ok")

    assert out.exists()
    on_disk = json.loads(out.read_text())
    assert on_disk == data
    assert on_disk["outcome"] == "ok"
    assert on_disk["repo"] == "acme/core"
    roles = {a["role"]: a for a in on_disk["artifacts"]}
    assert roles["review"]["exists"] is True
    assert roles["review"]["sha256"] == sha256_text("# Review\nlooks good")
    assert roles["review"]["bytes"] > 0
    assert roles["trace"]["exists"] is False
    assert roles["trace"]["sha256"] is None


def test_manifest_write_is_crash_safe_with_partial_data(tmp_path):
    # Simulate the fatal path: minimal fields, error outcome — still writes.
    man = RunManifest(run_id="r2")
    man.set(repo="acme/core")
    out = tmp_path / "run_manifest.json"
    data = man.write(out, outcome="error")
    assert data["outcome"] == "error"
    assert out.exists()
    assert "ended_at" in data and "total_s" in data
