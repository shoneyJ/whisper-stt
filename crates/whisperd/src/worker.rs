use std::path::PathBuf;
use std::process::Stdio;

use anyhow::{bail, Context, Result};
use base64::Engine;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use whisper_stt_proto::{WorkerCommand, WorkerEvent};

#[derive(Debug, Clone)]
pub struct WorkerConfig {
    pub python: PathBuf,
    pub script: PathBuf,
    pub model: String,
    pub device: String,
    pub compute_type: String,
    pub language: Option<String>,
    pub beam_size: Option<u32>,
}

impl WorkerConfig {
    pub fn from_env() -> Result<Self> {
        let cwd = std::env::current_dir().context("current dir")?;
        let python = std::env::var_os("WHISPER_STT_PYTHON")
            .map(PathBuf::from)
            .unwrap_or_else(|| cwd.join("worker/.venv/bin/python"));
        let script = std::env::var_os("WHISPER_STT_WORKER_SCRIPT")
            .map(PathBuf::from)
            .unwrap_or_else(|| cwd.join("worker/whisper_worker.py"));
        let model = std::env::var("WHISPER_STT_MODEL").unwrap_or_else(|_| "distil-large-v3".into());
        let device = std::env::var("WHISPER_STT_DEVICE").unwrap_or_else(|_| "cuda".into());
        let compute_type =
            std::env::var("WHISPER_STT_COMPUTE_TYPE").unwrap_or_else(|_| "float16".into());
        let language = std::env::var("WHISPER_STT_LANGUAGE")
            .ok()
            .or(Some("en".into()));
        let beam_size = std::env::var("WHISPER_STT_BEAM_SIZE")
            .ok()
            .and_then(|s| s.parse().ok());
        Ok(Self {
            python,
            script,
            model,
            device,
            compute_type,
            language,
            beam_size,
        })
    }
}

pub struct WorkerHandle {
    child: Child,
    stdin: BufWriter<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    pub model: String,
    pub device: String,
    pub compute_type: String,
}

impl WorkerHandle {
    pub async fn spawn(config: &WorkerConfig) -> Result<Self> {
        if !config.python.exists() {
            bail!(
                "worker python not found at {}; run scripts/setup-venv.sh",
                config.python.display()
            );
        }
        if !config.script.exists() {
            bail!("worker script not found at {}", config.script.display());
        }

        let mut child = Command::new(&config.python)
            .arg(&config.script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("spawn {}", config.python.display()))?;

        let stdin = BufWriter::new(child.stdin.take().context("worker missing stdin")?);
        let stdout = BufReader::new(child.stdout.take().context("worker missing stdout")?);

        let mut handle = Self {
            child,
            stdin,
            stdout,
            model: config.model.clone(),
            device: config.device.clone(),
            compute_type: config.compute_type.clone(),
        };

        let init = WorkerCommand::Init {
            model: config.model.clone(),
            device: config.device.clone(),
            compute_type: config.compute_type.clone(),
            language: config.language.clone(),
            beam_size: config.beam_size,
        };
        handle.send(&init).await?;

        match handle.recv().await? {
            WorkerEvent::Ready {
                model,
                device,
                compute_type,
            } => {
                log::info!(
                    "worker ready: model={model} device={device} compute_type={compute_type}"
                );
                handle.model = model;
                handle.device = device;
                handle.compute_type = compute_type;
                Ok(handle)
            }
            WorkerEvent::Error { msg, .. } => bail!("worker init failed: {msg}"),
            other => bail!("expected ready, got {other:?}"),
        }
    }

    async fn send(&mut self, cmd: &WorkerCommand) -> Result<()> {
        let line = serde_json::to_string(cmd).context("serialise worker command")?;
        self.stdin.write_all(line.as_bytes()).await?;
        self.stdin.write_all(b"\n").await?;
        self.stdin.flush().await?;
        Ok(())
    }

    async fn recv(&mut self) -> Result<WorkerEvent> {
        let mut line = String::new();
        let n = self
            .stdout
            .read_line(&mut line)
            .await
            .context("read worker line")?;
        if n == 0 {
            bail!("worker stdout closed unexpectedly");
        }
        serde_json::from_str(line.trim())
            .with_context(|| format!("parse worker event: {}", line.trim()))
    }

    pub async fn transcribe(&mut self, pcm_b64: String, sr: u32) -> Result<TranscribeResult> {
        self.send(&WorkerCommand::Transcribe { pcm_b64, sr })
            .await?;
        match self.recv().await? {
            WorkerEvent::Transcript {
                text,
                language,
                duration_ms,
            } => Ok(TranscribeResult {
                text,
                language,
                duration_ms,
            }),
            WorkerEvent::Error { msg, recoverable } => {
                if recoverable {
                    bail!("recoverable transcribe error: {msg}");
                } else {
                    bail!("fatal transcribe error: {msg}");
                }
            }
            other => bail!("expected transcript, got {other:?}"),
        }
    }

    pub async fn shutdown(mut self) -> Result<()> {
        let _ = self.send(&WorkerCommand::Shutdown).await;
        let timeout = tokio::time::sleep(std::time::Duration::from_secs(5));
        tokio::pin!(timeout);
        tokio::select! {
            res = self.child.wait() => {
                res.context("wait worker exit")?;
            }
            _ = &mut timeout => {
                log::warn!("worker did not exit within 5s; killing");
                let _ = self.child.kill().await;
                let _ = self.child.wait().await;
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct TranscribeResult {
    pub text: String,
    pub language: String,
    pub duration_ms: u64,
}

pub fn encode_pcm_le(pcm: &[i16]) -> String {
    let mut bytes = Vec::with_capacity(pcm.len() * 2);
    for s in pcm {
        bytes.extend_from_slice(&s.to_le_bytes());
    }
    base64::engine::general_purpose::STANDARD.encode(&bytes)
}
