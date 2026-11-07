//! `src/adapters/channel/protocol/ImAttachmentStore.ts` and
//! `ImAttachmentDelivery.ts` (baseline cc7c4e6), ported as the shared
//! `protocol/` layer the TS channel adapters import.
//!
//! Scope (#1822): the declaration surface plus the inbound caller. The weixin
//! adapter supplies an [`AttachmentFetcher`] over
//! [`crate::channel::weixin::media::WeixinCdnClient`] and persists inbound
//! message items through [`ImAttachmentStore::save_from_url`]. The outbound
//! half ([`ImAttachmentDelivery`]) is `weixin`'s `sendMedia`/`getUploadUrl`
//! wiring in #1824, so until then only its own mock-sink tests call it.
//!
//! Path and filename rules are the baseline's: root/channel/chat/message
//! directories created 0700, files written 0600, every path component
//! sanitized by `safePathPart` (120 UTF-16 units) and every basename by
//! `safeFilename` (180 UTF-16 units).
#![cfg_attr(not(test), allow(dead_code))]

use hyper::HeaderMap;
use pilotdeck_protocol::{JsObject, JsValue};
use std::{
    fmt,
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
};

use crate::metadata::js_trim;

/// `DEFAULT_MAX_BYTES` (ImAttachmentStore.ts:22). The weixin adapter passes
/// `WEIXIN_MAX_ATTACHMENT_BYTES`, which is the same value.
pub const DEFAULT_MAX_BYTES: usize = 50 * 1024 * 1024;

/// `ChannelAttachment["type"]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttachmentKind {
    File,
    Image,
    Text,
    Unknown,
}
impl AttachmentKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Image => "image",
            Self::Text => "text",
            Self::Unknown => "unknown",
        }
    }
}
impl From<&str> for AttachmentKind {
    fn from(value: &str) -> Self {
        match value {
            "file" => Self::File,
            "image" => Self::Image,
            "text" => Self::Text,
            _ => Self::Unknown,
        }
    }
}

/// A thrown JS `Error` at the attachment boundary. `formatWeixinError` only
/// ever reads `name` and `message` from these: neither protocol module sets
/// `code` or `cause`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttachmentError {
    pub name: String,
    pub message: String,
}
impl AttachmentError {
    pub fn error(message: impl Into<String>) -> Self {
        Self {
            name: "Error".into(),
            message: message.into(),
        }
    }
    /// `` `${name}: ${message}` `` — the `Error` arm of `formatError`.
    pub fn formatted(&self) -> String {
        format!("{}: {}", self.name, self.message)
    }
}
impl fmt::Display for AttachmentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.formatted())
    }
}
impl std::error::Error for AttachmentError {}

/// `ChannelAttachment` (gateway/protocol/types.ts:105-113): what an inbound
/// turn hands to A7. Absent optional fields stay absent, matching
/// `JSON.stringify` dropping `undefined`.
#[derive(Clone, Debug, Default)]
pub struct ChannelAttachment {
    pub kind: Option<AttachmentKind>,
    pub name: Option<String>,
    pub path: Option<String>,
    pub mime_type: Option<String>,
    pub content: Option<String>,
    pub bytes: Option<f64>,
    pub metadata: Vec<(String, JsValue)>,
}
impl ChannelAttachment {
    /// Key order and omission follow the baseline object literal.
    pub fn to_js(&self) -> JsValue {
        let mut out = JsObject::new();
        out.insert(
            "type".into(),
            JsValue::String(self.kind.unwrap_or(AttachmentKind::Unknown).as_str().into()),
        );
        if let Some(name) = &self.name {
            out.insert("name".into(), JsValue::String(name.as_str().into()));
        }
        if let Some(path) = &self.path {
            out.insert("path".into(), JsValue::String(path.as_str().into()));
        }
        if let Some(mime_type) = &self.mime_type {
            out.insert(
                "mimeType".into(),
                JsValue::String(mime_type.as_str().into()),
            );
        }
        if let Some(content) = &self.content {
            out.insert("content".into(), JsValue::String(content.as_str().into()));
        }
        if let Some(bytes) = self.bytes {
            out.insert("bytes".into(), JsValue::Number(bytes));
        }
        if !self.metadata.is_empty() {
            let mut metadata = JsObject::new();
            for (key, value) in &self.metadata {
                metadata.insert(key.as_str().into(), value.clone());
            }
            out.insert("metadata".into(), JsValue::Object(metadata));
        }
        JsValue::Object(out)
    }
}

