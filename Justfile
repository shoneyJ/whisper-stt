default:
    @just --list

# One-shot first-time setup: provision worker venv, build release, install
# binaries to ~/.local/bin, append the i3 bindings + indicator float rule.
# Run `just model-pull` separately if you want to pre-download the model.
bootstrap: worker-setup build-release install install-i3-binding install-i3-indicator-rule
    @echo "bootstrap done — reload i3 (i3-msg reload), then 'just daemon-start'."

# Build all Rust crates in the workspace.
build:
    cargo build --workspace --all-targets

# Optimised release build.
build-release:
    cargo build --workspace --release

# Run all tests (Rust workspace + Python worker if pytest available).
test:
    cargo test --workspace
    cd worker && [ -d .venv ] && .venv/bin/python -m pytest -q || echo "(worker venv not set up; skipping)"

# Format Rust + Python sources.
fmt:
    cargo fmt --all
    cd worker && [ -d .venv ] && .venv/bin/python -m ruff format . || true

# Lint Rust workspace.
lint:
    cargo clippy --workspace --all-targets -- -D warnings

# Provision the Python worker venv (./worker/.venv) and install faster-whisper.
worker-setup:
    cd worker && python3 -m venv .venv
    cd worker && .venv/bin/pip install -U pip
    cd worker && .venv/bin/pip install -e .

# Pre-download the faster-whisper model (~1.5 GB) so the first F9 press
# doesn't pay the download cost. Idempotent.
model-pull:
    worker/.venv/bin/hf download Systran/faster-distil-whisper-large-v3

# Run the daemon (debug build) in the foreground for local development.
dev:
    cargo run -p whisperd

# Install release binaries to ~/.local/bin (assumes the dir is on PATH).
# NOTE: this overwrites a GUI binary installed via `just gui-install`.
# Re-run `just gui-install` to put the GUI variant back.
install: build-release
    install -Dm755 target/release/whisperd  $HOME/.local/bin/whisperd
    install -Dm755 target/release/whisperctl $HOME/.local/bin/whisperctl

# Start the installed whisperd in the background. Logs to /tmp/whisperd.log.
# Assumes `Thank you.install` (or `just gui-install`) has placed the binary on
# PATH. Uses `nohup` so closing the shell does not kill it.
daemon-start:
    @if pgrep -x whisperd >/dev/null; then \
        echo "whisperd already running (pid $$(pgrep -x whisperd))" ; \
    else \
        nohup whisperd >/tmp/whisperd.log 2>&1 & \
        sleep 0.3 ; \
        echo "whisperd started; tail -f /tmp/whisperd.log" ; \
    fi

# Graceful shutdown via the IPC quit command, falling back to pkill.
daemon-stop:
    -whisperctl quit 2>/dev/null
    -pkill -x whisperd 2>/dev/null
    @echo "whisperd stopped"

# Stop and re-start (used after `just install` / `just gui-install`).
daemon-restart: daemon-stop daemon-start

# 50 rapid start/stop cycles — verification step 8 (stress).
# WARNING: every cycle injects a (likely silent → "Thank you.") transcript
# into the focused window. Focus a scratch terminal first.
stress:
    @echo "WARNING: 50 start/stop cycles will inject text into the focused window."
    @echo "Press Ctrl-C in the next 3 seconds to abort."
    @sleep 3
    @for i in $$(seq 1 50); do \
        whisperctl start && sleep 0.05 && whisperctl stop || exit 1 ; \
    done
    @echo "50 cycles OK"

# Append i3 keybindings (idempotent).
install-i3-binding:
    bash scripts/install-i3-binding.sh

# Append the i3 float-rule for the GTK4 indicator window (idempotent).
install-i3-indicator-rule:
    bash scripts/install-i3-indicator-rule.sh

# Build whisperd with the GTK4 indicator (M7) inside i3More's Docker
# build environment — no host dev libs required. Drops the binary at
# ./dist/whisperd-gui (root-owned; readable for install). See
# doc/plan/05-build-install.md for prerequisites.
gui-build:
    docker compose run --rm builder bash -c \
        "cargo build --release --features gui -p whisperd && \
         install -Dm755 target/release/whisperd dist/whisperd-gui"

# Install the GUI-enabled whisperd over ~/.local/bin/whisperd.
gui-install: gui-build
    install -Dm755 dist/whisperd-gui $HOME/.local/bin/whisperd
    @echo "GUI whisperd installed. Restart the daemon: just daemon-restart"
