use crate::models::chat::{Chat, ChatId};
use crate::models::contact::Contact;
use crate::models::message::{Message, MessageId};
use anyhow::Result;

#[async_trait::async_trait]
pub trait WhatsAppClientPort: Send + Sync {
    /// Discovers/connects to WA session and returns session info
    async fn connect(&self) -> Result<()>;
    /// Disconnects gracefully
    async fn disconnect(&self) -> Result<()>;
    /// Sends a text message to a specific chat (Contact or Group)
    async fn send_message(&self, chat_id: &ChatId, text: &str) -> Result<Message>;
    /// Retrieves full list of chats available in the current WA multi-device state
    async fn list_chats(&self) -> Result<Vec<Chat>>;
}

#[async_trait::async_trait]
pub trait StoragePort: Send + Sync {
    async fn save_message(&self, msg: &Message) -> Result<()>;
    async fn get_messages(&self, chat_id: &ChatId, limit: u32, before_cursor: Option<&MessageId>) -> Result<Vec<Message>>;
    
    async fn save_chat(&self, chat: &Chat) -> Result<()>;
    async fn get_chat(&self, chat_id: &ChatId) -> Result<Option<Chat>>;
    
    async fn save_contact(&self, contact: &Contact) -> Result<()>;
    async fn search_contacts(&self, query: &str) -> Result<Vec<Contact>>;
}
