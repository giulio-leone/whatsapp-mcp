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

/// Signal signed pre-key for device pairing registration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedPreKey {
    pub key_id: u32,
    pub priv_key: [u8; 32],
    pub pub_key: [u8; 32],
    pub signature: Vec<u8>,
}

impl SignedPreKey {
    /// Generate a new signed pre-key, signed by the identity key using XEdDSA.
    ///
    /// This matches whatsmeow's `KeyPair.Sign()` which uses
    /// `ecc.CalculateSignature(ecc.NewDjbECPrivateKey(*kp.Priv), pubKeyForSignature)`
    /// — an XEdDSA signature (Curve25519 → Ed25519 conversion + sign).
    pub fn new(identity_priv_bytes: &[u8; 32]) -> Self {
        let priv_key = StaticSecret::random_from_rng(rand::thread_rng());
        let pub_key = PublicKey::from(&priv_key);
        let key_id = rand::random::<u16>() as u32 | 1;

        // Prepend 0x05 (ecc.DjbType) to the public key before signing,
        // exactly as whatsmeow does in keypair.go:49-51
        let mut pub_key_for_signature = Vec::with_capacity(33);
        pub_key_for_signature.push(5); // ecc.DjbType = 5
        pub_key_for_signature.extend_from_slice(pub_key.as_bytes());

        // Sign using XEdDSA (NOT plain ed25519)
        let signature = crate::crypto::xeddsa::xeddsa_sign(
            identity_priv_bytes,
            &pub_key_for_signature,
        );

        Self {
            key_id,
            priv_key: priv_key.to_bytes(),
            pub_key: *pub_key.as_bytes(),
            signature: signature.to_vec(),
        }
    }
}

/// ADV (Account Device Verification) secret key for QR code generation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdvSecretKey(pub [u8; 32]);

impl AdvSecretKey {
    pub fn new() -> Self {
        let mut bytes = [0u8; 32];
        rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut bytes);
        Self(bytes)
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
    pub signed_prekey: SignedPreKey,
    pub adv_secret: AdvSecretKey,
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
    /// Serialized ADVSignedDeviceIdentity (used for device-identity node in pkmsg sends)
    pub account_identity: Option<Vec<u8>>,
}

impl DeviceStore {
    pub fn new() -> Self {
        let identity_priv = StaticSecret::random_from_rng(rand::thread_rng());
        let identity_pub = PublicKey::from(&identity_priv);
        let identity_priv_bytes = identity_priv.to_bytes();

        Self {
            noise_key: NoiseKey::new(),
            identity_key_priv: identity_priv_bytes,
            identity_key_pub: *identity_pub.as_bytes(),
            registration_id: (rand::random::<u16>() as u32) | 1, // Must be non-zero
            signed_prekey: SignedPreKey::new(&identity_priv_bytes),
            adv_secret: AdvSecretKey::new(),
            sessions: HashMap::new(),
            lid_to_jid: HashMap::new(),
            sender_keys: HashMap::new(),
            chats: HashMap::new(),
            our_jid: None,
            account_identity: None,
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

    /// Archive the current registered identity and atomically activate a fresh
    /// unregistered store. Message/chat tables are not touched.
    pub fn archive_and_replace_in_db(
        &self,
        conn: &mut rusqlite::Connection,
        replacement: &DeviceStore,
    ) -> anyhow::Result<String> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS device_store (
                key TEXT PRIMARY KEY,
                value BLOB NOT NULL
            );"
        )?;
        let archived_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let archive_key = format!("state.invalid.{archived_at}");
        let current = serde_json::to_vec(self)?;
        let fresh = serde_json::to_vec(replacement)?;
        let transaction = conn.transaction()?;
        transaction.execute(
            "INSERT INTO device_store (key, value) VALUES (?1, ?2)",
            rusqlite::params![archive_key, current],
        )?;
        transaction.execute(
            "INSERT OR REPLACE INTO device_store (key, value) VALUES ('state', ?1)",
            rusqlite::params![fresh],
        )?;
        transaction.commit()?;
        Ok(archive_key)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_registration_is_archived_before_fresh_pairing_state_is_activated() {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        let mut current = DeviceStore::new();
        current.our_jid = Some("saved-device@s.whatsapp.net".into());
        current.save_to_db(&conn).unwrap();
        conn.execute_batch(
            "CREATE TABLE messages (id TEXT PRIMARY KEY, body TEXT);
             INSERT INTO messages (id, body) VALUES ('kept', 'history');"
        ).unwrap();

        let fresh = DeviceStore::new();
        let archive_key = current.archive_and_replace_in_db(&mut conn, &fresh).unwrap();
        let active = DeviceStore::load_from_db(&conn).unwrap().unwrap();
        let archived: Vec<u8> = conn.query_row(
            "SELECT value FROM device_store WHERE key = ?1",
            [&archive_key],
            |row| row.get(0),
        ).unwrap();
        let archived: DeviceStore = serde_json::from_slice(&archived).unwrap();
        let message_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM messages",
            [],
            |row| row.get(0),
        ).unwrap();

        assert_eq!(archived.our_jid, current.our_jid);
        assert!(active.our_jid.is_none());
        assert_eq!(message_count, 1);
    }
}
