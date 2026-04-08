//! QR code generation for WhatsApp multi-device pairing.
//!
//! The QR code encodes a comma-separated string:
//! `ref,pubkey_base64,identity_base64,adv_secret_base64`
//!
//! Where:
//!  - ref: a random reference string
//!  - pubkey: Noise public key (32 bytes, base64)
//!  - identity: Identity public key (32 bytes, base64)
//!  - adv_secret: AdvSecretKey (32 bytes, random, base64)

use base64::Engine;
use rand::RngCore;

pub struct QrRef {
    pub reference: String,
    pub adv_secret: [u8; 32],
}

impl QrRef {
    pub fn generate() -> Self {
        let mut rng = rand::thread_rng();
        let mut ref_bytes = [0u8; 16];
        rng.fill_bytes(&mut ref_bytes);

        let mut adv_secret = [0u8; 32];
        rng.fill_bytes(&mut adv_secret);

        Self {
            reference: hex::encode(ref_bytes),
            adv_secret,
        }
    }

    /// Encode QR data: ref,noise_pub_b64,identity_b64,adv_secret_b64
    pub fn encode(&self, noise_pub: &[u8; 32], identity_key: &[u8; 32]) -> String {
        let engine = base64::engine::general_purpose::STANDARD;
        format!(
            "{},{},{},{}",
            self.reference,
            engine.encode(noise_pub),
            engine.encode(identity_key),
            engine.encode(self.adv_secret),
        )
    }

    /// Generate a QR code image as PNG bytes for the given data string.
    #[cfg(feature = "qr_render")]
    pub fn render_png(data: &str) -> anyhow::Result<Vec<u8>> {
        use qrcode::QrCode;
        let code = QrCode::new(data.as_bytes())?;
        let image = code.render::<image::Luma<u8>>().build();
        let mut buf = Vec::new();
        image::DynamicImage::ImageLuma8(image)
            .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)?;
        Ok(buf)
    }

    /// Render QR code as terminal-friendly Unicode block art.
    pub fn render_terminal(data: &str) -> String {
        use qrcode::{QrCode, EcLevel};
        // Use Low ECC for smallest possible QR — fits better in narrow terminals
        let code = match QrCode::with_error_correction_level(data.as_bytes(), EcLevel::L) {
            Ok(c) => c,
            Err(e) => return format!("QR encode error: {}", e),
        };

        let mut output = String::new();
        let width = code.width();
        let colors: Vec<bool> = code.into_colors().into_iter()
            .map(|c| c == qrcode::Color::Dark)
            .collect();

        // Top border
        output.push_str(&"█".repeat(width + 4));
        output.push('\n');

        for y in (0..colors.len() / width).step_by(2) {
            output.push_str("██"); // Left border
            for x in 0..width {
                let top = colors[y * width + x];
                let bottom = if (y + 1) * width + x < colors.len() {
                    colors[(y + 1) * width + x]
                } else {
                    false
                };
                match (top, bottom) {
                    (true, true) => output.push('█'),
                    (true, false) => output.push('▀'),
                    (false, true) => output.push('▄'),
                    (false, false) => output.push(' '),
                }
            }
            output.push_str("██\n"); // Right border
        }

        // Bottom border
        output.push_str(&"█".repeat(width + 4));
        output.push('\n');

        output
    }
}
