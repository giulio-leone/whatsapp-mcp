use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
use tokio_tungstenite::tungstenite::protocol::Message;
use anyhow::{Result, anyhow};
use aes_gcm::{Aes256Gcm, aead::{Aead, KeyInit}};

pub const WS_URL: &str = "wss://web.whatsapp.com/ws/chat";

use futures_util::stream::{SplitSink, SplitStream};

/// WhatsApp framed WebSocket.
///
/// Mirrors whatsmeow's `FrameSocket`:
/// - The WA connection header is prepended to the **first** `SendFrame` only.
/// - Every frame is length-prefixed with a 3-byte big-endian header.
/// - On the receive side, incoming WS messages are reassembled from
///   `[3-byte len][payload]` segments.
pub struct FrameSocket {
    ws: Option<WebSocketStream<MaybeTlsStream<TcpStream>>>,
    /// WA header to prepend to the first frame. `None` after first send.
    header: Option<Vec<u8>>,
}

impl FrameSocket {
    pub async fn connect(wa_header: &[u8]) -> Result<Self> {
        use tokio_tungstenite::tungstenite::http::Request;

        let request = Request::builder()
            .uri(WS_URL)
            .header("Origin", "https://web.whatsapp.com")
            .header("Host", "web.whatsapp.com")
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header("Sec-WebSocket-Version", "13")
            .header("Sec-WebSocket-Key", tokio_tungstenite::tungstenite::handshake::client::generate_key())
            .body(())?;

        let (ws, _) = connect_async(request).await
            .map_err(|e| anyhow!("WebSocket connection failed: {}", e))?;
        Ok(Self {
            ws: Some(ws),
            header: Some(wa_header.to_vec()),
        })
    }

    pub fn split(mut self) -> Result<(
        SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, tokio_tungstenite::tungstenite::protocol::Message>,
        SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>
    )> {
        let ws = self.ws.take().ok_or_else(|| anyhow!("Already split"))?;
        Ok(ws.split())
    }

    /// Send a frame with the whatsmeow framing protocol:
    /// `[header (first time only)] [3-byte big-endian length] [data]`
    ///
    /// This exactly mirrors `FrameSocket.SendFrame` in whatsmeow.
    pub async fn send_frame(&mut self, data: &[u8]) -> Result<()> {
        let ws = self.ws.as_mut().ok_or_else(|| anyhow!("Socket already split"))?;

        let header_len = self.header.as_ref().map_or(0, |h| h.len());
        let data_len = data.len();

        // Build: [header?] [3-byte len] [data]
        let mut whole_frame = Vec::with_capacity(header_len + 3 + data_len);

        // Prepend WA header on first frame only
        if let Some(hdr) = self.header.take() {
            whole_frame.extend_from_slice(&hdr);
        }

        // 3-byte big-endian length of data
        whole_frame.push(((data_len >> 16) & 0xFF) as u8);
        whole_frame.push(((data_len >> 8) & 0xFF) as u8);
        whole_frame.push((data_len & 0xFF) as u8);

        // Actual data
        whole_frame.extend_from_slice(data);

        ws.send(Message::Binary(whole_frame)).await
            .map_err(|e| anyhow!("Failed to send frame: {}", e))
    }

    /// Receive a frame: reads a WS message, strips the 3-byte length prefix,
    /// returns the payload.
    pub async fn receive_frame(&mut self) -> Result<Vec<u8>> {
        let ws = self.ws.as_mut().ok_or_else(|| anyhow!("Socket already split"))?;
        while let Some(msg) = ws.next().await {
            match msg {
                Ok(Message::Binary(data)) => {
                    if data.len() < 3 {
                        return Err(anyhow!("Frame too short: {} bytes", data.len()));
                    }
                    let length = ((data[0] as usize) << 16)
                        | ((data[1] as usize) << 8)
                        | (data[2] as usize);
                    let payload = &data[3..];
                    if payload.len() < length {
                        return Err(anyhow!(
                            "Incomplete frame: expected {} bytes, got {}",
                            length,
                            payload.len()
                        ));
                    }
                    return Ok(payload[..length].to_vec());
                }
                Ok(Message::Ping(_)) => continue,
                Ok(Message::Close(_)) => return Err(anyhow!("Connection closed")),
                Err(e) => return Err(anyhow!("WS error: {}", e)),
                _ => continue,
            }
        }
        Err(anyhow!("WS stream ended"))
    }
}

