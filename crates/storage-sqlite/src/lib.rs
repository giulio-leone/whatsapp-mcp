use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};
use std::sync::Mutex;
use wa_domain::models::chat::{Chat, ChatId};
use wa_domain::models::contact::Contact;
use wa_domain::models::message::{Message, MessageId};
use wa_domain::ports::StoragePort;

pub struct SqliteStorage {
    conn: Mutex<Connection>,
}

impl SqliteStorage {
    pub fn new(db_path: &str) -> Result<Self> {
        let conn = Connection::open(db_path)?;
        Self::init_db(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn init_db(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS messages (
                 id TEXT PRIMARY KEY,
                 chat_id TEXT NOT NULL,
                 sender_id TEXT NOT NULL,
                 text TEXT,
                 timestamp INTEGER NOT NULL,
                 is_from_me INTEGER NOT NULL,
                 is_forwarded INTEGER NOT NULL,
                 reply_to_id TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_messages_chat ON messages(chat_id, timestamp DESC);

            CREATE TABLE IF NOT EXISTS chats (
                 id TEXT PRIMARY KEY,
                 name TEXT,
                 unread_count INTEGER NOT NULL,
                 is_group INTEGER NOT NULL,
                 last_message_timestamp INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS contacts (
                 id TEXT PRIMARY KEY,
                 name TEXT,
                 push_name TEXT,
                 formatted_number TEXT NOT NULL,
                 is_business INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS runtime_state (
                 key TEXT PRIMARY KEY,
                 connected INTEGER NOT NULL,
                 updated_at_ms INTEGER NOT NULL
            );
            ",
        )?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl StoragePort for SqliteStorage {
    async fn save_message(&self, msg: &Message) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO messages (id, chat_id, sender_id, text, timestamp, is_from_me, is_forwarded, reply_to_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                msg.id.0,
                msg.chat_id.0,
                msg.sender_id,
                msg.text,
                msg.timestamp,
                msg.is_from_me as i32,
                msg.is_forwarded as i32,
                msg.reply_to_id.as_ref().map(|id| id.0.clone())
            ],
        )?;
        Ok(())
    }

    async fn get_messages(
        &self,
        chat_id: &ChatId,
        limit: u32,
        before_cursor: Option<&MessageId>,
    ) -> Result<Vec<Message>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = match before_cursor {
            Some(cursor) => {
                let mut s = conn.prepare(
                    "SELECT id, chat_id, sender_id, text, timestamp, is_from_me, is_forwarded, reply_to_id
                     FROM messages
                     WHERE chat_id = ?1 AND timestamp < (SELECT timestamp FROM messages WHERE id = ?2)
                     ORDER BY timestamp DESC
                     LIMIT ?3",
                )?;
                let rows = s
                    .query_map(rusqlite::params![chat_id.0, cursor.0, limit], |row| {
                        Ok(Message {
                            id: MessageId(row.get(0)?),
                            chat_id: ChatId(row.get(1)?),
                            sender_id: row.get(2)?,
                            text: row.get(3)?,
                            media: None,
                            timestamp: row.get(4)?,
                            is_from_me: row.get::<_, i32>(5)? != 0,
                            is_forwarded: row.get::<_, i32>(6)? != 0,
                            reply_to_id: row.get::<_, Option<String>>(7)?.map(MessageId),
                        })
                    })?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                return Ok(rows);
            }
            None => conn.prepare(
                "SELECT id, chat_id, sender_id, text, timestamp, is_from_me, is_forwarded, reply_to_id
                 FROM messages
                 WHERE chat_id = ?1
                 ORDER BY timestamp DESC
                 LIMIT ?2",
            )?,
        };
        let rows = stmt
            .query_map(rusqlite::params![chat_id.0, limit], |row| {
                Ok(Message {
                    id: MessageId(row.get(0)?),
                    chat_id: ChatId(row.get(1)?),
                    sender_id: row.get(2)?,
                    text: row.get(3)?,
                    media: None,
                    timestamp: row.get(4)?,
                    is_from_me: row.get::<_, i32>(5)? != 0,
                    is_forwarded: row.get::<_, i32>(6)? != 0,
                    reply_to_id: row.get::<_, Option<String>>(7)?.map(MessageId),
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    async fn get_message(&self, chat_id: &ChatId, message_id: &MessageId) -> Result<Option<Message>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, chat_id, sender_id, text, timestamp, is_from_me, is_forwarded, reply_to_id
             FROM messages WHERE chat_id = ?1 AND id = ?2",
        )?;
        let message = stmt
            .query_row(rusqlite::params![chat_id.0, message_id.0], |row| {
                Ok(Message {
                    id: MessageId(row.get(0)?),
                    chat_id: ChatId(row.get(1)?),
                    sender_id: row.get(2)?,
                    text: row.get(3)?,
                    media: None,
                    timestamp: row.get(4)?,
                    is_from_me: row.get::<_, i32>(5)? != 0,
                    is_forwarded: row.get::<_, i32>(6)? != 0,
                    reply_to_id: row.get::<_, Option<String>>(7)?.map(MessageId),
                })
            })
            .optional()?;
        Ok(message)
    }

    async fn update_message_text(
        &self,
        chat_id: &ChatId,
        message_id: &MessageId,
        text: &str,
        timestamp: i64,
    ) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let updated = conn.execute(
            "UPDATE messages SET text = ?3, timestamp = ?4
             WHERE chat_id = ?1 AND id = ?2 AND is_from_me = 1",
            rusqlite::params![chat_id.0, message_id.0, text, timestamp],
        )?;
        Ok(updated == 1)
    }

    async fn delete_message(&self, chat_id: &ChatId, message_id: &MessageId) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let deleted = conn.execute(
            "DELETE FROM messages
             WHERE chat_id = ?1 AND id = ?2 AND is_from_me = 1",
            rusqlite::params![chat_id.0, message_id.0],
        )?;
        Ok(deleted == 1)
    }

    async fn save_chat(&self, chat: &Chat) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO chats (id, name, unread_count, is_group, last_message_timestamp)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(id) DO UPDATE SET
                 name = COALESCE(excluded.name, chats.name),
                 unread_count = MAX(excluded.unread_count, chats.unread_count),
                 is_group = excluded.is_group,
                 last_message_timestamp = MAX(excluded.last_message_timestamp, chats.last_message_timestamp)",
            rusqlite::params![
                chat.id.0,
                chat.name,
                chat.unread_count,
                chat.is_group as i32,
                chat.last_message_timestamp
            ],
        )?;
        Ok(())
    }

    async fn get_chat(&self, chat_id: &ChatId) -> Result<Option<Chat>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, name, unread_count, is_group, last_message_timestamp FROM chats WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(rusqlite::params![chat_id.0], |row| {
            Ok(Chat {
                id: ChatId(row.get(0)?),
                name: row.get(1)?,
                unread_count: row.get(2)?,
                is_group: row.get::<_, i32>(3)? != 0,
                last_message_timestamp: row.get(4)?,
            })
        })?;
        match rows.next() {
            Some(Ok(chat)) => Ok(Some(chat)),
            _ => Ok(None),
        }
    }

    async fn list_chats(&self) -> Result<Vec<Chat>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, name, unread_count, is_group, last_message_timestamp
             FROM chats
             ORDER BY last_message_timestamp DESC",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok(Chat {
                    id: ChatId(row.get(0)?),
                    name: row.get(1)?,
                    unread_count: row.get(2)?,
                    is_group: row.get::<_, i32>(3)? != 0,
                    last_message_timestamp: row.get(4)?,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    async fn set_runtime_connection(&self, connected: bool, updated_at_ms: u64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO runtime_state (key, connected, updated_at_ms)
             VALUES ('whatsapp_connection', ?1, ?2)
             ON CONFLICT(key) DO UPDATE SET
                 connected = excluded.connected,
                 updated_at_ms = excluded.updated_at_ms",
            rusqlite::params![connected as i32, updated_at_ms],
        )?;
        Ok(())
    }

