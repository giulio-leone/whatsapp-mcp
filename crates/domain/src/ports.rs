use crate::models::chat::{Chat, ChatId};
use crate::models::contact::Contact;
use crate::models::message::{Message, MessageId};
use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PairingPhase {
    Preparing,
    AwaitingScan,
    Paired,
    Connected,
    Disconnected,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingSnapshot {
    pub phase: PairingPhase,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub qr_payload: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub qr_created_at_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_jid: Option<String>,
}

#[async_trait::async_trait]
pub trait WhatsAppClientPort: Send + Sync {
    /// Discovers/connects to WA session and returns session info
    async fn connect(&self) -> Result<()>;
    /// Reports current connection state without initiating a reconnect.
    async fn is_connected(&self) -> Result<bool>;
    /// Returns the current first-setup pairing state. QR payloads are sensitive
    /// and must only be delivered to a trusted UI surface.
    async fn pairing_snapshot(&self) -> Result<PairingSnapshot> {
        Ok(PairingSnapshot {
            phase: PairingPhase::Unsupported,
            qr_payload: None,
            qr_created_at_ms: None,
            account_jid: None,
        })
    }
    /// Retries first-time pairing when no saved session exists, or reconnection
    /// when a saved session is disconnected. Implementations must preserve the
    /// database and session; destructive replacement remains unavailable through
    /// MCP.
    async fn restart_pairing(&self) -> Result<()> {
        Err(anyhow::anyhow!("Pairing is not supported by this backend"))
    }
    /// Disconnects gracefully
    async fn disconnect(&self) -> Result<()>;
    /// Sends a text message to a one-to-one chat.
    async fn send_message(&self, chat_id: &ChatId, text: &str) -> Result<Message>;
    /// Edits one previously sent text message in a one-to-one chat.
    async fn edit_message(&self, _chat_id: &ChatId, _message_id: &str, _text: &str) -> Result<Message> {
        Err(anyhow::anyhow!("Message editing is not supported by this backend"))
    }
    /// Revokes one previously sent message in a one-to-one chat.
    async fn delete_message(&self, _chat_id: &ChatId, _message_id: &str) -> Result<()> {
        Err(anyhow::anyhow!("Message deletion is not supported by this backend"))
    }
    /// Sends an emoji reaction to a message
    async fn send_reaction(&self, chat_id: &ChatId, message_id: &str, emoji: &str) -> Result<()>;
    /// Sends an image message with optional caption
    async fn send_image(&self, chat_id: &ChatId, image_bytes: &[u8], mime: &str, caption: Option<&str>) -> Result<Message>;
    /// Retrieves full list of chats available in the current WA multi-device state
    async fn list_chats(&self) -> Result<Vec<Chat>>;
}

#[async_trait::async_trait]
pub trait StoragePort: Send + Sync {
    async fn save_message(&self, msg: &Message) -> Result<()>;
    async fn get_messages(&self, chat_id: &ChatId, limit: u32, before_cursor: Option<&MessageId>) -> Result<Vec<Message>>;

    async fn get_message(&self, _chat_id: &ChatId, _message_id: &MessageId) -> Result<Option<Message>> {
        Ok(None)
    }

    async fn update_message_text(
        &self,
        _chat_id: &ChatId,
        _message_id: &MessageId,
        _text: &str,
    ) -> Result<bool> {
        Err(anyhow::anyhow!("Message updates are not supported by this storage backend"))
    }

    async fn delete_message(&self, _chat_id: &ChatId, _message_id: &MessageId) -> Result<bool> {
        Err(anyhow::anyhow!("Message deletion is not supported by this storage backend"))
    }
    
    async fn save_chat(&self, chat: &Chat) -> Result<()>;
    async fn get_chat(&self, chat_id: &ChatId) -> Result<Option<Chat>>;
    async fn list_chats(&self) -> Result<Vec<Chat>> {
        Ok(Vec::new())
    }

    async fn set_runtime_connection(&self, _connected: bool, _updated_at_ms: u64) -> Result<()> {
        Ok(())
    }

    async fn get_runtime_connection(&self) -> Result<Option<(bool, u64)>> {
        Ok(None)
    }
    
    async fn save_contact(&self, contact: &Contact) -> Result<()>;
    async fn search_contacts(&self, query: &str) -> Result<Vec<Contact>>;
}
