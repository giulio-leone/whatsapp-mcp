# WA-Client (WhatsApp Multi-Device Protocol in Rust)

This module implements the core WhatsApp Web Multi-Device protocol, porting the logic from the Go library `whatsmeow`. 

## Architecture (Agentic First)

The WA-Client is designed with LLM consumption in mind:
- **Clean Tool Boundaries**: State-mutating functions (like sending a message) strictly validate inputs before executing, returning highly structured errors that an LLM can parse and use for retry logic.
- **Context-aware Pagination**: Historical chat queries return data with deterministic cursors rather than arbitrary counts to prevent LLM hallucination and context-window bloating.

## Porting Strategy

1. **Protobufs**: We extract the `.proto` definitions from the original project and compile them using [`prost`](https://github.com/tokio-rs/prost) and [`prost-build`](https://docs.rs/prost-build/latest/prost_build/). These define the schema for WebSockets frames (e.g. `Message`, `Contact`, `HistorySync`).
2. **Noise Protocol Handshake**: WhatsApp web uses the Noise Pipe framework, specifically `Noise_XX_25519_AESGCM_SHA256`. 
3. **Crypto Layer**: End-to-end encryption for the messages relies on the Signal protocol.
4. **WebSocket & Concurrency**: We use `tokio-tungstenite` for persistent websocket connections.

> *Note: For reference, the original Go implementation is preserved in the sibling directory `../../whatsapp-mcp-upstream`.*
