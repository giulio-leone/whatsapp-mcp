use anyhow::{anyhow, Context, Result};
use aes::cipher::{BlockEncryptMut, KeyIvInit};
use hmac::{Hmac, Mac};
use sha2::{Sha256, Digest};
use hkdf::Hkdf;
use rand::RngCore;

type Aes256CbcEnc = cbc::Encryptor<aes::Aes256>;
type HmacSha256 = Hmac<Sha256>;

/// Media types recognized by WhatsApp, each with its own HKDF info string
#[derive(Debug, Clone, Copy)]
pub enum MediaType {
    Image,
    Video,
    Audio,
    Document,
}

impl MediaType {
    /// HKDF info string used for key derivation per media type
    pub fn hkdf_info(&self) -> &[u8] {
        match self {
            MediaType::Image => b"WhatsApp Image Keys",
            MediaType::Video => b"WhatsApp Video Keys",
            MediaType::Audio => b"WhatsApp Audio Keys",
            MediaType::Document => b"WhatsApp Document Keys",
        }
    }

    /// Upload path segment on the CDN
    pub fn upload_path(&self) -> &str {
        match self {
            MediaType::Image => "image",
            MediaType::Video => "video",
            MediaType::Audio => "audio",
            MediaType::Document => "document",
        }
    }

    /// WhatsApp mediatype attribute value for IQ queries
    pub fn attr_value(&self) -> &str {
        match self {
            MediaType::Image => "image",
            MediaType::Video => "video",
            MediaType::Audio => "audio",
            MediaType::Document => "document",
        }
    }
}

/// Result of encrypting a media file for upload
#[derive(Debug)]
pub struct EncryptedMedia {
    /// 32-byte random media key (goes into the proto)
    pub media_key: Vec<u8>,
    /// The encrypted file bytes (ciphertext + 10-byte MAC)
    pub ciphertext: Vec<u8>,
    /// SHA256 of the original plaintext file
    pub file_sha256: Vec<u8>,
    /// SHA256 of the encrypted file (ciphertext + MAC)
    pub file_enc_sha256: Vec<u8>,
    /// Original file length
    pub file_length: u64,
}

/// Derives the 112-byte expanded key from a 32-byte media key.
///
/// Layout: IV (16 bytes) | AES key (32 bytes) | HMAC key (32 bytes) | ref key (32 bytes)
fn derive_media_keys(media_key: &[u8], media_type: MediaType) -> Result<(Vec<u8>, Vec<u8>, Vec<u8>)> {
    let hk = Hkdf::<Sha256>::new(None, media_key);
    let mut expanded = [0u8; 112];
    hk.expand(media_type.hkdf_info(), &mut expanded)
        .map_err(|_| anyhow!("HKDF expand failed"))?;

    let iv = expanded[0..16].to_vec();
    let aes_key = expanded[16..48].to_vec();
    let hmac_key = expanded[48..80].to_vec();

    Ok((iv, aes_key, hmac_key))
}

/// Encrypt a media file for WhatsApp upload.
///
/// WhatsApp media encryption spec:
/// 1. Generate random 32-byte media_key
/// 2. HKDF(media_key, info=type-specific) → IV(16) + AES(32) + HMAC(32)
/// 3. PKCS7-pad plaintext to AES block boundary
/// 4. AES-256-CBC encrypt with derived IV and key
/// 5. HMAC-SHA256(IV + ciphertext) with derived HMAC key
/// 6. Final = ciphertext + HMAC[0..10]
pub fn encrypt_media(plaintext: &[u8], media_type: MediaType) -> Result<EncryptedMedia> {
    // Generate random 32-byte media key
    let mut media_key = vec![0u8; 32];
    rand::thread_rng().fill_bytes(&mut media_key);

    encrypt_media_with_key(plaintext, &media_key, media_type)
}

/// Encrypt media with a specific key (for testing or deterministic use)
pub fn encrypt_media_with_key(plaintext: &[u8], media_key: &[u8], media_type: MediaType) -> Result<EncryptedMedia> {
    if media_key.len() != 32 {
        anyhow::bail!("media_key must be exactly 32 bytes");
    }

    let (iv, aes_key, hmac_key) = derive_media_keys(media_key, media_type)?;

    // SHA256 of plaintext
    let file_sha256 = Sha256::digest(plaintext).to_vec();

    // PKCS7 padding
    let block_size = 16usize;
    let pad_len = block_size - (plaintext.len() % block_size);
    let mut padded = plaintext.to_vec();
    padded.extend(std::iter::repeat(pad_len as u8).take(pad_len));

    // AES-256-CBC encrypt
    let iv_arr: [u8; 16] = iv.clone().try_into().map_err(|_| anyhow!("IV size mismatch"))?;
    let key_arr: [u8; 32] = aes_key.try_into().map_err(|_| anyhow!("AES key size mismatch"))?;
    let encryptor = Aes256CbcEnc::new(&key_arr.into(), &iv_arr.into());

    // We already manually padded, so use NoPadding — allocate extra block for safety
    let mut buf = padded.clone();
    buf.extend_from_slice(&[0u8; 16]);
    let ct_len = padded.len();
    let ciphertext = encryptor
        .encrypt_padded_mut::<aes::cipher::block_padding::NoPadding>(&mut buf, ct_len)
        .map_err(|_| anyhow!("AES-CBC encryption failed"))?
        .to_vec();

    // HMAC-SHA256(IV + ciphertext)
    let mut mac = HmacSha256::new_from_slice(&hmac_key)
        .map_err(|_| anyhow!("HMAC key error"))?;
    mac.update(&iv);
    mac.update(&ciphertext);
    let mac_result = mac.finalize().into_bytes();

    // Final: ciphertext + first 10 bytes of HMAC
    let mut final_bytes = ciphertext;
    final_bytes.extend_from_slice(&mac_result[..10]);

    let file_enc_sha256 = Sha256::digest(&final_bytes).to_vec();

    Ok(EncryptedMedia {
        media_key: media_key.to_vec(),
        ciphertext: final_bytes,
        file_sha256,
        file_enc_sha256,
        file_length: plaintext.len() as u64,
    })
}