/// The response surface [`ImAttachmentStore`] reads from `fetch`: the buffered
/// body and the `content-type` header the baseline falls back to for the mime
/// guess.
///
/// The transport-level guards (`response.ok`, the declared `content-length`
/// check, the 50 MiB received cap, the 60 s header deadline) belong to the
/// fetcher: `WeixinCdnClient::download` already enforces all four with the
/// baseline's exact messages (#1823), so re-deriving them here would mean two
/// implementations of one rule.
pub struct FetchedAttachment {
    pub bytes: Vec<u8>,
    pub content_type: Option<String>,
}
pub type AttachmentFetchFuture<'a> = Pin<
    Box<dyn std::future::Future<Output = Result<FetchedAttachment, AttachmentError>> + Send + 'a>,
>;

pub trait AttachmentFetcher: Send + Sync {
    fn fetch<'a>(&'a self, url: &'a str, headers: &'a HeaderMap) -> AttachmentFetchFuture<'a>;
}

/// `transform?: (buffer: Buffer) => Uint8Array | Promise<Uint8Array>`. Every
/// weixin transform (`decryptWeixinImageOrThrow` / `decryptWeixinFile`) is
/// synchronous and may throw into the caller's per-item diagnostics, so the
/// port keeps the sync shape.
pub type AttachmentTransform =
    Arc<dyn Fn(Vec<u8>) -> Result<Vec<u8>, AttachmentError> + Send + Sync>;

/// `SaveAttachmentFromUrlInput`.
pub struct SaveAttachmentFromUrlInput {
    pub url: String,
    /// `input.headers`; weixin never sets any, but the baseline keeps the
    /// option on the declaration.
    pub headers: HeaderMap,
    pub chat_id: String,
    pub message_id: String,
    pub kind: AttachmentKind,
    pub name: Option<String>,
    pub mime_type: Option<String>,
    pub bytes: Option<f64>,
    pub metadata: Vec<(String, JsValue)>,
    pub transform: Option<AttachmentTransform>,
}

/// `ImAttachmentStore`.
pub struct ImAttachmentStore {
    root_dir: PathBuf,
    channel_key: String,
    max_bytes: usize,
    fetcher: Arc<dyn AttachmentFetcher>,
}
impl ImAttachmentStore {
    pub fn new(
        root_dir: impl Into<PathBuf>,
        channel_key: impl Into<String>,
        max_bytes: usize,
        fetcher: Arc<dyn AttachmentFetcher>,
    ) -> Self {
        let root_dir = root_dir.into();
        // `resolve(options.rootDir)`.
        let root_dir = std::path::absolute(&root_dir).unwrap_or(root_dir);
        Self {
            root_dir,
            channel_key: channel_key.into(),
            max_bytes,
            fetcher,
        }
    }

    /// The `maxBytes` this store was built with. The weixin dispatcher uses it
    /// to tell a download that `saveFromUrl` rejects before it touches the
    /// transport from one that has to wait for bytes.
    pub(crate) fn max_bytes(&self) -> usize {
        self.max_bytes
    }

