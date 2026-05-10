//! Unix-socket IPC server for whisperd.
//!
//! Two arms over the same socket, dispatched on the verb of the first
//! NDJSON line a client sends:
//!
//! - **Command arm** (`start`/`stop`/`status`/`quit`) — request/response,
//!   one-shot. Forwarded to the daemon main task via `cmd_tx`; reply is
//!   read from a oneshot channel and written back; connection closes.
//! - **Event arm** (`subscribe`) — long-lived. The handler clones a
//!   `watch::Receiver<DaemonStatus>` and streams `CtlEvent::StateChanged`
//!   on every state transition until the peer disconnects.
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

/// Messages routed into the daemon main loop.
pub enum InternalCmd {
    Ipc(CtlCommand, oneshot::Sender<CtlResponse>),
    IdleTick,
    /// Segmenter detected a pause; transcribe + inject this audio segment.
    /// Sent by `crate::segmenter` while the daemon is in `Listening`.
    TranscribeSegment(Vec<i16>),
}

pub fn default_socket_path() -> Result<PathBuf> {
    if let Ok(p) = std::env::var(SOCKET_PATH_ENV) {
        return Ok(PathBuf::from(p));
    }
    let home = std::env::var_os("HOME").context("HOME not set")?;
    Ok(PathBuf::from(home).join(DEFAULT_SOCKET_REL_PATH))
}

pub async fn handle_client(
    stream: UnixStream,
    cmd_tx: mpsc::Sender<InternalCmd>,
    watch_rx: watch::Receiver<DaemonStatus>,
) -> Result<()> {
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
        CtlCommand::Subscribe => handle_subscribe(write_half, watch_rx).await,
        other => handle_one_shot(write_half, other, cmd_tx).await,
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

    // Stream every subsequent transition.
    while watch_rx.changed().await.is_ok() {
        let status = watch_rx.borrow_and_update().clone();
        if write_event(&mut write_half, &event_for(status))
            .await
            .is_err()
        {
            // Peer disconnected. Drop the receiver and exit cleanly.
            return Ok(());
        }
    }
    // Sender dropped → daemon is shutting down. Exit silently.
    Ok(())
}

fn event_for(status: DaemonStatus) -> CtlEvent {
    CtlEvent::StateChanged {
        state: status.state,
        worker_loaded: status.worker_loaded,
        idle_seconds: status.idle_seconds,
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
