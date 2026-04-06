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
│   (Rust, crates/     │     6 intent-based tools
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
| `mcp-server` | JSON-RPC 2.0 MCP server with 6 LLM-optimized tools |

## MCP Tools

| Tool | Risk | Description |
|------|------|-------------|
| `list_chats` | 🟢 read-only | List all chats with metadata |
| `get_messages` | 🟢 read-only | Retrieve messages with cursor pagination |
| `search_contacts` | 🟢 read-only | Search contacts by name/number |
| `get_chat_info` | 🟢 read-only | Get detailed info for a single chat |
| `send_message` | 🟡 write | Send a text message (requires confirmation) |
| `get_connection_status` | 🟢 read-only | Check WhatsApp session health |

## Quick Start

### Prerequisites
- **Rust** ≥ 1.75 with `cargo`

### Build
```bash
cargo build --release
```

### Run
```bash
# The server communicates over stdio (JSON-RPC)
./target/release/wa-mcp-server
```

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

### First Connection

On first launch, the server will display a QR code in stderr.
Scan it with WhatsApp → Settings → Linked Devices → Link a Device.

## Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `WA_DB_PATH` | `whatsapp.db` | Path to SQLite database |
| `RUST_LOG` | (none) | Log level (`info`, `debug`, `trace`) |

## Project Structure

```
whatsapp-mcp/
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

# Run with debug logging
RUST_LOG=debug cargo run --bin wa-mcp-server

# Test MCP protocol
echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}' | cargo run --bin wa-mcp-server
```

## License

MIT
