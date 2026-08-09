//! MCP Server — stdio transport with JSON-RPC 2.0.
//!
//! Handles the MCP lifecycle: initialize → initialized → tools/list → tools/call.

use crate::protocol::{
    JsonRpcRequest, JsonRpcResponse, ProtocolEra, ResourcesCapability, ServerCapabilities,
    ServerInfo, ToolResult, ToolResultContent, ToolsCapability, LEGACY_PROTOCOL_VERSION,
    MODERN_META_CLIENT_CAPABILITIES, MODERN_META_CLIENT_INFO, MODERN_META_PROTOCOL_VERSION,
    MODERN_META_SERVER_INFO, MODERN_PROTOCOL_VERSION, SUPPORTED_PROTOCOL_VERSIONS,
};
use crate::tools::{tool_registry, PAIRING_RESOURCE_URI};
use anyhow::Result;
use qrcode::{Color, EcLevel, QrCode};
use serde_json::json;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use wa_domain::ports::{PairingPhase, PairingSnapshot, StoragePort, WhatsAppClientPort};

const PAIRING_WIDGET_HTML: &str = include_str!("../../../apps/whatsapp-pairing.html");
const QR_TTL_MS: u64 = 120_000;
const RUNTIME_HEARTBEAT_TTL_MS: u64 = 10_000;

pub struct McpServer {
    storage: Arc<dyn StoragePort>,
    wa_client: Arc<dyn WhatsAppClientPort>,
    protocol_era: Mutex<Option<ProtocolEra>>,
}

impl McpServer {
    pub fn new(storage: Arc<dyn StoragePort>, wa_client: Arc<dyn WhatsAppClientPort>) -> Self {
        Self {
            storage,
            wa_client,
            protocol_era: Mutex::new(None),
        }
    }

    /// Run the MCP server on stdio (blocking).
    pub async fn run_stdio(&self) -> Result<()> {
        let stdin = tokio::io::stdin();
        let stdout = tokio::io::stdout();
        self.run_transport(stdin, stdout).await
    }

    pub async fn run_transport<R, W>(&self, input: R, mut output: W) -> Result<()>
    where
        R: AsyncRead + Unpin,
        W: AsyncWrite + Unpin,
    {
        let mut reader = BufReader::new(input);
        let mut line = String::new();

        loop {
            line.clear();
            let n = reader.read_line(&mut line).await?;
            if n == 0 {
                break; // EOF
            }

            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            let request: JsonRpcRequest = match serde_json::from_str(trimmed) {
                Ok(r) => r,
                Err(e) => {
                    let err_response = JsonRpcResponse::error(
                        None,
                        -32700,
                        format!("Parse error: {e}"),
                    );
                    let out = serde_json::to_string(&err_response)? + "\n";
                    output.write_all(out.as_bytes()).await?;
                    output.flush().await?;
                    continue;
                }
            };

            // JSON-RPC notifications have no id — never send a response for them
            if request.id.is_none() || request.method.starts_with("notifications/") {
                tracing::debug!("Received notification: {}", request.method);
                continue;
            }

            let response = self.handle_request(&request).await;
            let out = serde_json::to_string(&response)? + "\n";
            output.write_all(out.as_bytes()).await?;
            output.flush().await?;
        }

        Ok(())
    }

    async fn handle_request(&self, req: &JsonRpcRequest) -> JsonRpcResponse {
        let era = match self.classify_request(req) {
            Ok(era) => era,
            Err(response) => return response,
        };

        let response = match req.method.as_str() {
            "initialize" => self.handle_initialize(req),
            "server/discover" => self.handle_discover(req),
            "tools/list" => self.handle_tools_list(req),
            "tools/call" => self.handle_tools_call(req).await,
            "resources/list" => self.handle_resources_list(req),
            "resources/read" => self.handle_resources_read(req),
            _ => JsonRpcResponse::error(
                req.id.clone(),
                -32601,
                format!("Method not found: {}", req.method),
            ),
        };

        match era {
            ProtocolEra::Legacy => response,
            ProtocolEra::Modern => stamp_modern_response(response, &req.method),
        }
    }

    fn classify_request(
        &self,
        req: &JsonRpcRequest,
    ) -> std::result::Result<ProtocolEra, JsonRpcResponse> {
        if req.method == "initialize" {
            let legacy_version_is_modern = req
                .params
                .get("protocolVersion")
                .and_then(|value| value.as_str())
                == Some(MODERN_PROTOCOL_VERSION);
            let modern_meta_version_is_present = req
                .params
                .get("_meta")
                .and_then(|meta| meta.get(MODERN_META_PROTOCOL_VERSION))
                .and_then(|value| value.as_str())
                == Some(MODERN_PROTOCOL_VERSION);
            if legacy_version_is_modern || modern_meta_version_is_present {
                return Err(JsonRpcResponse::error(
                    req.id.clone(),
                    -32602,
                    "The 2026-07-28 protocol uses server/discover and per-request _meta; do not call initialize.".into(),
                ));
            }
            *self.protocol_era.lock().expect("protocol era mutex") = Some(ProtocolEra::Legacy);
            return Ok(ProtocolEra::Legacy);
        }

        if req.method == "server/discover" {
            validate_modern_metadata(req)?;
            *self.protocol_era.lock().expect("protocol era mutex") = Some(ProtocolEra::Modern);
            return Ok(ProtocolEra::Modern);
        }

        if req.params.get("_meta").is_some_and(|meta| !meta.is_object()) {
            return Err(invalid_metadata_response(
                req,
                "The request _meta field must be an object.",
            ));
        }

        let has_reserved_modern_metadata = req
            .params
            .get("_meta")
            .and_then(|meta| meta.as_object())
            .is_some_and(|meta| {
                meta.keys()
                    .any(|key| key.starts_with("io.modelcontextprotocol/"))
            });

        if has_reserved_modern_metadata {
            validate_modern_metadata(req)?;
            *self.protocol_era.lock().expect("protocol era mutex") = Some(ProtocolEra::Modern);
            return Ok(ProtocolEra::Modern);
        }

        match *self.protocol_era.lock().expect("protocol era mutex") {
            Some(ProtocolEra::Legacy) => Ok(ProtocolEra::Legacy),
            Some(ProtocolEra::Modern) | None => {
                return Err(invalid_metadata_response(
                    req,
                    "Use server/discover for modern requests or initialize for legacy requests before calling MCP methods.",
                ));
            }
        }
    }

