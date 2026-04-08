use crate::poll_config::{ActionConfig, EventFilter, TriggerConfig};
use wa_client::client::WhatsAppEvent;
use anyhow::Result;
use chrono::Local;
use serde_json::json;
use std::collections::HashMap;

// ─── Event Context (template variables) ─────────────────────────────

#[derive(Debug, Clone)]
pub struct EventContext {
    pub vars: HashMap<String, String>,
    pub event_type: String,
    pub raw_json: serde_json::Value,
}

impl EventContext {
    pub fn from_event(event: &WhatsAppEvent) -> Self {
        let now = Local::now().format("%Y-%m-%dT%H:%M:%S").to_string();
        let mut vars = HashMap::new();
        vars.insert("timestamp".into(), now);

        let (event_type, raw_json) = match event {
            WhatsAppEvent::MessageReceived(msg) => {
                vars.insert("jid".into(), msg.sender_id.clone());
                vars.insert("chat_id".into(), msg.chat_id.0.clone());
                vars.insert("text".into(), msg.text.clone().unwrap_or_default());
                vars.insert("message_id".into(), msg.id.0.clone());
                vars.insert("is_from_me".into(), msg.is_from_me.to_string());
                ("MessageReceived".into(), json!({
                    "event": "MessageReceived",
                    "jid": msg.sender_id,
                    "chat_id": msg.chat_id.0,
                    "text": msg.text,
                    "message_id": msg.id.0,
                    "timestamp": msg.timestamp,
                    "is_from_me": msg.is_from_me,
                }))
            }
            WhatsAppEvent::PresenceUpdate { jid, available, last_seen } => {
                vars.insert("jid".into(), jid.clone());
                vars.insert("available".into(), available.to_string());
                vars.insert("status".into(), if *available { "online" } else { "offline" }.into());
                if let Some(ts) = last_seen {
                    vars.insert("last_seen".into(), ts.to_string());
                }
                ("PresenceUpdate".into(), json!({
                    "event": "PresenceUpdate",
                    "jid": jid,
                    "available": available,
                    "last_seen": last_seen,
                }))
            }
            WhatsAppEvent::StatusReceived { from, text, media_type, timestamp } => {
                vars.insert("jid".into(), from.clone());
                vars.insert("text".into(), text.clone().unwrap_or_default());
                if let Some(mt) = media_type {
                    vars.insert("media_type".into(), mt.clone());
                }
                ("StatusReceived".into(), json!({
                    "event": "StatusReceived",
                    "jid": from,
                    "text": text,
                    "media_type": media_type,
                    "timestamp": timestamp,
                }))
            }
            WhatsAppEvent::ReceiptReceived { id, from, timestamp } => {
                vars.insert("jid".into(), from.clone());
                vars.insert("message_id".into(), id.clone());
                ("ReceiptReceived".into(), json!({
                    "event": "ReceiptReceived",
                    "id": id,
                    "from": from,
                    "timestamp": timestamp,
                }))
            }
            WhatsAppEvent::Connected { jid } => {
                vars.insert("jid".into(), jid.clone());
                ("Connected".into(), json!({ "event": "Connected", "jid": jid }))
            }
            WhatsAppEvent::Disconnected => {
                ("Disconnected".into(), json!({ "event": "Disconnected" }))
            }
            _ => {
                ("Unknown".into(), json!({ "event": "Unknown" }))
            }
        };

        Self { vars, event_type, raw_json }
    }

    /// Replace {var} placeholders in a template string
    pub fn render(&self, template: &str) -> String {
        let mut result = template.to_string();
        for (key, value) in &self.vars {
            result = result.replace(&format!("{{{}}}", key), value);
        }
        // Also replace {json} with the full JSON
        if result.contains("{json}") {
            result = result.replace("{json}", &self.raw_json.to_string());
        }
        result
    }
}

// ─── Event Matching ─────────────────────────────────────────────────

fn phone_from_jid(jid: &str) -> String {
    jid.split('@').next().unwrap_or("").split(':').next().unwrap_or("").to_string()
}

fn contacts_match(contacts: &Option<Vec<String>>, jid: &str) -> bool {
    match contacts {
        None => true,
        Some(list) if list.is_empty() => true,
        Some(list) => {
            let phone = phone_from_jid(jid);
            list.iter().any(|c| c == &phone || c == jid)
        }
    }
}

