"""faster-whisper JSON-RPC stdio worker.

Wire protocol: see ../doc/plan/02-protocol.md.

stdin   : NDJSON commands (one per line)
stdout  : NDJSON events  (one per line)
stderr  : free-form log; the daemon forwards this to its own log
"""

from __future__ import annotations

import base64
import json
import sys
import time
from typing import Any

import numpy as np

# Imported lazily inside InitState.run so that a misconfigured CUDA does not
# blow up before we have a chance to emit a structured "error" event.
WhisperModel = None  # type: ignore[assignment]


def emit(obj: dict[str, Any]) -> None:
    sys.stdout.write(json.dumps(obj, ensure_ascii=False) + "\n")
    sys.stdout.flush()


def log(msg: str) -> None:
    sys.stderr.write(f"[worker] {msg}\n")
    sys.stderr.flush()


def decode_pcm(pcm_b64: str) -> np.ndarray:
    raw = base64.b64decode(pcm_b64)
    return np.frombuffer(raw, dtype="<i2").astype(np.float32) / 32768.0


class Worker:
    def __init__(self) -> None:
        self.model: Any = None
        self.language: str | None = None
        self.beam_size: int = 5
        self.model_name: str = ""
        self.device: str = ""
        self.compute_type: str = ""

    def handle_init(self, cmd: dict[str, Any]) -> None:
        global WhisperModel
        if WhisperModel is None:
            from faster_whisper import WhisperModel as _WM
            WhisperModel = _WM

        self.model_name = cmd["model"]
        self.device = cmd["device"]
        self.compute_type = cmd["compute_type"]
        self.language = cmd.get("language")
        self.beam_size = int(cmd.get("beam_size", 5))

        log(f"loading model {self.model_name} on {self.device} ({self.compute_type})")
        self.model = WhisperModel(
            self.model_name,
            device=self.device,
            compute_type=self.compute_type,
        )
        emit({
            "event": "ready",
            "model": self.model_name,
            "device": self.device,
            "compute_type": self.compute_type,
        })

    def handle_transcribe(self, cmd: dict[str, Any]) -> None:
        if self.model is None:
            emit({"event": "error", "msg": "transcribe before init", "recoverable": True})
            return

        sr = int(cmd["sr"])
        if sr != 16000:
            emit({
                "event": "error",
                "msg": f"unsupported sample rate {sr}; v1 requires 16000",
                "recoverable": True,
            })
            return

        audio = decode_pcm(cmd["pcm_b64"])
        if audio.size == 0:
            emit({"event": "transcript", "text": "", "language": self.language or "", "duration_ms": 0})
            return

        t0 = time.monotonic()
        segments, info = self.model.transcribe(
            audio,
            language=self.language,
            beam_size=self.beam_size,
        )
        text = " ".join(seg.text.strip() for seg in segments).strip()
        elapsed_ms = int((time.monotonic() - t0) * 1000)

        emit({
            "event": "transcript",
            "text": text,
            "language": getattr(info, "language", self.language or ""),
            "duration_ms": elapsed_ms,
        })

    def handle_shutdown(self) -> None:
        # Drop the model reference so CUDA can release VRAM before we exit.
        self.model = None


def main() -> int:
    worker = Worker()

    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            cmd = json.loads(line)
        except json.JSONDecodeError as exc:
            emit({"event": "error", "msg": f"invalid json: {exc}", "recoverable": True})
            continue

        verb = cmd.get("cmd")
        try:
            if verb == "init":
                worker.handle_init(cmd)
            elif verb == "transcribe":
                worker.handle_transcribe(cmd)
            elif verb == "shutdown":
                worker.handle_shutdown()
                return 0
            else:
                emit({"event": "error", "msg": f"unknown cmd: {verb!r}", "recoverable": True})
        except Exception as exc:  # noqa: BLE001 — we surface every failure to the host
            recoverable = verb != "init"
            log(f"exception handling {verb}: {exc!r}")
            emit({"event": "error", "msg": f"{type(exc).__name__}: {exc}", "recoverable": recoverable})
            if not recoverable:
                return 1

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
