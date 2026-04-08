use crate::binary::noise::{NoiseHandshake, NOISE_MODE};
use crate::socket::{FrameSocket, NoiseSender, NoiseReceiver};
use crate::proto::wa_web_protobufs_wa6::{HandshakeMessage, ClientPayload};
use crate::proto::wa_web_protobufs_wa6::handshake_message::{ClientHello, ClientFinish};
use crate::binary::tokens::WA_CONN_HEADER;
use crate::store::DeviceStore;
use crate::binary::node::{Node, AttrValue, Content};
use crate::binary::{Encoder, Decoder};
use x25519_dalek::{StaticSecret, PublicKey};
use prost::Message as ProstMessage;
use anyhow::{Result, anyhow, Context};
use std::time::Duration;
use tokio::time::timeout;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, oneshot};

use wa_domain::ports::WhatsAppClientPort;
use wa_domain::models::chat::{Chat, ChatId};
use wa_domain::models::message::{Message, MessageId};
use crate::crypto::session::Session;

// ─── Events ─────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum WhatsAppEvent {
    QrCode(String),
    /// Pairing succeeded — session saved but client must reconnect with login payload
    PairSuccess { jid: String },
    /// Fully connected and ready to send/receive messages
    Connected { jid: String },
    MessageReceived(Message),
    ReceiptReceived { id: String, from: String, timestamp: i64 },
    PresenceUpdate { jid: String, available: bool, last_seen: Option<i64> },
    HistorySynced { chat_count: usize },
    Disconnected,
}

// ─── Connection State ───────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum ConnectionState {
    Disconnected,
    Connecting,
    WaitingForQr,
    Connected,
}

// ─── Client ─────────────────────────────────────────────────────────

#[derive(Clone)]
pub struct WhatsAppClient {
    inner: Arc<Mutex<WhatsAppClientInner>>,
    pub store: Arc<Mutex<DeviceStore>>,
    event_rx: Arc<Mutex<tokio::sync::mpsc::UnboundedReceiver<WhatsAppEvent>>>,
    state: Arc<Mutex<ConnectionState>>,
    db_path: String,
}

struct WhatsAppClientInner {
    sender: Option<NoiseSender>,
    noise: NoiseHandshake,
    iq_tracker: Arc<Mutex<HashMap<String, oneshot::Sender<Node>>>>,
    event_tx: tokio::sync::mpsc::UnboundedSender<WhatsAppEvent>,
    msg_counter: u64,
}

impl WhatsAppClient {
    pub fn new() -> Self {
        Self::with_db_path("whatsapp.db")
    }

    pub fn with_db_path(db_path: &str) -> Self {
        let (event_tx, event_rx) = tokio::sync::mpsc::unbounded_channel();

        // Try to load existing DeviceStore from SQLite
        let store = match rusqlite::Connection::open(db_path) {
            Ok(conn) => {
                match DeviceStore::load_from_db(&conn) {
                    Ok(Some(s)) => {
                        tracing::info!("Loaded existing device store from {}", db_path);
                        s
                    }
                    _ => {
                        tracing::info!("Creating new device store");
                        DeviceStore::new()
                    }
                }
            }
            Err(_) => DeviceStore::new(),
        };

        Self {
            inner: Arc::new(Mutex::new(WhatsAppClientInner {
                sender: None,
                noise: NoiseHandshake::new(),
                iq_tracker: Arc::new(Mutex::new(HashMap::new())),
                event_tx,
                msg_counter: 0,
            })),
            store: Arc::new(Mutex::new(store)),
            event_rx: Arc::new(Mutex::new(event_rx)),
            state: Arc::new(Mutex::new(ConnectionState::Disconnected)),
            db_path: db_path.to_string(),
        }
    }

    pub async fn next_event(&self) -> Option<WhatsAppEvent> {
        let mut rx = self.event_rx.lock().await;
        rx.recv().await
    }

    pub async fn connection_state(&self) -> ConnectionState {
        self.state.lock().await.clone()
    }

    /// Persist the device store to SQLite.
    pub async fn persist_store(&self) -> Result<()> {
        let store = self.store.lock().await;
        let conn = rusqlite::Connection::open(&self.db_path)?;
        store.save_to_db(&conn)?;
        tracing::debug!("Device store persisted to {}", self.db_path);
        Ok(())
    }
}

#[async_trait::async_trait]
impl WhatsAppClientPort for WhatsAppClient {
    async fn connect(&self) -> Result<()> {
        {
            let mut state = self.state.lock().await;
            if *state == ConnectionState::Connected {
                return Ok(());
            }
            *state = ConnectionState::Connecting;
        }

        let mut inner = self.inner.lock().await;
        match self.connect_internal(&mut inner).await {
            Ok(()) => {
                *self.state.lock().await = ConnectionState::Connected;
                // Persist store after successful connection (saves keys)
                drop(inner);
                let _ = self.persist_store().await;
                Ok(())
            }
            Err(e) => {
                *self.state.lock().await = ConnectionState::Disconnected;
                Err(e)
            }
        }
    }

    async fn disconnect(&self) -> Result<()> {
        let mut inner = self.inner.lock().await;
        inner.sender = None;
        *self.state.lock().await = ConnectionState::Disconnected;
        let _ = inner.event_tx.send(WhatsAppEvent::Disconnected);
        // Persist before disconnecting
        drop(inner);
        let _ = self.persist_store().await;
        Ok(())
    }

