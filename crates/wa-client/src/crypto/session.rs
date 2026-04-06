//! Signal session management.
//!
//! Wraps DoubleRatchet with pending-prekey bookkeeping for the initial
//! PreKeySignalMessage exchange.

use crate::crypto::ratchet::{DoubleRatchet, RootKey};
use serde::{Serialize, Deserialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct Session {
    pub remote_identity_key: [u8; 32],
    pub ratchet: DoubleRatchet,
    pub pending_prekey: Option<PendingPreKey>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PendingPreKey {
    pub prekey_id: u32,
    pub signed_prekey_id: u32,
    pub base_key: [u8; 32],
}

impl Session {
    /// Create a new session as the sender (Alice):
    /// After X3DH, use their signed prekey as the initial ratchet public key.
    pub fn new_as_sender(
        remote_identity_key: [u8; 32],
        root_key: RootKey,
        their_signed_prekey: [u8; 32],
    ) -> Self {
        Self {
            remote_identity_key,
            ratchet: DoubleRatchet::init_sender(root_key, their_signed_prekey),
            pending_prekey: None,
        }
    }

    /// Create a new session as the receiver (Bob):
    /// We hold the signed prekey private key; we wait for their first message
    /// to complete the DH ratchet.
    pub fn new_as_receiver(
        remote_identity_key: [u8; 32],
        root_key: RootKey,
        our_signed_prekey_priv: [u8; 32],
    ) -> Self {
        Self {
            remote_identity_key,
            ratchet: DoubleRatchet::init_receiver(root_key, our_signed_prekey_priv),
            pending_prekey: None,
        }
    }

    /// Encrypt plaintext. Returns (ciphertext, ratchet_pub, counter, prev_counter).
    pub fn encrypt(&mut self, plaintext: &[u8]) -> anyhow::Result<(Vec<u8>, [u8; 32], u32, u32)> {
        self.ratchet.encrypt(plaintext)
    }

    /// Decrypt a Signal message with the sender's ratchet key + counter.
    pub fn decrypt(
        &mut self,
        their_ratchet_pub: &[u8; 32],
        counter: u32,
        ciphertext: &[u8],
    ) -> anyhow::Result<Vec<u8>> {
        self.ratchet.decrypt(their_ratchet_pub, counter, ciphertext)
    }
}
