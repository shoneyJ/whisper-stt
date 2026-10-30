//! Unix-socket IPC server for whisperd.
//!
//! Two arms over the same socket, dispatched on the verb of the first
//! NDJSON line a client sends:
//!
//! - **Command arm** (`start`/`stop`/`status`/`quit`) — request/response,
//!   one-shot. Forwarded to the daemon main task via `cmd_tx`; reply is
//!   read from a oneshot channel and written back; connection closes.
//!   `toggle` is answered at once and handed to the key-repeat filter
//!   (`crate::toggle`), which forwards one `InternalCmd::Toggle` per press.
//! - **Event arm** (`subscribe`) — long-lived. The handler streams
//!   `CtlEvent::StateChanged` on every state transition and `CtlEvent::Level`
//!   while listening, until the peer disconnects.
//!
//! The actor pattern is preserved: state-mutating commands go through the
//! mpsc; subscriptions never enter the actor at all (they only read the
//! current value out of `watch`).

use std::path::PathBuf;

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot, watch};
use whisper_stt_proto::{
    CtlCommand, CtlEvent, CtlResponse, DaemonStatus, DEFAULT_SOCKET_REL_PATH, SOCKET_PATH_ENV,
};

use crate::audio::CaptureSession;
use crate::toggle::Poker;

/// Messages routed into the daemon main loop.
pub enum InternalCmd {
    Ipc(CtlCommand, oneshot::Sender<CtlResponse>),
    /// One accepted press from the key-repeat filter.
    Toggle,
    /// The microphone warm-up for session `generation` finished.
    CaptureReady {
        generation: u64,
        result: Result<CaptureSession, String>,
    },
    /// The segmenter heard no voice for the silence timeout, or the user
    /// said "stop dictation".
    AutoStop,
    WorkerLoaded(bool),
    /// The model failed to load for the session being started.
    WorkerFailed(String),
    /// The session's tail is done: `Ok(typed anything this session)`.
    TailDone(Result<bool, String>),
    /// A notice shown for session `generation` has been up long enough.
    NoticeExpired(u64),
    /// News from the wake-phrase listener.
    Wake(crate::wake::WakeEvent),
}

/// Mic level published while listening (see `CtlEvent::Level`).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Level {
    pub rms: f32,
    pub peak: f32,
}

/// What each client connection needs from the daemon.
#[derive(Clone)]
pub struct Shared {
    pub cmd_tx: mpsc::Sender<InternalCmd>,
    pub poker: Poker,
    pub status: watch::Receiver<DaemonStatus>,
    pub level: watch::Receiver<Level>,
}

pub fn default_socket_path() -> Result<PathBuf> {
    if let Ok(p) = std::env::var(SOCKET_PATH_ENV) {
        return Ok(PathBuf::from(p));
    }
    let home = std::env::var_os("HOME").context("HOME not set")?;
    Ok(PathBuf::from(home).join(DEFAULT_SOCKET_REL_PATH))
}

pub async fn handle_client(stream: UnixStream, shared: Shared) -> Result<()> {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();
    let n = reader.read_line(&mut line).await?;
    if n == 0 {
        return Ok(());
    }

    let cmd: CtlCommand = match serde_json::from_str(line.trim()) {
        Ok(c) => c,
        Err(e) => {
            let resp = CtlResponse::Error {
                msg: format!("bad command: {e}"),
            };
            write_response(&mut write_half, &resp).await?;
            return Ok(());
        }
    };

    match cmd {
        CtlCommand::Subscribe => handle_subscribe(write_half, shared.status, shared.level).await,
        CtlCommand::Toggle => {
            shared.poker.poke();
            write_response(&mut write_half, &CtlResponse::Ok).await
        }
        other => handle_one_shot(write_half, other, shared.cmd_tx).await,
    }
}

async fn handle_one_shot<W: AsyncWriteExt + Unpin>(
    mut write_half: W,
    cmd: CtlCommand,
    cmd_tx: mpsc::Sender<InternalCmd>,
) -> Result<()> {
    let (resp_tx, resp_rx) = oneshot::channel();
    if cmd_tx.send(InternalCmd::Ipc(cmd, resp_tx)).await.is_err() {
        let resp = CtlResponse::Error {
            msg: "daemon shutting down".into(),
        };
        write_response(&mut write_half, &resp).await?;
        return Ok(());
    }

    let resp = resp_rx.await.unwrap_or_else(|_| CtlResponse::Error {
        msg: "daemon dropped response".into(),
    });
    write_response(&mut write_half, &resp).await?;
    Ok(())
}

async fn handle_subscribe<W: AsyncWriteExt + Unpin>(
    mut write_half: W,
    mut watch_rx: watch::Receiver<DaemonStatus>,
    mut level_rx: watch::Receiver<Level>,
) -> Result<()> {
    // Send the current state immediately so the subscriber does not have to
    // wait for the next transition to know what state the daemon is in.
    // `borrow_and_update` marks the value as seen so the next `changed()`
    // call only fires on a *new* transition.
    {
        let initial = watch_rx.borrow_and_update().clone();
        if write_event(&mut write_half, &event_for(initial))
            .await
            .is_err()
        {
            return Ok(());
        }
    }

    level_rx.borrow_and_update();

    // Stream every subsequent transition and level update.
    loop {
        let event = tokio::select! {
            changed = watch_rx.changed() => {
                if changed.is_err() {
                    return Ok(()); // daemon shutting down
                }
                event_for(watch_rx.borrow_and_update().clone())
            }
            changed = level_rx.changed() => {
                if changed.is_err() {
                    return Ok(());
                }
                let l = *level_rx.borrow_and_update();
                CtlEvent::Level { rms: l.rms, peak: l.peak }
            }
        };
        if write_event(&mut write_half, &event).await.is_err() {
            return Ok(()); // peer disconnected
        }
    }
}

fn event_for(status: DaemonStatus) -> CtlEvent {
    CtlEvent::StateChanged {
        state: status.state,
        worker_loaded: status.worker_loaded,
        idle_seconds: status.idle_seconds,
        message: status.message,
        ok: status.ok,
    }
}

async fn write_response<W: AsyncWriteExt + Unpin>(w: &mut W, resp: &CtlResponse) -> Result<()> {
    let line = serde_json::to_string(resp)?;
    w.write_all(line.as_bytes()).await?;
    w.write_all(b"\n").await?;
    w.shutdown().await?;
    Ok(())
}

async fn write_event<W: AsyncWriteExt + Unpin>(w: &mut W, event: &CtlEvent) -> Result<()> {
    let line = serde_json::to_string(event)?;
    w.write_all(line.as_bytes()).await?;
    w.write_all(b"\n").await?;
    w.flush().await?;
    Ok(())
}
