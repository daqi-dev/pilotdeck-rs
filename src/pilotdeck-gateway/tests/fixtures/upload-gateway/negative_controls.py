#!/usr/bin/env python3
"""Temporary upload-gateway product mutations; each must turn a named test red.

The product files already exist in HEAD, but ``mutating_sources`` also refuses
while the fix-r2 patch is uncommitted (the working tree must match HEAD). Run
this after the coordinator commits the branch:

    CARGO_TARGET_DIR=<caller-owned cargo target directory> \
      python3 src/pilotdeck-gateway/tests/fixtures/upload-gateway/negative_controls.py <out-dir>

For a pre-commit rehearsal only, set ``PDRS_NEGATIVE_BASELINE`` to a tree object with
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

The lease-link rows pin the one descriptor the lease entry is checked
through: four drop the identity comparison, its content term, the
end-of-entry length term, or the non-blocking half of the open, and one drops
`O_NOFOLLOW` from that open, which lets a link planted in the entry be
followed again. All four are
killable on every Unix: ``std::fs::hard_link`` does not follow a symlink
source (``linkat(..., 0)`` on macOS as well as Linux), so the lease entry is
the link itself and a follow-the-link sample accepts it. Measured 2026-11-06:
after ``fs::hard_link(link, dest)``, ``fs::symlink_metadata(dest)`` reports a
symlink; Python's ``os.link`` is the one that follows, which is why the
fixture does not use it. The non-blocking row is measured 2026-11-07
(``_tmp/20261107-issue-1887/lease-open-probe/``): a plain open of a FIFO
never returns, and without ``O_NONBLOCK`` the named test's watchdog is what
turns that hang into a red test.

The #2098 rows pin the guarded open's three siblings: the lease entry's
helper became the family's, so `verified` and `read_at` each have their own
FIFO, symlink and length rows — the flag rows edit the same shared
``custom_flags`` line as the lease rows, but each is bound to its own site's
named test, so the family cannot lose a flag while any one caller is tested.
The `verify-hash-length-dropped` row restores the read-to-EOF digest (the
pre-fix shape) and `read-at-metadata-cap-dropped` drops the record-leaf size
cap.

The #1887/#1888 rows pin the two halves of the shared "home" question: the
reader's lexical prefix check refuses a `.tmp` that is a link into the same
project, and the writer's lease root goes through the same guard instead of a
bare join off `projectKey`. Their composer-side siblings live in
``tests/fixtures/attach-followups-a/negative_controls.py``; the assertion
strings name the side, so no row can be satisfied by the other side's test.
"""
import difflib
import hashlib
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_selection import claim_dir, missing_target, refuse, refuse_unusable_target, registered, select, verdict_exit
from _negkit import non_head

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
LEASE_LINK_REWRITE = 'upload::tests::upload_lease_link_of_a_rewritten_attachment_is_rejected'
LEASE_LINK_FIFO = ('upload::tests::upload_lease_link_of_a_swapped_entry_'
                  'is_not_read_through_the_path')
LEASE_LINK_GROWN = 'upload::tests::upload_lease_link_of_a_grown_entry_is_rejected'
# #2098: the lease entry's guarded open became the family's — `verified` and
# `read_at` open through the same `open_artifact_guarded` helper. Each site
# has its own row and its own named test, so a flag dropped from the one
# shared line is caught by the site it protects, not only by the lease.
VERIFY_FIFO = ('upload::tests::upload_verify_attachment_of_a_fifo_leaf_'
               'is_rejected_within_the_watchdog')
VERIFY_SYMLINK = 'upload::tests::upload_verify_attachment_never_follows_a_symlink_leaf'
VERIFY_GROWN = 'upload::tests::upload_verify_attachment_of_a_grown_leaf_is_rejected'
READ_AT_FIFO = ('upload::tests::upload_read_at_metadata_swapped_for_a_fifo_'
                'is_rejected_within_the_watchdog')
READ_AT_ZERO = ('upload::tests::upload_read_at_metadata_swapped_for_a_dev_zero_symlink_'
                'is_rejected_at_open')
READ_AT_CAP = 'upload::tests::upload_read_at_metadata_over_the_size_cap_is_rejected'
LEASE_ROOT_LINK = 'upload::tests::upload_lease_root_link_is_refused'
READ_AT_LINKED_ANCESTOR = 'upload::tests::upload_read_at_rejects_a_linked_tmp_ancestor'
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
# fix-r1 (#2423): the lease entry gate is the `canonical_home` component walk;
# its downgrade mutant is killed by the deterministic in-root alias test.
LEASE_ALIAS = (
    'upload::tests::upload_lease_cleanup_does_not_sweep_through_an_aliased_id_dir')
LEASE_RELEASE = 'upload::tests::upload_lease_release_does_not_follow_a_swapped_leases_root'
PLAIN_ROOT = 'upload::tests::plain_root_rejects_a_root_that_resolves_elsewhere'
ORPHAN_RM = 'upload::tests::upload_cleanup_orphan_removal_failure_does_not_stop_the_sweep'
SCHEDULE = 'upload::tests::cleanup_failure_keeps_the_schedule_alive'
# #1850: the cancel delete gate, the emptied `<id>` removal guard, and the
# release `realpath` branches.
CANCEL_ROOT_SWAP = (
    'upload::tests::upload_cancel_does_not_follow_a_root_swapped_mid_mutate')
CANCEL_TASK_SWAP = (
    'upload::tests::upload_cancel_does_not_follow_a_task_dir_swapped_mid_mutate')
CANCEL_ALIAS = (
    'upload::tests::upload_cancel_does_not_delete_through_an_aliased_task_dir')
LEASE_ID_SWAP = (
    'upload::tests::upload_lease_cleanup_does_not_remove_an_id_dir_swapped_mid_round')
