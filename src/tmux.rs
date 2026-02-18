use anyhow::{Context, Result, anyhow};
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub name: String,
    pub attached: bool,
    pub windows: u32,
    pub created: String,
    pub current_command: String,
    pub preview: Vec<String>,
    pub last_line: String,
}

pub fn list_sessions(filter: Option<&str>) -> Result<Vec<Session>> {
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
    if let Some(filter) = filter {
        let filter = filter.to_ascii_lowercase();
        sessions.retain(|s| s.name.to_ascii_lowercase().contains(&filter));
    }

    for session in &mut sessions {
        if let Ok(cmd) = run_tmux(&[
            "display-message",
            "-p",
            "-t",
            &session.name,
            "#{pane_current_command}",
        ]) {
            let cmd = cmd.trim();
            if !cmd.is_empty() {
                session.current_command = cmd.to_owned();
            }
        }

        if let Ok(preview) = run_tmux(&[
            "capture-pane",
            "-p",
            "-t",
            &format!("{}:0.0", session.name),
            "-S",
            "-20",
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

    Ok(sessions)
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
            preview: Vec::new(),
            last_line: "(no output yet)".to_owned(),
        });
    }

    Ok(sessions)
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
        assert_eq!(parsed[0].attached, false);
        assert_eq!(parsed[0].windows, 1);
        assert_eq!(parsed[1].attached, true);
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
