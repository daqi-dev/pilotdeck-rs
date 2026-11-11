//! #1321 fix-r2/r3 tests for the native attachment-turn composition
//! (`compose_attachment_turn`): the flat image block, the four guidance
//! forms of the registered-path note, text-extension inlining, the content
//! branches and plain-text fallback, the handle-based bounded read with its
//! 读满即超限 size gate, the lease guard (P1-1, TTL-race tolerant), the
//! fix-r3 hardening (lease-root containment, stat/open identity, channelKey
//! registration gate, pdf routing, audio-form selection, lone-surrogate
//! diagnostics, stat-authoritative oversize gate and the EISDIR turn
//! failure) plus the attach-followups-a follow-ups (#1458 pdf_size_estimate
//! info, #1469 attachment_unsupported diagnostics, #1462 the Node EISDIR
//! message, #1459/#1464 the lease-root/lease-path softening and #1629 the
//! path-level canonicalize softening), the #1460
//! two-pass split (a direct-content attachment with a channelKey path is
//! still registered; its path failures stay silent like the audio skip —
//! including the open failure of an unopenable lease file),
//! the #1461 lone-surrogate fidelity (the outer diagnostic keeps the
//! surrogate and the resolve prefix, only the fs error spelling is lossy,
//! and the route words follow the raw spelling rather than the resolved
//! tail),
//! and the #1320 fix-r1 F2/F3 settings-composition tests (the production
//! stack binds no notification server; the settings store anchors at
//! PILOT_HOME),
//! and the #2495 note-pass domain split (the hasInspectable accumulation
//! sits before the lexical note dedup, and both the note lines and the
//! accumulation key on the allowedReadFiles membership test instead of
//! channel registration — note_dedup_*,
//! dual_spelling_accumulates_inspectable_per_entry,
//! unregistered_colliding_spelling_* and
//! unregistered_non_member_inspectable_does_not_count).

use super::services::{
    NativeSettingsOptions, attachment_identity_matches, canonical_upload_store,
    compose_attachment_turn, handle_bound_to_upload_areas, native_settings,
    native_settings_for_process, swap_test_hook,
};
use crate::config::Env;
use crate::config::store::{ConfigStore, StoreOptions};
use crate::runtime::project::{
    DisposeFuture, ProjectRuntimeRegistry, ResourceBundle, RuntimeError, RuntimeResult,
};
use crate::runtime::refresh::{
    BoxFuture, ConsoleLog, NotificationServer, RefreshRegistry, RefreshRouter,
};
use crate::runtime::session::{
    LiveSession, LiveSessionHandle, SessionContext, SessionFactory, SessionOwner,
    SessionOwnerOptions,
};
use crate::{metadata::object, negative_assert, negative_assert_eq};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64_STANDARD};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::path::{Path, PathBuf};
use std::sync::Arc;

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

/// #1461: a block's text as raw UTF-16 code units — a lone surrogate in
/// the diagnostic text is not representable in a `String`, so the lossy
/// `block_text` would silently erase exactly the thing being pinned.
fn block_text_units(block: &JsValue) -> Vec<u16> {
    block
        .get("text")
        .and_then(JsValue::as_string)
        .map(|text| text.units().to_vec())
        .unwrap_or_default()
}

