# 01 — Architecture

## Context

`whisper-stt` is a hold-to-talk dictation tool for editors and terminals on Linux + i3.
Hold **F9**, speak English, release → transcribed text is typed at the cursor.
When the feature is idle, the inference model is unloaded so GPU/RAM is free.

This document is the architectural source of truth for v1. Companion docs under
`doc/plan/` will cover the wire protocol, the audio pipeline tuning, etc.

### Constraints

- **Backend**: `faster-whisper` (Python, CTranslate2) with the `distil-large-v3` alias
  (resolves to `Systran/faster-distil-whisper-large-v3`, the CT2-converted weights).
  Chosen for English accuracy; this differs from `i3More`, which uses `whisper-rs`.
- **Hotkey**: hold-to-talk — press F9 captures, release F9 transcribes + injects.
- **Process model**: long-running daemon + thin CLI client; idle timer unloads the model.
- **UI**: minimal indicator (state pill), no transcript viewer.
- **Target environment** (validated): X11 + i3, PipeWire (with `parec` shim),
  RTX 4050 + CUDA 12, Rust 1.93. F9 is currently unbound in the user's i3 config.

---

## Components

Three processes, two languages — Rust owns desktop integration, Python owns inference.
Whisperd is the central hub: it accepts one-shot commands from `whisperctl` and
**pushes state events to long-lived subscribers** (e.g. an indicator embedded in the
i3More nav bar) over the same Unix socket.

```
       ┌────────────────┐                       ┌──────────────────────┐
i3 F9 ─│  whisperctl    │                       │  i3More bar plugin   │
       │  (Rust CLI)    │  request / response   │  (or any subscriber) │
       └───────┬────────┘                       └──────────┬───────────┘
               │                                           │
               │  start │ stop │ status │ quit   subscribe │  ← long-lived
               ▼                                           ▼     connection
        ╔══════════════════════════════════════════════════════════╗
        ║                        whisperd                          ║
        ║  • parec capture            • idle unload timer          ║
        ║  • VAD-segmenter (streams)  • GTK4 indicator (optional)  ║
        ║  • worker lifecycle         • xdotool injector           ║
        ║  • state broadcast (tokio::sync::watch → all subs)       ║
        ╚════════════════════════╤═════════════════════════════════╝
                                 │ JSON-RPC over stdio
                                 ▼
                        ┌──────────────────────┐
                        │  whisper_worker.py   │
                        │  faster-whisper +    │
                        │  CTranslate2 (CUDA)  │
                        │  distil-large-v3     │
                        └──────────────────────┘
```

### Why split Rust + Python (not pyo3, not pure Python)

- Rust handles audio capture (proven `parec` pattern from `i3More`), the GTK4 indicator
  (`gtk4-rs`), Unix-socket IPC, and `xdotool` invocation — areas where i3More is already
  battle-tested.
- Python keeps `faster-whisper`'s API native; no FFI, no embedded interpreter.
- A subprocess boundary gives clean isolation: a worker crash never takes the daemon down,
  and killing the worker on idle reliably frees VRAM.

### Process responsibilities

**`whisperctl` — tiny Rust CLI.** Invoked by i3 keybindings. Connects to
`~/.cache/whisper-stt/whisperd.sock`, sends one of: `start`, `stop`, `status`, `quit`,
or `subscribe`. The first four are one-shot (write line, read response, close).
`subscribe` keeps the connection open and prints state events as they arrive — useful
for ad-hoc testing (`whisperctl subscribe | jq`) and as a reference implementation
for any subscriber (i3More bar plugin, status-bar widget, etc.). `whisperctl` itself
holds no state.

**`whisperd` — Rust daemon (autostarted via systemd user unit `~/.config/systemd/user/whisperd.service`):**

- Owns the Unix socket server. Each accepted connection is dispatched on its verb:
  one-shot commands return a single response and close; `subscribe` borrows a
  receiver from the internal watch channel and streams events for the lifetime
  of the connection.
- On `start`:
  1. Spawns the Python worker if not loaded.
  2. Opens `parec` capture and **awaits the first PCM sample** (up to 3 s) — this
     covers the PipeWire/Bluetooth HSP wake-up where the source can take 1–2 s to
     resume from `SUSPENDED`. Only after audio is actually flowing does the daemon
     transition to `listening`.
  3. Spawns a streaming **segmenter** task that watches the live PCM with the
     energy-based VAD. When a ~400 ms pause is detected after speech, the
     segmenter submits the segment for transcribe + inject — text appears at
     natural sentence boundaries during the press.
