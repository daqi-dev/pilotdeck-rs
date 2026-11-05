//! #1321 fix-r2/r3 tests for the native attachment-turn composition
//! (`compose_attachment_turn`): the flat image block, the four guidance
//! forms of the registered-path note, text-extension inlining, the content
//! branches and plain-text fallback, the handle-based bounded read with its
//! 读满即超限 size gate, the lease guard (P1-1, TTL-race tolerant), the
//! fix-r3 hardening (lease-root containment, stat/open identity, channelKey
//! registration gate, pdf routing, audio-form selection, lone-surrogate
//! diagnostics, stat-authoritative oversize gate and the EISDIR turn
//! failure).

use super::services::{attachment_identity_matches, compose_attachment_turn};
use crate::{metadata::object, negative_assert};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64_STANDARD};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::path::{Path, PathBuf};

/// Scratch project under the system temp dir (never inside the worktree),
/// canonicalized up front like `native_process::tests::Scratch` so the
/// guard's lease-root comparison sees the same bytes the test builds.
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("pdrs-1321-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join(".tmp").join("chat-upload-leases").join("u1"))
            .expect("#1321 scratch lease dirs");
        let canonical = root.canonicalize().expect("#1321 scratch canonical path");
        // Keep the canonical lease root layout after the rename.
        std::fs::create_dir_all(canonical.join(".tmp").join("chat-upload-leases").join("u1"))
            .expect("#1321 scratch canonical lease dirs");
        Self(canonical)
    }
    fn lease(&self, name: &str) -> String {
        let path = self
            .0
            .join(".tmp")
            .join("chat-upload-leases")
            .join("u1")
            .join(name);
        path.to_str().expect("#1321 lease UTF-8").to_owned()
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A channel-registered path attachment: `metadata.channelKey` is what the
/// composer's registration gate requires (GatewayAttachmentTurnComposer.ts:181,
/// P1-f), so the registration/note fixtures carry it.
fn attachment(path: &str, mime: &str) -> JsValue {
    object([
        ("path", JsValue::from(path)),
        ("mimeType", JsValue::from(mime)),
        (
            "metadata",
            object([("channelKey", JsValue::from("chan-1"))]),
        ),
    ])
}

/// An uploaded lease attachment (GatewayUploadedAttachmentBundle.ts:57-62):
/// metadata without channelKey — inlined but never registered (P1-f).
fn upload_attachment(path: &str, mime: &str) -> JsValue {
    object([
        ("path", JsValue::from(path)),
        ("mimeType", JsValue::from(mime)),
        (
            "metadata",
            object([
                ("uploadId", JsValue::from("u1")),
                ("attachmentId", JsValue::from("a1")),
                ("relativePath", JsValue::from("memo.txt")),
                ("sha256", JsValue::from("00")),
            ]),
        ),
    ])
}

fn attachment_with_bytes(path: &str, mime: &str, bytes: f64) -> JsValue {
    object([
        ("path", JsValue::from(path)),
        ("mimeType", JsValue::from(mime)),
        ("bytes", JsValue::Number(bytes)),
        (
            "metadata",
            object([("channelKey", JsValue::from("chan-1"))]),
        ),
    ])
}

fn block_text(block: &JsValue) -> String {
    block
        .get("text")
        .and_then(JsValue::as_string)
        .and_then(|s| s.to_utf8().ok())
        .unwrap_or_default()
}

fn content_blocks(result: &JsValue) -> Vec<JsValue> {
    if let JsValue::Array(blocks) = result.get("content").unwrap_or(&JsValue::Undefined) {
        blocks.clone()
    } else {
        Vec::new()
    }
}

fn diagnostics_block(blocks: &[JsValue]) -> String {
    blocks
        .iter()
        .map(block_text)
        .find(|text| text.starts_with("[Attachment diagnostics]"))
        .unwrap_or_default()
}

fn note_block(blocks: &[JsValue]) -> String {
    blocks
        .iter()
        .map(block_text)
        .find(|text| text.contains("[Registered attachment files in this session:]"))
        .unwrap_or_default()
}

fn message(text: &str) -> JsValue {
    JsValue::String(JsString::from(text))
}

#[test]
fn normal_lease_image_is_inlined_and_registered() {
    let scratch = Scratch::new();
    let path = scratch.lease("a.png");
    std::fs::write(&path, [1u8, 2, 3, 4]).expect("#1321 lease file");
    let result = compose_attachment_turn(
        &scratch.0,
        &message("hello"),
        &[attachment(&path, "image/png")],
    )
    .expect("#1321 normal lease image composes");
    let blocks = content_blocks(&result);
    negative_assert!(
        blocks.len() == 3,
        "#1321 normal lease attachment must keep message, image and note blocks, got {}",
        blocks.len()
    );
    negative_assert!(
        block_text(&blocks[0]) == "hello",
        "#1321 normal lease attachment must keep the message text block"
    );
    negative_assert!(
        blocks[1].get("type") == Some(&"image".into()),
        "#1321 normal lease image must be inlined as an image block"
    );
    // TS AttachmentResolver.ts:267-275 — the flat image block shape.
    negative_assert!(
        blocks[1].get("source") == Some(&"base64".into())
            && blocks[1].get("data")
                == Some(&JsValue::from(
                    BASE64_STANDARD.encode([1, 2, 3, 4]).as_str()
                ))
            && blocks[1].get("mimeType") == Some(&JsValue::from("image/png")),
        "#1321 flat image block must carry source/data/mimeType at the top level"
    );
    negative_assert!(
        blocks[1].get("bytes") == Some(&JsValue::Number(4.0)),
        "#1321 flat image block must report the bytes actually read"
    );
    // TS GatewayAttachmentTurnComposer.ts:118-122 — the plain-guidance note.
    let note = block_text(&blocks[2]);
    negative_assert!(
        note == format!(
            "\n\n[Registered attachment files in this session:]\n- a.png: {path}\n\
             These are path references for reuse. If an image/PDF is already visible in this \
             turn, do not call read_file just to view it."
        ),
        "#1321 path note must match the TS composer shape, got: {note}"
    );
    let allowed = match result.get("allowedReadFiles") {
        Some(JsValue::Array(files)) => files.clone(),
        _ => Vec::new(),
    };
    negative_assert!(
        allowed == vec![JsValue::from(path.as_str())],
        "#1321 normal lease attachment must register allowedReadFiles"
    );
}

#[cfg(unix)]
#[test]
fn lease_symlink_escape_is_rejected() {
    use std::os::unix::fs::symlink;
    let scratch = Scratch::new();
    // Sibling of the scratch root: a path the lease symlink escapes to.
    let outside = scratch.0.with_file_name(format!(
        "{}-outside-secret.png",
        scratch
            .0
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("pdrs-1321")
    ));
    std::fs::write(&outside, b"secret").expect("#1321 outside fixture");
    let link = scratch.lease("evil.png");
    symlink(&outside, Path::new(&link)).expect("#1321 lease symlink");
    let result = compose_attachment_turn(
        &scratch.0,
        &message("hello"),
        &[attachment(&link, "image/png")],
    );
    let Err(error) = result else {
        std::fs::remove_file(outside).ok();
        panic!("#1321 P1-1: lease symlink escape must be rejected, attachment was accepted");
    };
    std::fs::remove_file(outside).ok();
    negative_assert!(
        error
            .message
            .contains("outside the permitted lease directory"),
        "#1321 P1-1: lease symlink escape must be rejected, got: {}",
        error.message
    );
}

#[test]
fn lease_parent_dotdot_is_rejected() {
    let scratch = Scratch::new();
    let path = format!(
        "{}/../escape.png",
        scratch
            .0
            .join(".tmp")
            .join("chat-upload-leases")
            .to_str()
            .expect("#1321 lease root UTF-8")
    );
    let result = compose_attachment_turn(
        &scratch.0,
        &message("hello"),
        &[attachment(&path, "image/png")],
    );
    let Err(error) = result else {
        panic!("#1321 P1-1: '..' lease path must be rejected, attachment was accepted");
    };
    negative_assert!(
        error
            .message
            .contains("outside the permitted lease directory"),
        "#1321 P1-1: '..' lease path must be rejected, got: {}",
        error.message
    );
}

#[test]
fn oversize_image_and_file_skip_to_diagnostics() {
    let scratch = Scratch::new();
    // limit + 1024, not limit + 1: a mutant that drops the read bound must
    // inline the file, so the oversize fixture has to be distinguishable
    // from an exactly-limit+1 read.
    let image_path = scratch.lease("big.png");
    std::fs::write(&image_path, vec![1u8; 5 * 1024 * 1024 + 1024])
        .expect("#1321 oversize image fixture");
    let file_path = scratch.lease("big.txt");
    std::fs::write(&file_path, vec![b'a'; 1_000_000 + 1024]).expect("#1321 oversize file fixture");
    let result = compose_attachment_turn(
        &scratch.0,
        &message("hello"),
        &[
            attachment(&image_path, "image/png"),
            attachment(&file_path, "text/plain"),
        ],
    )
    .expect("#1321 oversize attachments still compose the turn");
    let blocks = content_blocks(&result);
    let diagnostics = diagnostics_block(&blocks);
    negative_assert!(
        diagnostics.contains(&format!(
            "Image {image_path} is 5243904 bytes (limit 5242880); skipped \
             (PilotDeck does not resize, intentional_difference §4.5)."
        )),
        "#1321 P1-2: oversize image must be skipped with the TS diagnostic, got: {diagnostics}"
    );
    negative_assert!(
        diagnostics.contains(&format!(
            "Attachment {file_path} is 1001024 bytes (limit 1000000); skipped."
        )),
        "#1321 P1-2: oversize file must be skipped with the TS diagnostic, got: {diagnostics}"
    );
    negative_assert!(
        blocks
            .iter()
            .all(|block| block.get("type") != Some(&"image".into())),
        "#1321 P1-2: oversize image must not be inlined"
    );
    negative_assert!(
        blocks
            .iter()
            .all(|block| !block_text(block).starts_with("<attachment path=")),
        "#1321 P1-2: oversize file must not be inlined"
    );
}

#[test]
fn declared_bytes_do_not_gate_inline_size() {
    let scratch = Scratch::new();
    let path = scratch.lease("declared-big.png");
    std::fs::write(&path, [9u8; 16]).expect("#1321 small actual fixture");
    // TS AttachmentResolver.ts:187-202: path requests always trust the
    // actual stat, never the attachment `bytes` field — the manifest's
    // 6 MB declaration must not skip the 16 real bytes.
    let result = compose_attachment_turn(
        &scratch.0,
        &message("hello"),
        &[attachment_with_bytes(&path, "image/png", 6_000_000.0)],
    )
    .expect("#1321 declared-oversize attachments still compose the turn");
    let blocks = content_blocks(&result);
    negative_assert!(
        blocks.len() == 3,
        "#1321 declared bytes must not change the composition, got {} blocks",
        blocks.len()
    );
    negative_assert!(
        blocks[1].get("type") == Some(&"image".into())
            && blocks[1].get("bytes") == Some(&JsValue::Number(16.0)),
        "#1321 the actually-16-byte image must be inlined with its real size"
    );
    negative_assert!(
        diagnostics_block(&blocks).is_empty(),
        "#1321 a small file with an oversize declaration must not diagnose"
    );
}

#[test]
fn bytes_mismatch_uses_metadata() {
    let scratch = Scratch::new();
    // The manifest claims 10 bytes; the file is a 5 MiB + 1 image. The
    // handle stat is authoritative, so the smuggled oversize image is
    // still skipped.
    let path = scratch.lease("smuggled.png");
    std::fs::write(&path, vec![1u8; 5 * 1024 * 1024 + 1]).expect("#1321 smuggled image fixture");
    let result = compose_attachment_turn(
        &scratch.0,
        &message("hello"),
        &[attachment_with_bytes(&path, "image/png", 10.0)],
    )
    .expect("#1321 mismatched manifest still composes the turn");
    let blocks = content_blocks(&result);
    let diagnostics = diagnostics_block(&blocks);
    negative_assert!(
        diagnostics.contains(&format!(
            "Image {path} is 5242881 bytes (limit 5242880); skipped"
        )),
        "#1321 P1-2: metadata must win over a mismatched manifest size, got: {diagnostics}"
    );
    negative_assert!(
        blocks
            .iter()
            .all(|block| block.get("type") != Some(&"image".into())),
        "#1321 P1-2: metadata-gated oversize image must not be inlined"
    );
}

#[test]
fn read_failure_fails_the_turn() {
    let scratch = Scratch::new();
    // P1-e (codex, ts-probe/probe.out "P1-e composer dir-*"): the resolver's
    // try/catch wraps only the stat — a directory path stats fine but the
    // read throws EISDIR uncaught, so the whole TS turn rejects. The Rust
    // composition fails the same way instead of converting the read error
    // into a diagnostic.
    let dir = scratch.lease("actually-a-dir.png");
    std::fs::create_dir_all(&dir).expect("#1321 directory fixture");
    let result = compose_attachment_turn(
        &scratch.0,
        &message("hello"),
        &[attachment(&dir, "image/png")],
    );
    let Err(error) = result else {
        panic!("#1321 P1-e: a directory read failure must fail the turn");
    };
    negative_assert!(
        error.message.contains("could not be read"),
        "#1321 P1-e: the read failure must reject the turn, got: {}",
        error.message
    );
    negative_assert!(
        error.message.contains(dir.as_str()),
        "#1321 P1-e: the rejection must name the unreadable attachment, got: {}",
        error.message
    );
}

#[test]
fn pathless_attachments_are_skipped_silently() {
    let scratch = Scratch::new();
    // TS drops attachments without a usable path instead of rejecting them
    // (GatewayAttachmentTurnComposer.ts:229), and with no blocks and no note
    // the turn falls back to the plain text input (composer :67-72).
    let pathless = object([("mimeType", JsValue::from("image/png"))]);
    let empty_path = object([
        ("path", JsValue::from("")),
        ("mimeType", JsValue::from("image/png")),
    ]);
    let result = compose_attachment_turn(&scratch.0, &message("hello"), &[pathless, empty_path])
        .expect("#1321 pathless attachments compose the turn");
    negative_assert!(
        result.get("type") == Some(&"text".into()) && result.get("text") == Some(&message("hello")),
        "#1321 P2a-2: an empty composition must fall back to the plain text turn"
    );
    negative_assert!(
        result.get("content").is_none(),
        "#1321 P2a-2: the plain-text fallback carries no blocks"
    );
    negative_assert!(
        result.get("allowedReadFiles").is_none(),
        "#1321 P2a-2: pathless attachments must not register allowedReadFiles"
    );
}

#[test]
fn lone_surrogate_path_becomes_not_found_diagnostic() {
    let scratch = Scratch::new();
    // P1-d (codex, ts-probe/probe.out "P1-d composer lone-surrogate"): a
    // lone-surrogate path is truthy in TS; `path.resolve` keeps it, fs
    // mangles it to U+FFFD, the stat fails ENOENT and the resolver emits
    // the image-not-found diagnostic. Rust spells the path lossily
    // (U+FFFD); it never reaches the lease guard, an open, or
    // allowedReadFiles.
    let mut fields = JsObject::new();
    fields.insert(
        "path".into(),
        JsValue::String(JsString::from_utf16(vec![0xD800])),
    );
    fields.insert("mimeType".into(), JsValue::from("image/png"));
    let result = compose_attachment_turn(&scratch.0, &message("hello"), &[JsValue::Object(fields)])
        .expect("#1321 a lone-surrogate path composes the turn");
    let lossy = "\u{FFFD}";
    let blocks = content_blocks(&result);
    negative_assert!(
        diagnostics_block(&blocks).contains(&format!(
            "Image attachment not found: {lossy} \
             (ENOENT: no such file or directory, stat '{lossy}')."
        )),
        "#1321 P1-d: the lone-surrogate path must become the TS not-found \
         diagnostic, got: {}",
        diagnostics_block(&blocks)
    );
    negative_assert!(
        result.get("allowedReadFiles").is_none(),
        "#1321 P1-d: a lone-surrogate path must not register allowedReadFiles"
    );
    negative_assert!(
        note_block(&blocks).is_empty(),
        "#1321 P1-d: a lone-surrogate path must not produce a note line"
    );
}

#[test]
fn unresolvable_lease_path_becomes_not_found_diagnostic() {
    let scratch = Scratch::new();
    // Lexically inside the lease root but the file is gone (TTL cleanup
    // race): a TS-shaped not-found diagnostic, never a whole-turn failure.
    let path = scratch.lease("ttl-gone.png");
    let result = compose_attachment_turn(
        &scratch.0,
        &message("hello"),
        &[attachment(&path, "image/png")],
    )
    .expect("#1321 a TTL-raced lease path must not fail the turn");
    let blocks = content_blocks(&result);
    let diagnostics = diagnostics_block(&blocks);
    negative_assert!(
        diagnostics.contains(&format!("Image attachment not found: {path} (")),
        "#1321 P2a-3: an unresolvable lease path must diagnose not-found, got: {diagnostics}"
    );
    negative_assert!(
        result.get("allowedReadFiles").is_none(),
        "#1321 P2a-3: an unresolvable lease path must not register allowedReadFiles"
    );
    negative_assert!(
        note_block(&blocks).is_empty(),
        "#1321 P2a-3: an unresolvable lease path must not produce a note line"
    );
}

#[test]
fn missing_lease_root_behaves_like_no_lease() {
    let scratch = Scratch::new();
    let path = scratch.lease("gone-root.png");
    std::fs::remove_dir_all(scratch.0.join(".tmp")).expect("#1321 remove lease root");
    // A project without any upload lease root must not panic: the path is
    // simply unresolvable.
    let result = compose_attachment_turn(
        &scratch.0,
        &message("hello"),
        &[attachment(&path, "image/png")],
    )
    .expect("#1321 a missing lease root must compose the turn");
    let blocks = content_blocks(&result);
    negative_assert!(
        diagnostics_block(&blocks).contains(&format!("Image attachment not found: {path} (")),
        "#1321 a missing lease root must diagnose not-found instead of panicking"
    );
}

#[test]
fn exact_limit_image_and_text_are_inlined() {
    let scratch = Scratch::new();
    // The caps are inclusive (TS stat `>` comparisons,
    // AttachmentResolver.ts:164 and :202): exactly 5242880 / 1000000 bytes
    // must still be inlined.
    let image_path = scratch.lease("exact.png");
    std::fs::write(&image_path, vec![7u8; 5 * 1024 * 1024]).expect("#1321 exact image fixture");
    let text_path = scratch.lease("exact.txt");
    std::fs::write(&text_path, vec![b'x'; 1_000_000]).expect("#1321 exact text fixture");
    let result = compose_attachment_turn(
        &scratch.0,
        &message("hello"),
        &[
            attachment(&image_path, "image/png"),
            attachment(&text_path, "text/plain"),
        ],
    )
    .expect("#1321 exact-limit attachments compose the turn");
    let blocks = content_blocks(&result);
    negative_assert!(
        blocks.len() == 4,
        "#1321 exact-limit attachments must inline both blocks, got {} blocks",
        blocks.len()
    );
    negative_assert!(
        blocks[1].get("type") == Some(&"image".into())
            && blocks[1].get("bytes") == Some(&JsValue::Number(5_242_880.0)),
        "#1321 an exactly-5242880-byte image must be inlined with its real size"
    );
    negative_assert!(
        block_text(&blocks[2])
            == format!(
                "<attachment path=\"{text_path}\">\n{}\n</attachment>",
                "x".repeat(1_000_000)
            ),
        "#1321 an exactly-1000000-byte text attachment must be inlined in the TS wrapper"
    );
    negative_assert!(
        diagnostics_block(&blocks).is_empty(),
        "#1321 exact-limit attachments must not diagnose"
    );
}

#[test]
fn binary_extension_attachment_stays_path_reference() {
    let scratch = Scratch::new();
    // TS AttachmentResolver.ts:149-163: only whitelisted text extensions are
    // read; a .docx stays a registered path reference, and because it is not
    // read_file-inspectable the note takes the conversion guidance
    // (GatewayAttachmentTurnComposer.ts:147-149).
    let path = scratch.lease("doc.docx");
    std::fs::write(&path, b"PK\x03\x04office").expect("#1321 docx fixture");
    let result = compose_attachment_turn(
        &scratch.0,
        &message("hello"),
        &[attachment(
            &path,
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        )],
    )
    .expect("#1321 binary attachments compose the turn");
    let blocks = content_blocks(&result);
    negative_assert!(
        blocks.len() == 2,
        "#1321 a binary-container attachment must stay a path reference, got {} blocks",
        blocks.len()
    );
    let note = note_block(&blocks);
    negative_assert!(
        note.contains(&format!("- doc.docx: {path}")),
        "#1321 the binary attachment must keep its note line, got: {note}"
    );
    negative_assert!(
        note.ends_with(
            "Some attachments were not shown inline. These registered files are not \
             directly inspectable with read_file; ask for a supported export or convert \
             them before inspection."
        ),
        "#1321 the no-inspectable guidance form must be selected, got: {note}"
    );
}

#[test]
fn guidance_inspectable_form_via_oversize_text() {
    let scratch = Scratch::new();
    // An oversize .txt is registered and inspectable but produces a
    // diagnostic, so the guidance takes the read_file-scoped form
    // (GatewayAttachmentTurnComposer.ts:150).
    let path = scratch.lease("big2.txt");
    std::fs::write(&path, vec![b'b'; 1_000_000 + 512]).expect("#1321 oversize text fixture");
    let result = compose_attachment_turn(
        &scratch.0,
        &message("hello"),
        &[attachment(&path, "text/plain")],
    )
    .expect("#1321 oversize text composes the turn");
    let blocks = content_blocks(&result);
    negative_assert!(
        diagnostics_block(&blocks).contains(&format!(
            "Attachment {path} is 1000512 bytes (limit 1000000); skipped."
        )),
        "#1321 the oversize text must be skipped with the TS diagnostic"
    );
    let note = note_block(&blocks);
    negative_assert!(
        note.ends_with(
            "Some attachments were not shown inline. Use read_file with the exact path only \
             for readable text, image, PDF, or notebook attachments; Office/archive/binary \
             files need conversion before inspection."
        ),
        "#1321 the inspectable guidance form must be selected, got: {note}"
    );
}

#[test]
fn guidance_audio_form_names_project_local_path() {
    let scratch = Scratch::new();
    // TS GatewayAttachmentTurnComposer.ts:131-140 — an audio attachment
    // stays a registered path reference and selects the funasr guidance
    // with the project-local path hint.
    let path = scratch.lease("memo.mp3");
    std::fs::write(&path, vec![0u8; 32]).expect("#1321 audio fixture");
    let result = compose_attachment_turn(
        &scratch.0,
        &message("hello"),
        &[attachment(&path, "audio/mpeg")],
    )
    .expect("#1321 audio attachments compose the turn");
    let blocks = content_blocks(&result);
    negative_assert!(
        blocks.len() == 2,
        "#1321 an audio attachment must stay a path reference, got {} blocks",
        blocks.len()
    );
    let note = note_block(&blocks);
    negative_assert!(
        note == format!(
            "\n\n[Registered attachment files in this session:]\n- memo.mp3: {path}\n\
             Audio attachments are not readable with read_file. When the user asks for \
             transcription, subtitles, or audio analysis, use the funasr MCP server's \
             mcp__funasr__transcribe_audio tool. Pass the registered project-local path \
             {path} to transcribe_audio. If that tool reports that its runtime is missing, \
             run npm run install:asr and retry the tool in this session."
        ),
        "#1321 the audio guidance form must match the TS composer text, got: {note}"
    );
    let allowed = match result.get("allowedReadFiles") {
        Some(JsValue::Array(files)) => files.clone(),
        _ => Vec::new(),
    };
    negative_assert!(
        allowed == vec![JsValue::from(path.as_str())],
        "#1321 the audio attachment must register allowedReadFiles"
    );
}

#[test]
fn content_attachments_inline_before_path_checks() {
    let scratch = Scratch::new();
    // TS GatewayAttachmentTurnComposer.ts:212-227 — content blocks are
    // produced before the path handling, without needing a path or a lease.
    let image_content = object([
        ("type", JsValue::from("image")),
        (
            "content",
            JsValue::from(BASE64_STANDARD.encode("ABC").as_str()),
        ),
        ("mimeType", JsValue::from("image/png")),
        ("bytes", JsValue::Number(3.0)),
    ]);
    let text_content = object([
        ("type", JsValue::from("text")),
        ("content", JsValue::from("inline text")),
    ]);
    let result = compose_attachment_turn(
        &scratch.0,
        &message("hello"),
        &[image_content, text_content],
    )
    .expect("#1321 content attachments compose without any path or lease");
    let blocks = content_blocks(&result);
    negative_assert!(
        blocks.len() == 3,
        "#1321 content attachments must keep message, image and text blocks, got {}",
        blocks.len()
    );
    let image = &blocks[1];
    negative_assert!(
        image.get("type") == Some(&"image".into())
            && image.get("source") == Some(&"base64".into())
            && image.get("data") == Some(&JsValue::from(BASE64_STANDARD.encode("ABC").as_str()))
            && image.get("mimeType") == Some(&JsValue::from("image/png"))
            && image.get("bytes") == Some(&JsValue::Number(3.0)),
        "#1321 content image block must be the flat shape with the declared bytes"
    );
    negative_assert!(
        block_text(&blocks[2]) == "inline text",
        "#1321 a text-content attachment must become a plain text block"
    );
    negative_assert!(
        result.get("allowedReadFiles").is_none(),
        "#1321 content attachments without paths must not register allowedReadFiles"
    );
    negative_assert!(
        note_block(&blocks).is_empty(),
        "#1321 content attachments without paths must not produce a note"
    );
}

#[test]
fn note_name_cleaning_and_dedupe() {
    let scratch = Scratch::new();
    let path = scratch.lease("same.txt");
    std::fs::write(&path, b"body").expect("#1321 dedupe fixture");
    let named = object([
        ("path", JsValue::from(path.as_str())),
        ("mimeType", JsValue::from("text/plain")),
        ("name", JsValue::from("multi\r\nline\nname")),
        (
            "metadata",
            object([("channelKey", JsValue::from("chan-1"))]),
        ),
    ]);
    let unnamed = attachment(&path, "text/plain");
    let newline_only = object([
        ("path", JsValue::from(path.as_str())),
        ("mimeType", JsValue::from("text/plain")),
        ("name", JsValue::from("\n")),
        (
            "metadata",
            object([("channelKey", JsValue::from("chan-1"))]),
        ),
    ]);
    let result = compose_attachment_turn(
        &scratch.0,
        &message("hello"),
        &[named, unnamed, newline_only],
    )
    .expect("#1321 duplicate attachments compose the turn");
    let blocks = content_blocks(&result);
    let note = note_block(&blocks);
    // TS GatewayAttachmentTurnComposer.ts:105-111 — dedupe by normalized
    // path, the first name wins, and CR/LF runs collapse to one space.
    negative_assert!(
        note.contains(&format!("- multi line name: {path}")),
        "#1321 note names must collapse CR/LF runs to a single space, got: {note}"
    );
    negative_assert!(
        !note.contains("multi\r") && !note.contains("multi\n"),
        "#1321 note names must not leak raw CR/LF, got: {note}"
    );
    negative_assert!(
        note.lines().filter(|line| line.starts_with("- ")).count() == 1,
        "#1321 duplicate normalized paths must produce exactly one note line, got: {note}"
    );
}

#[test]
fn image_mime_falls_back_to_extension() {
    let scratch = Scratch::new();
    // TS AttachmentResolver.ts:214-215 — a missing mimeType falls back to
    // the IMAGE_MIME extension map.
    let path = scratch.lease("photo.jpg");
    std::fs::write(&path, [1u8, 2]).expect("#1321 jpg fixture");
    let entry = object([
        ("path", JsValue::from(path.as_str())),
        ("type", JsValue::from("image")),
    ]);
    let result = compose_attachment_turn(&scratch.0, &message("hi"), &[entry])
        .expect("#1321 extension-typed image composes");
    let blocks = content_blocks(&result);
    negative_assert!(
        blocks[1].get("type") == Some(&"image".into())
            && blocks[1].get("mimeType") == Some(&JsValue::from("image/jpeg")),
        "#1321 an image without mimeType must use the TS extension map"
    );
}

#[test]
fn image_without_known_mime_is_diagnosed() {
    let scratch = Scratch::new();
    // TS AttachmentResolver.ts:216-227 — neither a mimeType nor a mapped
    // extension is an attachment_unsupported warning.
    let path = scratch.lease("blob.bin");
    std::fs::write(&path, [1u8, 2]).expect("#1321 bin fixture");
    let entry = object([
        ("path", JsValue::from(path.as_str())),
        ("type", JsValue::from("image")),
    ]);
    let result = compose_attachment_turn(&scratch.0, &message("hi"), &[entry])
        .expect("#1321 unmapped image composes the turn");
    let blocks = content_blocks(&result);
    negative_assert!(
        diagnostics_block(&blocks).contains(&format!(
            "Cannot determine image mime type from {path}; provide mimeType explicitly."
        )),
        "#1321 an image without a determinable mime must be diagnosed"
    );
    negative_assert!(
        blocks
            .iter()
            .all(|block| block.get("type") != Some(&"image".into())),
        "#1321 an image without a determinable mime must not be inlined"
    );
}

#[cfg(unix)]
#[test]
fn lease_root_swapped_outside_project_disables_resolution() {
    use std::os::unix::fs::symlink;
    let scratch = Scratch::new();
    // P0-a (codex): the lease root replaced by a symlink pointing outside
    // the project must not become a passing prefix — every path attachment
    // becomes a not-found diagnostic; nothing is read or registered.
    let outside_root = scratch.0.with_file_name(format!(
        "{}-outside-lease",
        scratch
            .0
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("pdrs-1321")
    ));
    std::fs::create_dir_all(outside_root.join("u1")).expect("#1321 outside lease fixture");
    std::fs::write(outside_root.join("u1").join("secret.png"), b"secret-bytes")
        .expect("#1321 outside lease file");
    let lease_root = scratch.0.join(".tmp").join("chat-upload-leases");
    std::fs::remove_dir_all(&lease_root).expect("#1321 remove the real lease root");
    symlink(&outside_root, &lease_root).expect("#1321 swapped lease root symlink");
    let path = scratch.lease("secret.png");
    let result = compose_attachment_turn(
        &scratch.0,
        &message("hello"),
        &[attachment(&path, "image/png")],
    )
    .expect("#1321 a swapped lease root must not fail the turn");
    let blocks = content_blocks(&result);
    negative_assert!(
        diagnostics_block(&blocks).contains(&format!(
            "Image attachment not found: {path} (attachment lease root is unavailable)."
        )),
        "#1321 P0-a: a swapped lease root must diagnose every attachment \
         not-found, got: {}",
        diagnostics_block(&blocks)
    );
    negative_assert!(
        blocks
            .iter()
            .all(|block| block.get("type") != Some(&"image".into())),
        "#1321 P0-a: a swapped lease root must never inline outside bytes"
    );
    negative_assert!(
        result.get("allowedReadFiles").is_none(),
        "#1321 P0-a: a swapped lease root must never register allowedReadFiles"
    );
    negative_assert!(
        note_block(&blocks).is_empty(),
        "#1321 P0-a: a swapped lease root must never produce a note line"
    );
    std::fs::remove_dir_all(&outside_root).ok();
}

#[test]
fn attachment_identity_distinguishes_files() {
    let scratch = Scratch::new();
    // P0-b: the resolved path stat recorded before the open and the opened
    // handle's fstat identify the same file by (dev, inode) — a link swapped
    // between the two lookups opens a different file and must not match.
    let a = scratch.lease("identity-a.txt");
    let b = scratch.lease("identity-b.txt");
    std::fs::write(&a, b"a").expect("#1321 identity fixture a");
    std::fs::write(&b, b"b").expect("#1321 identity fixture b");
    let stat_a = std::fs::metadata(&a).expect("#1321 stat a");
    let stat_a_again = std::fs::metadata(&a).expect("#1321 stat a again");
    let stat_b = std::fs::metadata(&b).expect("#1321 stat b");
    let opened_a = std::fs::File::open(&a)
        .expect("#1321 open a")
        .metadata()
        .expect("#1321 fstat a");
    negative_assert!(
        attachment_identity_matches(&stat_a, &opened_a),
        "#1321 P0-b: the opened handle must match its own pre-open stat"
    );
    negative_assert!(
        attachment_identity_matches(&stat_a, &stat_a_again),
        "#1321 P0-b: two stats of the same file must match"
    );
    negative_assert!(
        !attachment_identity_matches(&stat_a, &stat_b),
        "#1321 P0-b: two different files must not match — the swap check \
         would pass a swapped link"
    );
}

#[test]
fn content_block_skips_the_path_branch() {
    let scratch = Scratch::new();
    // P1-a (composer :221/:226 `continue`): an attachment that already
    // produced a content block never reaches the path branch — a
    // root-escaping path can neither fail the turn nor be read.
    let outside = scratch.0.with_file_name("pdrs-1321-outside-content.png");
    let outside_path = outside.to_str().expect("#1321 outside UTF-8").to_owned();
    let image_content = object([
        ("type", JsValue::from("image")),
        (
            "content",
            JsValue::from(BASE64_STANDARD.encode("ABC").as_str()),
        ),
        ("mimeType", JsValue::from("image/png")),
        ("bytes", JsValue::Number(3.0)),
        ("path", JsValue::from(outside_path.as_str())),
    ]);
    let text_content = object([
        ("type", JsValue::from("text")),
        ("content", JsValue::from("inline text")),
        ("path", JsValue::from(outside_path.as_str())),
    ]);
    let result = compose_attachment_turn(
        &scratch.0,
        &message("hello"),
        &[image_content, text_content],
    )
    .expect("#1321 P1-a: content attachments with outside paths must not fail the turn");
    let blocks = content_blocks(&result);
    negative_assert!(
        blocks.len() == 3,
        "#1321 P1-a: content attachments must keep message, image and text blocks, got {}",
        blocks.len()
    );
    let image = &blocks[1];
    negative_assert!(
        image.get("type") == Some(&"image".into())
            && image.get("data") == Some(&JsValue::from(BASE64_STANDARD.encode("ABC").as_str())),
        "#1321 P1-a: the image content block must stay intact"
    );
    negative_assert!(
        block_text(&blocks[2]) == "inline text",
        "#1321 P1-a: the text content block must stay intact"
    );
    negative_assert!(
        diagnostics_block(&blocks).is_empty(),
        "#1321 P1-a: the outside path must not produce a diagnostic"
    );
    negative_assert!(
        result.get("allowedReadFiles").is_none(),
        "#1321 P1-a: the outside path must not register allowedReadFiles"
    );
    negative_assert!(
        note_block(&blocks).is_empty(),
        "#1321 P1-a: the outside path must not produce a note line"
    );
}

#[test]
fn audio_form_is_selected_by_attachment_presence() {
    let scratch = Scratch::new();
    // P1-b (composer :131-140): ANY audio attachment selects the audio
    // guidance — even one without a path, whose hint is then the generic
    // project-local wording (:138).
    let path = scratch.lease("note.txt");
    std::fs::write(&path, b"body").expect("#1321 text fixture");
    let pathless_audio = object([("name", JsValue::from("memo.mp3"))]);
    let result = compose_attachment_turn(
        &scratch.0,
        &message("hello"),
        &[pathless_audio, attachment(&path, "text/plain")],
    )
    .expect("#1321 audio-shaped attachments compose the turn");
    let blocks = content_blocks(&result);
    let note = note_block(&blocks);
    negative_assert!(
        note == format!(
            "\n\n[Registered attachment files in this session:]\n- note.txt: {path}\n\
             Audio attachments are not readable with read_file. When the user asks for \
             transcription, subtitles, or audio analysis, use the funasr MCP server's \
             mcp__funasr__transcribe_audio tool. Pass a project-local host path to \
             transcribe_audio; paths outside this project are rejected. If that tool \
             reports that its runtime is missing, run npm run install:asr and retry the \
             tool in this session."
        ),
        "#1321 P1-b: a pathless audio attachment must select the audio guidance \
         with the generic hint, got: {note}"
    );
}

#[test]
fn oversize_directory_is_skipped_by_the_stat_gate() {
    let scratch = Scratch::new();
    // P1-c: the stat gate must reject by stat alone — a >1 MB .txt directory
    // never reaches the EISDIR read (which fails the whole turn, P1-e). APFS
    // and ext4 grow a directory's stat size with its entry count, so hard
    // links inflate it deterministically.
    let dir = scratch.lease("bigdir.txt");
    std::fs::create_dir_all(&dir).expect("#1321 big directory fixture");
    let seed = Path::new(&dir).join("seed");
    std::fs::write(&seed, b"x").expect("#1321 seed fixture");
    let mut entries = 0usize;
    while std::fs::metadata(&dir).expect("#1321 dir stat").len() <= 1_000_000 {
        entries += 1;
        std::fs::hard_link(&seed, Path::new(&dir).join(format!("h{entries}")))
            .expect("#1321 dir entry fixture");
        negative_assert!(
            entries <= 500_000,
            "#1321 cannot inflate the directory stat size past the limit"
        );
    }
    let size = std::fs::metadata(&dir).expect("#1321 dir stat").len();
    let result = compose_attachment_turn(
        &scratch.0,
        &message("hello"),
        &[attachment(&dir, "text/plain")],
    )
    .expect("#1321 an oversize directory attachment must not fail the turn");
    let blocks = content_blocks(&result);
    negative_assert!(
        diagnostics_block(&blocks).contains(&format!(
            "Attachment {dir} is {size} bytes (limit 1000000); skipped."
        )),
        "#1321 P1-c: the stat gate must skip the oversize directory by its \
         fstat size, got: {}",
        diagnostics_block(&blocks)
    );
    negative_assert!(
        blocks
            .iter()
            .all(|block| !block_text(block).starts_with("<attachment path=")),
        "#1321 P1-c: the oversize directory must not be inlined"
    );
}

#[test]
fn uploaded_lease_attachments_are_not_registered() {
    let scratch = Scratch::new();
    // P1-f (composer :181, bundle :57-62): uploaded lease attachments carry
    // metadata without channelKey — they are still inlined but never enter
    // allowedReadFiles or the path note.
    let image_path = scratch.lease("uploaded.png");
    std::fs::write(&image_path, [1u8, 2, 3]).expect("#1321 upload image fixture");
    let result = compose_attachment_turn(
        &scratch.0,
        &message("hello"),
        &[upload_attachment(&image_path, "image/png")],
    )
    .expect("#1321 uploaded lease attachments compose the turn");
    let blocks = content_blocks(&result);
    negative_assert!(
        blocks.len() == 2 && blocks[1].get("type") == Some(&"image".into()),
        "#1321 P1-f: the uploaded attachment must still be inlined, got {} blocks",
        blocks.len()
    );
    negative_assert!(
        result.get("allowedReadFiles").is_none(),
        "#1321 P1-f: an uploaded lease attachment must not register allowedReadFiles"
    );
    negative_assert!(
        note_block(&blocks).is_empty(),
        "#1321 P1-f: an uploaded lease attachment must not produce a note line"
    );
}

#[test]
fn pdf_attachment_routes_by_suffix_and_mime() {
    let scratch = Scratch::new();
    // P1-g (composer :240-245, resolver :287-320): the pdf request wins for
    // an `application/pdf` mime even on a .txt path, and for a bare .pdf
    // suffix; the pdf is stat-only, `Math.max(1, Math.round(size /
    // 102_400))` estimates the pages and the bytes are never read.
    let pdf_path = scratch.lease("doc.pdf");
    std::fs::write(&pdf_path, vec![7u8; 250_000]).expect("#1321 pdf fixture");
    let txt_path = scratch.lease("renamed.txt");
    std::fs::write(&txt_path, vec![7u8; 250_000]).expect("#1321 pdf-as-txt fixture");
    let suffix_entry = object([("path", JsValue::from(pdf_path.as_str()))]);
    let mime_entry = attachment(&txt_path, "application/pdf");
    let result =
        compose_attachment_turn(&scratch.0, &message("hello"), &[suffix_entry, mime_entry])
            .expect("#1321 pdf attachments compose the turn");
    let blocks = content_blocks(&result);
    negative_assert!(
        blocks.len() == 4,
        "#1321 P1-g: message, two pdf blocks and the note are expected, got {}",
        blocks.len()
    );
    negative_assert!(
        block_text(&blocks[1])
            == format!(
                "[PDF attachment: {pdf_path}, 250000 bytes, estimated 2 pages. \
                 Use read_file on this registered attachment path to inspect it.]"
            ),
        "#1321 P1-g: the suffix-routed pdf block must match the TS shape, got: {}",
        block_text(&blocks[1])
    );
    negative_assert!(
        block_text(&blocks[2])
            == format!(
                "[PDF attachment: {txt_path}, 250000 bytes, estimated 2 pages. \
                 Use read_file on this registered attachment path to inspect it.]"
            ),
        "#1321 P1-g: the mime-routed pdf block must match the TS shape, got: {}",
        block_text(&blocks[2])
    );
    negative_assert!(
        diagnostics_block(&blocks).is_empty(),
        "#1321 P1-g: a stat-only pdf must not diagnose"
    );
}

#[test]
fn pdf_attachment_not_found_is_diagnosed() {
    let scratch = Scratch::new();
    // P1-g (resolver :293-302): a missing pdf path produces the pdf-spelled
    // not-found diagnostic.
    let missing = scratch.lease("gone.pdf");
    let result = compose_attachment_turn(
        &scratch.0,
        &message("hello"),
        &[attachment(&missing, "application/pdf")],
    )
    .expect("#1321 a missing pdf composes the turn");
    let blocks = content_blocks(&result);
    negative_assert!(
        diagnostics_block(&blocks).contains(&format!("PDF attachment not found: {missing} (")),
        "#1321 P1-g: a missing pdf must produce the pdf not-found diagnostic, got: {}",
        diagnostics_block(&blocks)
    );
    negative_assert!(
        blocks
            .iter()
            .all(|block| !block_text(block).contains("[PDF attachment: ")),
        "#1321 P1-g: a missing pdf must not produce an estimate block"
    );
}

#[test]
fn oversize_pdf_still_estimates_pages() {
    let scratch = Scratch::new();
    // P1-g: the pdf request is stat-only — no byte limit applies, a >5 MiB
    // pdf still produces the estimate (6291456 / 102400 = 61.44 → 61 pages).
    let pdf_path = scratch.lease("huge.pdf");
    std::fs::write(&pdf_path, vec![7u8; 6 * 1024 * 1024]).expect("#1321 huge pdf fixture");
    let result =
        compose_attachment_turn(&scratch.0, &message("hello"), &[attachment(&pdf_path, "")])
            .expect("#1321 an oversize pdf composes the turn");
    let blocks = content_blocks(&result);
    negative_assert!(
        block_text(&blocks[1])
            == format!(
                "[PDF attachment: {pdf_path}, 6291456 bytes, estimated 61 pages. \
                 Use read_file on this registered attachment path to inspect it.]"
            ),
        "#1321 P1-g: the oversize pdf must still estimate its pages, got: {}",
        block_text(&blocks[1])
    );
    negative_assert!(
        diagnostics_block(&blocks).is_empty(),
        "#1321 P1-g: an oversize pdf must not diagnose a byte limit"
    );
}

#[cfg(unix)]
#[test]
fn symlink_loop_lease_path_fails_the_turn() {
    use std::os::unix::fs::symlink;
    let scratch = Scratch::new();
    // P2: only the ENOENT canonicalize failure softens into the not-found
    // diagnostic; a symlink loop (ELOOP) is a hard turn failure.
    let link = scratch.lease("loop.png");
    symlink(Path::new(&link), Path::new(&link)).expect("#1321 symlink loop fixture");
    let result = compose_attachment_turn(
        &scratch.0,
        &message("hello"),
        &[attachment(&link, "image/png")],
    );
    let Err(error) = result else {
        panic!("#1321 P2: a symlink-loop lease path must fail the turn");
    };
    negative_assert!(
        error.message.contains("cannot be resolved"),
        "#1321 P2: a non-ENOENT canonicalize failure must be a hard failure, got: {}",
        error.message
    );
}