fn units_contains(haystack: &[u16], needle: &[u16]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
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
fn lease_symlink_escape_softens_to_not_found() {
    use std::os::unix::fs::symlink;
    let scratch = Scratch::new();
    // #1464: a channelKey path whose leaf symlinks outside the lease root
    // no longer rejects the whole turn — the TS composer fails no turn over
    // a path — but takes the Unresolved not-found branch instead, and the
    // outside bytes are still never read, inlined or registered (P1-1).
    // Sibling of the scratch root: a path the lease symlink escapes to.
    let outside = scratch.0.with_file_name(format!(
        "{}-outside-secret.png",
        scratch
            .0
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("pdrs-1321")
    ));
    std::fs::write(&outside, "outside-secret-1464").expect("#1464 outside fixture");
    let link = scratch.lease("evil.png");
    symlink(&outside, Path::new(&link)).expect("#1464 lease symlink");
    let result = crate::test_negative_receipt::expect_ok(
        compose_attachment_turn(
            &scratch.0,
            &message("hello"),
            &[attachment(&link, "image/png")],
        ),
        "#1464 a lease symlink escape must not fail the turn",
    );
    std::fs::remove_file(outside).ok();
    let blocks = content_blocks(&result);
    negative_assert!(
        diagnostics_block(&blocks).contains(&format!(
            "Image attachment not found: {link} (attachment resolved outside the permitted \
             lease directory)."
        )),
        "#1464 a lease symlink escape must soften to a not-found diagnostic, got: {}",
        diagnostics_block(&blocks)
    );
    negative_assert!(
        blocks
            .iter()
            .all(|block| !block_text(block).contains("outside-secret-1464")),
        "#1464 the outside bytes must never appear in any block"
    );
    negative_assert!(
        blocks
            .iter()
            .all(|block| block.get("type") != Some(&"image".into())),
        "#1464 a lease symlink escape must never inline outside bytes"
    );
    negative_assert!(
        result.get("allowedReadFiles").is_none(),
        "#1464 a lease symlink escape must never register allowedReadFiles"
    );
    negative_assert!(
        note_block(&blocks).is_empty(),
        "#1464 a lease symlink escape must never produce a note line"
    );
}

#[cfg(unix)]
#[test]
fn symlinked_project_spelling_keeps_lease_attachments() {
    use std::os::unix::fs::symlink;
    let scratch = Scratch::new();
    // #1618 r1: `projectKey` keeps Node's path.resolve identity (symlinks
    // included), so the lease paths the client echoes carry the project's
    // logical spelling. The lexical root has to use that same spelling —
    // comparing against a canonicalized root dropped every lease attachment of
    // a symlinked project into the not-found branch.
    let alias = scratch.0.with_file_name(format!(
        "{}-alias",
        scratch
            .0
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("pdrs-1321")
    ));
    symlink(&scratch.0, &alias).expect("#1618 project alias symlink");
    let path = alias
        .join(".tmp")
        .join("chat-upload-leases")
        .join("u1")
        .join("a.png");
    std::fs::write(&path, [1u8, 2, 3, 4]).expect("#1618 alias lease file");
    let logical = path.to_str().expect("#1618 lease UTF-8").to_owned();
    let result = compose_attachment_turn(
        &alias,
        &message("hello"),
        &[attachment(&logical, "image/png")],
    )
    .expect("#1618 a symlinked project spelling must still compose");
    // Canonicalize the lease file while the alias still resolves: unlinking the
    // alias first makes `canonicalize` fail with ENOENT and the test dies on
    // its own expectation (#1618 r3, MiniMax/DeepSeek).
    let physical = std::fs::canonicalize(&path)
        .expect("#1618 alias lease file canonicalization")
        .to_str()
        .expect("#1618 canonical lease UTF-8")
        .to_owned();
    std::fs::remove_file(&alias).ok();
    let blocks = content_blocks(&result);
    negative_assert!(
        blocks
            .iter()
            .any(|block| block.get("type") == Some(&"image".into())),
        "#1618 a lease attachment under a symlinked project must inline, got: {}",
        diagnostics_block(&blocks)
    );
    let allowed = match result.get("allowedReadFiles") {
        Some(JsValue::Array(files)) => files.clone(),
        _ => Vec::new(),
    };
    // #1618 + #1446: allowedReadFiles carries the dual registration of TS
    // `collectRegisteredAttachmentReadFiles` (composer :182-183) — the
    // client's lexical spelling first, then the canonicalized realpath —
    // and the note line prints the lexical one
    // (`safeAllowedAttachmentPath`, composer :167-171). Under a symlinked
    // project the two spellings differ, so both must appear.
    negative_assert!(
        allowed
            == vec![
                JsValue::from(logical.as_str()),
                JsValue::from(physical.as_str()),
            ],
        "#1446 a lease attachment under a symlinked project must register both spellings, got: {allowed:?}"
    );
    negative_assert!(
        note_block(&blocks).contains(&format!("- a.png: {logical}")),
        "#1446 the note line must carry the client's lexical spelling, got: {}",
        note_block(&blocks)
    );
    negative_assert!(
        !diagnostics_block(&blocks).contains("not found"),
        "#1618 an in-lease attachment must not get a not-found diagnostic, got: {}",
        diagnostics_block(&blocks)
    );
}

#[cfg(unix)]
#[test]
fn lease_logical_root_keeps_alias_spelled_attachments_inline() {
    use std::os::unix::fs::symlink;
    let scratch = Scratch::new();
    // #1618 item 5: `lease_lexical` must stay the project's *logical* spelling
    // (`project_dir.join(".tmp").join("chat-upload-leases")`); canonicalizing it
    // back to the realpath root drops every lease attachment a client echoes
    // under the symlinked project spelling into the not-found branch. This is
    // the static-assertion copy of
    // `symlinked_project_spelling_keeps_lease_attachments` above — its killing
    // assertions are formatted (`got: …`), so the #946 receipt protocol cannot
    // bind a mutation row to it.
    let alias = scratch.0.with_file_name(format!(
        "{}-alias",
        scratch
            .0
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("pdrs-1321")
    ));
    symlink(&scratch.0, &alias).expect("#1618 project alias symlink");
    let path = alias
        .join(".tmp")
        .join("chat-upload-leases")
        .join("u1")
        .join("a.png");
    std::fs::write(&path, [1u8, 2, 3, 4]).expect("#1618 alias lease file");
    let logical = path.to_str().expect("#1618 lease UTF-8").to_owned();
    let result = crate::test_negative_receipt::expect_ok(
        compose_attachment_turn(
            &alias,
            &message("hello"),
            &[attachment(&logical, "image/png")],
        ),
        "#1618 an alias-spelled lease attachment must compose the turn",
    );
    // The alias is a sibling of `Scratch::drop`'s root, so nothing else
    // removes it; drop it before the assertions so a failing
    // `negative_assert!` does not leak it either (r5 review P2a, same
    // cleanup as `symlinked_project_spelling_keeps_lease_attachments`).
    std::fs::remove_file(&alias).ok();
    let blocks = content_blocks(&result);
    negative_assert!(
        blocks
            .iter()
            .any(|block| block.get("type") == Some(&"image".into())),
        "#1618 the logical spelling of the lease root must keep alias-spelled lease attachments inline"
    );
}

#[cfg(unix)]
#[test]
fn lease_canonical_root_keeps_physically_spelled_attachments_inline() {
    use std::os::unix::fs::symlink;
    let scratch = Scratch::new();
    // #1618 item 5 (the other spelling): a client that resolved the project
    // first hands back the canonical lease spelling, so the lexical pre-filter
    // carries the canonicalized root as its second entry
    // (`lease_lexical_roots = [lease_lexical, lease_lexical_canon]`). Dropping
    // the canonical entry sends the physically-spelled lease attachment to the
    // not-found branch even though the project alias still resolves.
    let alias = scratch.0.with_file_name(format!(
        "{}-alias",
        scratch
            .0
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("pdrs-1321")
    ));
    symlink(&scratch.0, &alias).expect("#1618 project alias symlink");
    // `Scratch::lease` spells the physical root (not the alias), which is
    // exactly the canonical spelling this test must keep inline.
    let spelling = scratch.lease("a.png");
    std::fs::write(&spelling, [1u8, 2, 3, 4]).expect("#1618 lease file");
    let result = crate::test_negative_receipt::expect_ok(
        compose_attachment_turn(
            &alias,
            &message("hello"),
            &[attachment(&spelling, "image/png")],
        ),
        "#1618 a physically-spelled lease attachment must compose the turn",
    );
    // Same as above: the alias lives outside `Scratch::drop`'s root and is
    // dead once the turn is composed; remove it before the assertions so a
    // failing `negative_assert!` does not leak it (r5 review P2a).
    std::fs::remove_file(&alias).ok();
    let blocks = content_blocks(&result);
    negative_assert!(
        blocks
            .iter()
            .any(|block| block.get("type") == Some(&"image".into())),
        "#1618 the canonical spelling of the lease root must keep physically-spelled lease attachments inline"
    );
}

#[test]
fn dual_registration_carries_both_spellings() {
    use std::os::unix::fs::symlink;
    let scratch = Scratch::new();
    // #1446 — the resolved≠realpath golden (ts-probe/p2-dual-path.out.txt,
    // real GatewayAttachmentTurnComposer on an intermediate-symlink
    // fixture): `collectRegisteredAttachmentReadFiles` (composer :182-183)
    // registers BOTH the lexical `resolve(path)` and the canonicalized
    // realpath. Here the intermediate link sits inside the lease tree
    // (`u1-link -> u1`), the counterpart of the project-level alias in the
    // #1618 test above.
    let leases = scratch.0.join(".tmp").join("chat-upload-leases");
    std::fs::write(leases.join("u1").join("a.png"), [1u8, 2, 3, 4]).expect("#1446 lease file");
    symlink("u1", leases.join("u1-link")).expect("#1446 lease dir link");
    let via_link = leases
        .join("u1-link")
        .join("a.png")
        .to_str()
        .expect("#1446 link spelling UTF-8")
        .to_owned();
    let physical = leases
        .join("u1")
        .join("a.png")
        .to_str()
        .expect("#1446 physical spelling UTF-8")
        .to_owned();
    let result = compose_attachment_turn(
        &scratch.0,
        &message("hello"),
        &[attachment(&via_link, "image/png")],
    )
    .expect("#1446 an in-lease attachment through a link must compose");
    let blocks = content_blocks(&result);
    let allowed = match result.get("allowedReadFiles") {
        Some(JsValue::Array(files)) => files.clone(),
        _ => Vec::new(),
    };
    negative_assert!(
        allowed
            == vec![
                JsValue::from(via_link.as_str()),
                JsValue::from(physical.as_str()),
            ],
        "#1446 the dual registration must carry the link spelling and the physical spelling"
    );
    negative_assert!(
        note_block(&blocks).contains(&format!("- a.png: {via_link}")),
        "#1446 the note line must carry the link spelling"
    );
    negative_assert!(
        !note_block(&blocks).contains(&format!("- a.png: {physical}")),
        "#1446 the note line must not carry the physical spelling"
    );
}

#[test]
fn dual_registration_dedups_the_set_across_spellings() {
    use std::os::unix::fs::symlink;
    let scratch = Scratch::new();
    // #1446 — the Set semantics: two attachments naming the same file by its
    // two spellings add the same entries twice (TS `allowed` Set keeps
    // [L, C]) while each spelling still gets its own note line (the TS
    // `seen` dedup is on the lexical spelling, composer :102/:168).
    let leases = scratch.0.join(".tmp").join("chat-upload-leases");
    std::fs::write(leases.join("u1").join("a.png"), [1u8, 2, 3, 4]).expect("#1446 lease file");
    symlink("u1", leases.join("u1-link")).expect("#1446 lease dir link");
    let via_link = leases
        .join("u1-link")
        .join("a.png")
        .to_str()
        .expect("#1446 link spelling UTF-8")
        .to_owned();
    let physical = leases
        .join("u1")
        .join("a.png")
        .to_str()
        .expect("#1446 physical spelling UTF-8")
        .to_owned();
    let result = compose_attachment_turn(
        &scratch.0,
        &message("hello"),
        &[
            attachment(&via_link, "image/png"),
            attachment(&physical, "image/png"),
        ],
    )
    .expect("#1446 both spellings must compose");
    let blocks = content_blocks(&result);
    let allowed = match result.get("allowedReadFiles") {
        Some(JsValue::Array(files)) => files.clone(),
        _ => Vec::new(),
    };
    negative_assert!(
        allowed
            == vec![
                JsValue::from(via_link.as_str()),
                JsValue::from(physical.as_str()),
            ],
        "#1446 the Set must keep one entry per spelling"
    );
    let note = note_block(&blocks);
    negative_assert!(
        note.contains(&format!("- a.png: {via_link}"))
            && note.contains(&format!("- a.png: {physical}")),
        "#1446 each spelling must get its own note line"
    );
}

/// #2495 — `application/vnd.openxmlformats-officedocument
/// .wordprocessingml.document`: a binary-container extension, so the entry
/// is NOT read_file-inspectable (composer :163).
const DOCX_MIME: &str = "application/vnd.openxmlformats-officedocument.\
                         wordprocessingml.document";

#[test]
fn note_dedup_keeps_second_entry_inspectable() {
    let scratch = Scratch::new();
    // #2495 — the same lexical spelling twice, the first entry not
    // read_file-inspectable (.docx) and the second inspectable (mime
    // text/plain): the note line dedups to the first entry (#1446,
    // composer :105) but `hasInspectableAttachment` is a separate
    // `attachments.some(...)` over the whole attachment list (composer
    // :142-151), so the skipped second entry still selects the read_file
    // guidance form (ts-probe `_tmp/20261026-issue-2495/probe/probe.out`
    // scenario A).
    let path = scratch.lease("same.docx");
    std::fs::write(&path, b"body").expect("#2495 dedupe fixture");
    let result = compose_attachment_turn(
        &scratch.0,
        &message("hello"),
        &[
            attachment(&path, DOCX_MIME),
            attachment(&path, "text/plain"),
        ],
    )
    .expect("#2495 duplicate attachments compose the turn");
    let blocks = content_blocks(&result);
    let note = note_block(&blocks);
    negative_assert!(
        note.lines().filter(|line| line.starts_with("- ")).count() == 1,
        "#2495 the duplicate spelling must keep exactly one note line, got: {note}"
    );
    negative_assert!(
        note.contains(&format!("- same.docx: {path}")),
        "#2495 the surviving note line must be the first entry's spelling, got: {note}"
    );
    negative_assert!(
        note.ends_with(
            "Some attachments were not shown inline. Use read_file with the exact path only \
             for readable text, image, PDF, or notebook attachments; Office/archive/binary \
             files need conversion before inspection."
        ),
        // Static message: the negative-control receipt protocol compares the
        // runtime message byte-for-byte (#2495 fix-r2), so a bound assertion
        // never interpolates diagnostics into its own identity.
        "#2495 the deduped second entry's inspectable must still select the read_file form"
    );
}

#[test]
fn note_dedup_first_inspectable_order_control() {
    let scratch = Scratch::new();
    // #2495 order control (probe scenario A2): with the first entry
    // inspectable the read_file form holds without the second entry —
    // the accumulation is an OR, never a replacement.
    let path = scratch.lease("order.docx");
    std::fs::write(&path, b"body").expect("#2495 order fixture");
    let result = compose_attachment_turn(
        &scratch.0,
        &message("hello"),
        &[
            attachment(&path, "text/plain"),
            attachment(&path, DOCX_MIME),
        ],
    )
    .expect("#2495 reversed duplicates compose the turn");
    let blocks = content_blocks(&result);
    let note = note_block(&blocks);
    negative_assert!(
        note.lines().filter(|line| line.starts_with("- ")).count() == 1,
        "#2495 the reversed duplicate must keep exactly one note line, got: {note}"
    );
    negative_assert!(
        note.ends_with(
            "Some attachments were not shown inline. Use read_file with the exact path only \
             for readable text, image, PDF, or notebook attachments; Office/archive/binary \
             files need conversion before inspection."
        ),
        "#2495 the first entry's inspectable must select the read_file form, got: {note}"
    );
}

#[test]
fn note_dedup_both_non_inspectable_keeps_conversion_form() {
    let scratch = Scratch::new();
    // #2495 negative control (probe scenario C): both entries
    // non-inspectable → the conversion form; the accumulation must not
    // flip to true on its own.
    let path = scratch.lease("none.docx");
    std::fs::write(&path, b"body").expect("#2495 conversion fixture");
    let result = compose_attachment_turn(
        &scratch.0,
        &message("hello"),
        &[
            attachment(&path, DOCX_MIME),
            attachment(&path, "application/octet-stream"),
        ],
    )
    .expect("#2495 non-inspectable duplicates compose the turn");
    let blocks = content_blocks(&result);
    let note = note_block(&blocks);
    negative_assert!(
        note.ends_with(
            "Some attachments were not shown inline. These registered files are not \
             directly inspectable with read_file; ask for a supported export or convert \
             them before inspection."
        ),
        "#2495 two non-inspectable entries must keep the conversion form"
    );
}

#[cfg(unix)]
#[test]
fn dual_spelling_accumulates_inspectable_per_entry() {
    use std::os::unix::fs::symlink;
    let scratch = Scratch::new();
    // #2495 — the double spelling with differing inspectable (probe
    // scenario B): the link entry is not inspectable, the physical entry
    // is. Each spelling keeps its own note line and its allowedReadFiles
    // entry (#1446), and the physical entry's inspectable selects the
    // read_file form — the note pass and the collection pass dedup
    // independently of the hasInspectable accumulation.
    let leases = scratch.0.join(".tmp").join("chat-upload-leases");
    std::fs::write(leases.join("u1").join("b.docx"), b"body").expect("#2495 dual-spelling fixture");
    symlink("u1", leases.join("u1-link")).expect("#2495 lease dir link");
    let via_link = leases
        .join("u1-link")
        .join("b.docx")
        .to_str()
        .expect("#2495 link spelling UTF-8")
        .to_owned();
    let physical = leases
        .join("u1")
        .join("b.docx")
        .to_str()
        .expect("#2495 physical spelling UTF-8")
        .to_owned();
    let result = compose_attachment_turn(
        &scratch.0,
        &message("hello"),
        &[
            attachment(&via_link, DOCX_MIME),
            attachment(&physical, "text/plain"),
        ],
    )
    .expect("#2495 dual-spelling attachments compose the turn");
    let blocks = content_blocks(&result);
    let allowed = match result.get("allowedReadFiles") {
        Some(JsValue::Array(files)) => files.clone(),
        _ => Vec::new(),
    };
    negative_assert!(
        allowed
            == vec![
                JsValue::from(via_link.as_str()),
                JsValue::from(physical.as_str()),
            ],
        "#2495 the dual spelling must register both spellings, got: {allowed:?}"
    );
    let note = note_block(&blocks);
    negative_assert!(
        note.contains(&format!("- b.docx: {via_link}"))
            && note.contains(&format!("- b.docx: {physical}")),
        "#2495 each spelling must keep its own note line, got: {note}"
    );
    negative_assert!(
        note.ends_with(
            "Some attachments were not shown inline. Use read_file with the exact path only \
             for readable text, image, PDF, or notebook attachments; Office/archive/binary \
             files need conversion before inspection."
        ),
        "#2495 the physical entry's inspectable must select the read_file form, got: {note}"
    );
}

#[test]
fn unregistered_colliding_spelling_counts_for_guidance() {
    let scratch = Scratch::new();
    // #2495 — TS gates `hasInspectableAttachment` only on the
    // allowedReadFiles membership test (composer :144), never on the
    // entry's own registration: an uploaded (channelKey-less) entry whose
    // spelling collides with a registered one still counts (probe
    // scenario D). The note line is still deduped on the shared spelling.
    let path = scratch.lease("collide.docx");
    std::fs::write(&path, b"body").expect("#2495 collision fixture");
    let result = compose_attachment_turn(
        &scratch.0,
        &message("hello"),
        &[
            attachment(&path, DOCX_MIME),
            upload_attachment(&path, "text/plain"),
        ],
    )
    .expect("#2495 colliding attachments compose the turn");
    let blocks = content_blocks(&result);
    let note = note_block(&blocks);
    negative_assert!(
        note.lines().filter(|line| line.starts_with("- ")).count() == 1,
        "#2495 the colliding spelling must keep exactly one note line, got: {note}"
    );
    negative_assert!(
        note.ends_with(
            "Some attachments were not shown inline. Use read_file with the exact path only \
             for readable text, image, PDF, or notebook attachments; Office/archive/binary \
             files need conversion before inspection."
        ),
        "#2495 the unregistered colliding entry's inspectable must still select the read_file form"
    );
}

#[cfg(unix)]
#[test]
fn unregistered_colliding_spelling_gets_note_line() {
    use std::os::unix::fs::symlink;
    let scratch = Scratch::new();
    // #2495 — the note lines use the same membership test (composer :103):
    // an uploaded entry naming the registered attachment's realpath
    // spelling gets its own line (probe scenario E).
    let leases = scratch.0.join(".tmp").join("chat-upload-leases");
    std::fs::write(leases.join("u1").join("e.docx"), b"body").expect("#2495 note-domain fixture");
    symlink("u1", leases.join("e-link")).expect("#2495 lease dir link");
    let via_link = leases
        .join("e-link")
        .join("e.docx")
        .to_str()
        .expect("#2495 link spelling UTF-8")
        .to_owned();
    let physical = leases
        .join("u1")
        .join("e.docx")
        .to_str()
        .expect("#2495 physical spelling UTF-8")
        .to_owned();
    let result = compose_attachment_turn(
        &scratch.0,
        &message("hello"),
        &[
            attachment(&via_link, DOCX_MIME),
            upload_attachment(&physical, "text/plain"),
        ],
    )
    .expect("#2495 note-domain attachments compose the turn");
    let blocks = content_blocks(&result);
    let note = note_block(&blocks);
    negative_assert!(
        note.contains(&format!("- e.docx: {via_link}"))
            && note.contains(&format!("- e.docx: {physical}")),
        "#2495 the unregistered entry naming the registered realpath spelling must get its own note line"
    );
    negative_assert!(
        note.ends_with(
            "Some attachments were not shown inline. Use read_file with the exact path only \
             for readable text, image, PDF, or notebook attachments; Office/archive/binary \
             files need conversion before inspection."
        ),
        "#2495 the unregistered colliding entry's inspectable must select the read_file form, got: {note}"
    );
}

#[test]
fn unregistered_non_member_inspectable_does_not_count() {
    let scratch = Scratch::new();
    // #2495 fix-r1 (sibling review P2) — the inspectable-side dual of the
    // #1469 note-line gate: `hasInspectableAttachment` keys on the
    // allowedReadFiles membership test (composer :144), never on the
    // entry's own registration or inspectability alone. The registered
    // .docx is a member but not read_file-inspectable; the uploaded .pdf
    // is inspectable but its own spelling is a member of nothing. Only a
    // member's inspectable may flip the guidance, so the conversion form
    // must hold — an accumulation hoisted above the membership gate
    // (or keyed on registration) would take the read_file form instead.
    let docx_path = scratch.lease("member.docx");
    std::fs::write(&docx_path, b"PK\x03\x04office").expect("#2495 member fixture");
    let pdf_path = scratch.lease("outsider.pdf");
    std::fs::write(&pdf_path, b"%PDF-1.4").expect("#2495 outsider fixture");
    let result = compose_attachment_turn(
        &scratch.0,
        &message("hello"),
        &[
            attachment(&docx_path, DOCX_MIME),
            upload_attachment(&pdf_path, "application/pdf"),
        ],
    )
    .expect("#2495 non-member attachments compose the turn");
    let blocks = content_blocks(&result);
    let note = note_block(&blocks);
    negative_assert!(
        note.contains(&format!("- member.docx: {docx_path}")),
        "#2495 the registered member must keep its note line, got: {note}"
    );
    negative_assert!(
        !note.contains(&format!("- outsider.pdf: {pdf_path}")),
        "#2495 the non-member upload must not get a note line, got: {note}"
    );
    negative_assert!(
        note.ends_with(
            "Some attachments were not shown inline. These registered files are not \
             directly inspectable with read_file; ask for a supported export or convert \
             them before inspection."
        ),
        "#2495 a non-member inspectable must not flip the guidance to the read_file form"
    );
}

#[test]
fn lease_parent_dotdot_softens_to_not_found() {
    let scratch = Scratch::new();
    // #1464: a `..` lease path is lexically outside the root — the
    // per-attachment not-found diagnostic, never a whole-turn rejection,
    // with the same no-read/no-registration boundary.
    let path = format!(
        "{}/../escape.png",
        scratch
            .0
            .join(".tmp")
            .join("chat-upload-leases")
            .to_str()
            .expect("#1464 lease root UTF-8")
    );
    let result = crate::test_negative_receipt::expect_ok(
        compose_attachment_turn(
            &scratch.0,
            &message("hello"),
            &[attachment(&path, "image/png")],
        ),
        "#1464 a '..' lease path must not fail the turn",
    );
    let blocks = content_blocks(&result);
    negative_assert!(
        diagnostics_block(&blocks).contains(&format!(
            "Image attachment not found: {path} (attachment path is outside the permitted \
             lease directory)."
        )),
        "#1464 a '..' lease path must soften to a not-found diagnostic, got: {}",
        diagnostics_block(&blocks)
    );
    negative_assert!(
        blocks
            .iter()
            .all(|block| block.get("type") != Some(&"image".into())),
        "#1464 a '..' lease path must never inline"
    );
    negative_assert!(
        result.get("allowedReadFiles").is_none(),
        "#1464 a '..' lease path must never register allowedReadFiles"
    );
    negative_assert!(
        note_block(&blocks).is_empty(),
        "#1464 a '..' lease path must never produce a note line"
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
    // read throws EISDIR uncaught, so the whole TS turn rejects. #1462: the
    // rejection message is Node's own descriptor-read spelling, without a
    // path — `fs.readFile(dir, {encoding:"utf8"})` on node v22.22.0 throws
    // exactly `EISDIR: illegal operation on a directory, read`
    // (`_tmp/20261106-issue-1458/node-probe/eisdir-probe.out`).
    let dir = scratch.lease("actually-a-dir.txt");
    std::fs::create_dir_all(&dir).expect("#1462 directory fixture");
    let error = crate::test_negative_receipt::expect_err(
        compose_attachment_turn(
            &scratch.0,
            &message("hello"),
            &[attachment(&dir, "text/plain")],
        ),
        "#1462 a directory read must reject the whole turn",
    );
    negative_assert_eq!(
        error.message.as_str(),
        "EISDIR: illegal operation on a directory, read",
        "#1462 a directory read failure must carry the Node EISDIR message"
    );
    negative_assert_eq!(
        error.code.as_str(),
        "gateway_submit_failed",
        "#1462 the read failure keeps the gateway submit error code"
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
    // the image-not-found diagnostic. #1461: the diagnostic keeps both TS
    // spellings — the outer path is the `path.resolve` form of the raw
    // UTF-16 (lone surrogate and absolute prefix included) and only the fs
    // error inside the parentheses carries the replacement character
    // (ts-probe `_tmp/20261108-issue-1460-r2/probe/composer-probe.out`
    // "Probe B"). It never reaches the lease guard, an open, or
    // allowedReadFiles.
    let mut fields = JsObject::new();
    fields.insert(
        "path".into(),
        JsValue::String(JsString::from_utf16(vec![0xD800])),
    );
    fields.insert("mimeType".into(), JsValue::from("image/png"));
    let result = compose_attachment_turn(&scratch.0, &message("hello"), &[JsValue::Object(fields)])
        .expect("#1321 a lone-surrogate path composes the turn");
    let blocks = content_blocks(&result);
    // The expected diagnostic in UTF-16 code units, so the lone surrogate
    // itself is pinned: `path.resolve("<cwd>/\u{D800}")` for the outer path
    // — the *process* cwd, which TS `path.resolve` uses as the base
    // (composer-probe.out "Probe B": the prefix is `/tmp/pdrs-1460-probe-cwd/`
    // while the project root is a different directory; the scratch project
    // root here differs from the test process cwd the same way, so a
    // project-root base would fail this expectation) — and the U+FFFD
    // spelling only inside the ENOENT message.
    let outer: Vec<u16> = std::env::current_dir()
        .expect("#1461 process cwd readable")
        .to_str()
        .expect("#1461 cwd UTF-8")
        .encode_utf16()
        .chain([u16::from(b'/'), 0xD800])
        .collect();
    let inner: Vec<u16> = String::from_utf16_lossy(&outer).encode_utf16().collect();
    let mut expected: Vec<u16> = "Image attachment not found: ".encode_utf16().collect();
    expected.extend_from_slice(&outer);
    expected.extend(" (ENOENT: no such file or directory, stat '".encode_utf16());
    expected.extend_from_slice(&inner);
    expected.extend("').".encode_utf16());
    negative_assert!(
        blocks
            .iter()
            .any(|block| units_contains(&block_text_units(block), &expected)),
        "#1461 the not-found diagnostic must keep the surrogate outer and the lossy inner spelling"
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
        &[attachment(&path, DOCX_MIME)],
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
fn unsupported_extension_warning_diagnosed() {
    let scratch = Scratch::new();
    // #1469 (resolver :149-163, :323-329): a file attachment outside the
    // text whitelist produces the `attachment_unsupported` warning —
    // warnings enter the diagnostics block regardless of registration
    // (composer :259-263) — and the guidance takes the diagnostics form.
    // All three are registered here so the warning-only filter cannot hide
    // behind the info gate.
    let bin_path = scratch.lease("data.bin");
    std::fs::write(&bin_path, b"\0binary").expect("#1469 bin fixture");
    let noext_path = scratch.lease("data");
    std::fs::write(&noext_path, b"no extension").expect("#1469 noext fixture");
    let ipynb_path = scratch.lease("notebook.ipynb");
    std::fs::write(&ipynb_path, b"{}").expect("#1469 ipynb fixture");
    let result = crate::test_negative_receipt::expect_ok(
        compose_attachment_turn(
            &scratch.0,
            &message("hello"),
            &[
                attachment(&bin_path, ""),
                attachment(&noext_path, ""),
                attachment(&ipynb_path, ""),
            ],
        ),
        "#1469 unsupported extensions must compose the turn",
    );
    let blocks = content_blocks(&result);
    let diagnostics = diagnostics_block(&blocks);
    negative_assert!(
        diagnostics.contains(
            "File extension .bin is not in the inline text whitelist; skipped. If this is \
             plain text, use read_file with the exact path; otherwise convert it before \
             inspection."
        ) && diagnostics.contains(
            "File extension (none) is not in the inline text whitelist; skipped. If this is \
             plain text, use read_file with the exact path; otherwise convert it before \
             inspection."
        ) && diagnostics.contains(
            "File extension .ipynb is not in the inline text whitelist; skipped. If this is \
             plain text, use read_file with the exact path; otherwise convert it before \
             inspection."
        ),
        "#1469 non-whitelisted extensions must carry the attachment_unsupported warning, \
         got: {diagnostics}"
    );
    negative_assert!(
        blocks
            .iter()
            .all(|block| !block_text(block).starts_with("<attachment path=")),
        "#1469 unsupported extensions must not be inlined"
    );
    let note = note_block(&blocks);
    negative_assert!(
        note.ends_with(
            "Some attachments were not shown inline. Use read_file with the exact path only \
             for readable text, image, PDF, or notebook attachments; Office/archive/binary \
             files need conversion before inspection."
        ),
        "#1469 the diagnostics guidance form must be selected, got: {note}"
    );
    let allowed = match result.get("allowedReadFiles") {
        Some(JsValue::Array(files)) => files.clone(),
        _ => Vec::new(),
    };
    negative_assert!(
        allowed.contains(&JsValue::from(bin_path.as_str()))
            && allowed.contains(&JsValue::from(noext_path.as_str()))
            && allowed.contains(&JsValue::from(ipynb_path.as_str())),
        "#1469 the registered unsupported attachments must register allowedReadFiles"
    );
}

#[test]
fn registered_unsupported_extension_keeps_its_warning_diagnostic() {
    let scratch = Scratch::new();
    // #1469: warnings enter the diagnostics block regardless of registration
    // (composer :259-263 — `Warning => true` in `diagnostic_enters`). A
    // channel-registered attachment with a non-whitelisted extension must
    // still carry the `attachment_unsupported` warning; gating the warning on
    // registration would drop it. Static-assertion copy of the killing half
    // of `unsupported_extension_warning_diagnosed` above (its assertions are
    // formatted, so the #946 receipt protocol cannot bind a row to them).
    let bin_path = scratch.lease("data.bin");
    std::fs::write(&bin_path, b"\0binary").expect("#1469 bin fixture");
    let result = crate::test_negative_receipt::expect_ok(
        compose_attachment_turn(&scratch.0, &message("hello"), &[attachment(&bin_path, "")]),
        "#1469 a registered unsupported extension must compose the turn",
    );
    let blocks = content_blocks(&result);
    negative_assert!(
        diagnostics_block(&blocks).contains(
            "File extension .bin is not in the inline text whitelist; skipped. If this is \
             plain text, use read_file with the exact path; otherwise convert it before \
             inspection."
        ),
        "#1469 a registered attachment with an unsupported extension must still carry its warning diagnostic"
    );
}

#[test]
fn unregistered_unsupported_extension_still_carries_the_warning() {
    let scratch = Scratch::new();
    // #1469: the `unsupported_file_message` push is the one step both
    // severities share — dropping it silences the diagnostic for registered
    // and uploaded attachments alike. This row's attachment is deliberately
    // unregistered (an uploaded lease bundle, no channelKey): the warning
    // filter passes it either way (`Warning => true` / `Warning =>
    // !registered` both admit an unregistered attachment), so only the
    // missing push can turn this assertion red.
    let bin_path = scratch.lease("memo.bin");
    std::fs::write(&bin_path, b"\0binary").expect("#1469 bin fixture");
    let result = crate::test_negative_receipt::expect_ok(
        compose_attachment_turn(
            &scratch.0,
            &message("hello"),
            &[upload_attachment(&bin_path, "")],
        ),
        "#1469 an unregistered unsupported extension must compose the turn",
    );
    let blocks = content_blocks(&result);
    negative_assert!(
        diagnostics_block(&blocks).contains(
            "File extension .bin is not in the inline text whitelist; skipped. If this is \
             plain text, use read_file with the exact path; otherwise convert it before \
             inspection."
        ),
        "#1469 the unsupported-file message must enter the diagnostics block"
    );
}

#[test]
fn binary_container_info_diagnostic_gating() {
    let scratch = Scratch::new();
    // #1469: a binary-container extension is info (resolver :158) — kept
    // for an unregistered upload, dropped for a registered attachment by
    // the composer filter (:259-263). Both spellings of the message use
    // the same `unsupportedFileMessage` form (resolver :326).
    let upload_path = scratch.lease("upload.zip");
    std::fs::write(&upload_path, b"PK\x03\x04zip").expect("#1469 upload zip fixture");
    let registered_path = scratch.lease("chan.zip");
    std::fs::write(&registered_path, b"PK\x03\x04zip").expect("#1469 registered zip fixture");
    let result = crate::test_negative_receipt::expect_ok(
        compose_attachment_turn(
            &scratch.0,
            &message("hello"),
            &[
                upload_attachment(&upload_path, "application/zip"),
                attachment(&registered_path, "application/zip"),
            ],
        ),
        "#1469 binary-container attachments must compose the turn",
    );
    let blocks = content_blocks(&result);
    let diagnostics = diagnostics_block(&blocks);
    negative_assert!(
        diagnostics.contains(&format!(
            "Attachment {upload_path} has Office/archive/binary extension .zip; it was \
             registered as a file path but not shown inline, and read_file cannot inspect \
             this format directly."
        )),
        "#1469 an unregistered binary-container attachment must keep its info diagnostic, \
         got: {diagnostics}"
    );
    negative_assert!(
        diagnostics
            .lines()
            .filter(|line| line.starts_with("- "))
            .count()
            == 1,
        "#1469 a registered binary-container attachment must drop its info diagnostic"
    );
    negative_assert!(
        blocks
            .iter()
            .all(|block| block.get("type") != Some(&"image".into()))
            && blocks
                .iter()
                .all(|block| !block_text(block).starts_with("<attachment path=")),
        "#1469 binary-container attachments must not be inlined"
    );
    let note = note_block(&blocks);
    negative_assert!(
        note.contains(&format!("- chan.zip: {registered_path}"))
            && !note.contains(upload_path.as_str()),
        "#1469 only the registered binary container keeps a note line"
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
fn missing_audio_is_skipped_without_diagnostic() {
    let scratch = Scratch::new();
    // Gate r6 boundary P1 (TS probe, `_tmp/20261105-issue-1321-merge/audio-probe`):
    // TS `collectRegisteredAttachmentReadFiles` stats audio and swallows a
    // failure (ENOENT -> allowedReadFiles empty, agentInput is the bare
    // message) while the resolver is never called. Rust must mirror that: a
    // missing audio path is skipped silently — no diagnostic, no turn failure.
    let missing = scratch.lease("gone.mp3");
    let result = compose_attachment_turn(
        &scratch.0,
        &message("hello"),
        &[attachment(&missing, "audio/mpeg")],
    )
    .expect("#1321 a missing audio path must not fail the turn");
    // No block, no note and no diagnostic leaves the plain text turn
    // (composer :67-72), exactly the TS probe's `{"type":"text","text":"hi"}`.
    negative_assert!(
        result.get("type") == Some(&"text".into()) && result.get("text") == Some(&message("hello")),
        "#1321 a missing audio must fall back to the plain text turn"
    );
    negative_assert!(
        result.get("content").is_none(),
        "#1321 a missing audio must not produce blocks (including diagnostics)"
    );
    negative_assert!(
        result.get("allowedReadFiles").is_none(),
        "#1321 a missing audio must not register allowedReadFiles"
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
        "#1321 duplicate normalized paths must produce exactly one note line"
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

/// fix-r4 A/B/C positive control: the identical fixture with no swap
/// installed passes the open-first revalidation unchanged — the original
/// lease bytes are still inlined and registered.
#[cfg(unix)]
#[test]
fn open_first_sequence_without_swap_keeps_inline_control() {
    let scratch = Scratch::new();
    let path = scratch.lease("target.png");
    std::fs::write(&path, b"lease-bytes").expect("#1321 lease control fixture");
    let result = compose_attachment_turn(
        &scratch.0,
        &message("hello"),
        &[attachment(&path, "image/png")],
    )
    .expect("#1321 the control composes the turn");
    let blocks = content_blocks(&result);
    negative_assert!(
        !diagnostics_block(&blocks).contains("not found"),
        "#1321 fix-r4 control: no swap must not produce a not-found diagnostic, got: {}",
        diagnostics_block(&blocks)
    );
    negative_assert!(
        blocks.iter().any(|block| {
            block.get("type") == Some(&"image".into())
                && block.get("data")
                    == Some(&JsValue::from(
                        BASE64_STANDARD.encode(b"lease-bytes").as_str(),
                    ))
        }),
        "#1321 fix-r4 control: no swap must still inline the original lease bytes"
    );
    negative_assert!(
        result.get("allowedReadFiles").is_some(),
        "#1321 fix-r4 control: no swap must still register allowedReadFiles"
    );
}

/// fix-r4 window 1: the leaf is swapped for an outside symlink between
/// `File::open` and the post-open canonicalize revalidation — step 2's
/// containment check must catch it: not-found, no read, no registration.
/// The fired flag is asserted so an unfired hook (a silent test) fails.
/// #1468 honesty note: this covers one swap in one window. A choreographed
/// sequence across all three windows defeats every path-based step (see
/// `multi_step_swap_across_all_windows_is_not_found`); the guarantee that
/// no outside bytes are inlined now rests on the handle binding, step 4.
#[cfg(unix)]
#[test]
fn leaf_swapped_between_open_and_revalidation_is_not_found() {
    use std::cell::Cell;
    use std::os::unix::fs::symlink;
    use std::rc::Rc;

    let scratch = Scratch::new();
    let path = scratch.lease("target.png");
    std::fs::write(&path, b"lease-bytes").expect("#1321 lease fixture");
    let outside_root = scratch.0.with_file_name(format!(
        "{}-outside-leaf",
        scratch
            .0
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("pdrs-1321")
    ));
    std::fs::create_dir_all(&outside_root).expect("#1321 outside leaf fixture dir");
    std::fs::write(outside_root.join("outside.png"), b"outside-bytes")
        .expect("#1321 outside leaf fixture");
    let fired = Rc::new(Cell::new(false));
    let hook_fired = Rc::clone(&fired);
    let leaf = path.clone();
    let outside_link = outside_root.join("outside.png");
    let _guard = swap_test_hook::set(swap_test_hook::SwapWindow::AfterOpen, move |_| {
        std::fs::remove_file(&leaf).expect("#1321 remove leaf inside window");
        symlink(&outside_link, &leaf).expect("#1321 swap leaf inside window");
        hook_fired.set(true);
    });
    let result = compose_attachment_turn(
        &scratch.0,
        &message("hello"),
        &[attachment(&path, "image/png")],
    )
    .expect("#1321 a mid-window leaf swap must not fail the turn");
    drop(_guard);
    negative_assert!(
        fired.get(),
        "#1321 fix-r4 window 1: the swap hook must fire inside the production sequence"
    );
    let blocks = content_blocks(&result);
    negative_assert!(
        diagnostics_block(&blocks).contains(&format!(
            "Image attachment not found: {path} (attachment resolved outside the lease root \
             after open)."
        )),
        "#1321 fix-r4: a leaf swapped after the open must be diagnosed not-found, got: {}",
        diagnostics_block(&blocks)
    );
    negative_assert!(
        blocks
            .iter()
            .all(|block| block.get("type") != Some(&"image".into())),
        "#1321 fix-r4: a leaf swapped after the open must never inline outside bytes"
    );
    negative_assert!(
        result.get("allowedReadFiles").is_none(),
        "#1321 fix-r4: a leaf swapped after the open must never register allowedReadFiles"
    );
    negative_assert!(
        note_block(&blocks).is_empty(),
        "#1321 fix-r4: a leaf swapped after the open must never produce a note line"
    );
    std::fs::remove_dir_all(&outside_root).ok();
}

/// fix-r4 window 1 (ancestor): the lease `u1` directory is swapped for an
/// outside symlink between the open and the revalidation — the canonical
/// spelling escapes the lease root, step 2 must treat it as not-found.
/// Same #1468 honesty note as the leaf variant above: one swap, one
/// window; the multi-step choreography belongs to the handle-binding test.
#[cfg(unix)]
#[test]
fn ancestor_swapped_between_open_and_revalidation_is_not_found() {
    use std::cell::Cell;
    use std::os::unix::fs::symlink;
    use std::rc::Rc;

    let scratch = Scratch::new();
    let path = scratch.lease("target.png");
    std::fs::write(&path, b"lease-bytes").expect("#1321 lease fixture");
    let outside_root = scratch.0.with_file_name(format!(
        "{}-outside-ancestor",
        scratch
            .0
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("pdrs-1321")
    ));
    std::fs::create_dir_all(&outside_root).expect("#1321 outside ancestor fixture");
    std::fs::write(outside_root.join("target.png"), b"outside-bytes")
        .expect("#1321 outside ancestor file");
    let fired = Rc::new(Cell::new(false));
    let hook_fired = Rc::clone(&fired);
    let u1 = scratch.0.join(".tmp").join("chat-upload-leases").join("u1");
    let outside_link = outside_root.clone();
    let _guard = swap_test_hook::set(swap_test_hook::SwapWindow::AfterOpen, move |_| {
        std::fs::remove_dir_all(&u1).expect("#1321 remove ancestor inside window");
        symlink(&outside_link, &u1).expect("#1321 swap ancestor inside window");
        hook_fired.set(true);
    });
    let result = compose_attachment_turn(
        &scratch.0,
        &message("hello"),
        &[attachment(&path, "image/png")],
    )
    .expect("#1321 a mid-window ancestor swap must not fail the turn");
    drop(_guard);
    negative_assert!(
        fired.get(),
        "#1321 fix-r4 window 1: the ancestor swap hook must fire inside the production sequence"
    );
    let blocks = content_blocks(&result);
    negative_assert!(
        diagnostics_block(&blocks).contains(&format!(
            "Image attachment not found: {path} (attachment resolved outside the lease root \
             after open)."
        )),
        "#1321 fix-r4: an ancestor swapped after the open must be diagnosed not-found, got: {}",
        diagnostics_block(&blocks)
    );
    negative_assert!(
        blocks
            .iter()
            .all(|block| block.get("type") != Some(&"image".into())),
        "#1321 fix-r4: an ancestor swapped after the open must never inline outside bytes"
    );
    negative_assert!(
        result.get("allowedReadFiles").is_none(),
        "#1321 fix-r4: an ancestor swapped after the open must never register allowedReadFiles"
    );
    std::fs::remove_dir_all(&outside_root).ok();
}

/// fix-r4 window 2: the leaf is swapped AFTER the revalidation — the
/// revalidated path stat and the opened handle's fstat disagree on
/// (dev, inode), step 3 must treat it as not-found. The same #1468
/// honesty note applies: (dev, inode) equality only holds for one swap
/// in one window; a sequence that restores the in-root file before the
/// stat and swaps again afterwards passes step 3 and is caught only by
/// the step-4 handle binding (`multi_step_swap_across_all_windows_is_not_found`).
#[cfg(unix)]
#[test]
fn leaf_swapped_after_revalidation_fails_identity() {
    use std::cell::Cell;
    use std::os::unix::fs::symlink;
    use std::rc::Rc;

    let scratch = Scratch::new();
    let path = scratch.lease("target.png");
    std::fs::write(&path, b"lease-bytes").expect("#1321 lease fixture");
    let outside_root = scratch.0.with_file_name(format!(
        "{}-outside-late",
        scratch
            .0
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("pdrs-1321")
    ));
    std::fs::create_dir_all(&outside_root).expect("#1321 outside late fixture dir");
    std::fs::write(outside_root.join("outside.png"), b"outside-bytes")
        .expect("#1321 outside late fixture");
    let fired = Rc::new(Cell::new(false));
    let hook_fired = Rc::clone(&fired);
    let leaf = path.clone();
    let outside_link = outside_root.join("outside.png");
    let _guard = swap_test_hook::set(swap_test_hook::SwapWindow::AfterRevalidate, move |_| {
        std::fs::remove_file(&leaf).expect("#1321 remove leaf after revalidation");
        symlink(&outside_link, &leaf).expect("#1321 swap leaf after revalidation");
        hook_fired.set(true);
    });
    let result = compose_attachment_turn(
        &scratch.0,
        &message("hello"),
        &[attachment(&path, "image/png")],
    )
    .expect("#1321 a post-revalidation leaf swap must not fail the turn");
    drop(_guard);
    negative_assert!(
        fired.get(),
        "#1321 fix-r4 window 2: the swap hook must fire inside the production sequence"
    );
    let blocks = content_blocks(&result);
    // #1468: this assertion is the negative-control receipt for the
    // (dev, inode) depth check — a leaf swapped after the revalidation must
    // stay not-found because the path stat and the handle's fstat disagree.
    negative_assert!(
        diagnostics_block(&blocks)
            .contains("(attachment resolved to a different file between revalidation and read)."),
        "#1468 the (dev, inode) depth check must stay"
    );
    negative_assert!(
        diagnostics_block(&blocks).contains(&format!(
            "Image attachment not found: {path} (attachment resolved to a different file \
             between revalidation and read)."
        )),
        "#1321 fix-r4: a leaf swapped after the revalidation must be diagnosed not-found, \
         got: {}",
        diagnostics_block(&blocks)
    );
    negative_assert!(
        blocks
            .iter()
            .all(|block| block.get("type") != Some(&"image".into())),
        "#1321 fix-r4: a leaf swapped after the revalidation must never inline outside bytes"
    );
    negative_assert!(
        result.get("allowedReadFiles").is_none(),
        "#1321 fix-r4: a leaf swapped after the revalidation must never register \
         allowedReadFiles"
    );
    negative_assert!(
        note_block(&blocks).is_empty(),
        "#1321 fix-r4: a leaf swapped after the revalidation must never produce a note line"
    );
    std::fs::remove_dir_all(&outside_root).ok();
}

/// #1468: the issue's three-swap sequence — the leaf is a real in-root file
/// while `lease_guard` canonicalizes it, is swapped for a hard link onto an
/// outside regular file before the open (so the open itself lands on the
/// outside inode; #2098's guarded open refuses a symlink at the open, so the
/// window swap-in is a hard link), is swapped back to a real in-root file
/// for the post-open revalidation, and is swapped onto the outside file
/// again for the path-stat/fstat identity comparison. Every path-based check
/// passes while the opened handle is the outside file: only binding the
/// approval to the handle's own resolved path (not any spelling checked
/// around it) rejects the read. All three windows must fire and the
/// attachment must land in the not-found branch — zero inlined bytes, zero
/// registration, no note line.
#[cfg(unix)]
#[test]
fn multi_step_swap_across_all_windows_is_not_found() {
    use std::cell::Cell;
    use std::rc::Rc;

    let scratch = Scratch::new();
    let path = scratch.lease("target.png");
    std::fs::write(&path, b"lease-bytes").expect("#1468 lease fixture");
    let outside_root = scratch.0.with_file_name(format!(
        "{}-outside-multi",
        scratch
            .0
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("pdrs-1468")
    ));
    std::fs::create_dir_all(&outside_root).expect("#1468 outside multi fixture dir");
    std::fs::write(outside_root.join("outside.png"), b"outside-bytes")
        .expect("#1468 outside multi fixture");
    let fired = Rc::new(Cell::new(0));
    let _before = swap_test_hook::set(swap_test_hook::SwapWindow::BeforeOpen, {
        let fired = Rc::clone(&fired);
        let leaf = path.clone();
        let outside_link = outside_root.join("outside.png");
        move |_| {
            std::fs::remove_file(&leaf).expect("#1468 remove the in-root leaf");
            std::fs::hard_link(&outside_link, &leaf)
                .expect("#1468 swap the leaf for a hard link onto the outside file");
            fired.set(fired.get() + 1);
        }
    });
    let _after = swap_test_hook::set(swap_test_hook::SwapWindow::AfterOpen, {
        let fired = Rc::clone(&fired);
        let leaf = path.clone();
        move |_| {
            std::fs::remove_file(&leaf).expect("#1468 remove the swapped link");
            std::fs::write(&leaf, b"lease-bytes").expect("#1468 restore the in-root leaf");
            fired.set(fired.get() + 1);
        }
    });
    let _revalidate = swap_test_hook::set(swap_test_hook::SwapWindow::AfterRevalidate, {
        let fired = Rc::clone(&fired);
        let leaf = path.clone();
        let outside_link = outside_root.join("outside.png");
        move |_| {
            std::fs::remove_file(&leaf).expect("#1468 remove the in-root leaf again");
            std::fs::hard_link(&outside_link, &leaf)
                .expect("#1468 swap the leaf for a hard link onto the outside file again");
            fired.set(fired.get() + 1);
        }
    });
    let result = compose_attachment_turn(
        &scratch.0,
        &message("hello"),
        &[attachment(&path, "image/png")],
    )
    .expect("#1468 a multi-step swap must not fail the turn");
    let blocks = content_blocks(&result);
    negative_assert!(
        fired.get() == 3,
        "#1468 all three swap hooks must fire inside the production sequence"
    );
    let outside_data = JsValue::from(BASE64_STANDARD.encode(b"outside-bytes").as_str());
    let leaked = blocks.iter().find(|block| {
        block.get("type") == Some(&"image".into()) && block.get("data") == Some(&outside_data)
    });
    if let Some(block) = leaked {
        eprintln!(
            "#1468 the outside bytes reached the inline pipeline: image block data={:?}",
            block.get("data")
        );
    }
    negative_assert!(
        leaked.is_none(),
        "#1468 a handle outside the lease root must never be read"
    );
    negative_assert!(
        diagnostics_block(&blocks).contains(&format!(
            "Image attachment not found: {path} (attachment handle is not the lease object \
             approved before the open)."
        )),
        "#1468 the multi-step swap must be diagnosed not-found"
    );
    negative_assert!(
        result.get("allowedReadFiles").is_none(),
        "#1468 the multi-step swap must never register allowedReadFiles"
    );
    negative_assert!(
        note_block(&blocks).is_empty(),
        "#1468 the multi-step swap must never produce a note line"
    );
    std::fs::remove_dir_all(&outside_root).ok();
}

/// #1468 (review concurrency#1 / boundary#1): the handle binding answers with
/// the object's *current* name, so a swap alone is not enough to pin it. Open
/// the outside file through a swapped-in hard link (#2098's guarded open
/// refuses a symlink at the open, so the window swap-in is a hard link onto
/// the outside regular file), then rename that outside file over the leaf:
/// the handle's name is now a genuine in-root spelling, the post-open
/// canonicalize passes, and the path stat and the handle agree on
/// `(dev, inode)` — only the identity recorded before the open (step 0)
/// still shows that the approved file and the opened file are different
/// objects. Without it the outside bytes are inlined (RED on the previous
/// head, where the multi-step test also passed).
#[cfg(unix)]
#[test]
fn rename_after_open_is_not_found() {
    use std::cell::Cell;
    use std::rc::Rc;

    let scratch = Scratch::new();
    let path = scratch.lease("target.png");
    std::fs::write(&path, b"lease-bytes").expect("#1468 lease fixture");
    let outside_root = scratch.0.with_file_name(format!(
        "{}-outside-rename",
        scratch
            .0
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("pdrs-1468")
    ));
    std::fs::create_dir_all(&outside_root).expect("#1468 outside rename fixture dir");
    let outside_file = outside_root.join("outside.png");
    std::fs::write(&outside_file, b"outside-bytes").expect("#1468 outside rename fixture");
    let fired = Rc::new(Cell::new(0));
    let _before = swap_test_hook::set(swap_test_hook::SwapWindow::BeforeOpen, {
        let fired = Rc::clone(&fired);
        let leaf = path.clone();
        let outside_file = outside_file.clone();
        move |_| {
            std::fs::remove_file(&leaf).expect("#1468 remove the in-root leaf");
            std::fs::hard_link(&outside_file, &leaf)
                .expect("#1468 swap the leaf for a hard link onto the outside file");
            fired.set(fired.get() + 1);
        }
    });
    let _after = swap_test_hook::set(swap_test_hook::SwapWindow::AfterOpen, {
        let fired = Rc::clone(&fired);
        let leaf = path.clone();
        move |_| {
            std::fs::rename(&outside_file, &leaf)
                .expect("#1468 rename the outside file over the leaf");
            fired.set(fired.get() + 1);
        }
    });
    let result = compose_attachment_turn(
        &scratch.0,
        &message("hello"),
        &[attachment(&path, "image/png")],
    )
    .expect("#1468 a post-open rename must not fail the turn");
    drop(_before);
    drop(_after);
    negative_assert!(
        fired.get() == 2,
        "#1468 both rename-sequence hooks must fire inside the production sequence"
    );
    let blocks = content_blocks(&result);
    negative_assert!(
        diagnostics_block(&blocks)
            .contains("(attachment handle is not the lease object approved before the open)."),
        "#1468 a renamed-in outside handle must be diagnosed not-found"
    );
    negative_assert!(
        blocks
            .iter()
            .all(|block| block.get("type") != Some(&"image".into())),
        "#1468 a renamed-in outside handle must never inline outside bytes"
    );
    negative_assert!(
        result.get("allowedReadFiles").is_none(),
        "#1468 a renamed-in outside handle must never register allowedReadFiles"
    );
    std::fs::remove_dir_all(&outside_root).ok();
}

/// #2098 (S3, FIFO T1): a FIFO swapped into the lease entry inside the
/// `BeforeOpen` window — the same swap shape as the #1468 sequence, aimed at
/// the composer's open. `symlink_metadata` only refuses a link, so the FIFO
/// passes the approved stat; the guarded open returns at once (`O_NONBLOCK`)
/// and the type term on the handle diagnoses it not-found: no read, no
/// registration, inside the watchdog. The TS baseline shape (coordinator
/// probe, node v22.22.0, 2026-11-11) blocks `readFile` past 3 s — and its
/// process exit with it — on the same swap; refusing at once is the
/// registered hardening (A7.2-ATTACH-HANDLE-BINDING-003).
///
/// The swap hook is thread-local, so the hook and `compose_attachment_turn`
/// run inside the watched thread and the watchdog is the main test thread.
/// Every bound assertion lives on the main thread: an unnamed worker's
/// panic writes no #946 receipt.
#[cfg(unix)]
#[test]
fn composer_lease_entry_swapped_for_a_fifo_is_diagnosed_not_found_within_the_watchdog() {
    let scratch = Scratch::new();
    let project_dir = scratch.0.clone();
    let leaf = scratch.lease("target.png");
    std::fs::write(&leaf, b"lease-bytes").expect("#2098 lease fixture");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _guard = swap_test_hook::set(swap_test_hook::SwapWindow::BeforeOpen, {
            let leaf = leaf.clone();
            move |_path: &Path| {
                std::fs::remove_file(&leaf)
                    .expect("#2098 remove the in-root leaf for the FIFO swap");
                let made = std::process::Command::new("mkfifo")
                    .arg(&leaf)
                    .status()
                    .expect("#2098 run mkfifo for the FIFO lease fixture");
                assert!(
                    made.success(),
                    "#2098 create the FIFO lease fixture for the composer watchdog"
                );
            }
        });
        let result = compose_attachment_turn(
            &project_dir,
            &message("hello"),
            &[attachment(&leaf, "image/png")],
        );
        let _ = tx.send(result.map(|turn| {
            let blocks = content_blocks(&turn);
            let inlined = BASE64_STANDARD.encode(b"lease-bytes");
            (
                diagnostics_block(&blocks).contains("Image attachment not found"),
                blocks.iter().any(|block| {
                    block.get("type") == Some(&"image".into())
                        && block.get("data") == Some(&JsValue::from(inlined.as_str()))
                }),
                turn.get("allowedReadFiles").is_some(),
            )
        }));
    });
    let verdict = rx.recv_timeout(std::time::Duration::from_secs(2));
    negative_assert!(
        matches!(verdict, Ok(Ok(_))),
        "#2098 a FIFO lease entry must never make the composer wait for a writer"
    );
    // Fail-closed fallthrough for a verdict the watchdog above already
    // refused: every subsequent assertion must fail, not silently pass.
    let (not_found, inlined, registered) = match verdict {
        Ok(Ok(summary)) => summary,
        _ => (false, true, true),
    };
    negative_assert!(
        not_found,
        "#2098 a FIFO lease entry must be diagnosed not-found"
    );
    negative_assert!(!inlined, "#2098 a FIFO lease entry must never be inlined");
    negative_assert!(
        !registered,
        "#2098 a FIFO lease entry must never register allowedReadFiles"
    );
}

/// #2098 (S3, symlink T1): the lease entry swapped for a symlink to a hard
/// link of the *same inode* placed inside the lease root. Every identity
/// term — the approved stat, the post-open path stat, the handle's fstat,
/// the handle's own resolved path — answers for that object, so only the
/// no-follow half of the guarded open can refuse the swapped link; a plain
/// byte-identical copy would be caught by the approved-identity term and
/// prove nothing about `O_NOFOLLOW` here.
#[cfg(unix)]
#[test]
fn composer_lease_entry_swapped_for_a_same_inode_symlink_is_diagnosed_not_found() {
    use std::cell::Cell;
    use std::rc::Rc;

    let scratch = Scratch::new();
    let path = scratch.lease("target.png");
    std::fs::write(&path, b"lease-bytes").expect("#2098 lease fixture");
    let alias = scratch.lease("alias-of-target.png");
    std::fs::hard_link(&path, &alias)
        .expect("#2098 hard-link the same inode inside the lease root");
    let fired = Rc::new(Cell::new(0));
    let _before = swap_test_hook::set(swap_test_hook::SwapWindow::BeforeOpen, {
        let fired = Rc::clone(&fired);
        let leaf = path.clone();
        let alias = alias.clone();
        move |_path: &Path| {
            std::fs::remove_file(&leaf).expect("#2098 remove the in-root leaf");
            std::os::unix::fs::symlink(&alias, &leaf)
                .expect("#2098 swap the leaf for a same-inode link");
            fired.set(fired.get() + 1);
        }
    });
    let result = compose_attachment_turn(
        &scratch.0,
        &message("hello"),
        &[attachment(&path, "image/png")],
    )
    .expect("#2098 a same-inode link swap must not fail the turn");
    drop(_before);
    negative_assert!(
        fired.get() == 1,
        "#2098 the swap hook must fire inside the before-open window"
    );
    let blocks = content_blocks(&result);
    negative_assert!(
        diagnostics_block(&blocks).contains("Image attachment not found"),
        "#2098 a same-inode symlink swapped for the lease entry must be diagnosed not-found"
    );
    let inlined = BASE64_STANDARD.encode(b"lease-bytes");
    negative_assert!(
        blocks.iter().all(|block| {
            !(block.get("type") == Some(&"image".into())
                && block.get("data") == Some(&JsValue::from(inlined.as_str())))
        }),
        "#2098 a same-inode symlink swapped for the lease entry must never be followed"
    );
    negative_assert!(
        result.get("allowedReadFiles").is_none(),
        "#2098 a same-inode symlink swapped for the lease entry must never register \
         allowedReadFiles"
    );
}

/// #1468 (review deepseek boundary#2 / concurrency#2): the upload-store root
/// is the second legitimate home of a lease object, and it must get the same
/// containment guard as the lease root. `.tmp/chat-uploads` swapped for a
/// symlink out of the project would otherwise turn the outside directory the
/// link points at into an accepted home, and the very multi-swap sequence this
/// PR closes starts passing again. The window swap-ins of that sequence are
/// hard links onto the outside regular file (#2098's guarded open refuses a
/// symlink at the open, so a hard link keeps the open landing on the outside
/// inode); the store-root symlink is this test's subject and stays.
#[cfg(unix)]
#[test]
fn upload_store_root_swapped_outside_project_is_not_a_second_home() {
    use std::cell::Cell;
    use std::os::unix::fs::symlink;
    use std::rc::Rc;

    let scratch = Scratch::new();
    let path = scratch.lease("target.png");
    std::fs::write(&path, b"lease-bytes").expect("#1468 lease fixture");
    let outside_root = scratch.0.with_file_name(format!(
        "{}-outside-store",
        scratch
            .0
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("pdrs-1468")
    ));
    std::fs::create_dir_all(&outside_root).expect("#1468 outside store fixture dir");
    let outside_file = outside_root.join("outside.png");
    std::fs::write(&outside_file, b"outside-bytes").expect("#1468 outside store fixture");
    // The store root itself now points outside the project.
    let store_root = scratch.0.join(".tmp").join("chat-uploads");
    symlink(&outside_root, &store_root).expect("#1468 swap the store root outside the project");
    let fired = Rc::new(Cell::new(0));
    let _before = swap_test_hook::set(swap_test_hook::SwapWindow::BeforeOpen, {
        let fired = Rc::clone(&fired);
        let leaf = path.clone();
        let outside_file = outside_file.clone();
        move |_| {
            std::fs::remove_file(&leaf).expect("#1468 remove the in-root leaf");
            std::fs::hard_link(&outside_file, &leaf)
                .expect("#1468 swap the leaf for a hard link onto the outside file");
            fired.set(fired.get() + 1);
        }
    });
    let _after = swap_test_hook::set(swap_test_hook::SwapWindow::AfterOpen, {
        let fired = Rc::clone(&fired);
        let leaf = path.clone();
        move |_| {
            std::fs::remove_file(&leaf).expect("#1468 remove the swapped link");
            std::fs::write(&leaf, b"lease-bytes").expect("#1468 restore the in-root leaf");
            fired.set(fired.get() + 1);
        }
    });
    // Third swap, exactly as in the multi-step sequence: the path stat must
    // agree with the outside handle so that only the store-root guard stands
    // between the outside directory and an accepted second home.
    let _revalidate = swap_test_hook::set(swap_test_hook::SwapWindow::AfterRevalidate, {
        let fired = Rc::clone(&fired);
        let leaf = path.clone();
        let outside_file = outside_file.clone();
        move |_| {
            std::fs::remove_file(&leaf).expect("#1468 remove the in-root leaf again");
            std::fs::hard_link(&outside_file, &leaf)
                .expect("#1468 swap the leaf for a hard link onto the outside file again");
            fired.set(fired.get() + 1);
        }
    });
    let result = compose_attachment_turn(
        &scratch.0,
        &message("hello"),
        &[attachment(&path, "image/png")],
    )
    .expect("#1468 a swapped store root must not fail the turn");
    drop(_before);
    drop(_after);
    drop(_revalidate);
    negative_assert!(
        fired.get() == 3,
        "#1468 all three store-root swap hooks must fire inside the production sequence"
    );
    let blocks = content_blocks(&result);
    negative_assert!(
        blocks
            .iter()
            .all(|block| block.get("type") != Some(&"image".into())),
        "#1468 a store root outside the project must not become a second accepted home"
    );
    negative_assert!(
        result.get("allowedReadFiles").is_none(),
        "#1468 a store root outside the project must never register allowedReadFiles"
    );
    std::fs::remove_file(&store_root).ok();
    std::fs::remove_dir_all(&outside_root).ok();
}

/// #1468 (review deepseek sibling#1 / concurrency#3, minimax boundary#2 /
/// concurrency#2): the upload-store root is a second accepted home, so its
/// containment guard must be exercisable on its own. In the end-to-end
/// sequence an outside store root also trips the identity term first, so that
/// test cannot tell whether this guard exists; `canonical_upload_store` is the
/// whole decision, and the control below pins that it is not "always None".
#[cfg(unix)]
#[test]
fn upload_store_root_outside_the_project_is_not_a_home() {
    use std::os::unix::fs::symlink;
    let scratch = Scratch::new();
    let project = scratch.0.clone();
    let store = project.join(".tmp").join("chat-uploads");
    let outside = project.with_file_name(format!(
        "{}-outside-store-home",
        project
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("pdrs-1468")
    ));
    std::fs::create_dir_all(&outside).expect("#1468 outside store home fixture");
    std::fs::create_dir_all(project.join(".tmp")).expect("#1468 scratch .tmp");
    symlink(&outside, &store).expect("#1468 swap the store root outside the project");
    negative_assert!(
        canonical_upload_store(&project).is_none(),
        "#1468 a store root outside the project must not become a home"
    );
    std::fs::remove_file(&store).expect("#1468 remove the swapped store link");
    // Review boundary P2: a store root that is itself a link is refused even
    // when it points *inside* the project — otherwise the whole project (or
    // any subtree) becomes an accepted home while `read_at`'s lexical prefix
    // check rejects the same layout.
    symlink(&project, &store).expect("#1468 swap the store root for the project root");
    negative_assert!(
        canonical_upload_store(&project).is_none(),
        "#1468 a store root that is a link into the project must not become a home"
    );
    std::fs::remove_file(&store).expect("#1468 remove the in-project store link");
    // An ancestor (`.tmp`) that is a link points the canonical spelling
    // elsewhere; only the containment term sees it. The leaf is a real
    // directory, so the link check above cannot answer for it.
    std::fs::remove_dir_all(project.join(".tmp")).expect("#1468 drop scratch .tmp");
    let outside_tmp = outside.join("tmp");
    std::fs::create_dir_all(outside_tmp.join("chat-uploads"))
        .expect("#1468 outside ancestor store fixture");
    symlink(&outside_tmp, project.join(".tmp")).expect("#1468 swap .tmp outside the project");
    negative_assert!(
        canonical_upload_store(&project).is_none(),
        "#1468 a store root under an outside ancestor must not become a home"
    );
    std::fs::remove_file(project.join(".tmp")).expect("#1468 remove the swapped .tmp link");
    std::fs::create_dir_all(&store).expect("#1468 real in-project store root");
    negative_assert!(
        canonical_upload_store(&project).is_some(),
        "#1468 an in-project store root must stay a home"
    );
    std::fs::remove_dir_all(&outside).ok();
}

/// #1887 (review boundary P1): the per-component walk and the
/// canonicalization are two syscalls. A `.tmp` swapped for an **outside** link
/// inside that gap resolves outside the project and is refused by the
/// containment term, not by the walk — the walk already saw a real directory.
/// This is the deterministic proof that the containment term is load-bearing:
/// the `upload-store-containment-dropped` mutation row drops it, this root is
/// then accepted, and the assertion below goes red.
#[cfg(unix)]
#[test]
fn store_root_swapped_outside_after_the_walk_is_not_a_home() {
    use std::os::unix::fs::symlink;
    let scratch = Scratch::new();
    let project = scratch.0.clone();
    let store = project.join(".tmp").join("chat-uploads");
    std::fs::create_dir_all(&store).expect("#1887 store home fixture");
    let outside = project.with_file_name(format!(
        "{}-outside-swapped-store-home",
        project
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("pdrs-1887")
    ));
    std::fs::create_dir_all(outside.join("tmp").join("chat-uploads"))
        .expect("#1887 outside swapped store fixture");
    let project_for_hook = project.clone();
    let outside_for_hook = outside.clone();
    let guard = crate::upload::swap_test_hook::set(
        crate::upload::swap_test_hook::SwapWindow::BetweenWalkAndCanonicalize,
        move |_walked| {
            std::fs::rename(
                project_for_hook.join(".tmp"),
                project_for_hook.join(".tmp-moved"),
            )
            .expect("#1887 move the real .tmp aside");
            symlink(outside_for_hook.join("tmp"), project_for_hook.join(".tmp"))
                .expect("#1887 swap .tmp for an outside link inside the walk window");
        },
    );
    negative_assert!(
        crate::upload::canonical_home(&store, &project, &project).is_none(),
        "#1887 a store root swapped outside after the walk must not become a home"
    );
    drop(guard);
    std::fs::remove_dir_all(&outside).ok();
}

/// #1468 (review GLM sibling#1): the lease root is the other legitimate home
/// and had drifted from the store guard — a `.tmp/chat-upload-leases` that is
/// a link into the project keeps its lexical spelling but makes every path
/// under the link target a lease home. The target here is inside the project,
/// so the containment term alone accepts it: only the leaf-link term in
/// `canonical_home` keeps the composer from inlining and registering it.
/// End-to-end on purpose — a unit test of the helper would not notice the
/// composer going back to a bare `canonicalize` (mutation row
/// `lease-root-link-accepted` covers exactly that call site).
#[cfg(unix)]
#[test]
fn lease_root_link_is_not_a_home() {
    use std::os::unix::fs::symlink;
    let scratch = Scratch::new();
    let lease_root = scratch.0.join(".tmp").join("chat-upload-leases");
    let elsewhere = scratch.0.join("elsewhere");
    std::fs::create_dir_all(elsewhere.join("u1")).expect("#1468 in-project link target");
    std::fs::write(elsewhere.join("u1").join("linked.png"), [1u8, 2, 3, 4])
        .expect("#1468 link target file");
    // `Scratch` pre-creates the real lease root, so drop it before swapping in
    // a link at the same spelling.
    std::fs::remove_dir_all(&lease_root).expect("#1468 drop the scratch lease root");
    symlink(&elsewhere, &lease_root).expect("#1468 swap the lease root for an in-project link");
    let path = scratch.lease("linked.png");
    let result = crate::test_negative_receipt::expect_ok(
        compose_attachment_turn(
            &scratch.0,
            &message("hello"),
            &[attachment(&path, "image/png")],
        ),
        "#1468 a lease root that is a link into the project must not fail the turn",
    );
    let blocks = content_blocks(&result);
    let diagnostics = diagnostics_block(&blocks);
    negative_assert!(
        diagnostics.contains(&format!(
            "Image attachment not found: {path} (attachment lease root is unavailable)."
        )),
        "#1468 a lease root that is a link into the project must not become a home"
    );
    negative_assert!(
        blocks
            .iter()
            .all(|block| block.get("type") != Some(&"image".into())),
        "#1468 a lease root that is a link into the project must never inline"
    );
    negative_assert!(
        result.get("allowedReadFiles").is_none(),
        "#1468 a lease root that is a link into the project must never register allowedReadFiles"
    );
}

/// #1887: the ancestor term. A `.tmp` swapped for a link to another subtree of
/// the *same* project keeps the canonical spelling inside `project_canon`, so
/// the containment term alone accepts it and `src/...` becomes a lease home —
/// while `read_at`'s lexical prefix check refuses the same layout. Both sides
/// now walk every component between the project root and the home, so this
/// turn takes the same not-found branch as a missing lease root.
#[cfg(unix)]
#[test]
fn lease_root_under_a_linked_tmp_ancestor_is_not_a_home() {
    use std::os::unix::fs::symlink;
    let scratch = Scratch::new();
    let target = scratch.0.join("src");
    std::fs::create_dir_all(target.join("chat-upload-leases").join("u1"))
        .expect("#1887 in-project ancestor link target");
    std::fs::write(
        target
            .join("chat-upload-leases")
            .join("u1")
            .join("linked.png"),
        [1u8, 2, 3, 4],
    )
    .expect("#1887 ancestor link target file");
    // `Scratch` pre-creates the real `.tmp` tree; drop it so `.tmp` itself can
    // become the link (the leaf below it stays a real directory).
    std::fs::remove_dir_all(scratch.0.join(".tmp")).expect("#1887 drop the scratch .tmp tree");
    symlink(&target, scratch.0.join(".tmp")).expect("#1887 swap .tmp for an in-project link");
    let path = scratch.lease("linked.png");
    let result = crate::test_negative_receipt::expect_ok(
        compose_attachment_turn(
            &scratch.0,
            &message("hello"),
            &[attachment(&path, "image/png")],
        ),
        "#1887 a .tmp ancestor that is a link into the project must not fail the turn",
    );
    let blocks = content_blocks(&result);
    let diagnostics = diagnostics_block(&blocks);
    negative_assert!(
        diagnostics.contains(&format!(
            "Image attachment not found: {path} (attachment lease root is unavailable)."
        )),
        "#1887 a .tmp ancestor that is a link into the project must not become a home"
    );
    negative_assert!(
        blocks
            .iter()
            .all(|block| block.get("type") != Some(&"image".into())),
        "#1887 a .tmp ancestor that is a link into the project must never inline"
    );
    negative_assert!(
        result.get("allowedReadFiles").is_none(),
        "#1887 a .tmp ancestor that is a link into the project must never register allowedReadFiles"
    );
}

/// #1468 (review deepseek boundary#1 / concurrency#1,#2): the identity the
/// open is bound to must be the object the containment check validated, not
/// whatever the path points at after that check. A leaf swapped for an
/// outside symlink in the `[canonicalize, approved stat]` window is a swap;
/// `symlink_metadata` does not follow it, so the turn takes the not-found
/// branch. The `AfterOpen` rename gives the outside inode an in-root name —
/// under the pre-fix `fs::metadata` the approved object *is* the outside
/// inode and the rename lands it inside the lease root with every check
/// passing, so only the no-follow sample keeps the bytes out.
#[cfg(unix)]
#[test]
fn leaf_swapped_before_approved_stat_is_not_found() {
    use std::cell::Cell;
    use std::os::unix::fs::symlink;
    use std::rc::Rc;

    let scratch = Scratch::new();
    let path = scratch.lease("target.png");
    std::fs::write(&path, b"lease-bytes").expect("#1468 lease fixture");
    let outside_root = scratch.0.with_file_name(format!(
        "{}-outside-preapproved",
        scratch
            .0
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("pdrs-1468")
    ));
    std::fs::create_dir_all(&outside_root).expect("#1468 outside pre-approval dir");
    let outside_file = outside_root.join("outside.png");
    std::fs::write(&outside_file, b"outside-bytes").expect("#1468 outside pre-approval fixture");
    let fired = Rc::new(Cell::new(0));
    let _swap = swap_test_hook::set(swap_test_hook::SwapWindow::BeforeApprovedStat, {
        let fired = Rc::clone(&fired);
        let leaf = PathBuf::from(&path);
        let outside_file = outside_file.clone();
        move |_| {
            std::fs::remove_file(&leaf).expect("#1468 remove the in-root leaf");
            symlink(&outside_file, &leaf).expect("#1468 swap the leaf before the approved stat");
            fired.set(fired.get() + 1);
        }
    });
    let _rename = swap_test_hook::set(swap_test_hook::SwapWindow::AfterOpen, {
        let leaf = PathBuf::from(&path);
        let outside_file = outside_file.clone();
        move |_| {
            std::fs::rename(&outside_file, &leaf)
                .expect("#1468 rename the outside file over the leaf");
        }
    });
    let result = compose_attachment_turn(
        &scratch.0,
        &message("hello"),
        &[attachment(&path, "image/png")],
    )
    .expect("#1468 a pre-approval swap must not fail the turn");
    drop(_swap);
    drop(_rename);
    negative_assert!(
        fired.get() == 1,
        "#1468 the pre-approval swap hook must fire inside the production sequence"
    );
    let blocks = content_blocks(&result);
    negative_assert!(
        blocks
            .iter()
            .all(|block| block.get("type") != Some(&"image".into())),
        "#1468 a link planted before the approved stat must never inline outside bytes"
    );
    negative_assert!(
        result.get("allowedReadFiles").is_none(),
        "#1468 a link planted before the approved stat must never register allowedReadFiles"
    );
    negative_assert!(
        diagnostics_block(&blocks).contains("became a symlink after validation"),
        "#1468 a link planted before the approved stat must be diagnosed as a swap"
    );
    std::fs::remove_dir_all(&outside_root).ok();
}

/// #1468: the handle-binding decision must fail closed. A handle whose own
/// path the platform cannot produce (a pipe has no filesystem path: macOS
/// F_GETPATH answers EBADF, Linux /proc answers `pipe:[…]` and handle_path
/// rejects it) is never approved — a swapped symlink whose trick is to make
/// the handle unresolvable must not turn that into a pass.
#[cfg(unix)]
#[test]
fn handle_path_unavailable_fails_closed() {
    let scratch = Scratch::new();
    let lease_root = scratch.0.join(".tmp").join("chat-upload-leases");
    let mut child = std::process::Command::new("/bin/cat")
        .stdin(std::process::Stdio::piped())
        .spawn()
        .expect("#1468 spawn the pipe fixture");
    let stdin = child.stdin.take().expect("#1468 take the pipe write end");
    let pipe_file = std::fs::File::from(std::os::fd::OwnedFd::from(stdin));
    let approved_path = scratch.lease("approved.txt");
    std::fs::write(&approved_path, b"approved").expect("#1468 approved fixture");
    let approved = std::fs::metadata(&approved_path).expect("#1468 approved stat");
    negative_assert!(
        !handle_bound_to_upload_areas(&pipe_file, &lease_root, None, &approved),
        "#1468 an unavailable handle path must fail closed"
    );
    drop(pipe_file);
    let _ = child.wait();
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
fn direct_content_with_channel_key_path_is_registered() {
    let scratch = Scratch::new();
    // #1460 (ts-probe `_tmp/20261108-issue-1460-r2/probe/composer-probe.out`
    // "Probe A"): a direct-content attachment with a valid path and a
    // channelKey gets both TS passes — the image block from the content
    // branch (composer :211-221) and the registration collection the
    // content `continue` never skips (composer :57/:173-193): the path note
    // and allowedReadFiles carry the registered path.
    let path = scratch.lease("direct.png");
    std::fs::write(&path, [1u8, 2, 3, 4]).expect("#1460 lease fixture");
    let entry = object([
        ("type", JsValue::from("image")),
        (
            "content",
            JsValue::from(BASE64_STANDARD.encode("ABCD").as_str()),
        ),
        ("mimeType", JsValue::from("image/png")),
        ("bytes", JsValue::Number(4.0)),
        ("path", JsValue::from(path.as_str())),
        (
            "metadata",
            object([("channelKey", JsValue::from("chan-1"))]),
        ),
    ]);
    let result = compose_attachment_turn(&scratch.0, &message("hello"), &[entry])
        .expect("#1460 direct content with a channelKey path composes the turn");
    let blocks = content_blocks(&result);
    negative_assert!(
        blocks.len() == 3,
        "#1460 direct content with a channelKey path must keep message, image and note blocks"
    );
    negative_assert!(
        blocks[1].get("type") == Some(&"image".into())
            && blocks[1].get("data")
                == Some(&JsValue::from(BASE64_STANDARD.encode("ABCD").as_str()))
            && blocks[1].get("bytes") == Some(&JsValue::Number(4.0)),
        "#1460 the content image block must keep the direct-content shape"
    );
    let note = block_text(&blocks[2]);
    negative_assert!(
        note == format!(
            "\n\n[Registered attachment files in this session:]\n- direct.png: {path}\n\
             These are path references for reuse. If an image/PDF is already visible in this \
             turn, do not call read_file just to view it."
        ),
        "#1460 the note must carry the registered path, got: {note}"
    );
    let allowed = match result.get("allowedReadFiles") {
        Some(JsValue::Array(files)) => files.clone(),
        _ => Vec::new(),
    };
    negative_assert!(
        allowed == vec![JsValue::from(path.as_str())],
        "#1460 allowedReadFiles must carry the registered path"
    );
    negative_assert!(
        diagnostics_block(&blocks).is_empty(),
        "#1460 a valid direct-content path must not diagnose"
    );
}

#[test]
fn direct_content_with_an_invalid_channel_key_path_stays_silent() {
    let scratch = Scratch::new();
    // #1460: the registration collection swallows its own failures
    // (composer :181-189 `catch {}`) and the content `continue` keeps the
    // entry out of the resolver's diagnostics — an unresolvable or
    // lone-surrogate path on a content attachment keeps its block and
    // produces neither a diagnostic, nor a note line, nor allowedReadFiles.
    let outside = scratch.0.with_file_name("pdrs-1460-outside-content.png");
    let outside_path = outside.to_str().expect("#1460 outside UTF-8").to_owned();
    let mut surrogate = JsObject::new();
    surrogate.insert("type".into(), JsValue::from("image"));
    surrogate.insert(
        "content".into(),
        JsValue::from(BASE64_STANDARD.encode("AB").as_str()),
    );
    surrogate.insert("mimeType".into(), JsValue::from("image/png"));
    surrogate.insert(
        "path".into(),
        JsValue::String(JsString::from_utf16(vec![0xD800])),
    );
    surrogate.insert(
        "metadata".into(),
        object([("channelKey", JsValue::from("chan-1"))]),
    );
    let outside_entry = object([
        ("type", JsValue::from("image")),
        (
            "content",
            JsValue::from(BASE64_STANDARD.encode("CD").as_str()),
        ),
        ("mimeType", JsValue::from("image/png")),
        ("path", JsValue::from(outside_path.as_str())),
        (
            "metadata",
            object([("channelKey", JsValue::from("chan-1"))]),
        ),
    ]);
    let result = compose_attachment_turn(
        &scratch.0,
        &message("hello"),
        &[JsValue::Object(surrogate), outside_entry],
    )
    .expect("#1460 invalid paths on content attachments compose the turn");
    let blocks = content_blocks(&result);
    negative_assert!(
        blocks.len() == 3,
        "#1460 content attachments with invalid paths must keep message and image blocks only"
    );
    negative_assert!(
        diagnostics_block(&blocks).is_empty(),
        "#1460 an invalid path on a content attachment must not diagnose"
    );
    negative_assert!(
        result.get("allowedReadFiles").is_none(),
        "#1460 an invalid path on a content attachment must not register allowedReadFiles"
    );
    negative_assert!(
        note_block(&blocks).is_empty(),
        "#1460 an invalid path on a content attachment must not produce a note line"
    );
}

#[cfg(unix)]
#[test]
fn direct_content_with_an_eloop_path_stays_silent() {
    use std::os::unix::fs::symlink;
    let scratch = Scratch::new();
    // #1460/#1629: an ELOOP canonicalize failure of the path itself under
    // a valid root (#1629: no longer a P2 internal error — it softens
    // inside `lease_guard` like every other unresolvable path) is swallowed
    // for a content-produced attachment exactly like the registration
    // pass's own `catch {}` swallows its stat error — the turn must not
    // fail over a path the resolver projection never sees.
    let link = scratch.lease("loop.png");
    symlink(Path::new(&link), Path::new(&link)).expect("#1460 symlink loop fixture");
    let entry = object([
        ("type", JsValue::from("image")),
        (
            "content",
            JsValue::from(BASE64_STANDARD.encode("AB").as_str()),
        ),
        ("mimeType", JsValue::from("image/png")),
        ("path", JsValue::from(link.as_str())),
        (
            "metadata",
            object([("channelKey", JsValue::from("chan-1"))]),
        ),
    ]);
    let result = crate::test_negative_receipt::expect_ok(
        compose_attachment_turn(&scratch.0, &message("hello"), &[entry]),
        "#1460 an ELOOP path on a content attachment must not fail the turn",
    );
    let blocks = content_blocks(&result);
    negative_assert!(
        blocks.len() == 2,
        "#1460 an ELOOP content attachment must keep message and image blocks only"
    );
    negative_assert!(
        diagnostics_block(&blocks).is_empty(),
        "#1460 an ELOOP path on a content attachment must not diagnose"
    );
    negative_assert!(
        result.get("allowedReadFiles").is_none(),
        "#1460 an ELOOP path on a content attachment must not register allowedReadFiles"
    );
}

#[test]
fn folded_surrogate_path_routes_the_diagnostic_on_the_raw_spelling() {
    let scratch = Scratch::new();
    // DS r1 boundary P2: composer :234-251 routes the not-found words on the
    // *original* attachment path. `report.pdf/\uD800/..` resolves to a
    // `.pdf` tail, so routing on the resolved form would say "PDF
    // attachment not found"; the raw spelling ends in `..` and routes as
    // File — "Attachment not found" — while the resolved outer path (with
    // its process-cwd prefix) is what the diagnostic carries.
    let raw: Vec<u16> = "report.pdf/"
        .encode_utf16()
        .chain([0xD800])
        .chain("/..".encode_utf16())
        .collect();
    let mut fields = JsObject::new();
    fields.insert("path".into(), JsValue::String(JsString::from_utf16(raw)));
    fields.insert("mimeType".into(), JsValue::from("text/plain"));
    let result = compose_attachment_turn(&scratch.0, &message("hello"), &[JsValue::Object(fields)])
        .expect("#1461 a folded surrogate path composes the turn");
    let blocks = content_blocks(&result);
    let outer: Vec<u16> = std::env::current_dir()
        .expect("#1461 process cwd readable")
        .to_str()
        .expect("#1461 cwd UTF-8")
        .encode_utf16()
        .chain("/report.pdf".encode_utf16())
        .collect();
    let inner: Vec<u16> = String::from_utf16_lossy(&outer).encode_utf16().collect();
    let mut expected: Vec<u16> = "Attachment not found: ".encode_utf16().collect();
    expected.extend_from_slice(&outer);
    expected.extend(" (ENOENT: no such file or directory, stat '".encode_utf16());
    expected.extend_from_slice(&inner);
    expected.extend("').".encode_utf16());
    negative_assert!(
        blocks
            .iter()
            .any(|block| units_contains(&block_text_units(block), &expected)),
        "#1461 the not-found words route on the raw spelling, not the resolved tail"
    );
}

// #1461 r6 (DS r5 boundary P2 / sibling P3): the worker this launches is
// #[cfg(unix)]; without the same gate a non-unix build would spawn a test
// name that does not exist there.
#[cfg(unix)]
#[test]
fn absolute_surrogate_diagnostic_survives_deleted_cwd_matrix() {
    crate::test_process::run_cases(
        "native_process::services_tests::absolute_surrogate_diagnostic_survives_deleted_cwd",
        vec![("absolute-deleted-cwd".into(), vec![])],
    );
}

/// #1461 r2 (DS concurrency P1 / boundary P2): an absolute lone-surrogate
/// path never consults the process cwd — Node `path.resolve` only reads it
/// for relative spellings (the same shape as `skills::resolve`) — so the
/// not-found diagnostic must survive a cwd that has been deleted.
/// Launched in an isolated worker because deleting the cwd is
/// process-global (`matrix` above spawns this test with `--exact`).
#[cfg(unix)]
#[test]
#[ignore = "isolated cwd worker launched by absolute_surrogate_diagnostic_survives_deleted_cwd_matrix"]
fn absolute_surrogate_diagnostic_survives_deleted_cwd() {
    let holder = Scratch::new();
    // A doomed cwd must be an empty directory: remove_dir(2) refuses a
    // non-empty one, and a Scratch carries the lease subtree inside, so the
    // doomed cwd is a bare temp dir with nothing in it (cb8-measured: the
    // Scratch shape failed with DirectoryNotEmpty before the product path
    // ever ran).
    let gone = std::env::temp_dir().join(format!("pdrs-1461-gone-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&gone).expect("#1461 make the doomed cwd");
    std::env::set_current_dir(&gone).expect("#1461 enter the doomed cwd");
    std::fs::remove_dir(&gone).expect("#1461 delete the cwd");
    let mut raw: Vec<u16> = holder
        .0
        .to_str()
        .expect("#1461 scratch path UTF-8")
        .encode_utf16()
        .collect();
    raw.push(b'/' as u16);
    raw.push(0xD800);
    let mut fields = JsObject::new();
    fields.insert("path".into(), JsValue::String(JsString::from_utf16(raw)));
    fields.insert("mimeType".into(), JsValue::from("image/png"));
    let result = compose_attachment_turn(&holder.0, &message("hello"), &[JsValue::Object(fields)])
        .expect("#1461 an absolute lone-surrogate path composes the turn with a deleted cwd");
    let blocks = content_blocks(&result);
    // The outer is the absolute spelling itself — the resolve never read
    // the (deleted) cwd — and only the fs error inside the parentheses is
    // lossy, exactly like Probe B's relative shape.
    let outer: Vec<u16> = holder
        .0
        .to_str()
        .expect("#1461 scratch path UTF-8")
        .encode_utf16()
        .chain([b'/' as u16, 0xD800])
        .collect();
    let inner: Vec<u16> = String::from_utf16_lossy(&outer).encode_utf16().collect();
    let mut expected: Vec<u16> = "Image attachment not found: ".encode_utf16().collect();
    expected.extend_from_slice(&outer);
    expected.extend(" (ENOENT: no such file or directory, stat '".encode_utf16());
    expected.extend_from_slice(&inner);
    expected.extend("').".encode_utf16());
    negative_assert!(
        blocks
            .iter()
            .any(|block| units_contains(&block_text_units(block), &expected)),
        "#1461 an absolute surrogate diagnostic must survive a deleted cwd without reading it"
    );
    negative_assert!(
        result.get("allowedReadFiles").is_none(),
        "#1461 a lone-surrogate path must not register allowedReadFiles"
    );
    std::env::set_current_dir(&holder.0).expect("#1461 restore known cwd");
    crate::test_process::receipt("absolute-deleted-cwd");
}

/// #1461 r5 (DS sibling P2 / boundary P2): a *relative* lone-surrogate
/// spelling consults the process cwd, and the cache must behave like Node's
/// `process.cwd()` — seeded by the first successful read, still answering
/// after the cwd itself is deleted. The second relative diagnostic after
/// the deletion must still carry the warm prefix; the lazy first-read
/// residual (a cwd deleted *before* the first relative diagnostic stays
/// silent) is documented on `cached_process_cwd`. Same worker shape as the
/// absolute case above: deleting the cwd is process-global.
#[cfg(unix)]
#[test]
#[ignore = "isolated cwd worker launched by relative_surrogate_diagnostic_uses_the_cached_first_cwd_matrix"]
fn relative_surrogate_diagnostic_uses_the_cached_first_cwd() {
    let holder = Scratch::new();
    // Same constraint as the absolute worker: remove_dir(2) refuses a
    // non-empty directory, so the warm cwd is a bare temp dir.
    let warm = std::env::temp_dir().join(format!("pdrs-1461-warm-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&warm).expect("#1461 make the warm cwd");
    std::env::set_current_dir(&warm).expect("#1461 enter the warm cwd");
    // #1461 r6 (DS r5 concurrency P1 / boundary P2): the product cache
    // seeds from `std::env::current_dir()`, which on macOS resolves the
    // `/var` symlink to `/private/var` while `temp_dir()` keeps the
    // literal spelling — build the expectation from the same physical
    // read the cache sees, or the test is deterministically red on
    // darwin (tests/native_process.rs and `path_scratch` precedent).
    let warm_units: Vec<u16> = std::env::current_dir()
        .expect("#1461 warm cwd readable")
        .to_str()
        .expect("#1461 warm path UTF-8")
        .encode_utf16()
        .collect();
    // The relative spelling: a leaf that is a lone surrogate.
    let mut raw: Vec<u16> = "rel".encode_utf16().collect();
    raw.push(0xD800);
    let expected_outer = |warm: &[u16]| {
        let mut outer: Vec<u16> = warm.to_vec();
        outer.push(b'/' as u16);
        outer.extend_from_slice(&raw);
        let inner: Vec<u16> = String::from_utf16_lossy(&outer).encode_utf16().collect();
        let mut expected: Vec<u16> = "Image attachment not found: ".encode_utf16().collect();
        expected.extend_from_slice(&outer);
        expected.extend(" (ENOENT: no such file or directory, stat '".encode_utf16());
        expected.extend_from_slice(&inner);
        expected.extend("').".encode_utf16());
        expected
    };
    let fields = || {
        let mut fields = JsObject::new();
        fields.insert(
            "path".into(),
            JsValue::String(JsString::from_utf16(raw.clone())),
        );
        fields.insert("mimeType".into(), JsValue::from("image/png"));
        fields
    };
    // First compose while the cwd is readable: the diagnostic resolves
    // against the live warm cwd and seeds the cache.
    let first = compose_attachment_turn(&holder.0, &message("hello"), &[JsValue::Object(fields())])
        .expect("#1461 a relative lone-surrogate path composes the turn on a live cwd");
    negative_assert!(
        content_blocks(&first)
            .iter()
            .any(|block| units_contains(&block_text_units(block), &expected_outer(&warm_units))),
        "#1461 the first relative surrogate diagnostic seeds the cache with the warm cwd"
    );
    // Delete the cwd; the second relative diagnostic must still resolve
    // against the cached first read, not drop the diagnostic.
    std::fs::remove_dir(&warm).expect("#1461 delete the warm cwd");
    let second =
        compose_attachment_turn(&holder.0, &message("hello"), &[JsValue::Object(fields())])
            .expect("#1461 a relative lone-surrogate path composes the turn with a deleted cwd");
    negative_assert!(
        content_blocks(&second)
            .iter()
            .any(|block| units_contains(&block_text_units(block), &expected_outer(&warm_units))),
        "#1461 after the cwd is deleted the relative surrogate diagnostic still uses the cached first read"
    );
    negative_assert!(
        second.get("allowedReadFiles").is_none(),
        "#1461 a lone-surrogate path must not register allowedReadFiles after a cwd deletion"
    );
    std::env::set_current_dir(&holder.0).expect("#1461 restore known cwd");
    crate::test_process::receipt("relative-deleted-cwd-warm");
}

// #1461 r6 (DS r5 boundary P2 / sibling P3): same gate as the absolute
// matrix above — the launched worker is #[cfg(unix)]-only.
#[cfg(unix)]
#[test]
fn relative_surrogate_diagnostic_uses_the_cached_first_cwd_matrix() {
    crate::test_process::run_cases(
        "native_process::services_tests::relative_surrogate_diagnostic_uses_the_cached_first_cwd",
        vec![("relative-deleted-cwd-warm".into(), vec![])],
    );
}

#[cfg(unix)]
#[test]
fn direct_content_with_an_unopenable_path_stays_silent() {
    use std::os::unix::fs::PermissionsExt;
    let scratch = Scratch::new();
    // DS r1 boundary P2: the open failure is one of the twelve silent
    // points — a chmod-000 lease file still stats fine (the approved-stat
    // and identity data are all readable without the open) but the open
    // itself fails EACCES, and a content-produced attachment must stay as
    // silent as the audio skip there.
    let path = scratch.lease("secret.png");
    std::fs::write(&path, b"AB").expect("#1460 lease fixture");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000))
        .expect("#1460 chmod fixture");
    // A privileged runner ignores the mode bits; the case cannot be built.
    // #1461 r2 (DS sibling P1, the #1900/#1525 pattern): the skip has to
    // reach the log through the shared writer and fail when the CI switch
    // says the permission face is mandatory — same registration as the
    // storage/tests.rs root arms.
    // #1461 r6 (DS r5 concurrency P2): probe the behaviour, not the uid —
    // the native read probe alone covers every permission-blind runner
    // (root, CAP_DAC_OVERRIDE, permission-blind mounts), and an euid
    // shortcut would false-fail a mode-enforced root host (NFS root_squash)
    // under PDRS_REQUIRE_PERMISSION_FAULTS=1 (probe-only shape, as
    // tests/write_tools.rs).
    if std::fs::read(&path).is_ok() {
        assert!(
            !crate::test_permission_faults::require_permission_faults(),
            "#1460 PDRS_REQUIRE_PERMISSION_FAULTS is set but mode 000 did not stop the open \
             (root or a permission-blind runner); this arm needs a host that enforces mode bits \
             (需要强制 mode 位的宿主)"
        );
        crate::test_skip_line::stderr_skip_line(
            "#1460 skip: direct_content_with_an_unopenable_path_stays_silent: \
             running where mode 000 is readable (root or CAP_DAC_OVERRIDE)\n",
        );
        return;
    }
    let entry = object([
        ("type", JsValue::from("image")),
        (
            "content",
            JsValue::from(BASE64_STANDARD.encode("AB").as_str()),
        ),
        ("mimeType", JsValue::from("image/png")),
        ("path", JsValue::from(path.as_str())),
        (
            "metadata",
            object([("channelKey", JsValue::from("chan-1"))]),
        ),
    ]);
    let result = crate::test_negative_receipt::expect_ok(
        compose_attachment_turn(&scratch.0, &message("hello"), &[entry]),
        "#1460 an unopenable path on a content attachment must not fail the turn",
    );
    let blocks = content_blocks(&result);
    negative_assert!(
        diagnostics_block(&blocks).is_empty(),
        "#1460 an unopenable path on a content attachment must not diagnose"
    );
    negative_assert!(
        blocks.len() == 2,
        "#1460 an unopenable content attachment must keep message and image blocks only"
    );
    negative_assert!(
        result.get("allowedReadFiles").is_none(),
        "#1460 an unopenable path on a content attachment must not register allowedReadFiles"
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
    // #1458: the suffix entry has no channelKey, so its `pdf_size_estimate`
    // info diagnostic enters the block; the registered mime entry keeps the
    // composer's info drop. Message, two pdf blocks, diagnostics, note.
    negative_assert!(
        blocks.len() == 5,
        "#1458 message, two pdf blocks, the info diagnostic and the note are expected, got {}",
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
        diagnostics_block(&blocks).contains(
            "Estimated 2 pages from 250000 bytes (PilotDeck does not invoke pdfinfo, \
             intentional_difference §4.5)."
        ),
        "#1458 the unregistered suffix-routed pdf must carry the pdf_size_estimate info, got: {}",
        diagnostics_block(&blocks)
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

#[test]
fn unregistered_pdf_reports_size_estimate_diagnostic() {
    let scratch = Scratch::new();
    // #1458 (resolver :287-320): `resolvePdf` also produces the
    // `pdf_size_estimate` info diagnostic, and the composer filter
    // (GatewayAttachmentTurnComposer.ts:259-263) keeps info only for an
    // unregistered attachment — an uploaded (channelKey-less) pdf carries
    // it, and any diagnostic switches the path-note guidance to its
    // diagnostics form (composer :114-122). 204800 bytes / 102400 = 2 pages.
    let pdf_path = scratch.lease("upload.pdf");
    std::fs::write(&pdf_path, vec![7u8; 204_800]).expect("#1458 pdf fixture");
    let text_path = scratch.lease("note.txt");
    std::fs::write(&text_path, b"body").expect("#1458 note fixture");
    let result = crate::test_negative_receipt::expect_ok(
        compose_attachment_turn(
            &scratch.0,
            &message("hello"),
            &[
                upload_attachment(&pdf_path, "application/pdf"),
                attachment(&text_path, "text/plain"),
            ],
        ),
        "#1458 an unregistered pdf must compose the turn",
    );
    let blocks = content_blocks(&result);
    negative_assert!(
        diagnostics_block(&blocks).contains(
            "Estimated 2 pages from 204800 bytes (PilotDeck does not invoke pdfinfo, \
             intentional_difference §4.5)."
        ),
        "#1458 an unregistered pdf must carry the pdf_size_estimate diagnostic"
    );
    negative_assert!(
        blocks
            .iter()
            .any(|block| block_text(block).starts_with("[PDF attachment: ")),
        "#1458 the unregistered pdf must keep its estimate block"
    );
    let note = note_block(&blocks);
    negative_assert!(
        note.ends_with(
            "Some attachments were not shown inline. Use read_file with the exact path only \
             for readable text, image, PDF, or notebook attachments; Office/archive/binary \
             files need conversion before inspection."
        ),
        "#1458 the diagnostics guidance form must be selected, got: {note}"
    );
    negative_assert!(
        note.contains(&format!("- note.txt: {text_path}")),
        "#1458 the registered companion keeps its note line, got: {note}"
    );
}

#[test]
fn registered_pdf_has_no_size_estimate_diagnostic() {
    let scratch = Scratch::new();
    // #1458 pair: the composer filter drops the info diagnostic of a
    // registered (channelKey) pdf — the pre-#1458 "registered info is
    // dropped" outcome survives for exactly the registered half.
    let pdf_path = scratch.lease("doc.pdf");
    std::fs::write(&pdf_path, vec![7u8; 204_800]).expect("#1458 registered pdf fixture");
    let result = crate::test_negative_receipt::expect_ok(
        compose_attachment_turn(
            &scratch.0,
            &message("hello"),
            &[attachment(&pdf_path, "application/pdf")],
        ),
        "#1458 a registered pdf must compose the turn",
    );
    let blocks = content_blocks(&result);
    negative_assert!(
        diagnostics_block(&blocks).is_empty(),
        "#1458 a registered pdf must not carry the pdf_size_estimate diagnostic"
    );
    let note = note_block(&blocks);
    negative_assert!(
        note.ends_with(
            "These are path references for reuse. If an image/PDF is already visible in this \
             turn, do not call read_file just to view it."
        ),
        "#1458 a registered pdf keeps the plain guidance form, got: {note}"
    );
}

#[cfg(unix)]
#[test]
fn unresolvable_lease_root_softens_to_not_found() {
    use std::os::unix::fs::symlink;
    let scratch = Scratch::new();
    // #1459: a lease root whose canonicalize fails with a non-NotFound
    // errno is no valid root either — the TS composer has no lease-root
    // concept and no stat failure of its own ever rejects a turn, so the
    // ELOOP root (a self-referential symlink; an EACCES root via chmod 000
    // would not trigger under a root uid) softens to the same per-attachment
    // not-found diagnostics as a missing root. P0-a holds: zero reads, zero
    // registrations.
    let lease_root = scratch.0.join(".tmp").join("chat-upload-leases");
    std::fs::remove_dir_all(&lease_root).expect("#1459 remove the lease root");
    symlink(&lease_root, &lease_root).expect("#1459 lease root symlink loop");
    let path = scratch.lease("looped.png");
    let result = crate::test_negative_receipt::expect_ok(
        compose_attachment_turn(
            &scratch.0,
            &message("hello"),
            &[attachment(&path, "image/png")],
        ),
        "#1459 an ELOOP lease root must not fail the turn",
    );
    std::fs::remove_file(&lease_root).expect("#1459 remove the lease root loop");
    let blocks = content_blocks(&result);
    negative_assert!(
        diagnostics_block(&blocks).contains(&format!(
            "Image attachment not found: {path} (attachment lease root is unavailable)."
        )),
        "#1459 an ELOOP lease root must soften every lease path to not-found, got: {}",
        diagnostics_block(&blocks)
    );
    negative_assert!(
        blocks
            .iter()
            .all(|block| block.get("type") != Some(&"image".into())),
        "#1459 an ELOOP lease root must never inline"
    );
    negative_assert!(
        result.get("allowedReadFiles").is_none(),
        "#1459 an ELOOP lease root must never register allowedReadFiles"
    );
    negative_assert!(
        note_block(&blocks).is_empty(),
        "#1459 an ELOOP lease root must never produce a note line"
    );
}

#[cfg(unix)]
#[test]
fn symlink_loop_lease_path_softens_to_not_found_diagnostic() {
    use std::os::unix::fs::symlink;
    let scratch = Scratch::new();
    // #1629: a symlink loop on the lease path itself — a canonicalize
    // failure with a non-`NotFound` errno under a valid root — no longer
    // fails the turn. The TS resolver's stat try/catch has no errno filter
    // (AttachmentResolver.ts:188-201, Node probe
    // `_tmp/20261026-issue-1629/probe/ts-stat-failures.mjs`), so the ELOOP
    // softens exactly like the TTL cleanup race: no read, no registration,
    // no inline.
    let link = scratch.lease("loop.png");
    symlink(Path::new(&link), Path::new(&link)).expect("#1629 symlink loop fixture");
    let result = crate::test_negative_receipt::expect_ok(
        compose_attachment_turn(
            &scratch.0,
            &message("hello"),
            &[attachment(&link, "image/png")],
        ),
        "#1629 a symlink-loop lease path must not fail the turn",
    );
    let blocks = content_blocks(&result);
    // std's OS-error Display is `{strerror} (os error {errno})` on both
    // Linux and macOS, but the ELOOP errno differs (40/62), so the pin
    // stops before the number.
    negative_assert!(
        diagnostics_block(&blocks).contains(&format!(
            "Image attachment not found: {link} (Too many levels of symbolic links (os error "
        )),
        "#1629 a symlink-loop lease path must soften to the not-found diagnostic, got: {}",
        diagnostics_block(&blocks)
    );
    negative_assert!(
        blocks
            .iter()
            .all(|block| block.get("type") != Some(&"image".into())),
        "#1629 a symlink-loop lease path must never inline"
    );
    negative_assert!(
        result.get("allowedReadFiles").is_none(),
        "#1629 a symlink-loop lease path must never register allowedReadFiles"
    );
    negative_assert!(
        note_block(&blocks).is_empty(),
        "#1629 a symlink-loop lease path must never produce a note line"
    );
}

#[cfg(unix)]
#[test]
fn not_directory_lease_path_softens_to_not_found_diagnostic() {
    let scratch = Scratch::new();
    // #1629: the non-ELOOP spelling of a non-`NotFound` canonicalize
    // failure — a regular file where a directory component is expected
    // (ENOTDIR), the same error class the TS stat catch swallows. An EACCES
    // fixture would need chmod 000, which does not fail under a root uid
    // (the #1459 root-test precedent), so ENOTDIR — uid-independent and
    // POSIX-mandated on both Unixes CI runs (Linux and macOS; an exotic
    // mount reporting ENOENT here would still soften, only the pinned
    // wording would differ) — carries the "any failure" claim.
    std::fs::write(scratch.lease("plain.txt"), b"x").expect("#1629 regular-file fixture");
    let path = scratch.lease("plain.txt/sub.png");
    let result = crate::test_negative_receipt::expect_ok(
        compose_attachment_turn(
            &scratch.0,
            &message("hello"),
            &[attachment(&path, "image/png")],
        ),
        "#1629 an ENOTDIR lease path must not fail the turn",
    );
    let blocks = content_blocks(&result);
    negative_assert!(
        diagnostics_block(&blocks).contains(&format!(
            "Image attachment not found: {path} (Not a directory (os error "
        )),
        "#1629 an ENOTDIR lease path must soften to the not-found diagnostic, got: {}",
        diagnostics_block(&blocks)
    );
    negative_assert!(
        blocks
            .iter()
            .all(|block| block.get("type") != Some(&"image".into())),
        "#1629 an ENOTDIR lease path must never inline"
    );
    negative_assert!(
        result.get("allowedReadFiles").is_none(),
        "#1629 an ENOTDIR lease path must never register allowedReadFiles"
    );
    negative_assert!(
        note_block(&blocks).is_empty(),
        "#1629 an ENOTDIR lease path must never produce a note line"
    );
}

// ------------------------------------------- #1320 fix-r1 F2/F3 settings tests

/// The minimal valid scratch config (the `management_tests` fixture shape):
/// a load failure would fail the composition before either assertion runs.
const SETTINGS_YAML: &str = "schemaVersion: 1\nagent:\n  model: test/test\nmodel:\n  \
     providers:\n    test:\n      protocol: openai\n      url: http://127.0.0.1:1\n      \
     apiKey: test-only\n      models:\n        test: {}\n";

/// A no-op `LiveSession`/`SessionFactory` pair for the settings compositions'
/// session-owner port: none of the F2/F3 assertions drives a turn, so the
/// factory only has to exist (the same minimal pair `management_tests` uses).
struct NullSession;
impl LiveSession for NullSession {
    fn snapshot(&self) -> JsValue {
        object([("messages", JsValue::Array(vec![]))])
    }
    fn abort(&self, _: &JsString) -> RuntimeResult {
        Ok(())
    }
    fn dispose(&self) -> DisposeFuture<'_> {
        Box::pin(async { Ok(()) })
    }
}
struct NullFactory;
impl SessionFactory for NullFactory {
    fn create(
        &self,
        _: SessionContext,
        _: Option<LiveSessionHandle>,
    ) -> BoxFuture<Result<LiveSessionHandle, RuntimeError>> {
        Box::pin(async { Ok(LiveSessionHandle::new(Arc::new(NullSession), None)) })
    }
    fn can_recreate(&self) -> bool {
        true
    }
}

fn settings_owner() -> SessionOwner {
    let mut options = SessionOwnerOptions::new(Arc::new(NullFactory));
    options.idle_sweep_interval_ms = 0;
    SessionOwner::new(options)
}

struct NullRegistry;
impl RefreshRegistry for NullRegistry {
    fn reload(&self) -> Result<BoxFuture<RuntimeResult>, RuntimeError> {
        Ok(Box::pin(async { Ok(()) }))
    }
    fn invalidate(&self, _: Option<&JsString>) {}
}

struct NullRouter;
impl RefreshRouter for NullRouter {
    fn mark_all_dirty(&self, _: &str) -> usize {
        0
    }
    fn mark_project_dirty(&self, _: &JsString, _: &str) -> usize {
        0
    }
    fn mark_session_dirty(&self, _: &JsString, _: &str) -> bool {
        false
    }
    fn cached_session_count(&self) -> usize {
        0
    }
    fn snapshot_session(&self, _: &JsString) -> Option<JsValue> {
        None
    }
}

struct NullServer;
impl NotificationServer for NullServer {
    fn broadcast_notification(&self, _: &str, _: &JsValue) {}
}

/// A store over an own scratch home (the `own_store` shape of the #1320 S4
/// fixtures): the same file anchor and env, so only the composition under
/// test decides what loads.
fn scratch_store(home: &Path, yaml: &str) -> ConfigStore {
    std::fs::create_dir_all(home).expect("#1320 scratch home");
    std::fs::write(home.join("pilotdeck.yaml"), yaml).expect("#1320 scratch config");
    let home_text = home.to_string_lossy().into_owned();
    let env = Env::from([
        ("HOME".to_owned(), home_text.clone()),
        ("PILOT_HOME".to_owned(), home_text),
    ]);
    let mut options = StoreOptions::new(home.to_path_buf(), home.to_path_buf(), env);
    options.project = Some(home.join("pilotdeck.yaml").to_string_lossy().into_owned());
    ConfigStore::new(options).expect("#1320 the scratch store must load")
}

/// #1320 fix-r1 F2: the production settings composition binds no notification
/// server — the isolated native process has no server-push channel, so the
/// bundle's broadcasts are lost (the connection registry is #1669, not built
/// here). The bound-sever half is the positive control proving the negative
/// assertion is not vacuous: the seam reports `true` the moment a server is
/// bound, exactly what the S4 order tests rely on.
#[tokio::test]
async fn native_settings_production_binds_no_notification_server() {
    let root = std::env::temp_dir().join(format!("pdrs-1320-f2-{}", uuid::Uuid::new_v4()));
    let store = scratch_store(&root.join("home"), SETTINGS_YAML);
    let settings = native_settings(NativeSettingsOptions {
        owner: settings_owner(),
        router: Arc::new(NullRouter),
        registry: Arc::new(NullRegistry),
        store,
        project_root: JsString::from("/pdrs-fixture/project"),
        log: Arc::new(ConsoleLog),
    });
    negative_assert!(
        !settings.bundle.has_notification_server(),
        "#1320 生产组合不得假装有广播通道（见 #1669）"
    );
    settings.bundle.bind_server(Arc::new(NullServer));
    negative_assert!(
        settings.bundle.has_notification_server(),
        "#1320 the bind_server seam must report the recording server the order tests bind"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// #1869 item 5: `settings_store_anchors_at_pilot_home_env` is the kill target
/// of five roster mutants, so it panics on essentially every negative-control
/// run. The scratch tree must therefore be removed by a guard, not by the
/// success path's trailing `remove_dir_all` — that line never ran on the runs
/// that matter and each of them left one `pdrs-1320-f3-<uuid>` directory
/// behind.
struct AnchorScratch(PathBuf);
impl AnchorScratch {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("pdrs-1320-f3-{}", uuid::Uuid::new_v4())))
    }
}
impl Drop for AnchorScratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// #1320 fix-r1 F3: the settings store anchors at PILOT_HOME from the
/// environment — the same `resolvePilotHome` every other config read of the
/// process uses (`NativeConfigSource`) — with `--home` only as the fallback
/// homedir. This is a unit property of `resolve_pilot_home`: the synthetic
/// env (`PILOT_HOME=ph` while `--home=flag_home`) is unreachable in
/// production, where `Options::environment` overwrites PILOT_HOME with
/// `--home` unconditionally, so the production anchor is always `--home`.
/// Two configs differing by anchor comment: the resolved
/// `contentHash`/`sources` must come from the PILOT_HOME copy, never from
/// the `--home` copy.
#[tokio::test]
async fn settings_store_anchors_at_pilot_home_env() {
    let scratch = AnchorScratch::new();
    let root = scratch.0.clone();
    let pilot_home = root.join("ph");
    let flag_home = root.join("home");
    let served_project = root.join("project");
    std::fs::create_dir_all(&served_project).expect("#1320 served project");
    let pilot_yaml = format!("{SETTINGS_YAML}# pilot-home anchor\n");
    let flag_yaml = format!("{SETTINGS_YAML}# flag-home anchor\n");
    std::fs::create_dir_all(&pilot_home).expect("#1320 the PILOT_HOME scratch dir");
    std::fs::write(pilot_home.join("pilotdeck.yaml"), &pilot_yaml)
        .expect("#1320 the PILOT_HOME copy");
    std::fs::create_dir_all(&flag_home).expect("#1320 the --home scratch dir");
    std::fs::write(flag_home.join("pilotdeck.yaml"), &flag_yaml).expect("#1320 the --home copy");
    let env = Env::from([
        ("HOME".to_owned(), flag_home.to_string_lossy().into_owned()),
        (
            "PILOT_HOME".to_owned(),
            pilot_home.to_string_lossy().into_owned(),
        ),
    ]);
    let registry = Arc::new(ProjectRuntimeRegistry::new(
        served_project.clone(),
        Arc::new(|_: &Path, _: &mut ResourceBundle| Ok(())),
    ));
    let settings = native_settings_for_process(
        settings_owner(),
        registry,
        &flag_home,
        &served_project,
        &env,
    )
    .expect("#1320 the production settings stack must compose");
    let resolved = match settings
        .gateway
        .dispatch("resolve_settings", &JsValue::Undefined)
        .await
    {
        Some(Ok(value)) => value,
        Some(Err(error)) => panic!(
            "#1320 resolve_settings must succeed over the production stack: {}",
            String::from_utf16_lossy(error.message().units())
        ),
        None => panic!("#1320 resolve_settings must be dispatched by the gateway"),
    };
    // Reference stores over each home, anchored exactly like the composition
    // (cwd/projectRoot = the served project): the resolved projection has to
    // match the PILOT_HOME one and differ from the --home one.
    let reference = |home: &Path| {
        let mut options =
            StoreOptions::new(home.to_path_buf(), served_project.clone(), env.clone());
        options.project = Some(served_project.to_string_lossy().into_owned());
        let store = ConfigStore::new(options).expect("#1320 reference store");
        store.resolved_settings()
    };
    let pilot_reference = reference(&pilot_home);
    let flag_reference = reference(&flag_home);
    // #1320 non-vacuous: every side must carry the compared fields before any
    // comparison runs. A projection that stopped producing `sources` or
    // `contentHash` would otherwise pass vacuously (`[] == []`,
    // `Undefined == Undefined`) and silently retire this, the F3 production
    // anchor's only guard — a missing field must fail explicitly here.
    //
    // #1762: `flag_reference` is in the loop too. It only ever appeared in the
    // `!=` comparison below, so a projection that emptied it kept
    // `resolved != []` true and the anti-anchor control went vacuous.
    for (side, value) in [
        ("resolved", &resolved),
        ("pilot_reference", &pilot_reference),
        ("flag_reference", &flag_reference),
    ] {
        negative_assert!(
            matches!(value.get("sources"), Some(JsValue::Array(sources)) if !sources.is_empty()),
            "#1320 non-vacuous: {side} must carry a non-empty sources array — a missing \
             field must fail explicitly, not pass vacuously as [] == []"
        );
        // #1869: "is a string" is not enough — a projection that emptied the
        // hash on all three sides would compare "" == "" and report a clean
        // anchor. The empty check is the top-level twin of `text_of`'s
        // per-source guard.
        negative_assert!(
            matches!(value.get("contentHash"), Some(JsValue::String(text)) if !text.is_empty()),
            "#1869 non-vacuous: {side} must carry a non-empty string contentHash — an empty \
             string must fail explicitly, not pass vacuously as \"\" == \"\""
        );
        // #1762: env sources carry no `path` by design, so the per-source
        // requirement above cannot cover `path`; the file source must survive
        // on every side or the path half of the triple goes vacuous.
        negative_assert!(
            has_file_source(value),
            "#1762 non-vacuous: {side} must carry at least one source with a non-empty path \
             — dropping `path` from the projection must not pass as \"\" == \"\""
        );
    }
    negative_assert_eq!(
        resolved
            .get("contentHash")
            .cloned()
            .unwrap_or(JsValue::Undefined),
        pilot_reference
            .get("contentHash")
            .cloned()
            .unwrap_or(JsValue::Undefined),
        "#1320 the resolved contentHash must come from the PILOT_HOME copy"
    );
    negative_assert!(
        source_triples(&resolved) == source_triples(&pilot_reference),
        "#1320 the resolved sources must come from the PILOT_HOME copy, got: {:?}",
        source_triples(&resolved)
    );
    negative_assert!(
        source_triples(&resolved) != source_triples(&flag_reference),
        "#1320 the resolved sources must not come from the --home copy"
    );
}

/// `(kind, path, contentHash)` per source: the fields that identify WHICH
/// copy was loaded, unlike `loadedAt` (a per-load timestamp).
///
/// #1320 non-vacuous: a missing or non-array `sources` is an explicit
/// failure, not `Vec::new()`. The empty return used to let the anchor
/// comparison above pass vacuously when the projection stopped producing
/// the field.
///
/// #1762 non-vacuous: an empty array is a failure too, and the always-present
/// per-source fields must be strings. `config/settings.rs::projected()`
/// projects a missing `sources` to `JsonValue::Array([])`, so "missing"
/// reaches this helper as `[]` — not as `Undefined` — and `[] == []` would
/// pass the anchor comparison vacuously.
///
/// `path` is deliberately lenient: `config/snapshot.rs::SourceRecord::value`
/// omits it for the `env` sources (`bootstrap_env` / `merge_env` carry
/// `path: None` by design), so requiring a string on every source would fail
/// on a healthy tree — measured, not assumed (#1762 item 3 assumed otherwise).
/// The loss-of-field regression it is there to catch is covered by
/// [`has_file_source`] in the non-vacuous loop above: every side must still
/// carry at least one source with a real path, so a projection that stopped
/// emitting `path` at all fails instead of comparing `("file", "", hash)`.
fn source_triples(resolved: &JsValue) -> Vec<(String, String, String)> {
    let sources = match resolved.get("sources") {
        Some(JsValue::Array(sources)) => sources,
        _ => panic!(
            "#1320 non-vacuous: sources must be present as an array before its triples are \
             compared — a missing field must fail explicitly, not pass vacuously"
        ),
    };
    negative_assert!(
        !sources.is_empty(),
        "#1762 non-vacuous: sources must not be an empty array — the projection maps a \
         missing field to [], and [] == [] would pass vacuously"
    );
    sources
        .iter()
        .map(|source| {
            (
                text_of(source, "kind"),
                optional_path_of(source),
                text_of(source, "contentHash"),
            )
        })
        .collect()
}

/// #1762: `kind` and `contentHash` are always present on a projected source
/// (`SourceRecord` builds both unconditionally). A missing or non-string value
/// is an explicit failure: the old `unwrap_or_default()` compared `("", "")`
/// with `("", "")` and let a source that lost the field pass the anchor
/// comparison.
fn text_of(value: &JsValue, key: &str) -> String {
    let text = match value.get(key) {
        Some(JsValue::String(text)) => text,
        _ => panic!(
            "#1762 non-vacuous: every source must carry a string `{key}` before its triples \
             are compared — a missing or non-string field must fail explicitly, not pass \
             vacuously as an empty string"
        ),
    };
    let text = text.to_utf8().unwrap_or_else(|_| {
        panic!("#1762 non-vacuous: source `{key}` must be valid UTF-8, not lossy text")
    });
    // Present-but-empty is still vacuous: a projection that wiped every
    // `contentHash` to "" would compare ("", "") with ("", "") and report a
    // clean anchor. `SourceRecord` builds both fields unconditionally and a
    // content hash is a digest, so empty can only be a projection bug.
    negative_assert!(
        !text.is_empty(),
        "#1762 non-vacuous: source `{key}` must be non-empty, not \"\""
    );
    text
}

/// `path` is absent on the `env` sources by construction and present whenever
/// the source came from a file. Absent is `""`; present-but-not-a-string is a
/// failure, because that can only be a projection bug.
fn optional_path_of(source: &JsValue) -> String {
    match source.get("path") {
        None => String::new(),
        Some(JsValue::String(text)) => text.to_utf8().unwrap_or_else(|_| {
            panic!("#1762 non-vacuous: source `path` must be valid UTF-8, not lossy text")
        }),
        Some(other) => panic!(
            "#1762 non-vacuous: a source `path` must be a string when present, got: {other:?}"
        ),
    }
}

/// #1762: at least one source must still name a file. Without it, a
/// projection that stopped emitting `path` on the file sources would compare
/// `("file", "", hash) == ("file", "", hash)` and report a clean anchor.
fn has_file_source(value: &JsValue) -> bool {
    match value.get("sources") {
        Some(JsValue::Array(sources)) => sources.iter().any(|source| {
            matches!(source.get("path"), Some(JsValue::String(text))
                if text.to_utf8().is_ok_and(|path| !path.is_empty()))
        }),
        _ => false,
    }
}

/// #1448 r5：limit+1 边界——恰好 limit 内联（`exact_limit_image_and_text_
/// are_inlined` 已钉），恰好 limit+1 跳过进诊断，两侧互为对照。
#[test]
fn limit_plus_one_attachments_skip_to_diagnostics() {
    let scratch = Scratch::new();
    let image_path = scratch.lease("over.png");
    std::fs::write(&image_path, vec![1u8; 5 * 1024 * 1024 + 1])
        .expect("#1448 limit-plus-one image fixture");
    let text_path = scratch.lease("over.txt");
    std::fs::write(&text_path, vec![b'x'; 1_000_001]).expect("#1448 limit-plus-one text fixture");
    let result = compose_attachment_turn(
        &scratch.0,
        &message("hello"),
        &[
            attachment(&image_path, "image/png"),
            attachment(&text_path, "text/plain"),
        ],
    )
    .expect("#1448 limit-plus-one attachments still compose the turn");
    let blocks = content_blocks(&result);
    let diagnostics = diagnostics_block(&blocks);
    negative_assert!(
        diagnostics.contains(&format!(
            "Image {image_path} is 5242881 bytes (limit 5242880); skipped \
             (PilotDeck does not resize, intentional_difference §4.5)."
        )),
        "#1448 a 5242881-byte image must be skipped with the exact size"
    );
    negative_assert!(
        diagnostics.contains(&format!(
            "Attachment {text_path} is 1000001 bytes (limit 1000000); skipped."
        )),
        "#1448 a 1000001-byte text attachment must be skipped with the exact size"
    );
    negative_assert!(
        blocks
            .iter()
            .all(|block| block.get("type") != Some(&"image".into())),
        "#1448 a limit-plus-one image must not be inlined"
    );
    negative_assert!(
        blocks
            .iter()
            .all(|block| !block_text(block).starts_with("<attachment path=")),
        "#1448 a limit-plus-one text attachment must not be inlined"
    );
}

/// #1448：manifest `bytes: 0`——真实零字节附件照常注册（note 行 +
/// allowedReadFiles）并按 TS 包装内联空文本；`bytes` 声明不改变读到的
/// 实际字节数（AttachmentResolver.ts:187-202 只信 stat）。
#[test]
fn zero_byte_text_attachment_registers_and_inlines_empty() {
    let scratch = Scratch::new();
    let path = scratch.lease("empty.txt");
    std::fs::write(&path, b"").expect("#1448 zero-byte fixture");
    let result = compose_attachment_turn(
        &scratch.0,
        &message("hello"),
        &[attachment(&path, "text/plain")],
    )
    .expect("#1448 a zero-byte attachment composes the turn");
    let blocks = content_blocks(&result);
    negative_assert!(
        blocks.len() == 3,
        "#1448 a zero-byte attachment must keep message, text and note blocks, got {}",
        blocks.len()
    );
    negative_assert!(
        block_text(&blocks[1]) == format!("<attachment path=\"{path}\">\n\n</attachment>"),
        "#1448 a zero-byte text attachment must inline the empty TS wrapper"
    );
    let allowed = match result.get("allowedReadFiles") {
        Some(JsValue::Array(files)) => files.clone(),
        _ => Vec::new(),
    };
    negative_assert!(
        allowed == vec![JsValue::from(path.as_str())],
        "#1448 a zero-byte attachment must register allowedReadFiles"
    );
    negative_assert!(
        note_block(&blocks).contains(&format!("- empty.txt: {path}")),
        "#1448 a zero-byte attachment must keep its note line"
    );
}

/// #1448 fix-r2 收编行「无界读」的杀测试：fstat（权威超限门）之后、读之前
/// 文件长过上限——有界读最多读回 limit+1 字节，读满即超限回填诊断；无界
/// 读会把超出上限的字节整段内联。注入点 `BeforeRead`。
#[test]
fn grown_between_stat_and_read_hits_the_full_read_backstop() {
    let scratch = Scratch::new();
    let path = scratch.lease("grown.txt");
    std::fs::write(&path, vec![b'x'; 1_000_000]).expect("#1448 exact-limit fixture");
    let grown = std::path::PathBuf::from(&path);
    let _hook = swap_test_hook::set(swap_test_hook::SwapWindow::BeforeRead, move |resolved| {
        if resolved == grown {
            std::fs::write(&grown, vec![b'x'; 1_000_005]).expect("#1448 grow the leaf");
        }
    });
    let result = compose_attachment_turn(
        &scratch.0,
        &message("hello"),
        &[attachment(&path, "text/plain")],
    )
    .expect("#1448 a grown read must not fail the turn");
    let blocks = content_blocks(&result);
    let diagnostics = diagnostics_block(&blocks);
    negative_assert!(
        diagnostics.contains(&format!(
            "Attachment {path} is 1000001 bytes (limit 1000000); skipped."
        )),
        "#1448 a grown read must hit the full-read backstop and skip"
    );
    negative_assert!(
        blocks
            .iter()
            .all(|block| !block_text(block).starts_with("<attachment path=")),
        "#1448 a grown read must not inline over the cap"
    );
}

/// #1448 fix-r2 收编行「content 删除」的杀测试：content 图片分支被删除
/// （或反转）时该测试红。断言消息为静态串（负控回执按格式化后的全文匹配）。
#[test]
fn content_image_branch_emits_the_flat_block() {
    let scratch = Scratch::new();
    let image_content = object([
        ("type", JsValue::from("image")),
        (
            "content",
            JsValue::from(BASE64_STANDARD.encode("ABC").as_str()),
        ),
        ("mimeType", JsValue::from("image/png")),
        ("bytes", JsValue::Number(3.0)),
    ]);
    let result = compose_attachment_turn(&scratch.0, &message("hello"), &[image_content])
        .expect("#1448 a content image composes without any path");
    let blocks = content_blocks(&result);
    negative_assert!(
        blocks.len() == 2,
        "#1448 the content image branch must keep message and image blocks"
    );
    negative_assert!(
        blocks[1].get("type") == Some(&"image".into())
            && blocks[1].get("source") == Some(&"base64".into())
            && blocks[1].get("data")
                == Some(&JsValue::from(BASE64_STANDARD.encode("ABC").as_str()))
            && blocks[1].get("mimeType") == Some(&JsValue::from("image/png")),
        "#1448 the content image branch must emit the flat block"
    );
}

/// #1448 端到端：经 `SubmitHostServices::prepare_attachment_turn`（trait →
/// validate_profile → spawn_blocking → compose_attachment_turn）组合轮次，
/// 正常租约附件全链（内联 + 双路注册 + note 行）。
#[tokio::test]
async fn prepare_attachment_turn_composes_the_turn_end_to_end() {
    use crate::runtime::turn::submit_host::SubmitHostServices as _;
    let (_fixture, requests) = std::panic::catch_unwind(super::management_tests::setup)
        .expect("#1448 registry fixture setup");
    let project = requests.options.project.clone();
    let lease_dir = project.join(".tmp").join("chat-upload-leases").join("u1");
    std::fs::create_dir_all(&lease_dir).expect("#1448 e2e lease fixture dir");
    std::fs::write(lease_dir.join("a.png"), [1u8, 2, 3, 4]).expect("#1448 e2e lease file");
    let path = lease_dir
        .join("a.png")
        .to_str()
        .expect("#1448 e2e UTF-8")
        .to_owned();
    let services = super::services::Services {
        options: requests.options.clone(),
        project: requests.project.clone(),
        env: requests.env.clone(),
        recorders: Arc::default(),
        owner: requests.owner.clone(),
        snapshot_recorder: None,
        upload_store: Arc::new(crate::upload::UploadStore::new(
            crate::upload::UploadStoreOptions::new(
                |_| Ok(std::path::PathBuf::new()),
                || Ok(Vec::new()),
            ),
        )),
    };
    let result = services
        .prepare_attachment_turn(
            &message("hello"),
            vec![attachment(&path, "image/png")],
            None,
            None,
        )
        .await
        .expect("#1448 prepare_attachment_turn must compose the turn");
    let blocks = content_blocks(&result);
    negative_assert!(
        blocks.len() == 3,
        "#1448 the e2e composition must keep message, image and note blocks, got {}",
        blocks.len()
    );
    negative_assert!(
        blocks[1].get("type") == Some(&"image".into())
            && blocks[1].get("bytes") == Some(&JsValue::Number(4.0)),
        "#1448 the e2e composition must inline the lease image"
    );
    let allowed = match result.get("allowedReadFiles") {
        Some(JsValue::Array(files)) => files.clone(),
        _ => Vec::new(),
    };
    negative_assert!(
        allowed == vec![JsValue::from(path.as_str())],
        "#1448 the e2e composition must register the lease path"
    );
    negative_assert!(
        note_block(&blocks).contains(&format!("- a.png: {path}")),
        "#1448 the e2e composition must keep the note line"
    );
    requests
        .owner
        .shutdown()
        .await
        .expect("#1448 e2e fixture owner shutdown");
    requests
        .project
        .dispose()
        .await
        .expect("#1448 e2e fixture project shutdown");
}

/// #1448 端到端边界：lease 根本身是指向项目内其它子树的 symlink——经
/// `prepare_attachment_turn` 全链走 not-found 分支（canonical_home 逐组件
/// 走查拒绝链根），不内联、不注册。
#[cfg(unix)]
#[tokio::test]
async fn prepare_attachment_turn_refuses_a_linked_lease_root() {
    use crate::runtime::turn::submit_host::SubmitHostServices as _;
    use std::os::unix::fs::symlink;
    let (_fixture, requests) = std::panic::catch_unwind(super::management_tests::setup)
        .expect("#1448 registry fixture setup");
    let project = requests.options.project.clone();
    let lease_root = project.join(".tmp").join("chat-upload-leases");
    std::fs::create_dir_all(&lease_root).expect("#1448 boundary lease root");
    let elsewhere = project.join("elsewhere");
    std::fs::create_dir_all(elsewhere.join("u1")).expect("#1448 boundary target dir");
    std::fs::write(elsewhere.join("u1").join("linked.png"), [1u8, 2, 3, 4])
        .expect("#1448 boundary target file");
    std::fs::remove_dir_all(&lease_root).expect("#1448 drop the real lease root");
    symlink(&elsewhere, &lease_root).expect("#1448 swap the lease root for a link");
    let path = lease_root
        .join("u1")
        .join("linked.png")
        .to_str()
        .expect("#1448 boundary UTF-8")
        .to_owned();
    let services = super::services::Services {
        options: requests.options.clone(),
        project: requests.project.clone(),
        env: requests.env.clone(),
        recorders: Arc::default(),
        owner: requests.owner.clone(),
        snapshot_recorder: None,
        upload_store: Arc::new(crate::upload::UploadStore::new(
            crate::upload::UploadStoreOptions::new(
                |_| Ok(std::path::PathBuf::new()),
                || Ok(Vec::new()),
            ),
        )),
    };
    let result = services
        .prepare_attachment_turn(
            &message("hello"),
            vec![attachment(&path, "image/png")],
            None,
            None,
        )
        .await
        .expect("#1448 a linked lease root must not fail the e2e turn");
    let blocks = content_blocks(&result);
    negative_assert!(
        diagnostics_block(&blocks).contains(&format!(
            "Image attachment not found: {path} (attachment lease root is unavailable)."
        )),
        "#1448 the e2e turn must take the not-found branch for a linked lease root"
    );
    negative_assert!(
        blocks
            .iter()
            .all(|block| block.get("type") != Some(&"image".into())),
        "#1448 the e2e turn must never inline through a linked lease root"
    );
    negative_assert!(
        result.get("allowedReadFiles").is_none(),
        "#1448 the e2e turn must never register through a linked lease root"
    );
    requests
        .owner
        .shutdown()
        .await
        .expect("#1448 boundary fixture owner shutdown");
    requests
        .project
        .dispose()
        .await
        .expect("#1448 boundary fixture project shutdown");
}