- On `stop`: reaps the segmenter, takes the buffer's tail (whatever the segmenter
  did not commit before release), transcribes + injects it as the final segment,
  state → `idle`. Broadcasts `state_changed → transcribing → idle`.
- Background idle thread: if `now - last_use > IDLE_TIMEOUT` (default 300 s), sends a
  graceful shutdown to the worker and reaps it → VRAM freed. Broadcasts the
  `worker_loaded: false` transition.
- Local GTK4 indicator (when built with `--features gui`) and any remote subscriber
  (e.g. the i3More nav-bar plugin) are equal subscribers to the same watch
  channel — no special path for the in-process indicator. States:
  `idle | listening | transcribing | error`.

**`whisper_worker.py` — single long-running Python subprocess:**

- Reads JSON commands from stdin: `{"cmd":"init"}`, `{"cmd":"transcribe","pcm_b64":...,"sr":16000}`,
  `{"cmd":"shutdown"}`.
- Writes JSON events to stdout: `{"event":"ready"}`, `{"event":"transcript","text":...}`,
  `{"event":"error","msg":...}`.
- Loads `distil-large-v3` (faster-whisper alias for the CT2 repo
  `Systran/faster-distil-whisper-large-v3`) via
  `WhisperModel(..., device="cuda", compute_type="float16")` once on `init`.
- Auto-downloads to `~/.cache/huggingface/` on first run.

### IPC channels — commands + pub/sub events (no polling)

A single Unix socket at `~/.cache/whisper-stt/whisperd.sock` serves both arms of the
protocol; the verb on the first NDJSON line a client sends determines which arm
the connection takes.

**Command arm — request / response, one-shot:**

| Verb     | Reply shape                                                | Connection lifetime |
| -------- | ---------------------------------------------------------- | ------------------- |
| `start`  | `{"result":"ok"}` or `{"result":"error","msg":...}`         | client closes after one response |
| `stop`   | same                                                        | same                |
| `status` | `{"result":"status","state":"...","worker_loaded":...,"idle_seconds":...}` | same                |
| `quit`   | `{"result":"ok"}`                                           | same                |

**Event arm — pub/sub, long-lived:**

| Verb         | First reply                                  | Subsequent traffic                                      |
| ------------ | -------------------------------------------- | ------------------------------------------------------- |
| `subscribe`  | `{"event":"state_changed", ... initial state ... }` | one `{"event":"state_changed", ...}` per state transition, until the client disconnects |

A subscriber sees:

```json
{"event":"state_changed","state":"idle","worker_loaded":false,"idle_seconds":4}
{"event":"state_changed","state":"listening","worker_loaded":true,"idle_seconds":0}
{"event":"state_changed","state":"transcribing","worker_loaded":true,"idle_seconds":0}
{"event":"state_changed","state":"idle","worker_loaded":true,"idle_seconds":0}
```

**Why broadcast, not poll.** Polling the `status` verb every 200 ms would burn an
accept-handle every tick and add up-to-200 ms of latency between a state transition
and the indicator updating. The broadcast model has zero idle cost and propagates
state changes within a tokio scheduler tick — feedback feels instantaneous.

**Implementation sketch (daemon side).** A `tokio::sync::broadcast::Sender<DaemonStatus>`
lives in the daemon main task. On every state mutation (`AppState::set_state`), the
new status is sent. Each accepted `subscribe` connection spawns a small task that
holds a `Receiver`, awaits messages, serialises them as NDJSON, and writes to the
client. When the client disconnects (or the writer errors), the task drops its
receiver and exits — the broadcast channel cleans up on its own.

**Implementation sketch (subscriber side, e.g. i3More bar plugin).** Connect to the
socket, write `{"cmd":"subscribe"}\n`, then loop `read_line()` and update the bar
label on each `state_changed` event. No polling, no timers. Disconnect by closing
the socket — daemon notices on the next send.

The shared types live in `crates/whisper-stt-proto/src/lib.rs`, so any subscriber
written in Rust can depend on that crate (path or git) and avoid hand-rolling JSON
parsing. Subscribers in other languages (e.g. a Python status widget) can read the
schema off this doc — the wire is stable NDJSON.

### i3 binding

