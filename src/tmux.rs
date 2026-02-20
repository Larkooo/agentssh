use anyhow::{Context, Result, anyhow};
use std::process::Command;

pub fn is_tmux_available() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub name: String,
    pub attached: bool,
    pub windows: u32,
    pub created: String,
    pub current_command: String,
    pub pane_current_path: String,
    pub pane_title: String,
    pub preview: Vec<String>,
    pub last_line: String,
}

pub fn list_sessions() -> Result<Vec<Session>> {
    let raw = match run_tmux(&[
        "list-sessions",
        "-F",
        "#{session_name}\t#{session_attached}\t#{session_windows}\t#{session_created_string}",
    ]) {
        Ok(out) => out,
        Err(err) if is_no_server_error(&err.to_string()) => return Ok(Vec::new()),
        Err(err) => return Err(err),
    };

    let mut sessions = parse_session_list(&raw)?;

    for session in &mut sessions {
        if let Ok(info) = run_tmux(&[
            "display-message",
            "-p",
            "-t",
            &format!("{}:", session.name),
            "#{pane_current_command}\t#{pane_current_path}\t#{pane_title}",
        ]) {
            let parts: Vec<&str> = info.trim_end().splitn(3, '\t').collect();
            if let Some(cmd) = parts.first() {
                let cmd = cmd.trim();
                if !cmd.is_empty() {
                    session.current_command = cmd.to_owned();
                }
            }
            if let Some(path) = parts.get(1) {
                let path = path.trim();
                if !path.is_empty() {
                    session.pane_current_path = path.to_owned();
                }
            }
            if let Some(title) = parts.get(2) {
                let title = title.trim();
                if !title.is_empty() {
                    session.pane_title = title.to_owned();
                }
            }
        }

        let target = format!("{}:0.0", session.name);
        if let Ok(preview) = run_tmux(&[
            "capture-pane",
            "-p",
            "-t",
            &target,
            "-S",
            "-30",
        ]) {
            let lines: Vec<String> = preview
                .lines()
                .map(str::trim_end)
                .map(ToOwned::to_owned)
                .collect();
            let last = last_non_empty_line(&lines).unwrap_or("(no output yet)");
            session.last_line = last.to_owned();
            session.preview = lines;
        }
    }

    sessions.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(sessions)
}

pub fn create_session(name: &str, working_dir: &str, shell_command: &str) -> Result<()> {
    // Step 1: Create session with the user's default shell so .bashrc/.zshrc are
    // sourced and PATH (nvm, pyenv, etc.) is fully configured.
    let status = Command::new("tmux")
        .arg("new-session")
        .arg("-d")
        .arg("-s")
        .arg(name)
        .arg("-c")
        .arg(working_dir)
        .status()
        .with_context(|| format!("failed to run tmux new-session for {name}"))?;

    if !status.success() {
        return Err(anyhow!("tmux new-session exited with status {status}"));
    }

    // Step 2: Send the command as keystrokes into the session's shell.
    // This way the shell runs the command in its fully initialized environment,
    // and if the agent exits, the shell stays alive so the user can see errors.
    // NOTE: Append ":" to the session name so tmux treats dots as literal chars
    // rather than session.window.pane separators.
    let target = format!("{name}:");
    let send_status = Command::new("tmux")
        .arg("send-keys")
        .arg("-t")
        .arg(&target)
        .arg(shell_command)
        .arg("Enter")
        .status()
        .with_context(|| format!("failed to send command to session {name}"))?;

    if !send_status.success() {
        return Err(anyhow!("tmux send-keys exited with status {send_status}"));
    }

    Ok(())
}

#[allow(dead_code)]
pub fn send_keys(session_name: &str, text: &str) -> Result<()> {
    let target = format!("{session_name}:");
    let status = Command::new("tmux")
        .arg("send-keys")
        .arg("-t")
        .arg(&target)
        .arg(text)
        .arg("Enter")
        .status()
        .with_context(|| format!("failed to send keys to session {session_name}"))?;

    if status.success() {
        Ok(())
    } else {
        Err(anyhow!("tmux send-keys exited with status {status}"))
    }
}

/// Send keystrokes to a session after a delay, in a fire-and-forget background
/// process.  This gives TUI-based agents (e.g. Codex) time to boot before
/// receiving input.
pub fn send_keys_delayed(session_name: &str, text: &str, delay_secs: u32) -> Result<()> {
    let target = format!("{session_name}:");
    // Single-quote the text for the shell, escaping inner single quotes.
    let escaped = text.replace('\'', "'\\''");
    // Send the text literally with -l (no key-name lookup), pause briefly for
    // the TUI to process, then send Enter as a separate keypress.
    let script = format!(
        "sleep {delay_secs} && tmux send-keys -t '{target}' -l '{escaped}' && sleep 1 && tmux send-keys -t '{target}' Enter"
    );
    Command::new("sh")
        .arg("-c")
        .arg(&script)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .with_context(|| format!("failed to spawn delayed send-keys for {session_name}"))?;
    Ok(())
}

