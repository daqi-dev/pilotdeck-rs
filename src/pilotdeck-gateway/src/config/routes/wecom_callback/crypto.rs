//! Node crypto AES-256-CBC/no-auto-padding and WeCom 32-byte padding contract.
use super::*;
use aes::{
    Aes256,
    cipher::{BlockDecrypt, KeyInit},
};
use base64::Engine;
use sha1::{Digest, Sha1};
pub(crate) fn signature(token: &str, timestamp: &str, nonce: &str, encrypted: &str) -> String {
    let mut fields = [token, timestamp, nonce, encrypted];
    fields.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
    format!("{:x}", Sha1::digest(fields.concat().as_bytes()))
}
fn decode_base64(text: &str) -> Result<Vec<u8>> {
    // Buffer.from(..., 'base64') accepts URL-safe alphabet and ignores spaces
    // and non-alphabet bytes. It stops at padding and accepts an unpadded tail.
    let mut normalized = String::new();
    for byte in text.bytes() {
        match byte {
            b'=' => break,
            b'-' => normalized.push('+'),
            b'_' => normalized.push('/'),
            byte if byte.is_ascii_alphanumeric() || byte == b'+' || byte == b'/' => {
                normalized.push(char::from(byte))
            }
            _ => {}
        }
    }
    if normalized.len() % 4 == 1 {
        normalized.pop();
    }
    base64::engine::general_purpose::STANDARD_NO_PAD
        .decode(normalized)
        .map_err(|_| failure("Invalid WeCom base64"))
}
pub(super) fn decrypt(key: &str, encrypted: &str, corp: &str) -> Result<String> {
    let key = decode_base64(&format!("{key}="))?;
    if key.len() != 32 {
        return Err(failure("encodingAesKey must decode to 32 bytes"));
    }
    let bytes = decode_base64(encrypted)?;
    if bytes.is_empty() || bytes.len() % 16 != 0 {
        return Err(failure("Invalid WeCom AES ciphertext length"));
    }
    let cipher = Aes256::new_from_slice(&key).map_err(|_| failure("Invalid WeCom AES key"))?;
    let mut previous = [0u8; 16];
    previous.copy_from_slice(&key[..16]);
    let mut raw = Vec::with_capacity(bytes.len());
    for chunk in bytes.chunks_exact(16) {
        let mut block = aes::Block::default();
        block.copy_from_slice(chunk);
        cipher.decrypt_block(&mut block);
        raw.extend(block.iter().zip(previous).map(|(byte, iv)| byte ^ iv));
        previous.copy_from_slice(chunk);
    }
    let pad = usize::from(*raw.last().unwrap());
    if !(1..=32).contains(&pad) {
        return Err(failure("invalid PKCS#7 padding"));
    }
    // The baseline checks the final pad byte, without checking the other bytes.
    let end = raw.len().saturating_sub(pad);
    raw.truncate(end);
    if raw.len() < 20 {
        return Err(failure("Invalid WeCom decrypted envelope"));
    }
    let length = u32::from_be_bytes(raw[16..20].try_into().unwrap()) as usize;
    let end = 20usize.saturating_add(length).min(raw.len());
    let tail = String::from_utf8_lossy(&raw[end..]);
    if !corp.is_empty() && !tail.is_empty() && tail != corp {
        return Err(failure("corpId mismatch after decrypt"));
    }
    Ok(String::from_utf8_lossy(&raw[20..end]).into_owned())
}
#[cfg(test)]
pub(super) fn encrypt(key: &str, text: &str, corp: &str) -> String {
    use aes::cipher::BlockEncrypt;
    let key = decode_base64(key).unwrap();
    let mut bytes = b"0123456789abcdef".to_vec();
    bytes.extend_from_slice(&(text.len() as u32).to_be_bytes());
    bytes.extend_from_slice(text.as_bytes());
    bytes.extend_from_slice(corp.as_bytes());
    let pad = 32 - bytes.len() % 32;
    bytes.extend(std::iter::repeat_n(pad as u8, pad));
    let cipher = Aes256::new_from_slice(&key).unwrap();
    let mut previous = [0u8; 16];
    previous.copy_from_slice(&key[..16]);
    for chunk in bytes.chunks_exact_mut(16) {
        for (byte, iv) in chunk.iter_mut().zip(previous) {
            *byte ^= iv;
        }
        let mut block = aes::Block::default();
        block.copy_from_slice(chunk);
        cipher.encrypt_block(&mut block);
        chunk.copy_from_slice(&block);
        previous.copy_from_slice(chunk);
    }
    base64::engine::general_purpose::STANDARD.encode(bytes)
}