    /// `saveFromUrl`, in the baseline's order: declared-size guard, fetch,
    /// transform, post-transform size guard, mime normalization, mkdir,
    /// filename, containment check, write, `stat`.
    pub async fn save_from_url(
        &self,
        input: SaveAttachmentFromUrlInput,
    ) -> Result<ChannelAttachment, AttachmentError> {
        if let Some(bytes) = input.bytes
            && bytes > self.max_bytes as f64
        {
            let label = input.name.clone().unwrap_or_else(|| input.url.clone());
            return Err(AttachmentError::error(format!(
                "Attachment {label} is {} bytes (limit {}).",
                crate::js_value::number_string(bytes),
                self.max_bytes
            )));
        }

        let fetched = self.fetcher.fetch(&input.url, &input.headers).await?;

        let mut buffer = fetched.bytes;
        if let Some(transform) = &input.transform {
            buffer = transform(buffer)?;
        }
        if buffer.len() > self.max_bytes {
            return Err(AttachmentError::error(format!(
                "Attachment is {} bytes (limit {}).",
                buffer.len(),
                self.max_bytes
            )));
        }

        // `input.mimeType ?? responseMimeType`, where the header is split at
        // the first `;`, trimmed, and `|| undefined` when empty.
        let response_mime = fetched
            .content_type
            .as_deref()
            .and_then(|value| value.split(';').next())
            .filter(|value| !js_trim(value).is_empty())
            .map(|value| js_trim(value).to_string());
        let mime_type = normalize_attachment_mime_type(
            input.mime_type.clone().or(response_mime),
            &buffer,
            input.kind,
        );

        let dir = self
            .root_dir
            .join(safe_path_part(&self.channel_key))
            .join(safe_path_part(&input.chat_id))
            .join(safe_path_part(&input.message_id));
        create_private_dir(&dir).await.map_err(io_error)?;

        let filename = safe_filename(input.name.as_deref(), mime_type.as_deref(), input.kind);
        let path = dir.join(&filename);
        if !is_path_within_directory(&path, &dir) {
            return Err(AttachmentError::error(
                "Attachment path escaped target directory.",
            ));
        }
        write_private_file(&path, &buffer).await.map_err(io_error)?;
        let size = tokio::fs::metadata(&path).await.map_err(io_error)?.len();

        let mut metadata: Vec<(String, JsValue)> = vec![
            (
                "channelKey".into(),
                JsValue::String(self.channel_key.as_str().into()),
            ),
            (
                "chatId".into(),
                JsValue::String(input.chat_id.as_str().into()),
            ),
            (
                "messageId".into(),
                JsValue::String(input.message_id.as_str().into()),
            ),
            (
                "sourceUrl".into(),
                JsValue::String(input.url.as_str().into()),
            ),
        ];
        // `{ ...base, ...input.metadata }`: an existing key keeps its original
        // position, a new key appends.
        for (key, value) in input.metadata {
            match metadata.iter_mut().find(|(name, _)| name == &key) {
                Some(slot) => slot.1 = value,
                None => metadata.push((key, value)),
            }
        }

        Ok(ChannelAttachment {
            kind: Some(input.kind),
            name: Some(filename),
            path: Some(path.to_string_lossy().into_owned()),
            mime_type,
            content: None,
            bytes: Some(size as f64),
            metadata,
        })
    }
}

/// Node `fs` failures (`EACCES`, `ENOENT`, …) reach `formatWeixinError` as an
/// `Error`. The message text beyond the error class is Node-specific
/// (`code: description, syscall 'path'`), so the port keeps the class and lets
/// the enclosing per-item `catch` turn it into the diagnostic, which is what
/// both baselines do with an fs failure.
fn io_error(error: std::io::Error) -> AttachmentError {
    AttachmentError::error(error.to_string())
}

/// `mkdir(dir, { recursive: true, mode: 0o700 })`.
async fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    let mut builder = tokio::fs::DirBuilder::new();
    builder.recursive(true);
    // `tokio::fs::DirBuilder::mode` is an inherent unix-only method.
    #[cfg(unix)]
    builder.mode(0o700);
    builder.create(dir).await
}

/// `writeFile(path, buffer, { mode: 0o600 })`.
async fn write_private_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(path).await?;
    file.write_all(bytes).await?;
    file.flush().await
}

fn normalize_attachment_mime_type(
    mime_type: Option<String>,
    buffer: &[u8],
    kind: AttachmentKind,
) -> Option<String> {
    if kind != AttachmentKind::Image {
        return mime_type;
    }
    detect_image_mime(buffer).map(str::to_string).or(mime_type)
}

/// `detectImageMime` magic prefixes (ImAttachmentStore.ts:130-145).
pub fn detect_image_mime(buffer: &[u8]) -> Option<&'static str> {
    if buffer.len() >= 8 && buffer[..8] == [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a] {
        return Some("image/png");
    }
    if buffer.len() >= 3 && buffer[..3] == [0xff, 0xd8, 0xff] {
        return Some("image/jpeg");
    }
    if buffer.len() >= 6 && (&buffer[..6] == b"GIF87a" || &buffer[..6] == b"GIF89a") {
        return Some("image/gif");
    }
    if buffer.len() >= 12 && &buffer[..4] == b"RIFF" && &buffer[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    None
}

/// `extensionForMime`.
fn extension_for_mime(mime_type: Option<&str>) -> &'static str {
    match mime_type.map(str::to_lowercase).as_deref() {
        Some("image/jpeg") => "jpg",
        Some("image/png") => "png",
        Some("image/gif") => "gif",
        Some("image/webp") => "webp",
        Some("application/pdf") => "pdf",
        Some("text/plain") => "txt",
        _ => "bin",
    }
}

