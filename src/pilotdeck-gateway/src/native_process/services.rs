use super::{Options, assembly::Recorders};
use crate::{
    config::Env,
    metadata::{object, resolve_path, resolve_units, text_field},
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
use std::io::{Read, Write as IoWrite};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

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
    /// A truthy-but-non-UTF-8 `path` (a lone surrogate), kept as the raw
    /// UTF-16 for the not-found diagnostic (P1-d/#1461: the outer path is
    /// the TS `path.resolve` form of it — surrogate included — and only the
    /// fs error spelling is lossy): it never reaches the lease guard, an
    /// open, or allowedReadFiles.
    invalid_path: Option<JsString>,
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
/// regular file. allowedReadFiles comes from these, mirroring TS
/// `collectRegisteredAttachmentReadFiles` (GatewayAttachmentTurnComposer.ts
/// :173-193, `kind === "file"` only). #2495 — the note lines and the
/// hasInspectable accumulation read the whole attachment list below, not
/// this table: TS gates both only on the allowedReadFiles membership test
/// (`safeAllowedAttachmentPath`, composer :103/:144), so a second entry
/// with the same lexical spelling (or an unregistered entry colliding with
/// a registered spelling) still contributes.
struct RegisteredAttachment {
    /// #1446 — the Node `path.resolve` spelling of the client-sent path
    /// (`allowed.add(resolve(attachment.path))`, composer :182). This is
    /// the allowedReadFiles lexical spelling; the #2495 note pass re-derives
    /// the same spelling from `entries` through the same resolve
    /// (`attachment_lexical` below — the two are equal by construction,
    /// `safeAllowedAttachmentPath`, composer :167-171); a symlinked project
    /// spelling or a mid-path link keeps it distinct from `resolved`.
    lexical: PathBuf,
    /// The canonicalized spelling (`allowed.add(resolve(await
    /// port.realpath(attachment.path)))`, composer :183) — the containment
    /// authority and the second allowedReadFiles entry.
    resolved: PathBuf,
}

/// Outcome of the per-attachment lease guard.
enum LeaseGuard {
    /// The path canonicalizes under the canonical lease root; inlining uses
    /// this resolved path, and #1446 registers it beside the client's
    /// lexical spelling in allowedReadFiles (composer :182-183).
    Resolved(PathBuf),
    /// The path is lexically inside the lease root but does not resolve
    /// (missing file / dangling symlink — a lease TTL cleanup race — or its
    /// own canonicalize failing, #1629): the attachment becomes a TS-shaped
    /// not-found diagnostic instead of failing the whole turn. A missing
    /// lease root (no uploads yet) takes the same branch, never a panic.
    Unresolved(std::io::Error),
}

/// The per-attachment lease guard. Guards P1-1: only files inside the lease
/// root (`<project>/.tmp/chat-upload-leases/`, spelled either logically or
/// canonically — both name the same directory, #1618), computed once
/// per prepare) may be inlined or added to allowedReadFiles; arbitrary
/// client-supplied paths never reach allowedReadFiles or any read (same
/// shape as `read_at`'s PROJECT_PATH_FORBIDDEN symlink check in
/// upload/mod.rs). A path outside the root — lexically (`..` or a foreign
/// prefix, #1464) or through a symlink escape (#1464) — and an unavailable
/// root (missing, swapped outside the project, or its own canonicalize
/// failing, P0-a/#1459) all soften into the TS-shaped not-found branch the
/// same way a missing lease file (the TTL cleanup race, the TS stat ENOENT)
/// does: the TS composer never rejects a whole turn over a path. #1629
/// completes the set: a canonicalization failure of the path itself under a
/// valid root (ELOOP, ENOTDIR, EACCES, …) softens the same way — the TS
/// resolver's stat try/catch has no errno filter, so *any* stat failure
/// becomes a not-found diagnostic (AttachmentResolver.ts:131-144, :188-201,
/// :290-303) — and no path-inspection failure of this guard rejects the
/// turn anymore. The `Err` half of the `Result` stays for the
/// attach-followups-a negative controls, whose mutations restore each
/// softened branch's pre-softening hard-failure shape (`Err(failure(..))`)
/// as compiling runtime mutants; no production arm below returns it.
fn lease_guard(
    path: &str,
    lexical_roots: &[PathBuf],
    canonical_root: Option<&Path>,
) -> Result<LeaseGuard, SubmitError> {
    let p = Path::new(path);
    // #1618 r2: the root is matched against both spellings of the same
    // directory — the project's logical spelling (`projectKey` keeps Node's
    // path.resolve identity, symlinks included) and its canonical one (the
    // resolved upload record and a client that re-canonicalized before
    // echoing). Either spelling is the same lease directory; the canonical
    // containment check below still decides what may be read.
    let lexically_inside = lexical_roots.iter().any(|root| p.starts_with(root));
    if p.components().any(|c| c == Component::ParentDir) || !lexically_inside {
        // #1464: not lexically inside the lease root — a per-attachment
        // not-found diagnostic, not a whole-turn rejection. No read, no
        // registration.
        return Ok(LeaseGuard::Unresolved(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "attachment path is outside the permitted lease directory",
        )));
    }
    match canonical_root {
        // P0-a (codex) / #1459: there is no valid canonical root — the
        // lease root is missing, was swapped outside the project, or its
        // own canonicalize failed (EACCES/ELOOP/…). Every lease path takes
        // the not-found branch: no read, no registration.
        None => Ok(LeaseGuard::Unresolved(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "attachment lease root is unavailable",
        ))),
        Some(root) => match fs::canonicalize(p) {
            Ok(resolved) if resolved.starts_with(root) => Ok(LeaseGuard::Resolved(resolved)),
            // #1464: the root is valid but the path resolved outside it —
            // a symlink escape. A per-attachment not-found diagnostic, not
            // a whole-turn rejection. No read, no registration.
            Ok(_) => Ok(LeaseGuard::Unresolved(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "attachment resolved outside the permitted lease directory",
            ))),
            // #1321's TTL-cleanup race (the TS stat ENOENT) and #1629's
            // last hard failure (any other canonicalize errno of the path
            // itself — ELOOP, ENOTDIR, EACCES, …) soften identically: the
            // TS resolver catches every stat failure, whatever its errno.
            Err(error) => Ok(LeaseGuard::Unresolved(error)),
        },
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

/// The resolver diagnostic severities the composer filters on
/// (AttachmentResolver.ts:12-22): every pre-existing diagnostic in this
/// file is TS `warning` severity there; only the two info diagnostics
/// (#1458 `pdf_size_estimate`, #1469 binary-container
/// `attachment_unsupported`) carry `Info`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DiagnosticSeverity {
    Warning,
    Info,
}

