//! WhatsApp MCP Server — Go Bridge Backend
//!
//! Uses the Go whatsmeow bridge as the WhatsApp backend.
//! The bridge must be running on localhost (default: port 9876).
//!
//! # Usage
//! ```bash
//! # Start the Go bridge first:
//! cd bridge && BRIDGE_DB_PATH=whatsmeow.db ./wa-bridge
//!
//! # Then start this MCP server:
//! BRIDGE_URL=http://127.0.0.1:9876 ./wa-mcp-bridge
//! ```

use anyhow::Result;
use std::sync::Arc;
use wa_mcp_server::bridge::BridgeClient;
use wa_mcp_server::server::McpServer;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let bridge_url = std::env::var("BRIDGE_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:9876".to_string());

    tracing::info!("Starting WhatsApp MCP Server (bridge mode)");
    tracing::info!("Bridge URL: {}", bridge_url);

    let bridge = Arc::new(BridgeClient::new(&bridge_url));

    // Verify bridge is reachable
    match bridge.health().await {
        Ok(status) => {
            let connected = status["connected"].as_bool().unwrap_or(false);
            let logged_in = status["logged_in"].as_bool().unwrap_or(false);
            tracing::info!(
                "Bridge status: connected={}, logged_in={}",
                connected,
                logged_in
            );
            if !logged_in {
                tracing::warn!(
                    "Bridge is not logged in. Pair first: cd bridge && BRIDGE_DB_PATH=whatsmeow.db ./wa-bridge, then POST /rpc connect"
                );
            }
        }
        Err(e) => {
            tracing::error!("Cannot reach bridge at {}: {}", bridge_url, e);
            tracing::error!("Start the bridge first: cd bridge && BRIDGE_DB_PATH=whatsmeow.db ./wa-bridge");
            return Err(e);
        }
    }

    // BridgeClient implements both WhatsAppClientPort and StoragePort
    let server = McpServer::new(bridge.clone(), bridge);
    server.run_stdio().await?;

    Ok(())
}
