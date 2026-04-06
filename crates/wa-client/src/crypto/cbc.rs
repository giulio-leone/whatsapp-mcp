use aes::Aes256;
use cbc::cipher::{BlockDecryptMut, BlockEncryptMut, KeyIvInit};
use anyhow::{Result, anyhow};

type Aes256CbcDec = cbc::Decryptor<Aes256>;
type Aes256CbcEnc = cbc::Encryptor<Aes256>;

pub fn decrypt(key: &[u8], iv: &[u8], ciphertext: &[u8]) -> Result<Vec<u8>> {
    if ciphertext.is_empty() {
        return Ok(Vec::new());
    }
    
    let mut buf = ciphertext.to_vec();
    let pt = Aes256CbcDec::new_from_slices(key, iv)
        .map_err(|e| anyhow!("Cipher init failed: {}", e))?
        .decrypt_padded_mut::<cbc::cipher::block_padding::Pkcs7>(&mut buf)
        .map_err(|e| anyhow!("Decryption failed: {}", e))?;
    
    Ok(pt.to_vec())
}

pub fn encrypt(key: &[u8], iv: &[u8], plaintext: &[u8]) -> Result<Vec<u8>> {
    let mut buf = vec![0u8; plaintext.len() + 32]; // Enough for padding
    buf[..plaintext.len()].copy_from_slice(plaintext);
    
    let ct = Aes256CbcEnc::new_from_slices(key, iv)
        .map_err(|e| anyhow!("Cipher init failed: {}", e))?
        .encrypt_padded_mut::<cbc::cipher::block_padding::Pkcs7>(&mut buf, plaintext.len())
        .map_err(|e| anyhow!("Encryption failed: {}", e))?;
    
    Ok(ct.to_vec())
}
