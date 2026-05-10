//! `parec` (PulseAudio compatibility) microphone capture backend.
//!
//! Adapted from `i3More/src/speech_text/parec.rs`. Two key deviations:
//!
//! - **Captures from the default source (mic), not a sink monitor.** i3More
//!   transcribes what is *played*; whisper-stt transcribes what is *spoken*.
//!   We drop `--device=<sink>.monitor` and let parec use the system default
//!   input. An optional `source` parameter targets a specific source name.
//!
//! - **Buffers PCM into a shared `Vec<i16>` (under `Mutex`) instead of a
//!   bounded channel.** Hold-to-talk dictation must not drop audio under
//!   back-pressure; we read everything for the duration of the press and
//!   hand it to the worker on release.
//!
//! Lifecycle: `Drop` SIGKILLs `parec` and joins the reader thread.

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

const READ_BUF_BYTES: usize = 8192;

pub struct ParecCapture {
    child: Child,
    reader: Option<JoinHandle<()>>,
    buf: Arc<Mutex<Vec<i16>>>,
    stopped: Arc<AtomicBool>,
    /// Fires once when the reader thread appends the first PCM samples.
    /// Crucial on PipeWire+Bluetooth where the source can take 1–2 s to
    /// resume from SUSPENDED. Consumed by `await_first_sample`.
    first_sample_rx: tokio::sync::Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
}

impl ParecCapture {
    /// Spawn `parec` against the system default source (or the named source if
    /// `source` is `Some(_)`) and start appending PCM samples to the internal
    /// buffer.
    pub fn start(source: Option<&str>) -> Result<Self, String> {
        let mut cmd = Command::new("parec");
        cmd.args(["--format=s16le", "--rate=16000", "--channels=1", "--raw"]);
        if let Some(src) = source {
            cmd.args(["--device", src]);
        }
        let mut child = cmd
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("spawn parec: {e}"))?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "parec stdout not piped".to_string())?;

        // Pre-allocate ~30 s of audio at 16 kHz mono S16 (≈ 1 MB) so typical
        // dictation presses do not realloc during capture.
        let buf: Arc<Mutex<Vec<i16>>> = Arc::new(Mutex::new(Vec::with_capacity(16_000 * 30)));
        let stopped = Arc::new(AtomicBool::new(false));

        // Wired up so the reader can notify us as soon as the first PCM
        // bytes arrive (which on Bluetooth/SUSPENDED sources can lag the
        // parec spawn by up to ~2 s).
        let (first_tx, first_rx) = tokio::sync::oneshot::channel::<()>();
        let first_tx_for_reader = Arc::new(Mutex::new(Some(first_tx)));

        let reader_buf = buf.clone();
        let reader_stopped = stopped.clone();
        let reader_first_tx = first_tx_for_reader.clone();
        let reader = thread::Builder::new()
            .name("parec-reader".into())
            .spawn(move || run_reader(stdout, reader_buf, reader_stopped, reader_first_tx))
            .map_err(|e| format!("spawn parec reader thread: {e}"))?;

        log::info!(
            "parec capture started (source={})",
            source.unwrap_or("<default>")
        );
        Ok(Self {
            child,
            reader: Some(reader),
            buf,
            stopped,
            first_sample_rx: tokio::sync::Mutex::new(Some(first_rx)),
        })
    }

    /// Shared accumulator the reader thread writes into. Cloning the `Arc`
    /// is cheap; locking the inner `Mutex<Vec<i16>>` gives a snapshot of
    /// every sample captured so far. Used by the segmenter to read
    /// in-progress audio without stopping capture.
    pub fn buffer(&self) -> Arc<Mutex<Vec<i16>>> {
        self.buf.clone()
    }

    /// Wait until the reader thread has appended at least one PCM chunk to
    /// the shared buffer. On a freshly-resumed PipeWire/Bluetooth source
    /// this is the dominant latency (up to ~2 s). Returns an error if the
    /// reader exits before any samples arrive (the daemon should treat
    /// this as a capture failure: mic muted, headset not in HSP profile,
    /// permission denied, etc.).
    pub async fn await_first_sample(&self, timeout: Duration) -> Result<(), String> {
        let mut guard = self.first_sample_rx.lock().await;
        let rx = guard
            .take()
            .ok_or_else(|| "await_first_sample called twice".to_string())?;
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(_)) => Err("parec exited before producing samples".into()),
            Err(_) => Err(format!(
                "no audio within {:?} (mic muted? source not in HSP/HFP profile?)",
                timeout
            )),
        }
    }

    /// Stop capture, return the accumulated PCM, and drop self.
    pub fn stop(mut self) -> Vec<i16> {
        self.stopped.store(true, Ordering::Relaxed);
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(h) = self.reader.take() {
            let _ = h.join();
        }
        let mut guard = self.buf.lock().expect("parec buf mutex poisoned");
        std::mem::take(&mut *guard)
    }
}

impl Drop for ParecCapture {
    fn drop(&mut self) {
        if !self.stopped.load(Ordering::Relaxed) {
            self.stopped.store(true, Ordering::Relaxed);
            let _ = self.child.kill();
            let _ = self.child.wait();
            if let Some(h) = self.reader.take() {
                let _ = h.join();
            }
            log::info!("parec capture dropped without explicit stop");
        }
    }
}

fn run_reader<R: Read>(
    mut stdout: R,
    buf: Arc<Mutex<Vec<i16>>>,
    stopped: Arc<AtomicBool>,
    first_tx: Arc<Mutex<Option<tokio::sync::oneshot::Sender<()>>>>,
) {
    let mut bytes = [0u8; READ_BUF_BYTES];
    loop {
        if stopped.load(Ordering::Relaxed) {
            return;
        }
        let n = match stdout.read(&mut bytes) {
            Ok(0) => {
                log::info!("parec stdout EOF; reader exiting");
                return;
            }
            Ok(n) => n,
            Err(e) => {
                log::error!("parec stdout read: {e}");
                return;
            }
        };
        let mut samples = Vec::with_capacity(n / 2);
        let mut i = 0;
        while i + 1 < n {
            samples.push(i16::from_le_bytes([bytes[i], bytes[i + 1]]));
            i += 2;
        }
        if samples.is_empty() {
            continue;
        }
        if let Ok(mut guard) = buf.lock() {
            guard.extend_from_slice(&samples);
        }
        // Notify await_first_sample once, on the first non-empty append.
        // After this the Option is None and the lock contention disappears.
        if let Ok(mut tx_slot) = first_tx.lock() {
            if let Some(tx) = tx_slot.take() {
                let _ = tx.send(());
            }
        }
    }
}
