//! Run a voice-command [`Niri`] action through `niri msg`.

use crate::command::Niri;
use crate::focus::{self, App};

async fn msg(args: &[&str]) -> Result<String, String> {
    let out = tokio::process::Command::new("niri")
        .arg("msg")
        .args(args)
        .output()
        .await
        .map_err(|e| format!("niri: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("niri: {}", err.trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Run `action`; on success, a short result such as "niri: workspace 3".
pub async fn run(action: &Niri) -> Result<String, String> {
    match action {
        Niri::Workspace(w) => {
            msg(&["action", "focus-workspace", w]).await?;
            Ok(format!("niri: workspace {w}"))
        }
        Niri::Maximize => {
            msg(&["action", "maximize-column"]).await?;
            Ok("niri: maximize".into())
        }
        Niri::Focus(app) => {
            let windows = msg(&["-j", "windows"]).await?;
            let id = pick(&windows, *app).ok_or_else(|| format!("No {app:?} window"))?;
            msg(&["action", "focus-window", "--id", &id.to_string()]).await?;
            Ok(format!("niri: {app:?}"))
        }
    }
}

/// Id of the most recently focused window of kind `app`.
fn pick(windows_json: &str, app: App) -> Option<u64> {
    let windows: Vec<serde_json::Value> = serde_json::from_str(windows_json).ok()?;
    windows
        .iter()
        .filter(|w| w["app_id"].as_str().map(focus::kind) == Some(app))
        .max_by_key(|w| {
            let t = &w["focus_timestamp"];
            (t["secs"].as_u64(), t["nanos"].as_u64())
        })
        .and_then(|w| w["id"].as_u64())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_most_recently_focused_window_of_a_kind() {
        let json = r#"[
            {"id":3,"app_id":"Alacritty","focus_timestamp":{"secs":10,"nanos":0}},
            {"id":8,"app_id":"zen","focus_timestamp":{"secs":20,"nanos":0}},
            {"id":9,"app_id":"Alacritty","focus_timestamp":{"secs":30,"nanos":5}},
            {"id":4,"app_id":"Slack","focus_timestamp":null}
        ]"#;
        assert_eq!(pick(json, App::Terminal), Some(9));
        assert_eq!(pick(json, App::Browser), Some(8));
        assert_eq!(pick("[]", App::Browser), None);
    }
}