pub fn event_matches_filter(event: &WhatsAppEvent, filter: &EventFilter) -> bool {
    match (event, filter) {
        (WhatsAppEvent::MessageReceived(msg), EventFilter::MessageReceived { from_regex, text_contains, contacts }) => {
            if !contacts_match(contacts, &msg.sender_id) { return false; }
            if let Some(pattern) = from_regex {
                if let Ok(re) = regex::Regex::new(pattern) {
                    if !re.is_match(&msg.sender_id) { return false; }
                }
            }
            if let Some(needle) = text_contains {
                if let Some(text) = &msg.text {
                    if !text.to_lowercase().contains(&needle.to_lowercase()) { return false; }
                } else {
                    return false;
                }
            }
            true
        }
        (WhatsAppEvent::PresenceUpdate { jid, available, .. }, EventFilter::PresenceUpdate { contacts, only_online, only_offline }) => {
            if !contacts_match(contacts, jid) { return false; }
            if only_online.unwrap_or(false) && !available { return false; }
            if only_offline.unwrap_or(false) && *available { return false; }
            true
        }
        (WhatsAppEvent::StatusReceived { from, .. }, EventFilter::StatusReceived { contacts }) => {
            contacts_match(contacts, from)
        }
        (WhatsAppEvent::Connected { .. }, EventFilter::Connected) => true,
        (WhatsAppEvent::Disconnected, EventFilter::Disconnected) => true,
        (WhatsAppEvent::ReceiptReceived { from, .. }, EventFilter::ReceiptReceived { contacts }) => {
            contacts_match(contacts, from)
        }
        _ => false,
    }
}

pub fn trigger_matches(event: &WhatsAppEvent, trigger: &TriggerConfig) -> bool {
    if !trigger.is_enabled() { return false; }
    trigger.events.iter().any(|f| event_matches_filter(event, f))
}

// ─── Action Execution ───────────────────────────────────────────────

pub async fn execute_actions(actions: &[ActionConfig], ctx: &EventContext) -> Vec<ActionResult> {
    let mut results = Vec::new();
    for action in actions {
        let result = execute_action(action, ctx).await;
        results.push(result);
    }
    results
}

#[derive(Debug)]
pub struct ActionResult {
    pub action_type: String,
    pub success: bool,
    pub detail: String,
}

async fn execute_action(action: &ActionConfig, ctx: &EventContext) -> ActionResult {
    match action {
        ActionConfig::HttpPost { url, headers, timeout_secs } => {
            execute_http_post(url, headers, *timeout_secs, ctx).await
        }
        ActionConfig::FileAppend { path, format } => {
            execute_file_append(path, format, ctx)
        }
        ActionConfig::Command { cmd, shell } => {
            execute_command(cmd, shell, ctx).await
        }
    }
}

async fn execute_http_post(
    url: &str,
    headers: &Option<HashMap<String, String>>,
    timeout_secs: u64,
    ctx: &EventContext,
) -> ActionResult {
    let rendered_url = ctx.render(url);
    let client = reqwest::Client::new();
    let mut req = client
        .post(&rendered_url)
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .json(&ctx.raw_json);

    if let Some(hdrs) = headers {
        for (k, v) in hdrs {
            req = req.header(k, ctx.render(v));
        }
    }

    match req.send().await {
        Ok(resp) => {
            let status = resp.status();
            ActionResult {
                action_type: "http_post".into(),
                success: status.is_success(),
                detail: format!("POST {} → {}", rendered_url, status),
            }
        }
        Err(e) => ActionResult {
            action_type: "http_post".into(),
            success: false,
            detail: format!("POST {} failed: {}", rendered_url, e),
        }
    }
}

fn execute_file_append(path: &str, format: &str, ctx: &EventContext) -> ActionResult {
    let rendered_path = ctx.render(path);
    let line = match format {
        "json" => format!("{}\n", ctx.raw_json),
        _ => {
            let ts = ctx.vars.get("timestamp").cloned().unwrap_or_default();
            format!("[{}] {} {}\n", ts, ctx.event_type,
                ctx.vars.get("jid").cloned().unwrap_or_default())
        }
    };

    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&rendered_path)
    {
        Ok(mut file) => {
            use std::io::Write;
            match file.write_all(line.as_bytes()) {
                Ok(_) => ActionResult {
                    action_type: "file_append".into(),
                    success: true,
                    detail: format!("Appended to {}", rendered_path),
                },
                Err(e) => ActionResult {
                    action_type: "file_append".into(),
                    success: false,
                    detail: format!("Write error: {}", e),
                },
            }
        }
        Err(e) => ActionResult {
            action_type: "file_append".into(),
            success: false,
            detail: format!("Open error: {}", e),
        }
    }
}

async fn execute_command(cmd: &str, shell: &Option<String>, ctx: &EventContext) -> ActionResult {
    let rendered_cmd = ctx.render(cmd);
    let shell_bin = shell.as_deref().unwrap_or("/bin/sh");

    match tokio::process::Command::new(shell_bin)
        .arg("-c")
        .arg(&rendered_cmd)
        .output()
        .await
    {
        Ok(output) => {
            let success = output.status.success();
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            ActionResult {
                action_type: "command".into(),
                success,
                detail: if success {
                    format!("OK: {}", if stdout.is_empty() { "(no output)" } else { &stdout })
                } else {
                    format!("FAIL ({}): {}", output.status, stderr)
                },
            }
        }
        Err(e) => ActionResult {
            action_type: "command".into(),
            success: false,
            detail: format!("Exec error: {}", e),
        }
    }
}
