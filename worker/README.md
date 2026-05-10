# whisper-stt worker

Python subprocess that runs `faster-whisper` for the `whisperd` daemon.

Wire protocol: see [`../doc/plan/02-protocol.md`](../doc/plan/02-protocol.md).

## Setup

```bash
bash scripts/setup-venv.sh
# or, equivalently:
python3 -m venv worker/.venv
worker/.venv/bin/pip install -e worker
```

The first run of the worker downloads the model
(`distil-large-v3`, an alias faster-whisper resolves to the
CTranslate2-converted repo `Systran/faster-distil-whisper-large-v3`,
~1.5 GB) to `~/.cache/huggingface/`.

## End-to-end smoke test

```bash
# 1. record a 3-second sample (one-time):
arecord -f S16_LE -c 1 -r 16000 -d 3 worker/tests/fixtures/hello.wav

# 2. drive the worker:
worker/.venv/bin/python worker/tests/smoke.py worker/tests/fixtures/hello.wav
```

Expected output:

```
worker ready: {'event': 'ready', 'model': 'distil-large-v3', ...}
============================================================
  transcript: 'hello world this is a test'
  language  : en
  duration  : 312 ms
============================================================
```

While the smoke is running, `nvidia-smi` should show the daemon process
holding ~2 GiB of VRAM. After the worker exits, VRAM returns to 0.
