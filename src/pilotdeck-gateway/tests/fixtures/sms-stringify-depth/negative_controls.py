#!/usr/bin/env python3
"""#1929 depth-cap mutants: each must break a `#1929`-labelled assertion.

Mutations are applied to `channel/sms/permission.rs` only, through
`mutating_sources` (tree must equal HEAD, so this runs post-commit on the
integration host). Run with CARGO_TARGET_DIR set:

    python3 negative_controls.py <output-dir> [name ...]

Every mutant is compiled (`cargo test --no-run`) before it is judged; a
non-compiling mutant is INVALID, never SURVIVED, and a stale anchor aborts the
run instead of recording a verdict.
"""
import hashlib
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_panic import named_test_failed, named_test_killed
from negative_selection import (baseline_passed, claim_dir, missing_target,
                                 refuse, refuse_unusable_target, registered,
                                 select, verdict_exit)
from negative_process import mutating_sources, run_owned

root = Path(__file__).resolve().parents[5]
source = root / 'src/pilotdeck-gateway/src/channel/sms'
TEST_PREFIX = 'channel::sms::permission::tests::'

DEPTH_TEST = TEST_PREFIX + 'depth_past_threshold_truncates_without_panicking'
WIDE_TEST = TEST_PREFIX + 'wide_deep_payload_stays_bounded_after_first_truncation'

# (name, file, anchor, replacement, target test, kill marker in the payload)
MUTATIONS = [
    ('cap-removed', 'permission.rs',
     '        if depth >= MAX_STRINGIFY_DEPTH {\n            out.push_str("\\"...\\"");',
     '        if depth >= usize::MAX {\n            out.push_str("\\"...\\"");',
     DEPTH_TEST,
     '#1929 array chain at depth 256 must carry the truncation marker'),
    ('truncate-flag-dropped', 'permission.rs',
     '            out.push_str("\\"...\\"");\n            *truncated = true;',
     '            out.push_str("\\"...\\"");\n            ();',
     WIDE_TEST,
     '#1929 the wide-deep payload produced'),
]
EXPECTED_NAMES = ('cap-removed', 'truncate-flag-dropped')

try:
    registered(MUTATIONS, EXPECTED_NAMES)
    run_rows = select(MUTATIONS, sys.argv[2:], always=())
except ValueError as error:
    refuse(str(error))
env = os.environ.copy()
if missing_target(env):
    refuse('CARGO_TARGET_DIR must be supplied by the caller')
refuse_unusable_target()
out = claim_dir(sys.argv[1])
summary = []
failures = []


def cargo(test, extra):
    command = ['cargo', 'test', '--manifest-path', 'src/Cargo.toml',
               '-p', 'pilotdeck-gateway', '--lib', test, '--', '--exact']
    command.extend(extra)
    result = run_owned(command, cwd=root, env=env, timeout=600)
    return result.returncode, result.stdout + result.stderr


with mutating_sources(root, sorted({source / row[1] for row in run_rows})) as snapshots:
    original = snapshots[source / 'permission.rs']

    for test in dict.fromkeys(row[4] for row in run_rows):
        code, log = cargo(test, [])
        (out / f'baseline-{test.split("::")[-1]}.log').write_text(log)
        ok = code == 0 and baseline_passed(log, test)
        print(f'baseline {test}: {"PASS" if ok else "INVALID"}', flush=True)
        if not ok:
            failures.append('baseline-' + test)

    for name, filename, anchor, replacement, test, marker in run_rows:
        path = source / filename
        source_sha = hashlib.sha256(original).hexdigest()
        content = original.decode()
        try:
            occurrences = content.count(anchor)
            if occurrences != 1:
                raise ValueError(
                    f'{name}: anchor occurs {occurrences} times (expected 1)')
            (out / f'{name}.src').write_text(
                content.replace(anchor, replacement, 1))
            path.write_text(content.replace(anchor, replacement, 1))
            code, build = cargo(test, ['--no-run'])
            (out / f'{name}.build.log').write_text(build)
            if code != 0:
                raise RuntimeError(f'{name}: mutant does not compile')
            code, log = cargo(test, [])
            (out / f'{name}.log').write_text(log)
            killed = (code != 0 and named_test_failed(log, test)
                      and named_test_killed(log, test, marker))
            status = 'KILLED' if killed else (
                'SURVIVED' if code == 0 else 'INVALID')
            failure = next((line.strip() for line in log.splitlines()
                            if '#1929' in line), '')
            summary.append(dict(name=name, test=test, status=status,
                                returncode=code, failure=failure,
                                sourceSha256=source_sha))
            print(f'{name}: {status} rc={code} {failure}', flush=True)
            if status != 'KILLED':
                failures.append(name)
        except Exception as error:
            summary.append(dict(name=name, test=test, status='INVALID',
                                reason=f'{type(error).__name__}: {error}',
                                sourceSha256=source_sha))
            failures.append(name)
        finally:
            path.write_bytes(original)
            if hashlib.sha256(path.read_bytes()).hexdigest() != source_sha:
                raise RuntimeError(f'{name}: restore failed; the tree is dirty')
            (out / 'summary.json').write_text(
                json.dumps(summary, ensure_ascii=False, indent=2) + '\n')

counts = {state: sum(row['status'] == state for row in summary)
          for state in ('KILLED', 'SURVIVED', 'INVALID')}
print('summary: ' + ', '.join(f'{state}={count}'
                              for state, count in counts.items()), flush=True)
if failures:
    print(f'not killed: {", ".join(failures)}', file=sys.stderr)
killed_rows = sum(1 for row in summary if row['status'] == 'KILLED')
print(f'{killed_rows}/{len(run_rows)} killed; source restored byte-for-byte',
      flush=True)
sys.exit(verdict_exit(summary, key='status'))
