use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use tokio::net::UnixListener;
use tokio::sync::mpsc;
use whisper_stt_proto::{CtlCommand, CtlResponse, DaemonState, DaemonStatus};

mod audio;
mod indicator;
mod injector;
mod ipc;
mod segmenter;
mod vad;
mod worker;

const IDLE_TICK_INTERVAL: Duration = Duration::from_secs(30);

fn idle_timeout() -> Duration {
    std::env::var("WHISPER_STT_IDLE_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .map(Duration::from_secs)
        .unwrap_or(Duration::from_secs(300))
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("--transcribe") => {
            let path: PathBuf = args
                .next()
                .context("--transcribe requires a WAV path")?
                .into();
            run_transcribe(&path).await
        }
        Some("--record") => {
            let secs: u64 = args
                .next()
                .context("--record requires a duration in seconds")?
                .parse()
                .context("parse --record duration")?;
            run_record(secs).await
        }
        Some("--help" | "-h") => {
            print_help();
            Ok(())
        }
        Some(other) => bail!("unknown flag: {other:?} (try --help)"),
        None => run_daemon().await,
    }
}

fn print_help() {
    println!(
        "whisperd — whisper-stt daemon

USAGE:
    whisperd                       start the IPC server (default)
    whisperd --transcribe <wav>    dev: transcribe a 16 kHz mono WAV
    whisperd --record <secs>       dev: capture <secs> from the mic, transcribe, print

ENV (worker):
    WHISPER_STT_PYTHON         override the venv python
    WHISPER_STT_WORKER_SCRIPT  override the worker script path
    WHISPER_STT_MODEL          default: distil-large-v3
    WHISPER_STT_DEVICE         default: cuda
    WHISPER_STT_COMPUTE_TYPE   default: float16
    WHISPER_STT_LANGUAGE       default: en
    WHISPER_STT_BEAM_SIZE      default: 5

ENV (audio):
    WHISPER_STT_SOURCE         optional PulseAudio source name (default: system default)

ENV (lifecycle):
    WHISPER_STT_IDLE_TIMEOUT_SECS  default: 300 (worker unloaded after this)
    WHISPER_STT_SOCK               override the IPC socket path
"
    );
}

// -------- dev paths (M2/M3) --------

async fn run_transcribe(wav: &Path) -> Result<()> {
    let pcm = read_wav_mono_16k(wav)?;
    log::info!(
        "decoded {} samples ({} ms)",
        pcm.len(),
        pcm.len() * 1000 / 16_000
    );
    let pcm_b64 = worker::encode_pcm_le(&pcm);
    let cfg = worker::WorkerConfig::from_env()?;
    let mut handle = worker::WorkerHandle::spawn(&cfg).await?;
    let result = handle.transcribe(pcm_b64, audio::SAMPLE_RATE).await?;
    println!("{}", result.text);
    log::info!(
        "transcript ({} ms, language={}): {:?}",
        result.duration_ms,
        result.language,
        result.text
    );
    handle.shutdown().await?;
    Ok(())
}

async fn run_record(secs: u64) -> Result<()> {
    let audio_cfg = audio::AudioConfig::from_env();
    let session = audio::CaptureSession::start(&audio_cfg).await?;
    log::info!("recording {secs} s from default source — speak now");
    tokio::time::sleep(Duration::from_secs(secs)).await;
    let pcm = session.stop();
    log::info!(
        "captured {} samples ({} ms)",
        pcm.len(),
        pcm.len() * 1000 / 16_000
    );
    let pcm_b64 = worker::encode_pcm_le(&pcm);
    let cfg = worker::WorkerConfig::from_env()?;
    let mut handle = worker::WorkerHandle::spawn(&cfg).await?;
    let result = handle.transcribe(pcm_b64, audio::SAMPLE_RATE).await?;
    println!("{}", result.text);
    log::info!(
        "transcript ({} ms, language={}): {:?}",
        result.duration_ms,
        result.language,
        result.text
    );
    handle.shutdown().await?;
    Ok(())
}

fn read_wav_mono_16k(path: &Path) -> Result<Vec<i16>> {
    let mut reader =
        hound::WavReader::open(path).with_context(|| format!("open WAV: {}", path.display()))?;
    let spec = reader.spec();
    if spec.channels != 1 {
        bail!("expected mono WAV, got {} channels", spec.channels);
    }
    if spec.sample_rate != 16_000 {
        bail!(
            "expected 16 kHz WAV, got {} Hz (resample with sox or ffmpeg)",
            spec.sample_rate
        );
    }
    if spec.bits_per_sample != 16 {
        bail!("expected 16-bit WAV, got {}-bit", spec.bits_per_sample);
    }
    let samples: std::result::Result<Vec<i16>, _> = reader.samples::<i16>().collect();
    samples.context("decode i16 samples")
}

// -------- daemon (M4) --------

