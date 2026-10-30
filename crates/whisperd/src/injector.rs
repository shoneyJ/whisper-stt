//! Type a transcript into the focused window.
//!
//! Under Wayland (`WAYLAND_DISPLAY` set) we use `wtype`, which speaks the
//! virtual-keyboard protocol (supported by niri, sway, Hyprland). Under X11
//! we use `xdotool type`. Both handle the UTF-8 codepoint → keycode mapping
//! themselves (em-dashes, smart quotes, accented characters that
//! faster-whisper happily emits), so we never synthesise key events here.
//!
//! `xdotool` cannot reach native Wayland windows, and `wtype` cannot reach
//! X11 ones, so the choice follows the session rather than a config knob.

use anyhow::{bail, Context, Result};

use crate::command::{Browser, Key};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Backend {
    Wtype,
    Xdotool,
}

fn backend() -> Backend {
    backend_for(std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty()))
}

fn backend_for(wayland: bool) -> Backend {
    if wayland {
        Backend::Wtype
    } else {
        Backend::Xdotool
    }
}

pub async fn type_text(text: &str) -> Result<()> {
    if text.trim().is_empty() {
        log::debug!("inject: text empty; skipping");
        return Ok(());
    }

    let (program, args, hint): (&str, Vec<&str>, &str) = match backend() {
        Backend::Wtype => ("wtype", vec!["-d", "1", "--", text], "apt install wtype"),
        Backend::Xdotool => (
            "xdotool",
            vec!["type", "--clearmodifiers", "--delay", "1", "--", text],
            "apt install xdotool",
        ),
    };

    let status = tokio::process::Command::new(program)
        .args(&args)
        .status()
        .await
        .with_context(|| format!("spawn {program} (install with `{hint}`)"))?;

    if !status.success() {
        bail!("{program} exited with {status}");
    }
    Ok(())
}

/// One key press with modifiers held ("ctrl", "shift", "alt"). Key and
/// modifier names are the same for wtype and xdotool (X keysyms).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chord {
    mods: &'static [&'static str],
    key: String,
}

fn chord(mods: &'static [&'static str], key: impl Into<String>) -> Chord {
    Chord {
        mods,
        key: key.into(),
    }
}

fn key_chords(key: Key) -> Vec<Chord> {
    match key {
        Key::Enter => vec![chord(&[], "Return")],
        Key::Cancel => vec![chord(&["ctrl"], "c")],
        Key::Clear => vec![chord(&["ctrl"], "l")],
        Key::Again => vec![chord(&[], "Up"), chord(&[], "Return")],
        Key::Tab => vec![chord(&[], "Tab")],
        Key::Escape => vec![chord(&[], "Escape")],
        Key::DeleteWord => vec![chord(&["ctrl"], "w")],
        Key::ClearLine => vec![chord(&["ctrl"], "u")],
    }
}

/// Press a voice-command key in the focused window.
pub async fn press(key: Key) -> Result<()> {
    send(&key_chords(key)).await
}

/// Run a Zen shortcut in the focused window (the caller checked it's Zen).
pub async fn browser(action: &Browser) -> Result<()> {
    // Let the URL or find bar take focus before typing into it.
    let settle = || tokio::time::sleep(std::time::Duration::from_millis(150));
    match action {
        Browser::NewTab => send(&[chord(&["ctrl"], "t")]).await,
        Browser::CloseTab => send(&[chord(&["ctrl"], "w")]).await,
        Browser::NextTab => send(&[chord(&["ctrl"], "Tab")]).await,
        Browser::PreviousTab => send(&[chord(&["ctrl", "shift"], "Tab")]).await,
        // Alt+1..8 = that tab, Alt+9 = last tab (Firefox/Zen on Linux).
        Browser::Tab(n @ 1..=9) => send(&[chord(&["alt"], n.to_string())]).await,
        Browser::Tab(n) => bail!("no shortcut for tab {n} (1-9)"),
        Browser::Back => send(&[chord(&["alt"], "Left")]).await,
        Browser::Forward => send(&[chord(&["alt"], "Right")]).await,
        Browser::Reload => send(&[chord(&["ctrl"], "r")]).await,
        Browser::Search(query) => {
            send(&[chord(&["ctrl"], "l")]).await?;
            settle().await;
            type_text(query).await?;
            send(&[chord(&[], "Return")]).await
        }
        Browser::Find(text) => {
            send(&[chord(&["ctrl"], "f")]).await?;
            settle().await;
            type_text(text).await
        }
    }
}

async fn send(chords: &[Chord]) -> Result<()> {
    let (program, args) = match backend() {
        Backend::Wtype => ("wtype", wtype_args(chords)),
        Backend::Xdotool => ("xdotool", xdotool_args(chords)),
    };
    let status = tokio::process::Command::new(program)
        .args(&args)
        .status()
        .await
        .with_context(|| format!("spawn {program}"))?;
    if !status.success() {
        bail!("{program} exited with {status}");
    }
    Ok(())
}

/// `-M ctrl -k c -m ctrl` per chord, all in one call.
fn wtype_args(chords: &[Chord]) -> Vec<String> {
    let mut args = Vec::new();
    for c in chords {
        for m in c.mods {
            args.extend(["-M".to_string(), m.to_string()]);
        }
        args.extend(["-k".to_string(), c.key.clone()]);
        for m in c.mods.iter().rev() {
            args.extend(["-m".to_string(), m.to_string()]);
        }
    }
    args
}

/// `key --clearmodifiers ctrl+c Up Return`
fn xdotool_args(chords: &[Chord]) -> Vec<String> {
    let mut args = vec!["key".to_string(), "--clearmodifiers".to_string()];
    for c in chords {
        let mut name: Vec<&str> = c.mods.to_vec();
        name.push(&c.key);
        args.push(name.join("+"));
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chords_become_wtype_and_xdotool_args() {
        let chords = [chord(&["ctrl", "shift"], "Tab"), chord(&[], "Return")];
        assert_eq!(
            wtype_args(&chords).join(" "),
            "-M ctrl -M shift -k Tab -m shift -m ctrl -k Return"
        );
        assert_eq!(
            xdotool_args(&chords).join(" "),
            "key --clearmodifiers ctrl+shift+Tab Return"
        );
    }

    #[test]
    fn wayland_session_uses_wtype() {
        assert_eq!(backend_for(true), Backend::Wtype);
    }

    #[test]
    fn x11_session_uses_xdotool() {
        assert_eq!(backend_for(false), Backend::Xdotool);
    }
}
