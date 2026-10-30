//! `whisperd --eval-commands [dir]`: how well the wake model understands
//! recorded commands (doc/plan/07-voice-commands.md, V2 step 7).
//!
//! Reads `<dir>/phrases.tsv` (id, app, expected `Parsed`, phrase) and each
//! `<id>.wav`, decodes with the wake model (`WHISPER_STT_WAKE_MODEL`) once
//! with hotwords and once without, and scores the parsed result.

use std::path::Path;

use anyhow::{Context, Result};

use crate::command;
use crate::focus::App;
use crate::worker::{encode_pcm_le, WorkerConfig, WorkerHandle};

/// The eval's session list, so expected results don't depend on what's
/// running.
const SESSIONS: [&str; 3] = ["dotfiles", "main", "done"];

pub async fn run(dir: &Path, read_wav: impl Fn(&Path) -> Result<Vec<i16>>) -> Result<()> {
    let table = std::fs::read_to_string(dir.join("phrases.tsv"))
        .with_context(|| format!("read {}/phrases.tsv", dir.display()))?;
    let sessions: Vec<String> = SESSIONS.map(String::from).to_vec();
    let hotwords = format!(
        "Hey Jarvis, tmux session window pane split zoom tab browser terminal workspace maximize {}",
        SESSIONS.join(" ")
    );
    let cfg = WorkerConfig::wake_from_env()?;
    let mut worker = WorkerHandle::spawn(&cfg).await?;

    let (mut total, mut ok_hot, mut ok_plain) = (0, 0, 0);
    for line in table
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
    {
        let cols: Vec<&str> = line.split('\t').collect();
        let [id, app, expected, _say] = cols[..] else {
            println!("skip malformed line: {line:?}");
            continue;
        };
        let wav = dir.join(format!("{id}.wav"));
        if !wav.exists() {
            continue;
        }
        let app = match app {
            "browser" => App::Browser,
            "terminal" => App::Terminal,
            _ => App::Other,
        };
        let pcm = read_wav(&wav)?;
        let mut results = Vec::new();
        for hot in [Some(hotwords.clone()), None] {
            let r = worker
                .transcribe(encode_pcm_le(&pcm), crate::audio::SAMPLE_RATE, hot)
                .await?;
            let got = format!("{:?}", command::parse(&r.text, &sessions, true, app));
            results.push((got == expected, r.text));
        }
        total += 1;
        ok_hot += usize::from(results[0].0);
        ok_plain += usize::from(results[1].0);
        let mark = |ok: bool| if ok { "ok  " } else { "MISS" };
        let heard = format!("{:?}", results[0].1);
        println!(
            "{} {} {id:18} hot: {heard:40} plain: {:?}",
            mark(results[0].0),
            mark(results[1].0),
            results[1].1,
        );
    }
    worker.shutdown().await?;
    println!(
        "\n{}: {ok_hot}/{total} with hotwords, {ok_plain}/{total} without",
        cfg.model
    );
    Ok(())
}
