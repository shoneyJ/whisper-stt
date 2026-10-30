//! Owns the Whisper worker and types results, one job at a time.
//!
//! Before this task existed, the daemon loop transcribed inline, so a
//! toggle pressed during a long transcription waited behind it and pieces
//! queued by the segmenter could be typed *after* the final tail. Now:
//!
//! - jobs run in arrival order on one queue, so pieces always land before
//!   the tail that follows them
//! - the daemon loop never waits on the model and stays responsive
//! - nothing too short or without voice reaches Whisper: on sub-second
//!   near-silence it invents text ("Thank you."), so those are skipped
//!   (Myna skips tails under 0.3 s the same way, `MIN_DECODE_S`)
//!
//! The worker is started when a session begins, in parallel with the mic
//! warming up, and unloaded after the idle timeout to free the GPU.

use std::time::{Duration, Instant};

use tokio::sync::mpsc;

use crate::command::{self, Spoken};
use crate::ipc::InternalCmd;
use crate::joiner::{self, Joiner};
use crate::voice::VoiceTracker;
use crate::worker::{encode_pcm_le, TranscribeResult, WorkerConfig, WorkerHandle};
use crate::{audio, focus, injector};

/// 0.3 s at 16 kHz. Shorter audio is never decoded.
pub const MIN_DECODE_SAMPLES: usize = 4_800;

const IDLE_CHECK: Duration = Duration::from_secs(30);

pub enum Job {
    /// A session starts: load the model if needed, reset spacing.
    Begin,
    /// A piece the segmenter cut at a pause (always contains voice).
    Segment(Vec<i16>),
    /// The rest of the session after the final toggle.
    Tail { pcm: Vec<i16>, voice: bool },
    /// The session ended without a tail (cancelled while starting, or failed).
    Cancel,
}

/// True when standalone audio is worth sending to Whisper: long enough and
/// containing sustained voice. Used by the dev `--transcribe/--record` paths;
/// live sessions already know from the segmenter's tracker.
pub fn worth_decoding(pcm: &[i16]) -> bool {
    if pcm.len() < MIN_DECODE_SAMPLES {
        return false;
    }
    let mut tracker = VoiceTracker::default();
    tracker.push(pcm);
    tracker.last_voice_ms().is_some()
}

/// Transcript text only reaches the log with `WHISPER_STT_DEBUG=1`, so a
/// normal journal never holds what you dictated (Myna's two-tier logging).
pub fn log_transcript(what: &str, r: &TranscribeResult) {
    let (ms, lang) = (r.duration_ms, &r.language);
    if std::env::var("WHISPER_STT_DEBUG").is_ok_and(|v| v == "1") {
        log::info!("{what} transcript ({ms} ms, {lang}): {:?}", r.text);
    } else {
        log::info!(
            "{what} transcript ({ms} ms, {lang}): {} chars",
            r.text.chars().count()
        );
    }
}

pub fn spawn(
    cfg: WorkerConfig,
    idle_timeout: Duration,
    events: mpsc::Sender<InternalCmd>,
) -> mpsc::UnboundedSender<Job> {
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(run(cfg, idle_timeout, rx, events));
    tx
}

struct Session {
    joiner: Joiner,
    typed: bool,
    /// Why the model couldn't load; later jobs in this session are skipped.
    failed: Option<String>,
}

