#!/usr/bin/env python3
"""#1929 depth/width-cap mutants: each must break a `#1929`-labelled assertion.

Mutations are applied to `channel/sms/permission.rs` only, through
`mutating_sources` (tree must equal HEAD, so this runs post-commit on the
integration host). Run with CARGO_TARGET_DIR set:

    python3 negative_controls.py <output-dir> [name ...]

Every mutant is compiled (`cargo test --no-run`) before it is judged; a
non-compiling mutant is INVALID, never SURVIVED, and a stale anchor aborts the
run instead of recording a verdict. The unmutated target test must be green
first: a red baseline is recorded as an `INVALID` row (so it reaches the exit
code) and its mutants are not run at all. `summary.json` therefore can hold
baseline rows besides mutant rows — the `baselines:` line separates them.
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
OBJECT_TEST = TEST_PREFIX + 'object_siblings_stop_after_the_first_truncation'
BYTE_TEST = TEST_PREFIX + 'wide_shallow_payload_stays_within_the_byte_budget'
OVERSIZED_TEST = TEST_PREFIX + 'oversized_leaf_and_key_stay_within_the_byte_budget'
CAP = 'if depth >= MAX_STRINGIFY_DEPTH || out.len() >= MAX_STRINGIFY_BYTES {'
STRING_PUSH = ('                push_budgeted('
               '&pilotdeck_protocol::quote_json_string(text), out, truncated);')
KEY_PUSH = ('                    push_budgeted('
            '&pilotdeck_protocol::quote_json_string(key), out, truncated);')

# (name, file, anchor, replacement, target test, kill marker in the payload)
MUTATIONS = [
    ('cap-removed', 'permission.rs', CAP,
     'if depth >= usize::MAX || out.len() >= MAX_STRINGIFY_BYTES {',
     DEPTH_TEST,
     '#1929 array chain at depth 256 must carry the truncation marker'),
    ('cap-off-by-one', 'permission.rs', CAP,
     'if depth > MAX_STRINGIFY_DEPTH || out.len() >= MAX_STRINGIFY_BYTES {',
     DEPTH_TEST,
     '#1929 array chain at depth 256 must carry the truncation marker'),
    ('byte-budget-removed', 'permission.rs', CAP,
     'if depth >= MAX_STRINGIFY_DEPTH || out.len() >= usize::MAX {',
     BYTE_TEST,
     '#1929 the wide payload must carry the truncation marker'),
    ('truncate-flag-dropped', 'permission.rs',
     '            out.push_str("\\"...\\"");\n            *truncated = true;',
     '            out.push_str("\\"...\\"");\n            ();',
     WIDE_TEST,
     '#1929 the wide-deep payload produced'),
    ('object-break-dropped', 'permission.rs',
     '                    write(value, depth + 1, out, truncated);\n'
     '                    if *truncated {\n                        break;',
     '                    write(value, depth + 1, out, truncated);\n'
     '                    if *truncated {\n                        ();',
     OBJECT_TEST,
     '#1929 an elided object must not name its following key'),
    ('string-budget-dropped', 'permission.rs', STRING_PUSH,
     '                out.push_str('
     '&pilotdeck_protocol::quote_json_string(text));',
     OVERSIZED_TEST,
     '#1929 the oversized leaf must carry the truncation marker'),
    ('key-budget-dropped', 'permission.rs', KEY_PUSH,
     '                    out.push_str('
     '&pilotdeck_protocol::quote_json_string(key));',
     OVERSIZED_TEST,
     '#1929 the oversized key produced'),
]
EXPECTED_NAMES = ('cap-removed', 'cap-off-by-one', 'byte-budget-removed',
                  'truncate-flag-dropped', 'object-break-dropped',
                  'string-budget-dropped', 'key-budget-dropped')

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
    # Cargo-level flags are mutually exclusive with the libtest tail: `extra`
    # (only `--no-run` today) must precede `--`, otherwise cargo hands it to
    # the test binary and every mutant is judged "does not compile" (#1929).
    command = ['cargo', 'test', '--manifest-path', 'src/Cargo.toml',
               '-p', 'pilotdeck-gateway', '--lib']
    command.extend(extra if extra else [test, '--', '--exact'])
    result = run_owned(command, cwd=root, env=env, timeout=600)
    return result.returncode, result.stdout + result.stderr


with mutating_sources(root, sorted({source / row[1] for row in run_rows})) as snapshots:
    original = snapshots[source / 'permission.rs']

    # #819: every unmutated target test must be green before any mutant
    # verdict counts. A red baseline is also recorded as its own INVALID row so
    # it reaches the exit code — listing it in `failures` alone left the run
    # exiting 0 while nothing had been proven (#1929 review P0).
    baselines = {}
    for test in dict.fromkeys(row[4] for row in run_rows):
        short = test.split('::')[-1]
        reason = 'unmutated target test did not pass'
        try:
            code, log = cargo(test, [])
        except Exception as error:  # a timeout must still leave a verdict
            code, log = None, ''
            reason = f'baseline run raised {type(error).__name__}: {error}'
        logged = True
        try:
            (out / f'baseline-{short}.log').write_text(log)
        except OSError as error:
            # A baseline whose raw log cannot be archived is not evidence:
            # record it as INVALID instead of trusting the in-memory verdict.
            logged = False
            reason = f'{reason}; baseline log not written: {error}'
        baselines[test] = logged and code == 0 and baseline_passed(log, test)
        print(f'baseline {test}: '
              f'{"PASS" if baselines[test] else "INVALID"}', flush=True)
        if not baselines[test]:
            baseline_name = 'baseline-' + short
            failures.append(baseline_name)
            summary.append(dict(name=baseline_name, test=test,
                                status='INVALID', reason=reason))

    for name, filename, anchor, replacement, test, marker in run_rows:
        # A mutant judged against a test that is red without it proves
        # nothing: never count it, and never mutate the source for it.
        if not baselines.get(test):
            print(f'{name}: INVALID baseline failed; mutant not run', flush=True)
            summary.append(dict(name=name, test=test, status='INVALID',
                                reason='baseline failed; mutant not run'))
            failures.append(name)
            continue
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
            # #1087 fix-r5 (simplify #2): the FAILED summary requirement
            # moved into the shared named_test_killed default
            # (require_summary=True).
            killed = (code != 0
                      and named_test_failed(log, test)
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
baseline_invalid = sum(1 for ok in baselines.values() if not ok)
print(f'baselines: PASS={len(baselines) - baseline_invalid} '
      f'INVALID={baseline_invalid} (their INVALID rows are included in the '
      'counts below)', flush=True)
(out / 'summary.json').write_text(
    json.dumps(summary, ensure_ascii=False, indent=2) + '\n')
print('summary: ' + ', '.join(f'{state}={count}'
                              for state, count in counts.items()), flush=True)
if failures:
    print(f'not killed: {", ".join(failures)}', file=sys.stderr)
killed_rows = sum(1 for row in summary if row['status'] == 'KILLED')
print(f'{killed_rows}/{len(run_rows)} killed; source restored byte-for-byte',
      flush=True)
sys.exit(verdict_exit(summary, key='status'))
