//! TOTP two-factor authentication.
//!
//! Compatibility note: TOTP secrets are stored in the `users` table encrypted
//! with **PyFernet**. This module implements the Fernet token format exactly as
//! `cryptography.fernet` writes it (AES-128-CBC + HMAC-SHA256, base64url) so the
//! Rust service can decrypt secrets already present in the database and write
//! new ones that the legacy tooling can also read.

use aes::cipher::{block_padding::Pkcs7, BlockDecryptMut, BlockEncryptMut, InnerIvInit};
use base64::engine::general_purpose::{STANDARD, URL_SAFE, URL_SAFE_NO_PAD};
use base64::Engine;
use data_encoding::{BASE32, BASE32_NOPAD};
use hmac::{Hmac, Mac};
use rand::RngCore;
use sha1::Sha1;
use sha2::Sha256;
use std::time::{SystemTime, UNIX_EPOCH};

type Aes128Enc = cbc::Encryptor<aes::Aes128>;
type Aes128Dec = cbc::Decryptor<aes::Aes128>;
type HmacSha256 = Hmac<Sha256>;

const FERNET_VERSION: u8 = 0x80;
const TOTP_PERIOD: u64 = 30;
const TOTP_DIGITS: u32 = 6;

/// Parse a 32-byte Fernet key. Fernet uses URL-safe base64, but accepting the
/// standard alphabet here keeps existing local deployments working when their
/// secret manager emitted `+` or `/` while preserving the same decoded key.
fn parse_fernet_key(key_b64: &str) -> Result<[u8; 32], String> {
    // Normalise: PyFernet keys may or may not carry padding.
    let stripped = key_b64.trim().trim_end_matches('=');
    let padded = match stripped.len() % 4 {
        2 => format!("{}==", stripped),
        3 => format!("{}=", stripped),
        _ => stripped.to_string(),
    };
    let bytes = URL_SAFE
        .decode(padded.as_bytes())
        .or_else(|_| STANDARD.decode(padded.as_bytes()))
        .map_err(|_| "must be base64-encoded".to_string())?;
    bytes
        .try_into()
        .map_err(|_| "must decode to exactly 32 bytes".to_string())
}
pub fn validate_fernet_key(key_b64: &str) -> Result<(), String> {
    parse_fernet_key(key_b64).map(|_| ())
}

/// Encrypt `plaintext` with a Fernet key, returning the base64url token.
pub fn fernet_encrypt(key_b64: &str, plaintext: &str) -> Result<String, String> {
    let key = parse_fernet_key(key_b64)?;
    let signing_key = &key[0..16];
    let encryption_key = &key[16..32];

    let mut iv = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut iv);
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let cipher = <aes::Aes128 as aes::cipher::KeyInit>::new_from_slice(encryption_key).unwrap();
    let encryptor = Aes128Enc::inner_iv_slice_init(cipher, &iv).unwrap();
    let ciphertext = encryptor
        .encrypt_padded_vec_mut::<Pkcs7>(plaintext.as_bytes())
        .to_vec();

    // associated_data = version || timestamp(8) || iv || ciphertext
    let mut associated = Vec::with_capacity(1 + 8 + 16 + ciphertext.len());
    associated.push(FERNET_VERSION);
    associated.extend_from_slice(&timestamp.to_be_bytes());
    associated.extend_from_slice(&iv);
    associated.extend_from_slice(&ciphertext);

    let mut mac = HmacSha256::new_from_slice(signing_key).unwrap();
    mac.update(&associated);
    let full_mac = mac.finalize().into_bytes();
    associated.extend_from_slice(&full_mac[..16]);

    Ok(URL_SAFE_NO_PAD.encode(associated))
}

