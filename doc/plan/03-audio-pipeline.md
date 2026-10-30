# 03 — Audio Pipeline

How `whisperd` captures microphone audio and feeds it to the worker.

---

## Format

Fixed across the pipeline (and pinned by the worker — see
[`02-protocol.md`](02-protocol.md)):

| Field        | Value      |
| ------------ | ---------- |
| Sample rate  | 16 000 Hz  |
| Channels     | 1 (mono)   |
| Sample format| `S16_LE`   |

`crates/whisperd/src/audio/mod.rs::SAMPLE_RATE` is the single source of truth
for this constant.

---

## Backend: `parec`

We use `parec` (the PulseAudio compatibility tool, present on PipeWire systems
via `pipewire-pulse`) for capture. The exact command:

```
parec --format=s16le --rate=16000 --channels=1 --raw --latency-msec=50 [--device <source>]
```

`--device` is omitted by default so parec uses the system default source —
i.e. whatever the audio control panel currently selects. Set
`WHISPER_STT_SOURCE=<source-name>` to pin a specific microphone.

### Why parec, not native PipeWire?

i3More's history (see `i3More/src/speech_text/parec.rs:1` and
`pipewire.rs:1`) showed that on PipeWire 1.0.5, the native client capture
returns silent buffers in HSP/HFP profile (Bluetooth call profile). The
PulseAudio compat path that backs `parec` does not have this regression.
Whisper-stt v1 inherits that conclusion: stick with parec.

A native-PipeWire backend (mirroring `i3More/src/speech_text/pipewire.rs`)
is in the open-items pile, gated behind a `pipewire` cargo feature once the
upstream regression is resolved.

---

## Capture lifecycle

`crates/whisperd/src/audio/{mod.rs, parec.rs}` provides:

```rust
let session = CaptureSession::start(&AudioConfig::from_env()).await?;
let buf = session.buffer();   // Arc<Mutex<Vec<i16>>> — read while live
// ... user holds F9; segmenter reads from `buf` and commits at pauses ...
let pcm: Vec<i16> = session.stop();   // returns the full PCM
```

- **`start` (async)** spawns parec, takes its stdout, and starts a `parec-reader`
  thread. The reader appends decoded `i16` samples to a shared `Vec<i16>`
  (under `Mutex`). `start` then **awaits the first sample** with a 3 s
  timeout — see "First-sample wait" below.
- **`buffer()`** returns a clone of the shared `Arc<Mutex<Vec<i16>>>` so the
  segmenter can read in-progress audio without stopping capture.
- **`stop`** sets a stop flag, SIGKILLs the parec child, joins the reader
  thread, and returns the accumulated buffer.
- **`Drop`** does the same teardown if `stop` was not called explicitly,
  so a panic anywhere upstream cannot leak parec processes.

### First-sample wait

PipeWire suspends inactive sources, especially Bluetooth ones in HSP/HFP
profile. From a `SUSPENDED` source, parec takes ~1.5–2 s after spawn to
produce its first byte. Without compensating for that, short F9 holds
(typical: 1–3 s) capture zero samples and the daemon transcribes silence
("Thank you." hallucination).

`ParecCapture` exposes `await_first_sample(timeout)` — the reader thread
fires a `tokio::sync::oneshot` on the first non-empty append, and the
`CaptureSession::start` future awaits it (3 s timeout). Only after this
does the daemon transition to `listening`. So the indicator pill stays
gray during the warm-up and turns red exactly when the mic is live —
that's the user's cue to start speaking. If the timeout fires,
`handle_start` returns `Err`, daemon broadcasts `state → error`, and the
press is dropped. Clear failure mode for muted mics / dead Bluetooth links.

### Source pinning (`WHISPER_STT_SOURCE`)

By default the daemon passes no `--device` to parec, so parec uses
PulseAudio's default source. To pin to a specific source (e.g. when the
default is a flaky Bluetooth one and you'd rather use the laptop mic),
set `WHISPER_STT_SOURCE` to the source name (`pactl list sources short` →
the second column) in the systemd unit:

```
Environment=WHISPER_STT_SOURCE=alsa_input.pci-0000_00_1f.3-...HiFi__hw_sofhdadsp_6__source
```

Followed by `systemctl --user daemon-reload && systemctl --user restart whisperd`.
Remove the line to revert to "follow default".

---

## Streaming segmenter

`crates/whisperd/src/segmenter.rs` is the streaming-mode VAD-segmented
commit task that runs alongside parec while the daemon is in `Listening`.

- **Tick interval**: 100 ms. Cheap polling — only locks the buffer mutex
  briefly to copy a 500 ms window for VAD inspection.
- **VAD window**: 500 ms (8000 samples at 16 kHz). The energy-based
  silence check (`vad::is_chunk_silent`) is the same one ported from
  `i3More`'s `whisper.cpp`-derived VAD.
- **Commit trigger**: a transition from speech-in-window → silence-in-window
  (i.e. the speaker just paused). The committed segment runs from the last
  commit point to the current end of the buffer. Single mid-press commits
  produce log lines like:
  ```
  segmenter: committing 38400 samples (2400 ms) at pause
  segment transcript: "hello world this is a"
  ```
