use wa_domain::models::chat::ChatId;
use wa_domain::ports::WhatsAppClientPort;
use wa_mcp_server::cli_common;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 || args.contains(&"--help".to_string()) {
        eprintln!("Usage:");
        eprintln!("  wa-status publish <text>                              Publish a text status");
        eprintln!("  wa-status image <path> [caption]                      Publish an image status");
        eprintln!("  wa-status react <status_jid> <status_id> <emoji>      React to a status");
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
    cli_common::apply_stealth_flag(&client);

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
            client.send_message(&ChatId("status@broadcast".into()), &text).await?;
            eprintln!("✅ Status published!");
        }
        "image" => {
            if filtered_args.len() < 2 {
                anyhow::bail!("Usage: wa-status image <path> [caption]");
            }
            let path = filtered_args[1];
            let caption = if filtered_args.len() > 2 {
                Some(filtered_args[2..].join(" "))
            } else {
                None
            };

            let image_bytes = std::fs::read(path)
                .map_err(|e| anyhow::anyhow!("Failed to read image file '{}': {}", path, e))?;

            let mime = match path.rsplit('.').next().unwrap_or("").to_lowercase().as_str() {
                "jpg" | "jpeg" => "image/jpeg",
                "png" => "image/png",
                "webp" => "image/webp",
                "gif" => "image/gif",
                _ => "image/jpeg",
            };

            eprintln!("🖼️  Publishing image status: {} ({} bytes, {})", path, image_bytes.len(), mime);
            client.send_image(
                &ChatId("status@broadcast".into()),
                &image_bytes,
                mime,
                caption.as_deref(),
            ).await?;
            eprintln!("✅ Image status published!");
        }
        "react" => {
            if filtered_args.len() < 4 {
                anyhow::bail!("Usage: wa-status react <status_jid> <status_id> <emoji>");
            }
            let status_jid = filtered_args[1];
            let status_id = filtered_args[2];
            let emoji = filtered_args[3];
            eprintln!("💬 Reacting to status {} from {} with {}", status_id, status_jid, emoji);
            client.send_reaction(&ChatId(format!("{}@s.whatsapp.net", status_jid)), status_id, emoji).await?;
            eprintln!("✅ Reaction sent!");
        }
        _ => {
            anyhow::bail!("Unknown subcommand: {}. Use 'publish', 'image', or 'react'.", subcommand);
        }
    }

    client.disconnect().await?;
    Ok(())
}
