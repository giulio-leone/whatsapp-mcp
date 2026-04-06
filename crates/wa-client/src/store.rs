use x25519_dalek::{StaticSecret, PublicKey};
use serde::{Serialize, Deserialize};
use wa_domain::models::chat::Chat;
use crate::crypto::sender_key::SenderKeyRecord;

use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NoiseKey {
    pub priv_key: [u8; 32],
    pub pub_key: [u8; 32],
}

impl NoiseKey {
    pub fn new() -> Self {
        let priv_key = StaticSecret::random_from_rng(rand::thread_rng());
        let pub_key = PublicKey::from(&priv_key);
        Self {
            priv_key: priv_key.to_bytes(),
            pub_key: *pub_key.as_bytes(),
        }
    }
}

/// Persistent key/session store for the WhatsApp client.
///
/// Serialized to/from JSON and persisted to SQLite via the `device_store`
/// table (single-row key-value).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceStore {
    pub noise_key: NoiseKey,
    pub identity_key_priv: [u8; 32],
    pub identity_key_pub: [u8; 32],
    pub registration_id: u32,
    /// Maps JID → serialized Signal session JSON
    pub sessions: HashMap<String, Vec<u8>>,
    /// Maps LID → JID
    pub lid_to_jid: HashMap<String, String>,
    /// Maps "(group_jid, sender_jid)" → SenderKeyRecord
    pub sender_keys: HashMap<String, SenderKeyRecord>,
    /// Cached chat list (not persisted — rebuilt from SQLite / history sync)
    #[serde(skip)]
    pub chats: HashMap<String, Chat>,
    /// Our JID after successful login
    pub our_jid: Option<String>,
}

impl DeviceStore {
    pub fn new() -> Self {
        let identity_priv = StaticSecret::random_from_rng(rand::thread_rng());
        let identity_pub = PublicKey::from(&identity_priv);

        Self {
            noise_key: NoiseKey::new(),
            identity_key_priv: identity_priv.to_bytes(),
            identity_key_pub: *identity_pub.as_bytes(),
            registration_id: (rand::random::<u16>() as u32) | 1, // Must be non-zero
            sessions: HashMap::new(),
            lid_to_jid: HashMap::new(),
            sender_keys: HashMap::new(),
            chats: HashMap::new(),
            our_jid: None,
        }
    }

    pub fn get_session(&self, id: &str) -> Option<&Vec<u8>> {
        self.sessions.get(id)
    }

    pub fn save_session(&mut self, id: String, session: Vec<u8>) {
        self.sessions.insert(id, session);
    }

    pub fn set_lid_mapping(&mut self, lid: String, jid: String) {
        self.lid_to_jid.insert(lid, jid);
    }

    pub fn get_jid_from_lid(&self, lid: &str) -> Option<&String> {
        self.lid_to_jid.get(lid)
    }

    pub fn add_chat(&mut self, chat: Chat) {
        self.chats.insert(chat.id.0.clone(), chat);
    }

    // ─── SenderKey helpers ──────────────────────────────────────────

    fn sender_key_id(group_jid: &str, sender_jid: &str) -> String {
        format!("{}|{}", group_jid, sender_jid)
    }

    pub fn get_sender_key(&self, group_jid: &str, sender_jid: &str) -> Option<&SenderKeyRecord> {
        self.sender_keys.get(&Self::sender_key_id(group_jid, sender_jid))
    }

    pub fn get_sender_key_mut(&mut self, group_jid: &str, sender_jid: &str) -> Option<&mut SenderKeyRecord> {
        self.sender_keys.get_mut(&Self::sender_key_id(group_jid, sender_jid))
    }

    pub fn save_sender_key(&mut self, group_jid: &str, sender_jid: &str, record: SenderKeyRecord) {
        self.sender_keys.insert(Self::sender_key_id(group_jid, sender_jid), record);
    }

    // ─── Persistence ────────────────────────────────────────────────

    /// Save the entire store to a SQLite database.
    pub fn save_to_db(&self, conn: &rusqlite::Connection) -> anyhow::Result<()> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS device_store (
                key TEXT PRIMARY KEY,
                value BLOB NOT NULL
            );"
        )?;
        let json = serde_json::to_vec(self)?;
        conn.execute(
            "INSERT OR REPLACE INTO device_store (key, value) VALUES ('state', ?1)",
            rusqlite::params![json],
        )?;
        Ok(())
    }

    /// Load the store from a SQLite database, or return None if not found.
    pub fn load_from_db(conn: &rusqlite::Connection) -> anyhow::Result<Option<Self>> {
        // Table might not exist yet
        let table_exists: bool = conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='device_store'",
            [],
            |row| row.get::<_, i64>(0),
        )? > 0;

        if !table_exists {
            return Ok(None);
        }

        let result: Result<Vec<u8>, _> = conn.query_row(
            "SELECT value FROM device_store WHERE key = 'state'",
            [],
            |row| row.get(0),
        );

        match result {
            Ok(bytes) => {
                let store: DeviceStore = serde_json::from_slice(&bytes)?;
                Ok(Some(store))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
}
