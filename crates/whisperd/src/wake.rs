//! Always-on wake word: "Jarvis, switch to session main".
//!
//! While no dictation is running, a second capture stays open. [`Ear`]
//! cuts it into utterances with the same voice tracker dictation uses;
//! utterances short enough to be a command (up to 8 s) are decoded by a
//! small CPU model (`tiny.en`, ~0.6 s per 2 s clip) biased towards the wake
//! word, command words and the live tmux session names, then parsed by
//! `command` for the focused app.
//!
//! - wake phrase and command in one breath: run it, show the result
//! - wake phrase alone: enter `command` state (start cue) and treat the next
//!   utterance within [`COMMAND_WINDOW`] as the command
//! - "start dictation" hands over to a normal dictation session
//!
//! Dictation pauses this listener: its capture closes while a session runs,
//! so dictated text can never trigger a command.

use std::time::{Duration, Instant};

use tokio::sync::{mpsc, watch};
use whisper_stt_proto::{DaemonState, DaemonStatus};

use crate::audio::{self, AudioConfig, CaptureSession};
use crate::command::{self, Intent, Parsed};
use crate::ipc::InternalCmd;
use crate::voice::VoiceTracker;
use crate::worker::{encode_pcm_le, WorkerConfig, WorkerHandle};
use crate::{focus, injector, niri, tmux, transcriber};

/// After a lone wake phrase, how long to wait for the command.
const COMMAND_WINDOW: Duration = Duration::from_secs(6);
const TICK: Duration = Duration::from_millis(50);

/// What the listener tells the daemon loop.
#[derive(Debug)]
pub enum WakeEvent {
    /// Wake word alone: listening for the command. Carries the focused
    /// app, so the bar can hint at the commands that fit it.
    Heard(focus::App),
    /// A command ran (`Ok` result text) or was refused (`Err` reason).
    Done(Result<String, String>),
    /// No command followed the wake phrase.
    Expired,
    /// "Jarvis, start dictation".
    StartDictation,
}

/// Run an intent that needs nothing from the daemon. `StartDictation`
/// changes daemon state, so callers handle it before getting here.
pub async fn perform(intent: &Intent) -> Result<String, String> {
    match intent {
        Intent::Tmux(action) => tmux::run(action).await,
        Intent::Niri(action) => niri::run(action).await,
        Intent::Browser(action) => {
            if focus::focused().await != focus::App::Browser {
                return Err("Not in the browser".into());
            }
            injector::browser(action)
                .await
                .map(|()| format!("Zen: {action}"))
                .map_err(|e| format!("{e:#}"))
        }
        Intent::Key(key) => {
            if key.terminal_only() && focus::focused().await != focus::App::Terminal {
                return Err(format!("{key}: not in a terminal"));
            }
            injector::press(*key)
                .await
                .map(|()| format!("Key: {key}"))
                .map_err(|e| format!("{e:#}"))
        }
        Intent::StartDictation => Err("start dictation needs the daemon".into()),
    }
}

/// `WHISPER_STT_WAKE=0` turns the listener off.
pub fn enabled() -> bool {
    !std::env::var("WHISPER_STT_WAKE").is_ok_and(|v| v == "0")
}

pub fn spawn(
    audio: AudioConfig,
    cmd_tx: mpsc::Sender<InternalCmd>,
    status: watch::Receiver<DaemonStatus>,
) {
    tokio::spawn(async move {
        if let Err(e) = run(audio, cmd_tx, status).await {
            log::error!("wake listener stopped: {e:#}");
        }
    });
}

