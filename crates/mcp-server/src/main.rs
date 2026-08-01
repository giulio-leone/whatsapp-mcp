use anyhow::Result;
use std::sync::Arc;
use wa_client::client::{WhatsAppClient, WhatsAppEvent};
use wa_domain::ports::WhatsAppClientPort;
use wa_mcp_server::server::McpServer;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();

    let db_path = std::env::var("WA_DB_PATH").unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        format!("{}/.whatsapp-mcp/whatsapp.db", home)
    });

    // Ensure parent directory exists
    if let Some(parent) = std::path::Path::new(&db_path).parent() {
        std::fs::create_dir_all(parent)?;
    }

    let wa = Arc::new(WhatsAppClient::with_db_path(&db_path));
    let storage = Arc::new(wa_storage_sqlite::SqliteStorage::new(&db_path)?);

    // Auto-connect and complete the post-pair login reconnect in background.
    // McpServer serves both the stateless 2026-07-28 and legacy initialize eras.
    let wa_clone = wa.clone();
    tokio::spawn(async move {
        if let Err(e) = wa_clone.connect().await {
            tracing::warn!("Auto-connect failed: {}", e);
            return;
        }

        let mut reconnect_after_pairing = false;
        while let Some(event) = wa_clone.next_event().await {
            match event {
                WhatsAppEvent::PairSuccess { .. } => {
                    reconnect_after_pairing = true;
                }
                WhatsAppEvent::Disconnected if reconnect_after_pairing => {
                    reconnect_after_pairing = false;
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    if let Err(error) = wa_clone.connect().await {
                        tracing::warn!("Post-pair login reconnect failed: {}", error);
                    }
                }
                _ => {}
            }
        }
    });

    let server = McpServer::new(storage, wa);
    server.run_stdio().await?;

    Ok(())
}
