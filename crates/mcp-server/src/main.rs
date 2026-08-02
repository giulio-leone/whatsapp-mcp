use anyhow::Result;
use std::sync::Arc;
use wa_client::client::{WhatsAppClient, WhatsAppEvent};
use wa_domain::ports::{StoragePort, WhatsAppClientPort};
use wa_mcp_server::event_store::persist_whatsapp_event;
use wa_mcp_server::server::McpServer;

fn unix_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

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
    let storage_clone = storage.clone();
    tokio::spawn(async move {
        if let Err(e) = wa_clone.connect().await {
            tracing::warn!("Auto-connect failed: {}", e);
            return;
        }

        let heartbeat_client = wa_clone.clone();
        let heartbeat_storage = storage_clone.clone();
        tokio::spawn(async move {
            loop {
                let connected = heartbeat_client.is_connected().await.unwrap_or(false);
                if let Err(error) = heartbeat_storage
                    .set_runtime_connection(connected, unix_time_ms())
                    .await
                {
                    tracing::warn!("Failed to publish WhatsApp runtime state: {}", error);
                }
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        });

        let mut reconnect_after_pairing = false;
        while let Some(event) = wa_clone.next_event().await {
            if let Err(error) = persist_whatsapp_event(storage_clone.as_ref(), &event).await {
                tracing::warn!("Failed to persist WhatsApp event: {}", error);
            }
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
