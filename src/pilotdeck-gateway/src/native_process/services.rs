use super::{Options, assembly::Recorders};
use crate::{
    config::Env,
    metadata::{object, text_field},
    runtime::{
        context::compaction::js_truthy,
        project::{ProjectResources, ProjectRuntimeRegistry},
        snapshot::{SnapshotProjectResolver, SnapshotProjects},
        turn::{
            submit::{NativeSubmitSnapshots, SubmitError, SubmitFuture, SubmitSnapshots},
            submit_host::{SubmitAttachmentLease, SubmitCwdService, SubmitHostServices},
        },
    },
    storage::{ContentAddressedWorkspaceSnapshotRecorder, RunRef},
    upload::UploadStore,
};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64_STANDARD};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

pub struct Services {
    pub options: Options,
    pub project: Arc<ProjectRuntimeRegistry>,
    pub env: Env,
    pub recorders: Recorders,
    pub owner: crate::runtime::session::SessionOwner,
    /// Extracted once at startup from `PersistenceResources`; `None` when
    /// `--storage-root` was not supplied and snapshots are disabled.
    pub snapshot_recorder: Option<Arc<ContentAddressedWorkspaceSnapshotRecorder>>,
    pub upload_store: Arc<UploadStore>,
}
fn base64_encode(bytes: &[u8]) -> String {
    BASE64_STANDARD.encode(bytes)
}

/// TS AttachmentResolver.ts:36 — `DEFAULT_MAX_FILE_BYTES = 1_000_000` (1 MB):
/// larger file attachments are skipped with an `Attachment … skipped`
/// diagnostic instead of being resolved inline.
const MAX_INLINE_FILE_BYTES: u64 = 1_000_000;
/// TS AttachmentResolver.ts:37 — `DEFAULT_MAX_IMAGE_BYTES = 5_242_880` (5 MiB):
/// larger images are skipped with an `Image … skipped` diagnostic instead of
/// being base64-inlined (verified against the TS source by the
/// `_tmp/20261105-issue-1321-fix-r1/ts-probe/` probe).
const MAX_INLINE_IMAGE_BYTES: u64 = 5_242_880;
/// TS AttachmentResolver.ts:38 — `DEFAULT_BYTES_PER_PDF_PAGE = 102_400`
/// (100 KB): the divisor of the pdf size→page estimate
/// `Math.max(1, Math.round(size / bytesPerPdfPage))` (resolver :304).
const BYTES_PER_PDF_PAGE: u64 = 102_400;
/// TS GatewayAttachmentTurnComposer.ts:21 — `ATTACHMENT_PATH_NOTE_MARKER`.
const ATTACHMENT_PATH_NOTE_MARKER: &str = "[Registered attachment files in this session:]";
/// TS GatewayAttachmentTurnComposer.ts:95 — the funasr install command the
/// audio guidance names (`installCommand = "npm run install:asr"` default).
/// The native process has no TS package root to compute
/// `getPilotDeckInstallCommand()` (projectMcpSpec.ts:27-37), so the composer
/// default is used verbatim.
const FUNASR_INSTALL_COMMAND: &str = "npm run install:asr";
/// TS AttachmentResolver.ts:40-61 — `TEXT_EXTENSIONS`: the whitelisted
/// plain-text extensions a path-based file attachment is inlined from.
const TEXT_EXTENSIONS: &[&str] = &[
    ".txt", ".md", ".json", ".yaml", ".yml", ".csv", ".tsv", ".xml", ".html", ".css", ".ts",
    ".tsx", ".js", ".jsx", ".py", ".go", ".rs", ".java", ".sh", ".log",
];
/// TS GatewayAttachmentTurnComposer.ts:22-40 —
/// `READ_FILE_BINARY_ATTACHMENT_EXTENSIONS` (the same list as
/// AttachmentResolver.ts:62-80 `BINARY_CONTAINER_EXTENSIONS`): attachments
/// the guidance calls out as not directly inspectable with read_file.
const BINARY_CONTAINER_EXTENSIONS: &[&str] = &[
    ".zip", ".gz", ".tar", ".7z", ".rar", ".doc", ".docx", ".ppt", ".pptx", ".xls", ".xlsx",
    ".odt", ".ods", ".odp", ".pages", ".key", ".numbers",
];
/// TS GatewayAttachmentTurnComposer.ts:283 — the audio suffixes
/// `isAudioAttachment` matches case-insensitively.
const AUDIO_EXTENSIONS: &[&str] = &[
    ".aac", ".flac", ".m4a", ".mp3", ".oga", ".ogg", ".opus", ".wav", ".webm",
];
/// TS GatewayAttachmentTurnComposer.ts:118 — the plain (no-diagnostics)
/// guidance closing the path note.
const PLAIN_PATH_NOTE_GUIDANCE: &str = "These are path references for reuse. If an image/PDF is \
     already visible in this turn, do not call read_file just to view it.";

/// One attachment as the composer sees it (the TS ChannelAttachment slice
/// read by GatewayAttachmentTurnComposer.ts:211-253). A missing, empty, or
/// non-UTF-8 path leaves `path` empty; the composer's own
/// `if (!attachment.path) continue;` (:229) drops those silently.
struct AttachEntry {
    /// `attachment.type === "image"` (composer :212, :234).
    image_type: bool,
    /// `attachment.type === "text"` (composer :224).
    text_type: bool,
    path: String,
    /// A truthy-but-non-UTF-8 `path` (a lone surrogate), lossy-spelled for
    /// the not-found diagnostic only (P1-d): it never reaches the lease
    /// guard, an open, or allowedReadFiles.
    invalid_path: Option<String>,
    /// Raw `attachment.name` ("" when absent or not UTF-8).
    name: String,
    mime: String,
    /// Raw nonempty `attachment.content`, inlined before any path handling
    /// (composer :212, :224).
    content: Option<String>,
    /// Raw `attachment.bytes`: only the content image block forwards it
    /// verbatim (composer :218); path-based blocks report the bytes actually
    /// read (AttachmentResolver.ts:274).
    declared_bytes: Option<JsValue>,
    /// Truthy `metadata.channelKey` (composer :181, :238, :244, :250): only
    /// channel-registered attachments enter allowedReadFiles and the path
    /// note. Uploaded lease attachments (bundle :57-62 — uploadId,
    /// attachmentId, relativePath, sha256, no channelKey) are never
    /// registered.
    channel_key: bool,
}

