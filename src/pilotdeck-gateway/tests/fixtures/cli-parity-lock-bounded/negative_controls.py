#!/usr/bin/env python3
"""#1064 cli-parity lease boundedness/cancellation apparatus mutations; require
the bound unit test to fail at its #1064 assertion, then restore byte-for-byte.

Mutates the Python lease apparatus (scripts/cli-parity/common.py), not Rust
sources: the compile check is ``py_compile`` and the bound tests are the offline
unit tests of the same apparatus (``scripts/cli-parity/test_cli_parity.py``).
Run with no other writers in this worktree, after the apparatus change is
committed (``mutating_sources`` refuses files that differ from HEAD by design).
Evidence goes in a new output folder. No cargo is involved, so no
CARGO_TARGET_DIR is required. Run all mutants to completion; any non-KILLED
row is a nonzero exit (a stale anchor or a broken run is INVALID, never
SURVIVED).

Kill criterion (the shared negative_panic standard, applied to unittest
output): the *named* test must have failed at its own assertion — its FAIL
block must carry an ``AssertionError:`` line containing the mutant's exclusive
#1064 marker. The bound tests deliberately arm a SIGALRM before every wait:
a mutant that restores an unbounded block makes the test fail at its own
bounded-wait assertion instead of hanging (a hang or a signal death is
INVALID, never KILLED). An abnormal exit (no unittest summary, an unexpected
exit code) is INVALID too.
"""
import argparse
import hashlib
import json
from pathlib import Path
import py_compile
import re
import sys
import tempfile

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, new_output_directory, run_owned

root = Path(__file__).resolve().parents[5]
common = root / 'scripts/cli-parity/common.py'

# Every mutant is applied to the original independently and names the unit
# test whose assertion it must turn red. Rows are (name, source, old, new) so
# the anchor_selfcheck preflight can statically resolve each row's path and
# verify the old anchor against the live file.
mutations = [
    # The guard acquisition goes back to a blocking flock: no caller deadline
    # can interrupt it (the #1023 r3 Codex P1-1 shape).
    ('guard-blocking',
     common,
     'fcntl.flock(guard, fcntl.LOCK_EX | fcntl.LOCK_NB)',
     'fcntl.flock(guard, fcntl.LOCK_EX)'),
    # The deadline never expires: the LOCK_NB poll loops forever.
    ('guard-deadline-removed',
     common,
     'remaining = deadline - time.monotonic()',
     "remaining = float('inf')"),
    # The release phase stops inheriting the caller's deadline (default 60s).
    ('release-guard-unbounded',
     common,
     "            # 释放阶段同样有界（#1064 P1-1）：卡死的兄弟不能让清理无限期阻塞；\n"
     "            # 超时抛 Blocked、lease 原样保留，交给 README 的人工流程。\n"
     '            with lock_guard(lock, timeout):',
     "            # 释放阶段同样有界（#1064 P1-1）：卡死的兄弟不能让清理无限期阻塞；\n"
     "            # 超时抛 Blocked、lease 原样保留，交给 README 的人工流程。\n"
     '            with lock_guard(lock):'),
    # The acquisition phase stops inheriting the caller's deadline (default 60s).
    ('acquisition-guard-unbounded',
     common,
     "    with lock_guard(lock, timeout):\n"
     "        # mkdir 本身在 try 覆盖内：落盘后的任何取消都进回收分支；而 mkdir 失败",
     "    with lock_guard(lock):\n"
     "        # mkdir 本身在 try 覆盖内：落盘后的任何取消都进回收分支；而 mkdir 失败"),
    # A cancelled acquisition no longer recycles the lease directory it
    # created: the #1023 r3 Codex P1-2 shape blocks every later run.
    ('acquisition-cleanup-removed',
     common,
     '            discard_unpublished_lease(lock, mine, identity)',
     '            pass  # mutant #1064: no acquisition cleanup'),
    # The cleanup no longer proves the directory identity: a replaced
    # directory gets deleted.
    ('cleanup-identity-ignored',
     common,
     '    elif (current.st_dev, current.st_ino) != (identity.st_dev, identity.st_ino):',
     '    elif False:'),
    # The cleanup deletes foreign owner bytes it cannot prove are its own.
    ('cleanup-foreign-owner-deleted',
     common,
     '        if published is not None and published != mine:',
     '        if False:'),
    # A pre-existing foreign directory is no longer exempted from the
    # cancellation cleanup: the empty lease a crashed predecessor left is
    # deleted instead of reported as held.
    ('exists-clause-widened',
     common,
     '        except FileExistsError as error:',
     '        except FileNotFoundError as error:'),
]
tests = {
    'guard-blocking': 'ContractTests.test_guard_wait_is_bounded_when_a_sibling_holds_it',
    'guard-deadline-removed': 'ContractTests.test_guard_wait_is_bounded_when_a_sibling_holds_it',
    'release-guard-unbounded': 'ContractTests.test_release_guard_wait_is_bounded_when_a_sibling_holds_it',
    'acquisition-guard-unbounded': 'ContractTests.test_guard_wait_is_bounded_when_a_sibling_holds_it',
    'acquisition-cleanup-removed': 'ContractTests.test_interrupt_during_acquisition_leaves_no_lease',
    'cleanup-identity-ignored': 'ContractTests.test_interrupt_retains_a_replaced_lock_directory',
    'cleanup-foreign-owner-deleted': 'ContractTests.test_interrupt_retains_foreign_owner_bytes',
    'exists-clause-widened': 'ContractTests.test_blocked_acquisition_keeps_a_foreign_empty_lease',
}
# The exclusive assertion message each mutant must turn red.
failure_fragments = {
    'guard-blocking': '#1064: guard acquisition must answer its deadline, not block forever',
    'guard-deadline-removed': '#1064: guard acquisition must answer its deadline, not block forever',
    'release-guard-unbounded': '#1064: guard release must answer its deadline, not block forever',
    'acquisition-guard-unbounded': '#1064: guard acquisition must answer its deadline, not block forever',
    'acquisition-cleanup-removed': '#1064: an interrupted acquisition must not leave a lease',
    'cleanup-identity-ignored': '#1064: a replaced directory must be retained, never deleted',
    'cleanup-foreign-owner-deleted': '#1064: a directory with foreign owner bytes must be retained',
    'exists-clause-widened': '#1064: an existing foreign lease must fail with the existing Blocked contract',
}

