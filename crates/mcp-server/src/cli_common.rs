//! Shared CLI utilities for WhatsApp binary tools.
//!
//! Extracts common patterns: DB path resolution, client creation,
//! connection handling, session clearing, and event polling.

use wa_client::client::{WhatsAppClient, WhatsAppEvent};
use wa_domain::ports::WhatsAppClientPort;
use std::time::Duration;

/// Resolve the WhatsApp database path from env or default.
pub fn resolve_db_path() -> String {
    std::env::var("WA_DB_PATH").unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        format!("{}/.whatsapp-mcp/whatsapp.db", home)
    })
}

/// Initialize tracing subscriber with stderr output.
pub fn init_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
}

/// Clear cached Signal sessions for a specific phone number.
pub fn clear_sessions_for(db_path: &str, phone: &str) -> anyhow::Result<()> {
    let conn = rusqlite::Connection::open(db_path)?;
    if let Some(mut store) = wa_client::store::DeviceStore::load_from_db(&conn)? {
        let keys_to_remove: Vec<String> = store
            .sessions
            .keys()
            .filter(|k| k.starts_with(phone))
            .cloned()
            .collect();
        for k in &keys_to_remove {
            store.sessions.remove(k);
            eprintln!("   Cleared session: {}", k);
        }
        if keys_to_remove.is_empty() {
            eprintln!("   No cached sessions found for {}", phone);
        }
        store.save_to_db(&conn)?;
    }
    Ok(())
}

/// Create a WhatsApp client from the DB path, verifying the DB exists.
pub fn create_client(db_path: &str) -> anyhow::Result<WhatsAppClient> {
    if !std::path::Path::new(db_path).exists() {
        anyhow::bail!("No session found at {}. Run wa-pair first.", db_path);
    }
    Ok(WhatsAppClient::with_db_path(db_path))
}

/// Connect the client and wait for the Connected event.
/// Returns the connected JID on success.
pub async fn connect_and_wait(client: &WhatsAppClient, timeout_secs: u64) -> anyhow::Result<String> {
    client.connect().await?;

    let result = tokio::time::timeout(Duration::from_secs(timeout_secs), async {
        loop {
            match client.next_event().await {
                Some(WhatsAppEvent::Connected { jid }) => return Ok(jid),
                Some(WhatsAppEvent::Disconnected) => {
                    return Err(anyhow::anyhow!("Disconnected during login"))
                }
                Some(_) => continue,
                None => return Err(anyhow::anyhow!("Event channel closed")),
            }
        }
    })
    .await
    .map_err(|_| anyhow::anyhow!("Connection timed out after {}s", timeout_secs))??;

    Ok(result)
}

/// Read phone numbers from a file (one per line, ignoring empty lines and # comments).
pub fn read_contacts_file(path: &str) -> anyhow::Result<Vec<String>> {
    let content = std::fs::read_to_string(path)?;
    let phones: Vec<String> = content
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.to_string())
        .collect();
    if phones.is_empty() {
        anyhow::bail!("No phone numbers found in {}", path);
    }
    Ok(phones)
}
