# 📱 WhatsApp MCP Server

A **Model Context Protocol** server that enables LLM agents to interact with WhatsApp.
Built in Rust with zero-copy binary codec, Signal protocol encryption, and an agentic-first tool design.

## Architecture

```
┌──────────────────────┐
│   Claude Desktop /   │
│   Any MCP Client     │
│                      │
└──────────┬───────────┘
           │ JSON-RPC 2.0 (stdio)
           │
┌──────────▼───────────┐
│   wa-mcp-server      │  ← MCP protocol handler
│   (Rust, crates/     │     11 intent-based tools + pairing app
│    mcp-server)       │
└────┬────────────┬────┘
     │            │
┌────▼────┐  ┌───▼──────────┐
│ wa-     │  │ storage-     │
│ client  │  │ sqlite       │
│ (Rust)  │  │ (Rust)       │
│         │  │              │
│ Noise   │  │ Messages,    │
│ Signal  │  │ Chats,       │
│ Binary  │  │ Contacts     │
│ WS      │  │              │
└────┬────┘  └──────────────┘
     │
     │ WSS (Noise_XX_25519_AESGCM_SHA256)
     │
┌────▼────────────────────┐
│  WhatsApp Web Servers   │
│  web.whatsapp.com       │
└─────────────────────────┘
```

### Crates

| Crate | Description |
|-------|-------------|
| `wa-domain` | Shared models (`Chat`, `Message`, `Contact`, `Jid`) and port traits |
| `wa-client` | WhatsApp Web Multi-Device protocol: Noise handshake, Signal encryption, binary codec |
| `storage-sqlite` | SQLite persistence for messages, chats, contacts |
| `mcp-server` | JSON-RPC 2.0 MCP server with 11 tools and a private pairing app |

## MCP Tools

| Tool | Risk | Description |
|------|------|-------------|
| `list_chats` | 🟢 read-only | List all chats with metadata |
| `get_messages` | 🟢 read-only | Retrieve messages with cursor pagination |
| `search_contacts` | 🟢 read-only | Search contacts by name/number |
| `get_chat_info` | 🟢 read-only | Get detailed info for a single chat |
| `send_message` | 🟡 write | Send a text message (requires confirmation) |
| `edit_message` | 🟡 write | Edit a text message sent by this account (requires confirmation) |
| `delete_message` | 🔴 destructive | Delete a message sent by this account for all participants (requires confirmation) |
| `get_connection_status` | 🟢 read-only | Check WhatsApp session health |
| `open_pairing` | 🟢 read-only | Open the private pairing app in Codex |
| `get_pairing_status` | 🟢 app-only | Poll pairing state without exposing the QR to the model |
| `restart_pairing` | 🟡 app-only | Retry pairing; archive a server-rejected registration and generate a fresh QR without deleting chat history |

## Quick Start

### Prerequisites
- **Rust** ≥ 1.75 with `cargo`

### Build
```bash
cargo build --release
```

### Run
```bash
# Each MCP client communicates over stdio; one local runtime owns WhatsApp
./target/release/wa-mcp-server
```

### MCP protocol eras

The stdio server supports both MCP wire eras:

- Modern `2026-07-28`: probe with `server/discover`, then include
  `params._meta.io.modelcontextprotocol/protocolVersion` and
  `params._meta.io.modelcontextprotocol/clientCapabilities` on every request.
  `clientInfo` is accepted when supplied. Modern results carry server identity
  in `_meta.io.modelcontextprotocol/serverInfo`.
- Legacy `2025-06-18`: use the `initialize` / `initialized` handshake. Existing
  `tools/list` and `tools/call` clients remain supported without modern metadata.

The modern and legacy paths share the same tool registry; the server does not
enable HTTP transport.

Every stdio process connects to one Unix-domain runtime socket derived from
`WA_DB_PATH`. The first process owns the WhatsApp connection and SQLite-backed
session; later Codex tasks proxy requests to that owner. This prevents session
lock contention and keeps connection state consistent across task processes.

### Install as a Codex plugin

Install from this checkout:

```bash
codex plugin marketplace add .
codex plugin add whatsapp-mcp@whatsapp-mcp-local
```

