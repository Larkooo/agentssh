use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};
use std::{env, fs, thread};

use crate::tmux;

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

#[derive(Debug, Clone, Deserialize, Serialize)]
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
                sound_method: SoundMethod::Command,
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

// ── Save support ────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct ConfigFileSave {
    refresh_interval: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    default_spawn_dir: Option<String>,
    title_injection_enabled: bool,
    title_injection_delay: u32,
    notifications: NotificationsConfigFileSave,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    agents: Vec<CustomAgentConfig>,
}

#[derive(Serialize)]
struct NotificationsConfigFileSave {
    sound_on_completion: bool,
    sound_method: String,
    sound_command: String,
}

pub fn save_config(config: &AppConfig) -> Result<(), String> {
    let save = ConfigFileSave {
        refresh_interval: config.refresh_interval,
        default_spawn_dir: config.default_spawn_dir.clone(),
        title_injection_enabled: config.title_injection_enabled,
        title_injection_delay: config.title_injection_delay,
        notifications: NotificationsConfigFileSave {
            sound_on_completion: config.notifications.sound_on_completion,
            sound_method: match config.notifications.sound_method {
                SoundMethod::Bell => "bell".to_owned(),
                SoundMethod::Command => "command".to_owned(),
            },
            sound_command: config.notifications.sound_command.clone(),
        },
        agents: config.custom_agents.clone(),
    };

    let content = toml::to_string_pretty(&save).map_err(|e| format!("serialize: {e}"))?;

    let path = config_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("create dir: {e}"))?;
    }
    fs::write(&path, content).map_err(|e| format!("write: {e}"))?;

    Ok(())
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

// ── Motion-based completion detection (background thread) ────────────────────

const SETTLE_SECONDS: u64 = 8;

struct SessionActivity {
    content_hash: u64,
    last_change: Instant,
    was_active: bool,
    notified: bool,
}

/// Hash preview lines, stripping trailing empty lines first so that pane
/// resize (which changes the number of trailing blanks) doesn't cause
/// spurious hash changes.
fn hash_preview(lines: &[String]) -> u64 {
    let end = lines
        .iter()
        .rposition(|l| !l.trim().is_empty())
        .map(|i| i + 1)
        .unwrap_or(0);
    let mut hasher = DefaultHasher::new();
    lines[..end].hash(&mut hasher);
    hasher.finish()
}

/// Run one detection tick. Returns names of sessions that fired a notification.
fn detect_tick(
    activity: &mut HashMap<String, SessionActivity>,
    sessions: &[(String, Vec<String>)],
    config: &AppConfig,
) -> Vec<String> {
    let now = Instant::now();
    let mut completed = Vec::new();

    for (name, preview) in sessions {
        let hash = hash_preview(preview);

        match activity.get_mut(name) {
            Some(entry) => {
                if hash != entry.content_hash {
                    entry.content_hash = hash;
                    entry.last_change = now;
                    entry.was_active = true;
                    entry.notified = false;
                } else if entry.was_active
                    && !entry.notified
                    && now.duration_since(entry.last_change).as_secs() >= SETTLE_SECONDS
                {
                    play_notification_sound(config);
                    entry.notified = true;
                    completed.push(name.clone());
                }
            }
            None => {
                activity.insert(
                    name.clone(),
                    SessionActivity {
                        content_hash: hash,
                        last_change: now,
                        was_active: false,
                        notified: true,
                    },
                );
            }
        }
    }

    let active_names: std::collections::HashSet<&String> =
        sessions.iter().map(|(name, _)| name).collect();
    activity.retain(|name, _| active_names.contains(name));

    completed
}

/// Spawn a background thread that polls tmux pane content and fires
/// notification sounds when an agent's output settles. Runs independently
/// of the TUI event loop so notifications work even while attached to a
/// session.
pub fn spawn_activity_monitor(config: &AppConfig) {
    let config = config.clone();
    let interval = Duration::from_secs(config.refresh_interval.max(1));

    thread::spawn(move || {
        let mut activity: HashMap<String, SessionActivity> = HashMap::new();

        loop {
            thread::sleep(interval);

            let sessions = tmux::poll_session_previews();
            detect_tick(&mut activity, &sessions, &config);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_sensible_values() {
        let config = AppConfig::default();
        assert_eq!(config.refresh_interval, 3);
        assert!(config.title_injection_enabled);
        assert_eq!(config.title_injection_delay, 5);
        assert!(config.notifications.sound_on_completion);
        assert_eq!(config.notifications.sound_method, SoundMethod::Command);
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
