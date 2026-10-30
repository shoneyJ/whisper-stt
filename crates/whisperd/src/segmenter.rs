//! Watches the live capture while listening.
//!
//! Every [`TICK`] it measures the new audio with the [`VoiceTracker`] and:
//!
//! - **commits a piece** once voice has paused for [`PAUSE_MS`]: the audio
//!   since the last commit goes to the transcriber, so text appears clause
//!   by clause while you keep talking
//! - **publishes the mic level** for the bar pill (~20 Hz)
//! - **asks the daemon to auto-stop** once no voice has been heard for the
//!   silence timeout. Measured in captured audio, not wall-clock time, so a
//!   stalled device is never mistaken for a quiet speaker (as in Myna's
//!   `silence-timeout`)
//!
//! Only audio that contained voice is ever committed; the tail after the
//! final toggle is handled by the daemon with [`Outcome::voice_in_tail`].

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{mpsc, oneshot, watch};

use crate::ipc::{InternalCmd, Level};
use crate::transcriber::Job;
use crate::voice::{self, VoiceTracker};

/// 50 ms: the level meter updates at ~20 Hz.
const TICK: Duration = Duration::from_millis(50);
/// Silence after voice that ends a piece. Long enough for a breath between
/// words not to split a sentence, short enough to feel live.
const PAUSE_MS: u64 = 500;
/// The level covers the last 100 ms of audio.
const LEVEL_SAMPLES: usize = 1_600;

pub struct SegmenterHandle {
    stop_tx: oneshot::Sender<()>,
    join: tokio::task::JoinHandle<Outcome>,
}

/// What the segmenter knew when it stopped.
pub struct Outcome {
    tracker: VoiceTracker,
    /// Samples already fed to `tracker`.
    measured: usize,
    /// Samples already committed as pieces.
    pub committed: usize,
}

impl Outcome {
    /// Measure the audio captured after the last tick, then report whether
    /// the uncommitted tail contains voice.
    pub fn voice_in_tail(&mut self, pcm: &[i16]) -> bool {
        if self.measured < pcm.len() {
            self.tracker.push(&pcm[self.measured..]);
            self.measured = pcm.len();
        }
        let committed_ms = self.committed as u64 / 16;
        self.tracker
            .last_voice_ms()
            .is_some_and(|v| v > committed_ms)
    }
}

impl SegmenterHandle {
    pub async fn stop(self) -> Outcome {
        let _ = self.stop_tx.send(());
        self.join.await.expect("segmenter task panicked")
    }
}

pub struct Wiring {
    pub buf: Arc<Mutex<Vec<i16>>>,
    pub jobs: mpsc::UnboundedSender<Job>,
    pub cmd_tx: mpsc::Sender<InternalCmd>,
    pub level: Arc<watch::Sender<Level>>,
    /// `None` disables auto-stop.
    pub silence_timeout: Option<Duration>,
}

pub fn spawn(w: Wiring) -> SegmenterHandle {
    let (stop_tx, stop_rx) = oneshot::channel();
    let join = tokio::spawn(run(w, stop_rx));
    SegmenterHandle { stop_tx, join }
}

async fn run(w: Wiring, mut stop_rx: oneshot::Receiver<()>) -> Outcome {
    let mut out = Outcome {
        tracker: VoiceTracker::default(),
        measured: 0,
        committed: 0,
    };
    let mut asked_to_stop = false;
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            biased;
            _ = &mut stop_rx => break,
            _ = tick.tick() => {}
        }

        let (fresh, recent, total) = {
            let buf = w.buf.lock().expect("capture buffer mutex poisoned");
            let total = buf.len();
            let fresh = buf[out.measured.min(total)..].to_vec();
            let recent = voice::level(&buf[total.saturating_sub(LEVEL_SAMPLES)..]);
            (fresh, recent, total)
        };
        out.tracker.push(&fresh);
        out.measured = total;
        let _ = w.level.send(Level {
            rms: recent.0,
            peak: recent.1,
        });

        let position = out.tracker.position_ms();
        let committed_ms = out.committed as u64 / 16;
        let last_voice = out.tracker.last_voice_ms();

        let paused_after_voice =
            last_voice.is_some_and(|v| v > committed_ms && position.saturating_sub(v) >= PAUSE_MS);
        if paused_after_voice && total - out.committed >= crate::transcriber::MIN_DECODE_SAMPLES {
            let piece =
                w.buf.lock().expect("capture buffer mutex poisoned")[out.committed..total].to_vec();
            log::info!(
                "segmenter: piece of {} ms at a pause",
                piece.len() as u64 / 16
            );
            out.committed = total;
            if w.jobs.send(Job::Segment(piece)).is_err() {
                log::warn!("segmenter: transcriber gone");
                break;
            }
        }

        if let (Some(limit), false) = (w.silence_timeout, asked_to_stop) {
            let quiet_ms = position.saturating_sub(last_voice.unwrap_or(0));
            if quiet_ms >= limit.as_millis() as u64 {
                asked_to_stop = true;
                log::info!("segmenter: {} s without voice; auto-stop", quiet_ms / 1000);
                let _ = w.cmd_tx.send(InternalCmd::AutoStop).await;
            }
        }
    }
    let _ = w.level.send(Level::default());
    out
}
