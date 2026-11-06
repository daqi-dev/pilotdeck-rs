#!/usr/bin/env python3
"""Temporary upload-gateway product mutations; each must turn a named test red.

The product files already exist in HEAD, but ``mutating_sources`` also refuses
while the fix-r2 patch is uncommitted (the working tree must match HEAD). Run
this after the coordinator commits the branch:

    CARGO_TARGET_DIR=<caller-owned cargo target directory> \
      python3 src/pilotdeck-gateway/tests/fixtures/upload-gateway/negative_controls.py <out-dir>

For a pre-commit rehearsal only, set ``PDRS_NEG_BASELINE`` to a tree object with
the exact same bytes as the working tree (e.g. ``git stash create <label>``);
the default remains ``HEAD`` so a stale tree is never silently accepted.

``--check`` only verifies that every mutation anchor still matches the current
source; it does not mutate, compile, or need a commit.

A mutation counts as KILLED only when the *named* test fails with the
mutation's own expected assertion text: the same command can also kill an
unrelated assertion, e.g. the oracle's property-order check, which is not
evidence for the behavior under test (review P1-5).

The #1494 rows pin the read_at handle binding: dropping it, or restoring the
pre-fix pre-open path spelling check, must each turn its named TOCTOU test
red; the pre-open identity row drops the identity term; and the approved-stat
row restores the follow-the-link sample, which lets a link planted before the
approved stat become the approved object.

The lease-link rows pin the lease hard link: one drops the identity
comparison, the other restores the follow-the-link sample of the linked
entry. Both are killable on every Unix: ``std::fs::hard_link`` does not follow
a symlink source (``linkat(..., 0)`` on macOS as well as Linux), so the
lease entry is the link itself and the follow-the-link sample accepts it.
Measured 2026-11-06: after ``fs::hard_link(link, dest)``,
``fs::symlink_metadata(dest)`` reports a symlink; Python's ``os.link`` is the
one that follows, which is why the fixture does not use it.
"""
import difflib
import hashlib
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

root = Path(__file__).resolve().parents[5]
GATEWAY = root / 'src/pilotdeck-gateway/src/upload/gateway.rs'
STORE = root / 'src/pilotdeck-gateway/src/upload/mod.rs'
ORACLE = 'upload::gateway_tests::upload_gateway_oracle_replay_matches_ts_results'
PATH_TEST = 'upload::gateway_tests::upload_gateway_never_projects_attachment_disk_path'
PROJECTION = 'upload::gateway_tests::upload_gateway_projection_drops_unknown_and_empty_optional_fields'
ORDER = 'upload::gateway_tests::upload_gateway_validation_order_matches_ts'
NULL_ATTACH = 'upload::gateway_tests::upload_gateway_null_attachment_keeps_ts_property_error'
KEY_ORDER = 'upload::gateway_tests::upload_gateway_idempotency_key_type_is_checked_after_project_and_manifest'
KEY_NUMBER = 'upload::gateway_tests::upload_gateway_idempotency_number_messages_match_node'
KEY_DEDUPE = 'upload::gateway_tests::upload_gateway_idempotency_hash_still_dedupes_after_moving_into_store'
FIDELITY = 'upload::gateway_tests::upload_gateway_preserves_raw_utf16_parameters'
SIZE = 'upload::gateway_tests::upload_gateway_part_size_cap_is_checked_before_allocating'
MESSAGES = 'upload::gateway_tests::upload_gateway_error_messages_preserve_lone_surrogates'
READ_AT_SWAP = ('upload::tests::upload_read_at_metadata_swapped_between_'
                'canonicalize_and_open_is_rejected')
LEASE_CHAIN = 'upload::tests::upload_read_at_forged_record_cannot_reach_the_lease'
READ_AT_RENAME = 'upload::tests::upload_read_at_record_renamed_after_open_is_rejected'
READ_AT_PREAPPROVED = (
    'upload::tests::upload_read_at_metadata_swapped_before_approved_stat_is_rejected')
