use serde::{Deserialize, Serialize};

pub const SOCKET_PATH_ENV: &str = "WHISPER_STT_SOCK";
pub const DEFAULT_SOCKET_REL_PATH: &str = ".cache/whisper-stt/whisperd.sock";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum CtlCommand {
    Start,
    Stop,
    /// Start if idle, stop if listening. For compositors without key-release
    /// bindings (niri), where hold-to-talk start/stop pairs are impossible.
    /// Passed through a key-repeat filter, so a held or bounced key toggles once.
    Toggle,
    Status,
    Quit,
    /// Run a spoken-style command given as text, e.g. "switch to session
    /// main". Same parser and tmux actions as the wake phrase, no audio;
    /// the wake phrase itself is optional here.
    Run {
        text: String,
    },
    /// Long-lived subscription. The daemon emits one [`CtlEvent::StateChanged`]
    /// immediately (current state), then one per state transition, plus
    /// [`CtlEvent::Level`] about 20 times a second while listening, until the
    /// client disconnects.
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
        /// Headline for `notice` / `error` (e.g. "No speech detected").
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
        /// On a `notice` that reports a voice command: did it run?
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ok: Option<bool>,
    },
    /// Microphone level of the last ~100 ms, linear full scale in `[0, 1]`.
    /// Sent only while listening, then once more with zeros when it ends.
    Level { rms: f32, peak: f32 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonStatus {
    pub state: DaemonState,
    pub worker_loaded: bool,
    pub idle_seconds: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// On a `notice` that reports a voice command: did it run?
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ok: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonState {
    Idle,
    /// Toggle accepted; waiting for the microphone's first samples.
    Starting,
    Listening,
    Transcribing,
    /// Session ended without text, e.g. "No speech detected", or a voice
    /// command reported its result. Clears itself.
    Notice,
    /// Something failed (mic, worker, typing). Cleared by the next toggle.
    Error,
    /// The wake word ("Jarvis") was heard alone; listening for the
    /// command that follows. `message` names the focused app
    /// ("terminal" / "browser" / "other") so a UI can hint at commands.
    Command,
}

impl DaemonState {
    /// The transitions the daemon may make. Anything else is a bug.
    pub fn can_become(self, next: DaemonState) -> bool {
        use DaemonState::*;
        matches!(
            (self, next),
            (Idle | Notice | Error | Command, Starting)
                | (Starting, Listening | Idle | Error)
                | (Listening, Transcribing | Error)
                | (Transcribing, Idle | Notice | Error)
                | (Notice | Error, Idle)
                // Voice commands: a result can follow any resting state.
                | (Idle | Notice | Error, Command)
                | (Idle | Notice | Error | Command, Notice)
                | (Command, Idle)
        )
    }
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
        /// Words to bias decoding towards (faster-whisper `hotwords`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        hotwords: Option<String>,
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

#[cfg(test)]
mod tests {
    use super::*;
    use DaemonState::*;

    const ALL: [DaemonState; 7] = [
        Idle,
        Starting,
        Listening,
        Transcribing,
        Notice,
        Error,
        Command,
    ];

    #[test]
    fn only_listed_transitions_are_allowed() {
        let allowed = [
            (Idle, Starting),
            (Notice, Starting),
            (Error, Starting),
            (Starting, Listening),
            (Starting, Idle),
            (Starting, Error),
            (Listening, Transcribing),
            (Listening, Error),
            (Transcribing, Idle),
            (Transcribing, Notice),
            (Transcribing, Error),
            (Notice, Idle),
            (Error, Idle),
            (Command, Starting),
            (Idle, Command),
            (Notice, Command),
            (Error, Command),
            (Idle, Notice),
            (Notice, Notice),
            (Error, Notice),
            (Command, Notice),
            (Command, Idle),
        ];
        for from in ALL {
            for to in ALL {
                assert_eq!(
                    from.can_become(to),
                    allowed.contains(&(from, to)),
                    "{from:?} -> {to:?}"
                );
            }
        }
    }

    #[test]
    fn old_subscribers_still_parse_state_events_without_message() {
        let line =
            r#"{"event":"state_changed","state":"idle","worker_loaded":false,"idle_seconds":3}"#;
        let ev: CtlEvent = serde_json::from_str(line).unwrap();
        assert!(matches!(ev, CtlEvent::StateChanged { message: None, .. }));
    }

    #[test]
    fn message_is_omitted_from_the_wire_when_absent() {
        let ev = CtlEvent::StateChanged {
            state: Idle,
            worker_loaded: true,
            idle_seconds: 0,
            message: None,
            ok: None,
        };
        assert!(!serde_json::to_string(&ev).unwrap().contains("message"));
    }
}
