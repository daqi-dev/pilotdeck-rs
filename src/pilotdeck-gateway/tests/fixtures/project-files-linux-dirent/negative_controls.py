#!/usr/bin/env python3
"""#133 negative controls: Linux dirent parity must be anchored, not assumed.

Run on the Linux host (cb8) only, after the fixture is committed
(``mutating_sources`` refuses untracked files by design): macOS cannot hold
the raw-byte names this control mutates around. Each mutant is applied to the
original independently and must be killed by the named test's bound assertion;
original source bytes and hashes are verified after every mutation.
``CARGO_TARGET_DIR`` must come from the caller so no second build tree is ever
created; ``PDRS_BASELINE`` must point at the pinned PilotDeck TS checkout.
"""
import difflib
import hashlib
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_panic import named_test_killed
from negative_selection import baseline_passed
from negative_process import mutating_sources, run_owned, wait_for_slot

root = Path(__file__).resolve().parents[5]
out = Path(sys.argv[1]).resolve()
out.mkdir(parents=True, exist_ok=False)
source = root / 'src/pilotdeck-gateway/src/project_files.rs'
test = 'project_files::tests::linux_dirent::linux_dirent_matches_node_v22_on_raw_byte_names'

# Source occurrence counts, independent of replace(..., 1)'s replacement limit:
# the byte-order sort anchor exists in both the Unix and the non-Unix
# native_entries, and only the first (Unix) occurrence is replaced.
ANCHOR_COUNTS = {'reverse-dirent-order': 2}

# Every mutant is applied to the original independently and must be killed by
# the named test's bound assertion. Rows are (name, source, old, new) so the
# anchor_selfcheck preflight can statically resolve each row's path and verify
# the old anchor against the live file.
mutations = [
    # Lossy decoding replaced by strict decoding: every illegal name collapses
    # to "" and the dirent name grouping loses its multi-raw-byte collapse.
    ('strict-decode',
     source,
     'name.to_string_lossy().into_owned()',
     'name.to_str().unwrap_or_default().to_owned()'),
    # Byte-order sort reversed: distinct decoded names leave ascending byte
    # order, so the dirent sequence comparison fails.
    ('reverse-dirent-order',
     source,
     'entries.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));',
     'entries.sort_by(|a, b| b.0.as_bytes().cmp(a.0.as_bytes()));'),
    # d_type directory flag inverted: the dirent file/dir types flip.
    ('flip-directory-type',
     source,
     'entry.file_type() == rustix::fs::FileType::Directory',
     'entry.file_type() != rustix::fs::FileType::Directory'),
    # Final wire sort reversed: the response's relativePath sequence leaves
    # the collator order Node produced.
    ('reverse-wire-sort',
     source,
     '''    scan.entries.sort_by(|a, b| {
        scan.collator.compare(
            &text_field(a, "relativePath").unwrap(),
            &text_field(b, "relativePath").unwrap(),
        )
    });''',
     '''    scan.entries.sort_by(|a, b| {
        scan.collator.compare(
            &text_field(b, "relativePath").unwrap(),
            &text_field(a, "relativePath").unwrap(),
        )
    });'''),
]
# A killed mutant must fail through its bound assertion, not by failing to
# compile or by panicking elsewhere; the message proves the intended
# assertion fired.
failure_markers = {
    'strict-decode': '#133 dirent name grouping differs from Node readdir',
    'reverse-dirent-order': '#133 dirent byte-order sort differs from Node readdir',
    'flip-directory-type': '#133 dirent file/dir types differ from Node readdir',
    'reverse-wire-sort': '#133 wire item order differs from Node',
}
selected = sys.argv[2:]
if selected:
    mutations = [mutation for mutation in mutations if mutation[0] in selected]
    assert len(mutations) == len(selected), 'unknown mutation name'
if 'CARGO_TARGET_DIR' not in os.environ:
    sys.exit('set CARGO_TARGET_DIR to the caller-owned build tree before running')
if 'PDRS_BASELINE' not in os.environ:
    sys.exit('set PDRS_BASELINE to the pinned PilotDeck TS checkout before running')