    fn handle_initialize(&self, req: &JsonRpcRequest) -> JsonRpcResponse {
        JsonRpcResponse::success(
            req.id.clone(),
            json!({
                "protocolVersion": LEGACY_PROTOCOL_VERSION,
                "capabilities": ServerCapabilities {
                    tools: Some(ToolsCapability { list_changed: false }),
                    resources: Some(ResourcesCapability {
                        subscribe: false,
                        list_changed: false,
                    }),
                },
                "serverInfo": ServerInfo {
                    name: "whatsapp-mcp".into(),
                    version: env!("CARGO_PKG_VERSION").into(),
                },
            }),
        )
    }

    fn handle_discover(&self, req: &JsonRpcRequest) -> JsonRpcResponse {
        JsonRpcResponse::success(
            req.id.clone(),
            json!({
                "resultType": "complete",
                "supportedVersions": SUPPORTED_PROTOCOL_VERSIONS,
                "capabilities": ServerCapabilities {
                    tools: Some(ToolsCapability { list_changed: false }),
                    resources: Some(ResourcesCapability {
                        subscribe: false,
                        list_changed: false,
                    }),
                },
                "_meta": {
                    MODERN_META_SERVER_INFO: server_info_value(),
                },
                "instructions": "Use WhatsApp tools for connection status, pairing, chat metadata, and explicitly approved messages.",
                "ttlMs": 0,
                "cacheScope": "private",
            }),
        )
    }

    fn handle_tools_list(&self, req: &JsonRpcRequest) -> JsonRpcResponse {
        let tools = tool_registry();
        JsonRpcResponse::success(req.id.clone(), json!({ "tools": tools }))
    }

    fn handle_resources_list(&self, req: &JsonRpcRequest) -> JsonRpcResponse {
        JsonRpcResponse::success(
            req.id.clone(),
            json!({
                "resources": [{
                    "uri": PAIRING_RESOURCE_URI,
                    "name": "whatsapp-pairing",
                    "title": "WhatsApp pairing",
                    "description": "Private QR pairing setup for WhatsApp MCP.",
                    "mimeType": "text/html;profile=mcp-app"
                }]
            }),
        )
    }

    fn handle_resources_read(&self, req: &JsonRpcRequest) -> JsonRpcResponse {
        let uri = req.params.get("uri").and_then(|value| value.as_str());
        if uri != Some(PAIRING_RESOURCE_URI) {
            return JsonRpcResponse::error(
                req.id.clone(),
                -32602,
                format!("Unknown resource URI: {}", uri.unwrap_or("(missing)")),
            );
        }

        JsonRpcResponse::success(
            req.id.clone(),
            json!({
                "contents": [{
                    "uri": PAIRING_RESOURCE_URI,
                    "mimeType": "text/html;profile=mcp-app",
                    "text": PAIRING_WIDGET_HTML,
                    "_meta": {
                        "ui": {
                            "prefersBorder": true
                        }
                    }
                }]
            }),
        )
    }

    async fn handle_tools_call(&self, req: &JsonRpcRequest) -> JsonRpcResponse {
        let tool_name = req.params.get("name").and_then(|v| v.as_str());
        let arguments = req.params.get("arguments").cloned().unwrap_or(json!({}));

        match tool_name {
            Some("list_chats") => self.tool_list_chats(req, &arguments).await,
            Some("get_messages") => self.tool_get_messages(req, &arguments).await,
            Some("search_contacts") => self.tool_search_contacts(req, &arguments).await,
            Some("get_chat_info") => self.tool_get_chat_info(req, &arguments).await,
            Some("send_message") => self.tool_send_message(req, &arguments).await,
            Some("edit_message") => self.tool_edit_message(req, &arguments).await,
            Some("delete_message") => self.tool_delete_message(req, &arguments).await,
            Some("get_connection_status") => self.tool_connection_status(req).await,
            Some("open_pairing") | Some("get_pairing_status") => {
                self.tool_pairing_status(req).await
            }
            Some("restart_pairing") => self.tool_restart_pairing(req).await,
            Some(unknown) => JsonRpcResponse::error(
                req.id.clone(),
                -32602,
                format!(
                    "Unknown tool '{}'. Available tools: list_chats, get_messages, search_contacts, get_chat_info, send_message, edit_message, delete_message, get_connection_status, open_pairing, get_pairing_status, restart_pairing.",
                    unknown
                ),
            ),
            None => JsonRpcResponse::error(
                req.id.clone(),
                -32602,
                "Missing 'name' in tools/call params.".into(),
            ),
        }
    }

    // ─── Tool Implementations ────────────────────────────────────────

