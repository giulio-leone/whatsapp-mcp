use crate::binary::noise::{NoiseHandshake, NOISE_MODE};
use crate::socket::{FrameSocket, NoiseSender, NoiseReceiver};
use crate::proto::wa_web_protobufs_wa6::{HandshakeMessage, ClientPayload};
use crate::proto::wa_web_protobufs_wa6::handshake_message::{ClientHello, ClientFinish};
use crate::binary::tokens::WA_CONN_HEADER;
use crate::store::DeviceStore;
use crate::binary::node::{Node, AttrValue, Content};
use crate::binary::{Encoder, Decoder};
use crate::qr::QrRef;
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
    Connected { jid: String },
    MessageReceived(Message),
    ReceiptReceived { id: String, from: String, timestamp: i64 },
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

        // 1. Get or create Signal session
        let mut session = self.get_or_create_session(&jid).await?;

        let msg_id = format!("{:x}", rand::random::<u64>());
        let timestamp = chrono::Utc::now().timestamp();

        // 2. Build E2E Message protobuf
        use crate::proto::wa_web_protobufs_e2e as e2e;
        let e2e_msg = e2e::Message {
            conversation: Some(text.to_string()),
            ..Default::default()
        };
        let mut e2e_bytes = Vec::new();
        e2e_msg.encode(&mut e2e_bytes)?;

        // 3. Encrypt with full Double Ratchet
        let (ciphertext, ratchet_pub, counter, prev_counter) = session.encrypt(&e2e_bytes)?;

        // 4. Build MAC (HMAC-SHA256 truncated to 10 bytes)
        let mac = crate::crypto::compute_mac(
            &session.ratchet.root_key.key, // Use a derived key; simplified
            &ciphertext,
        );

        // 5. Build Signal envelope
        let signal_msg = crate::proto::signal::SignalMessage {
            ratcheting_key: ratchet_pub.to_vec(),
            counter,
            previous_counter: prev_counter,
            ciphertext: ciphertext.clone(),
        };
        let mut signal_bytes = Vec::new();
        signal_msg.encode(&mut signal_bytes)?;

        // 6. Version byte + serialized message + truncated MAC
        let mut envelope = Vec::with_capacity(1 + signal_bytes.len() + 10);
        let version_byte = match &session.pending_prekey {
            Some(_) => 0x33, // PreKeySignalMessage: version 3, type 3
            None => 0x32,    // SignalMessage: version 3, type 2
        };
        envelope.push(version_byte);
        envelope.extend_from_slice(&signal_bytes);
        envelope.extend_from_slice(&mac[..10]);

        // 7. Build WAP message node
        let mut msg_attrs = HashMap::new();
        msg_attrs.insert("id".to_string(), AttrValue::String(msg_id.clone()));
        msg_attrs.insert("to".to_string(), AttrValue::String(jid.clone()));
        msg_attrs.insert("type".to_string(), AttrValue::String("text".to_string()));

        let enc_type = if session.pending_prekey.is_some() { "pkmsg" } else { "msg" };
        let mut enc_attrs = HashMap::new();
        enc_attrs.insert("v".to_string(), AttrValue::String("2".to_string()));
        enc_attrs.insert("type".to_string(), AttrValue::String(enc_type.to_string()));

        let enc_node = Node::new("enc", enc_attrs, Content::Bytes(envelope));
        let message_node = Node::new("message", msg_attrs, Content::Nodes(vec![enc_node]));

        self.send_node(&message_node).await?;

        // 8. Persist updated session
        let session_bytes = serde_json::to_vec(&session)?;
        self.store.lock().await.save_session(jid.clone(), session_bytes);

        // 9. Persist store
        let _ = self.persist_store().await;

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

// ─── Internal Protocol Logic ────────────────────────────────────────

