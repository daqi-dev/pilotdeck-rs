//! Node `crypto` behaviour used by `WeComCallbackChannel.ts:18-44`:
//! SHA-1 signatures and the WeCom AES-256-CBC / PKCS#7 envelope.

use aes::Aes256;
use base64::{
    Engine,
    alphabet::STANDARD,
    engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig},
};
use cbc::cipher::{BlockDecryptMut, KeyIvInit, block_padding::NoPadding};
use sha1::{Digest, Sha1};

type Aes256CbcDec = cbc::Decryptor<Aes256>;

/// Node's base64 decoder in `Buffer.from(value, "base64")`: it accepts a last
/// quartet whose trailing bits are not zero (`AB==` -> `00`, Node v22.22.0
/// probe) and is indifferent about padding. The stock `STANDARD` engine
/// rejects both, which turned a valid baseline message into an empty buffer
/// and then into a 500. Same configuration as `upload::gateway` uses for the
/// same Node behaviour.
const NODE_BASE64: GeneralPurpose = GeneralPurpose::new(
    &STANDARD,
    GeneralPurposeConfig::new()
        .with_decode_allow_trailing_bits(true)
        .with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

/// `sha1Hex(s)` — lowercase hex, as Node `crypto.createHash("sha1")`.
pub(crate) fn sha1_hex(text: &str) -> String {
    let mut hasher = Sha1::new();
    hasher.update(text.as_bytes());
    let digest = hasher.finalize();
    let mut out = String::with_capacity(40);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// `verifyMsgSignature`: `[token, timestamp, nonce, msgEncrypt].sort().join("")`.
///
/// JavaScript's default `Array.prototype.sort` compares UTF-16 code units, so
/// the four values are sorted on their code-unit sequences instead of Rust's
/// UTF-8 byte order (the two differ for supplementary code points above
/// U+E000). Inputs are untrusted query/Xml values; all four are converted to
/// UTF-16 exactly.
pub(crate) fn verify_msg_signature(
    token: &str,
    timestamp: &str,
    nonce: &str,
    msg_encrypt: &str,
    signature: &str,
) -> bool {
    let mut parts: Vec<Vec<u16>> = [token, timestamp, nonce, msg_encrypt]
        .into_iter()
        .map(|value| value.encode_utf16().collect())
        .collect();
    parts.sort();
    let joined: Vec<u16> = parts.into_iter().flatten().collect();
    let joined = String::from_utf16_lossy(&joined);
    // `sha1Hex(sorted) === sig` — a plain JavaScript string comparison.
    sha1_hex(&joined) == signature
}

/// Node `Buffer.from(value, "base64")`: ignore characters outside the base64
/// alphabet (the base64url spelling `-`/`_` included, which Node accepts and
/// maps to 62/63), tolerate missing padding, and ignore a trailing single
/// quartet character. `ciphertextB64`/the 43-character `encodingAesKey` both
/// arrive this way.
fn node_base64(value: &str) -> Vec<u8> {
    let mut filtered = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '-' => filtered.push('+'),
            '_' => filtered.push('/'),
            _ if ch.is_ascii_alphanumeric() || ch == '+' || ch == '/' => filtered.push(ch),
            _ => {}
        }
    }
    // A single leftover character cannot form a byte; Node drops it.
    if filtered.len() % 4 == 1 {
        filtered.pop();
    }
    while !filtered.len().is_multiple_of(4) {
        filtered.push('=');
    }
    NODE_BASE64.decode(filtered).unwrap_or_default()
}

/// A decrypt failure as JavaScript would print it (`Error: …` / `RangeError: …`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DecryptError(pub String);
impl std::fmt::Display for DecryptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl DecryptError {
    fn error(message: &str) -> Self {
        Self(format!("Error: {message}"))
    }
    fn range(message: String) -> Self {
        Self(format!("RangeError: {message}"))
    }
}