/// A path attachment that resolved inside the lease root and opened as a
/// regular file. allowedReadFiles and the path-note lines come from these,
/// mirroring TS `collectRegisteredAttachmentReadFiles`
/// (GatewayAttachmentTurnComposer.ts:173-193, `kind === "file"` only).
struct RegisteredAttachment {
    resolved: PathBuf,
    /// Raw attachment name; the note cleans it (composer :108-109).
    name: String,
    /// `isReadFileInspectableAttachment` (composer :153-165) — the input the
    /// diagnostics guidance branches on (:142-151).
    inspectable: bool,
}

/// Outcome of the per-attachment lease guard.
enum LeaseGuard {
    /// The path canonicalizes under the canonical lease root; inlining and
    /// allowedReadFiles use this resolved path, never the client spelling.
    Resolved(PathBuf),
    /// The path is lexically inside the lease root but does not resolve
    /// (missing file / dangling symlink — a lease TTL cleanup race): the
    /// attachment becomes a TS-shaped not-found diagnostic instead of
    /// failing the whole turn. A missing lease root (no uploads yet) takes
    /// the same branch, never a panic.
    Unresolved(std::io::Error),
}

/// `Err` rejects the turn: the path is not lexically inside the lease root
/// (the canonical `<project>/.tmp/chat-upload-leases/`, computed once per
/// prepare) or contains `..`, or it canonicalizes outside that root — a
/// symlink escape — or its canonicalization fails with a non-`NotFound` error
/// (P2: ELOOP/ENOTDIR/EACCES are hard failures; only the missing-file
/// `NotFound` softens into the TS-shaped not-found diagnostic). Guards P1-1:
/// only hard-linked lease files may be inlined or added to allowedReadFiles;
/// arbitrary client-supplied paths are rejected before they reach
/// allowedReadFiles or any read (same shape as `read_at`'s
/// PROJECT_PATH_FORBIDDEN symlink check in upload/mod.rs).
fn lease_guard(
    path: &str,
    lexical_root: &Path,
    canonical_root: Option<&Path>,
) -> Result<LeaseGuard, SubmitError> {
    let p = Path::new(path);
    if p.components().any(|c| c == Component::ParentDir) || !p.starts_with(lexical_root) {
        return Err(failure(
            "Attachment path is outside the permitted lease directory.",
        ));
    }
    match fs::canonicalize(p) {
        Ok(resolved) => match canonical_root {
            Some(root) if resolved.starts_with(root) => Ok(LeaseGuard::Resolved(resolved)),
            // The root is valid but the path resolved outside it — a
            // symlink escape: reject the turn (P1-1).
            Some(_) => Err(failure(
                "Attachment path is outside the permitted lease directory.",
            )),
            // P0-a (codex): there is no valid canonical root (the lease root
            // was swapped outside the project) — the resolved spelling must
            // never pass by prefix. The attachment takes the not-found
            // branch: no read, no registration.
            None => Ok(LeaseGuard::Unresolved(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "attachment lease root is unavailable",
            ))),
        },
        // P2: a missing file (lease TTL cleanup race) is the TS stat ENOENT
        // — a not-found diagnostic, never a whole-turn failure.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(LeaseGuard::Unresolved(error))
        }
        Err(error) => Err(failure(format!(
            "Attachment lease path cannot be resolved: {error}."
        ))),
    }
}

/// Node `extname` on the last path segment, lowercased with its leading dot
/// ("" when there is no extension) — the shape both TS files compare against
/// (AttachmentResolver.ts:148, GatewayAttachmentTurnComposer.ts:161).
fn extension_of(source: &str) -> String {
    Path::new(source)
        .extension()
        .map(|ext| format!(".{}", ext.to_string_lossy().to_lowercase()))
        .unwrap_or_default()
}

/// TS AttachmentResolver.ts:148 — prefer the trimmed attachment name's
/// extension, else the path's: upload stores use an opaque id on disk, so
/// the original filename carries the format.
fn resolver_extension(entry: &AttachEntry) -> String {
    let trimmed = entry.name.trim();
    extension_of(if trimmed.is_empty() {
        entry.path.as_str()
    } else {
        trimmed
    })
}

/// TS AttachmentResolver.ts:81-87 — the `IMAGE_MIME` extension map.
fn image_mime_for_ext(ext: &str) -> Option<&'static str> {
    match ext {
        ".png" => Some("image/png"),
        ".jpg" | ".jpeg" => Some("image/jpeg"),
        ".gif" => Some("image/gif"),
        ".webp" => Some("image/webp"),
        _ => None,
    }
}

/// The image request's mime: the declared `mimeType`, else the extension
/// map, else none (AttachmentResolver.ts:214-227).
fn image_mime(entry: &AttachEntry) -> Option<String> {
    if !entry.mime.is_empty() {
        return Some(entry.mime.clone());
    }
    image_mime_for_ext(&extension_of(&entry.path)).map(str::to_owned)
}

/// TS GatewayAttachmentTurnComposer.ts:280-284 — the mime prefix first, else
/// the audio suffixes matched case-insensitively on the path (name as the
/// fallback). `is_audio_shape` takes the spelling explicitly so the
/// lone-surrogate branch (P1-d) can test the lossy path the way TS tests the
/// surrogate one (path wins over name there too).
fn is_audio_shape(mime: &str, path_or_name: &str) -> bool {
    if mime.to_lowercase().starts_with("audio/") {
        return true;
    }
    let lower = path_or_name.to_lowercase();
    AUDIO_EXTENSIONS.iter().any(|ext| lower.ends_with(ext))
}

fn is_audio_attachment(entry: &AttachEntry) -> bool {
    let path_or_name = if entry.path.is_empty() {
        entry.name.as_str()
    } else {
        entry.path.as_str()
    };
    is_audio_shape(&entry.mime, path_or_name)
}

/// TS GatewayAttachmentTurnComposer.ts:153-165 —
/// `isReadFileInspectableAttachment`.
fn is_read_file_inspectable(entry: &AttachEntry) -> bool {
    let mime = entry.mime.to_lowercase();
    if entry.image_type || mime.starts_with("image/") {
        return true;
    }
    if mime == "application/pdf"
        || mime.starts_with("text/")
        || mime == "application/json"
        || mime.ends_with("+json")
    {
        return true;
    }
    let path_or_name = if entry.name.is_empty() {
        entry.path.as_str()
    } else {
        entry.name.as_str()
    };
    let ext = extension_of(path_or_name);
    if ext == ".pdf" || ext == ".ipynb" {
        return true;
    }
    !BINARY_CONTAINER_EXTENSIONS.contains(&ext.as_str())
}

