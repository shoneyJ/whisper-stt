# Whisper STT

Offline dictation for editors and terminals on Linux.

- **niri / Wayland:** press **Super+J**, speak, press **Super+J** again. Text
  is typed at the cursor clause by clause as you pause. Dictation stops on
  its own after 30 s of silence. Start, stop and error sounds play, and a
  DankMaterialShell bar pill shows the state and a live mic level.
- **Voice commands (niri, tmux, Zen):** say **"Jarvis, switch to session
  main"**, "Jarvis, split right", "Jarvis, new tab", "Jarvis, listen". While
  dictating, a key name alone ("enter", "cancel", "run again") presses it
  and "stop listening" stops; text dictated into a terminal is typed as a
  command. Cheat sheet: [`doc/voice-commands.md`](doc/voice-commands.md)
  (**Super+Alt+J**). Design: [`doc/plan/07-voice-commands.md`](doc/plan/07-voice-commands.md).
- **i3 / X11 (original):** hold **F9**, speak, release.

When idle, the model is unloaded so GPU/RAM is free.

Architecture: [`doc/plan/01-architecture.md`](doc/plan/01-architecture.md).
Implementation roadmap (M1–M8): also in `doc/plan/`.

---

## Status

v1 is feature-complete; v2 streaming and the cross-repo i3More pill have landed:

- [x] **M1** Python worker (faster-whisper, CUDA / float16)
- [x] **M2** Daemon ↔ worker stdio channel + WAV dev CLI
- [x] **M3** parec mic capture + `--record` dev CLI
- [x] **M4** Unix-socket IPC server + `whisperctl` client
- [x] **M5** Text injection via `xdotool`
- [x] **M6** i3 binding (`scripts/install-i3-binding.sh`) + build/install doc
- [x] **M7** GTK4 indicator (behind `--features gui`, requires `libgtk-4-dev`)
- [x] **M8** clippy clean, fmt clean, tests green
- [x] **v2 pub/sub IPC** — `subscribe` verb on the same Unix socket; daemon pushes `state_changed` events via `tokio::sync::watch`
- [x] **v2 streaming segmenter** — VAD-segmented commits at speech-end pauses during the F9 hold; text appears clause by clause (`crates/whisperd/src/segmenter.rs`)
- [x] **v2 first-sample wait** — `start` blocks until parec actually produces audio (handles the 1–2 s PipeWire/Bluetooth HSP wake-up); pill stays gray until the mic is live
- [x] **v2 systemd user unit** — auto-start on login + restart-on-crash (`~/.config/systemd/user/whisperd.service`)
- [x] **i3More bar plugin** — `i3More/src/whisper_stt_indicator.rs` subscribes to the daemon and renders the state pill in the nav bar (no polling)

niri toggle experience, modelled on Canonical Myna ([`doc/plan/06-niri-toggle-ux.md`](doc/plan/06-niri-toggle-ux.md)):

- [x] **P1** No more "Thank you": silence/short-audio gate before Whisper + worker hallucination guards
- [x] **P2** Repeat-filtered `toggle`, explicit state machine (`starting`, `notice`), daemon never blocks
- [x] **P3** Adaptive voice tracker, pause-cut pieces, auto-stop on silence, live mic level
- [x] **P4** Correct spacing between typed pieces
- [x] **P5** Start / stop / error sound cues
- [x] **P6** `message` on notices/errors, `level` events, transcript-free logs
- [x] **P7** niri install: systemd unit, Super+J bind, DankMaterialShell bar pill
- [x] **P8** Docs

Voice commands ([`doc/plan/07-voice-commands.md`](doc/plan/07-voice-commands.md)):

- [x] **V1** "Hey niri" wake phrase + tmux session commands (switch/next/previous/last), `whisperctl run`
- [x] **V1.1** "start dictation", "enter" (wake and in dictation), "stop dictation"
- [x] **V2.1–3** "listen"/"stop listening", focus-aware routing + terminal mode, terminal keys (cancel, clear, again, tab, escape, delete word, clear line)
- [x] **V2.4–5** tmux windows/panes, Zen commands, "Jarvis" wake word, cheat sheet
- [x] **V2.6–8** desktop commands (terminal/browser/workspace/maximize), accuracy test tooling (`just record-commands`, `just eval-commands`), hint card on "Jarvis"
- [ ] Record your own clips and run the eval

---

## Quick start

### niri (Wayland)

```bash
just worker-setup      # one-time: worker venv
just model-pull        # one-time: ~1.5 GB model
just install-service   # build, install to ~/.local/bin, enable + start the user unit
```

Then bind a key in niri:

```kdl
Mod+J repeat=false { spawn "whisperctl" "toggle"; }
```

