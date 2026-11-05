//! #1321 fix-r1 tests for the native attachment-turn composition
//! (`compose_attachment_turn`): the canonical lease-root guard (P1-1),
//! the TS-aligned inline size caps (P1-2), read-failure diagnostics
//! (P2a-1), and the silent skip of pathless attachments (P2a-2).

use super::services::compose_attachment_turn;
use crate::{metadata::object, negative_assert};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64_STANDARD};
use pilotdeck_protocol::{JsString, JsValue};
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

fn attachment(path: &str, mime: &str) -> JsValue {
    object([
        ("path", JsValue::from(path)),
        ("mimeType", JsValue::from(mime)),
    ])
}

fn attachment_with_bytes(path: &str, mime: &str, bytes: f64) -> JsValue {
    object([
        ("path", JsValue::from(path)),
        ("mimeType", JsValue::from(mime)),
        ("bytes", JsValue::Number(bytes)),
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
    let source = blocks[1]
        .get("source")
        .cloned()
        .unwrap_or(JsValue::Undefined);
    negative_assert!(
        source.get("type") == Some(&"base64".into())
            && source.get("media_type") == Some(&JsValue::from("image/png"))
            && source.get("data")
                == Some(&JsValue::from(
                    BASE64_STANDARD.encode([1, 2, 3, 4]).as_str()
                )),
        "#1321 normal lease image block must carry base64 data"
    );
    let note = block_text(&blocks[2]);
    negative_assert!(
        note.contains("[Registered attachment files in this session:]")
            && note.contains(&format!("- a.png: {path}")),
        "#1321 normal lease attachment must keep the registered-path note, got: {note}"
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
    let image_path = scratch.lease("big.png");
    std::fs::write(&image_path, vec![1u8; 5 * 1024 * 1024 + 1])
        .expect("#1321 oversize image fixture");
    let file_path = scratch.lease("big.txt");
    std::fs::write(&file_path, vec![b'a'; 1_000_001]).expect("#1321 oversize file fixture");
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
    let diagnostics = blocks
        .iter()
        .map(block_text)
        .find(|text| text.starts_with("[Attachment diagnostics]"))
        .unwrap_or_default();
    negative_assert!(
        diagnostics.contains(&format!(
            "Image {image_path} is 5242881 bytes (limit 5242880); skipped \
             (PilotDeck does not resize, intentional_difference §4.5)."
        )),
        "#1321 P1-2: oversize image must be skipped with the TS diagnostic, got: {diagnostics}"
    );
    negative_assert!(
        diagnostics.contains(&format!(
            "Attachment {file_path} is 1000001 bytes (limit 1000000); skipped."
        )),
        "#1321 P1-2: oversize file must be skipped with the TS diagnostic, got: {diagnostics}"
    );
    negative_assert!(
        blocks
            .iter()
            .all(|block| block.get("type") != Some(&"image".into())),
        "#1321 P1-2: oversize image must not be inlined"
    );
}

#[test]
fn declared_oversize_bytes_skip_without_reading() {
    let scratch = Scratch::new();
    let path = scratch.lease("declared-big.png");
    std::fs::write(&path, [9u8; 16]).expect("#1321 small actual fixture");
    // A manifest that itself declares an oversize image is rejected before
    // any read (fail-closed: the guard never reads to double-check it).
    let result = compose_attachment_turn(
        &scratch.0,
        &message("hello"),
        &[attachment_with_bytes(&path, "image/png", 6_000_000.0)],
    )
    .expect("#1321 declared-oversize attachments still compose the turn");
    let blocks = content_blocks(&result);
    let diagnostics = blocks
        .iter()
        .map(block_text)
        .find(|text| text.starts_with("[Attachment diagnostics]"))
        .unwrap_or_default();
    negative_assert!(
        diagnostics.contains(&format!(
            "Image {path} is 6000000 bytes (limit 5242880); skipped"
        )),
        "#1321 P1-2: manifest-declared oversize image must skip without reading, got: {diagnostics}"
    );
    negative_assert!(
        blocks
            .iter()
            .all(|block| block.get("type") != Some(&"image".into())),
        "#1321 P1-2: manifest-declared oversize image must not be inlined"
    );
}

#[test]
fn bytes_mismatch_uses_metadata() {
    let scratch = Scratch::new();
    // The manifest claims 10 bytes; the file is a 5 MiB + 1 image. The stat
    // is authoritative, so the smuggled oversize image is still skipped.
    let path = scratch.lease("smuggled.png");
    std::fs::write(&path, vec![1u8; 5 * 1024 * 1024 + 1]).expect("#1321 smuggled image fixture");
    let result = compose_attachment_turn(
        &scratch.0,
        &message("hello"),
        &[attachment_with_bytes(&path, "image/png", 10.0)],
    )
    .expect("#1321 mismatched manifest still composes the turn");
    let blocks = content_blocks(&result);
    let diagnostics = blocks
        .iter()
        .map(block_text)
        .find(|text| text.starts_with("[Attachment diagnostics]"))
        .unwrap_or_default();
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
fn read_failure_becomes_diagnostic() {
    let scratch = Scratch::new();
    // A directory inside the lease root canonicalizes and stats fine, so the
    // guard passes, but reading it fails (EISDIR): the failure must surface
    // as a diagnostic instead of silently dropping the image.
    let dir = scratch.lease("actually-a-dir.png");
    std::fs::create_dir_all(&dir).expect("#1321 directory fixture");
    let result = compose_attachment_turn(
        &scratch.0,
        &message("hello"),
        &[attachment(&dir, "image/png")],
    )
    .expect("#1321 unreadable attachments still compose the turn");
    let blocks = content_blocks(&result);
    let diagnostics = blocks
        .iter()
        .map(block_text)
        .find(|text| text.starts_with("[Attachment diagnostics]"))
        .unwrap_or_default();
    negative_assert!(
        diagnostics.contains(&format!("Image attachment not found: {dir} (")),
        "#1321 P2a-1: read failure must surface as a not-found diagnostic, got: {diagnostics}"
    );
    negative_assert!(
        blocks
            .iter()
            .all(|block| block.get("type") != Some(&"image".into())),
        "#1321 P2a-1: an unreadable image must not be inlined"
    );
}

#[test]
fn pathless_attachments_are_skipped_silently() {
    let scratch = Scratch::new();
    // TS drops attachments without a usable path instead of rejecting them
    // (GatewayAttachmentTurnComposer.ts:102, :229).
    let pathless = object([("mimeType", JsValue::from("image/png"))]);
    let empty_path = object([
        ("path", JsValue::from("")),
        ("mimeType", JsValue::from("image/png")),
    ]);
    let result = compose_attachment_turn(&scratch.0, &message("hello"), &[pathless, empty_path])
        .expect("#1321 pathless attachments compose the turn");
    let blocks = content_blocks(&result);
    negative_assert!(
        blocks.len() == 1 && block_text(&blocks[0]) == "hello",
        "#1321 P2a-2: pathless attachments must be skipped silently, got {} blocks",
        blocks.len()
    );
    negative_assert!(
        result.get("allowedReadFiles").is_none(),
        "#1321 P2a-2: pathless attachments must not register allowedReadFiles"
    );
}