/// TS GatewayAttachmentTurnComposer.ts:286-295 — the funasr hint only names
/// audio paths inside the project (lexical `relative`, no symlink chase);
/// anything else is left to the generic "project-local" wording.
fn map_audio_path_for_funasr(path: &str, project_root: &Path) -> Option<String> {
    let relative = Path::new(path).strip_prefix(project_root).ok()?;
    if relative.as_os_str().is_empty() {
        return None;
    }
    Some(path.to_string())
}

/// TS GatewayAttachmentTurnComposer.ts:108-109 —
/// `String(name || fallback).replace(/[\r\n]+/g, " ").trim() || fallback`.
fn clean_attachment_name(raw: &str, fallback: &str) -> String {
    let base = if raw.is_empty() { fallback } else { raw };
    let mut cleaned = String::with_capacity(base.len());
    let mut previous_was_break = false;
    for character in base.chars() {
        if character == '\r' || character == '\n' {
            if !previous_was_break {
                cleaned.push(' ');
            }
            previous_was_break = true;
        } else {
            cleaned.push(character);
            previous_was_break = false;
        }
    }
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        fallback.to_string()
    } else {
        trimmed.to_string()
    }
}

/// The flat canonical image block (TS AttachmentResolver.ts:267-275):
/// `{type, source: "base64", data, mimeType, bytes}`. Every in-repo consumer
/// reads `source`/`data`/`mimeType` at the top level
/// (runtime/session/factory.rs `valid_snapshot_block`,
/// runtime/turn/projector.rs `canonical_content_text`,
/// runtime/model/count_body.rs).
fn flat_image_block(data: &str, mime: &str, bytes: Option<f64>) -> JsValue {
    let mut block = JsObject::new();
    block.insert("type".into(), "image".into());
    block.insert("source".into(), "base64".into());
    block.insert("data".into(), JsValue::from(data));
    block.insert("mimeType".into(), JsValue::from(mime));
    if let Some(bytes) = bytes {
        block.insert("bytes".into(), JsValue::Number(bytes));
    }
    JsValue::Object(block)
}

/// `Image attachment not found: ${absolute} (${error}).` — the missing-file
/// diagnostic of TS AttachmentResolver (AttachmentResolver.ts:191-200). A
/// read failure here (lease TTL cleanup, permissions, a directory swap)
/// uses the same channel so the attachment is never silently dropped.
fn image_not_found(path: &str, error: &std::io::Error) -> String {
    format!("Image attachment not found: {path} ({error}).")
}

/// `Attachment not found: ${absolute} (${error}).` — the missing-file
/// diagnostic of TS AttachmentResolver for non-image files
/// (AttachmentResolver.ts:134-143).
fn file_not_found(path: &str, error: &std::io::Error) -> String {
    format!("Attachment not found: {path} ({error}).")
}

/// The resolver request kind the composer picks for a path attachment
/// (GatewayAttachmentTurnComposer.ts:230-252): audio is skipped before any
/// request, so it is not a route.
enum AttachmentRoute {
    Image,
    Pdf,
    File,
}

/// composer :234-251 — the image request first (`type === "image"` or an
/// `image/` mime), then the pdf request (`application/pdf` mime or a `.pdf`
/// path suffix, case-insensitive), else the file request.
fn attachment_route(entry: &AttachEntry, path: &str) -> AttachmentRoute {
    if entry.image_type || entry.mime.starts_with("image/") {
        AttachmentRoute::Image
    } else if entry.mime == "application/pdf" || path.to_lowercase().ends_with(".pdf") {
        AttachmentRoute::Pdf
    } else {
        AttachmentRoute::File
    }
}

/// The not-found diagnostic in the TS resolver's own routing: image requests
/// say "Image attachment not found" (AttachmentResolver.ts:191-200), pdf
/// requests "PDF attachment not found" (:293-302), file requests "Attachment
/// not found" (:134-143).
fn attachment_not_found(entry: &AttachEntry, path: &str, error: &std::io::Error) -> String {
    match attachment_route(entry, path) {
        AttachmentRoute::Image => image_not_found(path, error),
        AttachmentRoute::Pdf => format!("PDF attachment not found: {path} ({error})."),
        AttachmentRoute::File => file_not_found(path, error),
    }
}

/// TS AttachmentResolver.ts:171 — the file too-large diagnostic.
fn file_too_large(path: &str, bytes: u64, limit: u64) -> String {
    format!("Attachment {path} is {bytes} bytes (limit {limit}); skipped.")
}

/// TS AttachmentResolver.ts:209 — the image too-large diagnostic.
fn image_too_large(path: &str, bytes: u64, limit: u64) -> String {
    format!(
        "Image {path} is {bytes} bytes (limit {limit}); skipped \
         (PilotDeck does not resize, intentional_difference §4.5)."
    )
}

/// Bounded inline read on the already-opened handle (codex#a/b): no second
/// path lookup between the guard and the bytes, and `Read::take(limit + 1)`
/// caps what enters memory. A read that fills the whole `limit + 1` budget
/// is itself the proof the file exceeds `limit` (读满即超限) — the oversize
/// file is never pulled in full.
fn read_bounded(file: &mut File, limit: u64) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// P0-b (codex, fix-r4 repositioned): the leaf/ancestor swap check between
/// the post-open revalidation and the read — `fs::metadata` of the
/// revalidated path against the opened handle's own fstat. Identity is
/// `(dev, inode)`: a link swapped between the two stats points the path at
/// a different file than the handle, and the mismatch is treated as
/// not-found (no read, no registration; allowedReadFiles keeps the
/// post-open canonical spelling either way).
#[cfg(unix)]
pub(super) fn attachment_identity_matches(before: &fs::Metadata, opened: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    before.dev() == opened.dev() && before.ino() == opened.ino()
}

#[cfg(not(unix))]
pub(super) fn attachment_identity_matches(_before: &fs::Metadata, _opened: &fs::Metadata) -> bool {
    // No stable cross-platform file identity: non-Unix leans on the
    // post-open canonicalize containment revalidation (step 2 of the
    // open-first sequence in compose_attachment_turn, which runs on every
    // platform) instead of this comparison.
    true
}

