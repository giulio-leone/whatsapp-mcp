use aes_gcm::{
    aead::{Aead, KeyInit, Payload},
    Aes256Gcm,
};
use hkdf::Hkdf;
use sha2::{Digest, Sha256};
use x25519_dalek::{PublicKey, StaticSecret};
use anyhow::{Result, anyhow};

pub const NOISE_MODE: &str = "Noise_XX_25519_AESGCM_SHA256\0\0\0\0";

pub struct NoiseHandshake {
    pub hash: [u8; 32],
    pub salt: [u8; 32],
    pub cipher: Option<Aes256Gcm>,
    pub counter: u32,
}

impl NoiseHandshake {
    pub fn new() -> Self {
        Self {
            hash: [0u8; 32],
            salt: [0u8; 32],
            cipher: None,
            counter: 0,
        }
    }

    pub fn start(&mut self, pattern: &str, header: &[u8]) -> Result<()> {
        let pattern_bytes = pattern.as_bytes();
        if pattern_bytes.len() == 32 {
            self.hash.copy_from_slice(pattern_bytes);
        } else {
            let mut hasher = Sha256::new();
            hasher.update(pattern_bytes);
            self.hash.copy_from_slice(&hasher.finalize());
        }
        self.salt.copy_from_slice(&self.hash);
        
        let key = self.hash;
        self.cipher = Some(Aes256Gcm::new_from_slice(&key).map_err(|e| anyhow!("Failed to init cipher: {}", e))?);
        self.authenticate(header);
        Ok(())
    }

    pub fn authenticate(&mut self, data: &[u8]) {
        let mut hasher = Sha256::new();
        hasher.update(&self.hash);
        hasher.update(data);
        self.hash.copy_from_slice(&hasher.finalize());
    }

    fn generate_iv(counter: u32) -> [u8; 12] {
        let mut iv = [0u8; 12];
        iv[8..12].copy_from_slice(&counter.to_be_bytes());
        iv
    }

    pub fn encrypt(&mut self, plaintext: &[u8]) -> Result<Vec<u8>> {
        let cipher = self.cipher.as_ref().ok_or_else(|| anyhow!("Cipher not initialized"))?;
        let iv = Self::generate_iv(self.counter);
        let nonce = aes_gcm::Nonce::from_slice(&iv);
        self.counter += 1;
        
        let payload = Payload {
            msg: plaintext,
            aad: &self.hash,
        };
        
        let ciphertext = cipher.encrypt(nonce, payload)
            .map_err(|e| anyhow!("Encryption failed: {}", e))?;
        
        self.authenticate(&ciphertext);
        Ok(ciphertext)
    }

    pub fn decrypt(&mut self, ciphertext: &[u8]) -> Result<Vec<u8>> {
        let cipher = self.cipher.as_ref().ok_or_else(|| anyhow!("Cipher not initialized"))?;
        let iv = Self::generate_iv(self.counter);
        let nonce = aes_gcm::Nonce::from_slice(&iv);
        self.counter += 1;
        
        let payload = Payload {
            msg: ciphertext,
            aad: &self.hash,
        };
        
        let plaintext = cipher.decrypt(nonce, payload)
            .map_err(|e| anyhow!("Decryption failed: {}", e))?;
        
        self.authenticate(ciphertext);
        Ok(plaintext)
    }

    pub fn mix_shared_secret_into_key(&mut self, priv_key: &[u8; 32], pub_key: &[u8; 32]) -> Result<()> {
        let secret = StaticSecret::from(*priv_key);
        let public = PublicKey::from(*pub_key);
        let shared = secret.diffie_hellman(&public);
        self.mix_into_key(shared.as_bytes())
    }

    pub fn mix_into_key(&mut self, data: &[u8]) -> Result<()> {
        self.counter = 0;
        let (write_key, read_key) = self.extract_and_expand(&self.salt, data)?;
        self.salt.copy_from_slice(&write_key);
        self.cipher = Some(Aes256Gcm::new_from_slice(&read_key).map_err(|e| anyhow!("Failed to init cipher: {}", e))?);
        Ok(())
    }

    pub fn finish(&self) -> Result<(Aes256Gcm, Aes256Gcm)> {
        let (write_key, read_key) = self.extract_and_expand(&self.salt, &[])?;
        let write_cipher = Aes256Gcm::new_from_slice(&write_key).map_err(|e| anyhow!("Failed to init write cipher: {}", e))?;
        let read_cipher = Aes256Gcm::new_from_slice(&read_key).map_err(|e| anyhow!("Failed to init read cipher: {}", e))?;
        Ok((write_cipher, read_cipher))
    }

    fn extract_and_expand(&self, salt: &[u8], data: &[u8]) -> Result<([u8; 32], [u8; 32])> {
        let hk = Hkdf::<Sha256>::new(Some(salt), data);
        let mut okm = [0u8; 64];
        hk.expand(&[], &mut okm).map_err(|_| anyhow!("HKDF expand failed"))?;
        
        let mut write_key = [0u8; 32];
        let mut read_key = [0u8; 32];
        write_key.copy_from_slice(&okm[0..32]);
        read_key.copy_from_slice(&okm[32..64]);
        Ok((write_key, read_key))
    }
}
