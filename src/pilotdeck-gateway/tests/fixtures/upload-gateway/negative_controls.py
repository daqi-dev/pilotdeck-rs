#!/usr/bin/env python3
"""Temporary upload-gateway product mutations; each must turn a named test red.

The adapter source is new in this PR, so ``mutating_sources`` refuses to run
before it exists in HEAD (it verifies the committed bytes first). Run this
after the coordinator commits the branch:

    CARGO_TARGET_DIR=<worktree>/_build/cargo-target \
      python3 src/pilotdeck-gateway/tests/fixtures/upload-gateway/negative_controls.py <out-dir>

``--check`` only verifies that every mutation anchor still matches the current
source; it does not mutate, compile, or need a commit.
"""
import difflib
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

root = Path(__file__).resolve().parents[5]
source = root / 'src/pilotdeck-gateway/src/upload/gateway.rs'
ORACLE = 'upload::gateway_tests::upload_gateway_oracle_replay_matches_ts_results'
PATH_TEST = 'upload::gateway_tests::upload_gateway_never_projects_attachment_disk_path'

# Every mutation is applied to the pristine original, never cumulatively.
mutations = [
    ('attachment-path-leak', '''    let mut out = object([
        ("attachmentId", value(attachment, "attachmentId")),
        ("name", value(attachment, "name")),
        ("relativePath", value(attachment, "relativePath")),
    ]);''', '    let mut out = attachment.clone();', PATH_TEST),
    ('record-unknown-leak', '    let mut out = JsObject::new();',
     '    let mut out = record.as_object().cloned().unwrap_or_default();', ORACLE),
    ('optional-empties-kept', '        if !truthy(&field) {',
     '        if field.is_undefined() {', ORACLE),
    ('files-not-array', '        if !matches!(files, JsValue::Array(_)) {',
     '        if false {', ORACLE),
    ('project-key-not-trimmed',
     '        JsValue::String(key) if trim_js(key).units().is_empty() => Err(required()),',
     '        JsValue::String(key) if key.units().is_empty() => Err(required()),', ORACLE),
    ('project-key-nonstring-accepted',
     '        // `input.projectKey?.trim()` boxes primitives; only nullish short-circuits.\n'
     '        _ => Err(request_failed("input.projectKey?.trim is not a function")),',
     '        // `input.projectKey?.trim()` boxes primitives; only nullish short-circuits.\n'
     '        _ => Ok(String::new()),', ORACLE),
    ('upload-id-not-safe', '    assert_id(&id)?;', '    let _ = &id;', ORACLE),
    ('client-file-id-not-trimmed',
     '    if trim_js(raw).units().is_empty() {',
     '    if raw.units().is_empty() {', ORACLE),
    ('base64-limit-off-by-one',
     '    if bytes.len() > MAX_REMOTE_UPLOAD_PART_BYTES {',
     '    if bytes.len() > MAX_REMOTE_UPLOAD_PART_BYTES + 1 {', ORACLE),
    ('base64-trailing-bits-rejected',
     '        .with_decode_allow_trailing_bits(true)',
     '        .with_decode_allow_trailing_bits(false)', ORACLE),
    ('idempotency-hash-skipped',
     '        let key = match idempotency_key.filter(|key| truthy(key)) {',
     '        let key = match None::<&JsValue> {', ORACLE),
    ('part-requires-provider-first', '''        let upload_id = upload_id(params)?;
        let client_file_id = client_file_id(params)?;
        let content = parameter(params, "contentBase64")?;
        let bytes = decode_upload_part(content)?;
        let provider = self.require()?;''', '''        let provider = self.require()?;
        let upload_id = upload_id(params)?;
        let client_file_id = client_file_id(params)?;
        let content = parameter(params, "contentBase64")?;
        let bytes = decode_upload_part(content)?;''', ORACLE),
    ('get-requires-provider-after-upload-id', '''        let provider = self.require()?;
        let upload_id = upload_id(params)?;
        gateway_record(&provider.get(&upload_id)?)''', '''        let upload_id = upload_id(params)?;
        let provider = self.require()?;
        gateway_record(&provider.get(&upload_id)?)''', ORACLE),
]

if '--check' in sys.argv[1:]:
    original = source.read_text()
    for name, old, _new, _test in mutations:
        assert original.count(old) == 1, (name, original.count(old))
    print(f'PASS: {len(mutations)} mutation anchors match {source}')
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
with mutating_sources(root, [source]) as snapshots:
    for name, old, new, test in mutations:
        original = snapshots[source]
        before = hashlib.sha256(original).hexdigest()
        content = original.decode()
        assert content.count(old) == 1, (name, content.count(old))
        changed = content.replace(old, new, 1)
        (out / f'{name}.patch').write_text(''.join(difflib.unified_diff(
            content.splitlines(True), changed.splitlines(True),
            fromfile='gateway.rs', tofile='gateway.rs')))
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
                      and 'panicked at' in log
                      and 'error[E' not in log)
            failure = next((line.strip() for line in log.splitlines()
                            if 'assertion' in line or 'panicked at' in line), '')
            summary.append(dict(name=name, test=test, killed=killed,
                                returncode=result.returncode, failure=failure,
                                sourceSha256=before))
            print(f'{name}: {"KILLED" if killed else "NOT KILLED"} rc={result.returncode} {failure}', flush=True)
            assert killed, f'{name}: compile/signal failure or surviving mutant is not evidence'
        finally:
            source.write_bytes(original)
            assert hashlib.sha256(source.read_bytes()).hexdigest() == before
            (out / 'summary.json').write_text(json.dumps(summary, ensure_ascii=False, indent=2) + '\n')
print(f'{len(summary)}/{len(mutations)} killed; every source restored byte-for-byte', flush=True)
