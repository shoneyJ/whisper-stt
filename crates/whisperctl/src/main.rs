use std::io::{BufRead, BufReader, Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use whisper_stt_proto::{CtlCommand, CtlResponse, DEFAULT_SOCKET_REL_PATH, SOCKET_PATH_ENV};

fn main() -> Result<()> {
    let cmd = parse_args().context("parsing arguments")?;
    let path = socket_path()?;
    let mut stream =
        UnixStream::connect(&path).with_context(|| format!("connect to {}", path.display()))?;

    let mut line = serde_json::to_string(&cmd)?;
    line.push('\n');
    stream.write_all(line.as_bytes())?;

    match cmd {
        CtlCommand::Subscribe => run_subscribe(stream),
        _ => run_one_shot(stream),
    }
}

fn run_one_shot(mut stream: UnixStream) -> Result<()> {
    stream.shutdown(Shutdown::Write)?;

    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes)?;
    let text = String::from_utf8(bytes).context("response not UTF-8")?;
    let resp: CtlResponse =
        serde_json::from_str(text.trim()).with_context(|| format!("parse response: {text:?}"))?;

    match resp {
        CtlResponse::Ok => Ok(()),
        CtlResponse::Status(s) => {
            println!("{}", serde_json::to_string_pretty(&s)?);
            if let Some(msg) = &s.message {
                eprintln!("{msg}");
            }
            Ok(())
        }
        CtlResponse::Error { msg } => bail!("{msg}"),
    }
}

fn run_subscribe(stream: UnixStream) -> Result<()> {
    // Leave the write half open — we have nothing more to send, but
    // half-closing can confuse some kernels' Unix-socket teardown semantics.
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let line = line.context("read event from daemon")?;
        if line.is_empty() {
            continue;
        }
        println!("{line}");
    }
    Ok(())
}

fn parse_args() -> Result<CtlCommand> {
    let mut args = std::env::args().skip(1);
    let verb = args
        .next()
        .context("expected: start|stop|toggle|status|quit|subscribe|run <text>")?;
    Ok(match verb.as_str() {
        "start" => CtlCommand::Start,
        "stop" => CtlCommand::Stop,
        "toggle" => CtlCommand::Toggle,
        "status" => CtlCommand::Status,
        "quit" => CtlCommand::Quit,
        "subscribe" => CtlCommand::Subscribe,
        "run" => {
            let text = args.collect::<Vec<_>>().join(" ");
            if text.is_empty() {
                bail!("run needs a command, e.g.: whisperctl run switch to session main");
            }
            CtlCommand::Run { text }
        }
        other => {
            bail!("unknown command: {other:?} (expected start|stop|toggle|status|quit|subscribe|run <text>)")
        }
    })
}

fn socket_path() -> Result<PathBuf> {
    if let Ok(p) = std::env::var(SOCKET_PATH_ENV) {
        return Ok(PathBuf::from(p));
    }
    let home = std::env::var_os("HOME").context("HOME not set")?;
    Ok(PathBuf::from(home).join(DEFAULT_SOCKET_REL_PATH))
}
