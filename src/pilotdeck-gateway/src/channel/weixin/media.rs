//! #1823 weixin inbound media: candidate AES-ECB decryption, plaintext
//! detection, and CDN download.
//!
//! Ported from the baseline `WeixinChannel.ts` media helpers (`:631-655`,
//! `:1154-1312`): `decryptWeixinMediaIfNeeded` / `decryptWeixinImageOrThrow` /
//! `decryptWeixinFile`, `extractWeixinMediaKeys`, the two diagnostics
//! summaries, and `extractWeixinMediaUrl`. Download behaviour mirrors the
//! `ImAttachmentStore.saveFromUrl` fetch the baseline inbound path uses.
//!
//! Scope (pr2b-2): this module only produces bytes. The inbound item pipeline
//! (`pr2b-1` #1822) owns when it is called; `pr2b-3` #1824 owns the reverse
//! (encrypt + upload) direction. Nothing here is wired into
//! `channel/protocol.rs`, `channel/mod.rs`, `channels.rs`, or `assembly.rs`.
//!
//! Two Node details drive the shape of this port and are pinned by the
//! `weixin-channel` oracle (media cases appended by this unit):
//! * `Buffer.from(value, "base64")` skips characters outside the base64
//!   alphabet, accepts the URL-safe `-`/`_` aliases, keeps going after them,
//!   and **stops at the first `=`** (Node v22.22.0 probe).
//! * `Buffer.from(value, "hex")` stops at the first non-hex pair and ignores a
//!   trailing odd character.
//!
//! The stock `base64` crate rejects both inputs, so the decoder below is an
//! explicit scanner rather than an engine configuration.
#![cfg_attr(not(test), allow(dead_code))]

use aes::{
    Aes128, Aes192, Aes256,
    cipher::{BlockDecrypt, KeyInit, generic_array::GenericArray},
};
use bytes::Bytes;
use http_body_util::Full;
use hyper::{HeaderMap, Method, header};
use hyper_util::{
    client::legacy::{Client, connect::HttpConnector},
    rt::TokioExecutor,
};
use pilotdeck_protocol::JsValue;
use std::{collections::HashSet, time::Duration};
use tokio::time::timeout;

use super::WeixinClientError;
use crate::fetch_redirect::{BodyError, follow, read_body};
use crate::metadata::js_trim;

/// `WeixinChannel.ts:49` `WEIXIN_CDN_BASE_URL`.
pub const WEIXIN_CDN_BASE_URL: &str = "https://novac2c.cdn.weixin.qq.com/c2c";
/// `ImAttachmentStore.ts` `DEFAULT_MAX_BYTES`.
pub const WEIXIN_ATTACHMENT_MAX_BYTES: usize = 50 * 1024 * 1024;
/// `ImAttachmentStore.ts` `DEFAULT_FETCH_TIMEOUT_MS`.
const WEIXIN_CDN_FETCH_TIMEOUT: Duration = Duration::from_secs(60);
const AES_BLOCK: usize = 16;

/// `isKnownImageBuffer`: JPEG / PNG / GIF87a / GIF89a / WebP magic.
pub fn is_known_image_buffer(buffer: &[u8]) -> bool {
    if buffer.len() < 12 {
        return false;
    }
    if buffer[0] == 0xff && buffer[1] == 0xd8 && buffer[2] == 0xff {
        return true;
    }
    if buffer[..8] == [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a] {
        return true;
    }
    let gif = &buffer[..6];
    if gif == b"GIF87a" || gif == b"GIF89a" {
        return true;
    }
    &buffer[..4] == b"RIFF" && &buffer[8..12] == b"WEBP"
}

