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
from negative_panic import named_test_killed, panic_evidence
from negative_process import (mutating_sources, run_owned, wait_for_slot,
                               write_source_atomic)
from negative_selection import claim_dir, missing_target, refuse, refuse_unusable_target, registered, select, verdict_exit

root = Path(__file__).resolve().parents[5]
source = root / 'src/pilotdeck-gateway/src/config/settings.rs'
shared = root / 'src/pilotdeck-gateway/src/config/snapshot.rs'

# Every mutant is applied to the original independently and names the test in
# `--test settings` whose assertion it must turn red. Rows are
# (name, source, old, new) so the anchor_selfcheck preflight can statically
# resolve each row's path and verify the old anchor against the live file.
mutations = [
    ('redaction-disabled',
     source,
     '''    out.insert(
        "config".into(),
        super::snapshot::redact(&field(snapshot, "config")),
    );''',
     '''    out.insert("config".into(), field(snapshot, "config"));'''),
    ('hint-always-included',
     source,
     '''    for key in ["path", "source", "hint", "redactedValue"] {
        put_truthy(&mut out, value, key);
    }''',
     '''    for key in ["path", "source", "hint", "redactedValue"] {
        put_truthy(&mut out, value, key);
    }
    out.insert("hint".into(), field(value, "hint"));'''),
    ('recoverable-truthiness',
     source,
     '''    let recoverable = field(value, "recoverable");
    if !matches!(recoverable, JsValue::Undefined) {
        out.insert("recoverable".into(), recoverable);
    }''',
     '''    let recoverable = field(value, "recoverable");
    if truthy(&recoverable) {
        out.insert("recoverable".into(), recoverable);
    }'''),
    ('source-path-always-included',
     source,
     '''    for key in ["path", "contentHash", "phase"] {
        put_truthy(&mut out, value, key);
    }''',
     '''    for key in ["path", "contentHash", "phase"] {
        put_truthy(&mut out, value, key);
    }
    out.insert("path".into(), field(value, "path"));'''),
    ('top-level-key-order-swapped',
     source,
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
     source,
     '''    for key in ["schemaVersion", "version", "loadedAt", "contentHash"] {
        out.insert(key.into(), field(snapshot, key));
    }''',
     '''    for key in ["schemaVersion", "loadedAt", "contentHash"] {
        out.insert(key.into(), field(snapshot, key));
    }
    out.insert("version".into(), JsValue::Number(1.0));'''),
    # The shared redact helper must fold case exactly like the baseline's
    # non-Unicode `/.../i` pattern: regressing to full Unicode folding (the
    # pre-#864-fix-r1 code) redacts lookalike keys such as the U+212A KELVIN
    # SIGN spelling of `token`.
    ('unicode-folding-regression',
     shared,
     '''        let folded: Vec<u16> = key
            .units()
            .iter()
            .map(|&unit| {
                if (65..=90).contains(&unit) {
                    unit + 32
                } else {
                    unit
                }
            })
            .collect();
        NEEDLES.iter().any(|needle| {
            let needle: Vec<u16> = needle.encode_utf16().collect();
            folded
                .windows(needle.len())
                .any(|window| window == needle.as_slice())
        })''',
     '''        let lower = String::from_utf16_lossy(key.units()).to_lowercase();
        NEEDLES.iter().any(|needle| lower.contains(needle))'''),
]
# #1666: the denominator is this literal roster, not `len(mutations)`. With a
# runtime denominator a deleted row shrinks the run silently and a table that
# lost every row reports `0/0 killed` and exits 0. Renaming or dropping a
# mutant here is a deliberate edit that must update this tuple too.
EXPECTED_NAMES = (
    'redaction-disabled',
    'hint-always-included',
    'recoverable-truthiness',
    'source-path-always-included',
    'top-level-key-order-swapped',
    'version-mistranscribed',
    'unicode-folding-regression',
)
tests = {
    'redaction-disabled': 'config_is_projected_through_the_shared_redaction',
    'hint-always-included': 'optional_fields_follow_truthiness',
    'recoverable-truthiness': 'recoverable_follows_presence_not_truthiness',
    'source-path-always-included': 'resolve_settings_replays_real_typescript_oracle',
    'top-level-key-order-swapped': 'response_and_element_key_orders_match_the_baseline',
    'version-mistranscribed': 'scalar_fields_pass_through_verbatim',
    'unicode-folding-regression': 'unicode_lookalike_keys_survive_the_secret_match',
}
# A killed mutant must fail through its bound assertion, not by failing to
# compile. #1087 fix-r3: each fragment is the bound assertion's own message
# (the three generic `left == right` payloads were replaced by dedicated
# messages in tests/settings.rs), so the shared strict judge can hold the
# marker inside the named test's own panic payload.
failure_fragments = {
    'redaction-disabled': '#1087 the shared redaction must redact secret-shaped keys',
    'hint-always-included': 'an empty hint is dropped',
    'recoverable-truthiness': 'recoverable: false must be kept',
    'source-path-always-included': 'response differences',
    'top-level-key-order-swapped': '#1087 the top-level response key order must match the baseline',
    'version-mistranscribed': '#1087 the version field must pass through the snapshot verbatim',
    'unicode-folding-regression': '(#864 fix-r1)',
}


