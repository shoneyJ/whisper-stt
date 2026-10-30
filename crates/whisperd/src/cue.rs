//! Start / stop / error sounds, so you know the state without looking.
//!
//! Driven from the daemon's state changes (see `AppState::set_state`), the
//! same idea as Myna's `Chiming` indicator wrapper: the sound can never
//! disagree with what the bar pill shows.
//!
//! - start: entering `Listening` (the mic is live, speak now); voice
//!   commands are silent, the bar pill animates instead
//! - stop: entering `Transcribing` (capture ended, by toggle or auto-stop)
//! - error: entering `Error`
//!
//! Sounds are the freedesktop theme files Ubuntu ships, played with
//! `pw-play` on a background task. A full queue drops the cue rather than
//! delaying the daemon; a failed play is logged once per kind and ignored.

use std::path::PathBuf;

use tokio::sync::mpsc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cue {
    Start,
    Stop,
    Error,
}

impl Cue {
    fn file(self) -> &'static str {
        match self {
            Cue::Start => "message-new-instant.oga",
            Cue::Stop => "complete.oga",
            Cue::Error => "dialog-error.oga",
        }
    }
}

const DEFAULT_DIR: &str = "/usr/share/sounds/freedesktop/stereo";
const QUEUE: usize = 3;

pub struct CuePlayer {
    tx: Option<mpsc::Sender<Cue>>,
}

impl CuePlayer {
    /// `WHISPER_STT_SOUNDS=0` disables sounds; `WHISPER_STT_SOUND_DIR`
    /// points at a folder with the same three file names.
    pub fn from_env() -> Self {
        if std::env::var("WHISPER_STT_SOUNDS").is_ok_and(|v| v == "0") {
            return Self { tx: None };
        }
        let dir: PathBuf = std::env::var_os("WHISPER_STT_SOUND_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| DEFAULT_DIR.into());
        let (tx, mut rx) = mpsc::channel::<Cue>(QUEUE);
        tokio::spawn(async move {
            let mut warned = false;
            while let Some(cue) = rx.recv().await {
                let path = dir.join(cue.file());
                let ok = tokio::process::Command::new("pw-play")
                    .arg(&path)
                    .status()
                    .await
                    .is_ok_and(|s| s.success());
                if !ok && !warned {
                    warned = true;
                    log::warn!("sound cue failed: pw-play {}", path.display());
                }
            }
        });
        Self { tx: Some(tx) }
    }

    pub fn play(&self, cue: Cue) {
        if let Some(tx) = &self.tx {
            let _ = tx.try_send(cue);
        }
    }
}