Added (idempotently) to `~/.config/i3/config` by `scripts/install-i3-binding.sh`:

```
bindsym            F9 exec --no-startup-id whisperctl start
bindsym --release  F9 exec --no-startup-id whisperctl stop
```

i3 supports `--release` natively, so hold-to-talk needs no X-grab in the daemon.

### GTK4 indicator details

- Small undecorated window, `_NET_WM_WINDOW_TYPE_NOTIFICATION`,
  `set_focusable(false)` and `set_can_focus(false)` so it never steals focus from the
  editor (preserves the `xdotool` target).
- Anchored bottom-right or near cursor (configurable).
- Reuses Gruvbox-dark tokens from `i3More/assets/speech-text-ui.css` for visual consistency.

---

## Monorepo conventions

The repo is a single Cargo workspace plus a co-located Python project, with shared
tooling at the root. Goals:

- **One clone, one build graph.** `just build` (or `cargo build --workspace`) compiles
  every Rust crate. `just worker-setup` provisions the Python venv. CI runs both.
- **Shared protocol types in Rust.** A `whisper-stt-proto` crate holds the IPC and
  worker-stdio data structures used by both `whisperctl` and `whisperd`. This prevents
  the CLI and daemon from drifting on wire shape.
- **Pinned toolchains.** `rust-toolchain.toml` pins the Rust channel; the worker pins
  Python via `requires-python` in `worker/pyproject.toml`.
- **Single task runner.** A root `Justfile` exposes the canonical commands
  (`build`, `test`, `fmt`, `lint`, `worker-setup`, `dev`, `install`) so contributors
  do not need to remember the Cargo / pip / venv incantations.
- **Editor parity.** `.editorconfig` at the root keeps Rust and Python files consistent.
- **One `.gitignore` covers everything** — `target/`, the worker venv, `__pycache__/`,
  build artefacts.

The Python worker is *not* embedded in Cargo via build scripts. It is launched as a
subprocess by `whisperd` — the workspace boundary stays at the language line.

---

## Repository layout

```
whisper-stt/
├── Cargo.toml                       # workspace manifest + [workspace.dependencies]
├── Cargo.lock
├── rust-toolchain.toml              # pin Rust channel
├── Justfile                         # canonical task runner
├── .editorconfig                    # cross-language editor settings
├── .gitignore
├── README.md
├── crates/
│   ├── whisper-stt-proto/           # shared IPC + worker-stdio types
│   │   ├── Cargo.toml
│   │   └── src/lib.rs
│   ├── whisperctl/                  # CLI client (tiny)
│   │   ├── Cargo.toml
│   │   └── src/main.rs
│   └── whisperd/                    # daemon
│       ├── Cargo.toml
│       └── src/
│           ├── main.rs
│           ├── ipc.rs               # Unix-socket server, JSON line proto
│           ├── audio/
│           │   ├── mod.rs
│           │   └── parec.rs         # PORT from i3More src/speech_text/parec.rs
│           ├── vad.rs               # PORT from i3More src/speech_text/vad.rs
│           ├── worker.rs            # spawn + JSON-RPC over stdio + idle reaper
│           ├── injector.rs          # xdotool type wrapper
│           └── indicator.rs         # GTK4 minimal popup
├── worker/                           # Python worker (uv- or pip-managed)
│   ├── pyproject.toml                # PEP 621 metadata + faster-whisper dep
│   ├── whisper_worker.py             # JSON-RPC stdio worker
│   └── README.md                     # standalone-run instructions
├── vendor/
│   └── faster-whisper/               # git submodule (source reference only;
│                                     # runtime install is via pip into worker/.venv)
├── assets/
│   └── indicator.css                 # styling for the GTK4 popup
├── scripts/
│   ├── install-i3-binding.sh         # appends bindsym lines, idempotent
│   └── setup-venv.sh                 # python -m venv worker/.venv && pip install -e worker
└── doc/
    └── plan/
        ├── 01-architecture.md        # this document
        └── 02-protocol.md            # (later) wire protocol detail
```

### Crate dependency graph

```
whisper-stt-proto  ←  whisperctl
        ▲
        └────────────  whisperd
```

`whisper-stt-proto` is the only shared internal crate in v1. `whisperd` adds external
deps (`tokio`, `gtk4`, `x11rb` if needed) that `whisperctl` does not pull in, so the CLI
binary stays small.

