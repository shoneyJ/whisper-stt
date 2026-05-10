# 05 — Build & Install

End-to-end installation of `whisper-stt` v1 on Linux + i3.

---

## Prerequisites

| What                | Why                                  | Install                              |
| ------------------- | ------------------------------------ | ------------------------------------ |
| Rust 1.93+          | build the workspace                  | `rustup default stable`              |
| Python 3.10+        | run the worker                       | system package or pyenv              |
| `parec` (PulseAudio compat) | mic capture                  | `apt install pulseaudio-utils` (works on PipeWire too) |
| `xdotool`           | type the transcript at the cursor    | `apt install xdotool`                |
| NVIDIA driver + CUDA 12 | GPU inference (optional but ~10× faster) | distro driver + `apt install nvidia-cuda-toolkit` |
| `libgtk-4-dev`      | only needed for `--features gui` (M7) | `apt install libgtk-4-dev`           |

CPU-only fallback: set `WHISPER_STT_DEVICE=cpu WHISPER_STT_COMPUTE_TYPE=int8`.

---

## Build

```bash
# Rust binaries
cargo build --release --workspace

# Python worker venv (one-time)
bash scripts/setup-venv.sh

# Pre-pull the model (~1.5 GB) to avoid first-press lag
worker/.venv/bin/hf download Systran/faster-distil-whisper-large-v3
```

The release binaries land at:

- `target/release/whisperd`
- `target/release/whisperctl`

---

## Install

The `Justfile` has a single-step install (binaries → `~/.local/bin/`):

```bash
just install
# or, equivalently:
install -Dm755 target/release/whisperd  ~/.local/bin/whisperd
install -Dm755 target/release/whisperctl ~/.local/bin/whisperctl
```

`whisperd` looks for the worker venv + script under the **current working
directory** by default. When running it from a global location, point at the
checkout explicitly:

```bash
export WHISPER_STT_PYTHON=$HOME/projects/github/shoneyj/whisper-stt/worker/.venv/bin/python
export WHISPER_STT_WORKER_SCRIPT=$HOME/projects/github/shoneyj/whisper-stt/worker/whisper_worker.py
```

A small launcher is the cleanest way to set those — drop this in
`~/.local/bin/whisperd-launcher`:

```bash
#!/usr/bin/env bash
export WHISPER_STT_PYTHON=$HOME/projects/github/shoneyj/whisper-stt/worker/.venv/bin/python
export WHISPER_STT_WORKER_SCRIPT=$HOME/projects/github/shoneyj/whisper-stt/worker/whisper_worker.py
exec $HOME/.local/bin/whisperd "$@"
```

---

## i3 binding (hold-to-talk)

```bash
bash scripts/install-i3-binding.sh
i3-msg reload
```

The script appends, between marker comments:

```
# >>> whisper-stt bindings >>>
bindsym            F9 exec --no-startup-id whisperctl start
bindsym --release  F9 exec --no-startup-id whisperctl stop
# <<< whisper-stt bindings <<<
```

Re-running the script is a no-op (marker-guarded). Remove the binding by
deleting the marked block from `~/.config/i3/config`.

---

## Autostart `whisperd` — systemd user unit (recommended)

Drop this in `~/.config/systemd/user/whisperd.service`:

```ini
[Unit]
Description=whisper-stt daemon (hold-to-talk dictation)
After=graphical-session.target
PartOf=graphical-session.target

[Service]
Type=simple
ExecStart=%h/.local/bin/whisperd
WorkingDirectory=%h/projects/github/shoneyj/whisper-stt
Restart=on-failure
RestartSec=2
# whisperd shells out to xdotool, which needs DISPLAY+XAUTHORITY. systemd
# user units don't always inherit them from the X session reliably.
# Adjust DISPLAY (:0/:1) and XAUTHORITY for your distro's location
# (Ubuntu/GDM uses /run/user/<uid>/gdm/Xauthority; some distros use ~/.Xauthority).
Environment=DISPLAY=:1
Environment=XAUTHORITY=/run/user/1000/gdm/Xauthority
# Daemon defaults find the venv + worker relative to cwd; pin absolute paths
# so the service works regardless of WorkingDirectory drift.
Environment=WHISPER_STT_PYTHON=%h/projects/github/shoneyj/whisper-stt/worker/.venv/bin/python
Environment=WHISPER_STT_WORKER_SCRIPT=%h/projects/github/shoneyj/whisper-stt/worker/whisper_worker.py
# Optional — pin a specific PulseAudio source. Omit to follow the system default.
# Useful when the default source is a flaky Bluetooth headset; see "Bluetooth gotchas".
# Environment=WHISPER_STT_SOURCE=alsa_input.pci-...HiFi__hw_sofhdadsp_6__source

[Install]
WantedBy=default.target
```