LEASE_LINK = 'upload::tests::upload_lease_link_of_a_swapped_attachment_is_rejected'
LEASE_LINK_SYMLINK = 'upload::tests::upload_lease_link_never_pins_a_symlink_source'
SWEEP = 'upload::tests::upload_cleanup_bad_record_does_not_abort_the_sweep'
ORPHAN = 'upload::tests::upload_cleanup_reclaims_expired_orphan_directories'
FRESH = 'upload::tests::upload_cleanup_keeps_fresh_orphan_directories'
LEASE_ATOMIC = 'upload::tests::upload_lease_atomic_rename_failure_keeps_valid_leases'
SYMLINK_ROOT = 'upload::tests::upload_cleanup_never_deletes_through_a_symlinked_root'
SWAP = 'upload::tests::upload_cleanup_does_not_follow_a_root_swapped_mid_round'
LEASE_SYMLINK = 'upload::tests::upload_lease_cleanup_never_deletes_through_a_symlinked_root'
LEASE_ENTRY = 'upload::tests::upload_lease_cleanup_never_deletes_through_an_entry_symlink'
SWAP_RECORD = 'upload::tests::upload_cleanup_record_removal_does_not_follow_a_root_swapped_mid_round'
RECORD_RM = 'upload::tests::upload_cleanup_record_removal_failure_does_not_stop_the_sweep'
LEASE_SWAP = 'upload::tests::upload_lease_cleanup_does_not_follow_a_root_swapped_mid_round'
LEASE_RELEASE = 'upload::tests::upload_lease_release_does_not_follow_a_swapped_leases_root'

