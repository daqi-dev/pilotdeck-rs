//! Node `crypto` behaviour used by `FeishuChannel.decryptFeishuPayload`
//! (baseline cc7c4e6): AES-256-CBC with the SHA-256 of the UTF-8 encrypt key
//! as the key, the first 16 bytes of the payload as the IV, and PKCS#7
//! padding (`decipher.setAutoPadding(true)`).
//!
//! The base64 step is Node's `Buffer.from(value, "base64")`, not RFC 4648:
//! characters outside the alphabet are skipped, decoding stops at the first
//! `=`, missing padding is tolerated and a trailing single character is
//! dropped. The WeCom lane measured the same behaviour on v22.22.0
//! (`channel/wecom_callback/crypto.rs`); this module keeps the feishu copy
//! independent because the two envelopes differ and a shared helper would
//! couple two frozen golden fixtures.

use aes::Aes256;
use base64::{
    Engine,
    alphabet::STANDARD,
    engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig},
};
use cbc::cipher::{BlockDecryptMut, KeyIvInit, block_padding::Pkcs7};
use sha2::{Digest, Sha256};

type Aes256CbcDec = cbc::Decryptor<Aes256>;

const NODE_BASE64: GeneralPurpose = GeneralPurpose::new(
    &STANDARD,
    GeneralPurposeConfig::new()
        .with_decode_allow_trailing_bits(true)
        .with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

/// Node `Buffer.from(value, "base64")`.
fn node_base64(value: &str) -> Vec<u8> {
    let mut filtered = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '=' => break,
            '-' => filtered.push('+'),
            '_' => filtered.push('/'),
            _ if ch.is_ascii_alphanumeric() || ch == '+' || ch == '/' => filtered.push(ch),
            _ => {}
        }
    }
    if filtered.len() % 4 == 1 {
        filtered.pop();
    }
    while !filtered.len().is_multiple_of(4) {
        filtered.push('=');
    }
    NODE_BASE64.decode(filtered).unwrap_or_default()
}

/// `decryptFeishuPayload(encrypted, key)`.
///
/// The error text is the TS `Error: …` shape (`final()`'s padding error
/// carries Node's wording); callers only log it (`feishu: decrypt failed: …`)
/// and ignore the event, so the exact message never reaches the wire.
pub(crate) fn decrypt_feishu_payload(encrypted: &str, key: &str) -> Result<String, String> {
    let aes_key = Sha256::digest(key.as_bytes());
    let buf = node_base64(encrypted);
    if buf.len() < 16 {
        return Err("Error: Invalid IV length".to_owned());
    }
    let (iv, cipher_text) = buf.split_at(16);
    let mut cipher_text = cipher_text.to_vec();
    let decrypted = Aes256CbcDec::new_from_slices(&aes_key, iv)
        .map_err(|error| format!("Error: {error}"))?
        .decrypt_padded_mut::<Pkcs7>(&mut cipher_text)
        .map_err(|_| "Error: error:1C800064:Provider routines::bad decrypt".to_owned())?;
    // `Buffer.toString("utf8")`: lone surrogates cannot come from this path in
    // TS either (they are not valid UTF-8), so lossy decoding is exact.
    Ok(String::from_utf8_lossy(decrypted).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use aes::cipher::BlockEncryptMut;
    use cbc::cipher::block_padding::Pkcs7 as EncPadding;

    type Aes256CbcEnc = cbc::Encryptor<Aes256>;

    fn encrypt(plain: &str, key: &str, iv: [u8; 16]) -> String {
        let aes_key = Sha256::digest(key.as_bytes());
        let mut buffer = plain.as_bytes().to_vec();
        let message_len = buffer.len();
        buffer.resize(message_len + 16, 0);
        let encrypted = Aes256CbcEnc::new_from_slices(&aes_key, &iv)
            .expect("#1329 aes key")
            .encrypt_padded_mut::<EncPadding>(&mut buffer, message_len)
            .expect("#1329 pkcs7")
            .to_vec();
        let mut payload = iv.to_vec();
        payload.extend_from_slice(&encrypted);
        base64::engine::general_purpose::STANDARD.encode(payload)
    }

    #[test]
    fn decrypts_aes_256_cbc_with_sha256_key() {
        let iv = [7u8; 16];
        let encrypted = encrypt(r#"{"type":"url_verification","challenge":"c-1"}"#, "k3y", iv);
        assert_eq!(
            decrypt_feishu_payload(&encrypted, "k3y").expect("#1329 decrypt"),
            r#"{"type":"url_verification","challenge":"c-1"}"#
        );
    }

    #[test]
    fn rejects_short_payloads_and_bad_padding() {
        assert!(decrypt_feishu_payload("AAAA", "k").is_err());
        let iv = [1u8; 16];
        let mut payload = iv.to_vec();
        payload.extend_from_slice(&[0u8; 16]);
        let encoded = base64::engine::general_purpose::STANDARD.encode(payload);
        assert!(decrypt_feishu_payload(&encoded, "k").is_err());
    }

    #[test]
    fn base64_follows_node_buffer_semantics() {
        // Whitespace and stray characters are skipped; decoding stops at `=`.
        assert_eq!(node_base64("QU JD"), b"ABC");
        assert_eq!(node_base64("QUJD=RA=="), b"ABC");
        assert_eq!(node_base64("QUJDRA"), b"ABCD");
        assert_eq!(node_base64("QUJDRA="), b"ABCD");
    }
}
