//! Type a transcript into the focused X11 window via `xdotool`.
//!
//! We use `xdotool type` rather than synthesising key events ourselves
//! because xdotool already handles UTF-8 codepoint → keycode mapping
//! (including em-dashes, smart quotes, and accented characters that
//! faster-whisper happily emits).
//!
//! Wayland note: `xdotool` is X11-only. The architecture doc places
//! Wayland support out of scope for v1; we will swap to `wtype`/`ydotool`
//! when that lands.

use anyhow::{bail, Context, Result};

pub async fn type_text(text: &str) -> Result<()> {
    if text.trim().is_empty() {
        log::debug!("inject: text empty; skipping");
        return Ok(());
    }

    let status = tokio::process::Command::new("xdotool")
        .args(["type", "--clearmodifiers", "--delay", "1", "--", text])
        .status()
        .await
        .context("spawn xdotool (install with `apt install xdotool`)")?;

    if !status.success() {
        bail!("xdotool exited with {status}");
    }
    Ok(())
}