/// Create a temporary tmux session with side-by-side panes, each nested-
/// attaching to one of the given target sessions.  The caller should
/// `attach_session(name)` immediately after to enter the split view.
pub fn create_split_session(name: &str, targets: &[String]) -> Result<()> {
    if targets.is_empty() {
        return Err(anyhow!("no target sessions for split"));
    }

    // Create the session; the first pane will attach to the first target.
    let attach_cmd = format!("TMUX='' tmux attach-session -t '{}'", targets[0]);
    let status = Command::new("tmux")
        .arg("new-session")
        .arg("-d")
        .arg("-s")
        .arg(name)
        .status()
        .with_context(|| format!("failed to create split session {name}"))?;
    if !status.success() {
        return Err(anyhow!("tmux new-session for split exited with {status}"));
    }

    // Send the nested-attach command into the first pane.
    let target0 = format!("{name}:");
    let _ = Command::new("tmux")
        .arg("send-keys")
        .arg("-t")
        .arg(&target0)
        .arg(&attach_cmd)
        .arg("Enter")
        .status();

    // For each additional target, split and nested-attach.
    for t in &targets[1..] {
        let _ = Command::new("tmux")
            .arg("split-window")
            .arg("-h")
            .arg("-t")
            .arg(name)
            .status();

        let attach = format!("TMUX='' tmux attach-session -t '{}'", t);
        let _ = Command::new("tmux")
            .arg("send-keys")
            .arg("-t")
            .arg(format!("{name}:"))
            .arg(&attach)
            .arg("Enter")
            .status();
    }

    // Even-horizontal layout for a clean split.
    let _ = Command::new("tmux")
        .arg("select-layout")
        .arg("-t")
        .arg(name)
        .arg("even-horizontal")
        .status();

    // Focus the first pane.
    let _ = Command::new("tmux")
        .arg("select-pane")
        .arg("-t")
        .arg(format!("{name}:.0"))
        .status();

    Ok(())
}

pub fn attach_session(name: &str) -> Result<()> {
    let status = Command::new("tmux")
        .arg("attach-session")
        .arg("-t")
        .arg(name)
        .status()
        .with_context(|| format!("failed to run tmux attach-session for {name}"))?;

    if status.success() {
        Ok(())
    } else {
        Err(anyhow!("tmux attach-session exited with status {status}"))
    }
}

pub fn kill_session(name: &str) -> Result<()> {
    let status = Command::new("tmux")
        .arg("kill-session")
        .arg("-t")
        .arg(name)
        .status()
        .with_context(|| format!("failed to run tmux kill-session for {name}"))?;

    if status.success() {
        Ok(())
    } else {
        Err(anyhow!("tmux kill-session exited with status {status}"))
    }
}

fn parse_session_list(raw: &str) -> Result<Vec<Session>> {
    if raw.trim().is_empty() {
        return Ok(Vec::new());
    }

    let mut sessions = Vec::new();
    for line in raw.lines() {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() != 4 {
            return Err(anyhow!("unexpected tmux output line: {line}"));
        }

        let windows = parts[2]
            .parse::<u32>()
            .with_context(|| format!("invalid window count in line: {line}"))?;

        sessions.push(Session {
            name: parts[0].to_owned(),
            attached: parts[1] == "1",
            windows,
            created: parts[3].to_owned(),
            current_command: "unknown".to_owned(),
            pane_current_path: String::new(),
            pane_title: String::new(),
            preview: Vec::new(),
            last_line: "(no output yet)".to_owned(),
        });
    }

    Ok(sessions)
}

/// Lightweight polling for background activity detection.
/// Returns `(session_name, preview_lines)` for all tmux sessions whose names
/// start with "agentssh_".
pub fn poll_session_previews() -> Vec<(String, Vec<String>)> {
    let Ok(raw) = run_tmux(&["list-sessions", "-F", "#{session_name}"]) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for name in raw.lines() {
        let name = name.trim();
        if name.is_empty() || !name.starts_with("agentssh_") {
            continue;
        }
        if let Ok(preview) = run_tmux(&[
            "capture-pane",
            "-p",
            "-t",
            &format!("{name}:0.0"),
            "-S",
            "-30",
        ]) {
            let lines: Vec<String> = preview
                .lines()
                .map(str::trim_end)
                .map(ToOwned::to_owned)
                .collect();
            out.push((name.to_owned(), lines));
        }
    }
    out
}

fn run_tmux(args: &[&str]) -> Result<String> {
    let output = Command::new("tmux")
        .args(args)
        .output()
        .with_context(|| format!("failed to execute tmux {}", args.join(" ")))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        Err(anyhow!(
            "tmux {} failed: {}",
            args.join(" "),
            if stderr.is_empty() {
                "unknown error"
            } else {
                &stderr
            }
        ))
    }
}

fn is_no_server_error(msg: &str) -> bool {
    let lower = msg.to_ascii_lowercase();
    lower.contains("failed to connect to server") || lower.contains("no server running")
}

fn last_non_empty_line(lines: &[String]) -> Option<&str> {
    for line in lines.iter().rev() {
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            return Some(trimmed);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn parse_session_list_handles_valid_rows() {
        let raw = "codex\t0\t1\tTue Feb 18 12:00:00 2026\nclaude\t1\t2\tTue Feb 18 13:00:00 2026\n";
        let parsed = parse_session_list(raw).expect("should parse");

        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].name, "codex");
        assert!(!parsed[0].attached);
        assert_eq!(parsed[0].windows, 1);
        assert!(parsed[1].attached);
    }

    #[test]
    fn parse_session_list_rejects_invalid_rows() {
        let raw = "codex\t0\n";
        let err = parse_session_list(raw).expect_err("invalid row should fail");
        assert!(err.to_string().contains("unexpected tmux output line"));
    }

    #[test]
    fn last_non_empty_line_skips_blank_lines() {
        let lines = vec![
            "".to_owned(),
            "  ".to_owned(),
            "hello world ".to_owned(),
            "".to_owned(),
        ];
        assert_eq!(last_non_empty_line(&lines), Some("hello world"));
    }
}
