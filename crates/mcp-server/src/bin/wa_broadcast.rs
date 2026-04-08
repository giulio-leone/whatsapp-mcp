//! Broadcast messages to multiple WhatsApp contacts.
//!
//! Usage:
//!   wa-broadcast [--fresh] [--delay <ms>] <message> <phone1> [phone2] ...
//!   wa-broadcast [--fresh] [--delay <ms>] --file <contacts.txt> <message>

use wa_domain::ports::WhatsAppClientPort;
use wa_domain::models::chat::ChatId;
use std::time::Duration;
use wa_mcp_server::cli_common;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    cli_common::init_tracing();

    let args: Vec<String> = std::env::args().skip(1).collect();

    // Parse flags
    let fresh = args.iter().any(|a| a == "--fresh");
    let delay_ms = parse_flag_value(&args, "--delay")
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(1000);
    let file_path = parse_flag_value(&args, "--file");

    // Extract positional args (skip flags and their values)
    let positional = extract_positional(&args);

    // Determine message and contacts
    let (message, phones) = if let Some(ref fp) = file_path {
        if positional.is_empty() {
            eprintln!("Usage: wa-broadcast [--fresh] [--delay <ms>] --file <contacts.txt> <message>");
            std::process::exit(1);
        }
        let msg = positional[0].clone();
        let contacts = cli_common::read_contacts_file(fp)?;
        (msg, contacts)
    } else {
        if positional.len() < 2 {
            eprintln!("Usage: wa-broadcast [--fresh] [--delay <ms>] <message> <phone1> [phone2] ...");
            eprintln!("       wa-broadcast --file <contacts.txt> <message>");
            eprintln!();
            eprintln!("Options:");
            eprintln!("  --fresh         Clear cached sessions before sending");
            eprintln!("  --delay <ms>    Delay between sends (default: 1000ms)");
            eprintln!("  --file <path>   Read phone numbers from file (one per line)");
            std::process::exit(1);
        }
        let msg = positional[0].clone();
        let phones: Vec<String> = positional[1..].to_vec();
        (msg, phones)
    };

    let db_path = cli_common::resolve_db_path();

    eprintln!("📡 WhatsApp Broadcast");
    eprintln!("   Message: {}", message);
    eprintln!("   Contacts: {} recipients", phones.len());
    eprintln!("   Delay: {}ms between sends", delay_ms);
    eprintln!("   DB: {}", db_path);

    // Clear sessions if --fresh
    if fresh {
        for phone in &phones {
            cli_common::clear_sessions_for(&db_path, phone)?;
        }
    }

    let client = cli_common::create_client(&db_path)?;
    eprintln!("   Connecting...");
    let jid = cli_common::connect_and_wait(&client, 15).await?;
    eprintln!("✅ Connected as: {}", jid);

    // Send to each contact
    let mut success_count = 0u32;
    let mut fail_count = 0u32;
    let total = phones.len();

    for (i, phone) in phones.iter().enumerate() {
        let recipient_jid = if phone.contains('@') {
            phone.clone()
        } else {
            format!("{}@s.whatsapp.net", phone)
        };

        eprint!("   [{}/{}] {} ... ", i + 1, total, phone);

        match client.send_message(&ChatId(recipient_jid), &message).await {
            Ok(msg) => {
                eprintln!("✅ sent (id: {})", msg.id.0);
                success_count += 1;
            }
            Err(e) => {
                eprintln!("❌ failed: {}", e);
                fail_count += 1;
            }
        }

        // Delay between sends (skip after last)
        if i + 1 < total {
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        }
    }

    eprintln!();
    eprintln!("📊 Broadcast complete: {} sent, {} failed, {} total", success_count, fail_count, total);

    // Brief wait for server ACKs
    tokio::time::sleep(Duration::from_secs(2)).await;
    let _ = client.disconnect().await;

    if fail_count > 0 {
        std::process::exit(1);
    }
    Ok(())
}

fn parse_flag_value(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1).cloned())
}

fn extract_positional(args: &[String]) -> Vec<String> {
    let flags_with_values = ["--delay", "--file"];
    let mut result = Vec::new();
    let mut skip_next = false;
    for arg in args {
        if skip_next {
            skip_next = false;
            continue;
        }
        if flags_with_values.contains(&arg.as_str()) {
            skip_next = true;
            continue;
        }
        if arg.starts_with("--") {
            continue;
        }
        result.push(arg.clone());
    }
    result
}
