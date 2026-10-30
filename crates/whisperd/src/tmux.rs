//! Run a voice-command [`Tmux`] action.
//!
//! The daemon has no terminal of its own, so it acts on the tmux client
//! used most recently (highest `client_activity`): the one you were just
//! typing in. `WHISPER_STT_TMUX_SOCKET` selects a server by socket name
//! (`tmux -L`), which keeps tests off the live server.

use tokio::process::Command;

use crate::command::{Dir, Split, Tmux};

fn tmux(args: &[&str]) -> Command {
    let mut cmd = Command::new("tmux");
    if let Ok(socket) = std::env::var("WHISPER_STT_TMUX_SOCKET") {
        cmd.args(["-L", &socket]);
    }
    cmd.args(args);
    cmd
}

async fn output(args: &[&str]) -> Result<String, String> {
    let out = tmux(args)
        .output()
        .await
        .map_err(|e| format!("tmux: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("tmux: {}", err.trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Running session names. No server means no sessions, not an error.
pub async fn sessions() -> Vec<String> {
    output(&["list-sessions", "-F", "#{session_name}"])
        .await
        .map(|s| s.lines().map(String::from).collect())
        .unwrap_or_default()
}

async fn recent_client() -> Result<String, String> {
    let list = output(&["list-clients", "-F", "#{client_activity} #{client_name}"])
        .await
        .unwrap_or_default();
    list.lines()
        .filter_map(|l| l.split_once(' '))
        .max_by_key(|(activity, _)| activity.parse::<u64>().unwrap_or(0))
        .map(|(_, name)| name.to_string())
        .ok_or_else(|| "No tmux client attached".to_string())
}

/// Run `action`; on success, a short result such as "tmux: main".
pub async fn run(action: &Tmux) -> Result<String, String> {
    let client = recent_client().await?;
    let session = output(&["display-message", "-p", "-c", &client, "#{client_session}"]).await?;
    // "=" = exact name, never a prefix match; "=s:" = its current window.
    let here = format!("={}:", session.trim());
    let cwd = "#{pane_current_path}";
    let args: Vec<String> = match action {
        Tmux::Switch(name) => switch(&client, &["-t", &format!("={name}")]),
        Tmux::Next => switch(&client, &["-n"]),
        Tmux::Previous => switch(&client, &["-p"]),
        Tmux::Last => switch(&client, &["-l"]),
        Tmux::NewWindow => strings(&["new-window", "-t", &here, "-c", cwd]),
        Tmux::Window(n) => strings(&["select-window", "-t", &format!("{here}{n}")]),
        Tmux::NextWindow => strings(&["next-window", "-t", &here]),
        Tmux::PreviousWindow => strings(&["previous-window", "-t", &here]),
        Tmux::Split(Split::Right) => strings(&["split-window", "-h", "-t", &here, "-c", cwd]),
        Tmux::Split(Split::Down) => strings(&["split-window", "-v", "-t", &here, "-c", cwd]),
        Tmux::Pane(d) => {
            let flag = match d {
                Dir::Left => "-L",
                Dir::Right => "-R",
                Dir::Up => "-U",
                Dir::Down => "-D",
            };
            strings(&["select-pane", flag, "-t", &here])
        }
        Tmux::Zoom => strings(&["resize-pane", "-Z", "-t", &here]),
        Tmux::ScrollUp => strings(&["copy-mode", "-u", "-t", &here]),
    };
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    output(&args).await?;
    // Ask the session, not the client: tmux next-3.9 reports a stale
    // `window_index` for `display -c <client>`.
    let now = match action {
        Tmux::Switch(_) | Tmux::Next | Tmux::Previous | Tmux::Last => {
            output(&["display-message", "-p", "-c", &client, "#{client_session}"]).await?
        }
        _ => {
            output(&[
                "display-message",
                "-p",
                "-t",
                &here,
                "#{session_name}:#{window_index}",
            ])
            .await?
        }
    };
    let msg = format!("tmux: {}", now.trim());
    let _ = output(&["display-message", "-c", &client, "-d", "1500", &msg]).await;
    Ok(msg)
}

fn switch(client: &str, how: &[&str]) -> Vec<String> {
    let mut args = strings(&["switch-client", "-c", client]);
    args.extend(strings(how));
    args
}

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|a| a.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command as StdCommand, Stdio};

    /// A throwaway server with sessions a and b and one control-mode client
    /// on a. Never the live server.
    #[tokio::test]
    async fn switches_the_recent_client_on_a_throwaway_server() {
        if StdCommand::new("tmux").arg("-V").output().is_err() {
            return; // tmux not installed
        }
        let socket = format!("whisper-test-{}", std::process::id());
        let t = |args: &[&str]| {
            StdCommand::new("tmux")
                .args(["-L", &socket, "-f", "/dev/null"])
                .args(args)
                .status()
                .unwrap()
        };
        assert!(t(&["new-session", "-d", "-s", "a"]).success());
        assert!(t(&["new-session", "-d", "-s", "b"]).success());
        let mut client = StdCommand::new("tmux")
            .args(["-L", &socket, "-C", "attach", "-t", "a"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(300));

        // Only this test reads the variable.
        std::env::set_var("WHISPER_STT_TMUX_SOCKET", &socket);
        let mut names = sessions().await;
        names.sort();
        let switched = run(&Tmux::Switch("b".into())).await;
        let back = run(&Tmux::Last).await;
        let missing = run(&Tmux::Switch("nope".into())).await;
        let new_window = run(&Tmux::NewWindow).await;
        // -f /dev/null: base-index 0, so the new window is 1.
        let window0 = run(&Tmux::Window(0)).await;
        let split = run(&Tmux::Split(Split::Right)).await;
        let panes = StdCommand::new("tmux")
            .args([
                "-L",
                &socket,
                "list-panes",
                "-t",
                "=a:0",
                "-F",
                "#{pane_active}",
            ])
            .output()
            .unwrap();
        let pane_left = run(&Tmux::Pane(Dir::Left)).await;
        let active_after_left = StdCommand::new("tmux")
            .args([
                "-L",
                &socket,
                "list-panes",
                "-t",
                "=a:0",
                "-F",
                "#{pane_active}",
            ])
            .output()
            .unwrap();

        let _ = client.kill();
        let _ = client.wait();
        t(&["kill-server"]);
        assert_eq!(names, ["a", "b"]);
        assert_eq!(switched, Ok("tmux: b".into()));
        assert_eq!(back, Ok("tmux: a".into()));
        assert!(missing.is_err());
        assert_eq!(new_window, Ok("tmux: a:1".into()));
        assert_eq!(window0, Ok("tmux: a:0".into()));
        assert!(split.is_ok());
        assert_eq!(String::from_utf8_lossy(&panes.stdout), "0\n1\n");
        assert!(pane_left.is_ok());
        assert_eq!(String::from_utf8_lossy(&active_after_left.stdout), "1\n0\n");
    }
}