Enable + start:

```bash
systemctl --user daemon-reload
systemctl --user enable --now whisperd
journalctl --user -fu whisperd
```

The unit auto-starts on every login (`WantedBy=default.target`) and restarts
on crash (`Restart=on-failure`). Does **not** survive logout unless you
enable lingering: `loginctl enable-linger $USER`. For a desktop dictation
tool you almost certainly don't need that.

Day-to-day commands:

```bash
systemctl --user status whisperd          # is it running, what's it doing
systemctl --user restart whisperd         # after `just install` updates the binary
journalctl --user -fu whisperd            # tail logs
systemctl --user disable --now whisperd   # turn off auto-start entirely
```

### Alternative — i3 `exec_always`

If you'd rather not use systemd, drop this in `~/.config/i3/config`:

```
exec_always --no-startup-id /home/<user>/.local/bin/whisperd
```

Restarts on `i3-msg reload`. Stdout/stderr go wherever i3 writes its log
(usually `/tmp/i3-<user>.<pid>/errorlog.txt`). No restart-on-crash; less
flexibility around env vars. Pick systemd if you can.

---

## Bluetooth gotchas (Jabra & friends)

PipeWire's HSP/HFP path for Bluetooth headsets (the only profile with a
working mic) is not bulletproof. Two failure modes worth knowing:

1. **Cold-press 1–2 s wake-up**: from `SUSPENDED`, parec takes ~2 s to
   produce its first sample. Whisperd handles this — `start` blocks
   until audio is actually flowing, the pill stays gray during the wake,
   then turns red. You just have to wait for red before speaking. After
   the source is awake, subsequent presses (within a few seconds) are
   near-instant.

2. **Stuck source**: occasionally PipeWire lands the bluez source in a
   state where parec produces 0 samples for any caller (not daemon-specific).
   The fix is to bounce the headset connection. There's a helper
   installed at `~/.local/bin/whisper-stt-use-jabra`:

   ```bash
   whisper-stt-use-jabra
   ```

   It does: `bluetoothctl disconnect → reconnect → set-card-profile headset-head-unit → set-default-source → 3 s parec sanity check → restart whisperd`.
   If the sanity check fails it leaves whisperd on the laptop mic so you
   don't end up in a worse state. (The script hard-codes the Jabra MAC
   `50:C2:75:48:AA:AF`; edit if yours is different.)

If your Bluetooth headset is unreliable for dictation, **pin to a wired/
internal mic** by adding `Environment=WHISPER_STT_SOURCE=<source-name>`
to the unit (see commented line above). Get the source name from:

```bash
pactl list sources short | grep -v monitor
```

---

## GUI indicator (optional, M7) — built via Docker

The minimal GTK4 indicator is gated behind the `gui` cargo feature, which
pulls `gtk4-rs` and therefore needs `libgtk-4-dev` + transitive `-dev`
libraries at compile time. Rather than installing those on the host, we
reuse the sibling `i3More` repo's Docker build environment (Ubuntu 24.04
+ Rust + the full GTK4 dev stack).

### Prerequisites on the host

| What                       | How                                  |
| -------------------------- | ------------------------------------ |
| Docker (with compose v2)   | `apt install docker.io docker-buildx-plugin docker-compose-plugin` (or equivalent). User in the `docker` group. |
| `i3More` repo at `../i3More` | Default location. Override with `I3MORE_DIR=/path/to/i3More`. |
| GTK4 **runtime** libs      | Already on every desktop install (`libgtk-4-1`, etc.). No `-dev` package on the host. |

### Build

One command:

```bash
just gui-build
```

Which runs (verbatim):

```bash
docker compose run --rm builder bash -c \
    "cargo build --release --features gui -p whisperd && \
     install -Dm755 target/release/whisperd dist/whisperd-gui"
```

