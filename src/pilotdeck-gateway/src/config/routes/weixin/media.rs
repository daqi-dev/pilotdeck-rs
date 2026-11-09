//! Weixin CDN media decrypt helpers (WeixinChannel.ts).
use serde_json::Value;

use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit};
use base64::Engine;

const WEIXIN_CDN_BASE_URL: &str = "https://novac2c.cdn.weixin.qq.com/c2c";

pub(crate) fn weixin_media_download_url(media_owner: &Value) -> Option<String> {
    read_string(media_owner.get("cdn_url"))
        .or_else(|| read_string(media_owner.get("url")))
        .or_else(|| extract_weixin_media_url(media_owner))
        .or_else(|| read_string(media_owner.get("cos_url")))
}

pub(crate) fn weixin_outbound_aes_key_field(aeskey: &[u8; 16]) -> String {
    let hex = aeskey
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    base64::engine::general_purpose::STANDARD.encode(hex.as_bytes())
}

fn read_string(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

pub(crate) fn extract_weixin_media_url(media_owner: &Value) -> Option<String> {
    let media = media_owner.get("media")?;
    if let Some(param) = read_string(media.get("encrypt_query_param")) {
        return Some(format!(
            "{WEIXIN_CDN_BASE_URL}/download?encrypted_query_param={}",
            url::form_urlencoded::byte_serialize(param.as_bytes()).collect::<String>()
        ));
    }
    read_string(media.get("full_url"))
        .or_else(|| read_string(media.get("download_url")))
        .or_else(|| read_string(media.get("url")))
        .or_else(|| read_string(media.get("cdn_url")))
}

pub(crate) fn decrypt_weixin_media_if_needed(
    buffer: &[u8],
    media_owner: Option<&Value>,
    image: bool,
) -> Result<Vec<u8>, String> {
    use super::http::failure;
    let is_expected = if image {
        is_known_image_buffer
    } else {
        is_likely_known_file_buffer
    };
    if is_expected(buffer) {
        return Ok(buffer.to_vec());
    }
    let Some(media_owner) = media_owner else {
        if image {
            return Err(failure("Weixin image decrypt failed").message.into());
        }
        return Ok(buffer.to_vec());
    };
    for key in extract_weixin_media_keys(media_owner) {
        for candidate in try_decrypt_weixin_media(buffer, &key) {
            if is_expected(&candidate) {
                return Ok(candidate);
            }
        }
    }
    if image {
        return Err(failure("Weixin image decrypt failed").message.into());
    }
    Ok(buffer.to_vec())
}

pub(crate) fn aes_ecb_padded_size(plaintext_size: usize) -> usize {
    ((plaintext_size + 1) + 15) / 16 * 16
}

pub(crate) fn encrypt_aes_128_ecb(plaintext: &[u8], key: &[u8; 16]) -> Vec<u8> {
    let cipher = aes::Aes128::new_from_slice(key).expect("AES-128 key");
    let mut size = aes_ecb_padded_size(plaintext.len());
    let pad = size - plaintext.len();
    let mut buf = plaintext.to_vec();
    buf.extend(std::iter::repeat_n(pad as u8, pad));
    for chunk in buf.chunks_exact_mut(16) {
        let mut block = aes::Block::default();
        block.copy_from_slice(chunk);
        cipher.encrypt_block(&mut block);
        chunk.copy_from_slice(block.as_slice());
    }
    debug_assert_eq!(buf.len(), size);
    buf
}

pub(crate) fn build_weixin_cdn_upload_url(upload: &Value, filekey: &str) -> Option<String> {
    if let Some(url) = upload
        .get("upload_full_url")
        .or_else(|| upload.get("data").and_then(|data| data.get("upload_full_url")))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|url| !url.is_empty())
    {
        return Some(url.to_owned());
    }
    let param = upload
        .get("upload_param")
        .or_else(|| upload.get("data").and_then(|data| data.get("upload_param")))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|url| !url.is_empty())?;
    Some(format!(
        "{WEIXIN_CDN_BASE_URL}/upload?encrypted_query_param={}&filekey={}",
        url::form_urlencoded::byte_serialize(param.as_bytes()).collect::<String>(),
        url::form_urlencoded::byte_serialize(filekey.as_bytes()).collect::<String>()
    ))
}

fn extract_weixin_media_keys(media_owner: &Value) -> Vec<Vec<u8>> {
    let mut keys = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut push = |key: Vec<u8>| {
        if key.len() != 16 && key.len() != 24 && key.len() != 32 {
            return;
        }
        let id = key.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
        if seen.insert(id) {
            keys.push(key);
        }
    };
    let owner_aes = read_string(media_owner.get("aeskey"));
    if let Some(owner_aes) = owner_aes.as_deref() {
        if owner_aes.len() == 32 && owner_aes.chars().all(|ch| ch.is_ascii_hexdigit()) {
            if let Some(bytes) = decode_hex(owner_aes) {
                push(bytes);
            }
        }
    }
    if let Some(media) = media_owner.get("media") {
        if let Some(media_aes) = read_string(media.get("aes_key")) {
            if let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(media_aes) {
                if decoded.len() == 16 || decoded.len() == 24 || decoded.len() == 32 {
                    push(decoded.clone());
                }
                let text = String::from_utf8_lossy(&decoded);
                if text.len() == 32 && text.chars().all(|ch| ch.is_ascii_hexdigit()) {
                    if let Some(bytes) = decode_hex(text.as_ref()) {
                        push(bytes);
                    }
                }
            }
        }
    }
    if let Some(owner_aes) = owner_aes {
        if !(owner_aes.len() == 32 && owner_aes.chars().all(|ch| ch.is_ascii_hexdigit())) {
            if let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(owner_aes) {
                if decoded.len() == 16 || decoded.len() == 24 || decoded.len() == 32 {
                    push(decoded);
                }
            }
        }
    }
    keys
}

