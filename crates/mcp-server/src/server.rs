//! MCP Server — stdio transport with JSON-RPC 2.0.
//!
//! Handles the MCP lifecycle: initialize → initialized → tools/list → tools/call.

use crate::protocol::{
    JsonRpcRequest, JsonRpcResponse, ServerCapabilities, ServerInfo, ToolResult, ToolResultContent,
    ToolsCapability,
};
use crate::tools::tool_registry;
use anyhow::Result;
use serde_json::json;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use wa_domain::ports::{StoragePort, WhatsAppClientPort};

pub struct McpServer {
    storage: Arc<dyn StoragePort>,
    wa_client: Arc<dyn WhatsAppClientPort>,
}

impl McpServer {
    pub fn new(storage: Arc<dyn StoragePort>, wa_client: Arc<dyn WhatsAppClientPort>) -> Self {
        Self { storage, wa_client }
    }

    /// Run the MCP server on stdio (blocking).
    pub async fn run_stdio(&self) -> Result<()> {
        let stdin = tokio::io::stdin();
        let mut stdout = tokio::io::stdout();
        let mut reader = BufReader::new(stdin);
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
                        serde_json::Value::Null,
                        -32700,
                        format!("Parse error: {e}"),
                    );
                    let out = serde_json::to_string(&err_response)? + "\n";
                    stdout.write_all(out.as_bytes()).await?;
                    stdout.flush().await?;
                    continue;
                }
            };

            let response = self.handle_request(&request).await;
            let out = serde_json::to_string(&response)? + "\n";
            stdout.write_all(out.as_bytes()).await?;
            stdout.flush().await?;
        }

        Ok(())
    }

    async fn handle_request(&self, req: &JsonRpcRequest) -> JsonRpcResponse {
        match req.method.as_str() {
            "initialize" => self.handle_initialize(req),
            "notifications/initialized" => {
                // Client acknowledged — no response needed for notifications,
                // but we return an empty success to avoid breaking the loop.
                JsonRpcResponse::success(req.id.clone(), json!({}))
            }
            "tools/list" => self.handle_tools_list(req),
            "tools/call" => self.handle_tools_call(req).await,
            _ => JsonRpcResponse::error(
                req.id.clone(),
                -32601,
                format!("Method not found: {}", req.method),
            ),
        }
    }

    fn handle_initialize(&self, req: &JsonRpcRequest) -> JsonRpcResponse {
        JsonRpcResponse::success(
            req.id.clone(),
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": ServerCapabilities {
                    tools: Some(ToolsCapability { list_changed: false }),
                },
                "serverInfo": ServerInfo {
                    name: "whatsapp-mcp".into(),
                    version: env!("CARGO_PKG_VERSION").into(),
                },
            }),
        )
    }

    fn handle_tools_list(&self, req: &JsonRpcRequest) -> JsonRpcResponse {
        let tools = tool_registry();
        JsonRpcResponse::success(req.id.clone(), json!({ "tools": tools }))
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
            Some("get_connection_status") => self.tool_connection_status(req).await,
            Some(unknown) => JsonRpcResponse::error(
                req.id.clone(),
                -32602,
                format!(
                    "Unknown tool '{}'. Available tools: list_chats, get_messages, search_contacts, get_chat_info, send_message, get_connection_status.",
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
        _args: &serde_json::Value,
    ) -> JsonRpcResponse {
        match self.wa_client.list_chats().await {
            Ok(chats) => {
                let result = ToolResult {
                    content: vec![ToolResultContent {
                        content_type: "text".into(),
                        text: serde_json::to_string_pretty(&chats)
                            .unwrap_or_else(|_| "[]".into()),
                    }],
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

    async fn tool_connection_status(&self, req: &JsonRpcRequest) -> JsonRpcResponse {
        // For now, try to connect and report status
        let status = match self.wa_client.connect().await {
            Ok(()) => json!({
                "connected": true,
                "status": "active",
                "suggestion": "Session is healthy. You can use list_chats, get_messages, or send_message."
            }),
            Err(e) => json!({
                "connected": false,
                "status": "disconnected",
                "error": e.to_string(),
                "suggestion": "Session is not active. The user may need to scan a QR code to reconnect."
            }),
        };
        let result = ToolResult {
            content: vec![ToolResultContent {
                content_type: "text".into(),
                text: serde_json::to_string_pretty(&status).unwrap_or_else(|_| "{}".into()),
            }],
            is_error: None,
        };
        JsonRpcResponse::success(req.id.clone(), serde_json::to_value(result).unwrap())
    }

    // ─── Helpers ─────────────────────────────────────────────────────

    fn tool_error(&self, req: &JsonRpcRequest, message: &str) -> JsonRpcResponse {
        let result = ToolResult {
            content: vec![ToolResultContent {
                content_type: "text".into(),
                text: message.to_string(),
            }],
            is_error: Some(true),
        };
        JsonRpcResponse::success(req.id.clone(), serde_json::to_value(result).unwrap())
    }
}