impl WhatsAppClient {
    async fn connect_internal(&self, inner: &mut WhatsAppClientInner) -> Result<()> {
        // Reset noise state for a fresh handshake
        inner.noise = NoiseHandshake::new();
        inner.msg_counter = 0;

        let mut fs = FrameSocket::connect().await?;

        let ephemeral_priv = StaticSecret::random_from_rng(rand::thread_rng());
        let ephemeral_pub = PublicKey::from(&ephemeral_priv);

        inner.noise.start(NOISE_MODE, WA_CONN_HEADER)?;

        // Send connection header
        fs.send_frame(WA_CONN_HEADER).await?;

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

        if is_new_login {
            *self.state.lock().await = ConnectionState::WaitingForQr;
            let qr_ref = QrRef::generate();
            let store = self.store.lock().await;
            let qr_data = qr_ref.encode(&store.noise_key.pub_key, &store.identity_key_pub);
            drop(store);
            let _ = inner.event_tx.send(WhatsAppEvent::QrCode(qr_data));
        }

        let (pub_key_bytes, priv_key_bytes) = {
            let store = self.store.lock().await;
            (store.noise_key.pub_key, store.noise_key.priv_key)
        };
        let encrypted_pubkey = inner.noise.encrypt(&pub_key_bytes)?;
        inner.noise.mix_shared_secret_into_key(&priv_key_bytes, &server_ephemeral_arr)?;

        let client_payload = ClientPayload {
            user_agent: Some(crate::proto::wa_web_protobufs_wa6::client_payload::UserAgent {
                platform: Some(crate::proto::wa_web_protobufs_wa6::client_payload::user_agent::Platform::Web.into()),
                app_version: Some(crate::proto::wa_web_protobufs_wa6::client_payload::user_agent::AppVersion {
                    primary: Some(2),
                    secondary: Some(2411),
                    tertiary: Some(2),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            web_info: Some(crate::proto::wa_web_protobufs_wa6::client_payload::WebInfo {
                web_sub_platform: Some(crate::proto::wa_web_protobufs_wa6::client_payload::web_info::WebSubPlatform::WebBrowser.into()),
                ..Default::default()
            }),
            connect_type: Some(crate::proto::wa_web_protobufs_wa6::client_payload::ConnectType::WifiUnknown.into()),
            connect_reason: Some(crate::proto::wa_web_protobufs_wa6::client_payload::ConnectReason::UserActivated.into()),
            push_name: Some("WhatsApp MCP".to_string()),
            ..Default::default()
        };

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
        let receiver = NoiseReceiver { rx, read_cipher, read_counter: 0 };

        inner.sender = Some(sender);

        let iq_mutex = inner.iq_tracker.clone();
        let event_tx = inner.event_tx.clone();
        let store_ref = self.store.clone();
        let state_ref = self.state.clone();
        let db_path = self.db_path.clone();

        tokio::spawn(async move {
            Self::read_loop(receiver, iq_mutex, event_tx, store_ref, state_ref, db_path).await;
        });

        Ok(())
    }

    async fn read_loop(
        mut receiver: NoiseReceiver,
        iq_tracker: Arc<Mutex<HashMap<String, oneshot::Sender<Node>>>>,
        event_tx: tokio::sync::mpsc::UnboundedSender<WhatsAppEvent>,
        store: Arc<Mutex<DeviceStore>>,
        state: Arc<Mutex<ConnectionState>>,
        db_path: String,
    ) {
        loop {
            match receiver.receive_encrypted_frame().await {
                Ok(bytes) => {
                    let mut decoder = Decoder::new(&bytes);
                    if let Ok(node) = decoder.read_node() {
                        tracing::debug!("Received node: tag={}, attrs={:?}", node.tag, node.attrs.keys().collect::<Vec<_>>());

                        match node.tag.as_str() {
                            "iq" => {
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

                                // Determine actual sender for group messages
                                let sender_jid = participant.as_deref().unwrap_or(&from);

                                let text = if let Some(enc_node) = node.get_child_by_tag("enc") {
                                    let enc_type = enc_node.get_attr("type").unwrap_or("");
                                    match &enc_node.content {
                                        Content::Bytes(cipher_bytes) => {
                                            match Self::decrypt_message(cipher_bytes, enc_type, sender_jid, &from, &store).await {
                                                Ok(plaintext) => {
                                                    use crate::proto::wa_web_protobufs_e2e as e2e;
                                                    if let Ok(e2e_msg) = e2e::Message::decode(&plaintext[..]) {
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
                                    // Fallback: try <body> text content
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
                                if let Some(jid_str) = node.get_attr("lid").or_else(|| node.get_attr("jid")) {
                                    let jid = jid_str.to_string();
                                    {
                                        let mut s = store.lock().await;
                                        s.our_jid = Some(jid.clone());
                                    }
                                    // Persist store after login
                                    Self::persist_store_static(&store, &db_path).await;
                                    let _ = event_tx.send(WhatsAppEvent::Connected { jid });
                                }
                            }
                            "failure" => {
                                let reason = node.get_attr("reason").unwrap_or("unknown");
                                tracing::error!("Login failed: {}", reason);
                            }
                            "stream:error" => {
                                tracing::error!("Stream error received");
                                *state.lock().await = ConnectionState::Disconnected;
                                let _ = event_tx.send(WhatsAppEvent::Disconnected);
                                break;
                            }
                            "notification" => {
                                let notif_type = node.get_attr("type").unwrap_or("");
                                tracing::debug!("Notification: type={}", notif_type);

                                // Process history sync notifications
                                if notif_type == "encrypt" || notif_type == "w:gp2" || notif_type == "server_sync" {
                                    Self::process_notification(&node, &store, &event_tx).await;
                                }
                            }
                            _ => {
                                tracing::debug!("Received unhandled node: {}", node.tag);
                            }
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
                        record.decrypt(sk_msg.iteration, &sk_msg.ciphertext)
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
                        &prekey_msg.message[..]
                    )?;

                    let mut ratchet_pub = [0u8; 32];
                    if inner_signal.ratcheting_key.len() == 32 {
                        ratchet_pub.copy_from_slice(&inner_signal.ratcheting_key);
                    }

                    let mut store_guard = store.lock().await;
                    if let Some(session_bytes) = store_guard.get_session(sender_jid) {
                        let mut session: Session = serde_json::from_slice(session_bytes)?;
                        let plaintext = session.decrypt(&ratchet_pub, inner_signal.counter, &inner_signal.ciphertext)?;
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
                    if signal_msg.ratcheting_key.len() == 32 {
                        ratchet_pub.copy_from_slice(&signal_msg.ratcheting_key);
                    }

                    let mut store_guard = store.lock().await;
                    if let Some(session_bytes) = store_guard.get_session(sender_jid) {
                        let mut session: Session = serde_json::from_slice(session_bytes)?;
                        let plaintext = session.decrypt(&ratchet_pub, signal_msg.counter, &signal_msg.ciphertext)?;
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
        let query_node = Node::new("query", [("xmlns".to_string(), AttrValue::String("encrypt".to_string()))].into(), Content::Nodes(vec![user_node]));

        let mut iq_attrs = HashMap::new();
        iq_attrs.insert("id".to_string(), AttrValue::String(id));
        iq_attrs.insert("xmlns".to_string(), AttrValue::String("encrypt".to_string()));
        iq_attrs.insert("type".to_string(), AttrValue::String("get".to_string()));
        iq_attrs.insert("to".to_string(), AttrValue::String(jid.to_string()));

        let iq_node = Node::new("iq", iq_attrs, Content::Nodes(vec![query_node]));
        self.send_iq(iq_node).await
    }

    pub async fn get_or_create_session(&self, jid: &str) -> Result<Session> {
        // 1. Try to find existing session
        {
            let store = self.store.lock().await;
            if let Some(session_bytes) = store.get_session(jid) {
                let session: Session = serde_json::from_slice(session_bytes)?;
                return Ok(session);
            }
        }

        // 2. Fetch PreKeys from server
        let prekey_node = self.fetch_prekeys(jid).await?;

        // 3. Parse PreKeys from the response node
        let (remote_identity, remote_signed_prekey, remote_prekey_id, remote_skey_id) =
            Self::parse_prekey_response(&prekey_node)?;

        // 4. X3DH key agreement using our identity key
        let (my_identity_priv, my_identity_pub) = {
            let store = self.store.lock().await;
            (store.identity_key_priv, store.identity_key_pub)
        };
        let my_identity = StaticSecret::from(my_identity_priv);
        let my_ephemeral = StaticSecret::random_from_rng(rand::thread_rng());
        let ephemeral_pub = PublicKey::from(&my_ephemeral);

        let root_key_bytes = crate::crypto::derive_root_key(
            &my_identity,
            &my_ephemeral,
            &PublicKey::from(remote_identity),
            &PublicKey::from(remote_signed_prekey),
            None,
        );

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
    /// signed prekey, and related IDs.
    fn parse_prekey_response(node: &Node) -> Result<([u8; 32], [u8; 32], u32, u32)> {
        let list_node = node.get_child_by_tag("list")
            .ok_or_else(|| anyhow!("Missing <list> in prekey response"))?;
        let user_node = list_node.get_child_by_tag("user")
            .ok_or_else(|| anyhow!("Missing <user> in prekey response"))?;

        // Parse identity key
        let identity_node = user_node.get_child_by_tag("identity")
            .ok_or_else(|| anyhow!("Missing <identity> in prekey response"))?;
        let identity_bytes = match &identity_node.content {
            Content::Bytes(b) if b.len() == 32 => {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(b);
                arr
            }
            _ => return Err(anyhow!("Invalid identity key format")),
        };

        // Parse signed prekey
        let skey_node = user_node.get_child_by_tag("skey")
            .ok_or_else(|| anyhow!("Missing <skey> in prekey response"))?;
        let skey_value_node = skey_node.get_child_by_tag("value")
            .ok_or_else(|| anyhow!("Missing <value> in <skey>"))?;
        let skey_bytes = match &skey_value_node.content {
            Content::Bytes(b) if b.len() == 32 => {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(b);
                arr
            }
            _ => return Err(anyhow!("Invalid signed prekey format")),
        };

        let skey_id = Self::parse_node_u32(skey_node.get_child_by_tag("id"));

        // Parse one-time prekey id
        let prekey_id = user_node.get_child_by_tag("key")
            .and_then(|k| Self::parse_node_u32(k.get_child_by_tag("id")).into())
            .unwrap_or(0);

        Ok((identity_bytes, skey_bytes, prekey_id, skey_id))
    }

    fn parse_node_u32(node: Option<&Node>) -> u32 {
        match node {
            Some(n) => {
                if let Content::Bytes(b) = &n.content {
                    String::from_utf8_lossy(b).parse::<u32>().unwrap_or(0)
                } else {
                    0
                }
            }
            None => 0,
        }
    }
}