fn try_decrypt_weixin_media(buffer: &[u8], key: &[u8]) -> Vec<Vec<u8>> {
    macro_rules! decrypt_key {
        ($ty:ty) => {{
            let Ok(cipher) = <$ty>::new_from_slice(key) else {
                return vec![];
            };
            if buffer.len() % 16 != 0 {
                return vec![];
            }
            let mut plain = Vec::with_capacity(buffer.len());
            for chunk in buffer.chunks_exact(16) {
                let mut block = aes::Block::default();
                block.copy_from_slice(chunk);
                cipher.decrypt_block(&mut block);
                plain.extend_from_slice(block.as_slice());
            }
            return vec![strip_pkcs7_padding(&plain).unwrap_or(plain)];
        }};
    }
    match key.len() {
        16 => decrypt_key!(aes::Aes128),
        24 => decrypt_key!(aes::Aes192),
        32 => decrypt_key!(aes::Aes256),
        _ => vec![],
    }
}

fn strip_pkcs7_padding(buffer: &[u8]) -> Option<Vec<u8>> {
    let pad = *buffer.last()?;
    if pad == 0 || pad > 16 || pad as usize > buffer.len() {
        return None;
    }
    if !buffer[buffer.len() - pad as usize..]
        .iter()
        .all(|byte| *byte == pad)
    {
        return None;
    }
    Some(buffer[..buffer.len() - pad as usize].to_vec())
}

pub(crate) fn is_known_image_buffer(buffer: &[u8]) -> bool {
    if buffer.len() < 12 {
        return false;
    }
    if buffer[0] == 0xff && buffer[1] == 0xd8 && buffer[2] == 0xff {
        return true;
    }
    if buffer.starts_with(&[0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]) {
        return true;
    }
    let gif = &buffer[..6.min(buffer.len())];
    if gif == b"GIF87a" || gif == b"GIF89a" {
        return true;
    }
    buffer.len() >= 12
        && &buffer[..4] == b"RIFF"
        && &buffer[8..12] == b"WEBP"
}

pub(crate) fn is_likely_known_file_buffer(buffer: &[u8]) -> bool {
    if buffer.len() < 4 {
        return false;
    }
    if is_known_image_buffer(buffer) {
        return true;
    }
    if buffer.starts_with(b"%PDF-") {
        return true;
    }
    if buffer.starts_with(&[0x50, 0x4b, 0x03, 0x04])
        || buffer.starts_with(&[0x50, 0x4b, 0x05, 0x06])
        || buffer.starts_with(&[0x50, 0x4b, 0x07, 0x08])
    {
        return true;
    }
    if buffer.starts_with(b"{\\rt") {
        return true;
    }
    looks_like_mostly_text(&buffer[..buffer.len().min(512)])
}

fn decode_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&text[index..index + 2], 16).ok())
        .collect()
}

fn looks_like_mostly_text(buffer: &[u8]) -> bool {
    if buffer.is_empty() {
        return false;
    }
    let printable = buffer.iter().filter(|byte| {
        **byte == 0x09
            || **byte == 0x0a
            || **byte == 0x0d
            || (**byte >= 0x20 && **byte <= 0x7e)
            || **byte >= 0x80
    }).count();
  buffer.iter().all(|byte| *byte != 0) && printable * 10 > buffer.len() * 9
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_media_full_url_over_cos_url() {
        let item = serde_json::json!({
            "cos_url": "https://cos.example/file",
            "media": { "full_url": "https://full.example/file" }
        });
        assert_eq!(
            weixin_media_download_url(&item).as_deref(),
            Some("https://full.example/file")
        );
    }

    #[test]
    fn prefers_cdn_url_in_download_url_helper() {
        let item = serde_json::json!({
            "cdn_url": "https://cdn.example/cdn.bin",
            "url": "https://cdn.example/url.bin"
        });
        assert_eq!(
            weixin_media_download_url(&item).as_deref(),
            Some("https://cdn.example/cdn.bin")
        );
    }

    #[test]
    fn outbound_aes_key_matches_ts_base64_hex_ascii() {
        let key = [0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef];
        let field = weixin_outbound_aes_key_field(&key);
        assert_eq!(
            field,
            base64::engine::general_purpose::STANDARD.encode(b"0123456789abcdef0123456789abcdef")
        );
    }

    #[test]
    fn image_decrypt_failure_returns_err() {
        let buffer = b"not-an-image";
        let owner = serde_json::json!({ "aeskey": "00112233445566778899aabbccddeeff" });
        assert!(decrypt_weixin_media_if_needed(buffer, Some(&owner), true).is_err());
    }

    #[test]
    fn round_trip_aes_ecb_encrypt_decrypt() {
        let key = [0u8; 16];
        let plain = b"hello-weixin";
        let encrypted = encrypt_aes_128_ecb(plain, &key);
        let decrypted = try_decrypt_weixin_media(&encrypted, &key)
            .into_iter()
            .next()
            .unwrap();
        assert_eq!(decrypted, plain);
    }
}