/// `safePathPart`: `[^a-zA-Z0-9._-]+` → `_`, then 120 UTF-16 units, then
/// `|| "unknown"`.
pub fn safe_path_part(value: &str) -> String {
    let collapsed = collapse_runs(value, |unit| {
        !u8::try_from(unit)
            .is_ok_and(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    });
    truncate_units(&collapsed, 120, "unknown")
}

/// Apply `/[class]+/g → "_"` over UTF-16 code units, so unit counts match JS
/// `.length` and `.slice`.
fn collapse_runs(value: &str, forbidden: fn(u16) -> bool) -> String {
    let mut units = Vec::new();
    let mut in_run = false;
    for unit in value.encode_utf16() {
        if forbidden(unit) {
            if !in_run {
                units.push(u16::from(b'_'));
                in_run = true;
            }
        } else {
            units.push(unit);
            in_run = false;
        }
    }
    String::from_utf16_lossy(&units)
}

/// `.slice(0, limit) || fallback`.
fn truncate_units(value: &str, limit: usize, fallback: &str) -> String {
    let units: Vec<u16> = value.encode_utf16().take(limit).collect();
    let text = String::from_utf16_lossy(&units);
    if text.is_empty() {
        fallback.to_string()
    } else {
        text
    }
}

/// `safeFilename(name, mimeType, type)`.
pub fn safe_filename(name: Option<&str>, mime_type: Option<&str>, kind: AttachmentKind) -> String {
    let extension = extension_for_mime(mime_type);
    let fallback = if kind == AttachmentKind::Image {
        format!("image.{extension}")
    } else {
        format!("attachment.{extension}")
    };
    let trimmed = name.map(js_trim).filter(|value| !value.is_empty());
    let raw = posix_basename(trimmed.unwrap_or(&fallback));
    let cleaned = truncate_units(&sanitize_basename(raw), 180, &fallback);
    if kind == AttachmentKind::Image {
        return replace_extension(&cleaned, extension);
    }
    if extname(&cleaned).is_some() {
        cleaned
    } else {
        format!("{cleaned}.{extension}")
    }
}

/// `raw.replace(/[\x00-\x1f\\/:*?"<>|]+/g, "_")`.
fn sanitize_basename(value: &str) -> String {
    collapse_runs(value, |unit| {
        u8::try_from(unit).is_ok_and(|byte| {
            byte <= 0x1f
                || matches!(
                    byte,
                    b'\\' | b'/' | b':' | b'*' | b'?' | b'"' | b'<' | b'>' | b'|'
                )
        })
    })
}

/// Node `path.basename` on POSIX (the baseline runs on macOS/Linux).
fn posix_basename(value: &str) -> &str {
    let trimmed = value.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(index) => &trimmed[index + 1..],
        None => trimmed,
    }
}

/// Node `path.extname`: the last `.` of the basename, `None` when it is the
/// first character or absent.
fn extname(value: &str) -> Option<&str> {
    let base = posix_basename(value);
    let index = base.rfind('.')?;
    (index != 0).then(|| &base[index..])
}

/// `replaceExtension`: drop the old extension, then `base || "image"`.
fn replace_extension(name: &str, extension: &str) -> String {
    let base = match extname(name) {
        Some(ext) => &name[..name.len() - ext.len()],
        None => name,
    };
    let base = if base.is_empty() { "image" } else { base };
    format!("{base}.{extension}")
}

/// `isPathWithinDirectory`: `relative(dir, candidate)` is empty or neither
/// `..`-rooted nor absolute. Both sides are already resolved (the store joins
/// single sanitized segments onto an absolute root), so the relative form is
/// exactly the joined filename, and Node's `startsWith("..")` is a prefix test
/// on that text — `..foo.txt` is a *single* component that is still inside the
/// directory lexically, yet the baseline rejects it (Node `path.relative`
/// returns `..foo.txt`, which starts with `..`). Keeping the prefix test fails
/// closed for every input the store can produce.
pub fn is_path_within_directory(candidate: &Path, directory: &Path) -> bool {
    let Ok(rest) = candidate.strip_prefix(directory) else {
        return false;
    };
    if rest.as_os_str().is_empty() {
        return true;
    }
    if rest.is_absolute() {
        return false;
    }
    // Defensive: a nested `..` component cannot come out of `safe_filename`
    // (every separator is rewritten to `_`), but rejecting it keeps the guard
    // fail-closed if that ever changes.
    if rest
        .components()
        .any(|component| component.as_os_str() == "..")
    {
        return false;
    }
    !rest.to_string_lossy().starts_with("..")
}