struct AppState {
    state: DaemonState,
    worker: Option<worker::WorkerHandle>,
    capture: Option<audio::CaptureSession>,
    last_use: Instant,
    worker_cfg: worker::WorkerConfig,
    audio_cfg: audio::AudioConfig,
    indicator: indicator::Indicator,
    watch_tx: tokio::sync::watch::Sender<DaemonStatus>,
    /// Segmenter task running while in `Listening`. Spawned by handle_start,
    /// reaped + tail-flushed by handle_stop.
    segmenter: Option<segmenter::SegmenterHandle>,
    /// Clone of the actor's command channel — used by handle_start to give
    /// the segmenter a way to submit `TranscribeSegment` jobs back to us.
    cmd_tx: mpsc::Sender<ipc::InternalCmd>,
}

impl AppState {
    fn new(
        watch_tx: tokio::sync::watch::Sender<DaemonStatus>,
        cmd_tx: mpsc::Sender<ipc::InternalCmd>,
    ) -> Result<Self> {
        Ok(Self {
            state: DaemonState::Idle,
            worker: None,
            capture: None,
            last_use: Instant::now(),
            worker_cfg: worker::WorkerConfig::from_env()?,
            audio_cfg: audio::AudioConfig::from_env(),
            indicator: indicator::Indicator::spawn(),
            watch_tx,
            segmenter: None,
            cmd_tx,
        })
    }

    fn current_status(&self) -> DaemonStatus {
        DaemonStatus {
            state: self.state,
            worker_loaded: self.worker.is_some(),
            idle_seconds: self.last_use.elapsed().as_secs(),
        }
    }

    fn set_state(&mut self, state: DaemonState) {
        self.state = state;
        self.indicator.set_state(state);
        // Push to all live subscribers. `watch::Sender::send` only fails when
        // every receiver has been dropped, which is fine — the daemon-only
        // local indicator is not a watch receiver, so a "no subscribers" case
        // is normal and silent.
        let _ = self.watch_tx.send(self.current_status());
    }

    async fn handle_ctl(&mut self, cmd: CtlCommand) -> CtlResponse {
        match cmd {
            CtlCommand::Start => match self.handle_start().await {
                Ok(()) => CtlResponse::Ok,
                Err(e) => {
                    self.set_state(DaemonState::Error);
                    log::error!("start failed: {e:#}");
                    CtlResponse::Error {
                        msg: format!("start: {e}"),
                    }
                }
            },
            CtlCommand::Stop => match self.handle_stop().await {
                Ok(()) => CtlResponse::Ok,
                Err(e) => {
                    self.set_state(DaemonState::Error);
                    log::error!("stop failed: {e:#}");
                    CtlResponse::Error {
                        msg: format!("stop: {e}"),
                    }
                }
            },
            CtlCommand::Status => CtlResponse::Status(self.current_status()),
            CtlCommand::Quit => CtlResponse::Ok,
            CtlCommand::Subscribe => CtlResponse::Error {
                msg: "subscribe is handled directly by the IPC layer, not the actor loop".into(),
            },
        }
    }

    async fn handle_start(&mut self) -> Result<()> {
        if self.capture.is_some() {
            log::debug!("start: already listening; ignoring");
            return Ok(());
        }
        if self.worker.is_none() {
            log::info!("start: spawning worker (cold)");
            self.worker = Some(worker::WorkerHandle::spawn(&self.worker_cfg).await?);
        }
        let session = audio::CaptureSession::start(&self.audio_cfg).await?;
        let buf = session.buffer();
        self.capture = Some(session);
        // Spawn the streaming segmenter — commits at speech-end pauses
        // during the press; the tail after F9 release is flushed by
        // handle_stop.
        self.segmenter = Some(segmenter::spawn(buf, self.cmd_tx.clone()));
        self.set_state(DaemonState::Listening);
        self.last_use = Instant::now();
        log::info!("listening (segmenter armed)");
        Ok(())
    }

    async fn handle_stop(&mut self) -> Result<()> {
        let session = match self.capture.take() {
            Some(s) => s,
            None => {
                log::debug!("stop: no active capture; ignoring");
                return Ok(());
            }
        };
        // Reap the segmenter first so it can't submit any more commits
        // after this point. Whatever it has *already* sent will be
        // processed by the actor's main loop after handle_stop returns —
        // those segments are still valid speech and should be transcribed.
        let last_committed = if let Some(seg) = self.segmenter.take() {
            let mark = seg.last_committed.load(std::sync::atomic::Ordering::Relaxed);
            seg.stop().await;
            mark
        } else {
            0
        };

        self.set_state(DaemonState::Transcribing);
        let pcm = session.stop();
        // Tail = whatever the segmenter didn't commit. Includes the user's
        // last clause that ended with the F9 release (no trailing silence
        // to trigger a VAD commit).
        let tail: Vec<i16> = if last_committed < pcm.len() {
            pcm[last_committed..].to_vec()
        } else {
            Vec::new()
        };
        log::info!(
            "captured {} samples total; segmenter committed {}; tail {} samples ({} ms)",
            pcm.len(),
            last_committed,
            tail.len(),
            tail.len() * 1000 / 16_000,
        );
        let inject_result = if tail.is_empty() {
            Ok(())
        } else {
            let pcm_b64 = worker::encode_pcm_le(&tail);
            let worker = self
                .worker
                .as_mut()
                .context("worker is None at stop time (start should have spawned it)")?;
            let result = worker.transcribe(pcm_b64, audio::SAMPLE_RATE).await?;
            log::info!(
                "tail transcript ({} ms, language={}): {:?}",
                result.duration_ms,
                result.language,
                result.text
            );
            injector::type_text(&result.text).await
        };
        // Reset state regardless of injection outcome so the next press works.
        self.set_state(DaemonState::Idle);
        self.last_use = Instant::now();
        inject_result.context("inject failed for stop tail")?;
        Ok(())
    }

