# 06 — niri toggle dictation (Myna-style experience)

Ports whisper-stt from i3 hold-to-talk (F9) to niri press-to-toggle
(Super+J), and borrows the dictation experience of Canonical Myna
(`github.com/canonical/myna`): auto-stop on silence, clause-by-clause text,
sound cues, a bar pill with a live mic level, and visible errors.

Myna is AGPL-3.0 and this repo is MIT/Apache-2.0, so **no Myna code was
copied**. Its algorithms, timings and thresholds (facts and measurements)
were reimplemented, and each place says where in Myna the idea comes from.

---

## Why: the "Thank you" bug

On niri, dictation only ever typed "Thank you." Three causes stacked:

1. **niri has no key-release binds and repeats binds by default.** Holding
   F9 fired start/stop pairs at key-repeat rate.
2. **Every stop transcribed its tail with no length or silence check**
   (`main.rs`, `handle_stop`). Each pair sent ~50–300 ms of near-silence to
   Whisper.
3. **The worker used faster-whisper's defaults.** On near-silent audio
   Whisper confidently answers "Thank you."

Smaller faults made it worse:
- segments were typed with no space between them ("VS codeHey")
- the fixed `0.003` silence threshold called steady room noise speech
- `Error` stuck until the next press
- the worker paths only resolved from the repo directory

---

## Phases

Each phase ended with `cargo test --workspace`, `cargo clippy
--workspace --all-targets -D warnings` and `cargo fmt --check` green.

### P1 — Stop the "Thank you" ✅

- **Gate before Whisper.** Nothing under 0.3 s and nothing without
  sustained voice is decoded (`transcriber::MIN_DECODE_SAMPLES`,
  `worth_decoding`, `Job::Tail { voice }`). A session that typed nothing
  ends in a `notice` "No speech detected", never in invented text. *(Myna:
  `MIN_DECODE_S = 0.3`, `server/src/myna/testbed/streaming/loop.py`.)*
- **Worker decoding guards** (`worker/whisper_worker.py`):
  - `log_prob_threshold=-0.5`
  - `temperature=(0.0, 0.2)`
  - `condition_on_previous_text=False`
  - `vad_filter=True`
  The reasons are in code comments. *(Myna: `server/src/myna/testbed/
  whisper.py`, measured: silence hallucinations 5/12 → 0/12.)*
- **parec:** `--latency-msec=50`; stderr goes to the log instead of
  `/dev/null`.