RELEASE_REALPATH = (
    'upload::tests::upload_lease_release_surfaces_and_caches_a_realpath_failure')
# #2276: the #1631 pair regressions live in the `tests/` integration target,
# not the lib — the runner picks `--test <target>` for a named test by this
# registry (the roster names the target; it is never guessed from the test
# name), and everything not in it keeps the `--lib` face. Fix-r1 (review,
# MM boundary#6): every `#[test]` of the target is registered, so a future
# row that names any of them cannot silently fall to the `--lib` face.
INTEGRATION_TESTS = {
    'interleaved_complete_and_cancel_cannot_both_win': 'upload_concurrency_1631',
    'interleaved_complete_and_complete_has_one_winner': 'upload_concurrency_1631',
    'interleaved_cancel_and_cancel_wins_exactly_once': 'upload_concurrency_1631',
    # fix-r2 (DS r2 boundary#1): registered for the same reason as the three
    # above — the deadline test is a `#[test]` of this target, and the
    # every-test-registered invariant above must stay true.
    'rendezvous_deadline_panics_when_the_pair_never_meets': 'upload_concurrency_1631',
}

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
    # #2098 re-anchor note: the binding region gained the descriptor type
    # term between the name check and the identity check; both mutants keep
    # that term (it is #2098's, not the #1494 binding) and drop exactly the
    # halves these rows have always dropped.
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
                // #2098: the open is the guarded family, so the terms that
                // make it safe run on the one descriptor it returned. The
                // type term refuses a `metadata.json` swapped for a FIFO
                // (the `O_NONBLOCK` open returned at once) or a device, and
                // the length terms below refuse the sparse-file and
                // same-inode-append faces of the unbounded read.
                let opened = match file.metadata() {
                    Ok(metadata) => metadata,
                    Err(e) => return Err(UploadError::io(e, "stat", None)),
                };
                if !opened.file_type().is_file() && !opened.file_type().is_dir() {
                    // A directory keeps its Node read-stage shape: the read
                    // below fails at once with `EISDIR … read` (#656,
                    // `upload_metadata_directory_reports_the_read_stage`),
                    // so it is neither the FIFO's wait nor the device's
                    // unbounded stream this term exists for.
                    return Err(UploadError::dialog(
                        "PROJECT_PATH_FORBIDDEN",
                        "Upload metadata path became a non-regular file after validation.",
                    ));
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    if opened.dev() != approved.dev() || opened.ino() != approved.ino() {
                        return Err(UploadError::dialog(
                            "PROJECT_PATH_FORBIDDEN",
                            "Upload metadata handle names a different file than the one validated.",
                        ));
                    }
                }
''', '''                let opened = match file.metadata() {
                    Ok(metadata) => metadata,
                    Err(e) => return Err(UploadError::io(e, "stat", None)),
                };
                if !opened.file_type().is_file() && !opened.file_type().is_dir() {
                    // A directory keeps its Node read-stage shape: the read
                    // below fails at once with `EISDIR … read` (#656,
                    // `upload_metadata_directory_reports_the_read_stage`),
                    // so it is neither the FIFO's wait nor the device's
                    // unbounded stream this term exists for.
                    return Err(UploadError::dialog(
                        "PROJECT_PATH_FORBIDDEN",
                        "Upload metadata path became a non-regular file after validation.",
                    ));
                }