    async fn get_runtime_connection(&self) -> Result<Option<(bool, u64)>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT connected, updated_at_ms FROM runtime_state WHERE key = 'whatsapp_connection'",
        )?;
        let mut rows = stmt.query_map([], |row| {
            Ok((row.get::<_, i32>(0)? != 0, row.get::<_, u64>(1)?))
        })?;
        match rows.next() {
            Some(Ok(state)) => Ok(Some(state)),
            Some(Err(error)) => Err(error.into()),
            None => Ok(None),
        }
    }

    async fn save_contact(&self, contact: &Contact) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO contacts (id, name, push_name, formatted_number, is_business) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![
                contact.id.0,
                contact.name,
                contact.push_name,
                contact.formatted_number,
                contact.is_business as i32
            ],
        )?;
        Ok(())
    }

    async fn search_contacts(&self, query: &str) -> Result<Vec<Contact>> {
        let conn = self.conn.lock().unwrap();
        let pattern = format!("%{query}%");
        let mut stmt = conn.prepare(
            "SELECT id, name, push_name, formatted_number, is_business FROM contacts
             WHERE name LIKE ?1 OR push_name LIKE ?1 OR formatted_number LIKE ?1
             LIMIT 20",
        )?;
        let rows = stmt
            .query_map(rusqlite::params![pattern], |row| {
                Ok(Contact {
                    id: wa_domain::models::contact::ContactId(row.get(0)?),
                    name: row.get(1)?,
                    push_name: row.get(2)?,
                    formatted_number: row.get(3)?,
                    is_business: row.get::<_, i32>(4)? != 0,
                })
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::SqliteStorage;
    use wa_domain::models::chat::{Chat, ChatId};
    use wa_domain::models::message::{Message, MessageId};
    use wa_domain::ports::StoragePort;

    #[tokio::test]
    async fn shares_runtime_connection_and_persisted_chats() {
        let storage = SqliteStorage::new(":memory:").expect("in-memory storage");
        storage
            .save_chat(&Chat {
                id: ChatId("recent-chat".into()),
                name: None,
                unread_count: 0,
                is_group: false,
                last_message_timestamp: 42,
            })
            .await
            .expect("save chat");
        storage
            .set_runtime_connection(true, 1234)
            .await
            .expect("save runtime state");

        let chats = storage.list_chats().await.expect("list chats");
        let runtime = storage
            .get_runtime_connection()
            .await
            .expect("read runtime state");

        assert_eq!(chats.len(), 1);
        assert_eq!(chats[0].id.0, "recent-chat");
        assert_eq!(runtime, Some((true, 1234)));
    }

    #[tokio::test]
    async fn updates_and_deletes_only_owned_message_in_exact_chat() {
        let storage = SqliteStorage::new(":memory:").expect("in-memory storage");
        let owned = Message {
            id: MessageId("owned-message".into()),
            chat_id: ChatId("target-chat".into()),
            sender_id: "me".into(),
            text: Some("before".into()),
            media: None,
            timestamp: 42,
            is_from_me: true,
            is_forwarded: false,
            reply_to_id: None,
        };
        let incoming = Message {
            id: MessageId("incoming-message".into()),
            chat_id: ChatId("target-chat".into()),
            sender_id: "someone".into(),
            text: Some("incoming".into()),
            is_from_me: false,
            ..owned.clone()
        };
        storage.save_message(&owned).await.expect("save owned message");
        storage
            .save_message(&incoming)
            .await
            .expect("save incoming message");

        assert!(!storage
            .update_message_text(
                &ChatId("wrong-chat".into()),
                &owned.id,
                "must-not-change",
                99,
            )
            .await
            .expect("wrong-chat update"));
        assert!(!storage
            .update_message_text(&incoming.chat_id, &incoming.id, "must-not-change", 99)
            .await
            .expect("incoming update"));
        assert!(storage
            .update_message_text(&owned.chat_id, &owned.id, "after", 99)
            .await
            .expect("owned update"));
        assert_eq!(
            storage
                .get_message(&owned.chat_id, &owned.id)
                .await
                .expect("read owned")
                .expect("owned exists")
                .text
                .as_deref(),
            Some("after")
        );

        assert!(!storage
            .delete_message(&incoming.chat_id, &incoming.id)
            .await
            .expect("incoming delete"));
        assert!(storage
            .delete_message(&owned.chat_id, &owned.id)
            .await
            .expect("owned delete"));
        assert!(storage
            .get_message(&owned.chat_id, &owned.id)
            .await
            .expect("read deleted")
            .is_none());
    }
}