/// `guessMimeTypeFromName` (ImAttachmentDelivery.ts:73-85).
pub fn guess_mime_type_from_name(name: Option<&str>) -> Option<&'static str> {
    let lower = name.unwrap_or_default().to_lowercase();
    if lower.ends_with(".png") {
        return Some("image/png");
    }
    if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        return Some("image/jpeg");
    }
    if lower.ends_with(".gif") {
        return Some("image/gif");
    }
    if lower.ends_with(".webp") {
        return Some("image/webp");
    }
    if lower.ends_with(".pdf") {
        return Some("application/pdf");
    }
    if lower.ends_with(".txt") || lower.ends_with(".log") {
        return Some("text/plain");
    }
    if lower.ends_with(".md") {
        return Some("text/markdown");
    }
    if lower.ends_with(".json") {
        return Some("application/json");
    }
    None
}

/// `guessMimeTypeFromUrl(url, fallback)`: the URL's pathname through
/// `guessMimeTypeFromName`, or the fallback when the URL does not parse.
pub fn guess_mime_type_from_url(url: &str, fallback: Option<&str>) -> Option<String> {
    match url::Url::parse(url) {
        Ok(parsed) => guess_mime_type_from_name(Some(parsed.path()))
            .or(fallback)
            .map(str::to_string),
        Err(_) => fallback.map(str::to_string),
    }
}

// ── ImAttachmentDelivery ────────────────────────────────────────────────────

/// `PreparedImAttachment`.
#[derive(Debug)]
pub struct PreparedImAttachment {
    pub name: String,
    pub mime_type: Option<String>,
    pub buffer: Vec<u8>,
    pub file_type: PreparedFileType,
    /// `...(attachment.path ? { path } : {})`.
    pub path: Option<String>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreparedFileType {
    Image,
    File,
}
impl PreparedFileType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::File => "file",
        }
    }
}

/// `GatewayOutboundAttachment` (gateway/protocol/types.ts:144-153).
#[derive(Clone, Debug, Default)]
pub struct OutboundAttachment {
    pub kind: Option<AttachmentKind>,
    pub name: Option<String>,
    pub path: Option<String>,
    pub mime_type: Option<String>,
    pub content: Option<String>,
    /// Mirrored from `GatewayOutboundAttachment`; the weixin `sendMedia`
    /// caller that reads them is #1824.
    #[allow(dead_code)]
    pub bytes: Option<f64>,
    pub source: Option<String>,
    #[allow(dead_code)]
    pub metadata: Vec<(String, JsValue)>,
}
impl OutboundAttachment {
    /// `attachment.path?.split(/[\\/]/).pop()`.
    fn path_basename(&self) -> Option<String> {
        self.path
            .as_deref()
            .map(|path| path.rsplit(['\\', '/']).next().unwrap_or(path).to_string())
    }
}

