use serde::{Deserialize, Serialize};

pub const SOCKET_PATH_ENV: &str = "WHISPER_STT_SOCK";
pub const DEFAULT_SOCKET_REL_PATH: &str = ".cache/whisper-stt/whisperd.sock";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum CtlCommand {
    Start,
    Stop,
    Status,
    Quit,
    /// Long-lived subscription. The daemon emits one [`CtlEvent::StateChanged`]
    /// immediately (current state) and then one per state transition until
    /// the client disconnects.
    Subscribe,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum CtlResponse {
    Ok,
    Status(DaemonStatus),
    Error { msg: String },
}

/// Pushed to subscribers over the same Unix socket after a `Subscribe` verb.
/// One event per line (NDJSON). Fields mirror [`DaemonStatus`] but are
/// inlined here so the wire shape is stable and easy to consume from
/// non-Rust subscribers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum CtlEvent {
    StateChanged {
        state: DaemonState,
        worker_loaded: bool,
        idle_seconds: u64,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonStatus {
    pub state: DaemonState,
    pub worker_loaded: bool,
    pub idle_seconds: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonState {
    Idle,
    Listening,
    Transcribing,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum WorkerCommand {
    Init {
        model: String,
        device: String,
        compute_type: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        language: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        beam_size: Option<u32>,
    },
    Transcribe {
        pcm_b64: String,
        sr: u32,
    },
    Shutdown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum WorkerEvent {
    Ready {
        model: String,
        device: String,
        compute_type: String,
    },
    Transcript {
        text: String,
        #[serde(default)]
        language: String,
        #[serde(default)]
        duration_ms: u64,
    },
    Error {
        msg: String,
        #[serde(default)]
        recoverable: bool,
    },
}
