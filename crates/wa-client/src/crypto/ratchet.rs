//! Signal Double Ratchet implementation.
//!
//! Implements the full symmetric + DH ratchet per the Signal specification:
//! - ChainKey → MessageKeys via HMAC-SHA256 + HKDF expansion
//! - RootKey + DH → next (RootKey, ChainKey) via HKDF
//! - DoubleRatchet wraps the full sending/receiving state.

use hmac::{Hmac, Mac};
use sha2::Sha256;
use hkdf::Hkdf;
use serde::{Serialize, Deserialize};
use std::collections::HashMap;
use x25519_dalek::{StaticSecret, PublicKey};

type HmacSha256 = Hmac<Sha256>;

pub const CHAIN_KEY_SEED: &[u8] = &[0x02];
pub const MESSAGE_KEY_SEED: &[u8] = &[0x01];

// ─── MessageKeys ────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageKeys {
    pub cipher_key: [u8; 32],
    pub mac_key: [u8; 32],
    pub iv: [u8; 16],
    pub index: u32,
}

// ─── ChainKey ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChainKey {
    pub key: [u8; 32],
    pub index: u32,
}

impl ChainKey {
    pub fn new(key: [u8; 32], index: u32) -> Self {
        Self { key, index }
    }

    pub fn get_next(&self) -> Self {
        let mut hmac = HmacSha256::new_from_slice(&self.key).expect("HMAC-SHA256 Init failed");
        hmac.update(CHAIN_KEY_SEED);
        let next_key = hmac.finalize().into_bytes();
        let mut key = [0u8; 32];
        key.copy_from_slice(&next_key);
        Self {
            key,
            index: self.index + 1,
        }
    }

    pub fn get_message_keys(&self) -> MessageKeys {
        let mut hmac = HmacSha256::new_from_slice(&self.key).expect("HMAC-SHA256 Init failed");
        hmac.update(MESSAGE_KEY_SEED);
        let seed = hmac.finalize().into_bytes();

        let hk = Hkdf::<Sha256>::new(None, &seed);
        let mut cipher_key = [0u8; 32];
        let mut mac_key = [0u8; 32];
        let mut iv = [0u8; 16];

        let mut info = [0u8; 80];
        hk.expand(b"WhisperMessageKeys", &mut info).expect("HKDF expand failed");

        cipher_key.copy_from_slice(&info[0..32]);
        mac_key.copy_from_slice(&info[32..64]);
        iv.copy_from_slice(&info[64..80]);

        MessageKeys {
            cipher_key,
            mac_key,
            iv,
            index: self.index,
        }
    }
}

// ─── RootKey ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RootKey {
    pub key: [u8; 32],
}

impl RootKey {
    pub fn new(key: [u8; 32]) -> Self {
        Self { key }
    }

    /// Perform a single DH ratchet step: RootKey + dh_result → (new RootKey, new ChainKey).
    pub fn create_chain(&self, dh_result: &[u8; 32]) -> (RootKey, ChainKey) {
        let hk = Hkdf::<Sha256>::new(Some(&self.key), dh_result);
        let mut okm = [0u8; 64];
        hk.expand(b"WhisperRatchet", &mut okm).expect("HKDF expand failed");

        let mut next_root = [0u8; 32];
        let mut next_chain = [0u8; 32];
        next_root.copy_from_slice(&okm[0..32]);
        next_chain.copy_from_slice(&okm[32..64]);

        (RootKey::new(next_root), ChainKey::new(next_chain, 0))
    }
}

// ─── Double Ratchet ─────────────────────────────────────────────────

/// Full Double Ratchet state for a single session with one remote party.
#[derive(Debug, Serialize, Deserialize)]
pub struct DoubleRatchet {
    pub root_key: RootKey,

    /// Our current DH ratchet keypair (private + public).
    pub our_ratchet_priv: [u8; 32],
    pub our_ratchet_pub: [u8; 32],

    /// Their current ratchet public key.
    pub their_ratchet_pub: Option<[u8; 32]>,

    /// Sending chain (advances each time we encrypt).
    pub sending_chain: Option<ChainKey>,

    /// Receiving chains keyed by their ratchet public key.
    /// We keep old chains to decrypt out-of-order messages.
    pub receiving_chains: HashMap<[u8; 32], ChainKey>,

    /// Skipped message keys for out-of-order decryption.
    /// Maps (ratchet_pub, chain_index) → MessageKeys.
    pub skipped_keys: HashMap<([u8; 32], u32), MessageKeys>,

    /// Number of messages sent in the previous sending chain (for header).
    pub previous_counter: u32,
}

impl DoubleRatchet {
    /// Initialize as the sender (Alice) after X3DH.
    ///
    /// - `root_key`: derived from X3DH
    /// - `their_ratchet_pub`: Bob's signed prekey (used as initial ratchet key)
    pub fn init_sender(root_key: RootKey, their_ratchet_pub: [u8; 32]) -> Self {
        let our_priv = StaticSecret::random_from_rng(rand::thread_rng());
        let our_pub = PublicKey::from(&our_priv);

        // Perform the first DH ratchet step to derive the sending chain.
        let public = PublicKey::from(their_ratchet_pub);
        let shared = our_priv.diffie_hellman(&public);
        let (new_root, sending_chain) = root_key.create_chain(shared.as_bytes());

        Self {
            root_key: new_root,
            our_ratchet_priv: our_priv.to_bytes(),
            our_ratchet_pub: *our_pub.as_bytes(),
            their_ratchet_pub: Some(their_ratchet_pub),
            sending_chain: Some(sending_chain),
            receiving_chains: HashMap::new(),
            skipped_keys: HashMap::new(),
            previous_counter: 0,
        }
    }