- **Minimum segment length**: 300 ms — avoids triggering on micro-pauses
  inside a single word.
- **Submission path**: segmenter sends `InternalCmd::TranscribeSegment(pcm)`
  through the actor's mpsc channel. The actor processes it serially with
  any pending IPC commands. The actor stays in `Listening` state during
  mid-press commits — no pill flicker between transcribes.
- **Tail flush**: when F9 is released, `handle_stop` reaps the segmenter
  (signalling it to exit), reads the segmenter's `last_committed`
  position from a shared `AtomicUsize`, and transcribes everything from
  that position to the buffer's end as the final segment. This catches
  the clause the user was speaking when they released the key (which
  doesn't end with the trailing silence the segmenter normally needs).
- **Visibility to subscribers**: the i3More pill stays red throughout
  the press and only flicks to yellow/idle on the tail flush. Mid-press
  commits don't change the daemon state, so they don't broadcast.

### What was stripped from i3More

i3More's `src/speech_text/capture.rs` runs a *supervisor* thread that:

- Subscribes to `pactl subscribe` events.
- Re-resolves the default sink on every event.
- Tears down + re-spawns the backend whenever the default sink changes
  (Bluetooth A2DP ↔ HSP/HFP, USB hot-swap, etc.).

Whisper-stt v1 drops that entire mechanism. Justifications:

1. **Different target.** i3More captures the default *sink monitor*
   (playback). Sink switching is part of its core UX (transcribe whatever
   is playing, on whatever output is active). Whisper-stt captures the
   default *source* (mic). Mic switching mid-utterance is rare and
   recoverable by re-pressing F9.
2. **Press is short.** A capture session lives for the duration of a F9
   hold — typically a few seconds. The probability of a default-source
   change inside one press is vanishingly small.
3. **Less code, less surface.** Cutting the supervisor saves ~150 LOC and
   the `pactl subscribe` subprocess. We add it back if real-world use
   shows mid-press source changes are an issue.

---

## Backpressure policy

i3More uses a `sync_channel(64)` between the reader thread and the consumer
that **drops** PCM chunks under backpressure (see
`i3More/src/speech_text/capture.rs:27` — `CHANNEL_DEPTH = 64`). Acceptable
for live transcription where slight gaps are tolerable.

Whisper-stt does not drop. A dictation tool that loses words in the middle
of a sentence is unusable. Instead the reader appends every chunk to an
unbounded `Vec<i16>`. At 32 KB/s, even a 5-minute monologue is ~10 MB —
well within RAM. If we ever care, an upper bound + backpressure-stop can
be added at the audio layer without touching the worker side.

---

## VAD

*(Rewritten in 06.)* The old `vad.rs` (i3More's fixed `mean |s| < 0.003`
check) is gone. `crates/whisperd/src/voice.rs` is an adaptive voice tracker
that is in the capture path: the segmenter feeds it every new sample.

- It runs on 20 ms frames, after an 80 Hz high-pass that only feeds the
  measurement. The PCM sent to Whisper is untouched.
- It keeps a noise floor that adapts to the mic and the room. Voice starts
  above 5× the floor and continues above 3× it, both clamped.
- Voice must last 200 ms before it counts, so clicks and coughs don't.
- It decides three things:
  - where pieces are cut (500 ms of pause after voice)
  - whether the final tail has voice; if not, it's never sent to Whisper
  - when to auto-stop (30 s of captured audio without voice)

The design and thresholds follow Myna's tracker; see
`06-niri-toggle-ux.md` (P3).

---

## Verification (M3)

```bash
cargo run -p whisperd -- --record 3
```

Speak for the 3 seconds. Expected output (with a real mic and mid-volume
speech):

```
[whisperd] recording 3 s from default source — speak now
[whisperd] captured 48000 samples (3000 ms)
[worker]   loading model distil-large-v3 on cuda (float16)
[whisperd::worker] worker ready: model=distil-large-v3 device=cuda compute_type=float16
hello world this is a test
[whisperd] transcript (612 ms inference, language=en): "hello world this is a test"
```

Sanity checks:

- `captured 48000 samples` exactly = 3 s × 16 kHz. If you see far fewer,
  parec is not reading from your mic — check `pactl get-default-source`
  and `pavucontrol` input levels.
- If the transcript is `"Thank you."` again, the mic is feeding silence.
  Try `WHISPER_STT_SOURCE=$(pactl get-default-source)` explicitly, or
  pass another source name from `pactl list sources short`.

---

## Code reference

- Capture wrapper: [`crates/whisperd/src/audio/mod.rs`](../../crates/whisperd/src/audio/mod.rs)
- parec subprocess: [`crates/whisperd/src/audio/parec.rs`](../../crates/whisperd/src/audio/parec.rs)
- VAD: [`crates/whisperd/src/vad.rs`](../../crates/whisperd/src/vad.rs)
- M3 dev CLI: `whisperd --record <secs>`
