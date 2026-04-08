use wa_domain::models::chat::ChatId;
use wa_domain::ports::WhatsAppClientPort;
use wa_mcp_server::cli_common;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 || args.contains(&"--help".to_string()) {
        eprintln!("Usage:");
        eprintln!("  wa-status publish <text>        Publish a text status");
        eprintln!("  wa-status react <status_jid> <status_id> <emoji>   React to a status");
        eprintln!();
        eprintln!("Options:");
        eprintln!("  --fresh   Clear signal sessions before connecting");
        std::process::exit(1);
    }

    let fresh = args.contains(&"--fresh".to_string());
    let filtered_args: Vec<&str> = args.iter()
        .map(|s| s.as_str())
        .filter(|s| *s != "--fresh")
        .skip(1)
        .collect();

    let subcommand = filtered_args.first().copied().unwrap_or("");

    cli_common::init_tracing();
    let db_path = cli_common::resolve_db_path();
    let client = cli_common::create_client(&db_path)?;

    if fresh {
        cli_common::clear_sessions_for(&db_path, "status")?;
    }

    eprintln!("   Connecting...");
    cli_common::connect_and_wait(&client, 15).await?;

    match subcommand {
        "publish" => {
            let text = filtered_args.get(1..).map(|s| s.join(" ")).unwrap_or_default();
            if text.is_empty() {
                anyhow::bail!("Usage: wa-status publish <text>");
            }
            eprintln!("📝 Publishing status: {}", text);

            // Status messages are sent to status@broadcast JID
            client.send_message(&ChatId("status@broadcast".into()), &text).await?;
            eprintln!("✅ Status published!");
        }
        "react" => {
            if filtered_args.len() < 4 {
                anyhow::bail!("Usage: wa-status react <status_jid> <status_id> <emoji>");
            }
            let status_jid = filtered_args[1];
            let status_id = filtered_args[2];
            let emoji = filtered_args[3];
            eprintln!("💬 Reacting to status {} from {} with {}", status_id, status_jid, emoji);

            // Reactions are sent as special messages with ReactionMessage proto
            // For now, use the send_message with a reaction wrapper
            // TODO: implement proper ReactionMessage encoding when proto is compiled to Rust
            client.send_reaction(&ChatId(format!("{}@s.whatsapp.net", status_jid)), status_id, emoji).await?;
            eprintln!("✅ Reaction sent!");
        }
        _ => {
            anyhow::bail!("Unknown subcommand: {}. Use 'publish' or 'react'.", subcommand);
        }
    }

    client.disconnect().await?;
    Ok(())
}
