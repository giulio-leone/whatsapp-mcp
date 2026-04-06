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
    
    let mut ikm = Vec::with_capacity(32 * 4);
    // WhatsApp/Signal X3DH order: DH1 = DH(IK_A, SPK_B), DH2 = DH(EK_A, IK_B), DH3 = DH(EK_A, SPK_B)
    ikm.extend_from_slice(dh1.as_bytes());
    ikm.extend_from_slice(dh2.as_bytes());
    ikm.extend_from_slice(dh3.as_bytes());
    
    if let Some(otpk) = remote_onetime_prekey {
        let dh4 = my_ephemeral.diffie_hellman(otpk);
        ikm.extend_from_slice(dh4.as_bytes());
    }

    let hk = Hkdf::<Sha256>::new(None, &ikm);
    let mut okm = [0u8; 32];
    // Note: Signal uses some padding/prefix in HKDF? 
    // Usually it's just an empty salt and empty info or a specific string.
    hk.expand(b"WhatsApp X3DH", &mut okm).expect("HKDF expand failed");
    okm
}