# (name, source, original, mutant, named test, exclusive failure text)
# Every mutation is applied to the pristine original, never cumulatively.
mutations = [
    ('attachment-path-leak', GATEWAY, '''    let mut out = object([
        ("attachmentId", value(attachment, "attachmentId")),
        ("name", value(attachment, "name")),
        ("relativePath", value(attachment, "relativePath")),
    ]);''', '    let mut out = (*attachment).clone();', PATH_TEST,
     'attachment projection must drop the store path'),
    ('record-unknown-leak', GATEWAY, '    let mut out = JsObject::new();',
     '    let mut out = record.as_object().cloned().unwrap_or_default();', PROJECTION,
     'diskPath leaked:'),
    ('optional-empties-kept', GATEWAY, '        if !truthy(&field) {',
     '        if field.is_undefined() {', PROJECTION, '"errorCode" leaked:'),
    ('files-not-array', GATEWAY, '        if !matches!(files, JsValue::Array(_)) {',
     '        if false {', ORDER, 'files must be an array before the store'),
    ('project-key-not-trimmed', GATEWAY,
     '        JsValue::String(key) if trim_js(key).units().is_empty() => Err(required()),',
     '        JsValue::String(key) if key.units().is_empty() => Err(required()),', ORDER,
     'projectKey whitespace must be rejected before the provider'),
    ('project-key-nonstring-accepted', GATEWAY,
     '        // `input.projectKey?.trim()` boxes primitives; only nullish short-circuits.\n'
     '        _ => Err(request_failed("input.projectKey?.trim is not a function")),',
     '        // `input.projectKey?.trim()` boxes primitives; only nullish short-circuits.\n'
     '        _ => Ok(JsString::from("")),', ORDER,
     'non-string projectKey must be rejected before the provider'),
    ('upload-id-not-safe', GATEWAY, '    assert_id(&id)?;', '    let _ = &id;', ORDER,
     'uploadId safety must be checked before clientFileId'),
    ('client-file-id-not-trimmed', GATEWAY,
     '    if trim_js(raw).units().is_empty() {',
     '    if raw.units().is_empty() {', ORDER,
     'whitespace-only clientFileId must be rejected before base64'),
    ('base64-limit-off-by-one', GATEWAY,
     '    if decoded_len > MAX_REMOTE_UPLOAD_PART_BYTES {',
     '    if decoded_len > MAX_REMOTE_UPLOAD_PART_BYTES + 1 {', SIZE,
     '#865: 8 MiB + 1 must be rejected by the upload-part cap'),
    ('base64-precheck-removed', GATEWAY,
     '    if decoded_len > MAX_REMOTE_UPLOAD_PART_BYTES {',
     '    if false {', SIZE,
     '#865: 8 MiB + 1 must be rejected by the upload-part cap'),
    ('cap-after-decode', GATEWAY, '''    // Check the exact decoded size before either allocation: a 1 GiB spelling
    // must be rejected without first copying it into an encoded buffer.
    if decoded_len > MAX_REMOTE_UPLOAD_PART_BYTES {
        return Err(UploadError::dialog(
            "UPLOAD_PART_TOO_LARGE",
            format!("Remote upload parts are limited to {MAX_REMOTE_UPLOAD_PART_BYTES} bytes."),
        ));
    }
    // Shape validation proves the input is ASCII, so the byte view is exact.
    #[cfg(test)]
    DECODE_CALLS.with(|calls| calls.set(calls.get() + 1));
    let encoded: Vec<u8> = units.iter().map(|&unit| unit as u8).collect();
    let bytes = BASE64.decode(encoded).map_err(|_| invalid())?;''', '''    // Shape validation proves the input is ASCII, so the byte view is exact.
    #[cfg(test)]
    DECODE_CALLS.with(|calls| calls.set(calls.get() + 1));
    let encoded: Vec<u8> = units.iter().map(|&unit| unit as u8).collect();
    let bytes = BASE64.decode(encoded).map_err(|_| invalid())?;
    // Check the exact decoded size before either allocation: a 1 GiB spelling
    // must be rejected without first copying it into an encoded buffer.
    if decoded_len > MAX_REMOTE_UPLOAD_PART_BYTES {
        return Err(UploadError::dialog(
            "UPLOAD_PART_TOO_LARGE",
            format!("Remote upload parts are limited to {MAX_REMOTE_UPLOAD_PART_BYTES} bytes."),
        ));
    }''', SIZE,
     '#865: 8 MiB + 1 must be rejected before the encoded allocation and decode'),
    ('project-error-lossy', STORE, '''fn unknown_project_key(project_key: &JsString) -> UploadError {
    let mut message = "Unknown projectKey: ".encode_utf16().collect::<Vec<_>>();
    message.extend_from_slice(project_key.units());
    UploadError::dialog("PROJECT_NOT_FOUND", JsString::from_utf16(message))
}''', '''fn unknown_project_key(project_key: &JsString) -> UploadError {
    let mut message = "Unknown projectKey: ".encode_utf16().collect::<Vec<_>>();
    message.extend_from_slice(project_key.units());
    UploadError::dialog(
        "PROJECT_NOT_FOUND",
        JsString::from(String::from_utf16_lossy(&message)),
    )
}''', MESSAGES,
     '#865: PROJECT_NOT_FOUND must keep the raw UTF-16 projectKey (no from_utf16_lossy)'),
    ('part-error-lossy', STORE, '''fn part_message(prefix: &str, value: &JsString, suffix: &str) -> JsString {
    let mut message = prefix.encode_utf16().collect::<Vec<_>>();
    message.extend_from_slice(value.units());
    message.extend(suffix.encode_utf16());
    JsString::from_utf16(message)
}''', '''fn part_message(prefix: &str, value: &JsString, suffix: &str) -> JsString {
    let mut message = prefix.encode_utf16().collect::<Vec<_>>();
    message.extend_from_slice(value.units());
    message.extend(suffix.encode_utf16());
    JsString::from(String::from_utf16_lossy(&message))
}''', MESSAGES,
     '#865: UPLOAD_MANIFEST_MISMATCH must keep the raw UTF-16 clientFileId (no from_utf16_lossy)'),
    ('base64-trailing-bits-rejected', GATEWAY,
     '        .with_decode_allow_trailing_bits(true)',
     '        .with_decode_allow_trailing_bits(false)', ORDER,
     'non-zero trailing bits must stay accepted'),
    ('surrogate-key-hash-not-lossy', STORE,
     'Some(hash(String::from_utf16_lossy(key.units()).as_bytes()))',
     'Some(hash(key.to_utf8().unwrap_or_default().as_bytes()))',
     'upload::gateway_tests::upload_gateway_lone_surrogate_idempotency_key_hashes_like_node',
     'Node hashes a lone surrogate and U+FFFD to the same UTF-8 bytes'),
    ('idempotency-hash-skipped', STORE,
     '        let key_hash = match idempotency_key.filter(|key| truthy(key)) {',
     '        let key_hash = match None::<&JsValue> {', KEY_DEDUPE,
     "idempotencyKey must hash into the store's dedup path"),
    ('idempotency-type-before-manifest', GATEWAY,
     '        let record = provider.create(&project_key, files, idempotency_key)?;',
     '''        if let Some(key) = idempotency_key.filter(|key| key.as_string().is_none()) {
            return Err(UploadError::dialog(
                "ERR_INVALID_ARG_TYPE",
                crypto_received(key),
            ));
        }
        let record = provider.create(&project_key, files, idempotency_key)?;''', KEY_ORDER,
     'project validation must precede the idempotencyKey type'),
    ('number-format-expanded', GATEWAY,
     '''        JsValue::Number(number) => {
            format!("type number ({})", crate::js_value::number_string(*number))
        }''',
     '        JsValue::Number(number) => format!("type number ({number})"),', KEY_NUMBER,
     'Node spells 1e21 as 1e+21'),
    ('surrogate-lossy-bridge', GATEWAY,
     '        JsValue::String(key) => Ok(key.clone()),',
     '        JsValue::String(key) => Ok(JsString::from(String::from_utf16_lossy(key.units()))),',
     FIDELITY, 'lone surrogate must not be replaced before the provider'),
    ('null-attachment-swallowed', GATEWAY,
     '    if matches!(attachment, JsValue::Undefined | JsValue::Null) {',
     '    if matches!(attachment, JsValue::Undefined) {', NULL_ATTACH,
     'null attachment must keep the TS property-access error'),
    ('part-requires-provider-first', GATEWAY, '''        let upload_id = upload_id(params)?;
        let client_file_id = client_file_id(params)?;
        let content = parameter(params, "contentBase64")?;
        let bytes = decode_upload_part(content)?;
        let provider = self.require()?;''', '''        let provider = self.require()?;
        let upload_id = upload_id(params)?;
        let client_file_id = client_file_id(params)?;
        let content = parameter(params, "contentBase64")?;
        let bytes = decode_upload_part(content)?;''', ORDER,
     'part must validate params before requiring the provider'),
    ('get-requires-provider-after-upload-id', GATEWAY, '''        let provider = self.require()?;
        let upload_id = upload_id(params)?;
        gateway_record(&provider.get(&upload_id)?)''', '''        let upload_id = upload_id(params)?;
        let provider = self.require()?;
        gateway_record(&provider.get(&upload_id)?)''', ORDER,
     'get must require the provider before validating uploadId'),
    # #1494: the read_at handle binding. read-at-handle-check-dropped
    # removes it outright; read-at-check-before-open restores the pre-fix
    # shape — a check of the pre-open path spelling, which the canonicalize→
    # open swap already passed — so both must turn their named tests red.
    ('read-at-handle-check-dropped', STORE, '''                let handle_inside = match crate::handle_path::opened_path(&file) {
                    Ok(handle) => handle.starts_with(&upload_root),
                    Err(_) => false,
                };
                if !handle_inside {
                    return Err(UploadError::dialog(
                        "PROJECT_PATH_FORBIDDEN",
                        "Upload metadata handle leaves the upload directory.",
                    ));
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    let opened = match file.metadata() {
                        Ok(metadata) => metadata,
                        Err(e) => return Err(UploadError::io(e, "stat", None)),
                    };
                    if opened.dev() != approved.dev() || opened.ino() != approved.ino() {
                        return Err(UploadError::dialog(
                            "PROJECT_PATH_FORBIDDEN",
                            "Upload metadata handle names a different file than the one validated.",
                        ));
                    }
                }
''', '', READ_AT_SWAP,
     '#1494 the swapped-in outside record must never be read back'),
    # The genuine pre-fix shape is both missing guards at once: the handle
    # binding *and* the pre-open identity. Restoring only the spelling check
    # leaves the r2 identity term in place, which refuses the swapped record
    # with its own message — the named test then dies on a different
    # assertion, which is not evidence for this row (review boundary P1).
    ('read-at-check-before-open', STORE, '''                let handle_inside = match crate::handle_path::opened_path(&file) {
                    Ok(handle) => handle.starts_with(&upload_root),
                    Err(_) => false,
                };
                if !handle_inside {
                    return Err(UploadError::dialog(
                        "PROJECT_PATH_FORBIDDEN",
                        "Upload metadata handle leaves the upload directory.",
                    ));
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    let opened = match file.metadata() {
                        Ok(metadata) => metadata,
                        Err(e) => return Err(UploadError::io(e, "stat", None)),
                    };
                    if opened.dev() != approved.dev() || opened.ino() != approved.ino() {
                        return Err(UploadError::dialog(
                            "PROJECT_PATH_FORBIDDEN",
                            "Upload metadata handle names a different file than the one validated.",
                        ));
                    }
                }
''',
     '                let handle_inside = canonical_path.starts_with(&upload_root);',
     LEASE_CHAIN,
     '#1494 a forged record must never reach the attachment lease'),
    # #1494: the identity recorded before the open. Dropping only this term
    # leaves the name-based handle check intact, so the renamed-in outside
    # record passes it — the named test must go red.
    ('read-at-identity-dropped', STORE,
     '''                    if opened.dev() != approved.dev() || opened.ino() != approved.ino() {''',
     '                    if false {',
     READ_AT_RENAME,
     '#1494 a renamed-in outside record must never be read back'),
    # #1494: the approved identity must be sampled without following a link.
    # Restoring `fs::metadata` lets a link planted in the
    # `[canonicalize, approved stat]` window become the approved object; the
    # after-open rename then gives the outside record an in-root name.
    ('read-at-approved-stat-follows-link', STORE,
     '        let approved = match fs::symlink_metadata(&canonical_path) {',
     '        let approved = match fs::metadata(&canonical_path) {',
     READ_AT_PREAPPROVED,
     '#1494 a link planted before the approved stat must be refused before the open'),
    # #1494 (review sibling P1): the lease hard link re-walks the attachment
    # path, so the linked inode must be compared with the identity recorded on
    # the verified handle. Dropping that comparison pins whatever the path
    # names at link time — the named test must go red.
    ('lease-link-identity-dropped', STORE,
     '                if !lease_link_matches(&destination, identities.get(index).copied().flatten()) {',
     '                if false {',
     LEASE_LINK,
     '#1494 a swapped attachment must never be pinned into the lease'),
    # #1494 (review deepseek boundary#1): the comparison must read the entry
    # that was actually linked. `fs::metadata` follows a symlink source, so a
    # source swapped for a link to the approved inode compares equal while the
    # lease holds the *link* itself — `std::fs::hard_link` goes through
    # `linkat(..., 0)` and does not follow a symlink source on any shipping
    # Unix (macOS measured 2026-11-06). The named test swaps the source for
    # exactly that link and must go red.
    ('lease-link-follows-symlink-source', STORE,
     '    match (fs::symlink_metadata(destination), expected) {',
     '    match (fs::metadata(destination), expected) {',
     LEASE_LINK_SYMLINK,
     '#1494 the lease entry must be a regular file, never a link'),
    # #1646: sweep robustness — each mutation restores one TS-parity weakness.
    ('cleanup-bad-record-aborts', STORE, '''                Ok(None) => {
                    self.reclaim_orphan(&root_real, &dir);
                    continue;
                }
                Err(e) => {
                    self.skip_record(&dir, &e);
                    continue;
                }''', '''                Ok(None) => {
                    self.reclaim_orphan(&root_real, &dir);
                    continue;
                }
                Err(e) => {
                    return;
                }''', SWEEP,
     'bad record aborts the sweep'),
    ('cleanup-orphan-reclaim-removed', STORE, '''                Ok(None) => {
                    self.reclaim_orphan(&root_real, &dir);
                    continue;
                }''', '''                Ok(None) => {
                    continue;
                }''', ORPHAN,
     'an orphan past the upload retention is reclaimed'),
    ('cleanup-orphan-fresh-deleted', STORE,
     '        if since_epoch.as_millis() as i64 >= (self.options.now)() - self.options.retention_ms {',
     '        if false {', FRESH,
     'a just-created orphan must survive its metadata write window'),
    ('lease-write-in-place', STORE, '''            let path = dir.join("lease.json");
            // Like `write` (metadata.json): the lease file only ever appears
            // complete, so a reader never parses a half-written lease. The
            // window is narrower than "a live lease is never deleted": the
            // reader treats a *missing* lease.json as expired (TS parity), and
            // the directory exists from `make_dir` until the rename (#1646).
            // TS:404–413 writes `lease.json` in place instead.
            let temp = path.with_file_name(format!(
                "lease.json.{}.{}.tmp",
                std::process::id(),
                random_id()?
            ));
            let mut file = open_new(&temp)?;
            self.fault("lease-write", &temp)?;
            file.write_all(json_line(&expiry)?.as_bytes())
                .map_err(|e| UploadError::io(e, "write", None))?;
            file.sync_all()
                .map_err(|e| UploadError::io(e, "fsync", None))?;
            drop(file);
            self.fault("lease-rename", &temp)?;
            fs::rename(&temp, &path).map_err(|e| UploadError::io(e, "rename", Some(&temp)))?;''', '''            let path = dir.join("lease.json");
            let mut file = open_new(&path)?;
            self.fault("lease-write", &path)?;
            file.write_all(json_line(&expiry)?.as_bytes())
                .map_err(|e| UploadError::io(e, "write", None))?;
            file.sync_all()
                .map_err(|e| UploadError::io(e, "fsync", None))?;''', LEASE_ATOMIC,
     'lease.json must appear only through an atomic rename'),
    # #1646 review: the two guards added after the first round.
    # The gate and the "resolves to exactly itself" check now live in the same
    # `plain_root` call, so replacing the match removes both at once — the
    # mutant really does sweep through the symlink, instead of being stopped by
    # the second line of defence and failing on a logging assertion instead.
    ('cleanup-symlink-root-swept', STORE, '''        let root_real = match plain_root(project, "chat-uploads") {
            PlainRoot::Plain(root) => root,
            // No uploads yet: the ordinary empty state, as silent as the old
            // `names()` on a missing directory.
            PlainRoot::Missing => return,
            PlainRoot::Blocked => {
                self.log_cleanup(format!(
                    "[uploads] cleanup skipped {}: uploads root is not a plain directory",
                    project.join(".tmp/chat-uploads").display()
                ));
                return;
            }
        };''', '''        let root_real =
            fs::canonicalize(project.join(".tmp/chat-uploads")).unwrap_or_default();''', SYMLINK_ROOT,
     'a symlinked uploads root must not be swept'),
    ('cleanup-record-rm-aborts', STORE, '''            match self
                .fault("record-rm", &dir)
                .and_then(|()| remove_tree(&dir))
            {
                Ok(()) => *count += 1,
                Err(e) => self.skip_record(&dir, &e),
            }''', '''            match self
                .fault("record-rm", &dir)
                .and_then(|()| remove_tree(&dir))
            {
                Ok(()) => *count += 1,
                Err(_) => return,
            }''', RECORD_RM,
     'a failed record removal does not stop the sweep'),
    # #1646 review r2: the plain-root gate only covers the round start; the
    # entry must be measured against the root snapshotted when the round began.
    ('cleanup-orphan-root-recanonicalized', STORE, '''        if !inside_root(root_real, dir) {
            return;
        }''', '''        let root_real = fs::canonicalize(root_real).unwrap_or_default();
        if !inside_root(&root_real, dir) {
            return;
        }''', SWAP,
     'a root swapped mid-round must not be followed'),
    ('cleanup-lease-symlink-root-swept', STORE, '''        let root_real = match plain_root(project, "chat-upload-leases") {
            PlainRoot::Plain(root) => root,
            PlainRoot::Missing => return Ok(()),
            PlainRoot::Blocked => {
                self.log_cleanup(format!(
                    "[uploads] lease cleanup skipped {}: leases root is not a plain directory",
                    project.join(".tmp/chat-upload-leases").display()
                ));
                return Ok(());
            }
        };''', '''        let root_real =
            fs::canonicalize(project.join(".tmp/chat-upload-leases")).unwrap_or_default();''', LEASE_SYMLINK,
     'a symlinked leases root must not be swept'),
    ('cleanup-record-rm-follows-swapped-root', STORE, '''            if !inside_root(&root_real, &dir) {
                self.log_cleanup(format!(
                    "[uploads] cleanup skipped {}: outside the uploads root",
                    dir.display()
                ));
                continue;
            }
''', '', SWAP_RECORD,
     'a record delete must not follow a root swapped mid-round'),
    ('cleanup-lease-entry-symlink-swept', STORE, '''                if !inside_root(&root_real, &dir) {
                    self.log_cleanup(format!(
                        "[uploads] lease cleanup skipped {}: outside the leases root",
                        dir.display()
                    ));
                    continue;
                }
''', '', LEASE_ENTRY,
     'a symlinked lease entry must not be swept'),
    # #1646 review r3: the record half's "do not re-resolve the root per entry"
    # property had two mutations; the lease half had none.
    ('cleanup-lease-root-recanonicalized', STORE, '''                if !inside_root(&root_real, &dir) {
                    self.log_cleanup(format!(
                        "[uploads] lease cleanup skipped {}: outside the leases root",
''', '''                let root_real = fs::canonicalize(&root_real).unwrap_or_default();
                if !inside_root(&root_real, &dir) {
                    self.log_cleanup(format!(
                        "[uploads] lease cleanup skipped {}: outside the leases root",
''', LEASE_SWAP,
     'a lease delete must not follow a root swapped mid-round'),
    # #1646 review r3: `release` is the third delete point on the leases tree and
    # fires long after acquire; it must not follow a swapped root either.
    ('lease-release-symlink-root-swept', STORE, '''                if root_real != root || !inside_root(&root_real, &self.dir) {
                    return Ok(());
                }
''', '', LEASE_RELEASE,
     'a lease release must not follow a swapped leases root'),
]

