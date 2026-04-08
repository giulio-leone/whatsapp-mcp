//! XEdDSA signature scheme for Signal protocol compatibility.
//!
//! This implements the XEdDSA signature algorithm used by Signal's libsignal,
//! which signs using a Curve25519 (Montgomery) private key by converting it to
//! Edwards (Ed25519) form.
//!
//! Reference: go.mau.fi/libsignal/ecc/SignCurve25519.go

use curve25519_dalek::scalar::Scalar;
use sha2::{Digest, Sha512};

/// Sign a message using XEdDSA with a Curve25519 private key.
///
/// This exactly mirrors the Go `sign()` function in `SignCurve25519.go`:
/// 1. Convert the Curve25519 private key to an Ed25519 keypair
/// 2. Generate a deterministic nonce r using SHA-512(diversifier || privkey || msg || random)
/// 3. Compute R = r * B
/// 4. Compute S = r + SHA-512(R || A || msg) * a (mod L)
/// 5. Embed the sign bit of A into signature[63]
pub fn xeddsa_sign(private_key: &[u8; 32], message: &[u8]) -> [u8; 64] {
    // Generate 64 bytes of randomness for the nonce
    let mut random = [0u8; 64];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut random);
    xeddsa_sign_with_random(private_key, message, &random)
}

/// Sign with explicit randomness (for testing / determinism).
fn xeddsa_sign_with_random(private_key: &[u8; 32], message: &[u8], random: &[u8; 64]) -> [u8; 64] {
    // 1. Clamp the private key and compute Ed25519 public key
    let mut clamped = *private_key;
    clamped[0] &= 248;
    clamped[31] &= 127;
    clamped[31] |= 64;

    let private_scalar = Scalar::from_bytes_mod_order(clamped);
    let a_point = curve25519_dalek::constants::ED25519_BASEPOINT_POINT * private_scalar;
    let public_key = a_point.compress().to_bytes();

    // 2. Calculate r = SHA-512(diversifier || privateKey || message || random) (mod L)
    let diversifier: [u8; 32] = [
        0xFE, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
        0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
        0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
        0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
    ];

    let mut hash = Sha512::new();
    hash.update(diversifier);
    hash.update(private_key);
    hash.update(message);
    hash.update(random);
    let r_hash: [u8; 64] = hash.finalize().into();
    let r_reduced = Scalar::from_bytes_mod_order_wide(&r_hash);

    // 3. Calculate R = r * B
    let r_point = curve25519_dalek::constants::ED25519_BASEPOINT_POINT * r_reduced;
    let encoded_r = r_point.compress().to_bytes();

    // 4. Calculate S = r + SHA-512(R || A_ed || msg) * a  (mod L)
    let mut hash2 = Sha512::new();
    hash2.update(encoded_r);
    hash2.update(public_key);
    hash2.update(message);
    let hram_digest: [u8; 64] = hash2.finalize().into();
    let hram_reduced = Scalar::from_bytes_mod_order_wide(&hram_digest);

    let s_scalar = hram_reduced * private_scalar + r_reduced;
    let s_bytes = s_scalar.to_bytes();

    // 5. Assemble signature: [R (32 bytes) || S (32 bytes)]
    //    with the sign bit of the public key embedded in signature[63]
    let mut signature = [0u8; 64];
    signature[..32].copy_from_slice(&encoded_r);
    signature[32..].copy_from_slice(&s_bytes);
    signature[63] |= public_key[31] & 0x80;

    signature
}

/// Verify an XEdDSA signature against a Curve25519 (Montgomery) public key.
///
/// This mirrors the Go `verify()` function:
/// 1. Convert Montgomery x-coordinate to Edwards y-coordinate
/// 2. Move sign bit from signature[63] into the public key
/// 3. Verify as standard Ed25519
pub fn xeddsa_verify(public_key: &[u8; 32], message: &[u8], signature: &[u8; 64]) -> bool {
    use curve25519_dalek::montgomery::MontgomeryPoint;

    let mut sig = *signature;

    // Extract the sign bit from signature[63] and clear it
    let sign_bit = sig[63] & 0x80;
    sig[63] &= 0x7F;

    // Convert Montgomery public key to Edwards form
    let mont_point = MontgomeryPoint(*public_key);
    let ed_point = match mont_point.to_edwards(0) {
        Some(p) => p,
        None => return false,
    };

    // Get the compressed Edwards y-coordinate and set the sign bit
    let mut a_ed = ed_point.compress().to_bytes();
    a_ed[31] |= sign_bit;

    // Verify as Ed25519
    use ed25519_dalek::{Signature, VerifyingKey, Verifier};
    let Ok(verifying_key) = VerifyingKey::from_bytes(&a_ed) else {
        return false;
    };
    let ed_sig = Signature::from_bytes(&sig);
    verifying_key.verify(message, &ed_sig).is_ok()
}