/// TS GatewayAttachmentTurnComposer.ts:125-151 — `attachmentDiagnosticsGuidance`.
fn attachment_diagnostics_guidance(
    entries: &[AttachEntry],
    has_inspectable: bool,
    project_root: &Path,
) -> String {
    // :131-140 — the audio form wins whenever ANY audio attachment exists
    // (P1-b: presence decides, not path mappability); a form with no
    // mappable path takes the generic hint (:138).
    let audio_attachments: Vec<&AttachEntry> = entries
        .iter()
        .filter(|entry| is_audio_attachment(entry))
        .collect();
    if !audio_attachments.is_empty() {
        let audio_paths: Vec<String> = audio_attachments
            .iter()
            .filter(|entry| !entry.path.is_empty())
            .filter_map(|entry| map_audio_path_for_funasr(&entry.path, project_root))
            .collect();
        let mapped_hint = if audio_paths.is_empty() {
            " Pass a project-local host path to transcribe_audio; paths outside this project \
             are rejected."
                .to_string()
        } else if audio_paths.len() == 1 {
            format!(
                " Pass the registered project-local path {} to transcribe_audio.",
                audio_paths[0]
            )
        } else {
            format!(
                " Pass the registered project-local paths {} to transcribe_audio.",
                audio_paths.join(", ")
            )
        };
        return format!(
            "Audio attachments are not readable with read_file. When the user asks for \
             transcription, subtitles, or audio analysis, use the funasr MCP server's \
             mcp__funasr__transcribe_audio tool.{mapped_hint} If that tool reports that its \
             runtime is missing, run {FUNASR_INSTALL_COMMAND} and retry the tool in this session."
        );
    }
    // :142-151 — no registered inspectable attachment left → the conversion
    // form; otherwise the read_file-scoped form.
    if !has_inspectable {
        return "Some attachments were not shown inline. These registered files are not \
                directly inspectable with read_file; ask for a supported export or \
                convert them before inspection."
            .to_string();
    }
    "Some attachments were not shown inline. Use read_file with the exact path only for \
     readable text, image, PDF, or notebook attachments; Office/archive/binary files \
     need conversion before inspection."
        .to_string()
}