On first invocation, `docker compose` builds the image from
`${I3MORE_DIR:-../i3More}/Dockerfile` and tags it `whisper-stt-builder:latest`.
Subsequent builds reuse the image, the `cargo-registry` volume, and the
`cargo-target` volume — typical incremental rebuilds finish in tens of
seconds.

The artefact lands at `./dist/whisperd-gui` on the host.

### Install

```bash
just gui-install
```

Equivalent to:

```bash
install -Dm755 dist/whisperd-gui ~/.local/bin/whisperd
pkill whisperd || true
whisperd &     # or: systemctl --user restart whisperd
```

The existing i3 binding + IPC socket are unchanged. The new binary is a
drop-in replacement; the only difference is that `whisperd` now spawns the
indicator thread on startup.

### Sanity checks

```bash
file dist/whisperd-gui                     # ELF 64-bit, dynamically linked
ldd dist/whisperd-gui | grep libgtk-4     # libgtk-4.so.1 => /lib/x86_64-linux-gnu/...
```

`ldd` should resolve every `lib*.so` to a host library. If something is
missing (e.g. `libgraphene-1.0.so.0`), install only the corresponding
runtime package on the host (`apt install libgraphene-1.0-0`) — never the
`-dev` variant.

### Notes

- **File ownership**: the container runs as root, so `dist/whisperd-gui`
  ends up root-owned on the host. Reading it is fine (mode 0755), but
  rebuilding requires the container to overwrite it (which it can, since
  it runs as root again). Mirrors the i3More pattern. If it bothers you:
  `sudo chown $USER dist/whisperd-gui`.
- **GTK runtime version drift**: i3More's image and the host are both on
  GTK 4.14 today (Ubuntu 24.04). If i3More's image ever moves to GTK 4.18+
  while the host stays on 4.14, the binary may demand symbols the host
  cannot provide. Today, not a concern.
- **No CUDA**: M7 build uses i3More's plain `Dockerfile`, not the heavier
  `Dockerfile.whisper-cuda`. The Python worker on the host still uses CUDA
  via its own venv wheels — that path is untouched.

---

## Verifying the full pipeline

1. `whisperctl status` → JSON snapshot, `worker_loaded:false`.
2. Open a terminal, focus it, hold **F9**, speak `"hello world"`, release.
   The text appears in the terminal.
3. `whisperctl status` → `worker_loaded:true, idle_seconds: <small>`.
4. Wait > 5 minutes. `whisperctl status` → `worker_loaded:false`,
   `nvidia-smi` shows whisperd at 0 MiB.
5. Press F9 again — slight cold-start lag (~2–4 s), then transcript.

---

## Troubleshooting

| Symptom                                                 | Cause                                              | Fix                                                                              |
| ------------------------------------------------------- | -------------------------------------------------- | -------------------------------------------------------------------------------- |
| `whisperctl: ... No such file or directory` (in i3 log) | Binary not on PATH for i3-spawned processes        | Use a launcher in `~/.local/bin/` (see above) or use absolute paths in `bindsym`.|
| `connect to ...whisperd.sock: No such file or directory`| Daemon not running                                 | Start it (autostart or `whisperd-launcher &`).                                   |
| Every transcript is `Thank you.`                        | Mic capture is silent                              | See [`03-audio-pipeline.md`](03-audio-pipeline.md) — verify with `aplay`, set `WHISPER_STT_SOURCE`. |
| First press blocks for ~30 s                            | Model download                                     | Pre-pull with `worker/.venv/bin/hf download Systran/faster-distil-whisper-large-v3`. |
| Indicator never appears (with `--features gui`)         | `libgtk-4-dev` missing or feature flag forgotten   | Re-build: `cargo build --release -p whisperd --features gui`.                    |
| Daemon spins up, no transcript appears in editor        | Editor not actually focused, or running on Wayland | Check focus; xdotool is X11-only, see [`01-architecture.md`](01-architecture.md) open items. |
| Two daemons fight for the socket                        | Stale socket from a crashed previous run           | `rm ~/.cache/whisper-stt/whisperd.sock` and restart (or just restart — daemon unlinks on bind). |

---

## Code reference

- Build profile: [`Cargo.toml`](../../Cargo.toml) (`[profile.release]`)
- Justfile tasks: [`Justfile`](../../Justfile)
- i3 binding script: [`scripts/install-i3-binding.sh`](../../scripts/install-i3-binding.sh)
- Worker venv setup: [`scripts/setup-venv.sh`](../../scripts/setup-venv.sh)
