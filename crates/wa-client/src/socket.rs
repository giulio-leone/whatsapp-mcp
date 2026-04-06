use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
use tokio_tungstenite::tungstenite::protocol::Message;
use std::time::Duration;
use anyhow::{Result, anyhow};
use url::Url;
use aes_gcm::{Aes256Gcm, aead::{Aead, KeyInit}};

pub const WS_URL: &str = "wss://web.whatsapp.com/ws/chat";

use futures_util::stream::{SplitSink, SplitStream};

pub struct FrameSocket {
    ws: Option<WebSocketStream<MaybeTlsStream<TcpStream>>>,
}

impl FrameSocket {
    pub async fn connect() -> Result<Self> {
        let url = Url::parse(WS_URL)?;
        let (ws, _) = connect_async(url).await?;
        Ok(Self { ws: Some(ws) })
    }

    pub fn split(mut self) -> Result<(
        SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, tokio_tungstenite::tungstenite::protocol::Message>,
        SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>
    )> {
        let ws = self.ws.take().ok_or_else(|| anyhow!("Already split"))?;
        Ok(ws.split())
    }

    pub async fn send_frame(&mut self, data: &[u8]) -> Result<()> {
        if let Some(ws) = &mut self.ws {
            ws.send(tokio_tungstenite::tungstenite::protocol::Message::Binary(data.to_vec())).await
                .map_err(|e| anyhow!("Failed to send frame: {}", e))
        } else {
            Err(anyhow!("Socket already split"))
        }
    }

    pub async fn receive_frame(&mut self) -> Result<Vec<u8>> {
        if let Some(ws) = &mut self.ws {
            while let Some(msg) = ws.next().await {
                match msg {
                    Ok(tokio_tungstenite::tungstenite::protocol::Message::Binary(data)) => return Ok(data),
                    Ok(tokio_tungstenite::tungstenite::protocol::Message::Ping(_)) => continue,
                    Ok(tokio_tungstenite::tungstenite::protocol::Message::Close(_)) => return Err(anyhow!("Connection closed")),
                    Err(e) => return Err(anyhow!("WS error: {}", e)),
                    _ => continue,
                }
            }
            Err(anyhow!("WS stream ended"))
        } else {
            Err(anyhow!("Socket already split"))
        }
    }
}

pub struct NoiseSender {
    pub tx: SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, tokio_tungstenite::tungstenite::protocol::Message>,
    pub write_cipher: Aes256Gcm,
    pub write_counter: u32,
}

pub struct NoiseReceiver {
    pub rx: SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>,
    pub read_cipher: Aes256Gcm,
    pub read_counter: u32,
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
            
        self.tx.send(tokio_tungstenite::tungstenite::protocol::Message::Binary(ciphertext)).await
            .map_err(|e| anyhow!("Failed to send frame: {}", e))
    }
}

impl NoiseReceiver {
    fn generate_iv(counter: u32) -> [u8; 12] {
        let mut iv = [0u8; 12];
        iv[8..12].copy_from_slice(&counter.to_be_bytes());
        iv
    }

    pub async fn receive_encrypted_frame(&mut self) -> Result<Vec<u8>> {
        while let Some(msg) = self.rx.next().await {
            match msg {
                Ok(tokio_tungstenite::tungstenite::protocol::Message::Binary(ciphertext)) => {
                    let iv = Self::generate_iv(self.read_counter);
                    let nonce = aes_gcm::Nonce::from_slice(&iv);
                    self.read_counter += 1;
                    
                    let plaintext = self.read_cipher.decrypt(nonce, &ciphertext[..])
                        .map_err(|e| anyhow!("Decryption failed: {}", e))?;
                        
                    return Ok(plaintext);
                },
                Ok(_) => continue,
                Err(e) => return Err(anyhow!("WS error: {}", e)),
            }
        }
        Err(anyhow!("WS stream ended"))
    }
}