/// Media connection info received from WhatsApp server
#[derive(Debug, Clone)]
pub struct MediaConnInfo {
    pub auth: String,
    pub hosts: Vec<MediaHost>,
}

#[derive(Debug, Clone)]
pub struct MediaHost {
    pub hostname: String,
}

/// Upload encrypted media to WhatsApp CDN.
///
/// Steps:
/// 1. Use auth token + host from media_conn response
/// 2. POST to https://{host}/mms/{type}/{file_enc_sha256_b64}
/// 3. Headers: Origin: https://web.whatsapp.com, Referer, Auth token
/// 4. Body: encrypted file bytes
/// 5. Response contains `url` and `direct_path`
pub async fn upload_media(
    encrypted: &EncryptedMedia,
    media_type: MediaType,
    conn: &MediaConnInfo,
) -> Result<MediaUploadResult> {
    let file_enc_b64 = base64::Engine::encode(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        &encrypted.file_enc_sha256,
    );

    let host = conn.hosts.first()
        .ok_or_else(|| anyhow!("No media upload hosts available"))?;

    let url = format!(
        "https://{}/mms/{}/{}?auth={}&token={}",
        host.hostname,
        media_type.upload_path(),
        file_enc_b64,
        conn.auth,
        file_enc_b64,
    );

    tracing::info!("Uploading {} bytes to {}", encrypted.ciphertext.len(), host.hostname);

    let client = reqwest::Client::new();
    let resp = client.post(&url)
        .header("Origin", "https://web.whatsapp.com")
        .header("Referer", "https://web.whatsapp.com/")
        .header("Content-Type", "application/octet-stream")
        .body(encrypted.ciphertext.clone())
        .send()
        .await
        .context("Media upload HTTP request failed")?;

    let status = resp.status();
    let body = resp.text().await.context("Reading upload response")?;

    if !status.is_success() {
        anyhow::bail!("Media upload failed (HTTP {}): {}", status, body);
    }

    let json: serde_json::Value = serde_json::from_str(&body)
        .context("Parsing upload response JSON")?;

    let download_url = json["url"].as_str()
        .ok_or_else(|| anyhow!("No 'url' in upload response"))?
        .to_string();

    let direct_path = json["direct_path"].as_str()
        .ok_or_else(|| anyhow!("No 'direct_path' in upload response"))?
        .to_string();

    Ok(MediaUploadResult {
        url: download_url,
        direct_path,
    })
}

#[derive(Debug)]
pub struct MediaUploadResult {
    pub url: String,
    pub direct_path: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encrypt_media_roundtrip() {
        let plaintext = b"Hello, this is a test image payload!";
        let result = encrypt_media(plaintext, MediaType::Image).unwrap();

        assert_eq!(result.media_key.len(), 32);
        assert_eq!(result.file_length, plaintext.len() as u64);
        assert_eq!(result.file_sha256, Sha256::digest(plaintext).to_vec());
        // Ciphertext should be padded plaintext (48 bytes for 35-byte input) + 10 byte MAC
        let expected_ct_len = 48 + 10; // 35 bytes → pad to 48 + 10 MAC
        assert_eq!(result.ciphertext.len(), expected_ct_len);
    }

    #[test]
    fn test_encrypt_media_deterministic() {
        let plaintext = b"Deterministic test";
        let key = [42u8; 32];
        let r1 = encrypt_media_with_key(plaintext, &key, MediaType::Image).unwrap();
        let r2 = encrypt_media_with_key(plaintext, &key, MediaType::Image).unwrap();
        assert_eq!(r1.ciphertext, r2.ciphertext);
        assert_eq!(r1.file_sha256, r2.file_sha256);
        assert_eq!(r1.file_enc_sha256, r2.file_enc_sha256);
    }

    #[test]
    fn test_media_type_info_strings() {
        assert_eq!(MediaType::Image.hkdf_info(), b"WhatsApp Image Keys");
        assert_eq!(MediaType::Video.hkdf_info(), b"WhatsApp Video Keys");
        assert_eq!(MediaType::Audio.hkdf_info(), b"WhatsApp Audio Keys");
        assert_eq!(MediaType::Document.hkdf_info(), b"WhatsApp Document Keys");
    }

    #[test]
    fn test_invalid_key_length() {
        let result = encrypt_media_with_key(b"test", &[0u8; 16], MediaType::Image);
        assert!(result.is_err());
    }
}
