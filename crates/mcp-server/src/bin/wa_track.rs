//! Track when WhatsApp contacts go online/offline.
//!
//! Usage:
//!   wa-track <phone1> [phone2] ...
//!   wa-track --file <contacts.txt>
//!   wa-track --json <phone1> ...    # JSON Lines output for piping
//!
//! Runs until Ctrl+C. Streams presence events to stdout.

use wa_client::client::WhatsAppEvent;
use wa_domain::ports::WhatsAppClientPort;
use wa_mcp_server::cli_common;
use chrono::{Local, TimeZone};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    cli_common::init_tracing();

    let args: Vec<String> = std::env::args().skip(1).collect();

    let json_output = args.iter().any(|a| a == "--json");
    let file_path = parse_flag_value(&args, "--file");

    let positional: Vec<String> = args
        .iter()
        .filter(|a| !a.starts_with("--"))
        .cloned()
        .collect();

    // Skip the value after --file
    let phones = if let Some(ref fp) = file_path {
        cli_common::read_contacts_file(fp)?
    } else {
        if positional.is_empty() {
            eprintln!("Usage: wa-track [--json] <phone1> [phone2] ...");
            eprintln!("       wa-track [--json] --file <contacts.txt>");
            eprintln!();
            eprintln!("Options:");
            eprintln!("  --json          Output as JSON Lines (for piping/scripting)");
            eprintln!("  --file <path>   Read phone numbers from file (one per line)");
            eprintln!();
            eprintln!("Tracks contact presence (online/offline) in real-time.");
            eprintln!("Press Ctrl+C to stop.");
            std::process::exit(1);
        }
        positional
    };

    let db_path = cli_common::resolve_db_path();

    eprintln!("👁️  WhatsApp Presence Tracker");
    eprintln!("   Tracking: {} contacts", phones.len());
    for p in &phones {
        eprintln!("   • {}", p);
    }
    eprintln!("   Output: {}", if json_output { "JSON Lines" } else { "human-readable" });

    let client = cli_common::create_client(&db_path)?;
    eprintln!("   Connecting...");
    let jid = cli_common::connect_and_wait(&client, 15).await?;
    eprintln!("✅ Connected as: {}", jid);

    // Send our presence as available (required to receive presence updates)
    client.send_available_presence().await?;

    // Subscribe to all contacts
    client.subscribe_presence(&phones).await?;
    eprintln!("✅ Subscribed to {} contacts. Waiting for events... (Ctrl+C to stop)", phones.len());
    eprintln!();

    if !json_output {
        println!("{:<20} {:<12} {:<20}", "CONTACT", "STATUS", "LAST SEEN");
        println!("{}", "-".repeat(52));
    }

    // Event loop — stream presence updates until Ctrl+C
    let client_ref = &client;
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            eprintln!("\n⏹️  Stopped by user");
        }
        _ = async {
            loop {
                match client_ref.next_event().await {
                    Some(WhatsAppEvent::PresenceUpdate { jid, available, last_seen }) => {
                        let contact = jid.split('@').next().unwrap_or(&jid);
                        let status = if available { "🟢 ONLINE" } else { "⚪ OFFLINE" };
                        let last_seen_str = last_seen
                            .map(|ts| Local.timestamp_opt(ts, 0)
                                .single()
                                .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
                                .unwrap_or_else(|| ts.to_string()))
                            .unwrap_or_default();

                        if json_output {
                            let now = Local::now().to_rfc3339();
                            println!(r#"{{"timestamp":"{}","contact":"{}","available":{},"last_seen":{}}}"#,
                                now,
                                contact,
                                available,
                                last_seen.map(|t| t.to_string()).unwrap_or("null".to_string())
                            );
                        } else {
                            let now = Local::now().format("%H:%M:%S");
                            println!("[{}] {:<14} {:<12} {}", now, contact, status, last_seen_str);
                        }
                    }
                    Some(WhatsAppEvent::Disconnected) => {
                        eprintln!("❌ Disconnected from WhatsApp");
                        break;
                    }
                    Some(_) => {} // Ignore other events
                    None => {
                        eprintln!("❌ Event channel closed");
                        break;
                    }
                }
            }
        } => {}
    }

    let _ = client.disconnect().await;
    Ok(())
}

fn parse_flag_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1).cloned())
}