/// `decryptWxMessage(encodingAesKeyB43, ciphertextB64, expectCorpId?)`
/// (`WeComCallbackChannel.ts:27-44`).
///
/// Mirrors the TS shape exactly, including its unverified padding bytes: the
/// last byte is the pad length, `> 32 || < 1` throws, and the slice is clamped
/// like `Buffer.subarray`.
pub(crate) fn decrypt_wx_message(
    encoding_aes_key: &str,
    ciphertext_b64: &str,
    expect_corp_id: Option<&str>,
) -> Result<String, DecryptError> {
    let key = node_base64(&format!("{encoding_aes_key}="));
    if key.len() != 32 {
        return Err(DecryptError::error(
            "encodingAesKey must decode to 32 bytes",
        ));
    }
    let ciphertext = node_base64(ciphertext_b64);
    if ciphertext.is_empty() {
        // Node: the empty `raw` skips the pad check (undefined), then
        // `readUInt32BE(0)` throws.
        return Err(DecryptError::range(
            "Attempt to access memory outside buffer bounds".into(),
        ));
    }
    if !ciphertext.len().is_multiple_of(16) {
        return Err(DecryptError::error(
            "error:1C80006B:Provider routines::wrong final block length",
        ));
    }
    let iv = &key[..16];
    let mut raw = ciphertext.clone();
    let decrypted = Aes256CbcDec::new(key.as_slice().into(), iv.into())
        .decrypt_padded_mut::<NoPadding>(&mut raw)
        .map_err(|_| {
            DecryptError::error("error:1C80006B:Provider routines::wrong final block length")
        })?;
    let raw = decrypted.to_vec();
    let pad = raw[raw.len() - 1] as usize;
    if !(1..=32).contains(&pad) {
        return Err(DecryptError::error("invalid PKCS#7 padding"));
    }
    // `raw.subarray(0, raw.length - pad)`: a negative end clamps to 0.
    let end = raw.len().saturating_sub(pad);
    // `content = raw.subarray(16)`: the 16-byte random prefix is skipped.
    let content = &raw[16.min(end)..end];
    if content.len() < 4 {
        return Err(DecryptError::range(
            "Attempt to access memory outside buffer bounds".into(),
        ));
    }
    let xml_len = u32::from_be_bytes([content[0], content[1], content[2], content[3]]) as usize;
    let body_start = 4usize;
    let body_end = (4 + xml_len).min(content.len());
    let tail_start = body_end;
    let tail = String::from_utf8_lossy(&content[tail_start..]);
    if let Some(corp_id) = expect_corp_id
        && !corp_id.is_empty()
        && !tail.is_empty()
        && tail != corp_id
    {
        return Err(DecryptError::error("corpId mismatch after decrypt"));
    }
    Ok(String::from_utf8_lossy(&content[body_start..body_end]).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_sorts_code_units_and_matches_node_hex() {
        // Baseline TS produces the same digest for this shape; the digest is
        // also pinned by the real oracle fixture.
        assert_eq!(sha1_hex("abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert!(verify_msg_signature(
            "tok",
            "123",
            "nonce",
            "cipher",
            &sha1_hex("123ciphernoncetok")
        ));
        assert!(!verify_msg_signature(
            "tok", "123", "nonce", "cipher", "nope"
        ));
    }

    #[test]
    fn base64_reader_matches_node_leniency() {
        assert_eq!(node_base64("YQ=="), b"a");
        assert_eq!(node_base64("YQ"), b"a");
        assert_eq!(node_base64("Y!Q"), b"a");
        assert_eq!(node_base64("Y"), Vec::<u8>::new());
        // Node v22.22.0 probes (node 22.22.0/ICU 78.2, 2026-11-06):
        // `Buffer.from('AB==','base64')` -> 00, `'AAAB'` -> 000001,
        // `'-_8='` -> fbff (base64url alphabet), `'A$B='` -> 00,
        // `'AAAAA'` -> 000000 (the trailing single character is dropped).
        assert_eq!(node_base64("AB=="), vec![0x00]);
        assert_eq!(node_base64("AAAB"), vec![0x00, 0x00, 0x01]);
        assert_eq!(node_base64("-_8="), vec![0xfb, 0xff]);
        assert_eq!(node_base64("+/8="), vec![0xfb, 0xff]);
        assert_eq!(node_base64("A$B="), vec![0x00]);
        assert_eq!(node_base64("AAAAA"), vec![0x00, 0x00, 0x00]);
    }
}