/// Composes the blocks-type agent input for a turn with attachments,
/// mirroring TS GatewayAttachmentTurnComposer.prepare: a text block for the
/// message, direct content blocks and inlined image/text blocks, the
/// `[Attachment diagnostics]` text block (composer :270-274), and the
/// registered-path note (composer :89-123). Falls back to a plain
/// `{type:"text"}` turn when nothing but the message remains (composer
/// :67-72). Extracted from `prepare_attachment_turn` so the lease guard and
/// the inline pipeline are unit-testable without the full native assembly;
/// runs entirely on a blocking thread.
pub(super) fn compose_attachment_turn(
    project_dir: &Path,
    message: &JsValue,
    attachments: &[JsValue],
) -> Result<JsValue, SubmitError> {
    let mut entries: Vec<AttachEntry> = Vec::new();
    for attachment in attachments {
        let kind = text_field(attachment, "type").unwrap_or_default();
        // P1-d (codex): a truthy-but-non-UTF-8 path (a lone surrogate) is not
        // a silent skip in TS — Node keeps it, `path.resolve` keeps it, fs
        // mangles it to U+FFFD and the stat fails ENOENT, so the resolver
        // emits its not-found diagnostic (ts-probe/probe.out
        // "P1-d composer lone-surrogate"). The lossy spelling feeds that
        // diagnostic only.
        let (path, invalid_path) = match attachment.get("path") {
            Some(JsValue::String(value)) if !value.is_empty() => match value.to_utf8() {
                Ok(path) => (path, None),
                Err(_) => (String::new(), Some(String::from_utf16_lossy(value.units()))),
            },
            _ => (String::new(), None),
        };
        entries.push(AttachEntry {
            image_type: kind == "image",
            text_type: kind == "text",
            path,
            invalid_path,
            name: text_field(attachment, "name").unwrap_or_default(),
            mime: text_field(attachment, "mimeType").unwrap_or_default(),
            content: text_field(attachment, "content").filter(|content| !content.is_empty()),
            declared_bytes: attachment
                .get("bytes")
                .filter(|bytes| bytes.as_number().is_some())
                .cloned(),
            channel_key: attachment
                .get("metadata")
                .and_then(|metadata| metadata.get("channelKey"))
                .is_some_and(js_truthy),
        });
    }
    // P1-1 (simplify#2): the lease roots are computed once per prepare; the
    // lexical root is the cheap pre-filter, the canonicalized lease root is
    // what resolved paths are held against. A root that does not exist (no
    // uploads yet) simply leaves every lease path unresolved.
    let project_canon = fs::canonicalize(project_dir).unwrap_or_else(|_| project_dir.to_path_buf());
    let lease_lexical = project_canon.join(".tmp").join("chat-upload-leases");
    // P0-a (codex): a lease root that resolved outside the canonical project
    // (a symlink swap) is no valid root at all — nothing under it may
    // resolve, be read, or be registered. P2: only a missing root (no uploads
    // yet) softens to `None`; any other root canonicalize failure is a hard
    // turn failure.
    let lease_canon = match fs::canonicalize(&lease_lexical) {
        Ok(resolved) if resolved.starts_with(&project_canon) => Some(resolved),
        Ok(_) => None,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(failure(format!(
                "Attachment lease root cannot be resolved: {error}."
            )));
        }
    };

    let mut direct_blocks: Vec<JsValue> = Vec::new();
    let mut inline_blocks: Vec<JsValue> = Vec::new();
    let mut diagnostics: Vec<String> = Vec::new();
    let mut registered: Vec<RegisteredAttachment> = Vec::new();

    // TS composer :211-227 — content blocks are produced before any path
    // handling, and the producing attachment never reaches the path branch
    // (`continue` at :221/:226 — P1-a: the guard, open, read and diagnostics
    // are all skipped, so a root-escaping path cannot fail or read a content
    // turn).
    let mut produced_direct = vec![false; entries.len()];
    for (index, entry) in entries.iter().enumerate() {
        if entry.image_type
            && let Some(content) = &entry.content
            && !entry.mime.is_empty()
        {
            let declared_bytes = entry.declared_bytes.as_ref().and_then(JsValue::as_number);
            direct_blocks.push(flat_image_block(content, &entry.mime, declared_bytes));
            produced_direct[index] = true;
        } else if entry.text_type
            && let Some(content) = &entry.content
        {
            direct_blocks.push(object([
                ("type", "text".into()),
                ("text", JsValue::from(content.as_str())),
            ]));
            produced_direct[index] = true;
        }
    }

    for (index, entry) in entries.iter().enumerate() {
        if produced_direct[index] {
            continue;
        }
        // TS composer :229 — a missing or empty path is skipped silently;
        // the content branches above already handled their blocks.
        if entry.path.is_empty() {
            // P1-d: the lone-surrogate spelling is truthy in TS, so the
            // resolver stats it and diagnoses not-found. An audio-shaped
            // spelling stays silent, matching the audio skip (:230-233)
            // which never reaches the resolver.
            if let Some(lossy) = &entry.invalid_path
                && !is_audio_shape(&entry.mime, lossy)
            {
                let error = std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("ENOENT: no such file or directory, stat '{lossy}'"),
                );
                diagnostics.push(attachment_not_found(entry, lossy, &error));
            }
            continue;
        }
        // TS composer :229-233 never routes audio to the resolver, and
        // `collectRegisteredAttachmentReadFiles` (:171-190) swallows a
        // stat/realpath failure silently: no diagnostic, no turn failure.
        // Mirror that on every path-inspection failure below while leaving
        // the success path's open-first revalidation order unchanged.
        let audio = is_audio_attachment(entry);
        let resolved = match lease_guard(&entry.path, &lease_lexical, lease_canon.as_deref()) {
            Ok(LeaseGuard::Resolved(resolved)) => resolved,
            Ok(LeaseGuard::Unresolved(error)) => {
                if audio {
                    continue;
                }
                // A lease TTL cleanup race becomes a TS-shaped not-found
                // diagnostic, not a whole-turn failure.
                diagnostics.push(attachment_not_found(entry, &entry.path, &error));
                continue;
            }
            Err(rejected) => {
                if audio {
                    continue;
                }
                return Err(rejected);
            }
        };
        let route = attachment_route(entry, &entry.path);
        // P0 fix-r4 (codex): open-first revalidation. The previous order
        // (path stat → open → identity) only pinned the object two adjacent
        // lookups happened to see: swap the leaf or an ancestor to an
        // outside-project symlink after `lease_guard`'s canonicalize and
        // both lookups follow the same link onto the same outside inode —
        // the (dev, inode) comparison passes and outside bytes get inlined.
        // The approved object is now bound by revalidating the path AFTER
        // the open, and every read comes from the handle:
        //   1. open `resolved`;
        //   2. post-open `canonicalize` containment under the canonical
        //      lease root — a swap made after the open resolves outside
        //      and is treated as not-found (no read, no registration);
        //   3. path stat vs the handle's fstat `(dev, inode)` — a swap
        //      made after the revalidation stats a different file than
        //      the handle and is treated as not-found;
        //   4. a swap made after all checks only moves the path, never
        //      the opened handle, so the bytes still come from the object
        //      this sequence validated.
        // Step 2 runs on every platform; the (dev, inode) comparison is a
        // Unix-only strengthening (non-Unix leans on step 2). The
        // registered allowedReadFiles spelling is the post-open canonical
        // form from step 2 (identical to the pre-open one when nothing
        // moved).
        let mut file = match File::open(&resolved) {
            Ok(file) => file,
            Err(error) => {
                if audio {
                    continue;
                }
                diagnostics.push(attachment_not_found(entry, &entry.path, &error));
                continue;
            }
        };
        #[cfg(test)]
        swap_test_hook::notify(swap_test_hook::SwapWindow::AfterOpen, &resolved);
        let resolved = match fs::canonicalize(&resolved) {
            Ok(post_open) => match lease_canon.as_deref() {
                Some(root) if post_open.starts_with(root) => post_open,
                // A leaf or ancestor swapped after the open: the
                // revalidated spelling escapes the lease root (or there is
                // no valid root left, P0-a). No read, no registration —
                // the TS-shaped not-found diagnostic.
                _ => {
                    if audio {
                        continue;
                    }
                    let error = std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        "attachment resolved outside the lease root after open",
                    );
                    diagnostics.push(attachment_not_found(entry, &entry.path, &error));
                    continue;
                }
            },
            // The revalidation cannot prove the approved object (a lease
            // TTL cleanup race, a swapped-away path): the TS-shaped
            // not-found diagnostic, never a whole-turn failure (#1459 owns
            // the hard-failure shapes).
            Err(error) => {
                if audio {
                    continue;
                }
                diagnostics.push(attachment_not_found(entry, &entry.path, &error));
                continue;
            }
        };
        #[cfg(test)]
        swap_test_hook::notify(swap_test_hook::SwapWindow::AfterRevalidate, &resolved);
        let path_stat = match fs::metadata(&resolved) {
            Ok(metadata) => metadata,
            Err(error) => {
                if audio {
                    continue;
                }
                diagnostics.push(attachment_not_found(entry, &entry.path, &error));
                continue;
            }
        };
        let metadata = match file.metadata() {
            Ok(metadata) => metadata,
            Err(error) => {
                if audio {
                    continue;
                }
                diagnostics.push(attachment_not_found(entry, &entry.path, &error));
                continue;
            }
        };
        if !attachment_identity_matches(&path_stat, &metadata) {
            if audio {
                continue;
            }
            let error = std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "attachment resolved to a different file between revalidation and read",
            );
            diagnostics.push(attachment_not_found(entry, &entry.path, &error));
            continue;
        }
        // P1-f (gate boundary#1) — composer :181: only attachments with a
        // truthy `metadata.channelKey` enter allowedReadFiles and the path
        // note (uploaded lease attachments, bundle :57-62, never do).
        if metadata.is_file() && entry.channel_key {
            registered.push(RegisteredAttachment {
                resolved: resolved.clone(),
                name: entry.name.clone(),
                inspectable: is_read_file_inspectable(entry),
            });
        }
        // TS composer :230-233 — audio stays a registered path reference for
        // the FunASR MCP consumer; it is never read here.
        if audio {
            continue;
        }
        match route {
            AttachmentRoute::Pdf => {
                // P1-g (gate boundary#2) — composer :240-245 routes pdf (an
                // `application/pdf` mime or a `.pdf` path suffix) to
                // resolver :287-320: stat only, the pdf is never read, so no
                // byte limit applies; `Math.max(1, Math.round(size /
                // bytesPerPdfPage))` estimates the pages.
                let estimated_pages = ((metadata.len() as f64) / BYTES_PER_PDF_PAGE as f64)
                    .round()
                    .max(1.0) as u64;
                inline_blocks.push(object([
                    ("type", "text".into()),
                    (
                        "text",
                        JsValue::from(
                            format!(
                                "[PDF attachment: {}, {} bytes, estimated {estimated_pages} \
                                 pages. Use read_file on this registered attachment path to \
                                 inspect it.]",
                                resolved.display(),
                                metadata.len()
                            )
                            .as_str(),
                        ),
                    ),
                ]));
            }
            routed => {
                // P2a-1: the image routing half of composer :234 (image byte
                // verification stays out of this round).
                let image = matches!(routed, AttachmentRoute::Image);
                // TS AttachmentResolver.ts:149-163 — only whitelisted text
                // extensions are read; every other non-image stays a
                // registered path reference.
                let text_inline =
                    !image && TEXT_EXTENSIONS.contains(&resolver_extension(entry).as_str());
                if !image && !text_inline {
                    continue;
                }
                let limit = if image {
                    MAX_INLINE_IMAGE_BYTES
                } else {
                    MAX_INLINE_FILE_BYTES
                };
                // P1-c (codex) — resolver :164/:202: the stat before the
                // read is the authoritative oversize gate; the 读满 check
                // below stays as the defensive backstop for a file that grew
                // between the fstat and the read.
                if metadata.len() > limit {
                    diagnostics.push(if image {
                        image_too_large(&entry.path, metadata.len(), limit)
                    } else {
                        file_too_large(&entry.path, metadata.len(), limit)
                    });
                    continue;
                }
                let bytes = match read_bounded(&mut file, limit) {
                    Ok(bytes) => bytes,
                    // P1-e (codex, ts-probe/probe.out "P1-e composer dir-*"):
                    // the read is outside the resolver's stat try/catch —
                    // EISDIR (a directory path) rejects the whole turn
                    // instead of becoming a diagnostic.
                    Err(error) => {
                        return Err(failure(format!(
                            "Attachment {} could not be read: {error}",
                            entry.path
                        )));
                    }
                };
                // 读满即超限 (TS stat caps, AttachmentResolver.ts:164-175 and
                // :202-212): a read that filled the limit + 1 budget is over
                // the cap. The diagnostic reports the fstat size like the TS
                // message.
                if bytes.len() as u64 == limit + 1 {
                    let oversize = metadata.len().max(bytes.len() as u64);
                    diagnostics.push(if image {
                        image_too_large(&entry.path, oversize, limit)
                    } else {
                        file_too_large(&entry.path, oversize, limit)
                    });
                    continue;
                }
                if image {
                    match image_mime(entry) {
                        Some(mime) => inline_blocks.push(flat_image_block(
                            &base64_encode(&bytes),
                            &mime,
                            Some(bytes.len() as f64),
                        )),
                        // AttachmentResolver.ts:216-227
                        None => diagnostics.push(format!(
                            "Cannot determine image mime type from {}; provide mimeType explicitly.",
                            entry.path
                        )),
                    }
                } else {
                    // TS AttachmentResolver.ts:176-182 — readText into the
                    // `<attachment path=…>` wrapper (invalid UTF-8 decodes with
                    // replacement characters like Node's utf8 read).
                    let text = String::from_utf8_lossy(&bytes);
                    inline_blocks.push(object([
                        ("type", "text".into()),
                        (
                            "text",
                            JsValue::from(
                                format!(
                                    "<attachment path=\"{}\">\n{text}\n</attachment>",
                                    resolved.display()
                                )
                                .as_str(),
                            ),
                        ),
                    ]));
                }
            }
        }
    }

    // TS composer :98-111 — the note lines come from registered paths,
    // deduped by normalized path; allowedReadFiles holds the same resolved
    // spellings (composer :167-171 `safeAllowedAttachmentPath`).
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut note_lines: Vec<String> = Vec::new();
    let mut allowed_paths: Vec<JsValue> = Vec::new();
    let mut has_inspectable = false;
    for attachment in &registered {
        if !seen.insert(attachment.resolved.clone()) {
            continue;
        }
        let normalized = attachment.resolved.to_string_lossy().into_owned();
        // composer :108 — `normalized.split(/[\\/]/).pop() || "attachment"`.
        let fallback_name = normalized
            .split(['/', '\\'])
            .next_back()
            .filter(|segment| !segment.is_empty())
            .unwrap_or("attachment");
        let name = clean_attachment_name(&attachment.name, fallback_name);
        note_lines.push(format!("- {name}: {normalized}"));
        allowed_paths.push(JsValue::from(normalized.as_str()));
        has_inspectable |= attachment.inspectable;
    }

    // TS composer :114-122 — the guidance takes a diagnostics form when any
    // diagnostic was collected, any attachment is audio, or any attachment
    // is not read_file-inspectable; otherwise the plain form.
    let guidance = if diagnostics.is_empty()
        && !entries.iter().any(is_audio_attachment)
        && entries.iter().all(is_read_file_inspectable)
    {
        PLAIN_PATH_NOTE_GUIDANCE.to_string()
    } else {
        attachment_diagnostics_guidance(&entries, has_inspectable, &project_canon)
    };

    // TS composer :67-72 — no content block, no inline block, no diagnostic,
    // and no note leaves a plain text turn.
    if direct_blocks.is_empty()
        && inline_blocks.is_empty()
        && diagnostics.is_empty()
        && note_lines.is_empty()
    {
        return Ok(object([("type", "text".into()), ("text", message.clone())]));
    }
    let mut blocks: Vec<JsValue> = Vec::new();
    if let JsValue::String(text) = message
        && !text.is_empty()
    {
        blocks.push(object([("type", "text".into()), ("text", message.clone())]));
    }
    blocks.extend(direct_blocks);
    blocks.extend(inline_blocks);
    if !diagnostics.is_empty() {
        // TS composer :270-274
        let note = format!(
            "[Attachment diagnostics]\n{}",
            diagnostics
                .iter()
                .map(|line| format!("- {line}"))
                .collect::<Vec<_>>()
                .join("\n")
        );
        blocks.push(object([
            ("type", "text".into()),
            ("text", JsValue::from(note.as_str())),
        ]));
    }
    if !note_lines.is_empty() {
        // TS composer :119-122 — `\n\n${MARKER}\n${lines}\n${guidance}`.
        let note = format!(
            "\n\n{ATTACHMENT_PATH_NOTE_MARKER}\n{lines}\n{guidance}",
            lines = note_lines.join("\n")
        );
        blocks.push(object([
            ("type", "text".into()),
            ("text", JsValue::from(note.as_str())),
        ]));
    }
    let mut result = object([
        ("type", "blocks".into()),
        ("content", JsValue::Array(blocks)),
    ]);
    if !allowed_paths.is_empty()
        && let JsValue::Object(ref mut o) = result
    {
        o.insert("allowedReadFiles".into(), JsValue::Array(allowed_paths));
    }
    Ok(result)
}