SEPARATOR = '-' * 70
REPORT_SEPARATOR = '=' * 70
BLOCK_HEADER = re.compile(r'^(FAIL|ERROR): (\S+) \(([^)]+)\)')
# The bound test runs as a script (``__main__``) or, on older Pythons, under
# its module name with the bare class in the parentheses.
MODULES = ('test_cli_parity.', '__main__.')


def passed(result, test):
    log = result.stdout + result.stderr
    return result.returncode == 0 and 'Ran 1 test' in log and '\nOK\n' in log


def unittest_blocks(log):
    """(kind, method, qualname, body) for every unittest FAIL:/ERROR: block.

    A block starts at its header line (a subTest description and the test's
    docstring may precede the opening separator), opens with a ``-``*70
    separator line, and closes at the next ``-``*70 separator, the ``=``*70
    report separator between consecutive failures, or the next block header;
    the body in between holds the traceback and the assertion."""
    lines = log.splitlines()
    blocks = []
    for index, line in enumerate(lines):
        header = BLOCK_HEADER.match(line)
        if not header:
            continue
        start = index + 1
        while start < len(lines) and lines[start] != SEPARATOR:
            start += 1
        start += 1
        body = []
        for later in lines[start:]:
            if later in (SEPARATOR, REPORT_SEPARATOR) or BLOCK_HEADER.match(later):
                break
            body.append(later)
        blocks.append((header.group(1), header.group(2), header.group(3), body))
    return blocks


def bound_failure_bodies(blocks, test):
    """The FAIL-block bodies of the bound test only.

    Python >= 3.11 prints the full dotted qualname in the parentheses; older
    versions print the class and keep the method outside. A failure of any
    other test, however similar, never counts."""
    owner, method = test.split('.', 1)
    names = {module + test for module in MODULES}
    classes = {module + owner for module in MODULES}
    return [body for kind, name, qualname, body in blocks
            if kind == 'FAIL'
            and (qualname in names or (qualname in classes and name == method))]


def classify(result, test, fragments):
    """(status, reason) for one finished negative run.

    KILLED requires the named test's own FAIL block to carry an
    ``AssertionError:`` whose message contains the mutant's exclusive marker:
    the marker must be the assertion's message, not a source echo elsewhere in
    the log. An abnormal exit — a signal, or no unittest summary — is INVALID,
    and so is a failure without the marker."""
    log = result.stdout + result.stderr
    if passed(result, test):
        return 'SURVIVED', 'bound test still passed'
    if result.returncode != 1:
        return 'INVALID', f'unexpected exit code {result.returncode}; not an assertion failure'
    if 'Ran 1 test' not in log or not re.search(r'^FAILED', log, re.MULTILINE):
        return 'INVALID', 'no unittest summary; the bound test crashed or never ran'
    named = bound_failure_bodies(unittest_blocks(log), test)
    if not named:
        return 'INVALID', 'the bound test did not fail at an assertion (no FAIL block)'
    for body in named:
        for index, line in enumerate(body):
            # unittest prints long diffs before the message, as continuation
            # lines of the exception — everything from the AssertionError
            # line to the block's end is the assertion's own message.
            if line.startswith('AssertionError: ') and fragments in '\n'.join(body[index:]):
                return 'KILLED', 'bound test failed at its #1064 assertion'
    return 'INVALID', 'bound test failed without the #1064 assertion (wrong failure point)'