    async fn send_message(&self, chat_id: &ChatId, text: &str) -> Result<Message> {
        let jid = chat_id.0.clone();
        tracing::info!("Sending message to {}", jid);

        let user_part = jid.split('@').next().unwrap_or(&jid);

        // 1. Discover recipient devices via usync (falls back to device 0)
        let recipient_devices = self.discover_devices(user_part).await;
        tracing::info!("Recipient devices for {}: {:?}", user_part, recipient_devices);

        // 2. Discover our own devices for fanout (exclude self)
        let our_jid_full = self.store.lock().await.our_jid.clone().unwrap_or_default();
        let our_user = our_jid_full.split(':').next().unwrap_or("").split('@').next().unwrap_or("");
        let our_device_id: u16 = our_jid_full.split(':').nth(1)
            .and_then(|s| s.split('@').next())
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let own_devices = if !our_user.is_empty() {
            self.discover_devices(our_user).await
                .into_iter()
                .filter(|&d| d != our_device_id)
                .collect::<Vec<_>>()
        } else {
            vec![]
        };
        tracing::info!("Own devices for fanout (excluding {}): {:?}", our_device_id, own_devices);

        // 3. Generate message ID
        let msg_id = {
            use sha2::{Sha256, Digest};
            let now = chrono::Utc::now().timestamp() as u64;
            let random_bytes: [u8; 16] = rand::random();
            let mut hasher = Sha256::new();
            hasher.update(now.to_be_bytes());
            hasher.update(our_jid_full.as_bytes());
            hasher.update(&random_bytes);
            let hash = hasher.finalize();
            format!("3EB0{}", hex::encode(&hash[..9]).to_uppercase())
        };
        let timestamp = chrono::Utc::now().timestamp();

        // 4. Build E2E Message protobuf + pad
        use crate::proto::wa_web_protobufs_e2e as e2e;
        let e2e_msg = e2e::Message {
            conversation: Some(text.to_string()),
            ..Default::default()
        };
        let mut e2e_bytes = Vec::new();
        e2e_msg.encode(&mut e2e_bytes)?;

        let padded = Self::pad_message(&e2e_bytes);

        // 5. Encrypt for each recipient device → build <to> nodes
        let mut to_nodes = Vec::new();
        let mut any_pkmsg = false;
        let mut sessions_to_persist: Vec<(String, Vec<u8>)> = Vec::new();

        for device_id in &recipient_devices {
            let device_jid = format!("{}:{}@s.whatsapp.net", user_part, device_id);
            match self.encrypt_for_device(&device_jid, &padded).await {
                Ok((enc_type, envelope, session)) => {
                    if enc_type == "pkmsg" { any_pkmsg = true; }

                    let mut enc_attrs = HashMap::new();
                    enc_attrs.insert("v".to_string(), AttrValue::String("2".to_string()));
                    enc_attrs.insert("type".to_string(), AttrValue::String(enc_type));
                    let enc_node = Node::new("enc", enc_attrs, Content::Bytes(envelope));

                    let mut to_attrs = HashMap::new();
                    to_attrs.insert("jid".to_string(), AttrValue::String(device_jid.clone()));
                    let to_node = Node::new("to", to_attrs, Content::Nodes(vec![enc_node]));
                    to_nodes.push(to_node);

                    let session_bytes = serde_json::to_vec(&session)?;
                    sessions_to_persist.push((device_jid, session_bytes));
                }
                Err(e) => {
                    tracing::warn!("Failed to encrypt for device {}: {}", device_jid, e);
                }
            }
        }

        if to_nodes.is_empty() {
            return Err(anyhow!("Failed to encrypt for any recipient device"));
        }

        // 6. Encrypt for own devices (fanout with DeviceSentMessage wrapper)
        if !own_devices.is_empty() {
            let dsm = e2e::Message {
                device_sent_message: Some(Box::new(e2e::DeviceSentMessage {
                    destination_jid: Some(jid.clone()),
                    message: Some(Box::new(e2e::Message {
                        conversation: Some(text.to_string()),
                        ..Default::default()
                    })),
                    ..Default::default()
                })),
                ..Default::default()
            };
            let mut dsm_bytes = Vec::new();
            dsm.encode(&mut dsm_bytes)?;
            let dsm_padded = Self::pad_message(&dsm_bytes);

            for device_id in &own_devices {
                let device_jid = format!("{}:{}@s.whatsapp.net", our_user, device_id);
                match self.encrypt_for_device(&device_jid, &dsm_padded).await {
                    Ok((enc_type, envelope, session)) => {
                        if enc_type == "pkmsg" { any_pkmsg = true; }

                        let mut enc_attrs = HashMap::new();
                        enc_attrs.insert("v".to_string(), AttrValue::String("2".to_string()));
                        enc_attrs.insert("type".to_string(), AttrValue::String(enc_type));
                        let enc_node = Node::new("enc", enc_attrs, Content::Bytes(envelope));

                        let mut to_attrs = HashMap::new();
                        to_attrs.insert("jid".to_string(), AttrValue::String(device_jid.clone()));
                        let to_node = Node::new("to", to_attrs, Content::Nodes(vec![enc_node]));
                        to_nodes.push(to_node);

                        let session_bytes = serde_json::to_vec(&session)?;
                        sessions_to_persist.push((device_jid, session_bytes));
                    }
                    Err(e) => {
                        tracing::warn!("Failed to encrypt for own device {}: {}", device_jid, e);
                    }
                }
            }
        }

        // 7. Build message node
        let participants_node = Node::new("participants", HashMap::new(), Content::Nodes(to_nodes));
        let mut msg_children = vec![participants_node];

        if any_pkmsg {
            let store = self.store.lock().await;
            if let Some(ref account_bytes) = store.account_identity {
                msg_children.push(Node::new("device-identity", HashMap::new(), Content::Bytes(account_bytes.clone())));
                tracing::info!("Including device-identity node ({} bytes)", account_bytes.len());
            } else {
                tracing::warn!("No account_identity — device-identity node missing!");
            }
        }

        let mut msg_attrs = HashMap::new();
        msg_attrs.insert("id".to_string(), AttrValue::String(msg_id.clone()));
        msg_attrs.insert("to".to_string(), AttrValue::String(jid.clone()));
        msg_attrs.insert("type".to_string(), AttrValue::String("text".to_string()));
        let message_node = Node::new("message", msg_attrs, Content::Nodes(msg_children));

        self.send_node(&message_node).await?;

        // 8. Persist all sessions
        {
            let mut store = self.store.lock().await;
            for (jid, bytes) in sessions_to_persist {
                store.save_session(jid, bytes);
            }
        }
        let _ = self.persist_store().await;

        tracing::info!("Message {} sent to {} recipient device(s) + {} own device(s)",
            msg_id, recipient_devices.len(), own_devices.len());

        Ok(Message {
            id: MessageId(msg_id),
            chat_id: chat_id.clone(),
            sender_id: "me".to_string(),
            text: Some(text.to_string()),
            media: None,
            timestamp,
            is_from_me: true,
            is_forwarded: false,
            reply_to_id: None,
        })
    }

    async fn list_chats(&self) -> Result<Vec<Chat>> {
        let store = self.store.lock().await;
        Ok(store.chats.values().cloned().collect())
    }
}

// ─── Presence Subscription ──────────────────────────────────────────

impl WhatsAppClient {
    /// Subscribe to presence updates for a list of JIDs.
    /// The server will send `<presence>` stanzas when contacts go online/offline.
    pub async fn subscribe_presence(&self, jids: &[String]) -> Result<()> {
        for jid in jids {
            let full_jid = if jid.contains('@') {
                jid.clone()
            } else {
                format!("{}@s.whatsapp.net", jid)
            };

            let mut attrs = HashMap::new();
            attrs.insert("to".to_string(), AttrValue::String(full_jid.clone()));
            attrs.insert("type".to_string(), AttrValue::String("subscribe".to_string()));

            let presence_node = Node::new("presence", attrs, Content::None);
            self.send_node(&presence_node).await?;
            tracing::info!("Subscribed to presence for {}", full_jid);
        }
        Ok(())
    }

    /// Send our own presence as available (required before receiving presence updates).
    pub async fn send_available_presence(&self) -> Result<()> {
        let mut attrs = HashMap::new();
        attrs.insert("type".to_string(), AttrValue::String("available".to_string()));
        let node = Node::new("presence", attrs, Content::None);
        self.send_node(&node).await?;
        tracing::info!("Sent own presence: available");
        Ok(())
    }
}

// ─── Multi-Device Helpers ───────────────────────────────────────────

impl WhatsAppClient {
    /// Pad a plaintext message using WhatsApp's PKCS7-like padding (N copies of byte N, N=1..16).
    fn pad_message(plaintext: &[u8]) -> Vec<u8> {
        let pad_value = {
            let r = rand::random::<u8>() & 0x0F; // 0-15
            if r == 0 { 16u8 } else { r }
        };
        let mut padded = Vec::with_capacity(plaintext.len() + pad_value as usize);
        padded.extend_from_slice(plaintext);
        for _ in 0..pad_value {
            padded.push(pad_value);
        }
        padded
    }

    /// Discover device IDs for a user via usync. Falls back to [0] on failure.
    async fn discover_devices(&self, user: &str) -> Vec<u16> {
        let bare_jid = if user.contains('@') {
            user.to_string()
        } else {
            format!("{}@s.whatsapp.net", user)
        };

        match self.query_usync(vec![bare_jid.clone()]).await {
            Ok(response) => {
                let devices = Self::parse_usync_devices(&response);
                if devices.is_empty() {
                    tracing::warn!("usync returned no devices for {}, falling back to [0]", bare_jid);
                    vec![0]
                } else {
                    tracing::info!("usync discovered {} devices for {}: {:?}", devices.len(), bare_jid, devices);
                    devices
                }
            }
            Err(e) => {
                tracing::warn!("usync failed for {}: {}, falling back to [0]", bare_jid, e);
                vec![0]
            }
        }
    }

    /// Parse device IDs from a usync response node.
    /// Response structure: <usync><result><devices><device-list><device jid="user:N@s.whatsapp.net"/></device-list></devices></result></usync>
    fn parse_usync_devices(response: &Node) -> Vec<u16> {
        let mut devices = Vec::new();

        // Navigate: response (iq) → usync → result → devices → device-list → device nodes
        let usync_node = response.get_child_by_tag("usync")
            .or_else(|| Some(response)); // response might BE the usync node

        if let Some(usync) = usync_node {
            // Try: usync → result(s) → devices → device-list → device
            if let Some(results) = usync.get_child_by_tag("result")
                .or_else(|| usync.get_child_by_tag("results"))
            {
                Self::extract_devices_from_result(&results, &mut devices);
            }
            // Also try direct: usync → list → user → devices → device-list
            if let Some(list) = usync.get_child_by_tag("list") {
                if let Content::Nodes(users) = &list.content {
                    for user in users {
                        if user.tag == "user" {
                            Self::extract_devices_from_result(user, &mut devices);
                        }
                    }
                }
            }
        }

        // Deduplicate and sort
        devices.sort();
        devices.dedup();
        devices
    }

    fn extract_devices_from_result(node: &Node, devices: &mut Vec<u16>) {
        // Look for <devices> → <device-list> → <device .../>
        if let Some(devices_node) = node.get_child_by_tag("devices") {
            Self::extract_device_ids_from_node(devices_node, devices);
            // Also check <device-list> sub-node
            if let Some(device_list) = devices_node.get_child_by_tag("device-list") {
                Self::extract_device_ids_from_node(device_list, devices);
            }
        }
    }

