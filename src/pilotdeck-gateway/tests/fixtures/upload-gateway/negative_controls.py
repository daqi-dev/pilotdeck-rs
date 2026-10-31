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