    async fn tool_list_chats(
        &self,
        req: &JsonRpcRequest,
        args: &serde_json::Value,
    ) -> JsonRpcResponse {
        let limit = args
            .get("limit")
            .and_then(|value| value.as_u64())
            .unwrap_or(20)
            .min(50) as usize;
        match self.storage.list_chats().await {
            Ok(mut chats) => {
                chats.truncate(limit);
                let result = ToolResult {
                    content: vec![ToolResultContent {
                        content_type: "text".into(),
                        text: serde_json::to_string_pretty(&chats)
                            .unwrap_or_else(|_| "[]".into()),
                    }],
                    structured_content: None,
                    meta: None,
                    is_error: None,
                };
                JsonRpcResponse::success(req.id.clone(), serde_json::to_value(result).unwrap())
            }
            Err(e) => {
                let result = ToolResult {
                    content: vec![ToolResultContent {
                        content_type: "text".into(),
                        text: format!(
                            "Failed to list chats: {}. Suggested next action: call 'get_connection_status' to check if the WhatsApp session is active.",
                            e
                        ),
                    }],
                    structured_content: None,
                    meta: None,
                    is_error: Some(true),
                };
                JsonRpcResponse::success(req.id.clone(), serde_json::to_value(result).unwrap())
            }
        }
    }

    async fn tool_get_messages(
        &self,
        req: &JsonRpcRequest,
        args: &serde_json::Value,
    ) -> JsonRpcResponse {
        let chat_id = match args.get("chat_id").and_then(|v| v.as_str()) {
            Some(id) => wa_domain::models::chat::ChatId(id.to_string()),
            None => {
                return self.tool_error(
                    req,
                    "Missing required parameter 'chat_id'. Use 'list_chats' first to obtain valid chat IDs.",
                );
            }
        };
        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(20)
            .min(100) as u32;
        let cursor = args
            .get("cursor")
            .and_then(|v| v.as_str())
            .map(|s| wa_domain::models::message::MessageId(s.to_string()));

        match self
            .storage
            .get_messages(&chat_id, limit, cursor.as_ref())
            .await
        {
            Ok(messages) => {
                let next_cursor = messages.last().map(|m| &m.id.0);
                let response = json!({
                    "messages": messages,
                    "next_cursor": next_cursor,
                    "has_more": messages.len() == limit as usize,
                });
                let result = ToolResult {
                    content: vec![ToolResultContent {
                        content_type: "text".into(),
                        text: serde_json::to_string_pretty(&response)
                            .unwrap_or_else(|_| "{}".into()),
                    }],
                    structured_content: None,
                    meta: None,
                    is_error: None,
                };
                JsonRpcResponse::success(req.id.clone(), serde_json::to_value(result).unwrap())
            }
            Err(e) => self.tool_error(req, &format!("Failed to get messages: {e}")),
        }
    }

    async fn tool_search_contacts(
        &self,
        req: &JsonRpcRequest,
        args: &serde_json::Value,
    ) -> JsonRpcResponse {
        let query = match args.get("query").and_then(|v| v.as_str()) {
            Some(q) => q,
            None => {
                return self.tool_error(req, "Missing required parameter 'query'.");
            }
        };
        match self.storage.search_contacts(query).await {
            Ok(contacts) => {
                let result = ToolResult {
                    content: vec![ToolResultContent {
                        content_type: "text".into(),
                        text: serde_json::to_string_pretty(&contacts)
                            .unwrap_or_else(|_| "[]".into()),
                    }],
                    structured_content: None,
                    meta: None,
                    is_error: None,
                };
                JsonRpcResponse::success(req.id.clone(), serde_json::to_value(result).unwrap())
            }
            Err(e) => self.tool_error(req, &format!("Failed to search contacts: {e}")),
        }
    }

    async fn tool_get_chat_info(
        &self,
        req: &JsonRpcRequest,
        args: &serde_json::Value,
    ) -> JsonRpcResponse {
        let chat_id = match args.get("chat_id").and_then(|v| v.as_str()) {
            Some(id) => wa_domain::models::chat::ChatId(id.to_string()),
            None => {
                return self.tool_error(
                    req,
                    "Missing required parameter 'chat_id'. Use 'list_chats' or 'search_contacts' to find a chat_id.",
                );
            }
        };
        match self.storage.get_chat(&chat_id).await {
            Ok(Some(chat)) => {
                let result = ToolResult {
                    content: vec![ToolResultContent {
                        content_type: "text".into(),
                        text: serde_json::to_string_pretty(&chat)
                            .unwrap_or_else(|_| "{}".into()),
                    }],
                    structured_content: None,
                    meta: None,
                    is_error: None,
                };
                JsonRpcResponse::success(req.id.clone(), serde_json::to_value(result).unwrap())
            }
            Ok(None) => self.tool_error(
                req,
                &format!(
                    "Chat '{}' not found. Use 'list_chats' to see available chats.",
                    chat_id.0
                ),
            ),
            Err(e) => self.tool_error(req, &format!("Failed to get chat info: {e}")),
        }
    }

    async fn tool_send_message(
        &self,
        req: &JsonRpcRequest,
        args: &serde_json::Value,
    ) -> JsonRpcResponse {
        let chat_id = match args.get("chat_id").and_then(|v| v.as_str()) {
            Some(id) => wa_domain::models::chat::ChatId(id.to_string()),
            None => {
                return self.tool_error(
                    req,
                    "Missing required parameter 'chat_id'.",
                );
            }
        };
        let text = match args.get("text").and_then(|v| v.as_str()) {
            Some(t) if !t.is_empty() => t,
            _ => {
                return self.tool_error(
                    req,
                    "Missing or empty required parameter 'text'. Message text must not be empty.",
                );
            }
        };

        match self.wa_client.send_message(&chat_id, text).await {
            Ok(msg) => {
                let result = ToolResult {
                    content: vec![ToolResultContent {
                        content_type: "text".into(),
                        text: format!(
                            "Message sent successfully. ID: {}, Timestamp: {}",
                            msg.id.0, msg.timestamp
                        ),
                    }],
                    structured_content: None,
                    meta: None,
                    is_error: None,
                };
                // Also persist to local storage
                let _ = self.storage.save_message(&msg).await;
                JsonRpcResponse::success(req.id.clone(), serde_json::to_value(result).unwrap())
            }
            Err(e) => self.tool_error(
                req,
                &format!(
                    "Failed to send message: {}. Suggested: call 'get_connection_status' to verify session health.",
                    e
                ),
            ),
        }
    }

