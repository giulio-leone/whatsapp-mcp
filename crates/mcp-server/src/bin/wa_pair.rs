//! Standalone WhatsApp pairing CLI tool.
//!
//! Run this binary to perform the initial QR code scan and persist
//! the session to the shared SQLite database.

use wa_client::client::{WhatsAppClient, WhatsAppEvent};
use wa_client::qr::QrRef;
use wa_domain::ports::WhatsAppClientPort;
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

    // Ensure parent directory exists
    if let Some(parent) = std::path::Path::new(&db_path).parent() {
        std::fs::create_dir_all(parent)?;
    }

    eprintln!("🔐 WhatsApp Pairing Tool");
    eprintln!("   DB path: {}", db_path);

    // Delete old store for fresh pairing
    if std::path::Path::new(&db_path).exists() {
        eprintln!("   Removing old session for fresh pairing...");
        std::fs::remove_file(&db_path)?;
    }

    // Phase 1: QR pairing
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
                        let qr_path = format!("{}/qr_code.png", 
                            std::path::Path::new(&db_path).parent()
                                .map(|p| p.to_string_lossy().to_string())
                                .unwrap_or_else(|| ".".to_string()));
                        match save_qr_png(&data, &qr_path) {
                            Ok(()) => {
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
            eprintln!();
            eprintln!("🎉 WhatsApp connection fully established!");
            eprintln!("   JID: {}", paired_jid);
            eprintln!("   DB:  {}", db_path);
            eprintln!();
            eprintln!("   You can now use the MCP server!");

            // Keep connection alive briefly to let server sync
            tokio::time::sleep(Duration::from_secs(5)).await;
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