''', READ_AT_SWAP,
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
                // #2098: the open is the guarded family, so the terms that
                // make it safe run on the one descriptor it returned. The
                // type term refuses a `metadata.json` swapped for a FIFO
                // (the `O_NONBLOCK` open returned at once) or a device, and
                // the length terms below refuse the sparse-file and
                // same-inode-append faces of the unbounded read.
                let opened = match file.metadata() {
                    Ok(metadata) => metadata,
                    Err(e) => return Err(UploadError::io(e, "stat", None)),
                };
                if !opened.file_type().is_file() && !opened.file_type().is_dir() {
                    // A directory keeps its Node read-stage shape: the read
                    // below fails at once with `EISDIR … read` (#656,
                    // `upload_metadata_directory_reports_the_read_stage`),
                    // so it is neither the FIFO's wait nor the device's
                    // unbounded stream this term exists for.
                    return Err(UploadError::dialog(
                        "PROJECT_PATH_FORBIDDEN",
                        "Upload metadata path became a non-regular file after validation.",
                    ));
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    if opened.dev() != approved.dev() || opened.ino() != approved.ino() {
                        return Err(UploadError::dialog(
                            "PROJECT_PATH_FORBIDDEN",
                            "Upload metadata handle names a different file than the one validated.",
                        ));
                    }
                }
''',
     '''                let opened = match file.metadata() {
                    Ok(metadata) => metadata,
                    Err(e) => return Err(UploadError::io(e, "stat", None)),
                };
                if !opened.file_type().is_file() && !opened.file_type().is_dir() {
                    // A directory keeps its Node read-stage shape: the read
                    // below fails at once with `EISDIR … read` (#656,
                    // `upload_metadata_directory_reports_the_read_stage`),
                    // so it is neither the FIFO's wait nor the device's
                    // unbounded stream this term exists for.
                    return Err(UploadError::dialog(
                        "PROJECT_PATH_FORBIDDEN",
                        "Upload metadata path became a non-regular file after validation.",
                    ));
                }
                let handle_inside = canonical_path.starts_with(&upload_root);
''',
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
     '''                if !lease_link_matches(
                    &destination,
                    identities.get(index).copied().flatten(),
                    &string(attachment, "sha256").unwrap_or_default(),
                    number(attachment, "bytes"),
                ) {''',
     '                if false {',
     LEASE_LINK,
     '#1494 a swapped attachment must never be pinned into the lease'),
    # #1494 (review deepseek boundary#1) / #1889 (review concurrency#1): the
    # comparison must read the entry that was actually linked, and it must read
    # it through the descriptor it fstat'ed. `O_NOFOLLOW` is that second half:
    # a source swapped for a link to the approved inode compares equal on
    # identity and bytes while the lease holds the *link* itself —
    # `std::fs::hard_link` goes through `linkat(..., 0)` and does not follow a
    # symlink source on any shipping Unix (macOS measured 2026-11-06). The
    # named test swaps the source for exactly that link and must go red.
    ('lease-link-follows-symlink-source', STORE,
     '        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);',
     '        .custom_flags(libc::O_NONBLOCK);',
     LEASE_LINK_SYMLINK,
     '#1494 the lease entry must be a regular file, never a link'),
    # #1889: `(dev, inode)` does not survive an inode-reusing replacement on
    # ext4 (measured 2026-11-07: 20000/20000 rounds reuse the freed number),
    # so the lease link also compares the linked entry's content with the
    # digest `verified` bound to its handle. This row drops the content term
    # and leaves the identity term intact: the named test rewrites the file in
    # place inside the `[verified, hard link]` window (same inode, same
    # length), so it is killable on every filesystem, not only where inode
    # reuse happens.
    ('lease-link-content-dropped', STORE,
     '    hash_file_exact(file, expected_bytes as u64).is_ok_and(|digest| digest == expected_sha256)',
     '''    let _ = (file, expected_sha256, expected_bytes);
    true''',
     LEASE_LINK_REWRITE,
     '#1889 a rewritten artifact must never be pinned into the lease'),
    # #1889 (review concurrency#1, measured 2026-11-07): the lease entry is
    # opened `O_NOFOLLOW | O_NONBLOCK`, so a FIFO swapped into it returns at
    # once instead of blocking the acquire on a writer that never comes. This
    # row drops only the non-blocking half; the named test's FIFO open then
    # never returns and its watchdog trips.
    ('lease-link-nonblock-dropped', STORE,
     '        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);',
     '        .custom_flags(libc::O_NOFOLLOW);',
     LEASE_LINK_FIFO,
     '#1889 a FIFO entry must never make the lease acquire wait'),
    # #1889 (review concurrency#1 / boundary#1): the content term reads exactly
    # the record's `bytes` and requires the entry to end there, so an approved
    # inode that keeps growing cannot turn the read into an unbounded stream.
    # This row drops the end-of-entry term; the named test appends to the
    # approved inode (same inode, same leading bytes) and must go red.
    ('lease-link-length-dropped', STORE,
     '    if trailing != 0 {',
     '    if trailing != 0 && false {',
     LEASE_LINK_GROWN,
     '#1889 an entry that grew after verification must never be pinned into the lease'),
    # #2098 (S1, `verified`): the FIFO and symlink rows drop one half of the
    # shared guarded open exactly like the lease rows above, but their named
    # tests are the verification-site ones, so the shared helper cannot lose
    # a flag as long as any one of its callers is tested. The hash row is
    # the read-to-EOF pre-fix shape: without the length terms (the fstat
    # length and the must-end-there read) the digest follows a same-inode
    # append to whatever EOF the writer allows.
    ('verify-open-nonblock-dropped', STORE,
     '        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);',
     '        .custom_flags(libc::O_NOFOLLOW);',
     VERIFY_FIFO,
     '#2098 a FIFO leaf must never make verification wait for a writer'),
    ('verify-open-nofollow-dropped', STORE,
     '        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);',
     '        .custom_flags(libc::O_NONBLOCK);',
     VERIFY_SYMLINK,
     '#2098 a symlink leaf must never pass attachment verification'),
    ('verify-hash-length-dropped', STORE,
     '''            if (!info.file_type().is_file() && !info.file_type().is_dir())
                || info.len() as f64 != expected_bytes
                || hash_file_exact(&mut file, expected_bytes as u64)?
                    != string(attachment, "sha256").unwrap_or_default()
            {
                return Err(attachment_integrity_failed(attachment));
            }''',
     '''            let _ = expected_bytes;
            let mut all = Vec::new();
            file.read_to_end(&mut all)
                .map_err(|e| UploadError::io(e, "read", None))?;
            if hash(&all) != string(attachment, "sha256").unwrap_or_default() {
                return Err(attachment_integrity_failed(attachment));
            }''',
     VERIFY_GROWN,
     '#2098 a grown leaf must never pass attachment verification'),
    # #2098 (S2, `read_at`): the same two flag rows for the record leaf's
    # open, plus the size cap. The cap row `if false`-es the one comparison;
    # the named test extends the leaf past the cap in place, so only that
    # term can refuse the sparse read.
    ('read-at-open-nonblock-dropped', STORE,
     '        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);',
     '        .custom_flags(libc::O_NOFOLLOW);',
     READ_AT_FIFO,
     '#2098 a FIFO record must never make the read wait for a writer'),
    ('read-at-open-nofollow-dropped', STORE,
     '        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);',
     '        .custom_flags(libc::O_NONBLOCK);',
     READ_AT_ZERO,
     '#2098 the guarded open must refuse the swapped link at the open itself'),
    ('read-at-metadata-cap-dropped', STORE,
     '                if opened.len() > MAX_METADATA_JSON_BYTES {',
     '                if false {',
     READ_AT_CAP,
     '#2098 a sparse record leaf past the cap must be refused before the read'),
    # #1888: the writer's lease root goes through the same home guard as the
    # reader's. This row restores the pre-fix shape — a bare join off
    # `projectKey`, created through whatever is at that spelling — so a lease
    # root that is a link into the project takes the write and the paired
    # write-side test goes red. The read-side sibling is
    # `read-at-upload-root-prefix-dropped`; each assertion names its side.
    ('lease-root-link-refused', STORE,
     '''        let dir = prepare_lease_root(&project)?
            .join(upload_id)
            .join(random_id()?);''',
     '''        let dir = project
            .join(".tmp")
            .join("chat-upload-leases")
            .join(upload_id)
            .join(random_id()?);''',
     LEASE_ROOT_LINK,
     '#1888 the writer must not create through a linked lease root'),
    # #1887: `read_at` holds a record to the upload root by lexical spelling,
    # so a `.tmp` that is a link to another subtree of the same project is
    # refused there. Dropping that prefix check leaves only the handle check,
    # which refuses the same layout with a *different* message — the named
    # test pins the read-side answer, and its composer-side sibling
    # (`attach-followups-a`: `lease-ancestor-link-accepted`) pins the other.
    ('read-at-upload-root-prefix-dropped', STORE,
     '        if !canonical_path.starts_with(&upload_root) {',
     '        if false {',
     READ_AT_LINKED_ANCESTOR,
     '#1887 the read side must keep its lexical prefix check'),
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
    ('cleanup-lease-entry-symlink-swept', STORE, '''                if canonical_home(&dir, &root_real, &root_real).is_none() {
                    self.log_cleanup(format!(
                        "[uploads] lease cleanup skipped {}: outside the leases root or a linked lease dir",
                        dir.display()
                    ));
                    continue;
                }
''', '', LEASE_ENTRY,
     'a symlinked lease entry must not be swept'),
    # #1646 review r3: the record half's "do not re-resolve the root per entry"
    # property had two mutations; the lease half had none. fix-r1 (#2423) moved
    # the gate to the `canonical_home` walk, so the snapshot property now lives
    # in the containment anchor: the walk's own stats go through a swapped
    # `.tmp` ancestor and find real dirs, only the round-start anchor refuses.
    ('cleanup-lease-root-recanonicalized', STORE,
     '''                if canonical_home(&dir, &root_real, &root_real).is_none() {''',
     '''                if canonical_home(
                    &dir,
                    &root_real,
                    &fs::canonicalize(&root_real).unwrap_or_default(),
                )
                .is_none()
                {''', LEASE_SWAP,
     'a lease delete must not follow a root swapped mid-round'),
    # fix-r1 (#2423 deepseek r1 concurrency#1，仲裁实测坐实): downgrading the
    # entry gate back to the containment-only comparison re-opens the
    # deterministic alias — a linked `<id>` whose resolution stays inside the
    # root lists the victim's inner files as "leases", whose lease.json read
    # ENOTDIRs into expired orphans.
    ('lease-alias-walk-downgraded', STORE,
     '''                if canonical_home(&dir, &root_real, &root_real).is_none() {''',
     '''                if !inside_root(&root_real, &dir) {''', LEASE_ALIAS,
     "an aliased id dir must not put an active lease's files on the sweep"),
    # #1646 review r3: `release` is the third delete point on the leases tree and
    # fires long after acquire; it must not follow a swapped root either.
    ('lease-release-symlink-root-swept', STORE, '''                if root_real != root {
                    return Ok(());
                }
                match resolved_inside(&root_real, &self.dir) {
                    Ok(Some(_)) => remove_tree(&self.dir),
                    Ok(None) => Ok(()),
                    Err(e) => Err(UploadError::io(e, "realpath", Some(&self.dir))),
                }
''', '''                remove_tree(&self.dir)
''', LEASE_RELEASE,
     'a lease release must not follow a swapped leases root'),
    # #1646 review r4/r5: the gate is an equality test, not "resolve and
    # accept" — a root that stats as a directory but resolves to a different
    # literal path must be refused. The mutant is the pre-r4 shape; a
    # `starts_with` mutant would NOT be killed here (the symlinked-root cells
    # are already refused by the leading stats), so this is the shape the test
    # actually has teeth against.
    ('plain-root-accepts-any-resolve', STORE, '''    match fs::canonicalize(&root) {
        Ok(real) if real == root => PlainRoot::Plain(real),
        _ => PlainRoot::Blocked,
    }''', '''    match fs::canonicalize(&root) {
        Ok(real) => PlainRoot::Plain(real),
        _ => PlainRoot::Blocked,
    }''', PLAIN_ROOT,
     'a root whose resolve lands on a different literal path must be blocked'),
    # #1646 review r5: the orphan branch's failure arm had no injection point
    # and no teeth; swallowing its error kept every test green.
    ('cleanup-orphan-rm-silent', STORE, '''        match self.fault("orphan-rm", dir).and_then(|()| remove_tree(dir)) {
            Ok(()) => self.log_cleanup(format!(
                "[uploads] cleanup reclaimed orphan: {}",
                dir.display()
            )),
            Err(e) => self.skip_record(dir, &e),
        }''', '''        match self.fault("orphan-rm", dir).and_then(|()| remove_tree(dir)) {
            Ok(()) => self.log_cleanup(format!(
                "[uploads] cleanup reclaimed orphan: {}",
                dir.display()
            )),
            Err(_) => {}
        }''', ORPHAN_RM,
     'a failed orphan removal is logged with its path'),
    # #1646 review r5: the rewritten schedule test had no mutation; a loop that
    # ends the schedule after one failed sweep is the exact regression it is
    # supposed to catch.
    ('cleanup-schedule-stop-on-failure', STORE, '''            if let Some(error) = failure {
                // Same reason as `log_cleanup`: `eprintln!` panics when
                // stderr is unwritable, and that panic would end this
                // detached loop — the one failure the schedule must survive.
                let _ = writeln!(io::stderr(), "[uploads] cleanup failed: {error}");
            }''', '''            if let Some(error) = failure {
                // Same reason as `log_cleanup`: `eprintln!` panics when
                // stderr is unwritable, and that panic would end this
                // detached loop — the one failure the schedule must survive.
                let _ = writeln!(io::stderr(), "[uploads] cleanup failed: {error}");
                return;
            }''', SCHEDULE,
     'the next tick sweeps again after the failed one'),
    # #1850: the cancel delete is the fourth removal point on the `chat-uploads`
    # tree; the mutant is its pre-#1850 shape — the bare `remove_tree` after
    # the in-gate transition, which follows a `.tmp` swapped during cancel's
    # record read / mutate rewrite window.
    ('cancel-remove-tree-ungated', STORE, '''        if cancelled {
            let project = PathBuf::from(string(&updated, "projectKey").unwrap_or_default());
            let dir = task_dir(&updated).join("files");
            let root_real = match plain_root(&project, "chat-uploads") {
                PlainRoot::Plain(root) => root,
                // The record was just read from this tree, so `Missing` means
                // the whole task directory is gone: the same quiet no-op the
                // bare `remove_tree` gave.
                PlainRoot::Missing => return Ok(updated),
                PlainRoot::Blocked => {
                    self.log_cleanup(format!(
                        "[uploads] cancel skipped {}: uploads root is not a plain directory",
                        project.join(".tmp/chat-uploads").display()
                    ));
                    return Ok(updated);
                }
            };
            // fix-r1 (minimax concurrency#2 arbitration): `inside_root` alone
            // passes a linked `<id>` whose resolution stays inside the root —
            // a deterministic alias onto another upload. The component walk
            // `canonical_home` uses for the lease/store roots refuses every
            // link component down to `files`; the residual between the walk
            // and `remove_tree` is the instruction-level TOCTOU family the
            // A8 ledger registers.
            if canonical_home(&dir, &project, &root_real).is_none() {
                self.log_cleanup(format!(
                    "[uploads] cancel skipped {}: outside the uploads root or a linked task dir",
                    dir.display()
                ));
                return Ok(updated);
            }
            remove_tree(&dir)?;
        }''', '''        if cancelled {
            remove_tree(&task_dir(&updated).join("files"))?;
        }''', CANCEL_ROOT_SWAP,
     'a cancel delete must not follow a root swapped mid-mutate'),
    # #1850 + fix-r1: the plain-root gate alone is not enough — the record's
    # task directory itself can be the swapped component, so the delete needs
    # the per-entry gate; the row drops the whole gate block.
    ('cancel-delete-gate-dropped', STORE, '''            // fix-r1 (minimax concurrency#2 arbitration): `inside_root` alone
            // passes a linked `<id>` whose resolution stays inside the root —
            // a deterministic alias onto another upload. The component walk
            // `canonical_home` uses for the lease/store roots refuses every
            // link component down to `files`; the residual between the walk
            // and `remove_tree` is the instruction-level TOCTOU family the
            // A8 ledger registers.
            if canonical_home(&dir, &project, &root_real).is_none() {
                self.log_cleanup(format!(
                    "[uploads] cancel skipped {}: outside the uploads root or a linked task dir",
                    dir.display()
                ));
                return Ok(updated);
            }
''', '', CANCEL_TASK_SWAP,
     'a cancel delete must not follow a task dir swapped mid-mutate'),
    # fix-r1 (minimax concurrency#2 arbitration): downgrading the component
    # walk back to the containment-only comparison re-opens the deterministic
    # cross-upload alias — the resolved path is inside the root, so only the
    # link-component term refuses the delete.
    ('cancel-alias-walk-downgraded', STORE,
     '''            if canonical_home(&dir, &project, &root_real).is_none() {''',
     '''            if !inside_root(&root_real, &dir) {''', CANCEL_ALIAS,
     "a cancel delete must not remove another upload's files through an aliased task dir"),
    # #1850: the emptied `<id>` directory at the end of a lease round. The
    # entry sweep skips a linked `<id>` entirely, so `names` reads through the
    # link and finds the outside target empty; without the `inside_root` term
    # the parent removal unlinks the symlinked `<id>` itself.
    ('cleanup-lease-id-guard-dropped', STORE, '''            if names(&leases).is_empty() && inside_root(&root_real, &leases) {
                remove_tree(&leases)?;
            }''', '''            if names(&leases).is_empty() {
                remove_tree(&leases)?;
            }''', LEASE_ID_SWAP,
     'an id directory swapped mid-round must not be removed'),
    # #1850: the non-`NotFound` `realpath` branch of `release` had no direct
    # regression test (A8 registered it as untestable without permission
    # injection); an ELOOP root is the deterministic shape. The mutant swallows
    # the failure into the idempotent `Ok`.
    ('release-realpath-swallowed', STORE,
     '''                    Err(e) => return Err(UploadError::io(e, "realpath", Some(root))),''',
     '''                    Err(_) => return Ok(()),''', RELEASE_REALPATH,
     'a release realpath failure must surface, not be swallowed'),
    # #1850: `release` caches its result — including errors — via
    # `get_or_insert_with`, so a retried release answers with the same verdict
    # instead of re-running against a since-healed root. The mutant caches
    # successes only, which lets the retry re-run and delete the lease tree.
    ('release-err-not-cached', STORE, '''    pub fn release(&self) -> Result<()> {
        self.released
            .lock()
            .unwrap()
            .get_or_insert_with(|| {
                // The same guard as the lease sweep, applied late: `dir` was
                // built from the record's canonical project at acquire time,
                // and the lease can live for many turns. If `.tmp` or
                // `chat-upload-leases` was swapped for a symlink since, the
                // recursive delete would follow it out of the project. Require
                // the root to still resolve to exactly the plain path, then the
                // entry to still be strictly inside it.
                let Some(root) = self.dir.parent().and_then(Path::parent) else {
                    return Ok(());
                };
                // "Gone" stays a quiet no-op (the lease is already clean, and
                // `release` is idempotent); any other failure keeps the
                // original `remove_tree` behaviour of surfacing the error
                // instead of caching a success that never happened.
                let root_real = match fs::canonicalize(root) {
                    Ok(real) => real,
                    Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
                    Err(e) => return Err(UploadError::io(e, "realpath", Some(root))),
                };
                if root_real != root {
                    return Ok(());
                }
                match resolved_inside(&root_real, &self.dir) {
                    Ok(Some(_)) => remove_tree(&self.dir),
                    Ok(None) => Ok(()),
                    Err(e) => Err(UploadError::io(e, "realpath", Some(&self.dir))),
                }
            })
            .clone()
    }''', '''    pub fn release(&self) -> Result<()> {
        let mut slot = self.released.lock().unwrap();
        if let Some(cached) = slot.clone() {
            return cached;
        }
        let computed = (|| {
                let Some(root) = self.dir.parent().and_then(Path::parent) else {
                    return Ok(());
                };
                let root_real = match fs::canonicalize(root) {
                    Ok(real) => real,
                    Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
                    Err(e) => return Err(UploadError::io(e, "realpath", Some(root))),
                };
                if root_real != root {
                    return Ok(());
                }
                match resolved_inside(&root_real, &self.dir) {
                    Ok(Some(_)) => remove_tree(&self.dir),
                    Ok(None) => Ok(()),
                    Err(e) => Err(UploadError::io(e, "realpath", Some(&self.dir))),
                }
        })();
        if computed.is_ok() {
            *slot = Some(computed.clone());
        }
        computed
    }''', RELEASE_REALPATH,
     'a cached release failure must stay cached after the root heals'),
    # #1631/#2276: the pair verdicts re-judged outside the gate — the
    # pre-#1631 shapes (require_active / status checks on the outside-`get`
    # snapshot), with the #2276 rendezvous kept so the barrier still
    # schedules 'both callers passed the snapshot, neither entered the gate'.
    # Reverting only one of the two is gate-order-dependent (flaky), so the
    # row reverts the pair together and the kill is deterministic round 1.
    ('pair-verdicts-outside-gate', STORE, '''    pub fn complete(&self, upload_id: &str) -> Result<UploadRecord> {
        let record = self.get(upload_id)?;
        // #2276/#2279 test rendezvous (see `set_pre_mutate_hook`).
        self.pre_mutate();
        let mut refusal: Option<UploadError> = None;
        let updated = self.mutate(&record, |r| {
            if !active(r) {
                refusal = Some(invalid_state(r));
                return false;
            }
            if array(r, "manifest")
                .iter()
                .any(|m| !array(r, "receivedClientFileIds").contains(&value(m, "clientFileId")))
            {
                let message = JsString::from("One or more declared files were not uploaded.");
                mark_failed(r, "UPLOAD_MANIFEST_MISMATCH", &message);
                refusal = Some(UploadError::dialog("UPLOAD_MANIFEST_MISMATCH", message));
            } else {
                set(r, "status", text("completed"));
                set(r, "uploadedBytes", value(r, "totalBytes"));
            }
            true
        })?;
        match refusal {
            Some(e) => Err(e),
            None => Ok(updated),
        }
    }

    /// Same #1631 gate-internal verdict as [`Self::complete`]: only the
    /// in-gate transition to `cancelled` is followed by `remove_tree`, so a
    /// `completed` record's files survive a racing cancel. The `remove_tree`
    /// itself still runs after the gate (the module-doc pair scope and #1503
    /// cover what a concurrently in-flight `write_part` can interleave).
    ///
    /// #1850: that delete is the fourth removal point on the `chat-uploads`
    /// tree, and unlike the sweeps it runs after the whole `mutate` rewrite —
    /// a window wide enough to swap `.tmp` (or the task directory itself) for
    /// a symlink, which `remove_tree` would follow out of the project. The
    /// status transition is already durable at that point, so a refused
    /// delete is logged and skipped, never a `cancel` error: the record is
    /// cancelled either way, and the sweep's "rather not delete" trade-off
    /// (A8-UPLOAD-CLEANUP-ROBUSTNESS-001) applies unchanged.
    pub fn cancel(&self, upload_id: &str) -> Result<UploadRecord> {
        let record = self.get(upload_id)?;
        // #2276/#2279 test rendezvous (see `set_pre_mutate_hook`).
        self.pre_mutate();
        let mut refusal: Option<UploadError> = None;
        let mut cancelled = false;
        let updated = self.mutate(&record, |r| {
            if active(r) {
                set(r, "status", text("cancelled"));
                cancelled = true;
                true
            } else if status(r) == "completed" {
                refusal = Some(UploadError::dialog(
                    "UPLOAD_ALREADY_COMPLETED",
                    "Completed uploads cannot be cancelled.",
                ));
                false
            } else {
                // #2275: cancelling a cancelled (or otherwise settled) upload
                // is a no-op, not a change: nothing to write or emit.
                false
            }
        })?;
        if let Some(e) = refusal {
            return Err(e);
        }
        if cancelled {
            let project = PathBuf::from(string(&updated, "projectKey").unwrap_or_default());
            let dir = task_dir(&updated).join("files");
            let root_real = match plain_root(&project, "chat-uploads") {
                PlainRoot::Plain(root) => root,
                // The record was just read from this tree, so `Missing` means
                // the whole task directory is gone: the same quiet no-op the
                // bare `remove_tree` gave.
                PlainRoot::Missing => return Ok(updated),
                PlainRoot::Blocked => {
                    self.log_cleanup(format!(
                        "[uploads] cancel skipped {}: uploads root is not a plain directory",
                        project.join(".tmp/chat-uploads").display()
                    ));
                    return Ok(updated);
                }
            };
            // fix-r1 (minimax concurrency#2 arbitration): `inside_root` alone
            // passes a linked `<id>` whose resolution stays inside the root —
            // a deterministic alias onto another upload. The component walk
            // `canonical_home` uses for the lease/store roots refuses every
            // link component down to `files`; the residual between the walk
            // and `remove_tree` is the instruction-level TOCTOU family the
            // A8 ledger registers.
            if canonical_home(&dir, &project, &root_real).is_none() {
                self.log_cleanup(format!(
                    "[uploads] cancel skipped {}: outside the uploads root or a linked task dir",
                    dir.display()
                ));
                return Ok(updated);
            }
            remove_tree(&dir)?;
        }
        Ok(updated)
    }

''', '''    pub fn complete(&self, upload_id: &str) -> Result<UploadRecord> {
        let record = self.get(upload_id)?;
        // #2276/#2279 test rendezvous (see `set_pre_mutate_hook`).
        self.pre_mutate();
        require_active(&record)?;
        if array(&record, "manifest")
            .iter()
            .any(|m| !array(&record, "receivedClientFileIds").contains(&value(m, "clientFileId")))
        {
            let message = JsString::from("One or more declared files were not uploaded.");
            self.fail(upload_id, "UPLOAD_MANIFEST_MISMATCH", &message)?;
            return Err(UploadError::dialog("UPLOAD_MANIFEST_MISMATCH", message));
        }
        self.mutate(&record, |r| {
            set(r, "status", text("completed"));
            set(r, "uploadedBytes", value(r, "totalBytes"));
            true
        })
    }

    pub fn cancel(&self, upload_id: &str) -> Result<UploadRecord> {
        let record = self.get(upload_id)?;
        // #2276/#2279 test rendezvous (see `set_pre_mutate_hook`).
        self.pre_mutate();
        if status(&record) == "completed" {
            return Err(UploadError::dialog(
                "UPLOAD_ALREADY_COMPLETED",
                "Completed uploads cannot be cancelled.",
            ));
        }
        if !active(&record) {
            return Ok(record);
        }
        let updated = self.mutate(&record, |r| {
            set(r, "status", text("cancelled"));
            true
        })?;
        remove_tree(&task_dir(&updated).join("files"))?;
        Ok(updated)
    }

''', 'interleaved_complete_and_cancel_cannot_both_win',
     'both calls must not be able to report success'),
    # #2279 (review fix-r1, DS sibling#1 + MM sibling#2/concurrency#3): the
    # in-gate re-read's non-fallback is itself a defense — this row reverts
    # it to the pre-fix outside-snapshot fallback (with the pre-#2275
    # unconditional change, the exact pre-batch shape), so a swept record is
    # resurrected and reported completed again.
    ('mutate-reread-falls-back-to-outside-snapshot', STORE, '''            let mut current = self.read_at(&project, &id)?.ok_or_else(|| {
                UploadError::dialog("UPLOAD_NOT_FOUND", format!("Unknown uploadId: {id}"))
            })?;
            if !change(&mut current) {
                return Ok(current);
            }
''', '''            let mut current = self
                .read_at(&project, &id)?
                .unwrap_or_else(|| record.clone());
            change(&mut current);
''', 'upload::tests::upload_complete_after_the_sweep_removed_the_record_refuses_without_resurrecting',
     '#2279 complete reported success over a swept record'),
    # #2275 (review fix-r1, same reviewers): the no-change gate is a defense —
    # dropping it restores the pre-fix "every verdict rewrites the record",
    # so an injected metadata-write failure answers in place of the refusal's
    # semantic code.
    ('mutate-no-change-gate-dropped', STORE, '''            if !change(&mut current) {
                return Ok(current);
            }
''', '''            change(&mut current);
''', 'upload::tests::upload_refusals_write_nothing_emit_nothing_and_do_not_poison_the_gate',
     '#2275 the refusal answers its semantic code, not the injected write fault'),
]

