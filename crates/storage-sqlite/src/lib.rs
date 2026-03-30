use anyhow::Result;
use rusqlite::Connection;
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

    async fn save_chat(&self, chat: &Chat) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO chats (id, name, unread_count, is_group, last_message_timestamp) VALUES (?1, ?2, ?3, ?4, ?5)",
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
