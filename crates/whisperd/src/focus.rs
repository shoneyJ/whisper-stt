//! Which app has focus, so voice commands and typing can fit it.
//!
//! Asks niri (`niri msg -j focused-window`). Outside niri, or if the call
//! fails, the answer is `Other`: plain dictation, Enter only.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum App {
    Terminal,
    Browser,
    Other,
}

/// App ids of terminals. Alacritty is the one in use; the others cost
/// nothing to list.
const TERMINALS: &[&str] = &["Alacritty", "kitty", "foot", "com.mitchellh.ghostty"];
const BROWSERS: &[&str] = &["zen", "firefox"];

pub async fn focused() -> App {
    let Ok(out) = tokio::process::Command::new("niri")
        .args(["msg", "-j", "focused-window"])
        .output()
        .await
    else {
        return App::Other;
    };
    serde_json::from_slice::<serde_json::Value>(&out.stdout)
        .ok()
        .and_then(|w| w.get("app_id")?.as_str().map(kind))
        .unwrap_or(App::Other)
}

pub fn kind(app_id: &str) -> App {
    if TERMINALS.contains(&app_id) {
        App::Terminal
    } else if BROWSERS.contains(&app_id) {
        App::Browser
    } else {
        App::Other
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_ids_map_to_kinds() {
        assert_eq!(kind("Alacritty"), App::Terminal);
        assert_eq!(kind("zen"), App::Browser);
        assert_eq!(kind("DBeaver"), App::Other);
    }
}