Or install the marketplace from GitHub:

```bash
codex plugin marketplace add giulio-leone/whatsapp-mcp --ref main
codex plugin add whatsapp-mcp@whatsapp-mcp-local
```

The plugin launcher builds the pinned Rust binary into
`${XDG_CACHE_HOME:-$HOME/.cache}/whatsapp-mcp` on first use and reuses it on
later starts. `cargo` must remain available in `PATH`.

### Configure with Claude Desktop

Add to your Claude Desktop configuration (`~/Library/Application Support/Claude/claude_desktop_config.json`):

```json
{
  "mcpServers": {
    "whatsapp": {
      "command": "/path/to/whatsapp-mcp/target/release/wa-mcp-server",
      "env": {
        "WA_DB_PATH": "/path/to/whatsapp.db"
      }
    }
  }
}
```

### First Connection in Codex

1. Start a new Codex task after installing the plugin.
2. Ask: `Open the WhatsApp pairing setup.`
3. In the rendered app, scan the QR from WhatsApp → Settings → Linked Devices →
   Link a Device.
4. Keep the app open until it reports `WhatsApp collegato`.

The MCP server exposes the component as an MCP Apps resource
(`text/html;profile=mcp-app`). The raw QR payload is returned only in MCP tool
result `_meta`, rendered inside the private app, and never placed in
model-visible `structuredContent` or `content`. No QR file is written to disk.

The integrated flow supports safe first-time setup and recovery. When no saved
session exists, `restart_pairing` retries first-time pairing. When WhatsApp
rejects a saved registration with 401, the runtime archives only its encrypted
`device_store` state under a timestamped key, activates fresh device keys
atomically, and generates a new QR. Chats, contacts, and message history remain
in the same database. The entire database is never replaced through MCP.

The legacy terminal utility remains available for recovery. It requires the
explicit `--replace-existing-session` flag before deleting an existing database:

```bash
cargo run --locked --release --bin wa-pair -- --replace-existing-session
```

Back up `WA_DB_PATH` first. This recovery command is outside the Codex pairing
app and replaces the entire local WhatsApp MCP database.

## Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `WA_DB_PATH` | `~/.whatsapp-mcp/whatsapp.db` | Path to SQLite database and encrypted session state |
| `RUST_LOG` | (none) | Log level (`info`, `debug`, `trace`) |
| `WHATSAPP_MCP_BUILD_CACHE` | `${XDG_CACHE_HOME:-$HOME/.cache}/whatsapp-mcp` | Plugin build cache override |

## Project Structure

```
whatsapp-mcp/
├── apps/                    # MCP Apps pairing component
├── Cargo.toml              # Workspace root
├── crates/
│   ├── domain/             # Shared models & port traits
│   ├── wa-client/          # WhatsApp protocol implementation
│   │   ├── src/
│   │   │   ├── binary/     # WAP binary codec (encoder/decoder/tokens)
│   │   │   ├── crypto/     # Signal: X3DH, Double Ratchet, CBC, HMAC
│   │   │   ├── client.rs   # Connection, send/receive, session mgmt
│   │   │   ├── socket.rs   # WebSocket + Noise transport
│   │   │   ├── qr.rs       # QR code generation for pairing
│   │   │   └── store.rs    # Device key store
│   │   └── proto/          # Protobuf definitions
│   ├── storage-sqlite/     # SQLite storage adapter
│   └── mcp-server/         # MCP JSON-RPC server
├── bridge/                 # Optional: Go bridge using whatsmeow
└── bindings/               # Python & TypeScript bindings (WIP)
```

## Development

```bash
# Check everything compiles
cargo check --workspace

# Run workspace tests
cargo test --workspace

# Run with debug logging
RUST_LOG=debug cargo run --bin wa-mcp-server

# Test MCP protocol
echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' | cargo run --bin wa-mcp-server

# Modern discovery probe (per-request metadata is required after discovery)
echo '{"jsonrpc":"2.0","id":1,"method":"server/discover","params":{"_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}}}' | cargo run --bin wa-mcp-server
```

## License

MIT