if '--check' in sys.argv[1:]:
    for name, source, old, _new, _test, _expected in mutations:
        original = source.read_text()
        assert original.count(old) == 1, (name, original.count(old))
    print(f'PASS: {len(mutations)} mutation anchors match their sources')
    sys.exit(0)

out = Path(sys.argv[1]).resolve()
out.mkdir(parents=True, exist_ok=False)
selected = sys.argv[2:]
if selected:
    mutations = [mutation for mutation in mutations if mutation[0] in selected]
    assert len(mutations) == len(selected), 'unknown mutation name'
env = os.environ.copy()
assert env.get('CARGO_TARGET_DIR'), 'set CARGO_TARGET_DIR to the caller build directory'
summary = []
baseline = os.environ.get('PDRS_NEG_BASELINE', 'HEAD')
with mutating_sources(root, [GATEWAY, STORE], baseline=baseline) as snapshots:
    for name, source, old, new, test, expected in mutations:
        original = snapshots[source]
        before = hashlib.sha256(original).hexdigest()
        content = original.decode()
        assert content.count(old) == 1, (name, content.count(old))
        changed = content.replace(old, new, 1)
        (out / f'{name}.patch').write_text(''.join(difflib.unified_diff(
            content.splitlines(True), changed.splitlines(True),
            fromfile=source.name, tofile=source.name)))
        try:
            wait_for_slot(root / 'scripts/cargo-slots.sh')
            source.write_text(changed)
            result = run_owned(
                ['cargo', 'test', '--manifest-path', 'src/Cargo.toml',
                 '-p', 'pilotdeck-gateway', '--lib', test, '--', '--exact'],
                cwd=root, env=env, timeout=900)
            log = result.stdout + result.stderr
            (out / f'{name}.log').write_text(log)
            killed = (result.returncode == 101
                      and f'{test} ... FAILED' in log
                      and expected in log
                      and 'panicked at' in log
                      and 'error[E' not in log)
            failure = next((line.strip() for line in log.splitlines()
                            if 'assertion' in line or 'panicked at' in line
                            or expected in line), '')
            summary.append(dict(name=name, source=str(source.relative_to(root)),
                                test=test, expected=expected, killed=killed,
                                returncode=result.returncode, failure=failure,
                                sourceSha256=before))
            print(f'{name}: {"KILLED" if killed else "NOT KILLED"} rc={result.returncode} {failure}', flush=True)
            assert killed, f'{name}: did not hit its named assertion ({expected!r})'
        finally:
            source.write_bytes(original)
            assert hashlib.sha256(source.read_bytes()).hexdigest() == before
            (out / 'summary.json').write_text(json.dumps(summary, ensure_ascii=False, indent=2) + '\n')
print(f'{len(summary)}/{len(mutations)} killed; every source restored byte-for-byte', flush=True)
