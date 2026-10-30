# 07 — Voice commands ("Jarvis", terminal first)

Builds on 06 (voice tracker, worker guards, state machine, bar pill).

Say a command instead of dictating text:

> "Jarvis, switch to session main"

The tmux client you used last switches to session `main`. Nothing is typed.
Or in two steps: "Jarvis" (start sound, pill shows a terminal icon, and a
hint card on the right lists the commands for the focused app), then "next
session" within 6 s. "Hey Jarvis" and the old "Hey niri" work too.

Commands fit the focused app: terminal keys in Alacritty, Zen shortcuts in
Zen, tmux from anywhere. While dictating, a key name said alone presses it
("enter", "cancel", "run again"), and "stop listening" ends the session.
Text dictated into a terminal is typed as a command ("git status").

**Cheat sheet:** [`doc/voice-commands.md`](../voice-commands.md), opened
with **Super+Alt+J** in niri.

---

## Why this is harder than dictation

Whisper mishears short names. When the user dictated this feature's
request, "hey niri" came out as "Hal needy", "Hey, needy", "Ni, re",
"N-I-R-A", and "tmux" as "T-Mex" and "Demux". So:

1. **Nothing is matched exactly.** The wake word is any known mishearing or
   anything within one edit of one; spelled letters ("N I R I") are joined.
2. **Names come from a closed list.** The spoken session name is matched
   against the live `tmux list-sessions`. A name that isn't close to one
   session, and clearly closer than the runner-up, is refused, never guessed.
3. **The recogniser is biased.** Each wake decode passes faster-whisper
   `hotwords`: "Hey niri, tmux session" plus the live session names.

---

## Decisions

