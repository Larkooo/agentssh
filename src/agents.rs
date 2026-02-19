use std::{
    env, fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentDefinition {
    pub id: String,
    pub label: String,
    pub binary: String,
    pub launch: String,
    /// CLI flag to inject a system prompt, e.g. `"--append-system-prompt"`.
    pub prompt_flag: Option<String>,
}

#[derive(Debug, Clone, Copy)]
struct KnownAgent {
    id: &'static str,
    label: &'static str,
    binary: &'static str,
    launch: &'static str,
    prompt_flag: Option<&'static str>,
}

/// Instruction appended via the agent's system-prompt flag so it keeps the
/// terminal title updated with a short description of its current task.
const TITLE_INSTRUCTION: &str = "\
Continuously set the terminal title to a short description of your current task \
using an OSC 0 escape sequence. Keep it under 6 words. Update it at every step.";

const KNOWN_AGENTS: &[KnownAgent] = &[
    KnownAgent {
        id: "codex",
        label: "Codex",
        binary: "codex",
        launch: "codex",
        prompt_flag: None,
    },
    KnownAgent {
        id: "claude",
        label: "Claude Code",
        binary: "claude",
        launch: "claude",
        prompt_flag: Some("--append-system-prompt"),
    },
    KnownAgent {
        id: "aider",
        label: "Aider",
        binary: "aider",
        launch: "aider",
        prompt_flag: None,
    },
    KnownAgent {
        id: "gemini",
        label: "Gemini CLI",
        binary: "gemini",
        launch: "gemini",
        prompt_flag: None,
    },
    KnownAgent {
        id: "opencode",
        label: "OpenCode",
        binary: "opencode",
        launch: "opencode",
        prompt_flag: None,
    },
];

pub fn detect_available_agents(
    custom_agents: &[crate::config::CustomAgentConfig],
) -> Vec<AgentDefinition> {
    let mut agents: Vec<AgentDefinition> = KNOWN_AGENTS
        .iter()
        .filter_map(|agent| {
            let full_path = find_binary(agent.binary)?;
            Some(AgentDefinition {
                id: agent.id.to_owned(),
                label: agent.label.to_owned(),
                binary: agent.binary.to_owned(),
                launch: full_path.to_string_lossy().to_string(),
                prompt_flag: agent.prompt_flag.map(ToOwned::to_owned),
            })
        })
        .collect();

    // Custom agents: same id overrides built-in, otherwise appended
    for custom in custom_agents {
        if let Some(existing) = agents.iter_mut().find(|a| a.id == custom.id) {
            existing.label = custom.label.clone();
            existing.binary = custom.binary.clone();
            existing.launch = custom.launch.clone();
            existing.prompt_flag = custom.prompt_flag.clone();
        } else {
            agents.push(AgentDefinition {
                id: custom.id.clone(),
                label: custom.label.clone(),
                binary: custom.binary.clone(),
                launch: custom.launch.clone(),
                prompt_flag: custom.prompt_flag.clone(),
            });
        }
    }

    agents
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
                prompt_flag: found.prompt_flag.map(ToOwned::to_owned),
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
            prompt_flag: a.prompt_flag.map(ToOwned::to_owned),
        })
}

/// Build the shell command used to launch an agent, injecting a title
/// instruction via the agent's system-prompt flag when available.
/// When `title_injection_enabled` is false, the prompt flag is not used.
pub fn build_launch_command(agent: &AgentDefinition, title_injection_enabled: bool) -> String {
    match &agent.prompt_flag {
        Some(flag) if title_injection_enabled => {
            format!("{} {} \"{}\"", agent.launch, flag, TITLE_INSTRUCTION)
        }
        _ => agent.launch.clone(),
    }
}

/// Returns true if this agent needs a send-keys title injection (i.e. it has
/// no system-prompt flag, so we fall back to injecting a first message).
pub fn needs_title_injection(agent: &AgentDefinition) -> bool {
    agent.prompt_flag.is_none()
}

