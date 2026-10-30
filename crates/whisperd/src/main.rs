use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use tokio::net::UnixListener;
use tokio::sync::{mpsc, watch};
use whisper_stt_proto::{CtlCommand, CtlResponse, DaemonState, DaemonStatus};

mod audio;
mod command;
mod cue;
mod eval;
mod focus;
mod indicator;
mod injector;
mod ipc;
mod joiner;
mod niri;
mod segmenter;
mod tmux;
mod toggle;
mod transcriber;
mod voice;
mod wake;
mod worker;

use cue::Cue;
use ipc::{InternalCmd, Level};
use wake::WakeEvent;

/// How long "No speech detected" and similar notices stay up. Myna scales
/// this with the text length (8–15 s); our notices are all short.
const NOTICE_FOR: Duration = Duration::from_secs(8);

fn env_secs(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

fn idle_timeout() -> Duration {
    Duration::from_secs(env_secs("WHISPER_STT_IDLE_TIMEOUT_SECS", 300))
}

/// `None` = never auto-stop.
fn silence_timeout() -> Option<Duration> {
    match env_secs("WHISPER_STT_SILENCE_TIMEOUT_SECS", 30) {
        0 => None,
        s => Some(Duration::from_secs(s)),
    }
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
        Some("--eval-commands") => {
            let dir: PathBuf = args
                .next()
                .unwrap_or_else(|| "tests/fixtures/commands".into())
                .into();
            eval::run(&dir, read_wav_mono_16k).await
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
    whisperd --eval-commands [dir] dev: score voice commands recorded in dir
                                   (default tests/fixtures/commands)

ENV (worker):
    WHISPER_STT_PYTHON         override the venv python
    WHISPER_STT_WORKER_SCRIPT  override the worker script path
    WHISPER_STT_MODEL          default: distil-large-v3
    WHISPER_STT_DEVICE         default: cuda
    WHISPER_STT_COMPUTE_TYPE   default: float16
    WHISPER_STT_LANGUAGE       default: en (empty = auto-detect)
    WHISPER_STT_BEAM_SIZE      default: 5

ENV (audio):
    WHISPER_STT_SOURCE         optional PulseAudio source name (default: system default)

ENV (session):
    WHISPER_STT_SILENCE_TIMEOUT_SECS  default: 30 (auto-stop after this much silence; 0 = never)
    WHISPER_STT_SOUNDS                default: 1 (0 = no start/stop/error sounds)
    WHISPER_STT_SOUND_DIR             default: /usr/share/sounds/freedesktop/stereo

ENV (voice commands):
    WHISPER_STT_WAKE           default: 1 (0 = no always-on \"Jarvis\" listener)
    WHISPER_STT_WAKE_MODEL     default: base.en (runs on the CPU)
    WHISPER_STT_TMUX_SOCKET    optional tmux socket name (tmux -L)

ENV (lifecycle):
    WHISPER_STT_IDLE_TIMEOUT_SECS  default: 300 (worker unloaded after this)
    WHISPER_STT_SOCK               override the IPC socket path
    WHISPER_STT_DEBUG              1 = log transcript text (off: only lengths)
"
    );
}

// -------- dev paths (M2/M3) --------

/// Dev paths decode like a live session would: audio without voice is not
/// sent to Whisper (it would invent "Thank you.").
async fn decode_once(pcm: &[i16]) -> Result<()> {
    if !transcriber::worth_decoding(pcm) {
        log::info!("no speech detected; not decoding");
        return Ok(());
    }
    let cfg = worker::WorkerConfig::from_env()?;
    let mut handle = worker::WorkerHandle::spawn(&cfg).await?;
    let result = handle
        .transcribe(worker::encode_pcm_le(pcm), audio::SAMPLE_RATE, None)
        .await?;
    println!("{}", result.text);
    transcriber::log_transcript("dev", &result);
    handle.shutdown().await?;
    Ok(())
}

async fn run_transcribe(wav: &Path) -> Result<()> {
    let pcm = read_wav_mono_16k(wav)?;
    log::info!(
        "decoded {} samples ({} ms)",
        pcm.len(),
        pcm.len() * 1000 / 16_000
    );
    decode_once(&pcm).await
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
    decode_once(&pcm).await
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

// -------- daemon --------
//
// One loop owns the session state. It never waits on the model or the
// microphone: mic warm-up runs as a task that reports `CaptureReady`, and
// decoding lives in `transcriber`. Allowed transitions are listed in
// `DaemonState::can_become`.
//
//   idle/notice/error --toggle--> starting --mic live--> listening
//   starting --toggle--> idle (cancelled)
//   listening --toggle or auto-stop--> transcribing --> idle | notice | error

struct AppState {
    state: DaemonState,
    message: Option<String>,
    /// Outcome of the voice command a `notice` reports.
    ok: Option<bool>,
    /// Bumped per session, so late replies for an older one are dropped.
    generation: u64,
    capture: Option<audio::CaptureSession>,
    segmenter: Option<segmenter::SegmenterHandle>,
    worker_loaded: bool,
    last_use: Instant,
    audio_cfg: audio::AudioConfig,
    silence_timeout: Option<Duration>,
    jobs: mpsc::UnboundedSender<transcriber::Job>,
    cmd_tx: mpsc::Sender<InternalCmd>,
    status_tx: watch::Sender<DaemonStatus>,
    level_tx: Arc<watch::Sender<Level>>,
    indicator: indicator::Indicator,
    cues: cue::CuePlayer,
}

impl AppState {
    fn status(&self) -> DaemonStatus {
        DaemonStatus {
            state: self.state,
            worker_loaded: self.worker_loaded,
            idle_seconds: self.last_use.elapsed().as_secs(),
            message: self.message.clone(),
            ok: self.ok,
        }
    }

    fn publish(&self) {
        // Fails only when no receiver exists, which is fine.
        let _ = self.status_tx.send(self.status());
    }

    fn set_state(&mut self, next: DaemonState, message: Option<String>) {
        self.transition(next, message, None);
    }

    fn transition(&mut self, next: DaemonState, message: Option<String>, ok: Option<bool>) {
        if !self.state.can_become(next) {
            log::error!("bug: illegal transition {:?} -> {next:?}", self.state);
            debug_assert!(false, "illegal transition {:?} -> {next:?}", self.state);
        }
        log::info!("state: {:?} -> {next:?}", self.state);
        // Voice commands (`command`, their `notice`) are silent: the bar
        // pill animates instead, as sounds after every command distracted.
        match next {
            DaemonState::Listening => self.cues.play(Cue::Start),
            DaemonState::Transcribing => self.cues.play(Cue::Stop),
            DaemonState::Error => self.cues.play(Cue::Error),
            _ => {}
        }
        if next == DaemonState::Notice {
            let tx = self.cmd_tx.clone();
            let generation = self.generation;
            tokio::spawn(async move {
                tokio::time::sleep(NOTICE_FOR).await;
                let _ = tx.send(InternalCmd::NoticeExpired(generation)).await;
            });
        }
        if let Some(m) = &message {
            log::warn!("{next:?}: {m}");
        }
        self.state = next;
        self.message = message;
        self.ok = ok;
        self.indicator.set_state(next);
        self.publish();
    }

    async fn handle(&mut self, cmd: InternalCmd) {
        match cmd {
            InternalCmd::Ipc(..) => unreachable!("handled in the main loop"),
            InternalCmd::Toggle => self.toggle().await,
            InternalCmd::CaptureReady { generation, result } => {
                self.capture_ready(generation, result)
            }
            InternalCmd::AutoStop => {
                if self.state == DaemonState::Listening {
                    self.finish().await;
                }
            }
            InternalCmd::WorkerLoaded(loaded) => {
                self.worker_loaded = loaded;
                self.publish();
            }
            InternalCmd::WorkerFailed(msg) => {
                self.fail(format!("Model failed to load: {msg}")).await
            }
            InternalCmd::TailDone(result) => self.tail_done(result),
            InternalCmd::NoticeExpired(generation) => {
                if self.state == DaemonState::Notice && generation == self.generation {
                    self.set_state(DaemonState::Idle, None);
                }
            }
            InternalCmd::Wake(event) => self.wake(event),
        }
    }

    /// Voice commands only act while no dictation is running.
    fn wake(&mut self, event: WakeEvent) {
        use DaemonState::*;
        match event {
            WakeEvent::Heard(app) if matches!(self.state, Idle | Notice | Error) => {
                self.generation += 1;
                // The message names the focused app ("terminal", "browser",
                // "other"); the bar pill shows matching command hints.
                self.set_state(Command, Some(format!("{app:?}").to_lowercase()));
            }
            WakeEvent::Done(result) if matches!(self.state, Idle | Notice | Error | Command) => {
                self.command_result(result)
            }
            WakeEvent::Expired if self.state == Command => self.set_state(Idle, None),
            WakeEvent::StartDictation if matches!(self.state, Idle | Notice | Error | Command) => {
                self.begin()
            }
            _ => {}
        }
    }

    fn command_result(&mut self, result: Result<String, String>) {
        self.generation += 1; // a new notice: the old one's timer must not clear it
        let (ok, message) = match result {
            Ok(m) => (true, m),
            Err(m) => (false, m),
        };
        self.transition(DaemonState::Notice, Some(message), Some(ok));
    }

    async fn ctl(&mut self, cmd: CtlCommand) -> CtlResponse {
        use DaemonState::*;
        match cmd {
            CtlCommand::Start => {
                if matches!(self.state, Idle | Notice | Error | Command) {
                    self.begin();
                }
                CtlResponse::Ok
            }
            CtlCommand::Stop => {
                if matches!(self.state, Starting | Listening) {
                    self.toggle().await;
                }
                CtlResponse::Ok
            }
            CtlCommand::Status => CtlResponse::Status(self.status()),
            CtlCommand::Run { text } => {
                if !matches!(self.state, Idle | Notice | Error | Command) {
                    return CtlResponse::Error {
                        msg: "busy dictating".into(),
                    };
                }
                let sessions = tmux::sessions().await;
                let app = focus::focused().await;
                let result = match command::parse(&text, &sessions, false, app) {
                    command::Parsed::Command(command::Intent::StartDictation) => {
                        self.begin();
                        return CtlResponse::Ok;
                    }
                    command::Parsed::Command(intent) => wake::perform(&intent).await,
                    command::Parsed::Unknown(why) => Err(why),
                    command::Parsed::WakeOnly | command::Parsed::NotWake => {
                        Err("Empty command".into())
                    }
                };
                self.command_result(result.clone());
                match result {
                    Ok(_) => CtlResponse::Ok,
                    Err(msg) => CtlResponse::Error { msg },
                }
            }
            CtlCommand::Quit => CtlResponse::Ok,
            CtlCommand::Toggle | CtlCommand::Subscribe => CtlResponse::Error {
                msg: "handled by the IPC layer".into(),
            },
        }
    }

    /// What a press means depends only on the current state.
    async fn toggle(&mut self) {
        use DaemonState::*;
        match self.state {
            Idle | Notice | Error | Command => self.begin(),
            Starting => {
                // Cancel: the warm-up's late CaptureReady is dropped by generation.
                self.generation += 1;
                let _ = self.jobs.send(transcriber::Job::Cancel);
                self.set_state(Idle, None);
            }
            Listening => self.finish().await,
            Transcribing => log::info!("toggle ignored while transcribing"),
        }
    }

    fn begin(&mut self) {
        self.generation += 1;
        let generation = self.generation;
        self.set_state(DaemonState::Starting, None);
        // Model load and mic warm-up run in parallel; audio captured before
        // the model is ready simply waits in the transcriber's queue.
        let _ = self.jobs.send(transcriber::Job::Begin);
        let cfg = self.audio_cfg.clone();
        let tx = self.cmd_tx.clone();
        tokio::spawn(async move {
            let result = audio::CaptureSession::start(&cfg)
                .await
                .map_err(|e| format!("{e:#}"));
            let _ = tx
                .send(InternalCmd::CaptureReady { generation, result })
                .await;
        });
    }

    fn capture_ready(&mut self, generation: u64, result: Result<audio::CaptureSession, String>) {
        if generation != self.generation || self.state != DaemonState::Starting {
            if let Ok(session) = result {
                drop(session.stop()); // cancelled while warming up
            }
            return;
        }
        match result {
            Ok(session) => {
                self.segmenter = Some(segmenter::spawn(segmenter::Wiring {
                    buf: session.buffer(),
                    jobs: self.jobs.clone(),
                    cmd_tx: self.cmd_tx.clone(),
                    level: self.level_tx.clone(),
                    silence_timeout: self.silence_timeout,
                }));
                self.capture = Some(session);
                self.set_state(DaemonState::Listening, None);
            }
            Err(e) => {
                let _ = self.jobs.send(transcriber::Job::Cancel);
                self.set_state(DaemonState::Error, Some(format!("Microphone: {e}")));
            }
        }
    }

    /// End capture and hand the tail to the transcriber.
    async fn finish(&mut self) {
        let Some(seg) = self.segmenter.take() else {
            return;
        };
        let Some(session) = self.capture.take() else {
            return;
        };
        let mut outcome = seg.stop().await;
        let pcm = session.stop();
        let voice = outcome.voice_in_tail(&pcm);
        let tail = pcm[outcome.committed.min(pcm.len())..].to_vec();
        log::info!(
            "captured {} ms; pieces covered {} ms; tail {} ms (voice={voice})",
            pcm.len() as u64 / 16,
            outcome.committed as u64 / 16,
            tail.len() as u64 / 16
        );
        self.set_state(DaemonState::Transcribing, None);
        let _ = self.jobs.send(transcriber::Job::Tail { pcm: tail, voice });
    }

    /// The model failed while a session was starting or running.
    async fn fail(&mut self, message: String) {
        if !matches!(self.state, DaemonState::Starting | DaemonState::Listening) {
            return;
        }
        self.generation += 1;
        if let Some(seg) = self.segmenter.take() {
            seg.stop().await;
        }
        if let Some(session) = self.capture.take() {
            drop(session.stop());
        }
        let _ = self.level_tx.send(Level::default());
        let _ = self.jobs.send(transcriber::Job::Cancel);
        self.set_state(DaemonState::Error, Some(message));
    }

    fn tail_done(&mut self, result: Result<bool, String>) {
        if self.state != DaemonState::Transcribing {
            return;
        }
        self.last_use = Instant::now();
        match result {
            Ok(true) => self.set_state(DaemonState::Idle, None),
            Ok(false) => self.set_state(DaemonState::Notice, Some("No speech detected".into())),
            Err(e) => self.set_state(DaemonState::Error, Some(e)),
        }
    }

    async fn shutdown(mut self) {
        if let Some(seg) = self.segmenter.take() {
            seg.stop().await;
        }
        if let Some(session) = self.capture.take() {
            drop(session.stop());
        }
        // Dropping `jobs` lets the transcriber shut the worker down.
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

    let (cmd_tx, mut cmd_rx) = mpsc::channel::<InternalCmd>(64);
    let (status_tx, status_rx) = watch::channel(DaemonStatus {
        state: DaemonState::Idle,
        worker_loaded: false,
        idle_seconds: 0,
        message: None,
        ok: None,
    });
    let (level_tx, level_rx) = watch::channel(Level::default());

    let toggle_tx = cmd_tx.clone();
    let poker = toggle::spawn(move || {
        !matches!(
            toggle_tx.try_send(InternalCmd::Toggle),
            Err(mpsc::error::TrySendError::Closed(_))
        )
    });

    if wake::enabled() {
        wake::spawn(
            audio::AudioConfig::from_env(),
            cmd_tx.clone(),
            status_rx.clone(),
        );
    }

    let shared = ipc::Shared {
        cmd_tx: cmd_tx.clone(),
        poker,
        status: status_rx,
        level: level_rx,
    };
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let shared = shared.clone();
                    tokio::spawn(async move {
                        if let Err(e) = ipc::handle_client(stream, shared).await {
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

    let jobs = transcriber::spawn(
        worker::WorkerConfig::from_env()?,
        idle_timeout(),
        cmd_tx.clone(),
    );
    let mut state = AppState {
        state: DaemonState::Idle,
        message: None,
        ok: None,
        generation: 0,
        capture: None,
        segmenter: None,
        worker_loaded: false,
        last_use: Instant::now(),
        audio_cfg: audio::AudioConfig::from_env(),
        silence_timeout: silence_timeout(),
        jobs,
        cmd_tx,
        status_tx,
        level_tx: Arc::new(level_tx),
        indicator: indicator::Indicator::spawn(),
        cues: cue::CuePlayer::from_env(),
    };

    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    loop {
        tokio::select! {
            cmd = cmd_rx.recv() => match cmd {
                Some(InternalCmd::Ipc(ctl, resp_tx)) => {
                    let quit = matches!(ctl, CtlCommand::Quit);
                    let _ = resp_tx.send(state.ctl(ctl).await);
                    if quit {
                        log::info!("quit received");
                        break;
                    }
                }
                Some(other) => state.handle(other).await,
                None => break,
            },
            _ = tokio::signal::ctrl_c() => {
                log::info!("SIGINT received");
                break;
            }
            _ = sigterm.recv() => {
                log::info!("SIGTERM received");
                break;
            }
        }
    }

    state.shutdown().await;
    let _ = std::fs::remove_file(&socket_path);
    Ok(())
}