| Question | Decision | Why |
|----------|----------|-----|
| How is the wake word heard? | **Always-on listener, small Whisper on the CPU** (user's choice); `base.en` since V2 step 7, was `tiny.en` | No training step, works today. Measured: ~0.6–1 s per utterance, ~0.6 % CPU and 165 MB RAM when idle. GPU model stays free to unload. Alternatives were openWakeWord (custom model to train first) and the GPU model (holds ~1.5 GB VRAM forever). |
| Which tmux client? | **Most recently used** (`client_activity`) (user's choice) | Simple; it's the one you were just typing in. |
| How is text understood? | **Rule parser** (`command.rs`), no LLM | Instant, offline, testable; a handful of commands. |
| Wake word? | **"Jarvis"** since 2026-10-30 (user's choice; was "Athena"), first word or after "hey"; "hey niri" kept | Jarvis: 3/3 alone and after "hey" on tiny.en; on the base.en command eval 57/58 across two synthetic voices (the miss: one voice slurred "Jarvis browser" into "sproulzer"). Athena history: | Tested with three synthetic voices on tiny.en without hints: Athena 3/3 with and without "hey"; "niri" 0/2 ("Neary", "near you"); Atlas 2/3 without "hey" ("at this"). Without "hey", unknown commands are ignored silently, to limit false wakes. |
| What may run? | **Allowlist:** tmux session/window/pane actions, terminal keys, Zen shortcuts | Speech never reaches a shell; nothing that closes tmux sessions, windows or panes. |
| Missing session? | **Refuse** with a notice ("No session like …") | Creating sessions by voice can come later (sesh). |
| Dictation running? | **Listener pauses** (its capture closes) | Dictated text can never trigger a command. |

---

## Pipeline

```
whisperd (idle/notice/error/command)
  wake.rs: parec (own capture) ─► Ear: VoiceTracker cuts utterances
     ├─ longer than 8 s: conversation, dropped undecoded
     └─ else ─► tiny.en (CPU) with hotwords ─► text
  command::parse(text, live sessions, require_wake)
     ├─ NotWake        ─► ignore
     ├─ WakeOnly       ─► state `command` (start cue), next utterance within 6 s is the command
     ├─ Command(intent) ─► tmux::run ─► `notice` "tmux: main" (stop cue) + tmux display-message
     └─ Unknown(why)    ─► `notice` with the reason (error cue)
```

---

## Phases

### V1 — Wake phrase and tmux session commands ✅

- **`command.rs`**: tokens (lowercase, punctuation dropped, spelled letters
  joined, commas end a spelled run), wake phrase stripping, intents
  `Switch(name) | Next | Previous | Last`, fuzzy session match (normalised
  Levenshtein ≥ 0.6, margin ≥ 0.15 over the runner-up). "a"/"he" are not
  accepted as "hey", and "nearly" is not accepted as "niri" ("a nearly done
  task" and "he needs…" would wake it).
- **`tmux.rs`**: lists sessions, picks the most recent client, runs
  `switch-client -c <client> -t =<name>` (or `-n`/`-p`/`-l`), shows
  `display-message`. `WHISPER_STT_TMUX_SOCKET` picks a server (`tmux -L`).
- **`wake.rs`**: the listener task and `Ear`, the utterance cutter
  (500 ms lead-in, ends after a 600 ms pause, drops anything over 8 s, keeps
  memory bounded). Restarts the wake model if it dies.
- **Proto**: state `command`; transitions into `notice` from any resting
  state; `run {text}` verb; optional `hotwords` on worker `transcribe`.
- **`whisperctl run <text>`**: same parser and actions, no audio.
- **DMS pill**: `terminal` icon with the blue background in `command` state.
- **Env**: `WHISPER_STT_WAKE=0` turns the listener off;
  `WHISPER_STT_WAKE_MODEL` (default `tiny.en`).

**Evidence:**
- Unit tests (36 in the workspace, all green; clippy, fmt clean):
  `command::tests::*` uses the real mishearings above, `wake::tests::*`
  covers utterance cutting, and `tmux::tests::*` runs on a throwaway
  `tmux -L` server with a control-mode client.
- End to end: piper voice `en_US-lessac-medium` played into a PipeWire
  null sink, used as the throwaway daemon's mic
  (`WHISPER_STT_SOURCE=wtest.monitor`), with a throwaway tmux server:

  | Spoken | tiny.en heard | Result |
  |--------|---------------|--------|
  | "I was talking about the weather today." | same | ignored |
  | "Hey Niri, switch to session main." | "Hey niri, switch to session main" | dotfiles → **main** |
  | "Hey Niri." … "Next session." | "Hey niri" … "next session" | `command` state, then → **dotfiles** |
  | (during dictation) "Hey Niri, switch to session main." | dictated as text | tmux unchanged |

  `whisperctl run switch to session dot files` → dotfiles;
  `whisperctl run switch to indeed` → `Error: No session like "indeed"`.
- Installed (`just install-service`); the journal shows `wake listener on
  (model tiny.en)`.

**Still to check by hand:** your own voice and mic, in a real room. The
built-in mic sits at 37 % (−26 dB); if "hey niri" is missed, raise it first.

### V1.1 — Enter and dictation by voice ✅

User's choices: Enter works both ways; Enter keeps dictating.

- **Wake intents** `Intent::Enter` ("enter", "press/hit enter") and
  `Intent::StartDictation` ("start dictation", "dictate"); tmux actions moved
  under `Intent::Tmux(..)`. "Hey niri, stop dictation" while not dictating
  is refused ("Not dictating").
- **In dictation** (`command::spoken_key`, used by `transcriber.rs`): a
  piece that is *only* "enter" / "press enter" presses Enter
  (`injector::press_enter`: `wtype -k Return`, or `xdotool key Return`) and
  resets spacing so the next piece starts clean; a piece that is only "stop
  dictation" / "stop dictating" ends the session like the silence timeout.
  "Please enter your name." is typed as text.
- `whisperctl run enter` / `whisperctl run start dictation` work too.

**Evidence:** 38 unit tests green (`enter_and_dictation_commands`,
`spoken_keys_only_when_said_alone`); clippy, fmt clean. End to end through
the virtual mic, with a fake `wtype` logging keystrokes:

| Spoken | Result |
|--------|--------|
| "Hey Niri, enter." | `wtype -k Return`, notice "Enter" |
| "Hey Niri, start dictation." | `starting → listening` |
| "git status" … "Enter." | `wtype -d 1 -- Git status`, then `wtype -k Return`; still listening |
| "Stop dictation." | `listening → transcribing → idle`, nothing typed |

**Known limit:** Whisper capitalises the first word ("Git status"), which a
shell rejects. Fixed in V2 step 2.

---

## V2 — Terminal-first voice control

Commands fit the focused app (niri `focused-window`): the same word can
mean different things in Alacritty and Zen ("back"), and keys like Ctrl+C
must never reach the wrong app. The vocabulary stays small; every new word
is one more thing `tiny.en` can mishear.

### Command list

**Anywhere** (after "Athena"; *in dictation* = also works said alone
while dictating):

| Say | Action | In dictation | Step |
|-----|--------|--------------|------|
| listen / dictate / start dictation | start dictation | – | 1 ✅ |
| stop listening / stop dictation | stop dictation | yes | 1 ✅ |
| terminal / browser | focus Alacritty / Zen | – | 6 ✅ |
| workspace 3 / workspace web | niri workspace | – | 6 ✅ |
| maximize | full-width column | – | 6 ✅ |

**Terminal and tmux** (Alacritty focused; tmux actions target the most
recently used client wherever focus is):

| Say | Action | In dictation | Step |
|-----|--------|--------------|------|
| enter / run it / send it / execute | Return (any app) | yes | V1.1 ✅ |
| cancel / control C | Ctrl+C | yes | 3 ✅ |
| clear / clear screen | Ctrl+L | yes | 3 ✅ |
| again / run again / repeat | Up, Return | yes | 3 ✅ |
| tab | Tab | yes | 3 ✅ |
| escape | Esc | yes | 3 ✅ |
| delete word / clear line | Ctrl+W / Ctrl+U | yes | 3 ✅ |
| session main / next / previous / last | switch session | – | V1 ✅ |
| new window, window 2, next/previous window | tmux windows | – | 4 ✅ |
| split right / down, pane left/right/up/down, zoom pane | tmux panes | – | 4 ✅ |
| scroll up | copy-mode | – | 4 ✅ |

**Zen** (Zen focused): new tab, close tab, next/previous tab, tab 3,
back/forward, reload, "search …" (Ctrl+L, type, Enter), "find …" (step 5 ✅).

**Left out on purpose:** closing tmux sessions, panes or windows (a
misheard word can't be undone), and spoken symbols ("dash dash", "pipe"),
which need a mapping table; revisit if commands are dictated often.

### Steps

1. **Synonyms ✅** — "listen", "dictate" start dictation; "stop listening"
   stops it (old phrases still work).
2. **Focus routing + terminal mode ✅** — `focus.rs` asks niri for the
   focused app id (`Alacritty`, `kitty`, `foot`, `ghostty` count as
   terminals; outside niri nothing does). Pieces dictated into a terminal
   go through `joiner::for_terminal`: first letter lowercased unless the
   word looks like an acronym ("AWS"), final `.`/`,` dropped.
3. **Terminal keys ✅** — `command::Key`, pressed by `injector::press`
   (`wtype -M ctrl -k c -m ctrl` …, `xdotool key ctrl+c` on X11). All
   keys but Enter act only while a terminal is focused, both after
   "Athena" ("Ctrl+C: not in a terminal") and in dictation (elsewhere
   "Cancel." is typed). A key resets spacing so the next piece starts
   clean.
4. **tmux windows and panes ✅** — `Tmux::{NewWindow, Window(n),
   NextWindow, PreviousWindow, Split, Pane, Zoom, ScrollUp}`, run on the
   current window of the most recent client's session (`-t =session:`);
   new windows and splits open in the pane's directory. Numbers as digits
   or words ("window two").
5. **Zen commands ✅** — `Intent::Browser`, parsed only while Zen is
   focused (and checked again before acting): Ctrl+T/W, Ctrl+(Shift+)Tab,
   Alt+1–9, Alt+Left/Right, Ctrl+R; "search …" = Ctrl+L, type, Enter;
   "find …" = Ctrl+F, type. In the terminal, "go back" stays the previous
   tmux session. Keys go through one chord builder for wtype and xdotool
   (`injector::Chord`).
   Also in V2: the **Athena** wake word, Enter synonyms (run it, send it,
   execute), readable notices ("Zen: tab 3", "Key: Ctrl+C"), and the
   cheat sheet (`doc/voice-commands.md`, Super+Alt+J).
6. **Desktop commands ✅** — `Intent::Niri` (`niri.rs`): "terminal" /
   "browser" focus the most recently focused window of that kind
   (`niri msg -j windows`, `focus_timestamp`), "workspace 3" / "workspace
   web" → `focus-workspace`, "maximize" → `maximize-column`. "terminal"
   only alone or after a verb, so "session terminal" is still a session.
   Plain "split" was dropped from this step: it would clash with tmux
   "split right/down", and the niri split script lives in dotfiles.
7. **Accuracy test set ✅ (tooling)** —
   `tests/fixtures/commands/phrases.tsv` (29 phrases: id, focused app,
   expected parse, what to say), `just record-commands`
   (`scripts/record-commands.sh`, one 3 s `parecord` clip per phrase; the
   `.wav` files are gitignored, your voice stays local), and `just
   eval-commands [model]` (`whisperd --eval-commands`, `eval.rs`): decodes
   each clip with and without hotwords and scores the parse.
   **Your own clips are not recorded yet**; the numbers below are
   synthetic (piper `en_US-ryan-medium`, one voice).
8. **Hint card ✅** — when "Athena" is heard alone, the `command` state's
   message names the focused app (`terminal` / `browser` / `other`); the
   DMS pill shows a card at the top right listing the commands that fit,
   until the command arrives or the 6 s window ends.

**Evidence (steps 1–3):** 40 unit tests green
(`key_and_dictation_commands`, `spoken_keys_only_when_said_alone`,
`terminal_pieces_read_as_commands`, `alacritty_is_a_terminal_zen_is_not`);
clippy, fmt clean. End to end through the virtual mic with a fake `wtype`,
Alacritty focused:

| Spoken | Heard | Result |
|--------|-------|--------|
| "Hey Niri, clear line." | "Hey niri, clear line." | `wtype -M ctrl -k u -m ctrl` |
| "Hey Niri, listen." | "Hey niri, listen." | `starting → listening` |
| "git status" | "Git status" | `wtype -d 1 -- git status` |
| "Clear." | "Clear." | Ctrl+L |
| "Cancel." | "Cancel" | Ctrl+C |
| "Run again." | "Run again." | Up, Return |
| "Stop listening." | "Stop listening." | `listening → transcribing → idle` |

**Evidence (steps 4–5, Athena):** 45 unit tests green
(`athena_wakes_with_or_without_hey`, `enter_synonyms`,
`tmux_windows_and_panes`, `browser_commands_only_when_the_browser_is_focused`,
`chords_become_wtype_and_xdotool_args`, and the throwaway-server tmux test
now covers new window, select window, split and pane moves); clippy, fmt
clean. End to end through the virtual mic, fake `wtype`, and a fake `niri`
reporting the focused app from a file (so Zen could be "focused" without
touching the real desktop):

| Focus | Spoken | Heard | Result |
|-------|--------|-------|--------|
| terminal | "Athena, new window." | "Athena new window" | window 1 created |
| terminal | "Athena, split right." | "Athena split right" | window 1: 2 panes |
| terminal | "Athena, pane left." | "Athena, pane left" | left pane active |
| terminal | "Athena, window one." | "Athena, window one." | window 1 |
| terminal | "Athena, zoom." ×3 voices | "Athena, zoom" ×2, "Athena, tmux" | zoom on, off; third missed |
| terminal | "Athena was great at the meeting." | same | ignored (no notice) |
| terminal | "Athena, go back." | "Athena go back" | previous tmux session |
| Zen | "Hey Athena, new tab." | same | Ctrl+T |
| Zen | "Athena, tab three." | "Athena tab 3" | Alt+3 |
| Zen | "Athena, go back." | "Athena go back" | Alt+Left |
| Zen | "Athena, search for rust tokio docs." | "…search for rust Tokyo dogs" | Ctrl+L, typed "rust tokyo dogs", Enter |

Two weak spots, both from the small wake model: one-syllable "zoom" (the
"zoom" hotword got it from 0/1 to 2/3; the cheat sheet suggests "zoom
pane"), and free-text search words. Search could be re-decoded with the
dictation model later; for now the cheat sheet suggests "new tab" plus
dictation for long queries.

The cheat-sheet key opens a floating 760×1018 Alacritty with `less`
(checked with `niri msg windows`); `niri validate` passes.

**Evidence (steps 6–8):** 47 unit tests green (`desktop_commands`,
`picks_the_most_recently_focused_window_of_a_kind`, zoom-pane case);
clippy, fmt clean.

- Step 6 through `whisperctl run` and a fake `niri` logging actions:
  "browser" → `focus-window --id 8` (zen), "terminal" → `--id 3`
  (Alacritty), "workspace three" → `focus-workspace 3`, "workspace web" →
  `focus-workspace web`, "maximize" → `maximize-column`, bare "workspace" →
  "Which workspace?".
- Step 7, synthetic set, wake model with / without hotwords:

  | Model | With hotwords | Without |
  |-------|---------------|---------|
  | tiny.en | 25/29 | 25/29 |
  | **base.en** | **29/29** | 26/29 |

  tiny.en with hotwords mangled the wake word itself ("a theta", "as
  theta", "addina", "a fina"); without them it mangled command words
  ("Wino 2", "Pain Left", "soon pain"). base.en needs the hotwords for the
  same command words and then gets everything. So the default wake model
  is now **base.en** (~1 s per command instead of ~0.6 s; idle cost still
  ~0 % CPU, 200 MB RAM measured). The eval also caught a parser bug:
  "zoom pane" parsed as a pane move without a direction; fixed.
- Step 8: with the new build on the real socket and the virtual mic,
  "Athena." → `command` state with message `terminal`; screenshot showed the
  card at the top right with the terminal hints.

**Rename to Jarvis (2026-10-30):** the evidence above was recorded with
"Athena"; the commands are unchanged. Re-run of the eval with "Jarvis"
phrases, base.en, with / without hotwords:

| Voice | With hotwords | Without |
|-------|---------------|---------|
| ryan | 29/29 | 25/29 |
| lessac | 28/29 | 26/29 |

The re-run found "maximize" heard as "maximized" (now accepted) and
"browser" missing from the hotwords (added, with terminal, workspace,
maximize). Also fixed while here: tmux `next-3.9` (now installed) reports
a stale `window_index` for `display -c <client>`, so window results are
read from the session (`display -t =session:`).

### V2.9 — Visual cues instead of sounds for commands ✅ (2026-10-30)

The start/stop/error sounds after every wake word and command were
distracting. Voice commands are now silent. Dictation followed the same
day at the user's request: `Environment=WHISPER_STT_SOUNDS=0` in
`systemd/whisperd.service` (no code change; verified no `pw-play` runs on
a dictation start/stop).

- **Daemon:** no cue on entering `command` or on a command's `notice`.
  `state_changed` / `status` gain an optional `ok` (bool) on a notice that
  reports a voice command, so a UI can tell success from failure
  (backward compatible: omitted otherwise). Set in the same transition, so
  subscribers never see the notice without it.
- **Pill (DMS):** a ripple ring around the terminal icon while "Jarvis"
  waits; on the result, a green check that bounces or a red cross that
  shakes, with a matching tint, for 1.8 s. The card fades and slides in,
  shows the hints while waiting and then the result line, and fades out.

**Evidence:** 47 tests green, clippy, fmt clean. Live daemon,
`whisperctl run session knockout-mvp` (a no-op switch) → screenshot: green
check on the pill, card "tmux: knockout-mvp"; `whisperctl run switch to
session zzzqq` → red cross, card `No session like "zzzqq"`. Wake word via
the virtual mic → screenshot of the terminal icon with tint and ripple.

### Later, only if the grammar proves too rigid

A local LLM mapping free text onto the same `Intent` enum; other apps (niri
workspaces). Spoken replies (TTS) are not planned.

---

## Known limits

- Saying "Athena, next …" while *not* addressing it (e.g. in a huddle) can
  run a command: any sentence that starts with the name and contains a
  command word. Use `WHISPER_STT_WAKE=0` to turn the listener off.
- A false wake shows a notice such as "No session like …" and plays the
  error cue; nothing runs.
- The mic is open whenever you aren't dictating. Audio stays in memory
  (last ~8 s at most) and is never written to disk; transcripts are logged
  only with `WHISPER_STT_DEBUG=1`.

## Code reference

| File | Role |
|------|------|
| `crates/whisperd/src/wake.rs` | always-on listener, `Ear` utterance cutter |
| `crates/whisperd/src/command.rs` | wake phrase + intent parser |
| `crates/whisperd/src/tmux.rs` | tmux actions on the most recent client |
| `crates/whisperd/src/transcriber.rs` | "enter" / "stop dictation" during dictation |
| `crates/whisperd/src/injector.rs` | `press(Key)` |
| `crates/whisperd/src/focus.rs` | focused app via niri |
| `crates/whisperd/src/niri.rs` | desktop actions (focus, workspace, maximize) |
| `crates/whisperd/src/eval.rs` | `--eval-commands` accuracy test |
| `tests/fixtures/commands/phrases.tsv` | eval phrases; your `.wav` clips go next to it |
| `scripts/record-commands.sh` | records the clips |
| dotfiles `plugins/whisperStt/DictationWidget.qml` | pill + hint card |
| `crates/whisperd/src/joiner.rs` | `for_terminal` formatting |
| `crates/whisperd/src/main.rs` | `command` state, `wake()`, `run` verb |
| `crates/whisper-stt-proto/src/lib.rs` | `Command` state, `Run`, `hotwords` |
| `worker/whisper_worker.py` | passes `hotwords` |