# #1666: static denominator; a deleted row must not shrink the run silently and
# an emptied table must never print `0/0 killed` and exit 0.
EXPECTED_NAMES = (
    'attachment-path-leak', 'record-unknown-leak', 'optional-empties-kept',
    'files-not-array', 'project-key-not-trimmed', 'project-key-nonstring-accepted',
    'upload-id-not-safe', 'client-file-id-not-trimmed',
    'base64-limit-off-by-one', 'base64-precheck-removed', 'cap-after-decode',
    'project-error-lossy', 'part-error-lossy', 'base64-trailing-bits-rejected',
    'surrogate-key-hash-not-lossy', 'idempotency-hash-skipped',
    'idempotency-type-before-manifest', 'number-format-expanded',
    'surrogate-lossy-bridge', 'null-attachment-swallowed',
    'part-requires-provider-first', 'get-requires-provider-after-upload-id',
    # main 的 #1494 read_at / lease 六条，rebase 后并入；名册与运行时必须逐名一致，
    # 否则整族在 `registered()` 处 rc 2，宿主读成「从未开跑」。
    'read-at-handle-check-dropped', 'read-at-check-before-open',
    'read-at-identity-dropped', 'read-at-approved-stat-follows-link',
    'lease-link-identity-dropped', 'lease-link-follows-symlink-source',
    # #1887/#1888/#1889 的五条（租约内容摘要 / 租约条目非阻塞打开 / 摘要读长 /
    # 写侧租约根 / 读侧词法前缀）也在同一 `registered()` 口径下：名册与运行时
    # 逐名同序，漏一条即整族 rc 2。
    'lease-link-content-dropped', 'lease-link-nonblock-dropped',
    'lease-link-length-dropped',
    # #2098 的六条（S1 verify 三条：非阻塞打开 / 不跟随链接 / 读长与恰好结尾；
    # S2 read_at 三条：非阻塞打开 / 不跟随链接 / metadata 上限），与上面同一名册
    # 口径：名册与运行时逐名同序，漏一条即整族 rc 2。
    'verify-open-nonblock-dropped', 'verify-open-nofollow-dropped',
    'verify-hash-length-dropped', 'read-at-open-nonblock-dropped',
    'read-at-open-nofollow-dropped', 'read-at-metadata-cap-dropped',
    'lease-root-link-refused',
    'read-at-upload-root-prefix-dropped',
    # main 的 #1646 清理鲁棒性十五条，rebase 后并入；同理，名册漏一条即
    # `registered()` rc 2，整族在宿主上被读成「从未开跑」。
    'cleanup-bad-record-aborts', 'cleanup-orphan-reclaim-removed',
    'cleanup-orphan-fresh-deleted', 'lease-write-in-place',
    'cleanup-symlink-root-swept', 'cleanup-record-rm-aborts',
    'cleanup-orphan-root-recanonicalized', 'cleanup-lease-symlink-root-swept',
    'cleanup-record-rm-follows-swapped-root', 'cleanup-lease-entry-symlink-swept',
    'cleanup-lease-root-recanonicalized', 'lease-alias-walk-downgraded',
    'lease-release-symlink-root-swept',
    'plain-root-accepts-any-resolve', 'cleanup-orphan-rm-silent',
    'cleanup-schedule-stop-on-failure',
    # #1850 的五条（cancel 删除点闸门 / 条目 containment / 空目录守卫 /
    # release realpath 分支 / Err 缓存）+ fix-r1 四条（cancel 与 lease 条目
    # 删除点各两条：删除闸门整块删除 / 别名走查降级回 containment-only），
    # 与上面同一名册口径。
    'cancel-remove-tree-ungated', 'cancel-delete-gate-dropped',
    'cancel-alias-walk-downgraded',
    'cleanup-lease-id-guard-dropped', 'release-realpath-swallowed',
    'release-err-not-cached',
    # #2276 的这一条（#1631 对判定双双挪回门外——预修复形态），与上面同一名册口径。
    'pair-verdicts-outside-gate',
    # #2279/#2275 两条（评审 fix-r1：门内重读回退门外快照 / no-change 闸门删除），
    # 同一名册口径。
    'mutate-reread-falls-back-to-outside-snapshot',
    'mutate-no-change-gate-dropped',
)

