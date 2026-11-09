//! WeCom media contract from fixed TS cc7c4e6. No websocket/session owner.
use super::super::{
    protocol::{BoxFuture, ChannelError},
    sms,
};
use aes::Aes256;
use base64::{
    Engine,
    alphabet::STANDARD,
    engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig},
};
use bytes::Bytes;
use cbc::cipher::{BlockDecryptMut, KeyIvInit, block_padding::NoPadding};
use http_body_util::{BodyExt, Full};
use hyper::Request;
use hyper_util::{client::legacy::Client, rt::TokioExecutor};
use md5::{Digest, Md5};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{
    path::{Component, Path, PathBuf},
    sync::Arc,
    time::Duration,
};
pub const FILE_MAX_BYTES: usize = 20 * 1024 * 1024;
pub const UPLOAD_CHUNK_SIZE: usize = 512 * 1024;
const NODE_B64: GeneralPurpose = GeneralPurpose::new(
    &STANDARD,
    GeneralPurposeConfig::new()
        .with_decode_allow_trailing_bits(true)
        .with_decode_padding_mode(DecodePaddingMode::Indifferent),
);
fn error(s: impl Into<String>) -> ChannelError {
    ChannelError::error(s.into())
}
trait JsTrim {
    fn js_trim(&self) -> &str;
}
impl JsTrim for str {
    fn js_trim(&self) -> &str {
        self.trim_matches(|c: char| (c.is_whitespace() && c != '\u{85}') || c == '\u{feff}')
    }
}
fn shown(s: &JsString) -> String {
    String::from_utf16_lossy(s.units())
}
fn text(v: Option<&JsValue>) -> String {
    v.filter(|v| !sms::nullish(v))
        .map(sms::js_string_of)
        .as_ref()
        .map(shown)
        .unwrap_or_default()
}
fn obj<const N: usize>(fields: [(&str, JsValue); N]) -> JsValue {
    let mut out = JsObject::new();
    for (k, v) in fields {
        out.insert(k.into(), v);
    }
    JsValue::Object(out)
}
fn js(s: impl AsRef<str>) -> JsValue {
    JsValue::String(s.as_ref().into())
}
pub trait MediaRequestPort: Send + Sync {
    fn request(&self, cmd: JsString, body: JsValue) -> BoxFuture<Result<JsValue, ChannelError>>;
}
#[derive(Clone, Debug)]
pub struct PreparedMedia {
    pub data: Vec<u8>,
    #[cfg(test)]
    pub content_type: String,
    pub file_name: String,
    pub detected_type: String,
    pub final_type: String,
    pub reject_reason: Option<String>,
    pub downgrade_note: Option<String>,
}
#[derive(Clone, Debug)]
pub struct Deliverable {
    pub path: PathBuf,
    pub media_type: String,
}
pub struct Extraction {
    pub text: JsString,
    pub deliverables: Vec<Deliverable>,
    pub warnings: Vec<String>,
}
#[derive(Debug)]
pub struct CacheError {
    pub error: ChannelError,
    pub storage: bool,
}
impl CacheError {
    pub fn is_storage(&self) -> bool {
        self.storage
    }
}
impl std::fmt::Display for CacheError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.error, f)
    }
}
impl std::error::Error for CacheError {}
impl From<ChannelError> for CacheError {
    fn from(error: ChannelError) -> Self {
        Self {
            error,
            storage: false,
        }
    }
}
pub struct MediaClient {
    http: super::super::http_client::OutboundClient,
    pub cache_dir: PathBuf,
    now: Arc<dyn Fn() -> u64 + Send + Sync>,
    uuid: Arc<dyn Fn() -> String + Send + Sync>,
}
impl Default for MediaClient {
    fn default() -> Self {
        Self::new(
            std::env::temp_dir().join("pilotdeck-wecom-media"),
            Arc::new(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64
            }),
            Arc::new(|| uuid::Uuid::new_v4().to_string()),
        )
    }
}
impl MediaClient {
    pub fn new(
        cache_dir: PathBuf,
        now: Arc<dyn Fn() -> u64 + Send + Sync>,
        uuid: Arc<dyn Fn() -> String + Send + Sync>,
    ) -> Self {
        let config = crate::runtime::model::http::webpki_client_config();
        Self {
            http: Client::builder(TokioExecutor::new())
                .build(crate::runtime::model::http::https_connector(config)),
            cache_dir,
            now,
            uuid,
        }
    }
    /// Decode/download/decrypt failures are returned for the caller's warning log.
    /// Missing source alone is Ok(None); mkdir/write errors are never swallowed.
    pub async fn cache_inbound(
        &self,
        kind: &str,
        media: &JsValue,
    ) -> Result<Option<JsValue>, CacheError> {
        let mut data = if media.get("base64").is_some_and(sms::truthy) {
            node_base64(&text(media.get("base64")))
        } else {
            let url = text(media.get("url")).js_trim().to_owned();
            if url.is_empty() {
                return Ok(None);
            }
            self.download(&url).await?.0
        };
        let key = text(media.get("aeskey"));
        if !key.js_trim().is_empty() {
            data = decrypt_bytes(&data, key.js_trim())?;
        }
        let raw = media
            .get("filename")
            .filter(|v| !sms::nullish(v))
            .or_else(|| media.get("name").filter(|v| !sms::nullish(v)))
            .map(|v| text(Some(v)))
            .unwrap_or_else(|| format!("wecom_{kind}{}", default_ext(kind)));
        let raw = raw.js_trim();
        let fallback = format!("wecom_{kind}{}", default_ext(kind));
        let name = safe_file_name(if raw.is_empty() { &fallback } else { raw });
        tokio::fs::create_dir_all(&self.cache_dir)
            .await
            .map_err(|e| CacheError {
                error: error(e.to_string()),
                storage: true,
            })?;
        let path = self.cache_dir.join(format!(
            "{}-{}-{name}",
            (self.now)(),
            (self.uuid)().replace('-', "")
        ));
        let mut options = tokio::fs::OpenOptions::new();
        options.create(true).truncate(true).write(true);
        #[cfg(unix)]
        {
            options.mode(0o600);
        }
        let mut file = options.open(&path).await.map_err(|e| CacheError {
            error: error(e.to_string()),
            storage: true,
        })?;
        use tokio::io::AsyncWriteExt;
        file.write_all(&data).await.map_err(|e| CacheError {
            error: error(e.to_string()),
            storage: true,
        })?;
        let mime = normalize_content_type(
            &text(
                media
                    .get("content_type")
                    .filter(|v| !sms::nullish(v))
                    .or_else(|| media.get("contentType")),
            ),
            &name,
        );
        Ok(Some(obj([
            ("type", js(if kind == "image" { "image" } else { "file" })),
            ("name", js(&name)),
            ("path", js(path.to_string_lossy())),
            ("mimeType", js(mime)),
            ("bytes", JsValue::Number(data.len() as f64)),
            (
                "metadata",
                obj([
                    ("channelKey", js("wecom")),
                    ("source", js("wecom")),
                    ("mediaType", js(kind)),
                ]),
            ),
        ])))
    }
    pub async fn download(
        &self,
        source: &str,
    ) -> Result<(Vec<u8>, String, Option<String>), ChannelError> {
        tokio::time::timeout(Duration::from_secs(30), async {
            let mut url = url::Url::parse(source).map_err(|e| error(e.to_string()))?;
            for redirects in 0..=20 {
                if !matches!(url.scheme(), "http" | "https") {
                    return Err(error(format!(
                        "unsupported media URL protocol: {}:",
                        url.scheme()
                    )));
                }
                let request = Request::get(url.as_str())
                    .header("User-Agent", "PilotDeck/1.0")
                    .header("Accept", "*/*")
                    .body(Full::new(Bytes::new()))
                    .map_err(|e| error(e.to_string()))?;
                let response = self
                    .http
                    .request(request)
                    .await
                    .map_err(|e| error(e.to_string()))?;
                let status = response.status();
                if matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308)
                    && let Some(location) = response.headers().get("location")
                {
                    if redirects == 20 {
                        return Err(error("redirect count exceeded"));
                    }
                    url = url
                        .join(location.to_str().map_err(|e| error(e.to_string()))?)
                        .map_err(|e| error(e.to_string()))?;
                    continue;
                }
                if !status.is_success() {
                    return Err(error(format!("HTTP {}", status.as_u16())));
                }
                let get = |key: &str| {
                    response
                        .headers()
                        .get(key)
                        .and_then(|h| h.to_str().ok())
                        .map(str::to_owned)
                };
                let mime = get("content-type").unwrap_or_default();
                let disposition = get("content-disposition");
                if let Some(length) = get("content-length") {
                    let digits = length
                        .trim_start()
                        .trim_start_matches('+')
                        .chars()
                        .take_while(|c| c.is_ascii_digit())
                        .collect::<String>();
                    if digits
                        .parse::<u64>()
                        .is_ok_and(|n| n > FILE_MAX_BYTES as u64)
                    {
                        return Err(error(format!(
                            "remote media exceeds limit: {length} bytes > {FILE_MAX_BYTES}"
                        )));
                    }
                }
                let data = response
                    .into_body()
                    .collect()
                    .await
                    .map_err(|e| error(e.to_string()))?
                    .to_bytes()
                    .to_vec();
                if data.len() > FILE_MAX_BYTES {
                    return Err(error(format!(
                        "remote media exceeds limit: {} bytes > {FILE_MAX_BYTES}",
                        data.len()
                    )));
                }
                return Ok((data, mime, disposition));
            }
            Err(error("redirect count exceeded"))
        })
        .await
        .map_err(|_| error("The operation was aborted due to timeout"))?
    }
    pub async fn load_source(
        &self,
        source: &str,
        file_name: Option<&str>,
    ) -> Result<PreparedMedia, ChannelError> {
        let source = source.js_trim();
        if source.is_empty() {
            return Err(error("media source is required"));
        }
        let parsed = url::Url::parse(source).ok();
        let mut mime = String::new();
        let (data, name) = match parsed.as_ref().map(url::Url::scheme) {
            Some("http" | "https") => {
                let (data, content_type, disposition) = self.download(source).await?;
                mime = content_type;
                let name = match file_name {
                    Some(name) => name.to_owned(),
                    None => guess_file_name(source, disposition.as_deref())?,
                };
                (data, name)
            }
            Some("file") => {
                let path = parsed
                    .unwrap()
                    .to_file_path()
                    .map_err(|_| error("Invalid file URL"))?;
                let data = tokio::fs::read(&path)
                    .await
                    .map_err(|e| error(e.to_string()))?;
                (
                    data,
                    file_name
                        .map(str::to_owned)
                        .unwrap_or_else(|| basename(&path.to_string_lossy())),
                )
            }
            _ => {
                let path = resolve(Path::new(source));
                let info = tokio::fs::metadata(&path)
                    .await
                    .map_err(|e| error(e.to_string()))?;
                if !info.is_file() {
                    return Err(error(format!(
                        "media source is not a file: {}",
                        path.display()
                    )));
                }
                let data = tokio::fs::read(&path)
                    .await
                    .map_err(|e| error(e.to_string()))?;
                (
                    data,
                    file_name
                        .map(str::to_owned)
                        .unwrap_or_else(|| basename(&path.to_string_lossy())),
                )
            }
        };
        Ok(prepare_bytes(data, &mime, &name))
    }
}
pub fn node_base64(value: &str) -> Vec<u8> {
    let mut input = String::new();
    for c in value.chars() {
        match c {
            '=' => break,
            '-' => input.push('+'),
            '_' => input.push('/'),
            c if c.is_ascii_alphanumeric() || c == '+' || c == '/' => input.push(c),
            _ => {}
        }
    }
    if input.len() % 4 == 1 {
        input.pop();
    }
    NODE_B64.decode(input).unwrap_or_default()
}
pub fn decrypt_bytes(encrypted: &[u8], key: &str) -> Result<Vec<u8>, ChannelError> {
    let key = node_base64(key);
    if key.len() != 32 {
        return Err(error(format!(
            "Invalid WeCom AES key length: expected 32 bytes, got {}",
            key.len()
        )));
    }
    let mut data = encrypted.to_vec();
    let cipher = cbc::Decryptor::<Aes256>::new_from_slices(&key, &key[..16])
        .map_err(|e| error(e.to_string()))?;
    let data = cipher
        .decrypt_padded_mut::<NoPadding>(&mut data)
        .map_err(|e| error(e.to_string()))?;
    let pad = data.last().copied().unwrap_or(0) as usize;
    if !(1..=32).contains(&pad) || pad > data.len() {
        return Err(error(format!("Invalid PKCS#7 padding value: {pad}")));
    }
    if data[data.len() - pad..].iter().any(|b| *b as usize != pad) {
        return Err(error("Invalid PKCS#7 padding bytes"));
    }
    Ok(data[..data.len() - pad].to_vec())
}
pub fn response_error(value: &JsValue) -> Option<String> {
    let body = value.get("body");
    let code = body
        .and_then(|b| b.get("errcode"))
        .filter(|v| !sms::nullish(v))
        .or_else(|| value.get("errcode"));
    match code {
        None | Some(JsValue::Null | JsValue::Undefined) => None,
        Some(JsValue::Number(n)) if *n == 0.0 => None,
        Some(code) => {
            let message = body
                .and_then(|b| b.get("errmsg"))
                .filter(|v| !sms::nullish(v))
                .or_else(|| value.get("errmsg"))
                .filter(|v| !sms::nullish(v));
            Some(format!(
                "WeCom errcode {}: {}",
                text(Some(code)),
                message
                    .map(|v| text(Some(v)))
                    .unwrap_or_else(|| "error".into())
            ))
        }
    }
}
trait IfEmpty {
    fn if_empty(self, fallback: &str) -> String;
}
impl IfEmpty for String {
    fn if_empty(self, fallback: &str) -> String {
        if self.is_empty() {
            fallback.into()
        } else {
            self
        }
    }
}
pub async fn upload_media(
    port: &dyn MediaRequestPort,
    prepared: &PreparedMedia,
) -> Result<JsValue, ChannelError> {
    let data = &prepared.data;
    if data.is_empty() {
        return Err(error("Cannot upload empty media"));
    }
    let chunks = data.len().div_ceil(UPLOAD_CHUNK_SIZE);
    if chunks > 100 {
        return Err(error(format!(
            "File too large: {chunks} chunks exceeds maximum of 100"
        )));
    }
    let init = port
        .request(
            "aibot_upload_media_init".into(),
            obj([
                ("type", js(&prepared.final_type)),
                ("filename", js(&prepared.file_name)),
                ("total_size", JsValue::Number(data.len() as f64)),
                ("total_chunks", JsValue::Number(chunks as f64)),
                ("md5", js(format!("{:x}", Md5::digest(data)))),
            ]),
        )
        .await?;
    if let Some(e) = response_error(&init) {
        return Err(error(format!("media upload init failed: {e}")));
    }
    let body = init.get("body");
    let id = text(
        body.and_then(|b| b.get("upload_id"))
            .filter(|v| !sms::nullish(v))
            .or_else(|| body.and_then(|b| b.get("uploadId"))),
    )
    .js_trim()
    .to_owned();
    if id.is_empty() {
        return Err(error("media upload init failed: missing upload_id"));
    }
    for (index, chunk) in data.chunks(UPLOAD_CHUNK_SIZE).enumerate() {
        let reply = port
            .request(
                "aibot_upload_media_chunk".into(),
                obj([
                    ("upload_id", js(&id)),
                    ("chunk_index", JsValue::Number(index as f64)),
                    (
                        "base64_data",
                        js(base64::engine::general_purpose::STANDARD.encode(chunk)),
                    ),
                ]),
            )
            .await?;
        if let Some(e) = response_error(&reply) {
            return Err(error(format!("media upload chunk {index} failed: {e}")));
        }
    }
    let reply = port
        .request(
            "aibot_upload_media_finish".into(),
            obj([("upload_id", js(&id))]),
        )
        .await?;
    if let Some(e) = response_error(&reply) {
        return Err(error(format!("media upload finish failed: {e}")));
    }
    let mut merged = reply.as_object().cloned().unwrap_or_default();
    if let Some(body) = reply.get("body").and_then(JsValue::as_object) {
        for (k, v) in body.entries() {
            merged.insert(k.clone(), v.clone());
        }
    }
    Ok(JsValue::Object(merged))
}

