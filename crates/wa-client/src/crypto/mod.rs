pub mod ratchet;
pub mod session;
pub mod cbc;
pub mod envelope;
pub mod x3dh;
pub mod sender_key;
pub mod xeddsa;

pub use session::Session;
pub use envelope::SignalEnvelope;
pub use x3dh::derive_root_key;
pub use sender_key::SenderKeyRecord;
pub use ratchet::DoubleRatchet;

use hmac::{Hmac, Mac};
use sha2::Sha256;

/// Compute HMAC-SHA256 MAC for message authentication.
/// Returns the full 32-byte MAC — the caller truncates as needed (e.g., 10 bytes for Signal).
pub fn compute_mac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC init failed");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}