    /// Mid-press segment from the segmenter: transcribe + inject. Stays in
    /// `Listening` state — no pill flicker for streaming commits.
    async fn handle_transcribe_segment(&mut self, pcm: Vec<i16>) -> Result<()> {
        let pcm_b64 = worker::encode_pcm_le(&pcm);
        let worker = self
            .worker
            .as_mut()
            .context("worker gone during streaming segment")?;
        let result = worker.transcribe(pcm_b64, audio::SAMPLE_RATE).await?;
        log::info!(
            "segment transcript ({} ms, language={}): {:?}",
            result.duration_ms,
            result.language,
            result.text
        );
        if let Err(e) = injector::type_text(&result.text).await {
            log::warn!("segment inject failed (continuing): {e:#}");
        }
        self.last_use = Instant::now();
        Ok(())
    }

    async fn idle_tick(&mut self) {
        if !matches!(self.state, DaemonState::Idle) {
            return;
        }
        if self.worker.is_none() {
            return;
        }
        if self.last_use.elapsed() < idle_timeout() {
            return;
        }
        log::info!(
            "idle for {}s; unloading worker",
            self.last_use.elapsed().as_secs()
        );
        if let Some(w) = self.worker.take() {
            if let Err(e) = w.shutdown().await {
                log::warn!("worker shutdown during idle reap: {e:#}");
            }
        }
    }

    async fn shutdown(mut self) {
        if let Some(s) = self.capture.take() {
            let _ = s.stop();
        }
        if let Some(w) = self.worker.take() {
            if let Err(e) = w.shutdown().await {
                log::warn!("worker shutdown on quit: {e:#}");
            }
        }
    }
}

async fn run_daemon() -> Result<()> {
    let socket_path = ipc::default_socket_path()?;
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let _ = std::fs::remove_file(&socket_path);
    let listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("bind {}", socket_path.display()))?;
    log::info!("whisperd listening on {}", socket_path.display());

    let (cmd_tx, mut cmd_rx) = mpsc::channel::<ipc::InternalCmd>(16);

    // Watch channel: AppState pushes a fresh DaemonStatus on every set_state;
    // each Subscribe connection clones a Receiver and streams events to the peer.
    let initial_status = DaemonStatus {
        state: DaemonState::Idle,
        worker_loaded: false,
        idle_seconds: 0,
    };
    let (watch_tx, watch_rx) = tokio::sync::watch::channel(initial_status);

    // IPC accept task
    let cmd_tx_ipc = cmd_tx.clone();
    let watch_rx_ipc = watch_rx.clone();
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let cmd_tx = cmd_tx_ipc.clone();
                    let watch_rx = watch_rx_ipc.clone();
                    tokio::spawn(async move {
                        if let Err(e) = ipc::handle_client(stream, cmd_tx, watch_rx).await {
                            log::warn!("ipc client error: {e:#}");
                        }
                    });
                }
                Err(e) => {
                    log::error!("accept: {e}");
                    break;
                }
            }
        }
    });

    // Idle ticker
    let cmd_tx_idle = cmd_tx.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(IDLE_TICK_INTERVAL);
        interval.tick().await; // skip immediate first tick
        loop {
            interval.tick().await;
            if cmd_tx_idle.send(ipc::InternalCmd::IdleTick).await.is_err() {
                break;
            }
        }
    });

    let mut state = AppState::new(watch_tx, cmd_tx.clone())?;

    let result: Result<()> = loop {
        tokio::select! {
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(ipc::InternalCmd::Ipc(ctl, resp_tx)) => {
                        let is_quit = matches!(ctl, CtlCommand::Quit);
                        let resp = state.handle_ctl(ctl).await;
                        let _ = resp_tx.send(resp);
                        if is_quit {
                            log::info!("quit received");
                            break Ok(());
                        }
                    }
                    Some(ipc::InternalCmd::IdleTick) => state.idle_tick().await,
                    Some(ipc::InternalCmd::TranscribeSegment(pcm)) => {
                        if let Err(e) = state.handle_transcribe_segment(pcm).await {
                            log::warn!("transcribe segment failed: {e:#}");
                        }
                    }
                    None => break Ok(()),
                }
            }
            _ = tokio::signal::ctrl_c() => {
                log::info!("SIGINT received");
                break Ok(());
            }
        }
    };

    state.shutdown().await;
    let _ = std::fs::remove_file(&socket_path);
    result
}