/// TS GatewayAttachmentTurnComposer.ts:259-263 — the one composer filter
/// both info diagnostics are held to: `severity === "error" ||
/// severity === "warning" || !item.registered`. Warnings (and errors) always
/// enter the `[Attachment diagnostics]` block; info only when the attachment
/// is not registered (`registered = Boolean(metadata?.channelKey)`,
/// composer :238/:244/:250 — `entry.channel_key` here). #1458 and #1469 must
/// share this single criterion, not grow one each.
fn diagnostic_enters(severity: DiagnosticSeverity, registered: bool) -> bool {
    match severity {
        DiagnosticSeverity::Warning => true,
        DiagnosticSeverity::Info => !registered,
    }
}

/// TS AttachmentResolver.ts:312-318 — the `pdf_size_estimate` info
/// diagnostic; the `[PDF attachment: …]` block above carries the same
/// estimate (`{pages}` from `BYTES_PER_PDF_PAGE`, resolver :304).
fn pdf_size_estimate(size: u64, pages: u64) -> String {
    format!(
        "Estimated {pages} pages from {size} bytes (PilotDeck does not invoke pdfinfo, \
         intentional_difference §4.5)."
    )
}

/// TS AttachmentResolver.ts:323-329 — `unsupportedFileMessage`: the
/// non-whitelisted-extension diagnostic of `resolveFile`
/// (severity per :158 — binary container info, otherwise warning).
/// TS `BINARY_CONTAINER_EXTENSIONS.has(ext)` in all three call sites — the
/// message form, the text-whitelist exclusion, and the info severity.
fn is_binary_container(ext: &str) -> bool {
    BINARY_CONTAINER_EXTENSIONS.contains(&ext)
}

