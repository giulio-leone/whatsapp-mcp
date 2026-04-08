use x25519_dalek::{StaticSecret, PublicKey};
use sha2::Sha256;
use hkdf::Hkdf;
use anyhow::{Result, anyhow};

pub fn derive_root_key(
    my_identity: &StaticSecret,
    my_ephemeral: &StaticSecret,
    remote_identity: &PublicKey,
    remote_signed_prekey: &PublicKey,
    remote_onetime_prekey: Option<&PublicKey>,
) -> [u8; 32] {
    let dh1 = my_identity.diffie_hellman(remote_signed_prekey);
    let dh2 = my_ephemeral.diffie_hellman(remote_identity);
    let dh3 = my_ephemeral.diffie_hellman(remote_signed_prekey);
    
    // Signal X3DH: prepend 32 bytes of 0xFF (discontinuity bytes) per spec
    let mut ikm = Vec::with_capacity(32 + 32 * 4);
    ikm.extend_from_slice(&[0xFF; 32]);
    ikm.extend_from_slice(dh1.as_bytes());
    ikm.extend_from_slice(dh2.as_bytes());
    ikm.extend_from_slice(dh3.as_bytes());
    
    if let Some(otpk) = remote_onetime_prekey {
        let dh4 = my_ephemeral.diffie_hellman(otpk);
        ikm.extend_from_slice(dh4.as_bytes());
    }

    // Signal X3DH spec: salt = 32 zero bytes, info = "WhisperText"
    let salt = [0u8; 32];
    let hk = Hkdf::<Sha256>::new(Some(&salt), &ikm);
    let mut okm = [0u8; 32];
    hk.expand(b"WhisperText", &mut okm).expect("HKDF expand failed");
    okm
}
