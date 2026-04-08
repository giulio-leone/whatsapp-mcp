use std::path::PathBuf;
use wa_domain::ports::WhatsAppClientPort;
use wa_mcp_server::cli_common;
use wa_mcp_server::poll_config::PollConfig;
use wa_mcp_server::poll_engine::{EventContext, trigger_matches, execute_actions};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 || args.contains(&"--help".to_string()) {
        eprintln!("Usage: wa-poll <config.yaml> [--fresh]");
        eprintln!();
        eprintln!("Virtual webhook daemon — polls WhatsApp events and triggers actions.");
        eprintln!();
        eprintln!("Options:");
        eprintln!("  --fresh   Clear all signal sessions before connecting");
        eprintln!();
        eprintln!("Example config (YAML):");
        eprintln!("  daemon:");
        eprintln!("    db_path: ~/.whatsapp-mcp/whatsapp.db");
        eprintln!("    log_level: info");
        eprintln!("  triggers:");
        eprintln!("    - name: on_message");
        eprintln!("      events:");
        eprintln!("        - type: MessageReceived");
        eprintln!("      actions:");
        eprintln!("        - type: http_post");
        eprintln!("          url: http://localhost:3000/webhook");
        std::process::exit(1);
    }

    let config_path = PathBuf::from(&args[1]);
    let fresh = args.contains(&"--fresh".to_string());

    // Load config
    let config = PollConfig::load(&config_path)?;

    let active_triggers: Vec<_> = config.triggers.iter().filter(|t| t.is_enabled()).collect();
    eprintln!("🔄 WhatsApp Poll Daemon");
    eprintln!("   Config: {}", config_path.display());
    eprintln!("   Triggers: {} active ({} total)", active_triggers.len(), config.triggers.len());
    for t in &active_triggers {
        eprintln!("   • {} ({} events → {} actions)", t.name, t.events.len(), t.actions.len());
    }

    // Init tracing
    cli_common::init_tracing();

    // Create and connect client
    let db_path = cli_common::resolve_db_path();
    let client = cli_common::create_client(&db_path)?;
    cli_common::apply_stealth_flag(&client);

    if fresh {
        eprintln!("   Clearing signal sessions...");
        cli_common::clear_sessions_for(&db_path, "poll")?;
    }

    eprintln!("   Connecting...");
    cli_common::connect_and_wait(&client, 15).await?;

    eprintln!("✅ Connected. Polling for events... (Ctrl+C to stop)\n");

    // Main event loop with graceful shutdown
    let client_ref = client.clone();
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {
            eprintln!("\n⏹  Shutting down...");
        }
        _ = async {
            let mut event_count: u64 = 0;
            let mut trigger_count: u64 = 0;

            loop {
                match client_ref.next_event().await {
                    Some(event) => {
                        event_count += 1;
                        let ctx = EventContext::from_event(&event);

                        for trigger in &config.triggers {
                            if trigger_matches(&event, trigger) {
                                trigger_count += 1;
                                tracing::info!("Trigger '{}' matched: {}", trigger.name, ctx.event_type);
                                let results = execute_actions(&trigger.actions, &ctx).await;
                                for result in &results {
                                    if result.success {
                                        tracing::info!("  ✅ {}: {}", result.action_type, result.detail);
                                    } else {
                                        tracing::warn!("  ❌ {}: {}", result.action_type, result.detail);
                                    }
                                }
                            }
                        }

                        // Periodic stats (every 100 events)
                        if event_count % 100 == 0 {
                            tracing::info!("Stats: {} events processed, {} triggers fired", event_count, trigger_count);
                        }
                    }
                    None => {
                        tracing::error!("Event stream ended");
                        break;
                    }
                }
            }
        } => {}
    }

    client.disconnect().await?;
    eprintln!("👋 Disconnected.");
    Ok(())
}