/// Decrypt a Fernet token produced by `fernet_encrypt` or PyFernet.
pub fn fernet_decrypt(key_b64: &str, token: &str) -> Option<String> {
    let key = parse_fernet_key(key_b64).ok()?;
    let signing_key = &key[0..16];
    let encryption_key = &key[16..32];

    let data = URL_SAFE_NO_PAD.decode(token.trim()).ok()?;
    if data.len() < 1 + 8 + 16 + 16 {
        return None;
    }
    if data[0] != FERNET_VERSION {
        return None;
    }

    let iv = &data[9..25];
    let ciphertext_and_mac = &data[25..];
    let mac = &ciphertext_and_mac[ciphertext_and_mac.len() - 16..];
    let ciphertext = &ciphertext_and_mac[..ciphertext_and_mac.len() - 16];

    // Verify HMAC over version || timestamp || iv || ciphertext
    let mut mac_check = HmacSha256::new_from_slice(signing_key).unwrap();
    mac_check.update(&data[..25]);
    mac_check.update(ciphertext);
    let expected = mac_check.finalize().into_bytes();
    if expected[..16] != *mac {
        return None;
    }

    let cipher = <aes::Aes128 as aes::cipher::KeyInit>::new_from_slice(encryption_key).unwrap();
    let decryptor = Aes128Dec::inner_iv_slice_init(cipher, iv).unwrap();
    let plaintext = decryptor.decrypt_padded_vec_mut::<Pkcs7>(ciphertext).ok()?;
    String::from_utf8(plaintext).ok()
}

/// Generate a new base32 TOTP secret (20 random bytes, 32 chars).
pub fn generate_secret() -> String {
    let mut bytes = [0u8; 20];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    BASE32_NOPAD.encode(&bytes)
}

/// Compute the 6-digit TOTP code for `secret` at a given unix timestamp.
pub fn totp_code(secret_b32: &str, unix_ts: u64) -> Option<String> {
    let key = decode_base32(secret_b32)?;
    let counter = (unix_ts / TOTP_PERIOD) as u128;
    let mut counter_bytes = [0u8; 8];
    counter_bytes.copy_from_slice(&counter.to_be_bytes()[8..]);

    let mut mac = Hmac::<Sha1>::new_from_slice(&key).ok()?;
    mac.update(&counter_bytes);
    let result = mac.finalize().into_bytes();

    let offset = (result[19] & 0x0f) as usize;
    let binary = ((result[offset] as u32 & 0x7f) << 24)
        | ((result[offset + 1] as u32) << 16)
        | ((result[offset + 2] as u32) << 8)
        | (result[offset + 3] as u32);
    let otp = binary % 10u32.pow(TOTP_DIGITS);
    Some(format!("{:06}", otp))
}

/// Verify a 6-digit code against a base32 secret, allowing ±1 time step.
pub fn verify_totp(secret_b32: &str, code: &str, unix_ts: u64) -> bool {
    matching_totp_step(secret_b32, code, unix_ts).is_some()
}
pub fn matching_totp_step(secret_b32: &str, code: &str, unix_ts: u64) -> Option<i64> {
    let code = code.trim();
    if code.len() != TOTP_DIGITS as usize {
        return None;
    }
    for step in [
        unix_ts / TOTP_PERIOD,
        (unix_ts / TOTP_PERIOD).saturating_sub(1),
        (unix_ts / TOTP_PERIOD).saturating_add(1),
    ] {
        let ts = step * TOTP_PERIOD;
        if let Some(expected) = totp_code(secret_b32, ts) {
            if expected == code {
                return i64::try_from(step).ok();
            }
        }
    }
    None
}

/// Decode a base32 secret, tolerating padded and unpadded forms.
fn decode_base32(secret: &str) -> Option<Vec<u8>> {
    let secret = secret.trim();
    BASE32_NOPAD
        .decode(secret.as_bytes())
        .or_else(|_| BASE32.decode(secret.as_bytes()))
        .ok()
}

/// Build an `otpauth://` provisioning URI.
pub fn otpauth_uri(issuer: &str, account: &str, secret_b32: &str) -> String {
    format!(
        "otpauth://totp/{}:{}?secret={}&issuer={}&digits=6&period=30",
        urlencoding(issuer),
        urlencoding(account),
        secret_b32,
        urlencoding(issuer),
    )
}

/// Render an `otpauth://` URI as an SVG string for QR display.
pub fn qr_svg(uri: &str) -> String {
    use qrcode::render::svg;
    use qrcode::QrCode;
    QrCode::new(uri.as_bytes())
        .expect("qr code")
        .render::<svg::Color>()
        .build()
}

fn urlencoding(s: &str) -> String {
    s.replace(' ', "%20")
        .replace(':', "%3A")
        .replace('&', "%26")
        .replace('?', "%3F")
        .replace('/', "%2F")
}
