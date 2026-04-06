//! SenderKey distribution for WhatsApp group messages.
//!
//! Group messages use a shared symmetric key (SenderKey) distributed
//! to group members via SenderKeyDistributionMessage. Each sender has
//! their own chain; the receiver advances the chain to the iteration
//! indicated in the SenderKeyMessage header, then decrypts.

use crate::crypto::ratchet::ChainKey;
use serde::{Serialize, Deserialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SenderKeyChain {
    pub chain_key: ChainKey,
    pub iteration: u32,
}

impl SenderKeyChain {
    pub fn new(key: [u8; 32], iteration: u32) -> Self {
        Self {
            chain_key: ChainKey::new(key, iteration),
            iteration,
        }
    }

    /// Advance the chain to the target iteration, returning message keys.
    pub fn advance_to(&mut self, target_iteration: u32) -> anyhow::Result<([u8; 32], [u8; 32], [u8; 16])> {
        const MAX_ADVANCE: u32 = 2000;
        if target_iteration < self.chain_key.index {
            return Err(anyhow::anyhow!(
                "SenderKey iteration {} already past (current={})",
                target_iteration,
                self.chain_key.index
            ));
        }
        let delta = target_iteration - self.chain_key.index;
        if delta > MAX_ADVANCE {
            return Err(anyhow::anyhow!("SenderKey advance too large: {} steps", delta));
        }
        // Advance to target
        while self.chain_key.index < target_iteration {
            self.chain_key = self.chain_key.get_next();
        }
        let mk = self.chain_key.get_message_keys();
        self.chain_key = self.chain_key.get_next();
        Ok((mk.cipher_key, mk.mac_key, mk.iv))
    }
}

/// Holds one or more SenderKey chains for a given (group, sender) pair.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SenderKeyRecord {
    pub chains: Vec<SenderKeyChain>,
}

impl SenderKeyRecord {
    pub fn new(key: [u8; 32], iteration: u32) -> Self {
        Self {
            chains: vec![SenderKeyChain::new(key, iteration)],
        }
    }

    /// Decrypt a SenderKey message at the given iteration.
    /// Tries each chain until one succeeds.
    pub fn decrypt(&mut self, iteration: u32, ciphertext: &[u8]) -> anyhow::Result<Vec<u8>> {
        for chain in self.chains.iter_mut().rev() {
            match chain.advance_to(iteration) {
                Ok((cipher_key, _mac_key, iv)) => {
                    return crate::crypto::cbc::decrypt(&cipher_key, &iv, ciphertext);
                }
                Err(_) => continue,
            }
        }
        Err(anyhow::anyhow!("No SenderKey chain could decrypt at iteration {}", iteration))
    }

    /// Add a new chain from a SenderKeyDistributionMessage.
    pub fn add_chain(&mut self, key: [u8; 32], iteration: u32) {
        self.chains.push(SenderKeyChain::new(key, iteration));
    }
}
