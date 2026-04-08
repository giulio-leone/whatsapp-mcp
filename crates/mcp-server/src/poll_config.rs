use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use anyhow::{Result, Context};

// ─── Top-Level Config ───────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct PollConfig {
    #[serde(default)]
    pub daemon: DaemonConfig,
    pub triggers: Vec<TriggerConfig>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DaemonConfig {
    #[serde(default = "default_db_path")]
    pub db_path: String,
    #[serde(default = "default_log_level")]
    pub log_level: String,
    #[serde(default)]
    pub fresh_sessions: bool,
}

fn default_db_path() -> String {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    format!("{}/.whatsapp-mcp/whatsapp.db", home)
}

fn default_log_level() -> String { "info".into() }

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            db_path: default_db_path(),
            log_level: default_log_level(),
            fresh_sessions: false,
        }
    }
}

// ─── Trigger Config ─────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TriggerConfig {
    pub name: String,
    #[serde(default)]
    pub enabled: Option<bool>,
    pub events: Vec<EventFilter>,
    pub actions: Vec<ActionConfig>,
}

impl TriggerConfig {
    pub fn is_enabled(&self) -> bool {
        self.enabled.unwrap_or(true)
    }
}

// ─── Event Filters ──────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type")]
pub enum EventFilter {
    #[serde(rename = "MessageReceived")]
    MessageReceived {
        #[serde(default)]
        from_regex: Option<String>,
        #[serde(default)]
        text_contains: Option<String>,
        #[serde(default)]
        contacts: Option<Vec<String>>,
    },
    #[serde(rename = "PresenceUpdate")]
    PresenceUpdate {
        #[serde(default)]
        contacts: Option<Vec<String>>,
        #[serde(default)]
        only_online: Option<bool>,
        #[serde(default)]
        only_offline: Option<bool>,
    },
    #[serde(rename = "StatusReceived")]
    StatusReceived {
        #[serde(default)]
        contacts: Option<Vec<String>>,
    },
    #[serde(rename = "Connected")]
    Connected,
    #[serde(rename = "Disconnected")]
    Disconnected,
    #[serde(rename = "ReceiptReceived")]
    ReceiptReceived {
        #[serde(default)]
        contacts: Option<Vec<String>>,
    },
}

// ─── Action Configs ─────────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "type")]
pub enum ActionConfig {
    #[serde(rename = "http_post")]
    HttpPost {
        url: String,
        #[serde(default)]
        headers: Option<std::collections::HashMap<String, String>>,
        #[serde(default = "default_timeout")]
        timeout_secs: u64,
    },
    #[serde(rename = "file_append")]
    FileAppend {
        path: String,
        #[serde(default = "default_file_format")]
        format: String,
    },
    #[serde(rename = "command")]
    Command {
        cmd: String,
        #[serde(default)]
        shell: Option<String>,
    },
}

fn default_timeout() -> u64 { 5 }
fn default_file_format() -> String { "json".into() }

// ─── Loading ────────────────────────────────────────────────────────

impl PollConfig {
    pub fn load(path: &PathBuf) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read config file: {}", path.display()))?;
        let config: PollConfig = serde_yaml::from_str(&content)
            .with_context(|| format!("Failed to parse YAML config: {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        if self.triggers.is_empty() {
            anyhow::bail!("Config must have at least one trigger");
        }
        for trigger in &self.triggers {
            if trigger.name.is_empty() {
                anyhow::bail!("Trigger name cannot be empty");
            }
            if trigger.events.is_empty() {
                anyhow::bail!("Trigger '{}' must have at least one event filter", trigger.name);
            }
            if trigger.actions.is_empty() {
                anyhow::bail!("Trigger '{}' must have at least one action", trigger.name);
            }
            for action in &trigger.actions {
                match action {
                    ActionConfig::HttpPost { url, .. } => {
                        if !url.starts_with("http://") && !url.starts_with("https://") {
                            anyhow::bail!("Trigger '{}': HTTP URL must start with http:// or https://", trigger.name);
                        }
                    }
                    ActionConfig::FileAppend { path, .. } => {
                        if path.is_empty() {
                            anyhow::bail!("Trigger '{}': file path cannot be empty", trigger.name);
                        }
                    }
                    ActionConfig::Command { cmd, .. } => {
                        if cmd.is_empty() {
                            anyhow::bail!("Trigger '{}': command cannot be empty", trigger.name);
                        }
                    }
                }
            }
        }
        Ok(())
    }
}