/// The caller-owned transport (`options.sendPrepared` /
/// `options.sendTextFallback`). The futures are `'static` so a sink can move
/// the prepared buffer into its own request, exactly like the TS closures
/// capture their channel.
pub trait ImAttachmentDeliverySink: Send + Sync {
    fn send_prepared(
        &self,
        prepared: PreparedImAttachment,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<(), AttachmentError>> + Send>>;
    fn send_text_fallback(
        &self,
        text: String,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<(), AttachmentError>> + Send>>;
}

/// `logger?.error?.(...)`; only the error channel is used here.
pub type ImAttachmentErrorLogger = Arc<dyn Fn(&str) + Send + Sync>;

pub struct ImAttachmentDeliveryOptions {
    pub max_bytes: usize,
    pub sink: Arc<dyn ImAttachmentDeliverySink>,
    pub logger: Option<ImAttachmentErrorLogger>,
}

/// `ImAttachmentDelivery`.
pub struct ImAttachmentDelivery {
    options: ImAttachmentDeliveryOptions,
}
impl ImAttachmentDelivery {
    pub fn new(options: ImAttachmentDeliveryOptions) -> Self {
        Self { options }
    }

    /// `send`: a `local_path` source needs authorization first; every other
    /// failure logs and falls back to text. Returns whether the attachment
    /// itself was delivered.
    pub async fn send(&self, attachment: &OutboundAttachment) -> bool {
        if attachment.source.as_deref() == Some("local_path")
            && let Some(path) = &attachment.path
        {
            let _ = self
                .options
                .sink
                .send_text_fallback(format!("附件 {path} 需要授权后才能发送。"))
                .await;
            return false;
        }
        match self.prepare(attachment).await {
            Ok(prepared) => match self.options.sink.send_prepared(prepared).await {
                Ok(()) => true,
                Err(error) => {
                    self.log_failure(&error);
                    let _ = self
                        .options
                        .sink
                        .send_text_fallback(format_im_attachment_fallback(attachment))
                        .await;
                    false
                }
            },
            Err(error) => {
                self.log_failure(&error);
                let _ = self
                    .options
                    .sink
                    .send_text_fallback(format_im_attachment_fallback(attachment))
                    .await;
                false
            }
        }
    }

    fn log_failure(&self, error: &AttachmentError) {
        if let Some(logger) = &self.options.logger {
            logger(&format!("IM attachment send failed: {}", error.formatted()));
        }
    }

    /// `prepare`: base64 content first, then the path read; neither present is
    /// a hard error. `fileType` is `image` for an image type or an `image/*`
    /// mime.
    async fn prepare(
        &self,
        attachment: &OutboundAttachment,
    ) -> Result<PreparedImAttachment, AttachmentError> {
        let raw_name = attachment
            .name
            .clone()
            .or_else(|| attachment.path_basename())
            .unwrap_or_else(|| "attachment".into());
        let name = sanitize_delivery_filename(&raw_name);
        // `attachment.content ? ... : attachment.path ? ... : undefined` — an
        // empty content string is falsy, so the path wins.
        let buffer = if let Some(content) = attachment
            .content
            .as_deref()
            .filter(|content| !content.is_empty())
        {
            node_base64_decode(content)
        } else if let Some(path) = &attachment.path {
            tokio::fs::read(path).await.map_err(io_error)?
        } else {
            return Err(AttachmentError::error(
                "attachment has neither content nor path",
            ));
        };
        if buffer.len() > self.options.max_bytes {
            return Err(AttachmentError::error(format!(
                "attachment {name} is {} bytes (limit {})",
                buffer.len(),
                self.options.max_bytes
            )));
        }
        let mime_type = attachment
            .mime_type
            .clone()
            .or_else(|| guess_mime_type_from_name(Some(&name)).map(str::to_string));
        let file_type = if attachment.kind == Some(AttachmentKind::Image)
            || mime_type
                .as_deref()
                .is_some_and(|mime| mime.starts_with("image/"))
        {
            PreparedFileType::Image
        } else {
            PreparedFileType::File
        };
        Ok(PreparedImAttachment {
            name,
            mime_type,
            buffer,
            file_type,
            path: attachment.path.clone(),
        })
    }
}

/// `sanitizeFilename`: `[\x00-\x1f\\/:*?"<>|]+` → `_`, then `.trim()`, then
/// 180 UTF-16 units, then `|| "attachment.bin"`.
fn sanitize_delivery_filename(name: &str) -> String {
    truncate_units(js_trim(&sanitize_basename(name)), 180, "attachment.bin")
}

/// `formatImAttachmentFallback`: `附件发送失败：{name}{，可在本机查看：path}`.
pub fn format_im_attachment_fallback(attachment: &OutboundAttachment) -> String {
    let name = attachment
        .name
        .clone()
        .or_else(|| attachment.path_basename())
        .unwrap_or_else(|| "附件".into());
    let path_text = attachment
        .path
        .as_ref()
        .map(|path| format!("，可在本机查看：{path}"))
        .unwrap_or_default();
    format!("附件发送失败：{name}{path_text}")
}

/// Node `Buffer.from(value, "base64")`: each UTF-16 unit narrows to one byte
/// (`Ł` contributes `A`), characters outside the alphabet are skipped,
/// `-`/`_` are the URL-safe aliases, and `=` ends the input. Partial trailing
/// bits are discarded.
pub fn node_base64_decode(value: &str) -> Vec<u8> {
    fn sextet(byte: u8) -> Option<u32> {
        match byte {
            b'A'..=b'Z' => Some(u32::from(byte - b'A')),
            b'a'..=b'z' => Some(u32::from(byte - b'a') + 26),
            b'0'..=b'9' => Some(u32::from(byte - b'0') + 52),
            b'+' | b'-' => Some(62),
            b'/' | b'_' => Some(63),
            _ => None,
        }
    }
    let mut out = Vec::new();
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    for unit in value.encode_utf16() {
        let byte = u8::try_from(unit & 0xff).unwrap_or(0);
        if byte == b'=' {
            break;
        }
        let Some(value) = sextet(byte) else {
            continue;
        };
        acc = (acc << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(((acc >> bits) & 0xff) as u8);
        }
    }
    out
}

#[cfg(test)]
mod tests;