/// Path to the title file for a session: `/tmp/agentssh_{name}.title`
pub fn title_file_path(session_name: &str) -> PathBuf {
    PathBuf::from(format!("/tmp/agentssh_{session_name}.title"))
}

/// Read the title file for a session. Returns empty string if missing or unreadable.
pub fn read_title_file(session_name: &str) -> String {
    fs::read_to_string(title_file_path(session_name))
        .map(|s| s.trim().to_owned())
        .unwrap_or_default()
}

/// Build the message to inject via send-keys for agents without a prompt flag.
pub fn build_title_injection(session_name: &str) -> String {
    let path = title_file_path(session_name);
    format!(
        "IMPORTANT: Before and during all work, frequently write a 3-6 word \
         summary of your current task to {}. Update it whenever your focus \
         changes. Do this silently without mentioning it. Now wait for my \
         actual task.",
        path.display()
    )
}

pub fn build_managed_session_name(agent_id: &str) -> String {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Use underscores — dots are special in tmux target syntax (session.window.pane)
    format!("agentssh_{agent_id}_{ts}")
}

pub fn short_instance_name(session_name: &str) -> String {
    if let Some((agent, suffix)) = split_managed_session_name(session_name) {
        return format!("{agent}_{suffix}");
    }
    session_name.to_owned()
}

/// Derive a human-friendly display title for a session tab / list entry.
///
/// Priority:
/// 1. `title_override` — content read from the session's title file
///    (`/tmp/agentssh_{name}.title`), written by the agent itself.
/// 2. `pane_title` — agents like Claude Code set this via terminal escape
///    sequences.  Ignore default shell titles (e.g. "zsh", "bash").
/// 3. Basename of `pane_current_path` (e.g. `/Users/me/my-app` → `"my-app"`).
///    Returns `"~"` when the path equals `$HOME`.
/// 4. Falls back to `short_instance_name()`.
pub fn derive_display_title(
    session_name: &str,
    pane_title: &str,
    pane_current_path: &str,
    title_override: &str,
) -> String {
    // 1. Title file written by the agent (highest priority).
    let trimmed_override = title_override.trim();
    if !trimmed_override.is_empty() {
        return trimmed_override.to_owned();
    }

    // 2. Prefer the pane title if it looks meaningful (not just a shell name).
    let trimmed_title = pane_title.trim();
    if !trimmed_title.is_empty() && !is_default_shell_title(trimmed_title) {
        return trimmed_title.to_owned();
    }

    // 3. Try the path basename.
    if !pane_current_path.is_empty() && pane_current_path != "/" {
        if let Ok(home) = env::var("HOME") {
            if pane_current_path == home {
                return "~".to_owned();
            }
        }
        if let Some(base) = Path::new(pane_current_path).file_name() {
            let s = base.to_string_lossy();
            if !s.is_empty() {
                return s.into_owned();
            }
        }
    }

    // 4. Fallback.
    short_instance_name(session_name)
}

fn is_default_shell_title(title: &str) -> bool {
    // Bare shell names
    if matches!(
        title,
        "zsh" | "bash" | "fish" | "sh" | "dash" | "ksh" | "tcsh" | "csh" | "nu" | "nushell"
    ) {
        return true;
    }

    // Default terminal title format: "dirname: /path/to/command" or "dirname: command"
    // e.g. "agents: /opt/homebrew/bin/codex", "myproject: node"
    // These are set automatically by the shell, not intentionally by the agent.
    if let Some((_cwd, cmd)) = title.split_once(": ") {
        let cmd = cmd.trim();
        // Looks like a binary path or bare command name (no spaces = not a real title)
        if cmd.starts_with('/') || (!cmd.contains(' ') && !cmd.is_empty()) {
            return true;
        }
    }

    false
}