    async fn tool_edit_message(&self, req: &JsonRpcRequest, args: &serde_json::Value) -> JsonRpcResponse {
        let chat_id = match args.get("chat_id").and_then(|value| value.as_str()) {
            Some(value) => wa_domain::models::chat::ChatId(value.into()),
            None => return self.tool_error(req, "Missing required parameter 'chat_id'."),
        };
        let message_id = match args.get("message_id").and_then(|value| value.as_str()) {
            Some(value) => wa_domain::models::message::MessageId(value.into()),
            None => return self.tool_error(req, "Missing required parameter 'message_id'."),
        };
        let text = match args.get("text").and_then(|value| value.as_str()) {
            Some(value) if !value.is_empty() => value,
            _ => return self.tool_error(req, "Missing or empty required parameter 'text'."),
        };
        match self.storage.get_message(&chat_id, &message_id).await {
            Ok(Some(message)) if message.is_from_me => {}
            Ok(Some(_)) => return self.tool_error(req, "Only messages sent by this account can be edited."),
            Ok(None) => return self.tool_error(req, "Message not found in the selected chat. Refresh with get_messages and use the exact IDs."),
            Err(error) => return self.tool_error(req, &format!("Could not validate target message: {error}")),
        }
        match self.wa_client.edit_message(&chat_id, &message_id.0, text).await {
            Ok(message) => {
                let persisted = self
                    .storage
                    .update_message_text(&chat_id, &message_id, text, message.timestamp)
                    .await
                    .unwrap_or(false);
                let result = ToolResult {
                    content: vec![ToolResultContent {
                        content_type: "text".into(),
                        text: format!("Message edited successfully. ID: {}, Timestamp: {}, Local cache updated: {}", message.id.0, message.timestamp, persisted),
                    }],
                    structured_content: Some(json!({
                        "message_id": message.id.0,
                        "timestamp": message.timestamp,
                        "local_cache_updated": persisted,
                    })),
                    meta: None,
                    is_error: None,
                };
                JsonRpcResponse::success(req.id.clone(), serde_json::to_value(result).unwrap())
            }
            Err(error) => self.tool_error(req, &format!("Failed to edit message: {error}. Do not retry until get_messages confirms the current remote state.")),
        }
    }

    async fn tool_delete_message(&self, req: &JsonRpcRequest, args: &serde_json::Value) -> JsonRpcResponse {
        let chat_id = match args.get("chat_id").and_then(|value| value.as_str()) {
            Some(value) => wa_domain::models::chat::ChatId(value.into()),
            None => return self.tool_error(req, "Missing required parameter 'chat_id'."),
        };
        let message_id = match args.get("message_id").and_then(|value| value.as_str()) {
            Some(value) => wa_domain::models::message::MessageId(value.into()),
            None => return self.tool_error(req, "Missing required parameter 'message_id'."),
        };
        match self.storage.get_message(&chat_id, &message_id).await {
            Ok(Some(message)) if message.is_from_me => {}
            Ok(Some(_)) => return self.tool_error(req, "Only messages sent by this account can be deleted."),
            Ok(None) => return self.tool_error(req, "Message not found in the selected chat. Refresh with get_messages and use the exact IDs."),
            Err(error) => return self.tool_error(req, &format!("Could not validate target message: {error}")),
        }
        match self.wa_client.delete_message(&chat_id, &message_id.0).await {
            Ok(()) => {
                let removed = self.storage.delete_message(&chat_id, &message_id).await.unwrap_or(false);
                let result = ToolResult {
                    content: vec![ToolResultContent {
                        content_type: "text".into(),
                        text: format!("Message deleted successfully. ID: {}, Local cache removed: {}", message_id.0, removed),
                    }],
                    structured_content: Some(json!({
                        "message_id": message_id.0,
                        "local_cache_removed": removed,
                    })),
                    meta: None,
                    is_error: None,
                };
                JsonRpcResponse::success(req.id.clone(), serde_json::to_value(result).unwrap())
            }
            Err(error) => self.tool_error(req, &format!("Failed to delete message: {error}. Do not retry until get_messages confirms the current remote state.")),
        }
    }

    async fn tool_connection_status(&self, req: &JsonRpcRequest) -> JsonRpcResponse {
        // Read authoritative client state — never infer connectivity from cached chats.
        let (status, is_error) = match self.shared_connection_is_connected().await {
            Ok(true) => (
                connection_status_payload(true),
                None,
            ),
            Ok(false) => (
                connection_status_payload(false),
                None,
            ),
            Err(error) => (
                json!({
                    "connected": false,
                    "status": "unavailable",
                    "error": error.to_string(),
                    "suggestion": "Could not inspect WhatsApp session health. Verify the configured backend and retry."
                }),
                Some(true),
            ),
        };
        let result = ToolResult {
            content: vec![ToolResultContent {
                content_type: "text".into(),
                text: serde_json::to_string_pretty(&status).unwrap_or_else(|_| "{}".into()),
            }],
            structured_content: Some(status.clone()),
            meta: None,
            is_error,
        };
        JsonRpcResponse::success(req.id.clone(), serde_json::to_value(result).unwrap())
    }

