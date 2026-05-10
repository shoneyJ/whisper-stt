"""End-to-end smoke test for whisper_worker.py.

Drives the worker through stdin/stdout with a real WAV file. No pytest;
this is a one-shot script you run after `bash scripts/setup-venv.sh`.

    worker/.venv/bin/python worker/tests/smoke.py worker/tests/fixtures/hello.wav

If you do not have a fixture, record one (~3 s of speech):

    arecord -f S16_LE -c 1 -r 16000 -d 3 worker/tests/fixtures/hello.wav
"""

from __future__ import annotations

import base64
import json
import subprocess
import sys
import wave
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]
WORKER = REPO_ROOT / "worker" / "whisper_worker.py"
VENV_PYTHON = REPO_ROOT / "worker" / ".venv" / "bin" / "python"


def load_pcm(wav_path: Path) -> tuple[bytes, int]:
    with wave.open(str(wav_path), "rb") as wf:
        if wf.getnchannels() != 1:
            raise SystemExit(f"expected mono WAV, got {wf.getnchannels()} channels")
        if wf.getsampwidth() != 2:
            raise SystemExit(f"expected 16-bit WAV, got {wf.getsampwidth() * 8}-bit")
        sr = wf.getframerate()
        if sr != 16000:
            raise SystemExit(f"expected 16 kHz WAV, got {sr} Hz (resample with sox or ffmpeg)")
        return wf.readframes(wf.getnframes()), sr


def main(argv: list[str]) -> int:
    if len(argv) != 2:
        print(__doc__, file=sys.stderr)
        return 2
    wav_path = Path(argv[1])
    if not wav_path.exists():
        print(f"WAV not found: {wav_path}", file=sys.stderr)
        print("Record one with:", file=sys.stderr)
        print(f"    arecord -f S16_LE -c 1 -r 16000 -d 3 {wav_path}", file=sys.stderr)
        return 1

    if not VENV_PYTHON.exists():
        print(f"venv python not found at {VENV_PYTHON}", file=sys.stderr)
        print("Provision it with: bash scripts/setup-venv.sh", file=sys.stderr)
        return 1

    pcm, sr = load_pcm(wav_path)
    pcm_b64 = base64.b64encode(pcm).decode("ascii")

    proc = subprocess.Popen(
        [str(VENV_PYTHON), str(WORKER)],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=sys.stderr,
        text=True,
        bufsize=1,
    )
    assert proc.stdin and proc.stdout

    def send(obj: dict) -> None:
        assert proc.stdin
        proc.stdin.write(json.dumps(obj) + "\n")
        proc.stdin.flush()

    def recv() -> dict:
        assert proc.stdout
        line = proc.stdout.readline()
        if not line:
            raise SystemExit("worker exited unexpectedly; see stderr above")
        return json.loads(line)

    send({
        "cmd": "init",
        "model": "distil-large-v3",
        "device": "cuda",
        "compute_type": "float16",
        "language": "en",
        "beam_size": 5,
    })
    ready = recv()
    if ready.get("event") != "ready":
        print(f"init failed: {ready}", file=sys.stderr)
        proc.kill()
        return 1
    print(f"worker ready: {ready}")

    send({"cmd": "transcribe", "pcm_b64": pcm_b64, "sr": sr})
    transcript = recv()
    if transcript.get("event") != "transcript":
        print(f"transcribe failed: {transcript}", file=sys.stderr)
        proc.kill()
        return 1

    print()
    print("=" * 60)
    print(f"  transcript: {transcript['text']!r}")
    print(f"  language  : {transcript.get('language')}")
    print(f"  duration  : {transcript.get('duration_ms')} ms")
    print("=" * 60)

    send({"cmd": "shutdown"})
    proc.wait(timeout=10)
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
