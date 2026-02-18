use std::{
    env,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentDefinition {
    pub id: String,
    pub label: String,
    pub binary: String,
    pub launch: String,
}

#[derive(Debug, Clone, Copy)]
struct KnownAgent {
    id: &'static str,
    label: &'static str,
    binary: &'static str,
    launch: &'static str,
}

const KNOWN_AGENTS: &[KnownAgent] = &[
    KnownAgent {
        id: "codex",
        label: "Codex",
        binary: "codex",
        launch: "codex",
    },
    KnownAgent {
        id: "claude",
        label: "Claude Code",
        binary: "claude",
        launch: "claude",
    },
    KnownAgent {
        id: "aider",
        label: "Aider",
        binary: "aider",
        launch: "aider",
    },
    KnownAgent {
        id: "gemini",
        label: "Gemini CLI",
        binary: "gemini",
        launch: "gemini",
    },
    KnownAgent {
        id: "opencode",
        label: "OpenCode",
        binary: "opencode",
        launch: "opencode",
    },
];

pub fn detect_available_agents() -> Vec<AgentDefinition> {
    KNOWN_AGENTS
        .iter()
        .filter(|agent| command_exists(agent.binary))
        .map(|agent| AgentDefinition {
            id: agent.id.to_owned(),
            label: agent.label.to_owned(),
            binary: agent.binary.to_owned(),
            launch: agent.launch.to_owned(),
        })
        .collect()
}

pub fn classify_agent_from_session(
    session_name: &str,
    current_command: &str,
    available: &[AgentDefinition],
) -> Option<AgentDefinition> {
    if let Some(id) = managed_session_agent_id(session_name) {
        if let Some(found) = available.iter().find(|a| a.id == id) {
            return Some(found.clone());
        }

        if let Some(found) = KNOWN_AGENTS.iter().find(|a| a.id == id) {
            return Some(AgentDefinition {
                id: found.id.to_owned(),
                label: found.label.to_owned(),
                binary: found.binary.to_owned(),
                launch: found.launch.to_owned(),
            });
        }
    }

    let binary = command_binary(current_command)?;

    if let Some(found) = available
        .iter()
        .find(|a| binary_matches(&binary, &a.binary))
        .cloned()
    {
        return Some(found);
    }

    KNOWN_AGENTS
        .iter()
        .find(|a| binary_matches(&binary, a.binary))
        .map(|a| AgentDefinition {
            id: a.id.to_owned(),
            label: a.label.to_owned(),
            binary: a.binary.to_owned(),
            launch: a.launch.to_owned(),
        })
}

pub fn build_managed_session_name(agent_id: &str) -> String {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("agentssh.{agent_id}.{ts}")
}

pub fn build_launch_command(working_dir: &str, launch: &str) -> String {
    format!("cd {} && exec {}", shell_quote(working_dir), launch)
}

pub fn short_instance_name(session_name: &str) -> String {
    if let Some((agent, suffix)) = split_managed_session_name(session_name) {
        return format!("{agent}.{suffix}");
    }
    session_name.to_owned()
}

pub fn managed_session_agent_id(session_name: &str) -> Option<String> {
    split_managed_session_name(session_name).map(|(agent, _)| agent.to_owned())
}

fn split_managed_session_name(session_name: &str) -> Option<(&str, &str)> {
    let mut parts = session_name.split('.');
    let prefix = parts.next()?;
    let agent = parts.next()?;
    let suffix = parts.next()?;
    if prefix != "agentssh" || agent.is_empty() || suffix.is_empty() {
        return None;
    }
    Some((agent, suffix))
}

fn command_binary(command: &str) -> Option<String> {
    let first = command.split_whitespace().next()?.trim();
    if first.is_empty() {
        return None;
    }

    Path::new(first)
        .file_name()
        .map(|x| x.to_string_lossy().to_string())
        .or_else(|| Some(first.to_owned()))
}

fn binary_matches(actual: &str, expected: &str) -> bool {
    actual == expected || actual.starts_with(&format!("{expected}."))
}

fn shell_quote(input: &str) -> String {
    if input.is_empty() {
        return "''".to_owned();
    }
    format!("'{}'", input.replace('\'', "'\"'\"'"))
}

fn command_exists(binary: &str) -> bool {
    if binary.contains('/') {
        return is_executable(Path::new(binary));
    }

    let Some(path_var) = env::var_os("PATH") else {
        return false;
    };

    env::split_paths(&path_var)
        .map(|p| p.join(binary))
        .any(|candidate| is_executable(&candidate))
}

fn is_executable(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        match path.metadata() {
            Ok(meta) => meta.permissions().mode() & 0o111 != 0,
            Err(_) => false,
        }
    }

    #[cfg(not(unix))]
    {
        true
    }
}

#[allow(dead_code)]
fn _to_abs(path: PathBuf) -> String {
    path.to_string_lossy().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_managed_session_name() {
        assert_eq!(
            managed_session_agent_id("agentssh.codex.1234"),
            Some("codex".to_owned())
        );
        assert_eq!(managed_session_agent_id("random"), None);
    }

    #[test]
    fn short_name_compacts_managed_sessions() {
        assert_eq!(short_instance_name("agentssh.claude.999"), "claude.999");
        assert_eq!(short_instance_name("handmade"), "handmade");
    }

    #[test]
    fn command_binary_extracts_leaf() {
        assert_eq!(
            command_binary("/usr/local/bin/codex --help"),
            Some("codex".to_owned())
        );
        assert_eq!(command_binary("claude"), Some("claude".to_owned()));
        assert_eq!(command_binary(""), None);
    }

    #[test]
    fn classify_from_command_detects_known_agent() {
        let available = vec![AgentDefinition {
            id: "codex".to_owned(),
            label: "Codex".to_owned(),
            binary: "codex".to_owned(),
            launch: "codex".to_owned(),
        }];

        let found = classify_agent_from_session("freeform", "codex", &available)
            .expect("codex command should be classified");

        assert_eq!(found.id, "codex");
    }

    #[test]
    fn build_launch_command_quotes_working_directory() {
        let command = build_launch_command("/tmp/my dir/it's", "codex");
        assert_eq!(command, "cd '/tmp/my dir/it'\"'\"'s' && exec codex");
    }
}
