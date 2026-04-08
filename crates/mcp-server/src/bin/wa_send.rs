//! Quick send test — connects with saved session and sends a text message.

use wa_client::client::{WhatsAppClient, WhatsAppEvent};
use wa_domain::ports::WhatsAppClientPort;
use wa_domain::models::chat::ChatId;
use std::time::Duration;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"))
        )
        .with_writer(std::io::stderr)
        .init();

    let args: Vec<String> = std::env::args().collect();
    // Parse --fresh flag
    let fresh = args.iter().any(|a| a == "--fresh");
    let positional: Vec<&String> = args.iter().skip(1).filter(|a| !a.starts_with("--")).collect();
    if positional.len() < 2 {
        eprintln!("Usage: wa-send [--fresh] <phone_number> <message>");
        eprintln!("  --fresh   Clear cached Signal session to force new key exchange");
        eprintln!("  Example: wa-send --fresh 393661410914 'Ciao, test!'");
        std::process::exit(1);
    }

    let phone = positional[0];
    let text = positional[1];
    let jid = format!("{}@s.whatsapp.net", phone);

    let db_path = std::env::var("WA_DB_PATH").unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        format!("{}/.whatsapp-mcp/whatsapp.db", home)
    });

    eprintln!("📤 WhatsApp Send Test");
    eprintln!("   To: {} ({})", phone, jid);
    eprintln!("   Text: {}", text);
    eprintln!("   DB: {}", db_path);
    if fresh {
        eprintln!("   🔄 Fresh mode: clearing cached sessions for {}", phone);
    }

    if !std::path::Path::new(&db_path).exists() {
        eprintln!("❌ No session found at {}. Run wa-pair first.", db_path);
        std::process::exit(1);
    }

    // If --fresh, clear Signal sessions for this recipient
    if fresh {
        let conn = rusqlite::Connection::open(&db_path)?;
        if let Some(mut store) = wa_client::store::DeviceStore::load_from_db(&conn)? {
            // Remove sessions for all devices of this user
            let keys_to_remove: Vec<String> = store.sessions.keys()
                .filter(|k| k.starts_with(phone))
                .cloned()
                .collect();
            for k in &keys_to_remove {
                store.sessions.remove(k);
                eprintln!("   Cleared session: {}", k);
            }
            if keys_to_remove.is_empty() {
                eprintln!("   No cached sessions found");
            }
            store.save_to_db(&conn)?;
        }
    }

    let client = WhatsAppClient::with_db_path(&db_path);

    eprintln!("   Connecting...");
    client.connect().await?;

    // Wait for Connected event
    let connected = tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            match client.next_event().await {
                Some(WhatsAppEvent::Connected { jid }) => {
                    eprintln!("✅ Connected as: {}", jid);
                    return true;
                }
                Some(WhatsAppEvent::Disconnected) => {
                    eprintln!("❌ Disconnected");
                    return false;
                }
                Some(other) => {
                    eprintln!("   Event: {:?}", other);
                }
                None => return false,
            }
        }
    }).await;

    match connected {
        Ok(true) => {}
        _ => {
            eprintln!("❌ Failed to connect");
            std::process::exit(1);
        }
    }

    // Send message
    eprintln!("   Sending message...");
    match client.send_message(&ChatId(jid.clone()), text).await {
        Ok(msg) => {
            eprintln!("✅ Message sent!");
            eprintln!("   ID: {}", msg.id.0);
            eprintln!("   Timestamp: {}", msg.timestamp);
        }
        Err(e) => {
            eprintln!("❌ Send failed: {}", e);
        }
    }

    // Wait briefly for server ACK
    tokio::time::sleep(Duration::from_secs(3)).await;
    let _ = client.disconnect().await;

    Ok(())
}