if '--check' in sys.argv[1:]:
    # #1666: ordinary raises, so `python3 -O` cannot strip these guards.
    try:
        registered(mutations, EXPECTED_NAMES)
    except ValueError as error:
        refuse(str(error))
    for name, source, old, _new, _test, _expected in mutations:
        original = source.read_text()
        occurrences = original.count(old)
        if occurrences != 1:
            raise ValueError(f'{name}: anchor occurs {occurrences} times (expected 1)')
    print(f'PASS: {len(mutations)} mutation anchors match their sources')
    sys.exit(0)

selected = sys.argv[2:]
try:
    registered(mutations, EXPECTED_NAMES)
    # `mutations` stays the frozen literal so `anchor_selfcheck.py` can read it
    # statically; the requested subset is a separate name.
    run_rows = select(mutations, selected, always=())
except ValueError as error:
    refuse(str(error))
env = os.environ.copy()
if missing_target(env):  # #1666: absent, empty or blank, one shared predicate
    refuse('set CARGO_TARGET_DIR to the caller build directory')
refuse_unusable_target()  # #1666 fix-r9: dot / root / src are unusable too
out = claim_dir(sys.argv[1])
summary = []
# #1540: a fully-green run against that pinned baseline is NON-HOST EVIDENCE
# (the host gate only trusts HEAD), so it exits 42 instead of 0. The variable
# was mis-spelled PDRS_NEG_BASELINE until #1540, so an override silently
# never reached this guard.
baseline_source, baseline, baseline_sha = non_head.gate(root)
with mutating_sources(root, [GATEWAY, STORE], baseline=baseline) as snapshots:
    for name, source, old, new, test, expected in run_rows:
        original = snapshots[source]
        before = hashlib.sha256(original).hexdigest()
        content = original.decode()
        occurrences = content.count(old)
        if occurrences != 1:
            raise ValueError(f'{name}: anchor occurs {occurrences} times (expected 1)')
        changed = content.replace(old, new, 1)
        (out / f'{name}.patch').write_text(''.join(difflib.unified_diff(
            content.splitlines(True), changed.splitlines(True),
            fromfile=source.name, tofile=source.name)))
        try:
            wait_for_slot(root / 'scripts/cargo-slots.sh')
            source.write_text(changed)
            # #2276: a named test registered in INTEGRATION_TESTS runs against
            # its `tests/<target>.rs` integration target; every other row
            # keeps the lib face. The target comes from the registry, not a
            # guess off the test name.
            target = INTEGRATION_TESTS.get(test)
            command = (['cargo', 'test', '--manifest-path', 'src/Cargo.toml',
                        '-p', 'pilotdeck-gateway', '--test', target,
                        '--', '--exact', test]
                       if target is not None else
                       ['cargo', 'test', '--manifest-path', 'src/Cargo.toml',
                        '-p', 'pilotdeck-gateway', '--lib', test, '--', '--exact'])
            result = run_owned(command, cwd=root, env=env, timeout=900)
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
            if not killed:
                raise RuntimeError(
                    f'{name}: did not hit its named assertion ({expected!r})')
        finally:
            source.write_bytes(original)
            restored = hashlib.sha256(source.read_bytes()).hexdigest()
            if restored != before:
                raise RuntimeError(
                    f'{name}: restore failed ({restored} != {before}); the tree is dirty')
            (out / 'summary.json').write_text(json.dumps(summary, ensure_ascii=False, indent=2) + '\n')
killed_rows = sum(1 for row in summary if row['killed'])
print(f'{killed_rows}/{len(run_rows)} killed; every source restored byte-for-byte', flush=True)
# #1666: a zero-row or surviving run can never exit 0 under `-O`.
# #1540: green but non-HEAD is NON-HOST EVIDENCE and exits 42, never 0.
sys.exit(non_head.non_head_exit(baseline_source, baseline_sha,
                                code=verdict_exit(summary, key='killed', killed=True),
                                evidence=out))
