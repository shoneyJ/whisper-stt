# 02 — Protocol

This doc defines the two wire protocols used by `whisper-stt`:

1. **Worker stdio protocol** — between `whisperd` (Rust daemon) and `whisper_worker.py` (Python subprocess). Defined now; the worker is implemented against this in M1.
2. **CLI ↔ daemon protocol** — between `whisperctl` (Rust CLI) and `whisperd` (Rust daemon) over a Unix socket. *Spec stub; finalised in M4.*

Both protocols use **newline-delimited JSON (NDJSON)**: one JSON object per line, UTF-8, no embedded newlines inside the object.

---

## 1. Worker stdio protocol (`whisperd` ↔ `whisper_worker.py`)

The worker is a single long-running Python subprocess. The daemon spawns it once, sends commands on stdin, and reads events from stdout. **Stderr is free-form log output** and is forwarded to the daemon's log; the daemon never parses it.

### Lifecycle

```
host                            worker
  │   spawn                       │
  │ ─────────────────────────────►│   (python starts; no event yet)
  │                               │
  │   {"cmd":"init", ...}\n       │
  │ ─────────────────────────────►│   loads model (may take seconds)
  │                               │
  │            {"event":"ready", ...}\n
  │ ◄─────────────────────────────│
  │                               │
  │   {"cmd":"transcribe", ...}\n │
  │ ─────────────────────────────►│   runs inference
  │                               │
  │      {"event":"transcript", ...}\n
  │ ◄─────────────────────────────│
  │                               │
  │            ... (repeat) ...   │
  │                               │
  │   {"cmd":"shutdown"}\n        │
  │ ─────────────────────────────►│   exits 0
  │                               ╳
```

**Invariants**

- Exactly one event is emitted per command, except:
  - `shutdown` produces no event; the worker simply exits with status 0.
  - `init` emits `ready` only after the model finishes loading (which can take seconds and download files on first run).
- Commands are processed strictly in order; the host MUST NOT pipeline a second command before reading the response of the first.
- Any unrecoverable error during `init` → worker emits `error` then exits with status non-zero.
- Errors during `transcribe` → worker emits `error` and stays alive; the daemon may retry.

### Commands (host → worker on stdin)

#### `init`

```json
{
  "cmd": "init",
  "model": "distil-large-v3",
  "device": "cuda",
  "compute_type": "float16",
  "language": "en",
  "beam_size": 5
}
```

| Field          | Type    | Required | Notes                                                                 |
| -------------- | ------- | -------- | --------------------------------------------------------------------- |
| `cmd`          | string  | yes      | always `"init"`                                                       |
| `model`        | string  | yes      | faster-whisper alias (e.g. `distil-large-v3`), CT2-format HF repo id, or local path |
| `device`       | string  | yes      | `"cuda"` or `"cpu"`                                                   |
| `compute_type` | string  | yes      | `"float16"`, `"int8_float16"`, `"int8"`, `"float32"` per CTranslate2  |
| `language`     | string  | no       | ISO 639-1 code; omit for auto-detect                                  |
| `beam_size`    | integer | no       | default 5                                                             |

#### `transcribe`

```json
{
  "cmd": "transcribe",
  "pcm_b64": "AAAA....",
  "sr": 16000
}
```

| Field     | Type    | Required | Notes                                                                                                  |
| --------- | ------- | -------- | ------------------------------------------------------------------------------------------------------ |
| `cmd`     | string  | yes      | always `"transcribe"`                                                                                  |
| `pcm_b64` | string  | yes      | base64 of **little-endian signed 16-bit PCM**, mono, no header                                         |
| `sr`      | integer | yes      | sample rate in Hz; v1 requires `16000`                                                                 |
| `hotwords` | string | no       | words to bias decoding towards (faster-whisper `hotwords`); sent only by the wake listener (see 07)  |

The worker decodes via `numpy.frombuffer(base64.b64decode(...), dtype="<i2").astype(np.float32) / 32768.0`.

#### `shutdown`

```json
{ "cmd": "shutdown" }
```

The worker should attempt to release the model (free VRAM) before exiting. Exit status 0.

### Events (worker → host on stdout)

#### `ready`