// ─── Post-handshake encrypted sockets ───────────────────────────────

pub struct NoiseSender {
    pub tx: SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>,
    pub write_cipher: Aes256Gcm,
    pub write_counter: u32,
}

impl NoiseSender {
    fn generate_iv(counter: u32) -> [u8; 12] {
        let mut iv = [0u8; 12];
        iv[8..12].copy_from_slice(&counter.to_be_bytes());
        iv
    }

    pub async fn send_encrypted_frame(&mut self, plaintext: &[u8]) -> Result<()> {
        let iv = Self::generate_iv(self.write_counter);
        let nonce = aes_gcm::Nonce::from_slice(&iv);
        self.write_counter += 1;

        let ciphertext = self.write_cipher.encrypt(nonce, plaintext)
            .map_err(|e| anyhow!("Encryption failed: {}", e))?;

        // Post-handshake frames use 3-byte big-endian length prefix
        let data_len = ciphertext.len();
        let mut framed = Vec::with_capacity(3 + data_len);
        framed.push(((data_len >> 16) & 0xFF) as u8);
        framed.push(((data_len >> 8) & 0xFF) as u8);
        framed.push((data_len & 0xFF) as u8);
        framed.extend_from_slice(&ciphertext);

        self.tx.send(Message::Binary(framed)).await
            .map_err(|e| anyhow!("Failed to send frame: {}", e))
    }
}

pub struct NoiseReceiver {
    pub rx: SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>,
    pub read_cipher: Aes256Gcm,
    pub read_counter: u32,
    /// Internal buffer for reassembling frames across WS messages.
    buf: Vec<u8>,
    /// Queue of complete frames extracted from the buffer.
    frames: std::collections::VecDeque<Vec<u8>>,
}

impl NoiseReceiver {
    pub fn new(
        rx: SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>,
        read_cipher: Aes256Gcm,
    ) -> Self {
        Self {
            rx,
            read_cipher,
            read_counter: 0,
            buf: Vec::new(),
            frames: std::collections::VecDeque::new(),
        }
    }

    fn generate_iv(counter: u32) -> [u8; 12] {
        let mut iv = [0u8; 12];
        iv[8..12].copy_from_slice(&counter.to_be_bytes());
        iv
    }

    /// Extract complete frames from the internal buffer, mirroring
    /// whatsmeow's `FrameSocket.processData`.
    fn process_data(&mut self) {
        while self.buf.len() >= 3 {
            let length = ((self.buf[0] as usize) << 16)
                | ((self.buf[1] as usize) << 8)
                | (self.buf[2] as usize);

            if self.buf.len() < 3 + length {
                // Not enough data for a complete frame — wait for more
                break;
            }

            // Extract the frame payload (without the 3-byte header)
            let frame = self.buf[3..3 + length].to_vec();
            self.buf.drain(..3 + length);
            self.frames.push_back(frame);
        }
    }

    pub async fn receive_encrypted_frame(&mut self) -> Result<Vec<u8>> {
        loop {
            // First, check if we already have complete frames queued
            if let Some(ciphertext) = self.frames.pop_front() {
                let iv = Self::generate_iv(self.read_counter);
                let nonce = aes_gcm::Nonce::from_slice(&iv);
                self.read_counter += 1;

                let plaintext = self.read_cipher.decrypt(nonce, &ciphertext[..])
                    .map_err(|e| anyhow!("Decryption failed: {} (ct_len={}, counter={})", e, ciphertext.len(), self.read_counter - 1))?;

                return Ok(plaintext);
            }

            // Read more data from the WebSocket
            match self.rx.next().await {
                Some(Ok(Message::Binary(data))) => {
                    self.buf.extend_from_slice(&data);
                    self.process_data();
                    // Loop back to check if we got any complete frames
                },
                Some(Ok(_)) => continue,
                Some(Err(e)) => return Err(anyhow!("WS error: {}", e)),
                None => return Err(anyhow!("WS stream ended")),
            }
        }
    }
}