fn unsupported_file_message(absolute: &str, ext: &str) -> String {
    let display_ext = if ext.is_empty() { "(none)" } else { ext };
    if is_binary_container(ext) {
        format!(
            "Attachment {absolute} has Office/archive/binary extension {display_ext}; it was \
             registered as a file path but not shown inline, and read_file cannot inspect this \
             format directly."
        )
    } else {
        format!(
            "File extension {display_ext} is not in the inline text whitelist; skipped. If \
             this is plain text, use read_file with the exact path; otherwise convert it \
             before inspection."
        )
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
    !is_binary_container(&ext)
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

/// #1461 (P1-d, ts-probe `_tmp/20261108-issue-1460-r2/probe/composer-probe.out`
/// "Probe B"): the not-found diagnostic of a lone-surrogate path keeps two
/// spellings. The outer path is the `path.resolve` form of the raw UTF-16 —
/// Node's lexical resolve keeps an unpaired surrogate and adds the absolute
/// prefix, so the diagnostic text carries it verbatim — and only the fs layer
/// mangles the surrogate to U+FFFD: Node converts the string to a native path
/// lossily before the stat, so the ENOENT message carries that spelling. The
/// resolve runs over UTF-16 units (`resolve_units`) because a lone surrogate
/// is not representable in a Rust `String` at all, and the route words the
/// line (image/pdf/file) like `attachment_not_found` does.
fn surrogate_path_not_found(entry: &AttachEntry, original: &JsString, cwd: &Path) -> JsString {
    let resolved = resolve_units(&cwd.to_string_lossy(), original);
    let lossy = String::from_utf16_lossy(resolved.units());
    // #1461 fix (DS r1 boundary P2): TS composer :234-251 routes the
    // diagnostic words on the *original* attachment path, and the resolve
    // above can fold dot segments — `report.pdf/\uD800/..` resolves to a
    // `.pdf` tail while the raw spelling ends in `..` and routes as File.
    // The raw lossy spelling (the same one `is_audio_shape` reads) keeps
    // the route on the unfixed input.
    let raw_lossy = String::from_utf16_lossy(original.units());
    let prefix = match attachment_route(entry, &raw_lossy) {
        AttachmentRoute::Image => "Image attachment not found: ",
        AttachmentRoute::Pdf => "PDF attachment not found: ",
        AttachmentRoute::File => "Attachment not found: ",
    };
    let mut units: Vec<u16> = prefix.encode_utf16().collect();
    units.extend_from_slice(resolved.units());
    units.extend(format!(" (ENOENT: no such file or directory, stat '{lossy}').").encode_utf16());
    JsString::from_utf16(units)
}

/// #1461 r2 (DS concurrency P1): Node `process.cwd()` caches the first
/// successful read (until chdir) — after the cwd is removed, `process.cwd()`
/// and `path.resolve("rel")` keep returning the cached directory; only a
/// failed *first* read throws `uv_cwd` (measured Node 22.22; the same cache
/// form as `skills::process_cwd`,
/// specs/gateway-parity/diffs/A7.4-TOOLBUDGET-CWD-001-R1.md).
///
/// #1461 r3 honest residual (DS boundary P1): this cache is *lazy* — it is
/// seeded by the first relative surrogate diagnostic that finds the cwd
/// readable, not at process start the way Node's is. The snapshot therefore
/// covers the *second and later* diagnostics after a mid-run deletion; the
/// FIRST relative surrogate diagnostic after the deletion still sees
/// `current_dir()` fail, returns `None`, and stays silent where a warm TS
/// process would resolve against its startup-seeded cache and emit the
/// diagnostic. A cwd that was never readable (cold on both sides) stays
/// silent either way — TS `path.resolve` throws `uv_cwd` there, and #1464
/// keeps an attachment failure from failing the turn. Closing the gap
/// needs a crate-level cache seeded at gateway start and shared with
/// `skills::process_cwd` (the A7.4-TOOLBUDGET-CWD-001 area) — tracked as a
/// followup, not changed here.
fn cached_process_cwd() -> Option<PathBuf> {
    static CACHED: Mutex<Option<PathBuf>> = Mutex::new(None);
    let mut cached = CACHED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(cwd) = &*cached {
        return Some(cwd.clone());
    }
    let cwd = std::env::current_dir().ok()?;
    *cached = Some(cwd.clone());
    Some(cwd)
}

/// The Node `path.resolve` spelling of a client-sent attachment path —
/// `allowed.add(resolve(attachment.path))`, composer :182. The registration
/// pass and the #2495 note pass must derive the identical spelling for the
/// membership test, so both call this one helper instead of re-spelling
/// the resolve (a drift here silently drops note lines, not a compile
/// error).
fn attachment_lexical(cwd: &Path, entry: &AttachEntry) -> PathBuf {
    resolve_path(cwd, Path::new(&entry.path))
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
    // #1468, stated honestly: without a stable cross-platform file identity
    // this comparison says nothing, and the open-first sequence's path-based
    // steps (post-open canonicalize containment, path stat) are all spelled
    // against the path — two well-timed swaps (leaf to an outside symlink
    // before the open, back to the in-root file before the revalidation,
    // and onto the outside file again before the stat) read outside bytes
    // through an outside handle while every path check passes. The handle
    // binding below (`handle_bound_to_upload_areas`) is the step that does
    // not consult the path; on a platform whose `opened_path` fails (see
    // handle_path.rs) it fails closed, so the residual window on such a
    // platform is the platform itself, not this comparison. Non-Unix is not
    // a current shipping target; the residual window is accepted and
    // documented rather than papered over.
    true
}

/// #1468: the approval object is the opened handle, not any path spelling
/// checked around it. The handle's own resolved path (macOS F_GETPATH,
/// Linux /proc/self/fd — see `handle_path`) must name a location inside the
/// canonical lease root or the canonical upload-store root (a lease file is
/// a hard link of an upload-store file, and the platform may report either
/// name). An unavailable handle path fails closed — never approved.
///
/// Stated honestly, this answers with the vnode's *current* name: it says
/// the object has some legitimate home, not that the name is the one the
/// open used. A rename performed after the open therefore moves an outside
/// vnode under an accepted root — closed by the identity comparison in the
/// caller (step 0/3), not here. A hard link created inside the lease root
/// before the turn is not caught at all: the object then genuinely has an
/// in-root name, and only recording the lease's expected `(dev, inode)` at
/// creation time would distinguish it (#1468 follow-up).
pub(super) fn handle_bound_to_upload_areas(
    file: &File,
    lease_root: &Path,
    upload_store: Option<&Path>,
    approved: &fs::Metadata,
) -> bool {
    match crate::handle_path::opened_path(file) {
        Ok(handle) => {
            let named_inside = handle.starts_with(lease_root)
                || upload_store.is_some_and(|store| handle.starts_with(store));
            // The name says the object has a legitimate home; the identity
            // says it is the object the pre-open validation approved. A
            // rename after the open satisfies the first and not the second.
            let same_object = file
                .metadata()
                .map(|opened| attachment_identity_matches(approved, &opened))
                .unwrap_or(false);
            named_inside && same_object
        }
        // #1468: an unavailable handle path is never approved.
        Err(_) => false,
    }
}

/// #1468: the canonical upload-store root — the second legitimate home of a
/// lease object — or `None` when there is none to accept. `None` covers both
/// the ordinary "no uploads yet" case and the adversarial one: a
/// `.tmp/chat-uploads` that resolves outside the project is not a home at
/// all. The decision itself lives in `canonical_home` (shared with the lease
/// root) so it can be tested (and mutation-killed) without a live handle
/// path; the store-side unit test is
/// `upload_store_root_outside_the_project_is_not_a_home`.
pub(super) fn canonical_upload_store(project_canon: &Path) -> Option<PathBuf> {
    crate::upload::canonical_home(
        &project_canon.join(".tmp").join("chat-uploads"),
        project_canon,
        project_canon,
    )
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
    // :142-151 — no attachment with an allowedReadFiles spelling left
    // inspectable → the conversion form; otherwise the read_file-scoped form
    // (#2495: the accumulation is the caller's whole-list `some(...)`
    // pass, never gated by the note dedup).
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
        // "P1-d composer lone-surrogate"). #1461: the raw UTF-16 feeds that
        // diagnostic — the outer path keeps the surrogate and the resolve
        // prefix, only the fs error spelling is lossy.
        let (path, invalid_path) = match attachment.get("path") {
            Some(JsValue::String(value)) if !value.is_empty() => match value.to_utf8() {
                Ok(path) => (path, None),
                Err(_) => (String::new(), Some(value.clone())),
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
    // #1618 r1: the lease paths handed to the client are built from the
    // project's *logical* spelling (`projectKey` keeps Node's path.resolve
    // identity, symlinks included — native_process.rs). Comparing them against
    // a canonicalized root alone dropped every lease attachment of a
    // symlinked project into the not-found branch, so the lexical pre-filter
    // also accepts the logical spelling. The canonical comparison below still
    // decides containment.
    let lease_lexical = project_dir.join(".tmp").join("chat-upload-leases");
    // #1618 r2: the same directory spelled physically. The client may echo
    // either spelling (a lease path built from `projectKey` keeps the logical
    // one; a record or caller that resolved the project first hands back the
    // canonical one), so the lexical pre-filter accepts both and the
    // canonical containment check below remains the only authority.
    let lease_lexical_canon = project_canon.join(".tmp").join("chat-upload-leases");
    let lease_lexical_roots = [lease_lexical.clone(), lease_lexical_canon];
    // P0-a (codex): a lease root that resolved outside the canonical project
    // (a symlink swap) is no valid root at all — nothing under it may
    // resolve, be read, or be registered. #1459: a root that fails to
    // canonicalize (EACCES/ELOOP/ENOTDIR …) is equally no valid root: the TS
    // composer has no lease-root concept and no stat/canonicalize failure of
    // its own ever rejects the turn (each attachment failure becomes a
    // not-found diagnostic), so the root failure softens to `None` exactly
    // like a missing root — every lease path then takes the not-found branch
    // (zero reads, zero registrations).
    // The same guard as the upload store (review GLM sibling#1: the two roots
    // had drifted — the store refused a symlinked leaf while the lease root
    // accepted one, so a `.tmp/chat-upload-leases` swapped for a link into the
    // project turned that subtree into a lease home behind an unchanged
    // lexical spelling).
    let lease_canon = crate::upload::canonical_home(&lease_lexical, project_dir, &project_canon);
    // #1468: the second legitimate home of a lease object. Every lease file
    // is a hard link of the upload-store file (`acquire_attachment_lease`),
    // and the platform's handle-path lookup may answer with either name of
    // a hard-linked vnode (macOS F_GETPATH does — probe
    // `_tmp/20261106-issue-1468-1494/probe/`; Linux /proc always reports
    // the spelling the open used). A missing store root (no uploads yet)
    // simply leaves the lease spelling as the only accepted home.
    // P1 (deepseek boundary#2 / concurrency#2): the store root gets the same
    // containment guard as `lease_canon` — a `.tmp/chat-uploads` swapped for
    // a symlink out of the project must not become a second accepted home,
    // or step 4 starts approving handles that live wherever the link points.
    // The guard is a named function with its own unit test
    // (`upload_store_root_outside_the_project_is_not_a_home`) so the decision
    // is exercised without a live handle path (review deepseek
    // concurrency#3 / minimax boundary#2: the end-to-end fixture alone left
    // the guard unkilled).
    let upload_store_canon = canonical_upload_store(&project_canon);

    let mut direct_blocks: Vec<JsValue> = Vec::new();
    let mut inline_blocks: Vec<JsValue> = Vec::new();
    // #1461: the lines are `JsString`s — a lone-surrogate path keeps its
    // unpaired surrogate in the outer diagnostic text, which a Rust `String`
    // cannot hold at all.
    let mut diagnostics: Vec<JsString> = Vec::new();
    let mut registered: Vec<RegisteredAttachment> = Vec::new();

    // TS composer :211-227 — content blocks are produced before any path
    // handling, and the producing attachment's `continue` (:221/:226 — P1-a:
    // the guard, open, read and diagnostics of the resolver projection are
    // all skipped, so a root-escaping path cannot fail or read a content
    // turn) skips only that projection. #1460: the registration collection
    // (`collectRegisteredAttachmentReadFiles`, composer :57/:173-193) is a
    // pass of its own that runs over every attachment — a content
    // attachment with a valid path and a channelKey is registered
    // (allowedReadFiles + path note) exactly like a path-only one.
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
        // #1460: `content_only` marks an attachment whose block the pass
        // above already emitted. Everything below still runs for it — the
        // registration needs the validated path — except the resolver
        // projection the TS `continue` skips.
        let content_only = produced_direct[index];
        // TS composer :229 — a missing or empty path is skipped silently;
        // the content branches above already handled their blocks.
        if entry.path.is_empty() {
            // P1-d: the lone-surrogate spelling is truthy in TS, so the
            // resolver stats it and diagnoses not-found. An audio-shaped
            // spelling stays silent, matching the audio skip (:230-233)
            // which never reaches the resolver; a content-produced one is
            // equally silent (#1460 — the resolver projection, and its
            // diagnostics with it, is what the content `continue` skips).
            if let Some(original) = &entry.invalid_path
                && !content_only
                && !is_audio_shape(&entry.mime, &String::from_utf16_lossy(original.units()))
            {
                // #1461 fix (DS r1 P1): Node `path.resolve(path)` with no
                // explicit base resolves against the *process* cwd, not the
                // project root — the TS probe's diagnostic prefix is
                // `/tmp/pdrs-1460-probe-cwd/` while the project root is a
                // different directory (composer-probe.out "Probe B"); the
                // other cwd-free `resolve_units` callers (task_budget,
                // edgeclaw text) read the process cwd the same way. r2 fix
                // (DS concurrency P1 / boundary P2): the cwd is only
                // consulted for *relative* spellings — Node never reads it
                // for an absolute path (`path.resolve` stops at the first
                // absolute segment; `skills::resolve` is the same shape),
                // so the diagnostic must survive an unreadable cwd there —
                // and for relative spellings Node caches the first
                // successful read (until chdir), so a cwd deleted later
                // still resolves against the old path (measured Node 22.22,
                // `skills::process_cwd`,
                // specs/gateway-parity/diffs/A7.4-TOOLBUDGET-CWD-001-R1.md):
                // the cached snapshot covers it instead of dropping the
                // diagnostic. A cwd deleted *before the first relative
                // diagnostic* (or never readable) stays silent — see the
                // honest-residual note on `cached_process_cwd`; #1464 keeps
                // an attachment failure from failing the turn.
                if original.units().first() == Some(&(b'/' as u16)) {
                    diagnostics.push(surrogate_path_not_found(entry, original, Path::new("/")));
                } else if let Some(cwd) = cached_process_cwd() {
                    diagnostics.push(surrogate_path_not_found(entry, original, &cwd));
                }
            }
            continue;
        }
        // TS composer :229-233 never routes audio to the resolver, and
        // `collectRegisteredAttachmentReadFiles` (:171-190) swallows a
        // stat/realpath failure silently: no diagnostic, no turn failure.
        // Mirror that on every path-inspection failure below while leaving
        // the success path's open-first revalidation order unchanged.
        // #1460: a content-produced attachment joins audio in that
        // relationship — its block is already emitted (:221/:226), and the
        // collection pass swallows its own stat errors — so neither enters
        // the resolver projection and both keep every failure below silent.
        let audio = is_audio_attachment(entry);
        let no_resolver = audio || content_only;
        let resolved = match lease_guard(&entry.path, &lease_lexical_roots, lease_canon.as_deref())
        {
            Ok(LeaseGuard::Resolved(resolved)) => resolved,
            Ok(LeaseGuard::Unresolved(error)) => {
                if no_resolver {
                    continue;
                }
                // A lease TTL cleanup race, an unavailable root, a symlink
                // escape, or a canonicalize failure of the path itself
                // (#1629) all become the same TS-shaped not-found
                // diagnostic, never a whole-turn failure.
                diagnostics.push(attachment_not_found(entry, &entry.path, &error).into());
                continue;
            }
            Err(rejected) => {
                // #1629: no `lease_guard` arm returns `Err` anymore — every
                // path-inspection failure softens above (the TS resolver's
                // stat try/catch turns any stat failure into a not-found
                // diagnostic). The arm stays so the attach-followups-a
                // negative controls can restore a softened branch's
                // hard-failure shape as a compiling runtime mutation; if
                // production ever reaches it, the old rejection semantics
                // apply (audio and a content-produced attachment stay
                // silent) rather than a silent fallthrough.
                if no_resolver {
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
        // #1468 corrected the claim that those path-based steps bound the
        // approved object: three well-timed swaps (leaf to an outside
        // symlink before the open, back to an in-root file before the
        // revalidation, onto the outside file again before the stat) open
        // the outside file and pass every check that consults the path.
        // The approval is now bound to the opened handle itself, and every
        // read comes from that handle:
        //   0. the file identity the pre-open validation approved
        //      (`approved_stat`) is remembered before the open, so a
        //      post-open rename that moves an outside vnode's name into the
        //      lease root still changes which inode the handle references.
        //      The identity is sampled with `symlink_metadata`: the
        //      canonical path is symlink-free by construction, so a link
        //      appearing at the leaf is a swap and must not become the
        //      "approved" object (see the note on `approved_stat`);
        //   1. open `resolved`;
        //   2. post-open `canonicalize` containment under the canonical
        //      lease root — a swap made after the open resolves outside
        //      and is treated as not-found (no read, no registration);
        //   3. path stat vs the handle's fstat `(dev, inode)` — a swap
        //      made after the revalidation stats a different file than
        //      the handle and is treated as not-found;
        //   4. the handle's own resolved path (`handle_path`, macOS
        //      F_GETPATH / Linux / proc/self/fd) must name a location
        //      inside the canonical lease root or the canonical
        //      upload-store root, and an unavailable handle path is never
        //      approved — this is the step that closes the multi-swap
        //      sequence, because the swaps decide which object step 1
        //      opens and the handle's name follows that object. The name is
        //      still a name: a rename *after* the open moves an outside
        //      vnode under an accepted root, which is why step 0/3 compare
        //      identities rather than trusting the spelling. A hard link
        //      planted in the lease root before the turn leaves the object
        //      with a genuine in-root name and is out of this sequence's
        //      reach (documented on `handle_bound_to_upload_areas`);
        //   5. a swap made after all checks only moves the path, never
        //      the opened handle, so the bytes still come from the object
        //      this sequence validated.
        // Step 2 and the handle binding run on every platform; the
        // (dev, inode) comparison is a Unix-only strengthening (see the
        // honest residual note on `attachment_identity_matches`). The
        // registered allowedReadFiles spelling is the post-open canonical
        // form from step 2 (identical to the pre-open one when nothing
        // moved).
        // #1468 step 0 (deepseek concurrency#1 / boundary#1): the identity
        // the pre-open validation approved. Step 4 asks the platform for the
        // handle's *current* name, so a rename performed after the open
        // could hand an outside vnode a name inside the lease root and pass
        // every path check. A rename cannot change which inode the handle
        // references, so pinning the approved identity here and comparing it
        // with the handle's fstat (step 3) closes that entry. A hard link
        // planted in the lease root *before* the turn is not covered — the
        // object then genuinely has a legitimate name — and is documented on
        // `handle_bound_to_upload_areas`.
        //
        // #1468 r2: the identity must belong to the object the *containment
        // check* validated. `fs::metadata` re-walks the path and follows a
        // symlink, so a swap landing between `lease_guard`'s canonicalize
        // and this stat aimed the approval: "approved" became the outside
        // inode, the open followed the same link, and a post-open rename
        // gave that outside vnode an in-root name — identity included, every
        // check then passed. `fs::symlink_metadata` does not follow a link at
        // the leaf: anything that is a link here appeared after the
        // validation and takes the not-found branch. An *ancestor directory*
        // swapped to a link inside this same window is still open (the
        // canonical spelling re-resolves through it); only an identity
        // recorded when the lease is created closes that — the #1468
        // follow-up, stated on `handle_bound_to_upload_areas`.
        #[cfg(test)]
        swap_test_hook::notify(swap_test_hook::SwapWindow::BeforeApprovedStat, &resolved);
        let approved_stat = match fs::symlink_metadata(&resolved) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                if no_resolver {
                    continue;
                }
                let error = std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "attachment lease path became a symlink after validation",
                );
                diagnostics.push(attachment_not_found(entry, &entry.path, &error).into());
                continue;
            }
            Ok(metadata) => metadata,
            Err(error) => {
                if no_resolver {
                    continue;
                }
                diagnostics.push(attachment_not_found(entry, &entry.path, &error).into());
                continue;
            }
        };
        #[cfg(test)]
        swap_test_hook::notify(swap_test_hook::SwapWindow::BeforeOpen, &resolved);
        let mut file = match File::open(&resolved) {
            Ok(file) => file,
            Err(error) => {
                if no_resolver {
                    continue;
                }
                diagnostics.push(attachment_not_found(entry, &entry.path, &error).into());
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
                    if no_resolver {
                        continue;
                    }
                    let error = std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        "attachment resolved outside the lease root after open",
                    );
                    diagnostics.push(attachment_not_found(entry, &entry.path, &error).into());
                    continue;
                }
            },
            // The revalidation cannot prove the approved object (a lease
            // TTL cleanup race, a swapped-away path): the TS-shaped
            // not-found diagnostic, never a whole-turn failure (#1459 owns
            // the hard-failure shapes).
            Err(error) => {
                if no_resolver {
                    continue;
                }
                diagnostics.push(attachment_not_found(entry, &entry.path, &error).into());
                continue;
            }
        };
        #[cfg(test)]
        swap_test_hook::notify(swap_test_hook::SwapWindow::AfterRevalidate, &resolved);
        let path_stat = match fs::metadata(&resolved) {
            Ok(metadata) => metadata,
            Err(error) => {
                if no_resolver {
                    continue;
                }
                diagnostics.push(attachment_not_found(entry, &entry.path, &error).into());
                continue;
            }
        };
        let metadata = match file.metadata() {
            Ok(metadata) => metadata,
            Err(error) => {
                if no_resolver {
                    continue;
                }
                diagnostics.push(attachment_not_found(entry, &entry.path, &error).into());
                continue;
            }
        };
        if !attachment_identity_matches(&path_stat, &metadata) {
            if no_resolver {
                continue;
            }
            let error = std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "attachment resolved to a different file between revalidation and read",
            );
            diagnostics.push(attachment_not_found(entry, &entry.path, &error).into());
            continue;
        }
        // #1468 step 4: bind the approval to the opened handle. This is the
        // last gate before any registration or read, and the only one that
        // does not consult the movable path: the multi-swap sequence passes
        // steps 2 and 3 with an outside handle, and only the handle's own
        // resolved location exposes it. A handle whose path is unavailable
        // fails closed (`handle_bound_to_upload_areas`).
        let handle_bound = match lease_canon.as_deref() {
            Some(lease_root) => handle_bound_to_upload_areas(
                &file,
                lease_root,
                upload_store_canon.as_deref(),
                &approved_stat,
            ),
            // No valid canonical lease root is reachable here (lease_guard
            // resolved under one); if it ever is, fail closed anyway.
            None => false,
        };
        if !handle_bound {
            if no_resolver {
                continue;
            }
            let error = std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "attachment handle is not the lease object approved before the open",
            );
            diagnostics.push(attachment_not_found(entry, &entry.path, &error).into());
            continue;
        }
        // P1-f (gate boundary#1) — composer :181: only attachments with a
        // truthy `metadata.channelKey` enter allowedReadFiles (uploaded
        // lease attachments, bundle :57-62, never do). #2495 — the note
        // lines and the hasInspectable accumulation no longer key on this
        // registration gate: they read the whole entry list through the
        // allowedReadFiles membership test below.
        if metadata.is_file() && entry.channel_key {
            // #1446 — composer :182-183 registers both spellings: the
            // lexical `resolve(attachment.path)` and the canonicalized
            // realpath. The path reaching here is always absolute (the
            // lease guard's lexical pre-filter holds it against an absolute
            // root), so the process cwd a TS `path.resolve` would consult
            // never changes the result.
            let lexical = attachment_lexical(&cached_process_cwd().unwrap_or_default(), entry);
            registered.push(RegisteredAttachment {
                lexical,
                resolved: resolved.clone(),
            });
        }
        // TS composer :230-233 — audio stays a registered path reference for
        // the FunASR MCP consumer and is never read here; a content-produced
        // attachment (#1460) already emitted its block in the pass above and
        // equally never reaches the resolver projection.
        if no_resolver {
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
                // #1458 — resolver :312-318: `resolvePdf` also produces the
                // `pdf_size_estimate` info diagnostic; the composer filter
                // (:259-263) keeps it only for an unregistered (uploaded,
                // channelKey-less) pdf — a registered pdf's info is dropped.
                if diagnostic_enters(DiagnosticSeverity::Info, entry.channel_key) {
                    diagnostics.push(pdf_size_estimate(metadata.len(), estimated_pages).into());
                }
            }
            routed => {
                // P2a-1: the image routing half of composer :234 (image byte
                // verification stays out of this round).
                let image = matches!(routed, AttachmentRoute::Image);
                // TS AttachmentResolver.ts:149-163 — only whitelisted text
                // extensions are read; every other non-image stays a
                // registered path reference carrying the
                // `attachment_unsupported` diagnostic (#1469): info for a
                // binary-container extension, warning otherwise, through the
                // one composer filter.
                let ext = resolver_extension(entry);
                let text_inline = !image && TEXT_EXTENSIONS.contains(&ext.as_str());
                if !image && !text_inline {
                    let severity = if is_binary_container(&ext) {
                        DiagnosticSeverity::Info
                    } else {
                        DiagnosticSeverity::Warning
                    };
                    if diagnostic_enters(severity, entry.channel_key) {
                        diagnostics.push(unsupported_file_message(&entry.path, &ext).into());
                    }
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
                    diagnostics.push(
                        (if image {
                            image_too_large(&entry.path, metadata.len(), limit)
                        } else {
                            file_too_large(&entry.path, metadata.len(), limit)
                        })
                        .into(),
                    );
                    continue;
                }
                #[cfg(test)]
                swap_test_hook::notify(swap_test_hook::SwapWindow::BeforeRead, &resolved);
                let bytes = match read_bounded(&mut file, limit) {
                    Ok(bytes) => bytes,
                    // P1-e (codex, ts-probe/probe.out "P1-e composer dir-*"):
                    // the read is outside the resolver's stat try/catch —
                    // EISDIR (a directory path) rejects the whole turn
                    // instead of becoming a diagnostic. #1462: the message is
                    // Node's own — `readText` is `fs.readFile` and a
                    // descriptor read failure carries no path there
                    // (`EISDIR: illegal operation on a directory, read`,
                    // node v22.22.0 probe
                    // `_tmp/20261106-issue-1458/node-probe/eisdir-probe.out`);
                    // an errno outside the libuv table falls back to the
                    // previous wording.
                    Err(error) => {
                        // #1462: Node's own message, including libuv's
                        // `UNKNOWN: unknown error` fallback for errnos outside
                        // the table — the shape must not be invented locally.
                        return Err(failure(crate::node_fs_message_or_unknown(
                            &error, "read", None,
                        )));
                    }
                };
                // 读满即超限 (TS stat caps, AttachmentResolver.ts:164-175 and
                // :202-212): a read that filled the limit + 1 budget is over
                // the cap. The diagnostic reports the fstat size like the TS
                // message.
                if bytes.len() as u64 == limit + 1 {
                    let oversize = metadata.len().max(bytes.len() as u64);
                    diagnostics.push(
                        (if image {
                            image_too_large(&entry.path, oversize, limit)
                        } else {
                            file_too_large(&entry.path, oversize, limit)
                        })
                        .into(),
                    );
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
                        None => diagnostics.push(
                            format!(
                                "Cannot determine image mime type from {}; provide mimeType \
                                 explicitly.",
                                entry.path
                            )
                            .into(),
                        ),
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

    // TS composer :173-193 — `collectRegisteredAttachmentReadFiles`: the
    // allowed list is the Set of both spellings per registered attachment
    // (#1446). The note pass below and this collection dedup independently,
    // so a duplicate lexical spelling with a different realpath still
    // contributes its canonical entry.
    let mut allowed: HashSet<PathBuf> = HashSet::new();
    let mut allowed_paths: Vec<JsValue> = Vec::new();
    for attachment in &registered {
        for spelling in [&attachment.lexical, &attachment.resolved] {
            if !allowed.insert(spelling.clone()) {
                continue;
            }
            allowed_paths.push(JsValue::from(
                spelling.to_string_lossy().into_owned().as_str(),
            ));
        }
    }
    // TS composer :98-111/:142-146 — the note lines and the hasInspectable
    // accumulation both cover every attachment whose `resolve(path)`
    // spelling is a member of allowedReadFiles (`safeAllowedAttachmentPath`,
    // :103/:144): registered channel attachments, and an unregistered entry
    // colliding with a registered spelling (ts-probe
    // `_tmp/20261026-issue-2495/probe/probe.out` D/E). #2495 —
    // `hasInspectableAttachment` is an `attachments.some(...)` over the whole
    // list, never gated by the note dedup: a second entry with the same
    // lexical spelling but a different inspectable still counts, while the
    // note line itself is deduped on the lexical spelling to the first
    // entry (`seen` on `normalized`, :105 — #1446).
    //
    // #2495 fix-r1 (boundary review) — an honest residual of the #1461 r3
    // lazy cwd cache extends here: a *relative* entry path read while the
    // cwd is cold-unreadable resolves against an empty cwd, so membership
    // never holds and the entry silently loses its note line and its
    // inspectable contribution, where TS `path.resolve` throws `uv_cwd`
    // (ENOENT) on that same cold read — Node v22.22.0 measured,
    // `_tmp/20261026-issue-2495/fix-r1/probe/cwd-deleted*.out`; a warm
    // process keeps returning its cached spelling on both sides). See
    // `cached_process_cwd` for the tracked followup (a crate-level cache
    // seeded at gateway start, A7.4-TOOLBUDGET-CWD-001 area).
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut note_lines: Vec<String> = Vec::new();
    let mut has_inspectable = false;
    // One cwd read for the whole pass — the cache is immutable after its
    // seed (#1461 r2/r3), so a per-entry lock+clone would buy nothing.
    let process_cwd = cached_process_cwd().unwrap_or_default();
    for entry in &entries {
        if entry.path.is_empty() {
            continue;
        }
        let lexical = attachment_lexical(&process_cwd, entry);
        if !allowed.contains(&lexical) {
            continue;
        }
        has_inspectable |= is_read_file_inspectable(entry);
        if !seen.insert(lexical.clone()) {
            continue;
        }
        let normalized = lexical.to_string_lossy().into_owned();
        // composer :108 — `normalized.split(/[\\/]/).pop() || "attachment"`.
        let fallback_name = normalized
            .split(['/', '\\'])
            .next_back()
            .filter(|segment| !segment.is_empty())
            .unwrap_or("attachment");
        let name = clean_attachment_name(&entry.name, fallback_name);
        note_lines.push(format!("- {name}: {normalized}"));
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
        // TS composer :270-274 — assembled over raw UTF-16 (#1461): a line
        // keeps a lone surrogate the way the JS string carries it, not as a
        // replacement character.
        let mut units: Vec<u16> = "[Attachment diagnostics]\n".encode_utf16().collect();
        for (index, line) in diagnostics.iter().enumerate() {
            if index > 0 {
                units.push(u16::from(b'\n'));
            }
            units.extend("- ".encode_utf16());
            units.extend_from_slice(line.units());
        }
        blocks.push(object([
            ("type", "text".into()),
            ("text", JsValue::String(JsString::from_utf16(units))),
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
        // #2562: the filter skips `undefined` *and* `null`, the same nullish
        // read the request admission gate got in #2272 (requests.rs). The
        // submit path's TS source is `workspaceIdResolver(input.projectKey)`
        // (InProcessGateway.ts:954) and the local resolver is
        // `resolveProjectStorageId(requestedProjectKey ?? fallbackProjectRoot,
        // pilotHome)` (createLocalGateway.ts:1026-1028), so an explicit null is
        // "not provided" and takes the configured-project fallback instead of
        // the type error. Node v22.22.0 probes:
        // `_tmp/20261026-issue-2562/ts-probe/probe-{resolver,submit}.out`.
        // A non-string keeps the type error (the TS resolver chain throws on
        // it too — `resolve(7)` is ERR_INVALID_ARG_TYPE).
        if let Some(project) = input
            .get("projectKey")
            .filter(|v| !matches!(v, JsValue::Undefined | JsValue::Null))
        {
            let project = project
                .as_string()
                .ok_or_else(|| failure("projectKey must be a valid path string."))?;
            // #2617: the TS resolver chain runs a lone-surrogate `projectKey`
            // to completion (`resolveProjectStorageId` slugs the key lossily;
            // Node v22.22.0 probes: submit-level
            // `_tmp/20261111-issue-2617-probe/probe-submit-surrogate.out`,
            // resolver-level `_tmp/20261111-issue-2617/
            // probe-submit-surrogate-resolver.out`), so the string branch
            // converts lossily — the #2083 close-path shape (session_dir.rs)
            // — and only the `serves_project` capability gate decides. A
            // non-string keeps the type error above. Residual, accepted with
            // #2083: a key whose lossy form names the configured project (a
            // real path containing U+FFFD) is served.
            let project = String::from_utf16_lossy(project.units());
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
                                    // #1850: `eprintln!` panics when stderr is
                                    // unwritable, which would abort this
                                    // blocking task mid-release. The whole
                                    // loop is best-effort, so a dropped line
                                    // must not cost the remaining leases.
                                    let _ = writeln!(
                                        std::io::stderr(),
                                        "native upload lease release on error: {rel}"
                                    );
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
                                    // #1850: same as the error path above —
                                    // this runs detached from the submit
                                    // call, where an `eprintln!` panic would
                                    // silently kill the remaining releases.
                                    let _ = writeln!(
                                        std::io::stderr(),
                                        "native upload lease release: {e}"
                                    );
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

// ------------------------------------------------ U7 native settings (#1320)
use crate::config::store::{ConfigStore, StoreOptions};
use crate::runtime::refresh::{
    ConsoleLog, NoMemoryMaintenance, RefreshBundle, RefreshLog, RefreshOptions, RefreshRegistry,
    RefreshRouter, RegistryRefresh,
};
use crate::runtime::session::SessionOwner;
use crate::runtime::settings::{ResolvedSettingsPort, SettingsGateway, SettingsGatewayOptions};

/// The native U7 settings composition inputs. `router` and `registry` are the
/// bundle's collaborators as ports: production binds the session owner and
/// `RegistryRefresh` over the project runtime registry, tests substitute
/// recording collaborators to observe the dirty/broadcast order.
pub struct NativeSettingsOptions {
    /// The gateway's admission/close port: `hasActiveTurn` drives the
    /// SESSION_BUSY gate and `close` is the post-success session close.
    pub owner: SessionOwner,
    /// `getRouter`: the bundle's dirty router.
    pub router: Arc<dyn RefreshRouter>,
    /// The bundle's runtime registry.
    pub registry: Arc<dyn RefreshRegistry>,
    /// `createPilotConfigStoreSync({ projectRoot, env })`: the caller loads
    /// the store (a load failure is a boot failure, like the TS
    /// constructor's throw).
    pub store: ConfigStore,
    /// `projectRoot` for the bundle's log lines and diagnostics.
    pub project_root: JsString,
    /// The bundle's `log` (createLocalGateway.ts:806: `getConsoleLog()`).
    /// Production passes [`ConsoleLog`]; the #1320 replay passes a recorder
    /// because the golden pins the bundle's exact log lines.
    pub log: Arc<dyn RefreshLog>,
}

/// The composed U7 settings stack: the #1318 [`SettingsGateway`] over the
/// real refresh bundle and config store.
pub struct NativeSettings {
    pub gateway: Arc<SettingsGateway>,
    // Nothing in production reads the handle yet: the delegates inside the
    // gateway keep the bundle alive. #1320 fix-r1 F2: the production
    // composition never binds a notification server — the isolated native
    // process has no server-push channel, so the bundle's broadcasts are
    // simply lost (the connection registry is #1669, deliberately not built
    // here). Tests that need to observe the dirty/broadcast order must
    // `bind_server` a recording server themselves AND assert both
    // `has_notification_server()` and the difference from the production
    // assembly (`native_settings_production_binds_no_notification_server`).
    // Keep `dead_code`: only those tests read the handle.
    #[allow(dead_code)]
    pub bundle: Arc<RefreshBundle>,
}

/// Compose the native U7 settings stack (createLocalGateway.ts:795-831 and
/// 1383-1410): the real refresh bundle over the supplied registry, router
/// and store (attached exactly like `runtimeRefresh.attach()`), and the
/// settings gateway with the delegates the isolated native process owns —
/// `resolve_settings` over the store snapshot and the two reloads over the
/// bundle. `apply_flag_settings` / `update_settings` /
/// `reload_output_styles` stay unconfigured (their delegates are the SDK
/// session registry and the production local-settings writer, neither
/// assembled into the isolated native process) and answer the TS
/// unconfigured shapes through the component's own option handling.
///
/// #1320 fix-r1 F2: this composition never binds a notification server — the
/// isolated native process has no server-push channel, so the bundle's
/// broadcasts are lost (the connection registry is #1669, deliberately not
/// built here). Tests that need to observe the dirty/broadcast order must
/// `bind_server` a recording server themselves AND assert both
/// `has_notification_server()` and the difference from the production
/// assembly.
pub fn native_settings(options: NativeSettingsOptions) -> NativeSettings {
    let NativeSettingsOptions {
        owner,
        router,
        registry,
        store,
        project_root,
        log,
    } = options;
    let bundle = Arc::new(RefreshBundle::new(RefreshOptions {
        config_store: Arc::new(store.clone()),
        registry,
        memory_maintenance: Arc::new(NoMemoryMaintenance),
        router: Arc::new(move || Some(router.clone())),
        project_root,
        memory_diagnostics: None,
        sdk: None,
        log,
    }));
    // `runtimeRefresh.attach()` (createLocalGateway.ts:831): without the
    // subscription, an explicit `reload_config` still answers, but its
    // config change would never refresh runtimes or mark sessions dirty.
    bundle.attach();
    let gateway = Arc::new(SettingsGateway::new(SettingsGatewayOptions {
        owner,
        apply_flag_settings: None,
        update_settings: None,
        resolve_settings: Some(Arc::new(ResolvedSettingsPort {
            store: store.clone(),
        })),
        reload_output_styles: None,
        reload_config: Some(bundle.clone()),
        reload_extensions: Some(bundle.clone()),
    }));
    NativeSettings { gateway, bundle }
}

/// The production composition for the native process
/// (createLocalGateway.ts:795-835): [`native_settings`] with the
/// collaborators the isolated process owns — `createPilotConfigStoreSync`
/// over the served home/project (a load failure is the TS boot failure),
/// `RegistryRefresh` over the project runtime registry and the session
/// owner as the dirty router. The store anchors exactly like the process's
/// other config reads (`NativeConfigSource`: PILOT_HOME from the
/// environment, the served project as cwd and `projectRoot`).
///
/// #1320 fix-r1 F2: this composition never binds a notification server —
/// the isolated native process has no server-push channel, so the bundle's
/// broadcasts are lost (the connection registry is #1669, deliberately not
/// built here). Tests that need to observe the dirty/broadcast order must
/// `bind_server` a recording server themselves AND assert both
/// `has_notification_server()` and the difference from the production
/// assembly.
pub fn native_settings_for_process(
    owner: SessionOwner,
    project: Arc<crate::runtime::project::ProjectRuntimeRegistry>,
    home: &std::path::Path,
    served_project: &std::path::Path,
    env: &crate::config::Env,
) -> std::io::Result<NativeSettings> {
    // #1320 fix-r1 F3: same anchor as every other config read of the process
    // (`NativeConfigSource`): PILOT_HOME from the environment resolved
    // against the served project, `--home` only as the fallback homedir for
    // `~` / `~/` prefixes. In production the env arrives from
    // `Options::environment`, which has already overwritten PILOT_HOME with
    // `--home`, so the resolved anchor is always `--home` there; the explicit
    // `Some(home)` keeps direct callers (tests and future non-CLI
    // assemblies) anchored exactly like the CLI path.
    let pilot_home = crate::runtime::project::resolve_pilot_home(env, served_project, Some(home));
    let mut store_options = StoreOptions::new(pilot_home, served_project, env.clone());
    store_options.project = Some(served_project.to_string_lossy().into_owned());
    let store = ConfigStore::new(store_options).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "pilot config store failed to load: {} {}",
                error.code,
                String::from_utf16_lossy(error.message.units())
            ),
        )
    })?;
    Ok(native_settings(NativeSettingsOptions {
        owner: owner.clone(),
        router: Arc::new(owner),
        registry: Arc::new(RegistryRefresh::new(project)),
        store,
        project_root: JsString::from(served_project.to_string_lossy().into_owned()),
        log: Arc::new(ConsoleLog),
    }))
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

    /// Which injection point the swap happens in: right after `lease_guard`'s
    /// canonicalization but before the approved identity is sampled (#1468
    /// r2 — the approved stat must not follow a link planted here), after
    /// that sample but before `File::open` (#1468 — the open itself may then
    /// follow a swapped link), between `File::open` and the post-open
    /// `canonicalize` containment revalidation (step 2), between the
    /// revalidation and the `(dev, inode)` path-stat/fstat comparison
    /// (step 3), or between the handle's own stat — the authoritative
    /// oversize gate — and the bounded read (#1448: a file grown in that
    /// window only stays capped because `read_bounded` returns at most
    /// limit + 1 and the 读满 backstop fires). The #1468 multi-step
    /// sequence chains one swap per window, so every window holds its own
    /// slot.
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub enum SwapWindow {
        BeforeApprovedStat,
        BeforeOpen,
        AfterOpen,
        AfterRevalidate,
        BeforeRead,
    }

    type Hook = Box<dyn FnMut(&Path)>;

    thread_local! {
        static HOOKS: std::cell::RefCell<Vec<(SwapWindow, Hook)>> =
            std::cell::RefCell::default();
    }

    /// Removes exactly its own window's hook on drop.
    pub struct Guard {
        window: SwapWindow,
    }

    pub fn set(window: SwapWindow, hook: impl FnMut(&Path) + 'static) -> Guard {
        HOOKS.with(|hooks| {
            assert!(
                hooks.borrow().iter().all(|(set, _)| *set != window),
                "one swap hook per window"
            );
            hooks.borrow_mut().push((window, Box::new(hook)));
        });
        Guard { window }
    }

    pub fn notify(window: SwapWindow, resolved: &Path) {
        HOOKS.with(|hooks| {
            for (hook_window, hook) in hooks.borrow_mut().iter_mut() {
                if *hook_window == window {
                    hook(resolved);
                }
            }
        });
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            HOOKS.with(|hooks| {
                hooks
                    .borrow_mut()
                    .retain(|(window, _)| *window != self.window)
            });
        }
    }
}
