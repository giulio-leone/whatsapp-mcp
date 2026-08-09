---
name: whatsapp
description: Use the bundled WhatsApp MCP server to pair WhatsApp inside Codex and perform explicitly approved message CRUD.
---

# WhatsApp

Use the bundled `whatsapp` MCP server for WhatsApp requests.

## Workflow

1. Call `get_connection_status` before operations that need a live session. If first-time setup is needed, call `open_pairing` so Codex renders the private QR app.
2. Resolve recipients through `list_chats` or `search_contacts`; never guess a `chat_id`.
3. Use `get_messages` or `get_chat_info` for read-only requests.
4. Write tools support one-to-one chats only and must fail closed for groups, broadcasts, and unresolved LID-only identifiers.
5. Before `send_message`, read back the exact resolved recipient and final message text. Treat a current-turn instruction containing both as approval; otherwise request explicit approval.
6. Before `edit_message` or `delete_message`, read back the exact chat, message ID, current text, and requested mutation. Only messages sent by this account are eligible.
7. Report the returned message ID after a successful write. Never retry an uncertain send, edit, or delete because duplicate external mutations cannot be proven safe.

## Pairing boundary

Use `open_pairing` for safe setup. The app polls `get_pairing_status` and may call `restart_pairing`. If WhatsApp rejects a saved registration with 401, the runtime archives only that encrypted device state, activates fresh keys atomically, and generates a new QR while retaining chats and message history. Never extract, repeat, log, summarize, or expose the QR payload from tool `_meta`; it is widget-only data.

Never run the recovery-only `wa-pair --replace-existing-session` command unless the user explicitly requests destructive re-pairing, confirms the exact `WA_DB_PATH`, and understands that the whole database will be deleted. Never expose session databases, device keys, QR payloads, or message contents beyond the user's request.
