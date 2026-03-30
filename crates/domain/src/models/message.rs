use serde::{Deserialize, Serialize};
use super::chat::ChatId;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct MessageId(pub String);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MediaType {
    Image,
    Video,
    Audio,
    Document,
    Sticker,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaContent {
    pub url: String,
    pub mime_type: String,
    pub file_sha256: Vec<u8>,
    pub file_length: u64,
    pub media_type: MediaType,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: MessageId,
    pub chat_id: ChatId,
    pub sender_id: String,
    pub text: Option<String>,
    pub media: Option<MediaContent>,
    pub timestamp: i64,
    pub is_from_me: bool,
    pub is_forwarded: bool,
    pub reply_to_id: Option<MessageId>,
}