    async fn tool_pairing_status(&self, req: &JsonRpcRequest) -> JsonRpcResponse {
        match self.wa_client.pairing_snapshot().await {
            Ok(mut snapshot) => {
                if self.shared_connection_is_connected().await.unwrap_or(false) {
                    snapshot.phase = PairingPhase::Connected;
                }
                pairing_response(req, &snapshot)
            }
            Err(error) => self.tool_error(
                req,
                &format!("Could not inspect WhatsApp pairing state: {error}"),
            ),
        }
    }

    async fn tool_restart_pairing(&self, req: &JsonRpcRequest) -> JsonRpcResponse {
        if self.shared_connection_is_connected().await.unwrap_or(false) {
            return self.tool_pairing_status(req).await;
        }
        if let Err(error) = self.wa_client.restart_pairing().await {
            let snapshot = self.wa_client.pairing_snapshot().await.ok();
            return pairing_retry_failure_response(req, snapshot.as_ref(), &error.to_string());
        }

        self.tool_pairing_status(req).await
    }

    async fn shared_connection_is_connected(&self) -> Result<bool> {
        if self.wa_client.is_connected().await? {
            return Ok(true);
        }
        let Some((connected, updated_at_ms)) = self.storage.get_runtime_connection().await? else {
            return Ok(false);
        };
        Ok(connected && unix_time_ms().saturating_sub(updated_at_ms) <= RUNTIME_HEARTBEAT_TTL_MS)
    }

    // ─── Helpers ─────────────────────────────────────────────────────

    fn tool_error(&self, req: &JsonRpcRequest, message: &str) -> JsonRpcResponse {
        let result = ToolResult {
            content: vec![ToolResultContent {
                content_type: "text".into(),
                text: message.to_string(),
            }],
            structured_content: None,
            meta: None,
            is_error: Some(true),
        };
        JsonRpcResponse::success(req.id.clone(), serde_json::to_value(result).unwrap())
    }
}

fn validate_modern_metadata(
    req: &JsonRpcRequest,
) -> std::result::Result<(), JsonRpcResponse> {
    let meta = req
        .params
        .get("_meta")
        .and_then(|value| value.as_object())
        .ok_or_else(|| {
            invalid_metadata_response(
                req,
                "Modern requests require a _meta object with protocolVersion and clientCapabilities.",
            )
        })?;

    let requested_version = meta
        .get(MODERN_META_PROTOCOL_VERSION)
        .and_then(|value| value.as_str())
        .ok_or_else(|| {
            invalid_metadata_response(
                req,
                "Modern requests require _meta.io.modelcontextprotocol/protocolVersion.",
            )
        })?;
    if requested_version != MODERN_PROTOCOL_VERSION {
        return Err(JsonRpcResponse::error_with_data(
            req.id.clone(),
            -32602,
            format!("Unsupported MCP protocol version: {requested_version}"),
            Some(json!({
                "supported": SUPPORTED_PROTOCOL_VERSIONS,
                "requested": requested_version,
            })),
        ));
    }

    if !meta
        .get(MODERN_META_CLIENT_CAPABILITIES)
        .is_some_and(|value| value.is_object())
    {
        return Err(invalid_metadata_response(
            req,
            "Modern requests require _meta.io.modelcontextprotocol/clientCapabilities as an object.",
        ));
    }

    if let Some(client_info) = meta.get(MODERN_META_CLIENT_INFO) {
        let valid_client_info = client_info
            .as_object()
            .and_then(|info| info.get("name").and_then(|value| value.as_str()))
            .is_some_and(|name| !name.is_empty())
            && client_info
                .as_object()
                .and_then(|info| info.get("version").and_then(|value| value.as_str()))
                .is_some_and(|version| !version.is_empty());
        if !valid_client_info {
            return Err(invalid_metadata_response(
                req,
                "Modern _meta.io.modelcontextprotocol/clientInfo must contain non-empty name and version strings.",
            ));
        }
    }

    Ok(())
}

fn invalid_metadata_response(req: &JsonRpcRequest, message: &str) -> JsonRpcResponse {
    JsonRpcResponse::error_with_data(
        req.id.clone(),
        -32602,
        message.into(),
        Some(json!({
            "required": [
                MODERN_META_PROTOCOL_VERSION,
                MODERN_META_CLIENT_CAPABILITIES,
            ],
            "optional": [MODERN_META_CLIENT_INFO],
        })),
    )
}

fn server_info_value() -> serde_json::Value {
    json!({
        "name": "whatsapp-mcp",
        "version": env!("CARGO_PKG_VERSION"),
    })
}

fn stamp_modern_response(mut response: JsonRpcResponse, method: &str) -> JsonRpcResponse {
    let Some(result) = response.result.as_mut().and_then(|value| value.as_object_mut()) else {
        return response;
    };

    result
        .entry("resultType")
        .or_insert_with(|| json!("complete"));
    if matches!(method, "server/discover" | "tools/list" | "resources/list" | "resources/read") {
        result.entry("ttlMs").or_insert_with(|| json!(0));
        result
            .entry("cacheScope")
            .or_insert_with(|| json!("private"));
    }

    let meta = result
        .entry("_meta")
        .or_insert_with(|| json!({}));
    if !meta.is_object() {
        *meta = json!({});
    }
    if let Some(meta) = meta.as_object_mut() {
        meta.entry(MODERN_META_SERVER_INFO)
            .or_insert_with(server_info_value);
    }
    response
}

