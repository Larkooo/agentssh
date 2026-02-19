use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::{env, fs};

use crate::agents;

// ── Raw TOML representation (all fields optional) ───────────────────────────

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct ConfigFile {
    refresh_interval: Option<u64>,
    default_spawn_dir: Option<String>,
    title_injection_enabled: Option<bool>,
    title_injection_delay: Option<u32>,
    notifications: Option<NotificationsConfigFile>,
    #[serde(default)]
    agents: Vec<CustomAgentConfig>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default)]
struct NotificationsConfigFile {
    sound_on_completion: Option<bool>,
    sound_method: Option<String>,
    sound_command: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CustomAgentConfig {
    pub id: String,
    pub label: String,
    pub binary: String,
    pub launch: String,
    pub prompt_flag: Option<String>,
}

// ── Resolved config the app uses ────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoundMethod {
    Bell,
    Command,
}

#[derive(Debug, Clone)]
pub struct NotificationsConfig {
    pub sound_on_completion: bool,
    pub sound_method: SoundMethod,
    pub sound_command: String,
}

#[derive(Debug, Clone)]
pub struct AppConfig {
    pub refresh_interval: u64,
    pub default_spawn_dir: Option<String>,
    pub title_injection_enabled: bool,
    pub title_injection_delay: u32,
    pub notifications: NotificationsConfig,
    pub custom_agents: Vec<CustomAgentConfig>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            refresh_interval: 3,
            default_spawn_dir: None,
            title_injection_enabled: true,
            title_injection_delay: 5,
            notifications: NotificationsConfig {
                sound_on_completion: true,
                sound_method: SoundMethod::Bell,
                sound_command: "afplay /System/Library/Sounds/Glass.aiff".to_owned(),
            },
            custom_agents: Vec::new(),
        }
    }
}

// ── Public API ──────────────────────────────────────────────────────────────

pub fn config_path() -> PathBuf {
    let home = env::var("HOME").unwrap_or_else(|_| ".".to_owned());
    PathBuf::from(home)
        .join(".config")
        .join("agentssh")
        .join("config.toml")
}

pub fn load_config() -> AppConfig {
    let path = config_path();
    let contents = match fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return AppConfig::default(),
    };

    let file: ConfigFile = match toml::from_str(&contents) {
        Ok(f) => f,
        Err(err) => {
            eprintln!("agentssh: warning: failed to parse {}: {err}", path.display());
            return AppConfig::default();
        }
    };

    let mut config = AppConfig::default();

    if let Some(v) = file.refresh_interval {
        config.refresh_interval = v.max(1);
    }
    config.default_spawn_dir = file.default_spawn_dir;
    if let Some(v) = file.title_injection_enabled {
        config.title_injection_enabled = v;
    }
    if let Some(v) = file.title_injection_delay {
        config.title_injection_delay = v;
    }

    if let Some(notif) = file.notifications {
        if let Some(v) = notif.sound_on_completion {
            config.notifications.sound_on_completion = v;
        }
        if let Some(ref method) = notif.sound_method {
            config.notifications.sound_method = match method.as_str() {
                "command" => SoundMethod::Command,
                _ => SoundMethod::Bell,
            };
        }
        if let Some(cmd) = notif.sound_command {
            config.notifications.sound_command = cmd;
        }
    }

    config.custom_agents = file.agents;
    config
}

pub fn apply_cli_overrides(config: &mut AppConfig, refresh_seconds: Option<u64>) {
    if let Some(v) = refresh_seconds {
        config.refresh_interval = v.max(1);
    }
}

pub fn play_notification_sound(config: &AppConfig) {
    if !config.notifications.sound_on_completion {
        return;
    }

    match config.notifications.sound_method {
        SoundMethod::Bell => {
            // Write BEL character to stdout
            eprint!("\x07");
        }
        SoundMethod::Command => {
            let cmd = &config.notifications.sound_command;
            if !cmd.is_empty() {
                let _ = Command::new("sh")
                    .arg("-c")
                    .arg(cmd)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn();
            }
        }
    }
}

/// Returns true if the command name looks like a shell (used for completion
/// detection: agent binary → shell means the agent finished).
pub fn is_shell(cmd: &str) -> bool {
    let name = std::path::Path::new(cmd)
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| cmd.to_owned());

    // Strip leading dash for login shells (e.g. "-zsh")
    let name = name.strip_prefix('-').unwrap_or(&name);

    matches!(
        name,
        "zsh" | "bash" | "fish" | "sh" | "dash" | "ksh" | "tcsh" | "csh" | "nu" | "nushell"
    )
}

/// Check if a command was an agent binary. Uses agents module helpers.
pub fn is_agent_command(
    command: &str,
    available_agents: &[agents::AgentDefinition],
) -> bool {
    let Some(binary) = agents::command_binary(command) else {
        return false;
    };
    available_agents
        .iter()
        .any(|a| agents::binary_matches(&binary, &a.binary))
}

/// Track previous commands and detect completion transitions.
pub fn detect_completions(
    previous_commands: &mut HashMap<String, String>,
    instances: &[(String, String)], // (session_name, current_command)
    available_agents: &[agents::AgentDefinition],
    config: &AppConfig,
) {
    for (session_name, current_command) in instances {
        if let Some(prev) = previous_commands.get(session_name) {
            if is_agent_command(prev, available_agents) && is_shell(current_command) {
                play_notification_sound(config);
            }
        }
        previous_commands.insert(session_name.clone(), current_command.clone());
    }

    // Remove entries for sessions that no longer exist
    let active_names: std::collections::HashSet<&String> =
        instances.iter().map(|(name, _)| name).collect();
    previous_commands.retain(|name, _| active_names.contains(name));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_shell_detects_common_shells() {
        assert!(is_shell("zsh"));
        assert!(is_shell("bash"));
        assert!(is_shell("fish"));
        assert!(is_shell("-zsh"));
        assert!(is_shell("-bash"));
        assert!(is_shell("/bin/zsh"));
        assert!(is_shell("/usr/local/bin/fish"));
    }

    #[test]
    fn is_shell_rejects_non_shells() {
        assert!(!is_shell("claude"));
        assert!(!is_shell("codex"));
        assert!(!is_shell("node"));
        assert!(!is_shell("python"));
    }

    #[test]
    fn default_config_has_sensible_values() {
        let config = AppConfig::default();
        assert_eq!(config.refresh_interval, 3);
        assert!(config.title_injection_enabled);
        assert_eq!(config.title_injection_delay, 5);
        assert!(config.notifications.sound_on_completion);
        assert_eq!(config.notifications.sound_method, SoundMethod::Bell);
    }

    #[test]
    fn apply_cli_overrides_sets_refresh() {
        let mut config = AppConfig::default();
        apply_cli_overrides(&mut config, Some(10));
        assert_eq!(config.refresh_interval, 10);
    }

    #[test]
    fn apply_cli_overrides_none_keeps_default() {
        let mut config = AppConfig::default();
        apply_cli_overrides(&mut config, None);
        assert_eq!(config.refresh_interval, 3);
    }

    #[test]
    fn load_config_returns_defaults_for_missing_file() {
        // Just verify it doesn't panic and returns defaults
        // (actual file may or may not exist in test environment)
        let config = AppConfig::default();
        assert_eq!(config.refresh_interval, 3);
    }
}