def run_test(out, label, test):
    # -X pycache_prefix pins this invocation's bytecode cache to a fresh
    # directory: a pyc left next to a (restored) source can otherwise be
    # executed as the mutant whenever its recorded mtime and size match the
    # restored file — same-second, same-size mutants do exactly that, and a
    # "positive" or "restored" control would then judge mutant bytecode.
    with tempfile.TemporaryDirectory(prefix='negctl-pycache-') as pycache:
        result = run_owned([sys.executable, '-I', '-X', f'pycache_prefix={pycache}',
                            'scripts/cli-parity/test_cli_parity.py', test],
                           cwd=root, timeout=600)
    (out / f'{label}.log').write_text(result.stdout + result.stderr)
    return result


def compile_check(path):
    """A mutant that cannot even compile is not evidence.

    The bytecode goes to a throwaway file, never to the __pycache__ next to
    the source: that pyc outlives the mutant, and run_test's children must
    never find it (see run_test)."""
    with tempfile.TemporaryDirectory(prefix='negctl-compile-') as holder:
        py_compile.compile(str(path), cfile=str(Path(holder) / 'mutant.pyc'), doraise=True)


def persist(out, summary):
    counts = {state: sum(entry['status'] == state for entry in summary)
              for state in ('KILLED', 'SURVIVED', 'INVALID')}
    (out / 'summary.json').write_text(json.dumps(
        {'controls': summary, 'counts': counts}, indent=2) + '\n')


def record(out, summary, row):
    summary.append(row)
    persist(out, summary)
    print(f"{row['mutation']}: {row['status']}: {row['reason']}", flush=True)


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('output', type=Path)
    ap.add_argument('selection', nargs='*', help='mutation name; only those rows run')
    ap.add_argument('--baseline', default='HEAD',
                    help='tree to prove the sources pristine against (default HEAD)')
    args = ap.parse_args()
    rows = mutations
    if args.selection:
        rows = [mutation for mutation in mutations if mutation[0] in args.selection]
        assert len(rows) == len(args.selection), 'unknown mutation name'
    out = new_output_directory(args.output)
    summary = []
    try:
        with mutating_sources(root, [common], baseline=args.baseline) as snapshots:
            for name, path, old, new in rows:
                status, reason, stage, before = 'INVALID', '', 'anchor', None
                try:
                    original = snapshots[path]
                    before = hashlib.sha256(original).hexdigest()
                    content = original.decode()
                    # A stale anchor (source drifted, or a copy-paste duplicate)
                    # must abort this row; recording it as SURVIVED would hide a
                    # broken control.
                    if content.count(old) != 1:
                        raise AssertionError(
                            f'{name}: anchor occurs {content.count(old)} times (expected 1); '
                            'the anchor is stale and must be updated')
                    if not passed(run_test(out, name + '-positive', tests[name]), tests[name]):
                        status, reason = 'INVALID', 'positive control did not pass the bound test'
                    else:
                        stage = 'compile'
                        path.write_text(content.replace(old, new, 1))
                        compile_check(path)
                        stage = 'negative'
                        status, reason = classify(run_test(out, name + '-negative', tests[name]),
                                                 tests[name], failure_fragments[name])
                except Exception as error:
                    status, reason = 'INVALID', f'{stage}: {type(error).__name__}: {error}'
                finally:
                    path.write_bytes(snapshots[path])
                try:
                    stage = 'restored'
                    if not passed(run_test(out, name + '-restored', tests[name]), tests[name]):
                        status, reason = 'INVALID', reason + '; restored bound test did not pass'
                except Exception as error:
                    status, reason = 'INVALID', reason + f'; restored: {type(error).__name__}: {error}'
                record(out, summary, {'mutation': name, 'test': tests[name], 'status': status,
                                      'reason': reason, 'sourceSha256': before})
    except Exception as error:
        # A baseline/lock/restoration failure makes further mutation unsafe.
        # The rows flushed so far are unproven too: their restores happened
        # inside a guard that failed to verify, so the summary.json already
        # written saying KILLED claims kills the sources never made. Rewrite
        # the persisted rows INVALID before anything else.
        reason = f'source guard: {type(error).__name__}: {error}'
        for row in summary:
            row.update(status='INVALID', reason=row['reason'] + '; ' + reason)
        persist(out, summary)
        for name, _path, _old, _new in rows[len(summary):]:
            record(out, summary, {'mutation': name, 'test': tests[name], 'status': 'INVALID',
                                  'reason': reason, 'sourceSha256': None})
    counts = {state: sum(row['status'] == state for row in summary)
              for state in ('KILLED', 'SURVIVED', 'INVALID')}
    print('summary: ' + ', '.join(f'{state}={count}' for state, count in counts.items()), flush=True)
    raise SystemExit(int(not summary or any(row['status'] != 'KILLED' for row in summary)))


if __name__ == '__main__':
    main()