async fn run(
    audio: AudioConfig,
    cmd_tx: mpsc::Sender<InternalCmd>,
    status: watch::Receiver<DaemonStatus>,
) -> anyhow::Result<()> {
    let cfg = WorkerConfig::wake_from_env()?;
    let mut worker = WorkerHandle::spawn(&cfg).await?;
    log::info!("wake listener on (model {})", cfg.model);
    let send = |ev| {
        let tx = cmd_tx.clone();
        async move {
            let _ = tx.send(InternalCmd::Wake(ev)).await;
        }
    };

    let mut capture: Option<CaptureSession> = None;
    let mut ear = Ear::default();
    let mut awaiting: Option<Instant> = None;
    let mut tick = tokio::time::interval(TICK);
    loop {
        tick.tick().await;
        let state = status.borrow().state;
        if !matches!(
            state,
            DaemonState::Idle | DaemonState::Notice | DaemonState::Error | DaemonState::Command
        ) {
            // Dictation owns the mic: stop listening and forget partial audio.
            if let Some(c) = capture.take() {
                drop(c.stop());
                ear = Ear::default();
                awaiting = None;
            }
            continue;
        }
        let Some(cap) = &capture else {
            match CaptureSession::start(&audio).await {
                Ok(c) => capture = Some(c),
                Err(e) => {
                    log::warn!("wake listener: {e:#}; retrying in 5 s");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
            continue;
        };

        if awaiting.is_some_and(|t| t.elapsed() > COMMAND_WINDOW) && !ear.in_utterance() {
            awaiting = None;
            send(WakeEvent::Expired).await;
        }

        let new = std::mem::take(&mut *cap.buffer().lock().expect("capture buffer poisoned"));
        let Some(utterance) = ear.push(&new) else {
            continue;
        };

        let sessions = tmux::sessions().await;
        let hotwords = format!(
            "Hey Jarvis, tmux session window pane split zoom tab browser terminal workspace maximize {}",
            sessions.join(" ")
        );
        let pcm = encode_pcm_le(&utterance);
        let text = match worker
            .transcribe(pcm, audio::SAMPLE_RATE, Some(hotwords))
            .await
        {
            Ok(r) => {
                transcriber::log_transcript("wake", &r);
                r.text
            }
            Err(e) => {
                log::warn!("wake decode failed: {e:#}; restarting the wake model");
                worker = WorkerHandle::spawn(&cfg).await?;
                continue;
            }
        };

        let app = focus::focused().await;
        match command::parse(&text, &sessions, awaiting.is_none(), app) {
            Parsed::NotWake => {}
            Parsed::WakeOnly => {
                awaiting = Some(Instant::now());
                send(WakeEvent::Heard(app)).await;
            }
            Parsed::Command(intent) => {
                awaiting = None;
                log::info!("voice command: {intent:?}");
                if intent == Intent::StartDictation {
                    send(WakeEvent::StartDictation).await;
                } else {
                    send(WakeEvent::Done(perform(&intent).await)).await;
                }
            }
            Parsed::Unknown(why) => {
                awaiting = None;
                send(WakeEvent::Done(Err(why))).await;
            }
        }
    }
}

/// Voice must start this long before the tracker confirms it (its sustain
/// time), plus a little lead-in so the first syllable isn't clipped.
const LEAD_IN_MS: u64 = 200 + 300;
/// A pause this long ends an utterance.
const END_PAUSE_MS: u64 = 600;
/// Longer than this is conversation, not a command: not decoded at all.
const MAX_UTTERANCE_MS: u64 = 8_000;
const SAMPLES_PER_MS: u64 = 16;

/// Cuts a live capture into utterances.
#[derive(Default)]
pub struct Ear {
    tracker: VoiceTracker,
    /// Recent audio; `audio[0]` is stream sample `base`.
    audio: Vec<i16>,
    base: u64,
    /// Last voice position already accounted for.
    seen_voice: Option<u64>,
    /// Start (ms) of the utterance in progress.
    start_ms: Option<u64>,
    too_long: bool,
}

impl Ear {
    pub fn in_utterance(&self) -> bool {
        self.start_ms.is_some()
    }

    /// Feed captured audio; returns an utterance once it has ended.
    pub fn push(&mut self, pcm: &[i16]) -> Option<Vec<i16>> {
        self.tracker.push(pcm);
        self.audio.extend_from_slice(pcm);
        let now = self.tracker.position_ms();
        let voice = self.tracker.last_voice_ms();
        let fresh = voice.is_some() && voice != self.seen_voice;
        self.seen_voice = voice;

        let mut out = None;
        match self.start_ms {
            None if fresh => {
                self.start_ms = Some(voice.unwrap_or(0).saturating_sub(LEAD_IN_MS));
            }
            Some(start) => {
                let last = voice.unwrap_or(start);
                if now.saturating_sub(last) >= END_PAUSE_MS {
                    if !self.too_long {
                        out = Some(self.slice(start, last + 200));
                    }
                    self.start_ms = None;
                    self.too_long = false;
                } else if now.saturating_sub(start) > MAX_UTTERANCE_MS {
                    self.too_long = true;
                }
            }
            None => {}
        }

        // Keep only what a future utterance could need.
        let keep_from_ms = match self.start_ms {
            Some(start) if !self.too_long => start,
            _ => now.saturating_sub(LEAD_IN_MS + 100),
        };
        let drop = (keep_from_ms * SAMPLES_PER_MS).saturating_sub(self.base) as usize;
        let drop = drop.min(self.audio.len());
        self.audio.drain(..drop);
        self.base += drop as u64;
        out
    }

    fn slice(&self, from_ms: u64, to_ms: u64) -> Vec<i16> {
        let at = |ms: u64| {
            ((ms * SAMPLES_PER_MS).saturating_sub(self.base) as usize).min(self.audio.len())
        };
        self.audio[at(from_ms)..at(to_ms)].to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::voice::tests::{silence, tone};

    /// Feed in 50 ms chunks like the live tick; collect utterance lengths (ms).
    fn hear(parts: &[Vec<i16>]) -> Vec<u64> {
        let mut ear = Ear::default();
        let all: Vec<i16> = parts.concat();
        all.chunks(800)
            .filter_map(|c| ear.push(c))
            .map(|u| u.len() as u64 / SAMPLES_PER_MS)
            .collect()
    }

    #[test]
    fn silence_has_no_utterances() {
        assert!(hear(&[silence(5_000)]).is_empty());
    }

    #[test]
    fn a_short_phrase_is_one_utterance_with_its_start() {
        let got = hear(&[silence(1_000), tone(1_500, 300.0, 0.1), silence(1_000)]);
        assert_eq!(got.len(), 1, "{got:?}");
        // Speech plus up to ~300 ms of lead-in and the tail.
        assert!((1_500..2_200).contains(&got[0]), "{got:?}");
    }

    #[test]
    fn two_phrases_with_a_pause_are_two_utterances() {
        let got = hear(&[
            silence(1_000),
            tone(800, 300.0, 0.1),
            silence(1_000),
            tone(1_500, 300.0, 0.1),
            silence(1_000),
        ]);
        assert_eq!(got.len(), 2, "{got:?}");
    }

    #[test]
    fn long_talk_is_not_a_command_and_memory_stays_bounded() {
        let mut ear = Ear::default();
        let talk = [silence(1_000), tone(20_000, 300.0, 0.1), silence(1_000)].concat();
        let mut got = Vec::new();
        for c in talk.chunks(800) {
            got.extend(ear.push(c));
            assert!(ear.audio.len() <= (MAX_UTTERANCE_MS as usize + 1_000) * 16);
        }
        assert!(got.is_empty());
    }
}