/// `isLikelyKnownFileBuffer`: images plus PDF / ZIP (three signatures) / RTF,
/// falling back to "mostly printable text" over the first 512 bytes.
pub fn is_likely_known_file_buffer(buffer: &[u8]) -> bool {
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

fn looks_like_mostly_text(buffer: &[u8]) -> bool {
    if buffer.is_empty() {
        return false;
    }
    let mut printable = 0usize;
    for &byte in buffer {
        if byte == 0 {
            return false;
        }
        if byte == 0x09
            || byte == 0x0a
            || byte == 0x0d
            || (0x20..=0x7e).contains(&byte)
            || byte >= 0x80
        {
            printable += 1;
        }
    }
    printable as f64 / buffer.len() as f64 > 0.9
}

/// `decryptWeixinMediaIfNeeded(buffer, mediaOwner)` with the default
/// `isKnownImageBuffer` predicate. Returns the input unchanged when it is
/// already the expected format or when no candidate key produces one.
pub fn decrypt_weixin_media_if_needed(buffer: &[u8], media_owner: &JsValue) -> Vec<u8> {
    match try_decrypt_weixin_media_if_needed(buffer, media_owner, is_known_image_buffer) {
        Some(decrypted) => decrypted,
        None => buffer.to_vec(),
    }
}

/// `decryptWeixinMediaIfNeeded(buffer, mediaOwner, isExpectedBuffer)`.
/// `Some` means a candidate decryption was adopted (TS `result !== buffer`).
pub fn try_decrypt_weixin_media_if_needed(
    buffer: &[u8],
    media_owner: &JsValue,
    is_expected: impl Fn(&[u8]) -> bool,
) -> Option<Vec<u8>> {
    if is_expected(buffer) {
        return None;
    }
    for key in extract_weixin_media_keys(media_owner) {
        for deciphered in try_decrypt_weixin_media(buffer, &key) {
            if is_expected(&deciphered) {
                return Some(deciphered);
            }
        }
    }
    None
}

/// `decryptWeixinImageOrThrow`: the image path fails when the result is still
/// not a known image; the message is the baseline's verbatim Chinese text.
pub fn decrypt_weixin_image_or_throw(
    buffer: &[u8],
    image: &JsValue,
) -> Result<Vec<u8>, WeixinClientError> {
    let result = decrypt_weixin_media_if_needed(buffer, image);
    if !is_known_image_buffer(&result) {
        return Err(WeixinClientError::error(format!(
            "微信图片解密后仍不是可识别图片（{} {}）",
            summarize_weixin_media_key_shape(image),
            summarize_weixin_decrypt_candidates(buffer, image)
        )));
    }
    Ok(result)
}

/// `decryptWeixinFile`: file attachments never throw, they fall back to the
/// downloaded bytes.
pub fn decrypt_weixin_file(buffer: &[u8], file: &JsValue) -> Vec<u8> {
    try_decrypt_weixin_media_if_needed(buffer, file, is_likely_known_file_buffer)
        .unwrap_or_else(|| buffer.to_vec())
}

/// `summarizeWeixinMediaKeyShape`: `keyShape=ownerAesKeyLen=…,…`.
pub fn summarize_weixin_media_key_shape(media_owner: &JsValue) -> String {
    let Some(owner) = media_owner.as_object() else {
        return "keyShape=none".to_owned();
    };
    let media = owner.get("media").and_then(JsValue::as_object);
    let owner_aes_key = read_string(owner.get("aeskey"));
    let media_key = media.and_then(|media| media.get("aes_key"));
    let media_aes_key = read_string(media_key);
    let parts = [
        format!("ownerAesKeyLen={}", js_len(owner_aes_key.as_deref())),
        format!(
            "ownerAesKeyHex={}",
            owner_aes_key
                .as_deref()
                .is_some_and(|key| is_hex_text(js_trim(key)))
        ),
        format!("mediaAesKeyLen={}", js_len(media_aes_key.as_deref())),
        format!(
            "mediaAesKeyDecodedLen={}",
            media_aes_key
                .as_deref()
                .map_or(0, |_| node_base64_decode(&read_units(media_key)).len())
        ),
    ];
    format!("keyShape={}", parts.join(","))
}

/// `summarizeWeixinDecryptCandidates`:
/// `candidates=[0:aes-128:len=…:head=…:image=…;…]`.
pub fn summarize_weixin_decrypt_candidates(buffer: &[u8], media_owner: &JsValue) -> String {
    let mut summaries = Vec::new();
    for key in extract_weixin_media_keys(media_owner) {
        let algorithm = algorithm_name(key.len());
        for candidate in try_decrypt_weixin_media(buffer, &key) {
            summaries.push(format!(
                "{}:{}:len={}:head={}:image={}",
                summaries.len(),
                algorithm,
                candidate.len(),
                hex_lower(&candidate[..candidate.len().min(8)]),
                is_known_image_buffer(&candidate)
            ));
        }
    }
    format!("candidates=[{}]", summaries.join(";"))
}

/// `WeixinChannel.ts:635-638` log tail for the image path:
/// `input=… output=… changed=… image=… keyShape=… candidates=[…]`.
pub fn summarize_weixin_image_decrypt(buffer: &[u8], image: &JsValue) -> String {
    let decrypted = try_decrypt_weixin_media_if_needed(buffer, image, is_known_image_buffer);
    let output = decrypted.as_deref().unwrap_or(buffer);
    format!(
        "input={} output={} changed={} image={} {} {}",
        buffer.len(),
        output.len(),
        decrypted.is_some(),
        is_known_image_buffer(output),
        summarize_weixin_media_key_shape(image),
        summarize_weixin_decrypt_candidates(buffer, image)
    )
}

/// `extractWeixinMediaKeys`: `owner.aeskey` (32 hex chars) and
/// `media.aes_key` (base64; both the raw decoded key and its hex text), with
/// the base64 fallback for an `owner.aeskey` that is not 32 hex chars.
/// Keys are length-filtered to 16/24/32 bytes and de-duplicated by hex.
pub fn extract_weixin_media_keys(media_owner: &JsValue) -> Vec<Vec<u8>> {
    let Some(owner) = media_owner.as_object() else {
        return Vec::new();
    };
    let media = owner.get("media").and_then(JsValue::as_object);
    let mut keys = Vec::new();
    let mut seen = HashSet::new();
    let owner_aes_key = read_string(owner.get("aeskey"));
    let mut push = |key: Vec<u8>| {
        if matches!(key.len(), 16 | 24 | 32) && seen.insert(key.clone()) {
            keys.push(key);
        }
    };
    if let Some(key) = owner_aes_key.as_deref() {
        let trimmed = js_trim(key);
        if is_hex32(trimmed) {
            push(node_hex_decode(trimmed));
        }
    }
    let media_key = media.and_then(|media| media.get("aes_key"));
    if read_string(media_key).is_some() {
        let decoded = node_base64_decode(&read_units(media_key));
        push(decoded.clone());
        let text: String = decoded.iter().map(|byte| (byte & 0x7f) as char).collect();
        if is_hex32(&text) {
            push(node_hex_decode(&text));
        }
    }
    if let Some(key) = owner_aes_key.as_deref() {
        let trimmed = js_trim(key);
        if !is_hex32(trimmed) {
            push(node_base64_decode(&read_units(owner.get("aeskey"))));
        }
    }
    keys
}

/// `tryDecryptWeixinMedia`: one AES-ECB candidate per key. An input that is
/// not a whole number of blocks, or a key outside 16/24/32 bytes, yields no
/// candidate (Node's `createDecipheriv` throws for both).
pub fn try_decrypt_weixin_media(buffer: &[u8], key: &[u8]) -> Vec<Vec<u8>> {
    let mut outputs = Vec::new();
    if !matches!(key.len(), 16 | 24 | 32) || !buffer.len().is_multiple_of(AES_BLOCK) {
        return outputs;
    }
    let mut padded = buffer.to_vec();
    let decrypted = match key.len() {
        16 => decrypt_blocks::<Aes128>(key, &mut padded),
        24 => decrypt_blocks::<Aes192>(key, &mut padded),
        _ => decrypt_blocks::<Aes256>(key, &mut padded),
    };
    if !decrypted {
        return outputs;
    }
    // Node: `stripPkcs7Padding(padded) ?? padded`.
    let stripped = strip_pkcs7_padding(&padded).map(<[u8]>::to_vec);
    outputs.push(stripped.unwrap_or(padded));
    outputs
}

/// `false` when the key length is rejected by the cipher (unreachable after
/// the 16/24/32 filter above, so the caller stays panic-free either way).
fn decrypt_blocks<C>(key: &[u8], buffer: &mut [u8]) -> bool
where
    C: BlockDecrypt + KeyInit,
{
    let Ok(cipher) = C::new_from_slice(key) else {
        return false;
    };
    for chunk in buffer.chunks_mut(AES_BLOCK) {
        cipher.decrypt_block(GenericArray::from_mut_slice(chunk));
    }
    true
}

fn strip_pkcs7_padding(buffer: &[u8]) -> Option<&[u8]> {
    let pad = *buffer.last()? as usize;
    if pad == 0 || pad > AES_BLOCK || pad > buffer.len() {
        return None;
    }
    if buffer[buffer.len() - pad..]
        .iter()
        .any(|&byte| byte as usize != pad)
    {
        return None;
    }
    Some(&buffer[..buffer.len() - pad])
}

fn algorithm_name(key_len: usize) -> String {
    match key_len {
        16 => "aes-128".to_owned(),
        24 => "aes-192".to_owned(),
        32 => "aes-256".to_owned(),
        other => format!("aes-{other}"),
    }
}

// ── CDN download ────────────────────────────────────────────────────────────

/// `extractWeixinMediaUrl`: the encrypted query parameter wins; otherwise the
/// first non-empty of `full_url` / `download_url` / `url` / `cdn_url`.
pub fn extract_weixin_media_url(media_owner: &JsValue) -> Option<String> {
    let media = media_owner.get("media")?.as_object()?;
    if let Some(param) = read_string(media.get("encrypt_query_param")) {
        return Some(format!(
            "{WEIXIN_CDN_BASE_URL}/download?encrypted_query_param={}",
            super::client::uri_encode_component(&param)
        ));
    }
    ["full_url", "download_url", "url", "cdn_url"]
        .into_iter()
        .find_map(|name| read_string(media.get(name)))
}

type CdnHttpClient = Client<hyper_rustls::HttpsConnector<HttpConnector>, Full<Bytes>>;

/// CDN download over the shared rustls connector (`runtime::model::http`),
/// not a copy of `weixin_connector` (#1725 item 9: the private copy dropped
/// the proxy and connect-timeout settings).
pub struct WeixinCdnClient {
    client: CdnHttpClient,
}

impl Default for WeixinCdnClient {
    fn default() -> Self {
        Self::new()
    }
}

impl WeixinCdnClient {
    pub fn new() -> Self {
        let config = crate::runtime::model::http::webpki_client_config();
        Self {
            client: Client::builder(TokioExecutor::new())
                .build(crate::runtime::model::http::https_connector(config)),
        }
    }

    /// `ImAttachmentStore.saveFromUrl`'s fetch step: GET with a 60 s deadline
    /// through the shared undici redirect policy
    /// ([`crate::fetch_redirect::follow`], not a private copy),
    /// `response.ok` required, and the 50 MiB limit enforced on both the
    /// declared and the received size. The attachment store itself (paths,
    /// names, metadata) belongs to #1822.
    pub async fn download(&self, url: &str) -> Result<Vec<u8>, WeixinClientError> {
        let exchange = async {
            let target = url::Url::parse(url).map_err(|_| WeixinClientError {
                name: "TypeError".into(),
                message: format!("Failed to parse URL from {url}"),
            })?;
            let response = follow(
                target,
                Method::GET,
                HeaderMap::new(),
                Bytes::new(),
                WeixinClientError::fetch_failed,
                |request| async {
                    self.client
                        .request(request)
                        .await
                        .map_err(|_| WeixinClientError::fetch_failed())
                },
            )
            .await?;
            let status = response.status();
            if !status.is_success() {
                return Err(WeixinClientError::error(format!(
                    "Attachment download failed HTTP {}.",
                    status.as_u16()
                )));
            }
            if let Some(declared) = response
                .headers()
                .get(header::CONTENT_LENGTH)
                .and_then(|value| value.to_str().ok())
                .and_then(js_number)
                && declared > WEIXIN_ATTACHMENT_MAX_BYTES as f64
            {
                return Err(WeixinClientError::error(format!(
                    "Attachment is {declared} bytes (limit {WEIXIN_ATTACHMENT_MAX_BYTES})."
                )));
            }
            read_body(response.into_body(), WEIXIN_ATTACHMENT_MAX_BYTES, None)
                .await
                .map_err(|error| match error {
                    // The baseline buffers `arrayBuffer()` and only then
                    // reports the exact size; a hostile CDN could make that
                    // unbounded, so the port stops at the limit and reports
                    // the smallest size that must have arrived.
                    BodyError::TooLong => attachment_too_large(WEIXIN_ATTACHMENT_MAX_BYTES + 1),
                    // `arrayBuffer()` on a body that errors mid-stream rejects
                    // with `TypeError: terminated` (undici), the same shape
                    // `fetch_redirect`'s other call sites report.
                    BodyError::Terminated => WeixinClientError {
                        name: "TypeError".into(),
                        message: "terminated".into(),
                    },
                    BodyError::Aborted => WeixinClientError::abort(),
                })
        };
        timeout(WEIXIN_CDN_FETCH_TIMEOUT, exchange)
            .await
            .map_err(|_| WeixinClientError::abort())?
    }
}

fn attachment_too_large(bytes: usize) -> WeixinClientError {
    WeixinClientError::error(format!(
        "Attachment is {bytes} bytes (limit {WEIXIN_ATTACHMENT_MAX_BYTES})."
    ))
}

/// JS `Number(value)` for a `content-length` header: `""` is `0`, anything
/// unparsable is `NaN` (and therefore not greater than the limit).
fn js_number(value: &str) -> Option<f64> {
    if value.trim().is_empty() {
        return Some(0.0);
    }
    value.trim().parse::<f64>().ok()
}

// ── JS boundary helpers ─────────────────────────────────────────────────────

/// `readString`: a non-empty string after `trim()`, returned untrimmed.
fn read_string(value: Option<&JsValue>) -> Option<String> {
    let text = value?.as_string()?;
    // Lossy conversion preserves the UTF-16 unit count (a lone surrogate
    // becomes one U+FFFD), which is what `.length` reports.
    let text = String::from_utf16_lossy(text.units());
    if js_trim(&text).is_empty() {
        None
    } else {
        Some(text)
    }
}

/// The UTF-16 units of a `readString` value (`&[]` when absent); Node's
/// `Buffer.from` decoders read one byte per unit, so they must not go through
/// the lossy `String` (a lone surrogate would collapse into U+FFFD).
fn read_units(value: Option<&JsValue>) -> Vec<u16> {
    value
        .and_then(JsValue::as_string)
        .map_or_else(Vec::new, |text| text.units().to_vec())
}

/// JS `String.prototype.length` (UTF-16 code units); `0` for `undefined`.
fn js_len(value: Option<&str>) -> usize {
    value.map_or(0, |text| text.encode_utf16().count())
}

fn is_hex32(value: &str) -> bool {
    value.len() == 32 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// `/^[0-9a-fA-F]+$/`: the shape summary reports hex text of any length, while
/// key extraction only accepts exactly 32 characters.
fn is_hex_text(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Node `Buffer.from(value, "hex")`: decode pairs until one is not hex; a
/// trailing odd character is ignored.
fn node_hex_decode(value: &str) -> Vec<u8> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() / 2);
    let mut index = 0;
    while index + 1 < bytes.len() {
        let (Some(high), Some(low)) = (hex_digit(bytes[index]), hex_digit(bytes[index + 1])) else {
            break;
        };
        out.push(high << 4 | low);
        index += 2;
    }
    out
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Node `Buffer.from(value, "base64")`: Node narrows the JS string to **one
/// byte per UTF-16 code unit** (the low byte) before scanning, so `"\u{0141}"`
/// decodes as `A` and `"\u{013D}"` stops like `=`. Characters outside the
/// alphabet (including whitespace and `!`) are skipped, `-`/`_` are accepted,
/// and decoding stops at the first `=`. A trailing group of two or three
/// characters still yields 1 or 2 bytes.
fn node_base64_decode(units: &[u16]) -> Vec<u8> {
    let mut sextets = Vec::with_capacity(units.len());
    for unit in units {
        let byte = *unit as u8;
        let sextet = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            _ => continue,
        };
        sextets.push(sextet);
    }
    let mut out = Vec::with_capacity(sextets.len() * 3 / 4);
    for chunk in sextets.chunks(4) {
        // Node emits nothing for a trailing single character.
        if chunk.len() < 2 {
            break;
        }
        out.push(chunk[0] << 2 | chunk[1] >> 4);
        if chunk.len() > 2 {
            out.push(chunk[1] << 4 | chunk[2] >> 2);
        }
        if chunk.len() > 3 {
            out.push(chunk[2] << 6 | chunk[3]);
        }
    }
    out
}

/// JS `encodeURIComponent` (unreserved set `A-Za-z0-9-_.!~*'()`).
#[cfg(test)]
mod tests;
