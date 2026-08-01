//! Standalone WhatsApp pairing CLI tool.
//!
//! Run this binary to perform the initial QR code scan and persist
//! the session to the shared SQLite database.

use wa_client::client::{WhatsAppClient, WhatsAppEvent};
use wa_client::qr::QrRef;
use wa_domain::ports::WhatsAppClientPort;
use wa_mcp_server::event_store::persist_whatsapp_event;
use wa_storage_sqlite::SqliteStorage;
use std::time::Duration;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Use RUST_LOG env var if set, otherwise default to INFO (not TRACE — it floods the QR output)
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"))
        )
        .with_writer(std::io::stderr)
        .init();

    let db_path = std::env::var("WA_DB_PATH").unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        format!("{}/.whatsapp-mcp/whatsapp.db", home)
    });
    let replace_existing_session = std::env::args()
        .skip(1)
        .any(|arg| arg == "--replace-existing-session");

    // Ensure parent directory exists
    if let Some(parent) = std::path::Path::new(&db_path).parent() {
        std::fs::create_dir_all(parent)?;
    }

    eprintln!("🔐 WhatsApp Pairing Tool");
    eprintln!("   DB path: {}", db_path);

    // Re-pairing is destructive. Never delete a store without the explicit
    // recovery flag; normal first-time setup now happens inside Codex.
    if std::path::Path::new(&db_path).exists() {
        if !replace_existing_session {
            return Err(anyhow::anyhow!(
                "Refusing to delete existing database at {}. Back it up, verify WA_DB_PATH, then re-run with --replace-existing-session if destructive re-pairing is intended.",
                db_path
            ));
        }
        eprintln!("   Explicit replacement requested; removing existing database...");
        std::fs::remove_file(&db_path)?;
    }
    let storage = SqliteStorage::new(&db_path)?;

    // Phase 1: QR pairing
    let mut generated_qr_paths = Vec::new();
    let paired_jid = loop {
        eprintln!("   Connecting to WhatsApp Web...\n");
        let client = WhatsAppClient::with_db_path(&db_path);

        if let Err(e) = client.connect().await {
            eprintln!("❌ Connection failed: {}", e);
            eprintln!("⚠️  Retrying in 3s...");
            tokio::time::sleep(Duration::from_secs(3)).await;
            continue;
        }

        // Wait for events
        let mut paired_jid: Option<String> = None;
        let timeout_duration = Duration::from_secs(120);
        let deadline = tokio::time::Instant::now() + timeout_duration;

        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                eprintln!("\n⏰ Pairing timeout — retrying...");
                break;
            }

            match tokio::time::timeout(remaining, client.next_event()).await {
                Ok(Some(event)) => match event {
                    WhatsAppEvent::QrCode(data) => {
                        // Save QR as PNG file and auto-open with Preview
                        let qr_path = format!("{}/qr_code-{}.png",
                            std::path::Path::new(&db_path).parent()
                                .map(|p| p.to_string_lossy().to_string())
                                .unwrap_or_else(|| ".".to_string()),
                            chrono::Utc::now().timestamp_millis());
                        match save_qr_png(&data, &qr_path) {
                            Ok(()) => {
                                generated_qr_paths.push(qr_path.clone());
                                eprintln!("📱 QR code saved to: {}", qr_path);
                                eprintln!("   Opening with Preview...");
                                let _ = std::process::Command::new("open").arg(&qr_path).spawn();
                            }
                            Err(e) => eprintln!("⚠️  Failed to save QR PNG: {}", e),
                        }
                        eprintln!();
                        eprintln!("╔═══════════════════════════════════════════╗");
                        eprintln!("║  Scan this QR with WhatsApp:              ║");
                        eprintln!("║  Settings → Linked Devices → Link Device  ║");
                        eprintln!("╚═══════════════════════════════════════════╝");
                        eprintln!();
                        eprintln!("{}", QrRef::render_terminal(&data));
                    }
                    WhatsAppEvent::PairSuccess { jid } => {
                        eprintln!("✅ Pairing succeeded as: {}", jid);
                        eprintln!("   Session saved. Waiting for server disconnect before login reconnect...");
                        paired_jid = Some(jid);
                    }
                    WhatsAppEvent::Disconnected => {
                        if paired_jid.is_some() {
                            eprintln!("📡 Server disconnected after pairing (expected stream:error 515)");
                            break;
                        } else {
                            eprintln!("⚠️  Disconnected — retrying in 3s...");
                            break;
                        }
                    }
                    _ => {}
                },
                Ok(None) => {
                    eprintln!("⚠️  Event channel closed");
                    break;
                }
                Err(_) => {
                    eprintln!("\n⏰ Pairing timeout — retrying...");
                    break;
                }
            }
        }

        let _ = client.disconnect().await;

        if let Some(jid) = paired_jid {
            break jid;
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    };
    for qr_path in generated_qr_paths {
        let _ = std::fs::remove_file(qr_path);
    }

    // Phase 2: Login reconnection with saved credentials
    eprintln!();
    eprintln!("🔄 Phase 2: Reconnecting with login credentials...");
    eprintln!("   JID: {}", paired_jid);
    tokio::time::sleep(Duration::from_secs(2)).await;

    let mut login_attempts = 0;
    loop {
        login_attempts += 1;
        if login_attempts > 5 {
            eprintln!("❌ Login failed after 5 attempts");
            return Err(anyhow::anyhow!("Login reconnection failed after 5 attempts"));
        }

        eprintln!("   Login attempt {}...", login_attempts);
        let client = WhatsAppClient::with_db_path(&db_path);

        if let Err(e) = client.connect().await {
            eprintln!("❌ Login connection failed: {}", e);
            eprintln!("⚠️  Retrying in 3s...");
            tokio::time::sleep(Duration::from_secs(3)).await;
            continue;
        }

        // Wait for Connected event (login success) or failure
        let login_timeout = Duration::from_secs(30);
        let mut login_success = false;

        match tokio::time::timeout(login_timeout, async {
            loop {
                match client.next_event().await {
                    Some(WhatsAppEvent::Connected { jid }) => {
                        eprintln!("✅ Login successful! Connected as: {}", jid);
                        return true;
                    }
                    Some(WhatsAppEvent::Disconnected) => {
                        eprintln!("⚠️  Disconnected during login");
                        return false;
                    }
                    Some(other) => {
                        eprintln!("   Event during login: {:?}", other);
                    }
                    None => {
                        eprintln!("⚠️  Event channel closed during login");
                        return false;
                    }
                }
            }
        }).await {
            Ok(true) => login_success = true,
            Ok(false) => {}
            Err(_) => eprintln!("⏰ Login timeout"),
        }

        if login_success {
            eprintln!("   Waiting for initial WhatsApp history sync...");
            let sync_deadline = tokio::time::Instant::now() + Duration::from_secs(60);
            let mut idle_deadline = tokio::time::Instant::now() + Duration::from_secs(30);
            let mut synced_chats = 0usize;
            let mut synced_messages = 0usize;

            loop {
                let now = tokio::time::Instant::now();
                let next_deadline = std::cmp::min(sync_deadline, idle_deadline);
                if now >= next_deadline {
                    break;
                }
                match tokio::time::timeout(next_deadline - now, client.next_event()).await {
                    Ok(Some(event)) => {
                        if let Err(error) = persist_whatsapp_event(&storage, &event).await {
                            eprintln!("⚠️  Failed to persist WhatsApp sync event: {}", error);
                        }
                        match event {
                            WhatsAppEvent::HistorySyncBatch { chats, messages, progress } => {
                                synced_chats += chats.len();
                                synced_messages += messages.len();
                                idle_deadline = tokio::time::Instant::now() + Duration::from_secs(5);
                                if progress.is_some_and(|value| value >= 100) {
                                    break;
                                }
                            }
                            WhatsAppEvent::Disconnected => break,
                            _ => {}
                        }
                    }
                    Ok(None) | Err(_) => break,
                }
            }
            eprintln!(
                "   History sync persisted: {} chats, {} messages",
                synced_chats, synced_messages
            );

            eprintln!();
            eprintln!("🎉 WhatsApp connection fully established!");
            eprintln!("   JID: {}", paired_jid);
            eprintln!("   DB:  {}", db_path);
            eprintln!();
            eprintln!("   You can now use the MCP server!");

            let _ = client.disconnect().await;
            break;
        }

        let _ = client.disconnect().await;
        tokio::time::sleep(Duration::from_secs(3)).await;
    }

    Ok(())
}

fn save_qr_png(data: &str, path: &str) -> anyhow::Result<()> {
    use qrcode::{QrCode, EcLevel};
    use image::{Luma, ImageBuffer};

    let code = QrCode::with_error_correction_level(data.as_bytes(), EcLevel::L)?;
    let module_size = 10u32; // pixels per module
    let quiet_zone = 4u32; // modules of white border
    let width = code.width() as u32;
    let img_size = (width + quiet_zone * 2) * module_size;

    let colors: Vec<bool> = code.into_colors().into_iter()
        .map(|c| c == qrcode::Color::Dark)
        .collect();

    let img = ImageBuffer::from_fn(img_size, img_size, |x, y| {
        let mx = x / module_size;
        let my = y / module_size;
        if mx >= quiet_zone && mx < width + quiet_zone && my >= quiet_zone && my < width + quiet_zone {
            let idx = ((my - quiet_zone) * width + (mx - quiet_zone)) as usize;
            if idx < colors.len() && colors[idx] {
                Luma([0u8]) // black
            } else {
                Luma([255u8]) // white
            }
        } else {
            Luma([255u8]) // white border
        }
    });

    img.save(path)?;
    Ok(())
}