def classify(returncode, log, name):
    """The shared strict #813 verdict for one mutant row (#1087 fix-r3).

    The named test's own FAILED status line plus the bound assertion's
    message inside that test's own panic payload (``named_test_killed``
    judges both); the old whole-log substring check matched a sibling test's
    panic, captured stdout and build output. Returns ``(killed, failure)``.
    """
    test = tests[name]
    fragment = failure_fragments[name]
    killed = (returncode == 101 and named_test_killed(log, test, fragment))
    failure = next((line.strip() for line in panic_evidence(log, test).splitlines()
                    if fragment in line), '')
    return killed, failure


selected = sys.argv[2:]
# #1666: both guards are ordinary raises, so `python3 -O` cannot strip them.
# They run before the environment check so a typo is refused as "never started"
# (exit 2) instead of being mistaken for a failed control (exit 1).
try:
    registered(mutations, EXPECTED_NAMES)
    # `mutations` stays the frozen literal: `anchor_selfcheck.py` reads this
    # table statically, and rebinding it to a call would hide the roster from
    # the static extractor. The subset is a separate name.
    run_rows = select(mutations, selected, always=())
except ValueError as error:
    refuse(str(error))
if missing_target():  # #1666: absent, empty or blank, one shared predicate
    refuse('set CARGO_TARGET_DIR to the caller-owned build tree before running')
refuse_unusable_target()  # #1666 fix-r9: dot / root / src are unusable too
out = claim_dir(sys.argv[1])
env = os.environ.copy()
summary = []
with mutating_sources(root, [source, shared]) as snapshots:
    for name, path, old, new in run_rows:
        original = snapshots[path]
        before = hashlib.sha256(original).hexdigest()
        content = original.decode()
        # A stale anchor (source drifted, or a copy-paste duplicate) must abort
        # the run; recording it as SURVIVED would hide a broken control.
        occurrences = content.count(old)
        if occurrences != 1:
            raise ValueError(
                f'{name}: anchor occurs {occurrences} times (expected 1); '
                'the settings.rs anchor is stale and must be updated')
        changed = content.replace(old, new, 1)
        (out / f'{name}.patch').write_text(''.join(difflib.unified_diff(
            content.splitlines(True), changed.splitlines(True),
            fromfile='config/settings.rs', tofile='config/settings.rs')))
        try:
            wait_for_slot(root / 'scripts/cargo-slots.sh')
            # #1908: one rename, never a truncating in-place write. The shared
            # publish helper (#1903) keeps a SIGKILL from leaving an empty or
            # half-written source that the next preflight would read as "differs
            # from HEAD" and refuse the whole tree.
            write_source_atomic(root, path, changed.encode())
            test = tests[name]
            result = run_owned(['cargo', 'test', '--manifest-path', 'src/Cargo.toml',
                               '-p', 'pilotdeck-gateway', '--test', 'settings',
                               '--locked', '--offline', test,
                               '--', '--exact'],
                              cwd=root, env=env, timeout=900)
            log = result.stdout + result.stderr
            (out / f'{name}.log').write_text(log)
            killed, failure = classify(result.returncode, log, name)
            summary.append(dict(name=name, killed=killed, test=test,
                                returncode=result.returncode, failure=failure,
                                sourceSha256=before))
            print(f'{name}: {"KILLED" if killed else "NOT KILLED"} '
                  f'rc={result.returncode} {failure}', flush=True)
            if not killed:
                raise RuntimeError(
                    f'{name}: compile/signal failure or surviving mutant is not evidence')
        finally:
            # #1908: same rule on the way back; the shared publish helper
            # replaces a truncating write_bytes restore.
            write_source_atomic(root, path, original)
            restored = hashlib.sha256(path.read_bytes()).hexdigest()
            if restored != before:
                raise RuntimeError(
                    f'{name}: restore failed ({restored} != {before}); the tree is dirty')
            (out / 'summary.json').write_text(
                json.dumps(summary, ensure_ascii=False, indent=2) + '\n')
    killed_rows = sum(1 for row in summary if row['killed'])
    print(f'{killed_rows}/{len(run_rows)} killed; every source restored byte-for-byte',
          flush=True)
# #1666: a zero-row run can never exit 0, and neither can a surviving mutant.
sys.exit(verdict_exit(summary, key='killed', killed=True))