fn pairing_response(req: &JsonRpcRequest, snapshot: &PairingSnapshot) -> JsonRpcResponse {
    let now_ms = unix_time_ms();
    let qr_is_fresh = snapshot
        .qr_created_at_ms
        .is_some_and(|created_at| now_ms.saturating_sub(created_at) < QR_TTL_MS)
        && snapshot.qr_payload.is_some();

    let (phase, connected, can_retry, message) = match snapshot.phase {
        PairingPhase::Preparing => (
            "preparing",
            false,
            false,
            "Preparing a private WhatsApp pairing code.",
        ),
        PairingPhase::AwaitingScan if snapshot.qr_payload.is_none() => (
            "preparing",
            false,
            false,
            "Waiting for WhatsApp to issue a private pairing code.",
        ),
        PairingPhase::AwaitingScan if qr_is_fresh => (
            "awaiting_scan",
            false,
            false,
            "Scan the QR code in the Codex pairing app.",
        ),
        PairingPhase::AwaitingScan => (
            "expired",
            false,
            true,
            "The pairing code expired. Retry from the Codex pairing app.",
        ),
        PairingPhase::Paired => (
            "paired",
            false,
            false,
            "Pairing succeeded. Reconnecting the saved session.",
        ),
        PairingPhase::Connected => (
            "connected",
            true,
            false,
            "WhatsApp is connected.",
        ),
        PairingPhase::Disconnected if snapshot.account_jid.is_none() => (
            "disconnected",
            false,
            true,
            "No paired session exists. Retry first-time setup from the Codex pairing app.",
        ),
        PairingPhase::Disconnected => (
            "session_disconnected",
            false,
            true,
            "A saved session exists but is disconnected. Retry reconnects it; if WhatsApp rejects the registration, it is archived before a fresh QR is generated. Chat history is preserved.",
        ),
        PairingPhase::Unsupported => (
            "unsupported",
            false,
            false,
            "This WhatsApp backend does not support in-Codex pairing.",
        ),
    };

    let structured = json!({
        "phase": phase,
        "connected": connected,
        "hasQr": qr_is_fresh,
        "canRetry": can_retry,
        "message": message,
    });

    let mut meta = json!({
        "qrExpiresAtMs": snapshot.qr_created_at_ms.map(|created_at| created_at + QR_TTL_MS),
    });
    if qr_is_fresh {
        if let Some(payload) = snapshot.qr_payload.as_deref() {
            if let Ok(qr) = qr_matrix(payload) {
                meta["qr"] = qr;
            }
        }
    }

    let result = ToolResult {
        content: vec![ToolResultContent {
            content_type: "text".into(),
            text: message.into(),
        }],
        structured_content: Some(structured),
        meta: Some(meta),
        is_error: None,
    };
    JsonRpcResponse::success(req.id.clone(), serde_json::to_value(result).unwrap())
}

fn pairing_retry_failure_response(
    req: &JsonRpcRequest,
    snapshot: Option<&PairingSnapshot>,
    error: &str,
) -> JsonRpcResponse {
    let has_saved_session = snapshot.is_some_and(|snapshot| snapshot.account_jid.is_some());
    let phase = if has_saved_session {
        "session_disconnected"
    } else {
        "disconnected"
    };
    let message = format!(
        "Could not reconnect WhatsApp: {error} Retry to try again. The database and any archived registration were preserved."
    );
    let structured = json!({
        "phase": phase,
        "connected": false,
        "hasQr": false,
        "canRetry": true,
        "message": message,
    });
    let result = ToolResult {
        content: vec![ToolResultContent {
            content_type: "text".into(),
            text: message,
        }],
        structured_content: Some(structured),
        meta: Some(json!({ "qrExpiresAtMs": serde_json::Value::Null })),
        is_error: Some(true),
    };
    JsonRpcResponse::success(req.id.clone(), serde_json::to_value(result).unwrap())
}

fn qr_matrix(payload: &str) -> Result<serde_json::Value> {
    let code = QrCode::with_error_correction_level(payload.as_bytes(), EcLevel::L)?;
    let size = code.width();
    let modules: String = code
        .into_colors()
        .into_iter()
        .map(|color| if color == Color::Dark { '1' } else { '0' })
        .collect();
    Ok(json!({ "size": size, "modules": modules }))
}

