//! Audio capture for whisper-stt.
//!
//! v1 keeps this deliberately small: a single `CaptureSession` that runs
//! `parec` against the default microphone, buffers PCM in memory for the
//! duration of the press, and returns the buffer on release. No
//! sink-monitor switching, no PipeWire backend, no rolling window — those
//! are deferred per `doc/plan/03-audio-pipeline.md`.

mod parec;

use anyhow::{Context, Result};

pub use parec::ParecCapture;

/// Sample rate the worker (faster-whisper) expects.
pub const SAMPLE_RATE: u32 = 16_000;

/// Optional knobs. v1 has only one knob; expansion lives in M5+.
#[derive(Debug, Clone, Default)]
pub struct AudioConfig {
    /// Specific PulseAudio source name. `None` = system default (whatever the
    /// audio control panel selects).
    pub source: Option<String>,
}

impl AudioConfig {
    pub fn from_env() -> Self {
        Self {
            source: std::env::var("WHISPER_STT_SOURCE").ok(),
        }
    }
}

/// Maximum we'll wait for parec to produce its first PCM sample. Bluetooth
/// HSP/HFP source resume from SUSPENDED is the slowest case; ~2 s in
/// practice on this hardware. 3 s gives headroom without making the user
/// stare at a stuck pill forever if something is genuinely wrong.
const FIRST_SAMPLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// One press → one CaptureSession → one PCM buffer.
pub struct CaptureSession {
    capture: ParecCapture,
}

impl CaptureSession {
    /// Start parec and **wait until it actually produces samples**. The
    /// daemon broadcasts `state_changed → listening` only after this
    /// returns, so the indicator pill is a reliable cue that the mic is
    /// live (not just that the F9 keybind fired).
    pub async fn start(config: &AudioConfig) -> Result<Self> {
        let capture = ParecCapture::start(config.source.as_deref())
            .map_err(anyhow::Error::msg)
            .context("start parec capture")?;
        let warmup_start = std::time::Instant::now();
        capture
            .await_first_sample(FIRST_SAMPLE_TIMEOUT)
            .await
            .map_err(anyhow::Error::msg)
            .context("parec did not produce audio")?;
        let elapsed = warmup_start.elapsed();
        if elapsed.as_millis() > 200 {
            log::info!(
                "parec source warm-up took {} ms (typical Bluetooth HSP wake)",
                elapsed.as_millis()
            );
        }
        Ok(Self { capture })
    }

    /// Shared buffer the reader thread writes into. The segmenter clones
    /// this `Arc` and reads in-progress audio while capture is still live.
    pub fn buffer(&self) -> std::sync::Arc<std::sync::Mutex<Vec<i16>>> {
        self.capture.buffer()
    }

    /// Stop capture and return the accumulated PCM. Sample rate is fixed at
    /// [`SAMPLE_RATE`].
    pub fn stop(self) -> Vec<i16> {
        self.capture.stop()
    }
}