fn failure(message: impl Into<String>) -> SubmitError {
    SubmitError::new("gateway_submit_failed", message)
}
fn unavailable(message: &str) -> SubmitError {
    SubmitError::new("CAPABILITY_UNAVAILABLE", message)
}
struct Cwd(std::path::PathBuf);
impl SubmitCwdService for Cwd {
    fn set(&self, _: &JsString, cwd: &JsString) -> Result<(), SubmitError> {
        if cwd
            .to_utf8()
            .is_ok_and(|cwd| crate::metadata::resolve(&self.0, &cwd) == self.0)
        {
            Ok(())
        } else {
            Err(unavailable(
                "The isolated native process cannot change its configured project.",
            ))
        }
    }
}
impl SubmitHostServices for Services {
    fn workspace_id(&self, input: &JsValue) -> Result<Option<JsString>, SubmitError> {
        if let Some(project) = input.get("projectKey").filter(|v| !v.is_undefined()) {
            let project = project
                .as_string()
                .and_then(|s| s.to_utf8().ok())
                .ok_or_else(|| failure("projectKey must be a valid path string."))?;
            if !self.options.serves_project(&project) {
                return Err(unavailable(
                    "The isolated native process serves only its configured project.",
                ));
            }
        }
        // The composition does not expose the SDK/model setter yet. Refuse
        // these before session admission instead of reporting a false selection.
        for key in ["modelSelection", "modelOverride", "sdkConfig"] {
            if input.get(key).is_some_and(|v| !v.is_undefined()) {
                return Err(unavailable(
                    "Per-turn model/SDK overrides are not installed in this isolated process.",
                ));
            }
        }
        // When snapshots are enabled (--storage-root was supplied), resolve the
        // id exactly as the management snapshot route does
        // (`PersistenceResources::snapshot_gateway`): TS `resolveProjectStorageId`
        // prefers a registered `.cwd` marker, else the legacy path slug. One
        // resolver keeps turn-captured snapshots visible to `snapshot_list`.
        if self.snapshot_recorder.is_some() {
            let projects = SnapshotProjects::new(self.options.home.clone(), PathBuf::new());
            let project = self.options.project.to_string_lossy();
            Ok(Some(JsString::from(
                projects.workspace_id(&project).as_str(),
            )))
        } else {
            Ok(None)
        }
    }
    fn snapshots(&self) -> Option<Arc<dyn SubmitSnapshots>> {
        self.snapshot_recorder.as_ref().map(|recorder| {
            Arc::new(NativeSubmitSnapshots(recorder.clone())) as Arc<dyn SubmitSnapshots>
        })
    }
    fn snapshots_required(&self) -> bool {
        false
    }
    fn cwd(&self) -> Option<Arc<dyn SubmitCwdService>> {
        Some(Arc::new(Cwd(self.options.project.clone())))
    }
    fn refresh(&self) -> SubmitFuture<'_, Result<(), SubmitError>> {
        Box::pin(async {
            let current = crate::runtime::project::NativeConfigSource::new(
                self.options.project.clone(),
                self.env.clone(),
                Some(self.options.home.clone()),
                None,
            );
            use crate::runtime::project::ConfigSource;
            let fresh = current
                .load(&self.options.project)
                .map_err(|e| failure(e.to_string()))?;
            let lease = self
                .project
                .acquire(None)
                .map_err(|e| failure(e.to_string()))?;
            let staged = lease
                .runtime()
                .resources()
                .get::<ProjectResources>()
                .ok_or_else(|| failure("Native project resources are missing."))?
                .context()
                .snapshot();
            if fresh.get("config") != staged.get("config") {
                self.project
                    .reload()
                    .await
                    .map_err(|e| failure(e.to_string()))?;
                use crate::runtime::refresh::RefreshRouter;
                self.owner.mark_all_dirty("config_changed");
            }
            Ok(())
        })
    }
    fn run_policy(&self) -> Option<JsValue> {
        let lease = self.project.acquire(None).ok()?;
        lease
            .runtime()
            .resources()
            .get::<ProjectResources>()?
            .context()
            .snapshot()
            .get("config")?
            .get("runPolicy")
            .cloned()
    }
    fn resolve_attachments<'a>(
        &'a self,
        input: &'a JsValue,
    ) -> SubmitFuture<'a, Result<SubmitAttachmentLease, SubmitError>> {
        let store = self.upload_store.clone();
        let project = self.options.project.clone();
        Box::pin(async move {
            // `uploadedAttachments` is a non-empty array at this call site
            // (GatewaySubmitHost::prepare checks that before calling).
            let refs = match input.get("uploadedAttachments") {
                Some(JsValue::Array(refs)) => refs.clone(),
                _ => {
                    return Err(SubmitError::new(
                        "CAPABILITY_UNAVAILABLE",
                        "uploadedAttachments must be a non-empty array.",
                    ));
                }
            };
            let project_key = project.to_string_lossy().into_owned();
            // Acquire leases sequentially, mirroring TS GatewayUploadedAttachmentBundle.
            // Disk I/O runs on a blocking thread so the async executor is not blocked.
            let mut acquired: Vec<crate::upload::UploadArtifactLease> = Vec::new();
            let result: Result<Vec<crate::upload::UploadArtifactLease>, SubmitError> =
                tokio::task::spawn_blocking(move || {
                    // P1-2: On error mid-loop, release every lease acquired so far
                    // (reverse order, best-effort) before propagating. Without this
                    // the partially-acquired set is dropped without release.
                    let outcome: Result<(), SubmitError> = (|| {
                        for r in &refs {
                            let upload_id = r
                                .get("uploadId")
                                .and_then(JsValue::as_string)
                                .and_then(|s| s.to_utf8().ok())
                                .ok_or_else(|| {
                                    SubmitError::new(
                                        "CAPABILITY_UNAVAILABLE",
                                        "uploadedAttachments[].uploadId must be a string.",
                                    )
                                })?;
                            // attachmentIds: optional string array
                            let attachment_ids: Option<Vec<String>> = match r.get("attachmentIds") {
                                Some(JsValue::Array(ids)) => Some(
                                    ids.iter()
                                        .map(|v| {
                                            v.as_string().and_then(|s| s.to_utf8().ok()).ok_or_else(
                                                || {
                                                    SubmitError::new(
                                                        "CAPABILITY_UNAVAILABLE",
                                                        "attachmentIds must be strings.",
                                                    )
                                                },
                                            )
                                        })
                                        .collect::<Result<Vec<_>, _>>()?,
                                ),
                                _ => None,
                            };
                            let lease = store
                                .acquire_attachment_lease(
                                    &upload_id,
                                    &project_key,
                                    attachment_ids.as_deref(),
                                )
                                .map_err(|e| {
                                    SubmitError::new(
                                        e.code.as_str(),
                                        String::from_utf16_lossy(e.message.units()),
                                    )
                                })?;
                            acquired.push(lease);
                        }
                        Ok(())
                    })();
                    match outcome {
                        Ok(()) => Ok(acquired),
                        Err(e) => {
                            for lease in acquired.iter().rev() {
                                if let Err(rel) = lease.release() {
                                    eprintln!("native upload lease release on error: {rel}");
                                }
                            }
                            Err(e)
                        }
                    }
                })
                .await
                .map_err(|e| SubmitError::new("CAPABILITY_UNAVAILABLE", e.to_string()))?;
            let leases = result?;
            // Merge attachments from all leases (TS: flatMap over acquired).
            let attachments: Vec<JsValue> = leases
                .iter()
                .flat_map(|lease| lease.attachments.iter().cloned())
                .collect();
            // Release is idempotent on all acquired leases (reverse order like TS).
            let release: crate::runtime::turn::submit_host::SubmitLeaseRelease =
                Box::new(move || {
                    Box::pin(async move {
                        // Concurrency P2a (gate): `lease.release()` removes the
                        // whole lease directory; like the acquire path it runs
                        // on a blocking thread so the serial host event loop is
                        // never blocked on unlink churn.
                        tokio::task::spawn_blocking(move || {
                            for lease in leases.iter().rev() {
                                // Best-effort: log failures but continue.
                                if let Err(e) = lease.release() {
                                    eprintln!("native upload lease release: {e}");
                                }
                            }
                        })
                        .await
                        .map_err(|e| SubmitError::new("CAPABILITY_UNAVAILABLE", e.to_string()))
                    })
                });
            Ok(SubmitAttachmentLease {
                attachments,
                release,
            })
        })
    }
    fn prepare_attachment_turn<'a>(
        &'a self,
        message: &'a JsValue,
        attachments: Vec<JsValue>,
        _project: Option<&JsString>,
        _session: Option<&JsString>,
    ) -> SubmitFuture<'a, Result<JsValue, SubmitError>> {
        Box::pin(async move {
            super::assembly::validate_profile(&self.options, &self.env)
                .await
                .map_err(|error| unavailable(&error.to_string()))?;
            if !matches!(message, JsValue::String(_)) {
                return Err(failure("message must be a string."));
            }
            if attachments.is_empty() {
                return Ok(object([("type", "text".into()), ("text", message.clone())]));
            }
            let project_dir = self.options.project.clone();
            let message = message.clone();
            // P1-3/concurrency#5: the whole composition — the once-per-prepare
            // lease-root canonicalize (simplify#2), the per-attachment stat and
            // read — runs on a blocking thread so the async executor is not
            // blocked.
            tokio::task::spawn_blocking(move || {
                compose_attachment_turn(&project_dir, &message, &attachments)
            })
            .await
            .map_err(|e| failure(e.to_string()))?
        })
    }
    fn resolve_model_selection<'a>(
        &'a self,
        input: &'a JsValue,
    ) -> SubmitFuture<'a, Result<JsValue, SubmitError>> {
        Box::pin(async move {
            let metadata = crate::metadata::Metadata {
                home: self.options.home.clone(),
                cwd: std::path::PathBuf::new(),
                files: crate::metadata::NativeFiles,
            };
            let params = object([
                (
                    "projectKey",
                    JsValue::String(self.options.project.to_string_lossy().into_owned().into()),
                ),
                (
                    "sessionKey",
                    input
                        .get("sessionKey")
                        .cloned()
                        .unwrap_or(JsValue::Undefined),
                ),
            ]);
            let state =
                crate::session_model::execute(&metadata, &params, &self.env).map_err(|e| {
                    SubmitError::new(&e.code, String::from_utf16_lossy(e.message.units()))
                })?;
            let saved = state.get("saved").cloned().unwrap_or(JsValue::Undefined);
            if saved.get("mode") == Some(&"model".into()) {
                Ok(object([("source", "session".into()), ("selection", saved)]))
            } else {
                Ok(object([(
                    "source",
                    state
                        .get("effective")
                        .and_then(|v| v.get("source"))
                        .cloned()
                        .unwrap_or("default".into()),
                )]))
            }
        })
    }
    fn record_failure<'a>(
        &'a self,
        run: &'a RunRef,
        status: &'a JsValue,
    ) -> SubmitFuture<'a, Result<(), SubmitError>> {
        let recorder = self
            .recorders
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&run.session_key)
            .and_then(std::sync::Weak::upgrade);
        let run = run.clone();
        let status = status.clone();
        Box::pin(async move {
            let recorder =
                recorder.ok_or_else(|| failure("Native session recorder is unavailable."))?;
            tokio::task::spawn_blocking(move || {
                recorder.writer().record_agent_status_message(
                    &run.session_key,
                    &run.run_id,
                    &status,
                )
            })
            .await
            .map_err(|e| failure(e.to_string()))?
            .map_err(|e| failure(e.to_string()))
        })
    }
    fn after_turn(&self, _: &RunRef) -> Result<(), SubmitError> {
        self.recorders
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|_, recorder| recorder.strong_count() > 0);
        Ok(())
    }
    fn now_ms(&self) -> f64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64()
            * 1000.0
    }
    fn abort_turn_timeout_ms(&self) -> u64 {
        self.options.drain_budget.as_millis() as u64
    }
}

