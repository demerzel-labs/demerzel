"""Structured JSONL trace writer + run manifest.

The trace is the stable, pipeline-consumed record of a run (PLAN.md sec 8). Every
event is one flushed JSON line: `{"ts", "run_id", "event", ...}`. Writing never kills
a run — IO errors degrade to a single stderr warning and are then swallowed.

The manifest (`run_manifest.json`) is the finalize artifact (sec 8.1): a single file
describing the run and every artifact it produced (path, bytes, sha256), so the bash
S3 step has one authoritative, integrity-checked thing to upload — even on a crash.
"""

from __future__ import annotations

import hashlib
import json
import os
import sys
import time
import uuid
from dataclasses import dataclass, field
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


def _now_iso() -> str:
    return datetime.now(timezone.utc).isoformat()


class TraceWriter:
    """Append-only JSONL trace. Resilient: a broken sink warns once then no-ops."""

    def __init__(self, path: str | os.PathLike | None, run_id: str | None = None):
        self.run_id = run_id or uuid.uuid4().hex
        self.path = Path(path) if path else None
        self._fh = None
        self._broken = False
        self._warned = False
        if self.path is not None:
            try:
                self.path.parent.mkdir(parents=True, exist_ok=True)
                self._fh = self.path.open("a", encoding="utf-8")
            except OSError as e:
                self._degrade(e)

    def _degrade(self, err: Exception) -> None:
        self._broken = True
        if not self._warned:
            print(f"[harness] WARN trace unavailable: {err}", file=sys.stderr)
            self._warned = True

    def emit(self, event: str, **fields: Any) -> None:
        """Write one event line. `event` is the type; fields are its payload."""
        if self._broken or self._fh is None:
            return
        record = {"ts": _now_iso(), "run_id": self.run_id, "event": event}
        record.update(fields)
        try:
            # json.dumps raises ValueError on a circular payload and TypeError on an
            # unserializable one; catch both so a bad field can never kill a run.
            self._fh.write(json.dumps(record, default=str) + "\n")
            self._fh.flush()
        except (OSError, TypeError, ValueError) as e:
            self._degrade(e)

    def close(self) -> None:
        if self._fh is not None:
            try:
                self._fh.close()
            except OSError:
                pass
            self._fh = None


def sha256_file(path: str | os.PathLike) -> str | None:
    """Hex sha256 of a file, or None if unreadable."""
    try:
        h = hashlib.sha256()
        with open(path, "rb") as f:
            for chunk in iter(lambda: f.read(65536), b""):
                h.update(chunk)
        return h.hexdigest()
    except OSError:
        return None


def sha256_text(text: str) -> str:
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


@dataclass
class Artifact:
    role: str
    path: str

    def describe(self) -> dict:
        p = Path(self.path)
        # Fully defensive: a stat/hash failure here must never escape finalize (it would
        # skip the manifest entirely). Treat any IO error as "not present".
        try:
            size = p.stat().st_size
            exists = True
        except OSError:
            size, exists = 0, False
        return {
            "role": self.role,
            "path": str(p),
            "bytes": size,
            "sha256": sha256_file(p) if exists else None,
            "exists": exists,
        }


@dataclass
class RunManifest:
    """Accumulates run facts + artifact list, then writes run_manifest.json.

    `finalize` is crash-safe: it writes whatever it has, so a run that blew up is
    still described and uploadable.
    """

    run_id: str
    schema_version: int = 1
    started_at: str = field(default_factory=_now_iso)
    fields: dict = field(default_factory=dict)
    _artifacts: list[Artifact] = field(default_factory=list)
    _t0: float = field(default_factory=time.monotonic)

    def set(self, **fields: Any) -> None:
        self.fields.update(fields)

    def add_artifact(self, role: str, path: str | os.PathLike | None) -> None:
        if path:
            self._artifacts.append(Artifact(role=role, path=str(path)))

    def to_dict(self, outcome: str) -> dict:
        return {
            "run_id": self.run_id,
            "schema_version": self.schema_version,
            "outcome": outcome,
            "started_at": self.started_at,
            "ended_at": _now_iso(),
            "total_s": round(time.monotonic() - self._t0, 3),
            **self.fields,
            "artifacts": [a.describe() for a in self._artifacts],
        }

    def write(self, path: str | os.PathLike, outcome: str) -> dict:
        """Write the manifest JSON. Returns the dict written (also on IO failure,
        where it just warns)."""
        data = self.to_dict(outcome)
        try:
            p = Path(path)
            p.parent.mkdir(parents=True, exist_ok=True)
            p.write_text(json.dumps(data, indent=2, default=str), encoding="utf-8")
        except OSError as e:
            print(f"[harness] WARN manifest write failed: {e}", file=sys.stderr)
        return data
