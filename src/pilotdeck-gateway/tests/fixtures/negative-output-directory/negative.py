#!/usr/bin/env python3
"""#1060 Python meta-controls. Commit first; never bypass the HEAD guard.

Usage: CARGO_TARGET_DIR=<caller-owned> python3 -I -B negative.py <fresh-output>
No cargo runs here: the mutated Python helper is compiled before each test.
All controls are accounted for, including anchor/setup/restoration failures.
"""
import argparse
import json
import os
from pathlib import Path
import re
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import (exclusive_write, mutating_sources,
                              new_output_directory, run_owned, wait_for_slot)
from negative_suite import invalidate_completed

ROOT = Path(__file__).resolve().parents[5]
FIXTURES = ROOT / 'src/pilotdeck-gateway/tests/fixtures'
SOURCE = FIXTURES / 'negative_process.py'
TESTS = FIXTURES / 'test_negative_process.py'
PROBE = FIXTURES / 'negative-output-directory/process_probe.py'
cases = [
    ('missing-claim', SOURCE,
     '    # mkdir/iterdir are not an ownership decision:',
     '    return path\n    # mkdir/iterdir are not an ownership decision:',
     'test_mkdir_pause_has_exactly_one_winner',
     '#1060 mkdir loser must be rejected before work'),
    ('nonexclusive-claim', SOURCE,
     'with (path / ".claim").open("x", encoding="utf-8") as claim:',
     'with (path / ".claim").open("w", encoding="utf-8") as claim:',
     'test_empty_snapshot_has_exactly_one_winner',
     '#1060 iterdir loser must be rejected before work'),
    ('missing-pid', SOURCE,
     '{"pid": os.getpid(), "started_at": time.time(),',
     '{"pid": -1, "started_at": time.time(),',
     'test_missing_and_empty_paths_have_persistent_metadata',
     '#1060 claim identifies owner pid'),
    ('missing-start-time', SOURCE,
     '"started_at": time.time(),', '"started_at": 0,',
     'test_missing_and_empty_paths_have_persistent_metadata',
     '#1060 claim records start time'),
    ('missing-argv', SOURCE,
     '"argv": sys.argv}, claim)', '"argv": []}, claim)',
     'test_missing_and_empty_paths_have_persistent_metadata',
     '#1060 claim records invocation'),
]


def passed(result, test):
    log = result.stdout + result.stderr
    return (result.returncode == 0 and 'Ran 1 test in ' in log
            and f'{test} (__main__.OutputDirectoryClaim.{test}) ... ok' in log
            and log.rstrip().endswith('OK'))


def classify(result, test, assertion):
    if passed(result, test):
        return 'SURVIVED', 'bound test still passed'
    log = result.stdout + result.stderr
    # Python unittest, not libtest: only the actual AssertionError line counts.
    # A traceback quoting the assertion's source or any ERROR is not a kill.
    failures = [line for line in log.splitlines() if line.startswith('AssertionError: ')]
    if (result.returncode == 1 and 'Ran 1 test in ' in log
            and re.search(r'(?m)^FAIL: ' + re.escape(test) + r'\b', log)
            and not re.search(r'(?m)^ERROR:', log)
            and failures and all(line.endswith(assertion) for line in failures)
            and re.search(r'FAILED \(failures=\d+\)\s*$', log)):
        return 'KILLED', 'bound Python assertion failed after successful compilation'
    return 'INVALID', 'missing bound assertion failure (setup/error/timeout is not a kill)'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('output', type=Path)
    args = parser.parse_args()
    if not os.environ.get('CARGO_TARGET_DIR'):
        parser.error('CARGO_TARGET_DIR must be supplied by the caller')
    out = new_output_directory(args.output)
    results = []

    def run(label, test):
        # -B stops writes, while the fresh prefix also prevents same-size,
        # same-second mutants from loading a predecessor's timestamp pyc.
        result = run_owned([sys.executable, '-I', '-B', '-X',
                            'pycache_prefix=' + str(out / (label + '.pycache')),
                            str(TESTS),
                            'OutputDirectoryClaim.' + test], cwd=ROOT, timeout=90)
        exclusive_write(out / (label + '.log'), result.stdout + result.stderr)
        exclusive_write(out / (label + '.exit'), str(result.returncode) + '\n')
        return result

    # Each mutation owns a separate restore context; there is no hand-written
    # snapshot/restore or alternate Git baseline. The shared judge itself is
    # guarded as an input even though Python controls need a different runner.
    paths = [SOURCE, TESTS, PROBE, Path(__file__).resolve(), FIXTURES / 'negative_suite.py']
    for name, path, old, new, test, assertion in cases:
        status, reason, compiled = 'INVALID', '', False
        try:
            with mutating_sources(ROOT, paths) as originals:
                original = originals[path].decode()
                if original.count(old) != 1:
                    raise ValueError(f'#1060 stale anchor {name}: count={original.count(old)}')
                changed = original.replace(old, new, 1)
                if not passed(run(name + '-positive', test), test):
                    raise RuntimeError('#1060 positive control did not pass')
                path.write_text(changed)
                # Real Python compile of the on-disk mutant, without pycache.
                compile(path.read_bytes(), str(path), 'exec')
                compiled = True
                exclusive_write(out / (name + '-compile.log'), '#1060 Python compile PASS\n')
                status, reason = classify(run(name + '-negative', test), test, assertion)
            with mutating_sources(ROOT, paths):
                if not passed(run(name + '-restored', test), test):
                    raise RuntimeError('#1060 restored control did not pass')
        except Exception as error:
            status, reason = 'INVALID', f'{type(error).__name__}: {error}'
            # A restore/guard failure also revokes any previously green rows.
            invalidate_completed(results, '#1060 control/guard failure: ' + reason)
        results.append(dict(mutation=name, test=test, assertion=assertion,
                            status=status, reason=reason, compiled=compiled))
        print(f'{name}: {status}: {reason}', flush=True)
    counts = {state: sum(row['status'] == state for row in results)
              for state in ('KILLED', 'SURVIVED', 'INVALID')}
    exclusive_write(out / 'summary.json', json.dumps(
        dict(controls=results, counts=counts), indent=2) + '\n')
    print('summary: ' + ', '.join(f'{state}={count}' for state, count in counts.items()))
    return int(not results or any(row['status'] != 'KILLED' for row in results))


if __name__ == '__main__':
    raise SystemExit(main())