pub fn prepare_bytes(data: Vec<u8>, content_type: &str, file_name: &str) -> PreparedMedia {
    let mime = normalize_content_type(content_type, file_name);
    let detected = if mime.starts_with("image/") {
        "image"
    } else if mime.starts_with("video/") {
        "video"
    } else if mime.starts_with("audio/") || mime == "application/ogg" {
        "voice"
    } else {
        "file"
    };
    let size = data.len();
    let mb = size as f64 / (1024.0 * 1024.0);
    let mut result = PreparedMedia {
        data,
        #[cfg(test)]
        content_type: mime.clone(),
        file_name: if file_name.is_empty() {
            safe_file_name(&format!("wecom_media{}", default_ext(detected)))
        } else {
            safe_file_name(file_name)
        },
        detected_type: detected.into(),
        final_type: detected.into(),
        reject_reason: None,
        downgrade_note: None,
    };
    if size > FILE_MAX_BYTES {
        result.reject_reason = Some(format!(
            "文件大小 {mb:.2}MB 超过企业微信 20MB 限制，无法发送。"
        ));
    } else if detected == "image" && size > 10 * 1024 * 1024 {
        result.final_type = "file".into();
        result.downgrade_note = Some(format!(
            "图片大小 {mb:.2}MB 超过 10MB 限制，已转为文件格式发送。"
        ));
    } else if detected == "video" && size > 10 * 1024 * 1024 {
        result.final_type = "file".into();
        result.downgrade_note = Some(format!(
            "视频大小 {mb:.2}MB 超过 10MB 限制，已转为文件格式发送。"
        ));
    } else if detected == "voice" && mime != "audio/amr" {
        result.final_type = "file".into();
        result.downgrade_note = Some(format!(
            "语音格式 {mime} 不支持，企业微信原生语音仅支持 AMR，已转为文件格式发送。"
        ));
    } else if detected == "voice" && size > 2 * 1024 * 1024 {
        result.final_type = "file".into();
        result.downgrade_note = Some(format!(
            "语音大小 {mb:.2}MB 超过 2MB 限制，已转为文件格式发送。"
        ));
    }
    result
}
pub fn tool_event_images(event: &JsValue) -> Vec<PreparedMedia> {
    if event.get("type") != Some(&js("tool_call_finished")) {
        return vec![];
    }
    let Some(JsValue::Array(images)) = event.get("images") else {
        return vec![];
    };
    let id = event
        .get("toolCallId")
        .filter(|v| sms::truthy(v))
        .map(|v| text(Some(v)))
        .unwrap_or_else(|| "image".into());
    images
        .iter()
        .enumerate()
        .map(|(i, image)| {
            let mime = text(image.get("mimeType"));
            let ext = ext_for_mime(&mime).unwrap_or(".png");
            prepare_bytes(
                node_base64(&text(image.get("data"))),
                &mime,
                &format!("tool-{id}-{}{ext}", i + 1),
            )
        })
        .collect()
}
fn basename(value: &str) -> String {
    Path::new(value)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}