async fn run(
    cfg: WorkerConfig,
    idle_timeout: Duration,
    mut jobs: mpsc::UnboundedReceiver<Job>,
    events: mpsc::Sender<InternalCmd>,
) {
    let mut worker: Option<WorkerHandle> = None;
    let mut session: Option<Session> = None;
    let mut last_use = Instant::now();
    let mut idle = tokio::time::interval(IDLE_CHECK);
    idle.tick().await;

    loop {
        let job = tokio::select! {
            job = jobs.recv() => match job {
                Some(job) => job,
                None => break,
            },
            _ = idle.tick() => {
                if session.is_none() && worker.is_some() && last_use.elapsed() >= idle_timeout {
                    log::info!("idle for {} s; unloading worker", last_use.elapsed().as_secs());
                    if let Some(w) = worker.take() {
                        if let Err(e) = w.shutdown().await {
                            log::warn!("worker shutdown during idle unload: {e:#}");
                        }
                    }
                    let _ = events.send(InternalCmd::WorkerLoaded(false)).await;
                }
                continue;
            }
        };

        match job {
            Job::Begin => {
                let mut s = Session {
                    joiner: Joiner::default(),
                    typed: false,
                    failed: None,
                };
                if worker.is_none() {
                    log::info!("loading worker");
                    match WorkerHandle::spawn(&cfg).await {
                        Ok(w) => {
                            worker = Some(w);
                            let _ = events.send(InternalCmd::WorkerLoaded(true)).await;
                        }
                        Err(e) => {
                            let msg = format!("{e:#}");
                            log::error!("worker failed to load: {msg}");
                            s.failed = Some(msg.clone());
                            let _ = events.send(InternalCmd::WorkerFailed(msg)).await;
                        }
                    }
                }
                session = Some(s);
            }
            Job::Segment(pcm) => {
                if let (Some(s), Some(w)) = (session.as_mut(), worker.as_mut()) {
                    if s.failed.is_none() && pcm.len() >= MIN_DECODE_SAMPLES {
                        if let Err(e) = decode_and_type(w, s, &pcm, "piece", &events).await {
                            log::warn!("piece failed (continuing): {e:#}");
                        }
                    }
                }
            }
            Job::Tail { pcm, voice } => {
                let result = match (session.as_mut(), worker.as_mut()) {
                    (Some(s), _) if s.failed.is_some() => Err(s.failed.clone().unwrap()),
                    (Some(s), Some(w)) => {
                        if voice && pcm.len() >= MIN_DECODE_SAMPLES {
                            decode_and_type(w, s, &pcm, "tail", &events)
                                .await
                                .map(|()| s.typed)
                                .map_err(|e| format!("{e:#}"))
                        } else {
                            log::info!(
                                "tail of {} ms skipped (voice={voice})",
                                pcm.len() as u64 / 16
                            );
                            Ok(s.typed)
                        }
                    }
                    _ => Err("no worker for this session".to_string()),
                };
                session = None;
                last_use = Instant::now();
                let _ = events.send(InternalCmd::TailDone(result)).await;
            }
            Job::Cancel => {
                session = None;
                last_use = Instant::now();
            }
        }
    }
    if let Some(w) = worker.take() {
        if let Err(e) = w.shutdown().await {
            log::warn!("worker shutdown on quit: {e:#}");
        }
    }
}

async fn decode_and_type(
    worker: &mut WorkerHandle,
    session: &mut Session,
    pcm: &[i16],
    what: &str,
    events: &mpsc::Sender<InternalCmd>,
) -> anyhow::Result<()> {
    let result = worker
        .transcribe(encode_pcm_le(pcm), audio::SAMPLE_RATE, None)
        .await?;
    log_transcript(what, &result);
    let terminal = focus::focused().await == focus::App::Terminal;
    // A key name or "stop listening" said alone acts instead of being typed.
    // Keys other than Enter only in a terminal: elsewhere "Cancel." is text.
    match command::spoken_key(&result.text) {
        Some(Spoken::Key(key)) if terminal || !key.terminal_only() => {
            injector::press(key).await?;
            session.joiner = Joiner::default(); // fresh input: no leading space
            session.typed = true;
            return Ok(());
        }
        Some(Spoken::StopDictation) => {
            let _ = events.send(InternalCmd::AutoStop).await;
            return Ok(());
        }
        _ => {}
    }
    let text = if terminal {
        joiner::for_terminal(&result.text)
    } else {
        result.text.clone()
    };
    if let Some(text) = session.joiner.next(&text) {
        injector::type_text(&text).await?;
        session.typed = true;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::voice::tests::{silence, tone};

    #[test]
    fn short_audio_is_not_worth_decoding() {
        assert!(!worth_decoding(&tone(200, 300.0, 0.2)));
    }

    #[test]
    fn silence_is_not_worth_decoding() {
        assert!(!worth_decoding(&silence(3_000)));
    }

    #[test]
    fn audio_with_voice_is_worth_decoding() {
        let mut pcm = silence(300);
        pcm.extend(tone(1_000, 300.0, 0.1));
        assert!(worth_decoding(&pcm));
    }
}