pub fn managed_session_agent_id(session_name: &str) -> Option<String> {
    split_managed_session_name(session_name).map(|(agent, _)| agent.to_owned())
}

fn split_managed_session_name(session_name: &str) -> Option<(&str, &str)> {
    // Support both old "agentssh.agent.ts" and new "agentssh_agent_ts" formats
    let (prefix, agent, suffix) = if session_name.starts_with("agentssh_") {
        let rest = &session_name["agentssh_".len()..];
        let pos = rest.rfind('_')?;
        ("agentssh", &rest[..pos], &rest[pos + 1..])
    } else {
        let mut parts = session_name.split('.');
        let prefix = parts.next()?;
        let agent = parts.next()?;
        let suffix = parts.next()?;
        (prefix, agent, suffix)
    };

    if prefix != "agentssh" || agent.is_empty() || suffix.is_empty() {
        return None;
    }
    Some((agent, suffix))
}

pub(crate) fn command_binary(command: &str) -> Option<String> {
    let first = command.split_whitespace().next()?.trim();
    if first.is_empty() {
        return None;
    }

    Path::new(first)
        .file_name()
        .map(|x| x.to_string_lossy().to_string())
        .or_else(|| Some(first.to_owned()))
}

pub(crate) fn binary_matches(actual: &str, expected: &str) -> bool {
    actual == expected || actual.starts_with(&format!("{expected}."))
}

fn find_binary(binary: &str) -> Option<PathBuf> {
    if binary.contains('/') {
        let p = Path::new(binary);
        return if is_executable(p) {
            Some(p.to_path_buf())
        } else {
            None
        };
    }

    let path_var = env::var_os("PATH")?;
    env::split_paths(&path_var)
        .map(|p| p.join(binary))
        .find(|candidate| is_executable(candidate))
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
        assert_eq!(short_instance_name("agentssh.claude.999"), "claude_999");
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
    fn derive_title_prefers_title_override() {
        let title = derive_display_title(
            "agentssh_codex_999",
            "agents: /opt/homebrew/bin/codex",
            "/Users/me/agents",
            "Refactoring auth module",
        );
        assert_eq!(title, "Refactoring auth module");
    }

    #[test]
    fn derive_title_prefers_pane_title() {
        let title = derive_display_title(
            "agentssh_claude_999",
            "Claude Code - my-project",
            "/Users/me/my-project",
            "",
        );
        assert_eq!(title, "Claude Code - my-project");
    }

    #[test]
    fn derive_title_ignores_shell_names_uses_path() {
        let title = derive_display_title("agentssh_claude_999", "zsh", "/Users/me/my-app", "");
        assert_eq!(title, "my-app");
    }

    #[test]
    fn derive_title_ignores_default_terminal_title() {
        // "dirname: /path/to/binary" is the default terminal title format
        let title = derive_display_title(
            "agentssh_codex_999",
            "agents: /opt/homebrew/bin/codex",
            "/Users/me/agents",
            "",
        );
        assert_eq!(title, "agents");
    }

    #[test]
    fn derive_title_returns_tilde_for_home() {
        let home = env::var("HOME").unwrap_or_else(|_| "/Users/testuser".to_owned());
        let title = derive_display_title("agentssh_claude_999", "", &home, "");
        assert_eq!(title, "~");
    }

    #[test]
    fn derive_title_falls_back_to_short_name() {
        let title = derive_display_title("agentssh_claude_999", "", "", "");
        assert_eq!(title, "claude_999");
    }

    #[test]
    fn classify_from_command_detects_known_agent() {
        let available = vec![AgentDefinition {
            id: "codex".to_owned(),
            label: "Codex".to_owned(),
            binary: "codex".to_owned(),
            launch: "codex".to_owned(),
            prompt_flag: None,
        }];

        let found = classify_agent_from_session("freeform", "codex", &available)
            .expect("codex command should be classified");

        assert_eq!(found.id, "codex");
    }

}