    fn extract_device_ids_from_node(parent: &Node, devices: &mut Vec<u16>) {
        if let Content::Nodes(children) = &parent.content {
            for dev in children {
                if dev.tag == "device" {
                    // Strategy 1: <device id="N"/> (most common in usync responses)
                    if let Some(id_str) = dev.get_attr("id") {
                        if let Ok(id) = id_str.parse::<u16>() {
                            devices.push(id);
                            continue;
                        }
                    }
                    // Strategy 2: <device jid="user:N@s.whatsapp.net"/>
                    if let Some(jid_str) = dev.get_attr("jid") {
                        if let Some(device_part) = jid_str.split(':').nth(1) {
                            if let Ok(id) = device_part.split('@').next().unwrap_or("").parse::<u16>() {
                                devices.push(id);
                                continue;
                            }
                        }
                        // Bare JID with no device part = device 0
                        if !jid_str.contains(':') {
                            devices.push(0);
                        }
                    }
                }
            }
        }
    }

    /// Encrypt a padded plaintext for a specific device JID.
    /// Returns (enc_type, envelope_bytes, updated_session).
    async fn encrypt_for_device(&self, device_jid: &str, padded: &[u8]) -> Result<(String, Vec<u8>, Session)> {
        let mut session = self.get_or_create_session(device_jid).await?;

        let (ciphertext, ratchet_pub, counter, prev_counter, msg_keys) = session.encrypt(padded)?;

        // Build inner SignalMessage with 33-byte keys (0x05 prefix)
        let mut ratchet_key_33 = Vec::with_capacity(33);
        ratchet_key_33.push(0x05);
        ratchet_key_33.extend_from_slice(&ratchet_pub);

        let signal_msg = crate::proto::signal::SignalMessage {
            ratcheting_key: Some(ratchet_key_33),
            counter: Some(counter),
            previous_counter: Some(prev_counter),
            ciphertext: Some(ciphertext),
        };
        let mut signal_proto_bytes = Vec::new();
        signal_msg.encode(&mut signal_proto_bytes)?;

        // Compute MAC
        let version_byte: u8 = 0x33;
        let (our_identity_33, remote_identity_33, registration_id) = {
            let store = self.store.lock().await;
            let mut our_id = Vec::with_capacity(33);
            our_id.push(0x05);
            our_id.extend_from_slice(&store.identity_key_pub);
            let mut remote_id = Vec::with_capacity(33);
            remote_id.push(0x05);
            remote_id.extend_from_slice(&session.remote_identity_key);
            (our_id, remote_id, store.registration_id)
        };

        let mut mac_data = Vec::new();
        mac_data.extend_from_slice(&our_identity_33);
        mac_data.extend_from_slice(&remote_identity_33);
        mac_data.push(version_byte);
        mac_data.extend_from_slice(&signal_proto_bytes);
        let mac = crate::crypto::compute_mac(&msg_keys.mac_key, &mac_data);

        // SignalMessage envelope: version || proto || mac[0:8]
        let mut inner_signal = Vec::with_capacity(1 + signal_proto_bytes.len() + 8);
        inner_signal.push(version_byte);
        inner_signal.extend_from_slice(&signal_proto_bytes);
        inner_signal.extend_from_slice(&mac[..8]);

        // Wrap in PreKeySignalMessage if needed
        let (enc_type, envelope) = if let Some(ref pk) = session.pending_prekey {
            let mut base_key_33 = Vec::with_capacity(33);
            base_key_33.push(0x05);
            base_key_33.extend_from_slice(&pk.base_key);

            let pkmsg = crate::proto::signal::PreKeySignalMessage {
                registration_id: Some(registration_id),
                pre_key_id: Some(pk.prekey_id),
                signed_pre_key_id: Some(pk.signed_prekey_id),
                base_key: Some(base_key_33),
                identity_key: Some(our_identity_33),
                message: Some(inner_signal),
            };
            let mut pkmsg_bytes = Vec::new();
            pkmsg.encode(&mut pkmsg_bytes)?;

            let mut env = Vec::with_capacity(1 + pkmsg_bytes.len());
            env.push(version_byte);
            env.extend_from_slice(&pkmsg_bytes);
            ("pkmsg".to_string(), env)
        } else {
            ("msg".to_string(), inner_signal)
        };

        tracing::info!("Encrypted for {}: type={}, len={}", device_jid, enc_type, envelope.len());
        Ok((enc_type, envelope, session))
    }
}

// ─── Internal Protocol Logic ────────────────────────────────────────