```json
{
  "event": "ready",
  "model": "distil-large-v3",
  "device": "cuda",
  "compute_type": "float16"
}
```

Emitted exactly once, after `init` succeeds.

#### `transcript`

```json
{
  "event": "transcript",
  "text": "hello world this is a test",
  "language": "en",
  "duration_ms": 1250
}
```

| Field         | Type    | Notes                                                |
| ------------- | ------- | ---------------------------------------------------- |
| `text`        | string  | concatenated segment texts, leading/trailing spaces stripped |
| `language`    | string  | detected language (matches `init.language` if forced) |
| `duration_ms` | integer | wall-clock inference time, exclusive of decode       |

#### `error`

```json
{
  "event": "error",
  "msg": "human-readable message",
  "recoverable": true
}
```

`recoverable: false` means the worker is about to exit (only emitted in response to `init` failures). `recoverable: true` means the worker remains alive; the daemon may issue another `transcribe` or `shutdown`.

### Stderr

Free-form text. The daemon should redirect it to its own log at `info` or `debug` level. Common contents: faster-whisper download progress, CTranslate2 warnings, Python tracebacks on unrecoverable errors.

---

## 2. CLI ↔ daemon protocol (`whisperctl` ↔ `whisperd`)

### Transport

- Unix socket at `${WHISPER_STT_SOCK:-$HOME/.cache/whisper-stt/whisperd.sock}`.
- The verb on the first NDJSON line dispatches between two arms:
  - **Command arm** (`start`/`stop`/`toggle`/`status`/`quit`) — request/response,
    one-shot. Client half-closes write end, reads one response line, closes.
    `toggle` is answered `ok` at once and passed through the key-repeat
    filter (`toggle.rs`, see `06-niri-toggle-ux.md`), so a held or bounced
    key toggles once.
  - **Event arm** (`subscribe`) — long-lived. Daemon emits one event line
    immediately (the current state) and then one per state transition until
    the client disconnects. The client never writes again on this connection.
- Daemon accepts connections concurrently (one tokio task per accept).
  State-mutating commands are funnelled through an mpsc into the main task,
  preserving order. `subscribe` does **not** enter the actor — each
  subscriber holds its own `tokio::sync::watch::Receiver<DaemonStatus>` and
  forwards changes directly.
- The socket file is `unlink`ed on daemon start and on graceful shutdown.

### Commands (one-shot arm)

| Command  | Body                | Effect                                                                   |
| -------- | ------------------- | ------------------------------------------------------------------------ |
| `start`  | `{"cmd":"start"}`   | From `idle`/`notice`/`error`: load the worker (if needed) and open the mic in parallel. State → `starting`, then `listening` once the mic delivers audio. A no-op otherwise. |
| `stop`   | `{"cmd":"stop"}`    | From `listening`: end capture, transcribe the tail (if it has voice), type it. State → `transcribing` → `idle`, or `notice` if nothing was typed. From `starting`: cancel → `idle`. A no-op otherwise. |
| `toggle` | `{"cmd":"toggle"}`  | `start` or `stop`, decided by the current state; ignored while `transcribing`. Repeat-filtered. *(added for niri, see 06)* |
| `status` | `{"cmd":"status"}`  | Read-only snapshot. Does *not* update the idle clock.                    |
| `run`    | `{"cmd":"run","text":"switch to session main"}` | Parse `text` as a voice command (wake phrase optional) and run it on tmux. Result shown as a `notice`; refused commands reply `error`. *(see 07)* |
| `quit`   | `{"cmd":"quit"}`    | Reap capture + worker, remove the socket, exit.                          |

### Responses (one-shot arm)

| Response | Body shape                                                        | When                          |
| -------- | ----------------------------------------------------------------- | ----------------------------- |
| `ok`     | `{"result":"ok"}`                                                 | `start`/`stop`/`quit` success |
| `status` | `{"result":"status","state":"idle","worker_loaded":false,"idle_seconds":42}` | reply to `status`             |
| `error`  | `{"result":"error","msg":"..."}`                                  | any failure                   |

`DaemonStatus` shape (verbatim from `crates/whisper-stt-proto/src/lib.rs`):