fn unix_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn connection_status_payload(is_connected: bool) -> serde_json::Value {
    if is_connected {
        json!({
            "connected": true,
            "status": "active",
            "suggestion": "Session is healthy. You can use list_chats, get_messages, or send_message."
        })
    } else {
        json!({
            "connected": false,
            "status": "disconnected",
            "suggestion": "Session is not active. Use open_pairing to complete first-time setup in Codex."
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        connection_status_payload, pairing_response, unix_time_ms, McpServer,
        PAIRING_WIDGET_HTML,
    };
    use anyhow::{anyhow, Result};
    use async_trait::async_trait;
    use crate::protocol::{
        JsonRpcRequest, LEGACY_PROTOCOL_VERSION, MODERN_META_CLIENT_CAPABILITIES,
        MODERN_META_PROTOCOL_VERSION, MODERN_META_SERVER_INFO, MODERN_PROTOCOL_VERSION,
    };
    use serde_json::json;
    use std::sync::Arc;
    use wa_domain::models::chat::{Chat, ChatId};
    use wa_domain::models::contact::Contact;
    use wa_domain::models::message::{Message, MessageId};
    use wa_domain::ports::{PairingPhase, PairingSnapshot, StoragePort, WhatsAppClientPort};

    struct NoopStorage;

    #[async_trait]
    impl StoragePort for NoopStorage {
        async fn save_message(&self, _: &Message) -> Result<()> {
            Ok(())
        }

        async fn get_messages(
            &self,
            _: &ChatId,
            _: u32,
            _: Option<&MessageId>,
        ) -> Result<Vec<Message>> {
            Ok(vec![])
        }

        async fn save_chat(&self, _: &Chat) -> Result<()> {
            Ok(())
        }

        async fn get_chat(&self, _: &ChatId) -> Result<Option<Chat>> {
            Ok(None)
        }

        async fn save_contact(&self, _: &Contact) -> Result<()> {
            Ok(())
        }

        async fn search_contacts(&self, _: &str) -> Result<Vec<Contact>> {
            Ok(vec![])
        }
    }

    struct FailingRestartClient {
        snapshot: PairingSnapshot,
    }

    #[async_trait]
    impl WhatsAppClientPort for FailingRestartClient {
        async fn connect(&self) -> Result<()> {
            Err(anyhow!("unused test operation"))
        }

        async fn is_connected(&self) -> Result<bool> {
            Ok(false)
        }

        async fn pairing_snapshot(&self) -> Result<PairingSnapshot> {
            Ok(self.snapshot.clone())
        }

        async fn restart_pairing(&self) -> Result<()> {
            Err(anyhow!("saved session reconnect failed"))
        }

        async fn disconnect(&self) -> Result<()> {
            Ok(())
        }

        async fn send_message(&self, _: &ChatId, _: &str) -> Result<Message> {
            Err(anyhow!("unused test operation"))
        }

        async fn send_reaction(&self, _: &ChatId, _: &str, _: &str) -> Result<()> {
            Err(anyhow!("unused test operation"))
        }

        async fn send_image(
            &self,
            _: &ChatId,
            _: &[u8],
            _: &str,
            _: Option<&str>,
        ) -> Result<Message> {
            Err(anyhow!("unused test operation"))
        }

        async fn list_chats(&self) -> Result<Vec<Chat>> {
            Ok(vec![])
        }
    }

    fn protocol_test_server() -> McpServer {
        McpServer::new(
            Arc::new(NoopStorage),
            Arc::new(FailingRestartClient {
                snapshot: PairingSnapshot {
                    phase: PairingPhase::Disconnected,
                    qr_payload: None,
                    qr_created_at_ms: None,
                    account_jid: None,
                },
            }),
        )
    }

    fn request(method: &str, params: serde_json::Value) -> JsonRpcRequest {
        JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: method.into(),
            params,
        }
    }

    #[test]
    fn disconnected_status_does_not_claim_session_health() {
        let status = connection_status_payload(false);
        assert_eq!(status["connected"], false);
        assert_eq!(status["status"], "disconnected");
        assert!(status["suggestion"]
            .as_str()
            .expect("suggestion")
            .contains("open_pairing"));
    }

    #[tokio::test]
    async fn server_discover_advertises_modern_and_legacy_revisions() {
        let server = protocol_test_server();
        let response = server
            .handle_request(&request(
                "server/discover",
                json!({
                    "_meta": {
                        MODERN_META_PROTOCOL_VERSION: MODERN_PROTOCOL_VERSION,
                        MODERN_META_CLIENT_CAPABILITIES: {}
                    }
                }),
            ))
            .await;
        let wire = serde_json::to_value(response).expect("serialize discovery response");
        let result = &wire["result"];
        let versions = result["supportedVersions"].as_array().expect("versions");

        assert_eq!(result["resultType"], "complete");
        assert!(versions.iter().any(|version| version == MODERN_PROTOCOL_VERSION));
        assert!(versions.iter().any(|version| version == LEGACY_PROTOCOL_VERSION));
        assert_eq!(
            result["_meta"][MODERN_META_SERVER_INFO]["name"],
            "whatsapp-mcp"
        );
    }

    #[tokio::test]
    async fn modern_tool_calls_validate_required_request_metadata() {
        let server = protocol_test_server();
        let invalid = server
            .handle_request(&request(
                "tools/call",
                json!({
                    "name": "get_connection_status",
                    "arguments": {},
                    "_meta": {
                        MODERN_META_PROTOCOL_VERSION: MODERN_PROTOCOL_VERSION
                    }
                }),
            ))
            .await;
        let invalid_wire = serde_json::to_value(invalid).expect("serialize invalid response");
        assert_eq!(invalid_wire["error"]["code"], -32602);
        assert!(invalid_wire["error"]["message"]
            .as_str()
            .expect("error message")
            .contains("clientCapabilities"));

        let modern_initialize = server
            .handle_request(&request(
                "initialize",
                json!({
                    "_meta": {
                        MODERN_META_PROTOCOL_VERSION: MODERN_PROTOCOL_VERSION,
                        MODERN_META_CLIENT_CAPABILITIES: {}
                    }
                }),
            ))
            .await;
        let modern_initialize_wire =
            serde_json::to_value(modern_initialize).expect("serialize modern initialize");
        assert_eq!(modern_initialize_wire["error"]["code"], -32602);

        let valid = server
            .handle_request(&request(
                "tools/call",
                json!({
                    "name": "get_connection_status",
                    "arguments": {},
                    "_meta": {
                        MODERN_META_PROTOCOL_VERSION: MODERN_PROTOCOL_VERSION,
                        MODERN_META_CLIENT_CAPABILITIES: {}
                    }
                }),
            ))
            .await;
        let valid_wire = serde_json::to_value(valid).expect("serialize valid response");
        assert_eq!(valid_wire["result"]["resultType"], "complete");
        assert_eq!(
            valid_wire["result"]["_meta"][MODERN_META_SERVER_INFO]["name"],
            "whatsapp-mcp"
        );
    }

    #[tokio::test]
    async fn legacy_initialize_and_tools_list_remain_supported() {
        let server = protocol_test_server();
        let initialize = server
            .handle_request(&request(
                "initialize",
                json!({
                    "protocolVersion": LEGACY_PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": {"name": "legacy-test", "version": "1.0"}
                }),
            ))
            .await;
        let initialize_wire = serde_json::to_value(initialize).expect("serialize initialize");
        assert_eq!(
            initialize_wire["result"]["protocolVersion"],
            LEGACY_PROTOCOL_VERSION
        );

        let tools = server
            .handle_request(&request("tools/list", json!({})))
            .await;
        let tools_wire = serde_json::to_value(tools).expect("serialize tools list");
        assert!(tools_wire["result"]["tools"].as_array().is_some());
        assert!(tools_wire["result"].get("resultType").is_none());
        assert!(tools_wire["result"].get("_meta").is_none());
    }

    #[test]
    fn pairing_response_keeps_qr_payload_out_of_model_visible_fields() {
        let secret = "private-whatsapp-qr-payload";
        let request = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: "tools/call".into(),
            params: json!({}),
        };
        let snapshot = PairingSnapshot {
            phase: PairingPhase::AwaitingScan,
            qr_payload: Some(secret.into()),
            qr_created_at_ms: Some(unix_time_ms()),
            account_jid: None,
        };

        let response = pairing_response(&request, &snapshot);
        let wire = serde_json::to_value(response).expect("serialize response");
        let result = &wire["result"];
        assert_eq!(result["structuredContent"]["hasQr"], true);
        assert!(result["_meta"]["qr"]["size"].as_u64().unwrap() > 0);
        assert!(!serde_json::to_string(result).unwrap().contains(secret));
        assert!(result["structuredContent"].get("qr").is_none());
    }

    #[test]
    fn saved_disconnected_session_offers_preserving_reconnect() {
        let request = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: "tools/call".into(),
            params: json!({}),
        };
        let snapshot = PairingSnapshot {
            phase: PairingPhase::Disconnected,
            qr_payload: None,
            qr_created_at_ms: None,
            account_jid: Some("12345:1@s.whatsapp.net".into()),
        };

        let response = pairing_response(&request, &snapshot);
        let wire = serde_json::to_value(response).expect("serialize response");
        let structured = &wire["result"]["structuredContent"];
        let message = structured["message"].as_str().expect("message");

        assert_eq!(structured["phase"], "session_disconnected");
        assert_eq!(structured["connected"], false);
        assert_eq!(structured["hasQr"], false);
        assert_eq!(structured["canRetry"], true);
        assert!(message.contains("reconnect"));
        assert!(message.contains("preserved"));
    }

    #[test]
    fn first_time_disconnected_session_still_offers_pairing_retry() {
        let request = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: "tools/call".into(),
            params: json!({}),
        };
        let snapshot = PairingSnapshot {
            phase: PairingPhase::Disconnected,
            qr_payload: None,
            qr_created_at_ms: None,
            account_jid: None,
        };

        let response = pairing_response(&request, &snapshot);
        let wire = serde_json::to_value(response).expect("serialize response");
        let structured = &wire["result"]["structuredContent"];
        let message = structured["message"].as_str().expect("message");

        assert_eq!(structured["phase"], "disconnected");
        assert_eq!(structured["canRetry"], true);
        assert!(message.contains("first-time setup"));
    }

    #[tokio::test]
    async fn restart_failure_keeps_retryable_structured_state_without_qr() {
        let secret = "private-reconnect-qr-payload";
        let server = McpServer::new(
            Arc::new(NoopStorage),
            Arc::new(FailingRestartClient {
                snapshot: PairingSnapshot {
                    phase: PairingPhase::Disconnected,
                    qr_payload: Some(secret.into()),
                    qr_created_at_ms: Some(unix_time_ms()),
                    account_jid: Some("12345:1@s.whatsapp.net".into()),
                },
            }),
        );
        let request = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: Some(json!(1)),
            method: "tools/call".into(),
            params: json!({"name": "restart_pairing", "arguments": {}}),
        };

        let response = server.tool_restart_pairing(&request).await;
        let wire = serde_json::to_value(response).expect("serialize response");
        let result = &wire["result"];
        let structured = &result["structuredContent"];
        let serialized = serde_json::to_string(result).expect("serialize tool result");

        assert_eq!(result["isError"], true);
        assert_eq!(structured["phase"], "session_disconnected");
        assert_eq!(structured["connected"], false);
        assert_eq!(structured["hasQr"], false);
        assert_eq!(structured["canRetry"], true);
        assert!(structured["message"]
            .as_str()
            .expect("message")
            .contains("database and any archived registration were preserved"));
        assert!(result["_meta"].get("qr").is_none());
        assert!(!serialized.contains(secret));
    }

    #[test]
    fn pairing_widget_uses_the_mcp_apps_bridge_without_external_assets() {
        assert!(PAIRING_WIDGET_HTML.contains("ui/initialize"));
        assert!(PAIRING_WIDGET_HTML.contains("ui/notifications/initialized"));
        assert!(PAIRING_WIDGET_HTML.contains("tools/call"));
        assert!(PAIRING_WIDGET_HTML.contains("RPC_TIMEOUT_MS"));
        assert!(PAIRING_WIDGET_HTML.contains("Nessuna sessione salvata è stata cancellata"));
        assert!(!PAIRING_WIDGET_HTML.contains("https://"));
        assert!(!PAIRING_WIDGET_HTML.contains("http://"));
    }
}