- **Cleanup:** stray dictated text committed in `Justfile` ("`Thank
  you.install`") and `01-architecture.md` was removed.

**Evidence:** `whisperd --transcribe` on a 0.2 s silent WAV and on 3 s of
room noise both log `no speech detected; not decoding`. Unit tests:
`transcriber::tests::*`.

### P2 — A toggle that never misfires ✅

- **Repeat filter** `toggle.rs`: one toggle per press, whatever the key
  repeat does. *(Myna: `myna-desktop/src/shortcut/control.rs`, `REPEAT_QUIET`
  800 ms / `REPEAT_FOLLOW` 200 ms.)*
  - a poke ≥ 800 ms after the last toggles at once
  - a closer one toggles unless another follows within 200 ms (so a lone
    quick second press costs 200 ms)
  - a recognised repeat stream is dropped
  The meaning of a toggle (start/stop) comes from the daemon's state, so
  Myna's press/release `resync` isn't needed.
- **Explicit states:** `idle | starting | listening | transcribing |
  notice | error`. Legal transitions are in `DaemonState::can_become`
  (proto), tested pair by pair. *(Myna: `controller.rs` transition table.)*
- **Responsive daemon:**
  - mic warm-up runs as a task (`CaptureReady`)
  - decoding lives in `transcriber.rs`, so the loop never waits
  - a toggle during `starting` cancels cleanly; a late warm-up is dropped
    by session generation
- **Order:** one FIFO job queue, so pieces are always typed before the
  final tail.
- **Errors don't stick:** `error` clears on the next toggle, and `notice`
  clears itself after 8 s.

**Evidence:** `toggle::tests::*` (held key → 1 toggle, burst → 1, quick
second press → toggles after 200 ms, double tap → start then stop). Live:
three pokes 30 ms apart toggled once. 50 start/stop cycles 50 ms apart
all cancelled cleanly during warm-up: nothing decoded, no stray `parec`.

### P3 — Voice detection, auto-stop, mic level ✅

- **`voice.rs`** replaces `vad.rs`'s fixed threshold with an adaptive
  tracker:
  - 20 ms frames, measured after an 80 Hz high-pass (measurement only)
  - a noise floor that drops fast and rises slowly
  - voice starts above 5× the floor and stays above 3× it
  - 200 ms sustain, so clicks and coughs don't count
  *(Myna: `myna-audio/src/voice.rs`, from murmure's `AdaptiveVad`.)*
- **Segmenter** (`segmenter.rs`, 50 ms tick):
  - commits a piece after 500 ms of pause following voice
  - publishes the level about 20 times a second
  - asks for auto-stop after `WHISPER_STT_SILENCE_TIMEOUT_SECS` (default
    30, 0 = off) of *captured* audio without voice. *(Myna: `silence-timeout`,
    counted in captured audio so a stalled device isn't a quiet user.)*

**Evidence:** `voice::tests::*`:
- digital silence → no voice
- a 20 ms click → no voice
- DC offset → no voice
- steady room noise → voice only for about the first second, while the
  floor adapts; then speech over that noise is detected and ends promptly

Live: `level` events at about 20 Hz; the quiet room peaked at 0.0009 RMS
and the 11.8 s tail was skipped (`voice=false`).

### P4 — Clean typing ✅

- **`joiner.rs`** puts one space between pieces unless whitespace is
  already there. It adds none before `.,;:!?)]}…`, none after an opening
  bracket or quote, and none between CJK or Thai characters. *(Myna:
  `CommitBuffer` in `controller.rs`.)*
- Myna also batches consecutive pieces into one commit, to dodge an IBus
  race. That isn't needed here: `wtype` calls run one after another.

**Evidence:** `joiner::tests::*`.

### P5 — Sound cues ✅

- **`cue.rs`** plays sounds through `pw-play` from a queue of 3; when the
  queue is full, new cues are dropped.
  - start: entering `listening`
  - stop: entering `transcribing`
  - error: entering `error`
  - Sounds are driven by state changes, so they can't disagree with the
    pill. *(Myna: `Chiming` indicator wrapper.)*
- **Files:** freedesktop sounds `message-new-instant.oga`, `complete.oga`
  and `dialog-error.oga`.
- **Settings:** `WHISPER_STT_SOUNDS=0` turns them off;
  `WHISPER_STT_SOUND_DIR` overrides the folder.

### P6 — Events and visible errors ✅

- **`state_changed` gains an optional `message`.** It carries the notice
  or error headline (e.g. "Microphone: …", "Model failed to load: …").
- **New `level {rms, peak}` event** while listening.
- **Backward compatible:** old subscribers still parse events (test:
  `old_subscribers_still_parse_state_events_without_message`).
- **`whisperctl status`** prints the message.
- **Two-tier logging:** transcript text is logged only with
  `WHISPER_STT_DEBUG=1`; otherwise only lengths. *(Myna: `info_log!` /
  `dbg_log!`.)*

### P7 — Install on niri ✅

- **Worker paths** default to the repo the binary was built from
  (`WORKER_DIR`, fixed at build time). The env vars still override them.
  `WHISPER_STT_LANGUAGE=""` means auto-detect.
- **`systemd/whisperd.service`:**
  - bound to `graphical-session.target`
  - no `DISPLAY`/`XAUTHORITY`
  - `WAYLAND_DISPLAY` comes from the user manager, which niri imports
  - installed by `just install-service`
- **niri** (dotfiles `niri/config.kdl`):
  ```kdl
  Mod+J repeat=false hotkey-overlay-title="Dictation (whisper-stt)" { spawn "/home/shoney/.local/bin/whisperctl" "toggle"; }
  ```
  The previous `Mod+J` focus-left is still on `Mod+Left`.
- **DMS bar pill** (dotfiles `dms/.config/DankMaterialShell/plugins/
  whisperStt/`) streams `whisperctl subscribe`:
  - mic icon, tinted with a level bar while listening
  - pulses while starting or transcribing
  - dim notice icon; error icon plus a desktop notification
  - click toggles; when the daemon is down, a click starts it

**Evidence:** `niri validate` passes. The unit is active. The DMS log
shows `Plugin loaded: whisperStt` and a live `whisperctl subscribe`.
End-to-end on a throwaway daemon (own socket, typing disabled):
`starting → listening` (mic live before the model) → model loaded about
5 s later → toggle → `transcribing → notice "No speech detected"`.

### P8 — Docs ✅

This file, plus updates to `README.md`, `02-protocol.md`,
`03-audio-pipeline.md` and `05-build-install.md`.

---

## Live check

2026-10-29: the user confirmed on niri that dictation with Super+J works. The items below were not each reported back separately:

1. In zen and in Alacritty/tmux, press **Super+J** and say two sentences
   with a pause:
   - text appears clause by clause, with spaces between pieces
   - the start sound plays, then the stop sound when you press Super+J
     again
   - the pill shows the level while you speak
2. Press Super+J and stay silent: after 30 s it stops itself with the stop
   sound and a "No speech detected" notice.
3. **Mic gain:** the built-in mic sat at 37 % (−26 dB). If speech comes
   back as "No speech detected", raise the input volume.

## Known limits

- `wtype` types one character at a time (`-d 1`), so long pieces take a
  moment to appear.
- Typing goes wherever focus is; there is no password-field guard (Myna
  only gets one through IBus content types).
- A room that is loud from the first frame reads as voice for about a
  second while the floor adapts. Myna's tracker behaves the same.

---

## Code reference

| File | Role |
|------|------|
| `crates/whisper-stt-proto/src/lib.rs` | states + `can_become`, `message`, `level` event |
| `crates/whisperd/src/main.rs` | session state machine (`AppState`) |
| `crates/whisperd/src/toggle.rs` | key-repeat filter |
| `crates/whisperd/src/voice.rs` | adaptive voice tracker, level |
| `crates/whisperd/src/segmenter.rs` | pause commits, level, auto-stop |
| `crates/whisperd/src/transcriber.rs` | worker owner, gate, ordered jobs, idle unload |
| `crates/whisperd/src/joiner.rs` | spacing between pieces |
| `crates/whisperd/src/cue.rs` | sound cues |
| `worker/whisper_worker.py` | decoding guards |
| `systemd/whisperd.service` | user unit |