env = os.environ.copy()
env['PDRS_LINUX_DIRENT_CHECK'] = '1'
summary = []
failures = []


def run_named(receipt_dir):
    """Run the gated test with its own fresh receipt directory."""
    env['PDRS_LINUX_DIRENT_OUT'] = str(receipt_dir)
    result = run_owned(['cargo', 'test', '--manifest-path', 'src/Cargo.toml',
                        '-p', 'pilotdeck-gateway', '--lib', test,
                        '--', '--exact'],
                       cwd=root, env=env, timeout=3600)
    log = result.stdout + result.stderr
    return result.returncode, log


with mutating_sources(root, [source]) as snapshots:
    # #819: an unmutated baseline must be green first, and the receipt the
    # test publishes only at full completion must prove it actually compared.
    wait_for_slot(root / 'scripts/cargo-slots.sh')
    baseline_out = out / 'baseline'
    baseline_out.mkdir()
    code, log = run_named(baseline_out)
    (out / 'baseline.log').write_text(log)
    receipt_path = baseline_out / 'receipt.json'
    receipt_ok = False
    if receipt_path.exists():
        receipt = json.loads(receipt_path.read_text())
        receipt_ok = (receipt.get('cleanup') is True
                      and receipt.get('responses') == 5
                      and receipt.get('direntEntries') == 21
                      and receipt.get('wireItemCounts', [None])[0] == 6)
    baseline_ok = code == 0 and baseline_passed(log, test) and receipt_ok
    print(f'baseline: {"PASS" if baseline_ok else "INVALID"} '
          f'(receipt_ok={receipt_ok})', flush=True)
    if not baseline_ok:
        failures.append('baseline')

    for name, path, old, new in mutations:
        original = snapshots[path]
        before = hashlib.sha256(original).hexdigest()
        content = original.decode()
        # A stale anchor (source drifted, or a copy-paste duplicate) must abort
        # the run; recording it as SURVIVED would hide a broken control.
        assert content.count(old) == ANCHOR_COUNTS.get(name, 1), (
            f'{name}: anchor occurs {content.count(old)} times '
            f'(expected {ANCHOR_COUNTS.get(name, 1)}); the anchor is stale '
            'and must be updated')
        changed = content.replace(old, new, 1)
        (out / f'{name}.patch').write_text(''.join(difflib.unified_diff(
            content.splitlines(True), changed.splitlines(True),
            fromfile='project_files.rs', tofile='project_files.rs')))
        try:
            wait_for_slot(root / 'scripts/cargo-slots.sh')
            path.write_text(changed)
            mutant_out = out / name
            mutant_out.mkdir()
            code, log = run_named(mutant_out)
            (out / f'{name}.log').write_text(log)
            killed = (baseline_ok and code == 101
                      and named_test_killed(log, test, failure_markers[name])
                      and 'could not compile' not in log)
            failure = next((line.strip() for line in log.splitlines()
                            if failure_markers[name] in line), '')
            status = 'KILLED' if killed else (
                'SURVIVED' if code == 0 else 'INVALID')
            summary.append(dict(name=name, test=test, status=status,
                                returncode=code, failure=failure,
                                sourceSha256=before))
            print(f'{name}: {status} rc={code} {failure}', flush=True)
            if status != 'KILLED':
                failures.append(name)
        finally:
            path.write_bytes(original)
            assert hashlib.sha256(path.read_bytes()).hexdigest() == before
            (out / 'summary.json').write_text(
                json.dumps(summary, ensure_ascii=False, indent=2) + '\n')
counts = {state: sum(row['status'] == state for row in summary)
          for state in ('KILLED', 'SURVIVED', 'INVALID')}
print('summary: ' + ', '.join(f'{state}={count}' for state, count in counts.items()), flush=True)
if failures:
    print(f'not killed: {", ".join(failures)}', file=sys.stderr)
    sys.exit(1)
print(f'{len(summary)}/{len(mutations)} killed; every source restored byte-for-byte', flush=True)