Details, including the DMS bar pill, are in
[`doc/plan/05-build-install.md`](doc/plan/05-build-install.md#niri-wayland).

### i3 (X11)

```bash
# 1. Build
cargo build --release --workspace

# 2. Provision the worker venv (one-time)
bash scripts/setup-venv.sh

# 3. Pre-pull the model (~1.5 GB, optional — saves first-press lag)
worker/.venv/bin/hf download Systran/faster-distil-whisper-large-v3

# 4. Install binaries to ~/.local/bin
install -Dm755 target/release/whisperd  ~/.local/bin/whisperd
install -Dm755 target/release/whisperctl ~/.local/bin/whisperctl

# 5. Wire F9 in i3 (idempotent)
bash scripts/install-i3-binding.sh
i3-msg reload

# 6. Start the daemon (the launcher script in 05-build-install.md fixes the
#    cwd-relative venv lookup; see that doc for systemd unit + autostart).
whisperd &
```

Now hold **F9** in any focused terminal/editor, speak, release.

### Optional: GTK4 indicator (M7) via Docker

The indicator is a small floating status pill showing the daemon's current
state — `idle`, `listening`, `transcribing`, or `error`. It is purely a
visual signal that the daemon is alive and what it is doing right now; the
F9 → speak → text-at-cursor flow works identically whether or not you build
the GUI variant. Build the GUI binary if you want immediate visual
confirmation that a press registered; skip it for the smallest, headless
install.

Builds the `--features gui` binary inside i3More's Docker build environment,
so no GTK4 dev libs are needed on the host. Requires the sibling `i3More`
repo at `../i3More` (override with `I3MORE_DIR=...`) and Docker.

```bash
just gui-install   # builds dist/whisperd-gui in a container, installs to ~/.local/bin
pkill whisperd; whisperd &
```

For the indicator to float (not tile) under i3, also run once:

```bash
bash scripts/install-i3-indicator-rule.sh   # adds a `for_window [title="^whisper-stt$"]` float rule
i3-msg reload
```

Detailed install (launcher script, systemd user unit, troubleshooting):
[`doc/plan/05-build-install.md`](doc/plan/05-build-install.md).

---

## Configuration

All knobs are environment variables. Defaults work out of the box on this
target environment (niri or i3 + PipeWire + RTX 4050). For the systemd
unit, set them as `Environment=` lines in `systemd/whisperd.service`.

| Var                            | Default              | Notes                                  |
| ------------------------------ | -------------------- | -------------------------------------- |
| `WHISPER_STT_PYTHON`           | `<repo>/worker/.venv/bin/python` | Worker venv interpreter (repo = where the binary was built) |
| `WHISPER_STT_WORKER_SCRIPT`    | `<repo>/worker/whisper_worker.py` | Worker entry point        |
| `WHISPER_STT_MODEL`            | `distil-large-v3`    | faster-whisper alias or HF repo id     |
| `WHISPER_STT_DEVICE`           | `cuda`               | Set `cpu` to fall back without GPU     |
| `WHISPER_STT_COMPUTE_TYPE`     | `float16`            | `int8`, `int8_float16`, `float32` also |
| `WHISPER_STT_LANGUAGE`         | `en`                 | ISO 639-1; empty for auto-detect       |
| `WHISPER_STT_BEAM_SIZE`        | `5`                  | 1 = greedy, faster + lower quality     |
| `WHISPER_STT_SOURCE`           | (system default)     | Pin a specific PulseAudio source       |
| `WHISPER_STT_IDLE_TIMEOUT_SECS`| `300`                | Worker is unloaded after this idle     |
| `WHISPER_STT_SILENCE_TIMEOUT_SECS` | `30`             | Auto-stop after this much silence; `0` = never |
| `WHISPER_STT_SOUNDS`           | `1`                  | `0` = no start/stop/error sounds       |
| `WHISPER_STT_SOUND_DIR`        | `/usr/share/sounds/freedesktop/stereo` | Folder with `message-new-instant.oga`, `complete.oga`, `dialog-error.oga` |
| `WHISPER_STT_WAKE`             | `1`                  | `0` = no always-on "Jarvis" listener |
| `WHISPER_STT_WAKE_MODEL`       | `base.en`            | Wake/command model (always on the CPU, int8); `just eval-commands <model>` compares |
| `WHISPER_STT_TMUX_SOCKET`      | (default server)     | tmux socket name (`tmux -L`) for voice commands |
| `WHISPER_STT_DEBUG`            | unset                | `1` = log transcript text (otherwise only lengths) |
| `WHISPER_STT_SOCK`             | `~/.cache/whisper-stt/whisperd.sock` | IPC socket path        |

---

## Layout

```
crates/whisperctl/        Rust CLI (i3 → daemon)
crates/whisperd/          Rust daemon
crates/whisper-stt-proto/ shared IPC + worker-stdio types
worker/whisper_worker.py  faster-whisper stdio worker
worker/tests/smoke.py     end-to-end harness (M1 verification)
doc/plan/                 architecture + protocol + install docs
scripts/                  setup-venv.sh, install-i3-binding.sh
assets/                   indicator.css (used with --features gui)
```

---

## Developer paths

Beyond the production hotkey flow, `whisperd` exposes two dev CLI flags
useful for diagnosing layers in isolation. Run them from the repo root so the
default `WHISPER_STT_*` paths resolve.

```bash
# transcribe a 16 kHz mono WAV
cargo run -p whisperd -- --transcribe worker/tests/fixtures/hello.wav

# capture N seconds from the mic, transcribe, print
cargo run -p whisperd -- --record 3
```

The pure-Python smoke (drives the worker with no daemon at all):

```bash
arecord -f S16_LE -c 1 -r 16000 -d 3 worker/tests/fixtures/hello.wav
worker/.venv/bin/python worker/tests/smoke.py worker/tests/fixtures/hello.wav
```

---

## Verifying the install

```bash
whisperctl status
# {"state":"idle","worker_loaded":false,"idle_seconds":0}
```

After your first F9 hold-and-release: `worker_loaded:true`, `nvidia-smi`
shows ~2 GiB. After 5 min idle: VRAM returns to 0; next press reloads.

---

## Acceptance criteria (v1)

- Hold a hotkey (F9) while the cursor is in an editor → mic streams to the
  worker → transcript is typed at the cursor.
- When the feature is idle, GPU / RAM is freed from model data.

Both criteria are satisfied on the implemented path.