pub fn safe_file_name(value: &str) -> String {
    let name = basename(if value.is_empty() {
        "wecom_media"
    } else {
        value
    });
    regex::Regex::new(r"[^A-Za-z0-9_.\-()\u{4e00}-\u{9fff}]+")
        .unwrap()
        .replace_all(&name, "_")
        .into_owned()
        .if_empty("wecom_media")
}
fn ext(path: &str) -> String {
    Path::new(path)
        .extension()
        .map(|s| format!(".{}", s.to_string_lossy().to_lowercase()))
        .unwrap_or_default()
}
fn default_ext(kind: &str) -> &'static str {
    match kind {
        "image" => ".jpg",
        "video" => ".mp4",
        "voice" => ".amr",
        _ => ".bin",
    }
}
pub fn normalize_content_type(value: &str, name: &str) -> String {
    let value = value
        .split(';')
        .next()
        .unwrap_or("")
        .js_trim()
        .to_lowercase();
    if !value.is_empty() && value != "application/octet-stream" && value != "text/plain" {
        return value;
    }
    mime_for_ext(&ext(name))
        .map(str::to_owned)
        .unwrap_or_else(|| value.if_empty("application/octet-stream"))
}
fn mime_for_ext(ext: &str) -> Option<&'static str> {
    Some(match ext {
        ".png" => "image/png",
        ".jpg" | ".jpeg" => "image/jpeg",
        ".gif" => "image/gif",
        ".webp" => "image/webp",
        ".mp4" => "video/mp4",
        ".mov" => "video/quicktime",
        ".amr" => "audio/amr",
        ".pdf" => "application/pdf",
        ".txt" => "text/plain",
        _ => return None,
    })
}
fn ext_for_mime(mime: &str) -> Option<&'static str> {
    Some(
        match mime
            .split(';')
            .next()
            .unwrap_or("")
            .js_trim()
            .to_lowercase()
            .as_str()
        {
            "image/png" => ".png",
            "image/jpeg" => ".jpg",
            "image/gif" => ".gif",
            "image/webp" => ".webp",
            "video/mp4" => ".mp4",
            "audio/amr" => ".amr",
            "application/pdf" => ".pdf",
            _ => return None,
        },
    )
}
fn guess_file_name(source: &str, disposition: Option<&str>) -> Result<String, ChannelError> {
    let disposition = disposition.unwrap_or("");
    if let Some(c) = regex::Regex::new("(?i)filename\\*=UTF-8''([^;]+)")
        .unwrap()
        .captures(disposition)
    {
        return percent_decode(&c[1]).ok_or_else(|| error("URI malformed"));
    }
    if let Some(c) = regex::Regex::new("(?i)filename=\"?([^\";]+)\"?")
        .unwrap()
        .captures(disposition)
    {
        return Ok(c[1].to_owned());
    }
    Ok(url::Url::parse(source)
        .ok()
        .map(|u| basename(u.path()))
        .unwrap_or_default()
        .if_empty("wecom_media"))
}
fn percent_decode(s: &str) -> Option<String> {
    let mut bytes = vec![];
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            if i + 2 >= b.len() {
                return None;
            }
            bytes.push(u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).ok()?, 16).ok()?);
            i += 3;
        } else {
            bytes.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(bytes).ok()
}
fn resolve(path: &Path) -> PathBuf {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}
fn denied(path: &Path, home: &Path, pilot_home: &Path) -> bool {
    let path = resolve(path);
    if path.starts_with(home.join(".ssh"))
        || path.starts_with(pilot_home.join("server-token"))
        || path.starts_with(pilot_home.join("pilotdeck.yaml"))
    {
        return true;
    }
    let base = basename(&path.to_string_lossy()).to_lowercase();
    if [
        ".env",
        "auth.json",
        "credentials",
        "server-token",
        "pilotdeck.yaml",
    ]
    .contains(&base.as_str())
    {
        return true;
    }
    path.to_string_lossy().split(['\\', '/']).any(|s| {
        [".git", ".ssh", "node_modules", "mcp-tokens", "pairing"]
            .contains(&s.to_lowercase().as_str())
    })
}
fn deliverable_kind(ext: &str) -> &'static str {
    if [
        ".png", ".jpg", ".jpeg", ".gif", ".webp", ".bmp", ".tiff", ".svg",
    ]
    .contains(&ext)
    {
        "image"
    } else if [".mp4", ".mov", ".avi", ".mkv", ".webm"].contains(&ext) {
        "video"
    } else if [".mp3", ".wav", ".ogg", ".opus", ".m4a", ".flac", ".amr"].contains(&ext) {
        "voice"
    } else {
        "file"
    }
}
const EXTENSIONS: &str = "png|jpg|jpeg|gif|webp|bmp|tiff|svg|mp4|mov|avi|mkv|webm|mp3|wav|ogg|opus|m4a|flac|amr|pdf|docx|doc|odt|rtf|txt|md|xlsx|xls|csv|tsv|json|xml|yaml|yml|pptx|ppt|odp|zip|tar|gz|tgz|bz2|7z|html|htm";
fn mask(text: &str, ranges: &[(usize, usize)]) -> String {
    let mut bytes = text.as_bytes().to_vec();
    for &(start, end) in ranges {
        for b in &mut bytes[start..end] {
            if *b != b'\n' {
                *b = b' ';
            }
        }
    }
    String::from_utf8(bytes).unwrap()
}
pub async fn extract_deliverables(text: &JsString, home: &Path, pilot_home: &Path) -> Extraction {
    let source = shown(text);
    let mut spans = vec![];
    for m in regex::Regex::new(r"(?s)\x60\x60\x60.*?\x60\x60\x60")
        .unwrap()
        .find_iter(&source)
    {
        spans.push((m.start(), m.end()));
    }
    let media_marker = regex::Regex::new(r"(?i)MEDIA:\s*$").unwrap();
    for m in regex::Regex::new(r"\x60[^\x60\n]+\x60")
        .unwrap()
        .find_iter(&source)
    {
        if !media_marker.is_match(
            &source[..m.start()]
                .chars()
                .rev()
                .take(20)
                .collect::<String>()
                .chars()
                .rev()
                .collect::<String>(),
        ) {
            spans.push((m.start(), m.end()));
        }
    }
    for m in regex::Regex::new(r"(?m)^>.*$").unwrap().find_iter(&source) {
        spans.push((m.start(), m.end()));
    }
    let protected = mask(&source, &spans);
    let media=regex::Regex::new(r#"(?i)["'\x60]?MEDIA:\s*(?:\x60([^\x60\n]+)\x60|"([^"\n]+)"|'([^'\n]+)'|((?:~/|/)\S+))["'\x60]?"#).unwrap();
    let mut candidates = vec![];
    let mut tagged = vec![];
    for c in media.captures_iter(&protected) {
        let whole = c.get(0).unwrap();
        if let Some(raw) = (1..=4).find_map(|i| c.get(i)) {
            candidates.push((whole.start(), raw.as_str().to_owned()));
            tagged.push((whole.start(), whole.end()));
        }
    }
    let bare = mask(&protected, &tagged);
    let mut extensions = EXTENSIONS.split('|').collect::<Vec<_>>();
    extensions.sort_by_key(|s| std::cmp::Reverse(s.len()));
    let ext_pattern = extensions.join("|");
    let pattern = format!(r#"(?i)(^|[\s(:：])((?:~/|/)[^\s\x60"',;:)\]}}]+?\.(?:{ext_pattern}))"#);
    for c in regex::Regex::new(&pattern).unwrap().captures_iter(&bare) {
        let raw = c.get(2).unwrap();
        let tail = &bare[raw.end()..];
        if !tail.is_empty()
            && !tail
                .chars()
                .next()
                .is_some_and(|c| c.is_whitespace() || "\u{60}\"',;:)]}".contains(c))
        {
            continue;
        }
        candidates.push((raw.start(), raw.as_str().to_owned()));
    }
    candidates.sort_by_key(|c| c.0);
    let mut deliverables = vec![];
    let mut warnings = vec![];
    let mut seen = std::collections::HashSet::new();
    for (_, raw) in candidates {
        match validate_path(&raw, home, pilot_home).await {
            Ok(item) => {
                if seen.insert(item.path.clone()) {
                    deliverables.push(item);
                }
            }
            Err(reason) => {
                let reason = format!("附件未发送：{reason}");
                if !warnings.contains(&reason) {
                    warnings.push(reason);
                }
            }
        }
    }
    Extraction {
        text: JsString::from_utf16(sms::trim_units(text.units()).to_vec()),
        deliverables,
        warnings,
    }
}
async fn validate_path(raw: &str, home: &Path, pilot_home: &Path) -> Result<Deliverable, String> {
    let normalized = raw
        .js_trim()
        .trim_matches(['"', '\'', '\u{60}'])
        .trim_end_matches(['.', ',', ';', ':', ')', ']', '}']);
    if normalized.is_empty() {
        return Err("附件路径为空。".into());
    }
    if regex::Regex::new("(?i)^[a-z][a-z0-9+.-]*:")
        .unwrap()
        .is_match(normalized)
        && !normalized.starts_with("file:")
    {
        return Err("企业微信附件投递仅支持本地文件路径。".into());
    }
    let path = if normalized.starts_with("file:") {
        url::Url::parse(normalized)
            .map_err(|e| e.to_string())?
            .to_file_path()
            .map_err(|_| "Invalid file URL".to_owned())?
    } else if let Some(rest) = normalized.strip_prefix("~/") {
        home.join(rest)
    } else {
        PathBuf::from(normalized)
    };
    if !path.is_absolute() {
        return Err("附件路径必须是绝对路径或 ~/ 路径。".into());
    }
    let path = resolve(&path);
    let extension = ext(&path.to_string_lossy());
    if !EXTENSIONS.split('|').any(|e| extension == format!(".{e}")) {
        return Err(format!(
            "不支持的附件类型 {}。",
            if extension.is_empty() {
                "(无扩展名)"
            } else {
                &extension
            }
        ));
    }
    if denied(&path, home, pilot_home) {
        return Err("该路径位于敏感目录或敏感配置文件中。".into());
    }
    let info = tokio::fs::metadata(&path)
        .await
        .map_err(|_| "附件文件不存在。".to_owned())?;
    if !info.is_file() {
        return Err("附件路径不是普通文件。".into());
    }
    let real = tokio::fs::canonicalize(&path).await.unwrap_or(path);
    if denied(&real, home, pilot_home) {
        return Err("该路径位于敏感目录或敏感配置文件中。".into());
    }
    Ok(Deliverable {
        path: real,
        media_type: deliverable_kind(&extension).into(),
    })
}
#[cfg(test)]
#[path = "media_tests.rs"]
mod media_tests;