    /// Initialize as the receiver (Bob) after X3DH.
    ///
    /// - `root_key`: derived from X3DH
    /// - `our_signed_prekey`: the signed prekey that Alice used
    pub fn init_receiver(root_key: RootKey, our_signed_prekey_priv: [u8; 32]) -> Self {
        let our_pub = PublicKey::from(&StaticSecret::from(our_signed_prekey_priv));
        Self {
            root_key,
            our_ratchet_priv: our_signed_prekey_priv,
            our_ratchet_pub: *our_pub.as_bytes(),
            their_ratchet_pub: None,
            sending_chain: None,
            receiving_chains: HashMap::new(),
            skipped_keys: HashMap::new(),
            previous_counter: 0,
        }
    }

    /// Encrypt a plaintext message. Returns (ciphertext, our_ratchet_pub, counter, previous_counter).
    pub fn encrypt(&mut self, plaintext: &[u8]) -> anyhow::Result<(Vec<u8>, [u8; 32], u32, u32)> {
        let chain = self.sending_chain.as_mut()
            .ok_or_else(|| anyhow::anyhow!("No sending chain — ratchet not initialized"))?;

        let msg_keys = chain.get_message_keys();
        let counter = chain.index;
        *chain = chain.get_next();

        let ciphertext = crate::crypto::cbc::encrypt(
            &msg_keys.cipher_key,
            &msg_keys.iv,
            plaintext,
        )?;

        Ok((ciphertext, self.our_ratchet_pub, counter, self.previous_counter))
    }

    /// Decrypt a message given the sender's ratchet public key, counter, and ciphertext.
    pub fn decrypt(
        &mut self,
        their_ratchet_pub: &[u8; 32],
        counter: u32,
        ciphertext: &[u8],
    ) -> anyhow::Result<Vec<u8>> {
        // 1. Check skipped keys first
        if let Some(mk) = self.skipped_keys.remove(&(*their_ratchet_pub, counter)) {
            return crate::crypto::cbc::decrypt(&mk.cipher_key, &mk.iv, ciphertext);
        }

        // 2. If this is a new ratchet key, perform a DH ratchet step
        let need_ratchet = self.their_ratchet_pub.as_ref() != Some(their_ratchet_pub);
        if need_ratchet {
            // Skip any remaining message keys in the current receiving chain
            self.skip_message_keys(their_ratchet_pub, 0)?;

            // DH ratchet step: derive receiving chain
            let our_priv = StaticSecret::from(self.our_ratchet_priv);
            let their_pub = PublicKey::from(*their_ratchet_pub);
            let shared = our_priv.diffie_hellman(&their_pub);
            let (new_root, receiving_chain) = self.root_key.create_chain(shared.as_bytes());

            self.root_key = new_root;
            self.their_ratchet_pub = Some(*their_ratchet_pub);
            self.receiving_chains.insert(*their_ratchet_pub, receiving_chain);

            // Generate new sending keypair + chain
            if let Some(ref old_chain) = self.sending_chain {
                self.previous_counter = old_chain.index;
            }
            let new_priv = StaticSecret::random_from_rng(rand::thread_rng());
            let new_pub = PublicKey::from(&new_priv);
            let shared2 = new_priv.diffie_hellman(&their_pub);
            let (new_root2, sending_chain) = self.root_key.create_chain(shared2.as_bytes());

            self.root_key = new_root2;
            self.our_ratchet_priv = new_priv.to_bytes();
            self.our_ratchet_pub = *new_pub.as_bytes();
            self.sending_chain = Some(sending_chain);
        }

        // 3. Skip message keys up to the target counter
        self.skip_message_keys(their_ratchet_pub, counter)?;

        // 4. Derive the message key at the target counter
        let chain = self.receiving_chains.get_mut(their_ratchet_pub)
            .ok_or_else(|| anyhow::anyhow!("No receiving chain for ratchet key"))?;

        let mk = chain.get_message_keys();
        *chain = chain.get_next();

        crate::crypto::cbc::decrypt(&mk.cipher_key, &mk.iv, ciphertext)
    }

    /// Skip and store message keys for out-of-order decryption.
    fn skip_message_keys(&mut self, their_pub: &[u8; 32], until: u32) -> anyhow::Result<()> {
        const MAX_SKIP: u32 = 1000; // Prevent DoS
        if let Some(chain) = self.receiving_chains.get_mut(their_pub) {
            if until.saturating_sub(chain.index) > MAX_SKIP {
                return Err(anyhow::anyhow!("Too many skipped message keys (DoS protection)"));
            }
            while chain.index < until {
                let mk = chain.get_message_keys();
                self.skipped_keys.insert((*their_pub, mk.index), mk);
                *chain = chain.get_next();
            }
        }
        Ok(())
    }
}
