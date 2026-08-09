use anyhow::Result;
use std::sync::Arc;
use wa_client::client::{WhatsAppClient, WhatsAppEvent};
use wa_domain::ports::{StoragePort, WhatsAppClientPort};
use wa_mcp_server::event_store::persist_whatsapp_event;
#[cfg(unix)]
use wa_mcp_server::runtime::{RuntimeEndpoint, claim_or_connect, proxy_stdio, socket_path};
use wa_mcp_server::server::McpServer;

fn unix_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn start_whatsapp_runtime(
    wa: Arc<WhatsAppClient>,
    storage: Arc<wa_storage_sqlite::SqliteStorage>,
) {
    let wa_clone = wa.clone();
    let storage_clone = storage.clone();
    tokio::spawn(async move {
        if let Err(error) = wa_clone.connect().await {
            tracing::warn!("Auto-connect failed: {}", error);
        }

        let heartbeat_client = wa_clone.clone();
        let heartbeat_storage = storage_clone.clone();
        tokio::spawn(async move {
            loop {
                let connected = heartbeat_client.is_connected().await.unwrap_or(false);
                if let Err(error) = heartbeat_storage.set_runtime_connection(connected, unix_time_ms()).await {
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
                WhatsAppEvent::PairSuccess { .. } => reconnect_after_pairing = true,
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
}

#[cfg(unix)]
async fn run_owner(
    listener: tokio::net::UnixListener,
    storage: Arc<wa_storage_sqlite::SqliteStorage>,
    wa: Arc<WhatsAppClient>,
) -> Result<()> {
    let stdio_server = McpServer::new(storage.clone(), wa.clone());
    tokio::spawn(async move {
        if let Err(error) = stdio_server.run_stdio().await {
            tracing::warn!("WhatsApp owner stdio connection ended: {}", error);
        }
    });

    loop {
        let (stream, _) = listener.accept().await?;
        let connection_storage = storage.clone();
        let connection_client = wa.clone();
        tokio::spawn(async move {
            let (reader, writer) = stream.into_split();
            let server = McpServer::new(connection_storage, connection_client);
            if let Err(error) = server.run_transport(reader, writer).await {
                tracing::warn!("WhatsApp runtime client ended: {}", error);
            }
        });
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_writer(std::io::stderr).init();
    let db_path = std::env::var("WA_DB_PATH").unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        format!("{}/.whatsapp-mcp/whatsapp.db", home)
    });
    if let Some(parent) = std::path::Path::new(&db_path).parent() {
        std::fs::create_dir_all(parent)?;
    }

    #[cfg(unix)]
    let listener = match claim_or_connect(&socket_path(&db_path)).await? {
        RuntimeEndpoint::Client(stream) => return proxy_stdio(stream).await,
        RuntimeEndpoint::Owner(listener) => listener,
    };

    let wa = Arc::new(WhatsAppClient::with_db_path(&db_path));
    let storage = Arc::new(wa_storage_sqlite::SqliteStorage::new(&db_path)?);
    start_whatsapp_runtime(wa.clone(), storage.clone());

    #[cfg(unix)]
    return run_owner(listener, storage, wa).await;

    #[cfg(not(unix))]
    {
        let server = McpServer::new(storage, wa);
        server.run_stdio().await?;
        Ok(())
    }
}
