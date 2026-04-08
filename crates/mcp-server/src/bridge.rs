//! HTTP client for the Go whatsmeow bridge.
//!
//! Implements both `WhatsAppClientPort` and `StoragePort` by forwarding
//! all calls to the Go bridge HTTP API at `http://127.0.0.1:<port>/rpc`.

use anyhow::{Result, anyhow};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use wa_domain::models::chat::{Chat, ChatId};
use wa_domain::models::contact::Contact;
use wa_domain::models::message::{Message, MessageId};
use wa_domain::ports::{StoragePort, WhatsAppClientPort};

pub struct BridgeClient {
    base_url: String,
    http: reqwest::Client,
    req_id: AtomicU64,
}

#[derive(Debug, Deserialize)]
struct BridgeRpcResponse {
    result: Option<Value>,
    error: Option<BridgeRpcError>,
}

#[derive(Debug, Deserialize)]
struct BridgeRpcError {
    code: i32,
    message: String,
}

impl BridgeClient {
    pub fn new(base_url: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            http: reqwest::Client::new(),
            req_id: AtomicU64::new(1),
        }
    }

    fn next_id(&self) -> u64 {
        self.req_id.fetch_add(1, Ordering::Relaxed)
    }

    async fn rpc(&self, method: &str, params: Value) -> Result<Value> {
        let body = json!({
            "method": method,
            "params": params,
            "id": self.next_id()
        });

        let resp = self.http
            .post(format!("{}/rpc", self.base_url))
            .json(&body)
            .send()
            .await
            .map_err(|e| anyhow!("Bridge HTTP error (is wa-bridge running?): {}", e))?;

        let rpc_resp: BridgeRpcResponse = resp.json().await
            .map_err(|e| anyhow!("Bridge response parse error: {}", e))?;

        if let Some(err) = rpc_resp.error {
            return Err(anyhow!("Bridge RPC error ({}): {}", err.code, err.message));
        }

        Ok(rpc_resp.result.unwrap_or(json!(null)))
    }

    pub async fn health(&self) -> Result<Value> {
        let resp = self.http
            .get(format!("{}/health", self.base_url))
            .send()
            .await
            .map_err(|e| anyhow!("Bridge health check failed: {}", e))?;
        resp.json().await.map_err(|e| anyhow!("Health parse error: {}", e))
    }

    fn parse_chat(v: &Value) -> Chat {
        Chat {
            id: ChatId(v["id"].as_str().unwrap_or("").to_string()),
            name: v["name"].as_str().map(|s| s.to_string()),
            unread_count: 0,
            is_group: v["is_group"].as_bool().unwrap_or(false),
            last_message_timestamp: 0,
        }
    }

    fn parse_message(v: &Value) -> Message {
        Message {
            id: MessageId(v["id"].as_str().unwrap_or("").to_string()),
            chat_id: ChatId(v["chat_id"].as_str().unwrap_or("").to_string()),
            sender_id: v["sender_id"].as_str().unwrap_or("").to_string(),
            text: v["text"].as_str().map(|s| s.to_string()),
            media: None,
            timestamp: v["timestamp"].as_i64().unwrap_or(0),
            is_from_me: v["is_from_me"].as_bool().unwrap_or(false),
            is_forwarded: false,
            reply_to_id: None,
        }
    }

    fn parse_contact(v: &Value) -> Contact {
        use wa_domain::models::contact::ContactId;
        Contact {
            id: ContactId(v["id"].as_str().unwrap_or("").to_string()),
            name: v["name"].as_str().map(|s| s.to_string()),
            push_name: v["push_name"].as_str().map(|s| s.to_string()),
            formatted_number: v["formatted_number"].as_str().unwrap_or("").to_string(),
            is_business: v["is_business"].as_bool().unwrap_or(false),
        }
    }
}

#[async_trait]
impl WhatsAppClientPort for BridgeClient {
    async fn connect(&self) -> Result<()> {
        let result = self.rpc("connect", json!({})).await?;
        let status = result["status"].as_str().unwrap_or("");
        match status {
            "connected" => Ok(()),
            "qr_code" => Err(anyhow!(
                "QR code required. Scan with WhatsApp: {}",
                result["qr_code"].as_str().unwrap_or("(no code)")
            )),
            _ => Err(anyhow!("Unexpected connect status: {}", status)),
        }
    }

    async fn disconnect(&self) -> Result<()> {
        self.rpc("disconnect", json!({})).await?;
        Ok(())
    }

    async fn send_message(&self, chat_id: &ChatId, text: &str) -> Result<Message> {
        let result = self.rpc("send_message", json!({
            "chat_id": chat_id.0,
            "text": text
        })).await?;

        Ok(Message {
            id: MessageId(result["id"].as_str().unwrap_or("").to_string()),
            chat_id: chat_id.clone(),
            sender_id: "me".to_string(),
            text: Some(text.to_string()),
            media: None,
            timestamp: result["timestamp"].as_i64().unwrap_or(0),
            is_from_me: true,
            is_forwarded: false,
            reply_to_id: None,
        })
    }

    async fn send_reaction(&self, chat_id: &ChatId, message_id: &str, emoji: &str) -> Result<()> {
        self.rpc("send_reaction", json!({
            "chat_id": chat_id.0,
            "message_id": message_id,
            "emoji": emoji
        })).await?;
        Ok(())
    }

    async fn list_chats(&self) -> Result<Vec<Chat>> {
        let result = self.rpc("list_chats", json!({})).await?;
        let chats = result["chats"]
            .as_array()
            .map(|arr| arr.iter().map(Self::parse_chat).collect())
            .unwrap_or_default();
        Ok(chats)
    }
}

#[async_trait]
impl StoragePort for BridgeClient {
    async fn save_message(&self, _msg: &Message) -> Result<()> {
        // Bridge handles persistence internally
        Ok(())
    }

    async fn get_messages(
        &self,
        chat_id: &ChatId,
        limit: u32,
        before_cursor: Option<&MessageId>,
    ) -> Result<Vec<Message>> {
        let result = self.rpc("get_messages", json!({
            "chat_id": chat_id.0,
            "limit": limit,
            "cursor": before_cursor.map(|c| &c.0)
        })).await?;

        let messages = result["messages"]
            .as_array()
            .map(|arr| arr.iter().map(Self::parse_message).collect())
            .unwrap_or_default();
        Ok(messages)
    }

    async fn save_chat(&self, _chat: &Chat) -> Result<()> {
        Ok(())
    }

    async fn get_chat(&self, chat_id: &ChatId) -> Result<Option<Chat>> {
        match self.rpc("get_chat_info", json!({ "chat_id": chat_id.0 })).await {
            Ok(result) => Ok(Some(Self::parse_chat(&result))),
            Err(_) => Ok(None),
        }
    }

    async fn save_contact(&self, _contact: &Contact) -> Result<()> {
        Ok(())
    }

    async fn search_contacts(&self, query: &str) -> Result<Vec<Contact>> {
        let result = self.rpc("search_contacts", json!({ "query": query })).await?;
        let contacts = result["contacts"]
            .as_array()
            .map(|arr| arr.iter().map(Self::parse_contact).collect())
            .unwrap_or_default();
        Ok(contacts)
    }
}
