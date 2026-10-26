#!/usr/bin/env python3
"""Temporary product mutations; require an assertion failure, then restore.

Run with no other writers/builds in this worktree, after the component source
is committed (``mutating_sources`` refuses untracked files by design). Evidence
goes in a new output folder; original source bytes and hashes are verified
after every mutation. ``CARGO_TARGET_DIR`` must come from the caller so no
second build tree is ever created.
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
out = Path(sys.argv[1]).resolve()
out.mkdir(parents=True, exist_ok=False)
source = root / 'src/pilotdeck-gateway/src/config/settings.rs'

# Every mutant is applied to the original independently and names the test in
# `--test settings` whose assertion it must turn red.
mutations = [
    ('redaction-disabled',
     '''    out.insert(
        "config".into(),
        super::snapshot::redact(&field(snapshot, "config")),
    );''',
     '''    out.insert("config".into(), field(snapshot, "config"));'''),
    ('hint-always-included',
     '''    for key in ["path", "source", "hint", "redactedValue"] {
        put_truthy(&mut out, value, key);
    }''',
     '''    for key in ["path", "source", "hint", "redactedValue"] {
        put_truthy(&mut out, value, key);
    }
    out.insert("hint".into(), field(value, "hint"));'''),
    ('recoverable-truthiness',
     '''    let recoverable = field(value, "recoverable");
    if !matches!(recoverable, JsValue::Undefined) {
        out.insert("recoverable".into(), recoverable);
    }''',
     '''    let recoverable = field(value, "recoverable");
    if truthy(&recoverable) {
        out.insert("recoverable".into(), recoverable);
    }'''),
    ('source-path-always-included',
     '''    for key in ["path", "contentHash", "phase"] {
        put_truthy(&mut out, value, key);
    }''',
     '''    for key in ["path", "contentHash", "phase"] {
        put_truthy(&mut out, value, key);
    }
    out.insert("path".into(), field(value, "path"));'''),
    ('top-level-key-order-swapped',
     '''    out.insert(
        "config".into(),
        super::snapshot::redact(&field(snapshot, "config")),
    );
    out.insert("sources".into(), projected(snapshot, "sources", source));''',
     '''    out.insert("sources".into(), projected(snapshot, "sources", source));
    out.insert(
        "config".into(),
        super::snapshot::redact(&field(snapshot, "config")),
    );'''),
    ('version-mistranscribed',
     '''    for key in ["schemaVersion", "version", "loadedAt", "contentHash"] {
        out.insert(key.into(), field(snapshot, key));
    }''',
     '''    for key in ["schemaVersion", "loadedAt", "contentHash"] {
        out.insert(key.into(), field(snapshot, key));
    }
    out.insert("version".into(), JsValue::Number(1.0));'''),
]
tests = {
    'redaction-disabled': 'config_is_projected_through_the_shared_redaction',
    'hint-always-included': 'optional_fields_follow_truthiness',
    'recoverable-truthiness': 'recoverable_follows_presence_not_truthiness',
    'source-path-always-included': 'resolve_settings_replays_real_typescript_oracle',
    'top-level-key-order-swapped': 'response_and_element_key_orders_match_the_baseline',
    'version-mistranscribed': 'scalar_fields_pass_through_verbatim',
}
# A killed mutant must fail through its bound assertion, not by failing to
# compile; the message fragment proves the intended assertion fired.
failure_fragments = {
    'redaction-disabled': 'assertion `left == right` failed',
    'hint-always-included': 'an empty hint is dropped',
    'recoverable-truthiness': 'recoverable: false must be kept',
    'source-path-always-included': 'response differences',
    'top-level-key-order-swapped': 'assertion `left == right` failed',
    'version-mistranscribed': 'assertion `left == right` failed',
}
selected = sys.argv[2:]
if selected:
    mutations = [mutation for mutation in mutations if mutation[0] in selected]
    assert len(mutations) == len(selected), 'unknown mutation name'
if 'CARGO_TARGET_DIR' not in os.environ:
    sys.exit('set CARGO_TARGET_DIR to the caller-owned build tree before running')
env = os.environ.copy()
summary = []
with mutating_sources(root, [source]) as snapshots:
    for name, old, new in mutations:
        path = source
        original = snapshots[path]
        before = hashlib.sha256(original).hexdigest()
        content = original.decode()
        # A stale anchor (source drifted, or a copy-paste duplicate) must abort
        # the run; recording it as SURVIVED would hide a broken control.
        assert content.count(old) == 1, (
            f'{name}: anchor occurs {content.count(old)} times (expected 1); '
            'the settings.rs anchor is stale and must be updated')
        changed = content.replace(old, new, 1)
        (out / f'{name}.patch').write_text(''.join(difflib.unified_diff(
            content.splitlines(True), changed.splitlines(True),
            fromfile='config/settings.rs', tofile='config/settings.rs')))
        try:
            wait_for_slot(root / 'scripts/cargo-slots.sh')
            path.write_text(changed)
            test = tests[name]
            result = run_owned(['cargo', 'test', '--manifest-path', 'src/Cargo.toml',
                               '-p', 'pilotdeck-gateway', '--test', 'settings',
                               '--locked', '--offline', test],
                              cwd=root, env=env, timeout=900)
            log = result.stdout + result.stderr
            (out / f'{name}.log').write_text(log)
            killed = (result.returncode == 101 and f'{test} ... FAILED' in log
                      and failure_fragments[name] in log)
            failure = next((line.strip() for line in log.splitlines()
                            if failure_fragments[name] in line), '')
            summary.append(dict(name=name, killed=killed, test=test,
                                returncode=result.returncode, failure=failure,
                                sourceSha256=before))
            print(f'{name}: {"KILLED" if killed else "NOT KILLED"} '
                  f'rc={result.returncode} {failure}', flush=True)
            assert killed, f'{name}: compile/signal failure or surviving mutant is not evidence'
        finally:
            path.write_bytes(original)
            assert hashlib.sha256(path.read_bytes()).hexdigest() == before
            (out / 'summary.json').write_text(
                json.dumps(summary, ensure_ascii=False, indent=2) + '\n')
    print(f'{len(summary)}/{len(mutations)} killed; every source restored byte-for-byte',
          flush=True)