```rust
pub struct DaemonStatus {
    pub state: DaemonState,        // "idle" | "starting" | "listening" | "transcribing" | "notice" | "error" | "command"
                                   // "command": wake word heard alone; message = focused app ("terminal" | "browser" | "other")
    pub ok: Option<bool>,          // on a notice reporting a voice command: did it run? omitted otherwise
    pub worker_loaded: bool,
    pub idle_seconds: u64,
    pub message: Option<String>,   // notice/error headline; omitted when None
}
```

### Subscribe verb (event arm)

```
client → daemon:    {"cmd":"subscribe"}\n
daemon → client:    {"event":"state_changed","state":"idle","worker_loaded":false,"idle_seconds":0}\n
                    {"event":"state_changed","state":"starting","worker_loaded":true,"idle_seconds":2}\n
                    {"event":"state_changed","state":"listening","worker_loaded":true,"idle_seconds":2}\n
                    {"event":"level","rms":0.031,"peak":0.12}\n          (~20 Hz while listening)
                    {"event":"state_changed","state":"transcribing","worker_loaded":true,"idle_seconds":9}\n
                    {"event":"state_changed","state":"notice","worker_loaded":true,"idle_seconds":0,"message":"No speech detected"}\n
                    ...
```

Semantics:

- The **first** event a subscriber receives is the *current* state (immediate,
  no need to wait for a transition). Implemented via `tokio::sync::watch`,
  which always holds the latest value and replays it to new receivers.
- Each subsequent event corresponds to a real state transition inside the
  daemon (`AppState::set_state`). Watch deduplicates: `set_state` called with
  the same value twice in a row produces only one event.
- `idle_seconds` is a snapshot at the moment of the transition, not a live
  counter. Subscribers that need a live idle counter should compute it
  locally from a wall-clock timestamp, or call `status` on demand.
- Besides `state_changed`, the daemon sends `level` events (mic RMS and
  peak of the last ~100 ms, linear `[0, 1]`) about 20 times a second while
  listening, and one with zeros when listening ends. A client that only
  wants state can ignore them. *(added in 06)*
- `message` is present on `notice` and `error` (e.g. "No speech
  detected", "Microphone: …") and omitted otherwise, so subscribers written
  before it existed still parse every event.
- Termination: subscriber closes the socket → daemon's writer fails on the
  next `flush`, the per-connection task drops its receiver and exits. Or
  the daemon shuts down → `watch::Sender` is dropped, every subscriber's
  `changed().await` returns `Err(_)`, all connection tasks exit. Either way,
  no leaks.

`CtlEvent` shape (verbatim from `crates/whisper-stt-proto/src/lib.rs`):

```rust
#[serde(tag = "event", rename_all = "snake_case")]
pub enum CtlEvent {
    StateChanged {
        state: DaemonState,
        worker_loaded: bool,
        idle_seconds: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    Level { rms: f32, peak: f32 },
}
```

### Why pub/sub, not polling

Subscribers (e.g. an indicator on the i3More nav bar) need to react within
a few hundred ms of a state change. Polling `status` every 200 ms would burn
five accept-handles per second of idle and add up to one polling-interval of
latency between each transition and the indicator update. The watch-backed
push design has zero idle cost (the connection is held open; both ends are
parked in `read`/`changed`) and propagates state changes within a tokio
scheduler tick.

### Concurrency notes

- `start` and `stop` are **sync over the wire**: the client blocks until the
  daemon's full handler returns. On a cold `start`, that includes worker
  spawn + faster-whisper init (~2–4 s for the first press after an idle
  reap). On `stop`, that includes the entire transcribe round-trip.
- This is intentional. i3's `bindsym --release F9 exec --no-startup-id whisperctl stop`
  is fire-and-forget from i3's side, so the synchronous wait is invisible to
  the user — it only affects the `whisperctl` process, which exits when the
  reply comes back.
- Stress: in M4 verification, 50 rapid `start; stop` cycles in a shell loop
  do not deadlock the daemon. The serial-handler model gives natural
  back-pressure: subsequent commands queue on the mpsc channel.

### Wire types

The full schema lives in
[`crates/whisper-stt-proto/src/lib.rs`](../../crates/whisper-stt-proto/src/lib.rs).
Keeping it in a shared crate (compiled into both `whisperctl` and `whisperd`)
prevents the two from drifting on the wire.