impl WhatsAppClient {
    async fn connect_internal(&self, inner: &mut WhatsAppClientInner) -> Result<()> {
        // Reset noise state for a fresh handshake
        inner.noise = NoiseHandshake::new();
        inner.msg_counter = 0;

        let mut fs = FrameSocket::connect(WA_CONN_HEADER).await?;

        let ephemeral_priv = StaticSecret::random_from_rng(rand::thread_rng());
        let ephemeral_pub = PublicKey::from(&ephemeral_priv);

        inner.noise.start(NOISE_MODE, WA_CONN_HEADER)?;

        // Connection header is automatically sent by FrameSocket on the first frame

        let client_hello = HandshakeMessage {
            client_hello: Some(ClientHello {
                ephemeral: Some(ephemeral_pub.as_bytes().to_vec()),
                ..Default::default()
            }),
            ..Default::default()
        };

        let hello_bytes = client_hello.encode_to_vec();
        fs.send_frame(&hello_bytes).await?;

        inner.noise.authenticate(ephemeral_pub.as_bytes());

        let resp_bytes = timeout(Duration::from_secs(10), fs.receive_frame()).await
            .context("Handshake timeout")??;

        let handshake_resp = HandshakeMessage::decode(&resp_bytes[..])?;
        let server_hello = handshake_resp.server_hello.ok_or_else(|| anyhow!("Missing server hello"))?;

        let server_ephemeral = server_hello.ephemeral.ok_or_else(|| anyhow!("Missing server ephemeral"))?;
        let server_static_ciphertext = server_hello.r#static.ok_or_else(|| anyhow!("Missing server static"))?;
        let cert_ciphertext = server_hello.payload.ok_or_else(|| anyhow!("Missing certificate payload"))?;

        let mut server_ephemeral_arr = [0u8; 32];
        server_ephemeral_arr.copy_from_slice(&server_ephemeral);

        inner.noise.authenticate(&server_ephemeral);
        inner.noise.mix_shared_secret_into_key(ephemeral_priv.to_bytes().as_ref().try_into()?, &server_ephemeral_arr)?;

        let static_decrypted = inner.noise.decrypt(&server_static_ciphertext)?;
        let mut static_decrypted_arr = [0u8; 32];
        static_decrypted_arr.copy_from_slice(&static_decrypted);

        inner.noise.mix_shared_secret_into_key(ephemeral_priv.to_bytes().as_ref().try_into()?, &static_decrypted_arr)?;

        let _cert_decrypted = inner.noise.decrypt(&cert_ciphertext)?;

        // ── Build ClientPayload ─────────────────────────────────────
        let store = self.store.lock().await;
        let is_new_login = store.our_jid.is_none();
        drop(store);

        // NOTE: QR codes are NOT generated here.
        // The server sends `pair-device` IQs with ref codes AFTER the handshake.
        // The read_loop handler generates QR codes from those server-provided refs.
        if is_new_login {
            *self.state.lock().await = ConnectionState::WaitingForQr;
        }

        let (pub_key_bytes, priv_key_bytes) = {
            let store = self.store.lock().await;
            (store.noise_key.pub_key, store.noise_key.priv_key)
        };
        let encrypted_pubkey = inner.noise.encrypt(&pub_key_bytes)?;
        inner.noise.mix_shared_secret_into_key(&priv_key_bytes, &server_ephemeral_arr)?;

        // Version must match whatsmeow: {2, 3000, 1035920091}
        let mut client_payload = ClientPayload {
            username: None, // Only set for login (not registration) — whatsmeow: getLoginPayload only
            passive: Some(false),
            pull: Some(false),
            shards: vec![], // whatsmeow never sets shards
            user_agent: Some(crate::proto::wa_web_protobufs_wa6::client_payload::UserAgent {
                platform: Some(crate::proto::wa_web_protobufs_wa6::client_payload::user_agent::Platform::Web.into()),
                release_channel: Some(crate::proto::wa_web_protobufs_wa6::client_payload::user_agent::ReleaseChannel::Release.into()),
                app_version: Some(crate::proto::wa_web_protobufs_wa6::client_payload::user_agent::AppVersion {
                    primary: Some(2),
                    secondary: Some(3000),
                    tertiary: Some(1035920091),
                    ..Default::default()
                }),
                mcc: Some("000".to_string()),
                mnc: Some("000".to_string()),
                os_version: Some("0.1".to_string()),
                manufacturer: Some(String::new()),
                device: Some("Desktop".to_string()),
                os_build_number: Some("0.1".to_string()),
                locale_language_iso6391: Some("en".to_string()),
                locale_country_iso31661_alpha2: Some("US".to_string()),
                ..Default::default()
            }),
            web_info: Some(crate::proto::wa_web_protobufs_wa6::client_payload::WebInfo {
                web_sub_platform: Some(crate::proto::wa_web_protobufs_wa6::client_payload::web_info::WebSubPlatform::WebBrowser.into()),
                ..Default::default()
            }),
            connect_type: Some(crate::proto::wa_web_protobufs_wa6::client_payload::ConnectType::WifiUnknown.into()),
            connect_reason: Some(crate::proto::wa_web_protobufs_wa6::client_payload::ConnectReason::UserActivated.into()),
            ..Default::default()
        };

        if is_new_login {
            let store = self.store.lock().await;
            let mut reg_id_bytes = [0u8; 4];
            reg_id_bytes.copy_from_slice(&store.registration_id.to_be_bytes());

            let mut skey_id_bytes = [0u8; 4];
            skey_id_bytes.copy_from_slice(&store.signed_prekey.key_id.to_be_bytes());

            let device_props = crate::proto::wa_companion_reg::DeviceProps {
                os: Some("whatsmeow".to_string()),
                version: Some(crate::proto::wa_companion_reg::device_props::AppVersion {
                    primary: Some(0),
                    secondary: Some(1),
                    tertiary: Some(0),
                    ..Default::default()
                }),
                platform_type: Some(crate::proto::wa_companion_reg::device_props::PlatformType::Unknown.into()),
                require_full_sync: Some(false),
                history_sync_config: Some(crate::proto::wa_companion_reg::device_props::HistorySyncConfig {
                    full_sync_days_limit: None,
                    full_sync_size_mb_limit: None,
                    storage_quota_mb: Some(10240),
                    inline_initial_payload_in_e2_ee_msg: Some(true),
                    recent_sync_days_limit: None,
                    support_call_log_history: Some(false),
                    support_bot_user_agent_chat_history: Some(true),
                    support_cag_reactions_and_polls: Some(true),
                    support_biz_hosted_msg: Some(true),
                    support_recent_sync_chunk_message_count_tuning: Some(true),
                    support_hosted_group_msg: Some(true),
                    support_fbid_bot_chat_history: Some(true),
                    support_add_on_history_sync_migration: None,
                    support_message_association: Some(true),
                    support_group_history: Some(true),
                    on_demand_ready: None,
                    support_guest_chat: None,
                    complete_on_demand_ready: None,
                    thumbnail_sync_days_limit: Some(60),
                    initial_sync_max_messages_per_chat: None,
                    support_manus_history: Some(true),
                    support_hatch_history: Some(true),
                }),
                ..Default::default()
            };

            use prost::Message;
            let device_props_bytes = device_props.encode_to_vec();

            // BuildHash = md5("2.3000.1035920091")
            client_payload.device_pairing_data = Some(crate::proto::wa_web_protobufs_wa6::client_payload::DevicePairingRegistrationData {
                e_regid: Some(reg_id_bytes.to_vec()),
                e_keytype: Some(vec![5]),
                e_ident: Some(store.identity_key_pub.to_vec()),
                e_skey_id: Some(skey_id_bytes[1..].to_vec()),
                e_skey_val: Some(store.signed_prekey.pub_key.to_vec()),
                e_skey_sig: Some(store.signed_prekey.signature.clone()),
                build_hash: Some(vec![211, 73, 16, 53, 118, 193, 129, 58, 170, 79, 121, 172, 64, 243, 83, 192]),
                device_props: Some(device_props_bytes),
                ..Default::default()
            });

        } else {
            // Login path — reconnect with saved credentials (whatsmeow: getLoginPayload)
            let store = self.store.lock().await;
            if let Some(ref jid_str) = store.our_jid {
                // Parse JID: "393793667569:3@s.whatsapp.net" → user=393793667569, device=3
                let jid_without_server = jid_str.split('@').next().unwrap_or("");
                let parts: Vec<&str> = jid_without_server.split(':').collect();
                if let Some(user_str) = parts.first() {
                    if let Ok(user_id) = user_str.parse::<u64>() {
                        client_payload.username = Some(user_id);
                    }
                }
                if let Some(device_str) = parts.get(1) {
                    if let Ok(device_id) = device_str.parse::<u32>() {
                        client_payload.device = Some(device_id);
                    }
                }
            }
            client_payload.passive = Some(true);
            client_payload.pull = Some(true);
            client_payload.lid_db_migrated = Some(true);
            if client_payload.lc.is_none() {
                client_payload.lc = Some(1);
            }
            tracing::info!("Login payload: username={:?}, device={:?}, passive=true, pull=true",
                client_payload.username, client_payload.device);
        }

        use prost::Message;
        let payload_bytes = client_payload.encode_to_vec();
        let encrypted_payload = inner.noise.encrypt(&payload_bytes)?;

        let client_finish = HandshakeMessage {
            client_finish: Some(ClientFinish {
                r#static: Some(encrypted_pubkey),
                payload: Some(encrypted_payload),
                ..Default::default()
            }),
            ..Default::default()
        };

        let finish_bytes = client_finish.encode_to_vec();
        fs.send_frame(&finish_bytes).await?;

        // Split socket after handshake
        let (write_cipher, read_cipher) = inner.noise.finish()?;
        let (tx, rx) = fs.split()?;

        let sender = NoiseSender { tx, write_cipher, write_counter: 0 };
        let receiver = NoiseReceiver::new(rx, read_cipher);

        inner.sender = Some(sender);

        let client_clone = self.clone();
        tokio::spawn(async move {
            client_clone.read_loop(receiver).await;
        });

        Ok(())
    }

    /// Handle the pair-success IQ from the server.
    ///
    /// This implements the whatsmeow `handlePairSuccess` + `handlePair` flow:
    /// 1. Verify HMAC on the device identity container
    /// 2. Verify the account signature
    /// 3. Generate the device signature
    /// 4. Send the pairing confirmation IQ
    /// 5. Save the JID to the store
    async fn handle_pair_success(
        di_bytes: &[u8],
        iq_id: &str,
        jid: &str,
        store: &Arc<Mutex<DeviceStore>>,
        event_tx: &tokio::sync::mpsc::UnboundedSender<WhatsAppEvent>,
        client: &WhatsAppClient,
    ) -> Result<()> {
        use crate::proto::wa_adv::{AdvSignedDeviceIdentityHmac, AdvSignedDeviceIdentity, AdvDeviceIdentity};
        use hmac::{Hmac, Mac};
        use sha2::Sha256;

        // 1. Parse the HMAC container
        let container = AdvSignedDeviceIdentityHmac::decode(di_bytes)
            .map_err(|e| anyhow!("Failed to parse ADVSignedDeviceIdentityHMAC: {}", e))?;
        let details_raw = container.details.ok_or_else(|| anyhow!("Missing details in HMAC container"))?;
        let hmac_value = container.hmac.ok_or_else(|| anyhow!("Missing HMAC in container"))?;

        // 2. Verify HMAC using the ADV secret key
        let adv_secret = {
            let s = store.lock().await;
            s.adv_secret.0
        };

        type HmacSha256 = Hmac<Sha256>;
        let mut mac = HmacSha256::new_from_slice(&adv_secret)
            .map_err(|e| anyhow!("HMAC init failed: {}", e))?;
        mac.update(&details_raw);
        mac.verify_slice(&hmac_value)
            .map_err(|_| anyhow!("HMAC verification failed — adv_secret mismatch"))?;

        tracing::info!("HMAC verification passed");

        // 3. Parse the signed device identity
        let mut device_identity = AdvSignedDeviceIdentity::decode(&details_raw[..])
            .map_err(|e| anyhow!("Failed to parse ADVSignedDeviceIdentity: {}", e))?;
        let identity_details_raw = device_identity.details.as_ref()
            .ok_or_else(|| anyhow!("Missing details in signed identity"))?
            .clone();

        // 4. Parse device identity details
        let device_identity_details = AdvDeviceIdentity::decode(&identity_details_raw[..])
            .map_err(|e| anyhow!("Failed to parse ADVDeviceIdentity: {}", e))?;

        tracing::info!("Device identity parsed: key_index={:?}", device_identity_details.key_index);

        // 5. Verify account signature
        let account_sig_key = device_identity.account_signature_key.as_ref()
            .ok_or_else(|| anyhow!("Missing account signature key"))?;
        let account_sig = device_identity.account_signature.as_ref()
            .ok_or_else(|| anyhow!("Missing account signature"))?;

        if account_sig_key.len() != 32 || account_sig.len() != 64 {
            return Err(anyhow!("Invalid signature key/signature length"));
        }

        let identity_key_pub = {
            let s = store.lock().await;
            s.identity_key_pub
        };

        // Verify: sign(accountSignatureKey, [6,0] + details + identityKeyPub)
        // accountSignatureKey is a Curve25519 key — must use XEdDSA (not plain ed25519)
        let mut verify_msg = vec![6u8, 0u8];
        verify_msg.extend_from_slice(&identity_details_raw);
        verify_msg.extend_from_slice(&identity_key_pub);

        let sig_key_arr: [u8; 32] = account_sig_key[..32].try_into()?;
        let sig_arr: [u8; 64] = account_sig[..64].try_into()?;

        if !crate::crypto::xeddsa::xeddsa_verify(&sig_key_arr, &verify_msg, &sig_arr) {
            return Err(anyhow!("Account signature verification failed (XEdDSA)"));
        }

        tracing::info!("Account signature verified");

        // 6. Generate device signature
        let identity_key_priv = {
            let s = store.lock().await;
            s.identity_key_priv
        };

        let mut sign_msg = vec![6u8, 1u8];
        sign_msg.extend_from_slice(&identity_details_raw);
        sign_msg.extend_from_slice(&identity_key_pub);
        sign_msg.extend_from_slice(account_sig_key);

        // Sign using XEdDSA (NOT plain ed25519) — identity_key_priv is a Curve25519 key
        let device_sig = crate::crypto::xeddsa::xeddsa_sign(&identity_key_priv, &sign_msg);
        device_identity.device_signature = Some(device_sig.to_vec());

        // 7. Strip account_signature_key before sending back
        let account_sig_key_copy = device_identity.account_signature_key.take();
        let self_signed_identity = device_identity.encode_to_vec();
        device_identity.account_signature_key = account_sig_key_copy;

        // 8. Save JID + account identity + emit Connected event
        {
            let mut s = store.lock().await;
            s.our_jid = Some(jid.to_string());
            // Store the full AdvSignedDeviceIdentity for device-identity node in pkmsg sends
            s.account_identity = Some(device_identity.encode_to_vec());
        }

        // 9. Persist updated store (including account_identity) to SQLite
        Self::persist_store_static(&store, &client.db_path).await;

        // 10. Send pair-device-sign confirmation IQ
        let key_index_val = device_identity_details.key_index.unwrap_or(0);
        let confirm_node = Node::new(
            "iq",
            IntoIterator::into_iter([
                ("to".to_string(), AttrValue::String("s.whatsapp.net".to_string())),
                ("type".to_string(), AttrValue::String("result".to_string())),
                ("id".to_string(), AttrValue::String(iq_id.to_string())),
            ]).collect(),
            Content::Nodes(vec![
                Node::new("pair-device-sign", std::collections::HashMap::new(),
                    Content::Nodes(vec![
                        Node::new("device-identity", IntoIterator::into_iter([
                            ("key-index".to_string(), AttrValue::String(key_index_val.to_string())),
                        ]).collect(), Content::Bytes(self_signed_identity)),
                    ]),
                ),
            ]),
        );

        client.send_node(&confirm_node).await?;
        tracing::info!("Sent pair-device-sign confirmation IQ");

        // Emit pair-success event (NOT Connected — must reconnect with login payload first)
        let _ = event_tx.send(WhatsAppEvent::PairSuccess { jid: jid.to_string() });

        Ok(())
    }

