use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ChatId(pub String);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chat {
    pub id: ChatId,
    pub name: Option<String>,
    pub unread_count: u32,
    pub is_group: bool,
    pub last_message_timestamp: i64,
}
