# 04 — Worker

How `whisperd` manages the Python `whisper_worker.py` child process.

The worker stdio protocol itself is in [`02-protocol.md`](02-protocol.md). This
doc covers everything *around* the protocol on the daemon side: paths,
configuration knobs, error handling, idle lifecycle.

---

## Configuration

All knobs are surfaced via env vars, with sensible defaults derived from the
current working directory. Code reference:
[`crates/whisperd/src/worker.rs`](../../crates/whisperd/src/worker.rs)
(`WorkerConfig::from_env`).

| Env var                    | Default                            | Meaning                                                           |
| -------------------------- | ---------------------------------- | ----------------------------------------------------------------- |
| `WHISPER_STT_PYTHON`       | `./worker/.venv/bin/python`        | Interpreter the daemon spawns. Must be the venv from `setup-venv.sh`. |
| `WHISPER_STT_WORKER_SCRIPT`| `./worker/whisper_worker.py`       | Worker entry point.                                               |
| `WHISPER_STT_MODEL`        | `distil-large-v3`                  | faster-whisper alias or CT2 HF repo id.                           |
| `WHISPER_STT_DEVICE`       | `cuda`                             | Pass `cpu` to fall back without GPU.                              |
| `WHISPER_STT_COMPUTE_TYPE` | `float16`                          | CTranslate2 compute type. `int8`, `int8_float16`, `float32` also work. |
| `WHISPER_STT_LANGUAGE`     | `en`                               | ISO 639-1 code. Set empty/unset to auto-detect.                   |
| `WHISPER_STT_BEAM_SIZE`    | (worker default: 5)                | Higher = more accurate, slower. 1 for greedy.                     |

The defaults assume the daemon is launched from the repo root. When installed
to `~/.local/bin/whisperd`, the user must export `WHISPER_STT_PYTHON` and
`WHISPER_STT_WORKER_SCRIPT` (or `whisperd` should be wrapped in a launcher
script — see [`05-build-install.md`](05-build-install.md), once it lands).

---

## Lifecycle

### Spawn

`WorkerHandle::spawn(&config)` does:

1. Validates `python` and `script` paths exist.
2. `tokio::process::Command::spawn` with `kill_on_drop(true)` so a panicked
   daemon does not orphan the worker.
3. Sends `{"cmd":"init", ...}` on stdin.
4. Waits for `{"event":"ready", ...}` (or `error`) on stdout. The `ready`
   event echoes `model`/`device`/`compute_type` so the daemon can log what
   the worker actually loaded (faster-whisper may resolve the alias to a
   different repo than expected).

### Transcribe

`WorkerHandle::transcribe(pcm_b64, sr)`:

1. Sends `{"cmd":"transcribe", ...}`.
2. Reads exactly one event:
   - `transcript` → returns `TranscribeResult { text, language, duration_ms }`.
   - `error { recoverable: true }` → bubbles up as `Result::Err`; **the worker
     stays alive**, the daemon may retry or wait for the next user request.
   - `error { recoverable: false }` → fatal; daemon should drop the handle.

### Shutdown

`WorkerHandle::shutdown()`:

1. Sends `{"cmd":"shutdown"}` (best-effort — even if the worker died, we
   continue).
2. Waits up to 5 s for the child to exit.
3. Force-kills (`SIGKILL`) and reaps if the timeout elapses.

`shutdown` is also reached implicitly via `kill_on_drop` if the daemon is
killed without graceful tear-down; in that case CUDA may not release VRAM
immediately, but the OS will reclaim it within seconds.

### Per-press call count (streaming-mode change)

In v1 the daemon called `transcribe` exactly once per F9 press — on
release, with the full utterance. With the streaming segmenter (v2,
`crates/whisperd/src/segmenter.rs`), the daemon now calls `transcribe`
**once per detected speech segment**, plus once more for the tail on
release:

```
F9 down → wait first sample → listening
  user says "hello world."   ← pause detected → transcribe(segment) → inject
  user says "this is a test."← pause detected → transcribe(segment) → inject
F9 up                        ← tail flush     → transcribe(tail)    → inject
```

The worker doesn't know or care about this — each call is the same
`{"cmd":"transcribe", ...}` against a self-contained PCM blob. The model
stays warm for the whole press; only the first call after a 5-minute
idle pays the cold-start.

Practical implication: a long monologue press generates many short
inferences (~500 ms each on `distil-large-v3` at this rig) instead of
one long one, which actually keeps perceived latency lower at the cost
of slightly higher per-press total compute (Whisper sees less context
per call).

---

## Idle reaper *(introduced in M4)*

The dev `--transcribe` CLI in M2 spawns and shuts down the worker per
invocation, so no reaper runs there. In M4 the daemon owns a long-running
`WorkerManager` that wraps `Option<WorkerHandle>` plus a `last_use: Instant`.

Sketch:

```text
loop {
    select! {
        cmd = ipc.recv()       => handle(cmd)        // updates last_use
        _   = sleep(check_iv)  => {                  // every 30 s
            if worker.is_some() && last_use.elapsed() > IDLE_TIMEOUT {
                worker.take().shutdown().await;     // VRAM freed
            }
        }
    }
}
```

`IDLE_TIMEOUT` defaults to 300 s. Override with `WHISPER_STT_IDLE_TIMEOUT_SECS`.

Re-spawn is lazy: the next `start`/`transcribe` from `whisperctl` will see
`worker.is_none()` and call `WorkerHandle::spawn` again. First-press latency
after an idle reap is the cold-start cost (~2–4 s on this GPU for
`distil-large-v3`, mostly model load from the HF cache, not download).

---

## Failure modes & operator notes

| Symptom                                      | Likely cause                                              | Mitigation                                                              |
| -------------------------------------------- | --------------------------------------------------------- | ----------------------------------------------------------------------- |
| `worker python not found`                    | `worker/.venv/` missing                                   | `bash scripts/setup-venv.sh`                                            |
| `worker init failed: ... CUDA ...`           | CUDA / driver mismatch                                    | Set `WHISPER_STT_DEVICE=cpu WHISPER_STT_COMPUTE_TYPE=int8` to verify rest of pipeline; investigate driver separately. |
| `worker stdout closed unexpectedly`          | Worker crashed; check daemon stderr for the Python traceback | Re-running typically re-spawns cleanly; persistent crash → file an issue with the traceback. |
| First `transcribe` blocks for ~30 s          | First-run model download                                  | Pre-pull with `worker/.venv/bin/hf download Systran/faster-distil-whisper-large-v3` so the daemon never blocks on network on first use. |
| `Thank you.` transcribed for everything      | Whisper hallucination on silence/very-quiet audio         | Verify mic with `aplay <wav>`; pick a different `arecord -D` device; speak closer to the mic. |
| `nvidia-smi` still shows VRAM after shutdown | Driver release lag (typical on consumer GPUs)             | Allow ~5 s; if it persists across re-spawns, file an issue.             |

---

## Code reference

- Worker handle + config: [`crates/whisperd/src/worker.rs`](../../crates/whisperd/src/worker.rs)
- Wire types (shared with `whisperctl`): [`crates/whisper-stt-proto/src/lib.rs`](../../crates/whisper-stt-proto/src/lib.rs)
- Dev CLI used in M2 verification: `whisperd --transcribe <wav>` (will be removed when the IPC server lands in M4).