    async fn read_loop(self, mut receiver: NoiseReceiver) {
        let iq_tracker = self.inner.lock().await.iq_tracker.clone();
        let event_tx = self.inner.lock().await.event_tx.clone();
        let store = self.store.clone();
        let state = self.state.clone();
        let db_path = self.db_path.clone();

        loop {
            match receiver.receive_encrypted_frame().await {
                Ok(bytes) => {
                    tracing::info!("read_loop: received {} decrypted bytes, flag=0x{:02x}", bytes.len(), bytes.get(0).copied().unwrap_or(0));
                    if bytes.is_empty() {
                        continue;
                    }
                    let flag = bytes[0];
                    let payload = &bytes[1..];
                    let unpacked = if flag & 2 != 0 {
                        // zlib decompress
                        use std::io::Read;
                        match flate2::read::ZlibDecoder::new(payload).bytes().collect::<Result<Vec<u8>, _>>() {
                            Ok(decompressed) => {
                                tracing::info!("read_loop: decompressed {} -> {} bytes", payload.len(), decompressed.len());
                                decompressed
                            }
                            Err(e) => {
                                tracing::warn!("read_loop: zlib decompression failed: {}", e);
                                continue;
                            }
                        }
                    } else {
                        payload.to_vec()
                    };

                    let mut decoder = Decoder::new(&unpacked);
                    tracing::info!("read_loop: unpacked first bytes: {:02x?}", &unpacked[..std::cmp::min(20, unpacked.len())]);
                    match decoder.read_node() {
                        Ok(node) => {
                            tracing::info!("Received node: tag={}, attrs={:?}", node.tag, node.attrs.keys().collect::<Vec<_>>());

                            match node.tag.as_str() {
                                "iq" => {
                                    // Check for pair-device / pair-success IQs first
                                    let from_jid = node.attrs.get("from").map(|val| {
                                        match val {
                                            AttrValue::String(s) => s.clone(),
                                            AttrValue::Jid(j) => j.to_string(),
                                            _ => String::new(),
                                        }
                                    }).unwrap_or_else(|| String::new());
                                    let from = from_jid.as_str();

                                    let iq_type = node.get_attr("type").unwrap_or("");
                                    let first_child_tag = match &node.content {
                                        Content::Nodes(children) if !children.is_empty() => {
                                            children[0].tag.as_str().to_string()
                                        }
                                        _ => {
                                            tracing::debug!("IQ content was not nodes: {:?}", node.content);
                                            String::new()
                                        }
                                    };
                                    tracing::info!("IQ received: from={}, type={}, first_child={}", from, iq_type, first_child_tag);

                                    if from == "s.whatsapp.net" && first_child_tag == "pair-device" {
                                        tracing::info!("Received pair-device IQ");
                                        let iq_id = node.get_attr("id").unwrap_or("").to_string();

                                        let ack_node = Node::new(
                                            "iq",
                                            IntoIterator::into_iter([
                                                ("to".to_string(), AttrValue::String("s.whatsapp.net".to_string())),
                                                ("type".to_string(), AttrValue::String("result".to_string())),
                                                ("id".to_string(), AttrValue::String(iq_id.clone())),
                                            ]).collect(),
                                            Content::None,
                                        );
                                        
                                        // Send ack directly using self!
                                        if let Err(e) = self.send_node(&ack_node).await {
                                            tracing::error!("Failed to send pair-device ack: {}", e);
                                        } else {
                                            tracing::info!("Sent ACK for pair-device IQ {}", iq_id);
                                        }

                                        if let Content::Nodes(children) = &node.content {
                                            if let Some(pair_device_node) = children.iter().find(|c| c.tag == "pair-device") {
                                                let s = store.lock().await;
                                                for ref_child in pair_device_node.get_children_by_tag("ref") {
                                                    if let Content::Bytes(ref_bytes) = &ref_child.content {
                                                        let ref_str = String::from_utf8_lossy(ref_bytes).to_string();
                                                        let engine = base64::engine::general_purpose::STANDARD;
                                                        use base64::Engine;
                                                        let qr_data = format!(
                                                            "{},{},{},{}",
                                                            ref_str,
                                                            engine.encode(&s.noise_key.pub_key),
                                                            engine.encode(&s.identity_key_pub),
                                                            engine.encode(&s.adv_secret.0),
                                                        );
                                                        tracing::info!("QR ref received from server: {}", ref_str);
                                                        let _ = event_tx.send(WhatsAppEvent::QrCode(qr_data));
                                                        break; // Display only first QR
                                                    }
                                                }
                                            }
                                        }
                                    } else if from == "s.whatsapp.net" && first_child_tag == "pair-success" {
                                        tracing::info!("Received pair-success IQ, processing...");
                                        let iq_id = node.get_attr("id").unwrap_or("").to_string();

                                        // Extract pair-success child nodes
                                        if let Content::Nodes(children) = &node.content {
                                            if let Some(pair_success_node) = children.iter().find(|c| c.tag == "pair-success") {
                                                let device_identity_bytes = pair_success_node.get_child_by_tag("device-identity")
                                                    .and_then(|n| match &n.content { Content::Bytes(b) => Some(b.clone()), _ => None });
                                                // Device JID can be AttrValue::Jid or AttrValue::String
                                                let jid_str = pair_success_node.get_child_by_tag("device")
                                                    .and_then(|n| {
                                                        n.attrs.get("jid").map(|val| match val {
                                                            AttrValue::String(s) => s.clone(),
                                                            AttrValue::Jid(j) => j.to_string(),
                                                            _ => String::new(),
                                                        })
                                                    })
                                                    .filter(|s| !s.is_empty());

                                                if let (Some(di_bytes), Some(jid)) = (device_identity_bytes, jid_str) {
                                                    match Self::handle_pair_success(
                                                        &di_bytes, &iq_id, &jid, &store, &event_tx, &self
                                                    ).await {
                                                        Ok(()) => {
                                                            tracing::info!("✅ Pairing completed successfully as {}", jid);
                                                        }
                                                        Err(e) => {
                                                            tracing::error!("Failed to handle pair-success: {}", e);
                                                            // Send error IQ
                                                            let err_node = Node::new(
                                                                "iq",
                                                                IntoIterator::into_iter([
                                                                    ("to".to_string(), AttrValue::String("s.whatsapp.net".to_string())),
                                                                    ("type".to_string(), AttrValue::String("error".to_string())),
                                                                    ("id".to_string(), AttrValue::String(iq_id.clone())),
                                                                ]).collect(),
                                                                Content::Nodes(vec![
                                                                    Node::new("error", IntoIterator::into_iter([
                                                                        ("code".to_string(), AttrValue::String("500".to_string())),
                                                                        ("text".to_string(), AttrValue::String("internal-error".to_string())),
                                                                    ]).collect(), Content::None),
                                                                ]),
                                                            );
                                                            let _ = self.send_node(&err_node).await;
                                                        }
                                                    }
                                                } else {
                                                    // Detailed logging for debugging
                                                    let has_di = pair_success_node.get_child_by_tag("device-identity").is_some();
                                                    let has_dev = pair_success_node.get_child_by_tag("device").is_some();
                                                    let di_content = pair_success_node.get_child_by_tag("device-identity")
                                                        .map(|n| format!("{:?}", std::mem::discriminant(&n.content)));
                                                    let dev_attrs = pair_success_node.get_child_by_tag("device")
                                                        .map(|n| format!("{:?}", n.attrs.keys().collect::<Vec<_>>()));
                                                    tracing::error!(
                                                        "pair-success missing data: di_node={}, di_content={:?}, dev_node={}, dev_attrs={:?}",
                                                        has_di, di_content, has_dev, dev_attrs
                                                    );
                                                }
                                            }
                                        }
                                    }

                                    // Respond to unhandled server IQs (type=get/set) to prevent timeouts
                                    if (iq_type == "get" || iq_type == "set") 
                                        && first_child_tag != "pair-device" 
                                        && first_child_tag != "pair-success" 
                                    {
                                        let iq_id_resp = node.get_attr("id").unwrap_or("").to_string();
                                        if !iq_id_resp.is_empty() {
                                            let ack = Node::new(
                                                "iq",
                                                IntoIterator::into_iter([
                                                    ("to".to_string(), AttrValue::String("s.whatsapp.net".to_string())),
                                                    ("type".to_string(), AttrValue::String("result".to_string())),
                                                    ("id".to_string(), AttrValue::String(iq_id_resp.clone())),
                                                ]).collect(),
                                                Content::None,
                                            );
                                            if let Err(e) = self.send_node(&ack).await {
                                                tracing::warn!("Failed to ACK IQ {}: {}", iq_id_resp, e);
                                            } else {
                                                tracing::debug!("Auto-ACKed IQ {} (type={})", iq_id_resp, iq_type);
                                            }
                                        }
                                    }

                                    if let Some(id) = node.get_attr("id") {
                                        let mut tracker = iq_tracker.lock().await;
                                        if let Some(tx) = tracker.remove(id) {
                                            let _ = tx.send(node);
                                        }
                                    }
                                }
                                "message" => {
                                    let id = node.get_attr("id").unwrap_or("unknown").to_string();
                                    let from = node.get_attr("from").unwrap_or("unknown").to_string();
                                    let participant = node.get_attr("participant").map(|s| s.to_string());
                                    let sender_jid = participant.as_deref().unwrap_or(&from);

                                    let text = if let Some(enc_node) = node.get_child_by_tag("enc") {
                                        let enc_type = enc_node.get_attr("type").unwrap_or("");
                                        match &enc_node.content {
                                            Content::Bytes(cipher_bytes) => {
                                                match Self::decrypt_message(cipher_bytes, enc_type, sender_jid, &from, &store).await {
                                                    Ok(plaintext) => {
                                                        use crate::proto::wa_web_protobufs_e2e as e2e;
                                                        if let Ok(e2e_msg) = prost::Message::decode(&plaintext[..]) {
                                                            let e2e_msg: e2e::Message = e2e_msg;
                                                            e2e_msg.conversation.or_else(|| {
                                                                e2e_msg.extended_text_message.and_then(|m| m.text)
                                                            })
                                                        } else {
                                                            String::from_utf8(plaintext).ok()
                                                        }
                                                    }
                                                    Err(e) => {
                                                        tracing::warn!("Failed to decrypt message {}: {}", id, e);
                                                        None
                                                    }
                                                }
                                            }
                                            _ => None,
                                        }
                                    } else {
                                        if let Content::Nodes(children) = &node.content {
                                            children.iter().find(|c| c.tag == "body")
                                                .and_then(|c| if let Content::Bytes(b) = &c.content {
                                                    String::from_utf8(b.clone()).ok()
                                                } else {
                                                    None
                                                })
                                        } else {
                                            None
                                        }
                                    };

                                    let msg = Message {
                                        id: MessageId(id),
                                        chat_id: ChatId(from.clone()),
                                        sender_id: sender_jid.to_string(),
                                        text,
                                        media: None,
                                        timestamp: chrono::Utc::now().timestamp(),
                                        is_from_me: false,
                                        is_forwarded: false,
                                        reply_to_id: None,
                                    };
                                    let _ = event_tx.send(WhatsAppEvent::MessageReceived(msg));
                                }
                                "receipt" => {
                                    let id = node.get_attr("id").unwrap_or("").to_string();
                                    let from = node.get_attr("from").unwrap_or("").to_string();
                                    let _ = event_tx.send(WhatsAppEvent::ReceiptReceived {
                                        id,
                                        from,
                                        timestamp: chrono::Utc::now().timestamp(),
                                    });
                                }
                                "success" => {
                                    tracing::info!("Login successful");
                                    // Use JID from success node attrs, or fall back to stored JID
                                    let jid = node.get_attr("jid")
                                        .map(|s| s.to_string())
                                        .or_else(|| {
                                            // During login reconnect, JID is already in store
                                            let store_guard = store.try_lock();
                                            match store_guard {
                                                Ok(s) => s.our_jid.clone(),
                                                Err(_) => None,
                                            }
                                        });
                                    if let Some(jid) = jid {
                                        {
                                            if let Ok(mut s) = store.try_lock() {
                                                if s.our_jid.is_none() {
                                                    s.our_jid = Some(jid.clone());
                                                }
                                            }
                                        }
                                        Self::persist_store_static(&store, &db_path).await;
                                        let _ = event_tx.send(WhatsAppEvent::Connected { jid });
                                    } else {
                                        tracing::warn!("Login success but no JID available");
                                        // Still emit Connected with empty JID so the caller unblocks
                                        let _ = event_tx.send(WhatsAppEvent::Connected { jid: String::new() });
                                    }
                                }
                                "failure" => {
                                    let reason = node.get_attr("reason").unwrap_or("unknown");
                                    tracing::error!("Login failed: {}", reason);
                                }
                                "stream:error" => {
                                    // Log all available details from the stream:error node
                                    let err_attrs: Vec<_> = node.attrs.iter()
                                        .map(|(k, v)| format!("{}={:?}", k, v))
                                        .collect();
                                    let err_children = match &node.content {
                                        Content::Nodes(children) => children.iter()
                                            .map(|c| format!("<{} {:?}>", c.tag, c.attrs))
                                            .collect::<Vec<_>>().join(", "),
                                        Content::Bytes(b) => format!("bytes[{}]: {:02x?}", b.len(), &b[..std::cmp::min(64, b.len())]),
                                        Content::None => "(empty)".to_string(),
                                    };
                                    tracing::error!("Stream error received: attrs=[{}], content=[{}]",
                                        err_attrs.join(", "), err_children);
                                    *state.lock().await = ConnectionState::Disconnected;
                                    let _ = event_tx.send(WhatsAppEvent::Disconnected);
                                    break;
                                }
                                "notification" => {
                                    let notif_type = node.get_attr("type").unwrap_or("");
                                    tracing::debug!("Notification: type={}", notif_type);
                                    if notif_type == "encrypt" || notif_type == "w:gp2" || notif_type == "server_sync" {
                                        Self::process_notification(&node, &store, &event_tx).await;
                                    }
                                }
                                "presence" => {
                                    let from = node.get_attr("from").unwrap_or("").to_string();
                                    let ptype = node.get_attr("type").unwrap_or("available");
                                    let available = ptype != "unavailable";
                                    let last_seen = node.get_attr("last")
                                        .and_then(|v| v.parse::<i64>().ok());
                                    tracing::info!("Presence: {} is {} (last_seen: {:?})", from, ptype, last_seen);
                                    let _ = event_tx.send(WhatsAppEvent::PresenceUpdate {
                                        jid: from,
                                        available,
                                        last_seen,
                                    });
                                }
                                _ => {
                                    tracing::debug!("Received unhandled node: {}", node.tag);
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!("Failed to read node: {}", e);
                        }
                    }
                }
                Err(e) => {
                    tracing::error!("Read loop terminated: {}", e);
                    *state.lock().await = ConnectionState::Disconnected;
                    let _ = event_tx.send(WhatsAppEvent::Disconnected);
                    break;
                }
            }
        }
    }

    /// Decrypt a Signal-encrypted message payload.
    async fn decrypt_message(
        cipher_bytes: &[u8],
        enc_type: &str,
        sender_jid: &str,
        chat_jid: &str,
        store: &Arc<Mutex<DeviceStore>>,
    ) -> Result<Vec<u8>> {
        use crate::crypto::envelope::SignalEnvelope;

        match enc_type {
            "skmsg" => {
                // SenderKey (group message)
                let envelope = SignalEnvelope::deserialize(cipher_bytes)?;
                if let SignalEnvelope::SenderKey(sk_msg) = envelope {
                    let mut store_guard = store.lock().await;
                    if let Some(record) = store_guard.get_sender_key_mut(chat_jid, sender_jid) {
                        record.decrypt(sk_msg.iteration.unwrap_or(0), sk_msg.ciphertext.as_deref().unwrap_or_default())
                    } else {
                        Err(anyhow!("No SenderKey for {} in group {}", sender_jid, chat_jid))
                    }
                } else {
                    Err(anyhow!("Expected SenderKey envelope for skmsg"))
                }
            }
            "pkmsg" => {
                // PreKeySignalMessage
                let envelope = SignalEnvelope::deserialize(cipher_bytes)?;
                if let SignalEnvelope::PreKey(prekey_msg) = envelope {
                    let inner_signal = crate::proto::signal::SignalMessage::decode(
                        prekey_msg.message.as_deref().unwrap_or_default()
                    )?;

                    let mut ratchet_pub = [0u8; 32];
                    let rk = inner_signal.ratcheting_key.as_deref().unwrap_or_default();
                    if rk.len() == 32 {
                        ratchet_pub.copy_from_slice(rk);
                    } else if rk.len() == 33 && rk[0] == 0x05 {
                        ratchet_pub.copy_from_slice(&rk[1..]);
                    }

                    let mut store_guard = store.lock().await;
                    if let Some(session_bytes) = store_guard.get_session(sender_jid) {
                        let mut session: Session = serde_json::from_slice(session_bytes)?;
                        let plaintext = session.decrypt(&ratchet_pub, inner_signal.counter.unwrap_or(0), inner_signal.ciphertext.as_deref().unwrap_or_default())?;
                        // Persist updated session
                        let updated = serde_json::to_vec(&session)?;
                        store_guard.save_session(sender_jid.to_string(), updated);
                        Ok(plaintext)
                    } else {
                        Err(anyhow!("No session for {} — need key exchange first", sender_jid))
                    }
                } else {
                    Err(anyhow!("Expected PreKey envelope for pkmsg"))
                }
            }
            "msg" | _ => {
                // Standard Signal message
                let envelope = SignalEnvelope::deserialize(cipher_bytes)?;
                if let SignalEnvelope::Signal(signal_msg) = envelope {
                    let mut ratchet_pub = [0u8; 32];
                    let rk = signal_msg.ratcheting_key.as_deref().unwrap_or_default();
                    if rk.len() == 32 {
                        ratchet_pub.copy_from_slice(rk);
                    } else if rk.len() == 33 && rk[0] == 0x05 {
                        ratchet_pub.copy_from_slice(&rk[1..]);
                    }

                    let mut store_guard = store.lock().await;
                    if let Some(session_bytes) = store_guard.get_session(sender_jid) {
                        let mut session: Session = serde_json::from_slice(session_bytes)?;
                        let plaintext = session.decrypt(&ratchet_pub, signal_msg.counter.unwrap_or(0), signal_msg.ciphertext.as_deref().unwrap_or_default())?;
                        let updated = serde_json::to_vec(&session)?;
                        store_guard.save_session(sender_jid.to_string(), updated);
                        Ok(plaintext)
                    } else {
                        Err(anyhow!("No session for {}", sender_jid))
                    }
                } else {
                    Err(anyhow!("Unexpected envelope type for enc_type={}", enc_type))
                }
            }
        }
    }

    /// Process notification nodes — extract chat/contact updates.
    async fn process_notification(
        node: &Node,
        store: &Arc<Mutex<DeviceStore>>,
        event_tx: &tokio::sync::mpsc::UnboundedSender<WhatsAppEvent>,
    ) {
        // Look for <set> children with chat info
        for child in node.get_children_by_tag("add").iter()
            .chain(node.get_children_by_tag("set").iter())
        {
            // Group create/update notifications
            if let Some(jid) = child.get_attr("jid") {
                let name = child.get_attr("subject")
                    .or_else(|| child.get_attr("name"))
                    .map(|s| s.to_string());

                let chat = Chat {
                    id: ChatId(jid.to_string()),
                    name: name.or_else(|| Some(jid.to_string())),
                    unread_count: 0,
                    is_group: jid.contains("@g.us"),
                    last_message_timestamp: chrono::Utc::now().timestamp(),
                };
                store.lock().await.add_chat(chat);
            }
        }

        // Process history sync blobs (contained in <enc> children within notifications)
        if let Content::Nodes(children) = &node.content {
            let mut chat_count = 0usize;
            for child in children {
                if child.tag == "enc" || child.tag == "hist_sync_notification" || child.tag == "history" {
                    if let Content::Bytes(blob) = &child.content {
                        // Try to decompress (zlib) and extract chat JIDs
                        let data = Self::try_decompress(blob).unwrap_or_else(|| blob.clone());
                        // Parse as best-effort: look for JID-like strings in the blob
                        chat_count += Self::extract_chats_from_sync(&data, store).await;
                    }
                }
            }
            if chat_count > 0 {
                tracing::info!("History sync: {} chats loaded", chat_count);
                let _ = event_tx.send(WhatsAppEvent::HistorySynced { chat_count });
            }
        }
    }

    /// Extract chat JIDs from a history sync blob using regex-free binary scanning.
    /// WhatsApp history sync blobs contain JIDs as UTF-8 strings embedded in protobuf fields.
    async fn extract_chats_from_sync(
        data: &[u8],
        store: &Arc<Mutex<DeviceStore>>,
    ) -> usize {
        let text = String::from_utf8_lossy(data);
        let mut count = 0;
        let mut s = store.lock().await;

        // Scan for JID patterns: <digits>@s.whatsapp.net or <digits>-<digits>@g.us
        for segment in text.split(|c: char| c.is_control() || c == '\0') {
            let trimmed = segment.trim();
            if (trimmed.ends_with("@s.whatsapp.net") || trimmed.ends_with("@g.us"))
                && trimmed.len() < 50
                && trimmed.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false)
            {
                let jid = trimmed.to_string();
                let is_group = jid.ends_with("@g.us");
                if !s.chats.contains_key(&jid) {
                    let chat = Chat {
                        id: ChatId(jid.clone()),
                        name: Some(jid.clone()),
                        unread_count: 0,
                        is_group,
                        last_message_timestamp: chrono::Utc::now().timestamp(),
                    };
                    s.add_chat(chat);
                    count += 1;
                }
            }
        }
        count
    }

    /// Try to decompress a zlib blob. Returns None if it's not zlib or decompression fails.
    fn try_decompress(data: &[u8]) -> Option<Vec<u8>> {
        use std::io::Read;
        let mut decoder = flate2::read::ZlibDecoder::new(data);
        let mut result = Vec::new();
        decoder.read_to_end(&mut result).ok()?;
        Some(result)
    }

    /// Static helper for persisting store (usable from read_loop).
    async fn persist_store_static(store: &Arc<Mutex<DeviceStore>>, db_path: &str) {
        let s = store.lock().await;
        if let Ok(conn) = rusqlite::Connection::open(db_path) {
            if let Err(e) = s.save_to_db(&conn) {
                tracing::warn!("Failed to persist store: {}", e);
            }
        }
    }

    // ─── IQ / Node sending ──────────────────────────────────────────

    pub async fn query_usync(&self, users: Vec<String>) -> Result<Node> {
        let id = format!("{:x}", rand::random::<u64>());
        let request = crate::usync::USyncRequest::new(users);
        let node = request.to_node(&id);
        self.send_iq(node).await
    }

    pub async fn query_usync_for_message(&self, users: Vec<String>) -> Result<Node> {
        let id = format!("{:x}", rand::random::<u64>());
        let request = crate::usync::USyncRequest::for_message(users);
        let node = request.to_node(&id);
        self.send_iq(node).await
    }

    pub async fn send_node(&self, node: &Node) -> Result<()> {
        let mut inner_guard = self.inner.lock().await;
        let sender = inner_guard.sender.as_mut().ok_or_else(|| anyhow!("Not connected"))?;
        let mut encoder = Encoder::new();
        let bytes = encoder.encode(node)?;
        sender.send_encrypted_frame(&bytes).await
    }

    pub async fn send_iq(&self, node: Node) -> Result<Node> {
        let iq_id = node.get_attr("id").map(|s| s.to_string()).unwrap_or_else(|| {
            format!("{:x}", rand::random::<u64>())
        });

        let (tx, rx) = oneshot::channel();
        {
            let inner_guard = self.inner.lock().await;
            inner_guard.iq_tracker.lock().await.insert(iq_id.clone(), tx);
        }

        self.send_node(&node).await?;

        let response = timeout(Duration::from_secs(30), rx).await
            .context("IQ response timeout")?
            .map_err(|_| anyhow!("IQ response channel dropped"))?;

        Ok(response)
    }

    pub async fn fetch_prekeys(&self, jid: &str) -> Result<Node> {
        let id = format!("{:x}", rand::random::<u64>());
        let mut user_attrs = HashMap::new();
        user_attrs.insert("jid".to_string(), AttrValue::String(jid.to_string()));

        let user_node = Node::new("user", user_attrs, Content::None);
        // whatsmeow uses <key> wrapper (not <query>); IQ goes to server, not user
        let key_node = Node::new("key", HashMap::new(), Content::Nodes(vec![user_node]));

        let mut iq_attrs = HashMap::new();
        iq_attrs.insert("id".to_string(), AttrValue::String(id));
        iq_attrs.insert("xmlns".to_string(), AttrValue::String("encrypt".to_string()));
        iq_attrs.insert("type".to_string(), AttrValue::String("get".to_string()));
        iq_attrs.insert("to".to_string(), AttrValue::String("s.whatsapp.net".to_string()));

        let iq_node = Node::new("iq", iq_attrs, Content::Nodes(vec![key_node]));
        self.send_iq(iq_node).await
    }

    pub async fn get_or_create_session(&self, jid: &str) -> Result<Session> {
        // 1. Try to find existing session
        {
            let store = self.store.lock().await;
            if let Some(session_bytes) = store.get_session(jid) {
                let session: Session = serde_json::from_slice(session_bytes)?;
                tracing::info!("Reusing existing session for {}", jid);
                return Ok(session);
            }
        }

        // 2. Fetch PreKeys from server (use full device JID for per-device prekey fetch)
        tracing::info!("Fetching prekeys for device {}", jid);
        let prekey_node = self.fetch_prekeys(jid).await?;

        // 3. Parse PreKeys from the response node
        let (remote_identity, remote_signed_prekey, remote_prekey_id, remote_skey_id, remote_otpk) =
            Self::parse_prekey_response(&prekey_node)?;

        // 4. X3DH key agreement using our identity key
        let (my_identity_priv, _my_identity_pub) = {
            let store = self.store.lock().await;
            (store.identity_key_priv, store.identity_key_pub)
        };
        let my_identity = StaticSecret::from(my_identity_priv);
        let my_ephemeral = StaticSecret::random_from_rng(rand::thread_rng());
        let ephemeral_pub = PublicKey::from(&my_ephemeral);

        // Include one-time prekey in X3DH if the server provided one
        let otpk_pubkey = remote_otpk.map(PublicKey::from);
        let root_key_bytes = crate::crypto::derive_root_key(
            &my_identity,
            &my_ephemeral,
            &PublicKey::from(remote_identity),
            &PublicKey::from(remote_signed_prekey),
            otpk_pubkey.as_ref(),
        );

        tracing::info!("X3DH complete: root_key[0..4]={:02x?}, used_otpk={}", &root_key_bytes[..4], remote_otpk.is_some());

        let root_key = crate::crypto::ratchet::RootKey::new(root_key_bytes);
        let mut session = Session::new_as_sender(remote_identity, root_key, remote_signed_prekey);

        // Set pending prekey info for the first message
        session.pending_prekey = Some(crate::crypto::session::PendingPreKey {
            prekey_id: remote_prekey_id,
            signed_prekey_id: remote_skey_id,
            base_key: *ephemeral_pub.as_bytes(),
        });

        // 5. Persist session
        let session_bytes = serde_json::to_vec(&session)?;
        self.store.lock().await.save_session(jid.to_string(), session_bytes);

        // 6. Persist store
        let _ = self.persist_store().await;

        Ok(session)
    }

    /// Parse a prekey response IQ node to extract the remote's identity key,
    /// signed prekey, one-time prekey, and related IDs.
    /// Returns: (identity, signed_prekey, prekey_id, skey_id, one_time_prekey_value)
    fn parse_prekey_response(node: &Node) -> Result<([u8; 32], [u8; 32], u32, u32, Option<[u8; 32]>)> {
        let list_node = node.get_child_by_tag("list")
            .ok_or_else(|| anyhow!("Missing <list> in prekey response"))?;
        let user_node = list_node.get_child_by_tag("user")
            .ok_or_else(|| anyhow!("Missing <user> in prekey response"))?;

        // Log the device JID from the response
        if let Some(jid_attr) = user_node.get_attr("jid") {
            tracing::info!("Prekey response device JID: {}", jid_attr);
        }

        // Parse identity key (accept 32 or 33 bytes, strip 0x05 prefix if present)
        let identity_node = user_node.get_child_by_tag("identity")
            .ok_or_else(|| anyhow!("Missing <identity> in prekey response"))?;
        let identity_bytes = Self::extract_key_32(&identity_node.content, "identity")?;

        // Parse signed prekey
        let skey_node = user_node.get_child_by_tag("skey")
            .ok_or_else(|| anyhow!("Missing <skey> in prekey response"))?;
        let skey_value_node = skey_node.get_child_by_tag("value")
            .ok_or_else(|| anyhow!("Missing <value> in <skey>"))?;
        let skey_bytes = Self::extract_key_32(&skey_value_node.content, "signed prekey")?;

        let skey_id = Self::parse_key_id(skey_node.get_child_by_tag("id"));

        // Parse one-time prekey (both ID and VALUE)
        let (prekey_id, prekey_value) = if let Some(key_node) = user_node.get_child_by_tag("key") {
            let id = Self::parse_key_id(key_node.get_child_by_tag("id"));
            let value = key_node.get_child_by_tag("value")
                .and_then(|v| Self::extract_key_32(&v.content, "one-time prekey").ok());
            tracing::info!("One-time prekey: id={}, has_value={}", id, value.is_some());
            (id, value)
        } else {
            tracing::info!("No one-time prekey in response");
            (0, None)
        };

        tracing::info!("Parsed prekeys: identity={}B, skey={}B (id={}), prekey_id={}, has_otpk={}",
            identity_bytes.len(), skey_bytes.len(), skey_id, prekey_id, prekey_value.is_some());

        Ok((identity_bytes, skey_bytes, prekey_id, skey_id, prekey_value))
    }

    /// Extract a 32-byte key from Content::Bytes, handling both 32-byte (raw) and 33-byte (0x05 prefix) formats.
    fn extract_key_32(content: &Content, name: &str) -> Result<[u8; 32]> {
        match content {
            Content::Bytes(b) if b.len() == 32 => {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(b);
                Ok(arr)
            }
            Content::Bytes(b) if b.len() == 33 && b[0] == 0x05 => {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&b[1..]);
                Ok(arr)
            }
            Content::Bytes(b) => Err(anyhow!("Invalid {} key length: {} bytes", name, b.len())),
            _ => Err(anyhow!("Invalid {} key format: expected bytes", name)),
        }
    }

    /// Parse a key ID from a node's binary content.
    /// WhatsApp encodes key IDs as 3-byte big-endian (matching whatsmeow's serializedKeyID).
    fn parse_key_id(node: Option<&Node>) -> u32 {
        match node {
            Some(n) => {
                if let Content::Bytes(b) = &n.content {
                    match b.len() {
                        3 => (b[0] as u32) << 16 | (b[1] as u32) << 8 | (b[2] as u32),
                        4 => u32::from_be_bytes([b[0], b[1], b[2], b[3]]),
                        2 => (b[0] as u32) << 8 | (b[1] as u32),
                        1 => b[0] as u32,
                        _ => String::from_utf8_lossy(b).parse::<u32>().unwrap_or(0),
                    }
                } else {
                    0
                }
            }
            None => 0,
        }
    }
}