/// fix-r4 window-injection seam (tests only): the open-first sequence in
/// `compose_attachment_turn` notifies [`swap_test_hook::notify`] at each
/// window; a test installs a callback that swaps a leaf or an ancestor
/// for an outside-project symlink exactly inside that window, and must
/// assert the hook actually fired — an unfired hook proves nothing, so
/// the test fails (race-hook-target-assert). Per-thread and RAII-removed
/// like the `mutation_test_hook` convention in file_memory.rs; production
/// builds compile none of this.
#[cfg(test)]
pub(super) mod swap_test_hook {
    use super::*;

    /// Which injection point the swap happens in: between `File::open` and
    /// the post-open `canonicalize` containment revalidation (step 2), or
    /// between the revalidation and the `(dev, inode)` path-stat/fstat
    /// comparison (step 3).
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub enum SwapWindow {
        AfterOpen,
        AfterRevalidate,
    }

    type Hook = Box<dyn FnMut(&Path)>;

    thread_local! {
        static HOOK: std::cell::RefCell<Option<(SwapWindow, Hook)>> =
            std::cell::RefCell::default();
    }

    pub struct Guard;

    pub fn set(window: SwapWindow, hook: impl FnMut(&Path) + 'static) -> Guard {
        HOOK.with(|slot| {
            assert!(slot.borrow().is_none());
            *slot.borrow_mut() = Some((window, Box::new(hook)));
        });
        Guard
    }

    pub fn notify(window: SwapWindow, resolved: &Path) {
        HOOK.with(|slot| {
            let mut slot = slot.borrow_mut();
            if let Some((hook_window, hook)) = slot.as_mut()
                && *hook_window == window
            {
                hook(resolved);
            }
        });
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            HOOK.with(|slot| *slot.borrow_mut() = None);
        }
    }
}
