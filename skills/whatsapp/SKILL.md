---
name: whatsapp
description: Use the bundled WhatsApp MCP server to pair WhatsApp inside Codex, inspect chats, read messages, find contacts, check connection health, and send explicitly approved text messages.
---

# WhatsApp

Use the bundled `whatsapp` MCP server for WhatsApp requests.

## Workflow

1. Call `get_connection_status` before operations that need a live session. If first-time setup is needed, call `open_pairing` so Codex renders the private QR app.
2. Resolve recipients through `list_chats` or `search_contacts`; never guess a `chat_id`.
3. Use `get_messages` or `get_chat_info` for read-only requests.
4. Before `send_message`, read back the exact resolved recipient and final message text. Treat a current-turn instruction containing both as approval; otherwise request explicit approval.
5. Report returned message ID and timestamp after a successful send. Never retry an uncertain send without checking whether delivery already occurred.

## Pairing boundary

Use `open_pairing` for safe first-time setup. The app polls `get_pairing_status` and may call `restart_pairing` when no saved session exists or when a saved session is disconnected; it retries first-time pairing or non-destructive reconnection while preserving the database and session. Never extract, repeat, log, summarize, or expose the QR payload from tool `_meta`; it is widget-only data. Destructive session replacement remains unavailable through MCP.

Never run the recovery-only `wa-pair --replace-existing-session` command unless the user explicitly requests destructive re-pairing, confirms the exact `WA_DB_PATH`, and understands that the whole database will be deleted. Never expose session databases, device keys, QR payloads, or message contents beyond the user's request.