### Workspace deps (referenced from `Cargo.toml`)

- `serde`, `serde_json`, `anyhow` — used by every crate.
- `tokio` (full features) — daemon only.
- `log`, `env_logger` — daemon only.
- `gtk4`, `gdk4-x11` — daemon, gated behind a `gui` feature once added (so `cargo
  build -p whisperd --no-default-features` works without GTK4 dev libs installed).

---

## Reusable from `i3More` (do not reinvent)

| Concern               | Source                                                                       | How                                                       |
| --------------------- | ---------------------------------------------------------------------------- | --------------------------------------------------------- |
| `parec` capture loop  | `i3More/src/speech_text/capture.rs`, `parec.rs`                              | Copy and trim — drop sink-monitor switching for v1.       |
| VAD (energy-based)    | `i3More/src/speech_text/vad.rs`                                              | Copy verbatim; keep optional in the pipeline.             |
| GTK4 styling          | `i3More/assets/speech-text-ui.css`                                           | Copy a subset; reuse Gruvbox tokens.                      |
| D-Bus single-instance | `i3More/src/speech_text_ui_main.rs:31–46`                                    | Pattern reference for whisperd's app activation.          |
| Config persistence    | `i3More/src/speech_text.rs:137–155` (serde JSON under `~/.config/<app>/...`) | Same shape, new file `~/.config/whisper-stt/config.json`. |

`i3More`'s `whisper-rs` integration is **not** reused — different backend.

---

## Open items (deferred past v1)

- **Pre-arming `parec`** to eliminate the 1–2 s Bluetooth HSP wake-up. The current
  design waits for the first sample before declaring `listening`; that gives a
  reliable user cue (pill stays gray during warmup, turns red when the mic is
  actually live) but the user still has to wait. A continuously-running parec
  with a ring buffer would make first-press latency near-zero, at the cost of
  pinning the headset to call-mode.
- **Continuous-partial transcripts with backspace-revise**. Today's segmenter
  commits at speech-end pauses, so text appears in clauses (300–800 ms behind
  the speaker). True per-500 ms partials with backspace-revise would feel more
  "live" but trades against cursor stutter when Whisper revises.
- **Bluetooth-stack auto-recovery.** PipeWire/bluez occasionally lands the HSP
  source in a stuck state where parec produces 0 samples for any caller (not
  daemon-specific). For now the user runs `whisper-stt-use-jabra` to
  disconnect/reconnect. Could be automated via a daemon-side health probe.
- **i3More bar plugin** is now implemented in the sister `i3More` repo —
  `src/whisper_stt_indicator.rs` subscribes to the daemon's event channel and
  renders the pill in the nav bar.
- **Punctuation / capitalization normalization.** faster-whisper output is usually
  well-formatted; tweak only if needed.
- **Alternative injectors** (`dotool`, clipboard-paste). v1 sticks with `xdotool`
  (X11 only; confirmed available on this machine).
- **Wayland support.** Out of scope until the user moves off i3.

---

## Verification (end-to-end, post-implementation)

1. `just bootstrap`. (Optional: `just model-pull` to pre-download the
   ~1.5 GB model so step 4's first press does not block on the network.)
2. `i3-msg reload` to pick up the new bindings + indicator float rule.
3. `just daemon-start`. Confirm `worker ready (cuda, distil-large-v3)`
   appears in `tail -f /tmp/whisperd.log`.
4. Focus a terminal, hold **F9**, speak `"hello world this is a test"`, release.
   - Indicator transitions `listening → transcribing → idle`.
   - Text appears in the terminal.
5. Wait 5 minutes. Check `nvidia-smi` — the daemon's VRAM should drop to 0 MiB
   (worker reaped).
6. Press F9 again — worker respawns, transcribes. Cold-start latency expected on this press.
7. Open `nvim`, hold F9 in insert mode, speak, release — text inserts at cursor.
8. `just stress` — 50 rapid start/stop cycles via `whisperctl`. Should
   complete with `50 cycles OK` and leave no orphan processes
   (`pgrep -x whisperd` shows the single daemon). Focus a scratch
   terminal first; every cycle injects a transcript fragment.
   Tail `/tmp/whisperd.log` (or `journalctl --user -fu whisperd` if you
   set up the systemd unit) for errors during the run.


Open VS codeHey Ubuntu open

Testing one, two, three.
So cool. So cool.

Thank you.