//! Streaming segmenter for hold-to-talk dictation.
//!
//! While the daemon is in `Listening` state, this task watches the live
//! parec buffer, runs the existing energy-based VAD on a rolling 500 ms
//! window, and submits a transcribe-and-inject job each time the speaker
//! takes a breath (≥ ~400 ms of silence after speech). The actor task
//! handles the transcribe + xdotool inject; we stay decoupled from the
//! worker via the same mpsc channel that carries IPC commands.
//!
//! The final tail of audio after F9 release is **not** flushed by the
//! segmenter — the actor's `handle_stop` does that directly, using the
//! shared `last_committed_pos` to know where to start.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};

use crate::ipc::InternalCmd;
use crate::vad;

/// How often the segmenter polls the buffer.
const TICK_INTERVAL: Duration = Duration::from_millis(100);

/// Length of the rolling window the VAD inspects. 500 ms is short enough
/// to detect natural pauses, long enough to be stable against per-chunk
/// noise.
const VAD_WINDOW_SAMPLES: usize = 8_000; // 500 ms @ 16 kHz

/// Don't commit a segment shorter than this — avoids a flurry of empty
/// transcribes when the user holds F9 silently.
const MIN_SEGMENT_SAMPLES: usize = 4_800; // 300 ms

pub struct SegmenterHandle {
    stop_tx: Option<oneshot::Sender<()>>,
    join: tokio::task::JoinHandle<()>,
    /// Where the segmenter has committed up to. The actor reads this in
    /// `handle_stop` to figure out which tail of the buffer still needs
    /// to be transcribed as the final segment.
    pub last_committed: Arc<AtomicUsize>,
}

impl SegmenterHandle {
    pub async fn stop(mut self) {
        if let Some(tx) = self.stop_tx.take() {
            let _ = tx.send(());
        }
        let _ = self.join.await;
    }
}

/// Spawn the segmenter task. `buf` is the shared parec accumulator;
/// `cmd_tx` is the actor's command channel — segments are submitted via
/// `InternalCmd::TranscribeSegment`.
pub fn spawn(
    buf: Arc<Mutex<Vec<i16>>>,
    cmd_tx: mpsc::Sender<InternalCmd>,
) -> SegmenterHandle {
    let (stop_tx, stop_rx) = oneshot::channel::<()>();
    let last_committed = Arc::new(AtomicUsize::new(0));
    let last_committed_for_task = last_committed.clone();

    let join = tokio::spawn(async move {
        run(buf, cmd_tx, stop_rx, last_committed_for_task).await;
    });

    SegmenterHandle {
        stop_tx: Some(stop_tx),
        join,
        last_committed,
    }
}

async fn run(
    buf: Arc<Mutex<Vec<i16>>>,
    cmd_tx: mpsc::Sender<InternalCmd>,
    mut stop_rx: oneshot::Receiver<()>,
    last_committed: Arc<AtomicUsize>,
) {
    let mut tick = tokio::time::interval(TICK_INTERVAL);
    tick.tick().await; // skip immediate first tick
    let mut in_speech = false;

    loop {
        tokio::select! {
            biased;
            _ = &mut stop_rx => {
                log::debug!("segmenter: stop signal received");
                return;
            }
            _ = tick.tick() => {
                if let Some(segment) = check_for_commit(&buf, &last_committed, &mut in_speech) {
                    log::info!(
                        "segmenter: committing {} samples ({} ms) at pause",
                        segment.len(),
                        segment.len() * 1000 / 16_000
                    );
                    if cmd_tx
                        .send(InternalCmd::TranscribeSegment(segment))
                        .await
                        .is_err()
                    {
                        log::warn!("segmenter: actor channel closed; exiting");
                        return;
                    }
                }
            }
        }
    }
}

/// Inspect the latest VAD window. If we just transitioned from speech to
/// silence and have ≥ MIN_SEGMENT_SAMPLES of un-committed audio, return
/// the segment; else return None and update the speech flag.
fn check_for_commit(
    buf: &Arc<Mutex<Vec<i16>>>,
    last_committed: &Arc<AtomicUsize>,
    in_speech: &mut bool,
) -> Option<Vec<i16>> {
    let guard = buf.lock().expect("segmenter buf mutex poisoned");
    let total = guard.len();

    if total < VAD_WINDOW_SAMPLES {
        return None; // not enough audio yet
    }

    let win_start = total - VAD_WINDOW_SAMPLES;
    let window: Vec<f32> = guard[win_start..total]
        .iter()
        .map(|&s| s as f32 / 32768.0)
        .collect();

    let is_silent = vad::is_chunk_silent(
        &window,
        16_000,
        vad::DEFAULT_FREQ_THOLD_HZ,
        vad::DEFAULT_SILENCE_ENERGY_THOLD,
    );

    let prev = *in_speech;
    *in_speech = !is_silent;

    if !(prev && is_silent) {
        return None; // not a speech→silence transition
    }

    // Speech just ended. Commit from last_committed to current total.
    let committed = last_committed.load(Ordering::Relaxed);
    if total <= committed || total - committed < MIN_SEGMENT_SAMPLES {
        return None;
    }
    let segment = guard[committed..total].to_vec();
    last_committed.store(total, Ordering::Relaxed);
    Some(segment)
}
