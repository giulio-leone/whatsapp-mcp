use anyhow::Result;
use std::sync::Arc;
use wa_mcp_server::server::McpServer;
use wa_storage_sqlite::SqliteStorage;
use wa_client::client::{WhatsAppClient, WhatsAppEvent};
use wa_domain::ports::{WhatsAppClientPort, StoragePort};

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .init();

    // 1. Initialize Storage
    let db_path = std::env::var("WA_DB_PATH").unwrap_or_else(|_| "whatsapp.db".to_string());
    let storage = Arc::new(SqliteStorage::new(&db_path)?);

    // 2. Initialize WhatsApp Client (loads existing keys from DB if present)
    let wa_client = Arc::new(WhatsAppClient::with_db_path(&db_path));

    // 3. Attempt connection in background
    let connect_client = wa_client.clone();
    tokio::spawn(async move {
        match connect_client.connect().await {
            Ok(()) => tracing::info!("WhatsApp connection initiated"),
            Err(e) => tracing::warn!("Initial connection attempt failed: {} — will retry on demand", e),
        }
    });

    // 4. Background event loop — handles incoming messages, QR codes, etc.
    let event_client = wa_client.clone();
    let event_storage = storage.clone();
    tokio::spawn(async move {
        loop {
            if let Some(event) = event_client.next_event().await {
                match event {
                    WhatsAppEvent::QrCode(data) => {
                        let terminal_qr = wa_client::qr::QrRef::render_terminal(&data);
                        eprintln!("\n╔═══════════════════════════════════════════╗");
                        eprintln!("║  Scan this QR code with WhatsApp          ║");
                        eprintln!("║  (Settings → Linked Devices → Link)       ║");
                        eprintln!("╚═══════════════════════════════════════════╝\n");
                        eprintln!("{}", terminal_qr);
                    }
                    WhatsAppEvent::Connected { jid } => {
                        tracing::info!("Connected as {}", jid);
                    }
                    WhatsAppEvent::MessageReceived(msg) => {
                        tracing::info!("Message from {}: {:?}", msg.sender_id, msg.text);
                        if let Err(e) = event_storage.save_message(&msg).await {
                            tracing::warn!("Failed to persist message: {}", e);
                        }
                    }
                    WhatsAppEvent::ReceiptReceived { id, from, .. } => {
                        tracing::debug!("Receipt {} from {}", id, from);
                    }
                    WhatsAppEvent::HistorySynced { chat_count } => {
                        tracing::info!("History sync complete: {} chats loaded", chat_count);
                        // Persist synced chats to SQLite
                        let chats = event_client.store.lock().await.chats.values().cloned().collect::<Vec<_>>();
                        for chat in &chats {
                            if let Err(e) = event_storage.save_chat(chat).await {
                                tracing::warn!("Failed to persist chat: {}", e);
                            }
                        }
                    }
                    WhatsAppEvent::Disconnected => {
                        tracing::warn!("Disconnected from WhatsApp");
                        // Auto-reconnect after a delay
                        let reconnect_client = event_client.clone();
                        tokio::spawn(async move {
                            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                            tracing::info!("Attempting reconnection...");
                            match reconnect_client.connect().await {
                                Ok(()) => tracing::info!("Reconnected successfully"),
                                Err(e) => tracing::warn!("Reconnection failed: {}", e),
                            }
                        });
                    }
                }
            } else {
                break;
            }
        }
    });

    // 5. Run MCP server over stdio (blocks until stdin closes)
    let server = McpServer::new(storage, wa_client);
    server.run_stdio().await?;

    Ok(())
}
