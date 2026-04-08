use anyhow::Result;
use std::sync::Arc;
use wa_client::client::WhatsAppClient;
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
    
    // Auto-connect in background
    let wa_clone = wa.clone();
    tokio::spawn(async move {
        if let Err(e) = wa_clone.connect().await {
            tracing::warn!("Auto-connect failed: {} — pair first with wa-pair", e);
        }
    });

    let storage = Arc::new(wa_storage_sqlite::SqliteStorage::new(&db_path)?);
    let server = McpServer::new(storage, wa);
    server.run_stdio().await?;

    Ok(())
}
