#!/usr/bin/env python3
"""#1064 cli-parity lease boundedness/cancellation apparatus mutations; require
the bound unit test to fail at its #1064 assertion, then restore byte-for-byte.

Mutates the Python lease apparatus (scripts/cli-parity/common.py), not Rust
sources: the compile check is ``py_compile`` and the bound tests are the offline
unit tests of the same apparatus (``scripts/cli-parity/test_cli_parity.py``)
plus this runner's own self-checks (``test_negative_controls.py``). Run with no
other writers in this worktree, after the apparatus change is committed
(``mutating_sources`` refuses files that differ from HEAD by design). Evidence
goes in a new output folder. No cargo is involved, so no CARGO_TARGET_DIR is
required. Run all mutants to completion; any non-KILLED row is a nonzero exit (a
stale anchor or a broken run is INVALID, never SURVIVED): an INVALID row exits 2
(fix-r2 P1-1, the #1038/#1063 semantics — a broken run must not share SURVIVED's
"the apparatus held" exit 1), an empty row set exits 1 (fix-r2 P1 — an emptied or
fully filtered table is a zero denominator, never a pass; the selection gate
asserts unknown names before anything starts), and only all-KILLED is 0.

Kill criterion (the shared negative_panic standard, applied to unittest
output): the *named* test must have failed at its own assertion — its FAIL
block must carry an ``AssertionError:`` line containing the mutant's exclusive
#1064 marker. The bound tests deliberately arm a SIGALRM before every wait: a
mutant that restores an unbounded block makes the test fail at its own
bounded-wait assertion instead of hanging (a hang or a signal death is
INVALID, never KILLED). An abnormal exit (no unittest summary, an unexpected
exit code) is INVALID too.

Per-row results live in memory only (#1064 r1 P1-3, the #1059/#209 E1 family
semantics). ``summary.json`` is published exactly once, after
``mutating_sources`` restored every guarded file and verified it byte-for-byte
against the baseline; the payload goes to a sibling temp file and is renamed
into place (#166 r8). A failure or cancellation therefore leaves either no
``summary.json`` or an INVALID one, never an all-KILLED file whose sources were
not verifiably restored. Nothing in the target directory is deleted or renamed
before ``new_output_directory`` admits it (#1038 fix-r4): admission refuses any
non-empty directory with every byte kept, and an admitted directory is empty,
so a previous success cannot be inherited either. The guard exit catches
``BaseException``: a ``KeyboardInterrupt`` or ``SystemExit`` during the
restore/re-verify window voids the recorded rows
(``negative_suite.invalidate_completed``), publishes INVALID, and is re-raised
(non-Exception) or exits 2; if that void publish is itself interrupted by a
second SIGINT, no summary is published at all — missing means failure
(fail-closed), never the previous all-KILLED row set.
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
from negative_suite import invalidate_completed

root = Path(__file__).resolve().parents[5]
common = root / 'scripts/cli-parity/common.py'
test_script = root / 'scripts/cli-parity/test_cli_parity.py'
# Rows mutate this delivered runner and the bound test script itself; the
# runner self-checks live beside it (#1064 r1 P1-3, the #1059 runner-row shape;
# fix-r2 adds the test script for the real-SIGINT disposition rows).
RUNNER = Path(__file__).resolve()
SELF_TEST = RUNNER.with_name('test_negative_controls.py')

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
     "                # 释放阶段同样有界（#1064 P1-1）：卡死的兄弟不能让清理无限期阻塞；\n"
     "                # 超时抛 Blocked、lease 原样保留，交给 README 的人工流程。\n"
     '                with lock_guard(lock, timeout):',
     "                # 释放阶段同样有界（#1064 P1-1）：卡死的兄弟不能让清理无限期阻塞；\n"
     "                # 超时抛 Blocked、lease 原样保留，交给 README 的人工流程。\n"
     '                with lock_guard(lock):'),
    # The acquisition phase stops inheriting the caller's deadline (default 60s).
    ('acquisition-guard-unbounded',
     common,
     "        with lock_guard(lock, timeout):\n"
     "            # 「是否本次创建」以 identity 是否捕获为准：identity 只在 lock.mkdir()",
     "        with lock_guard(lock):\n"
     "            # 「是否本次创建」以 identity 是否捕获为准：identity 只在 lock.mkdir()"),
    # A cancelled acquisition no longer recycles the lease directory it
    # created: the #1023 r3 Codex P1-2 shape blocks every later run.
    ('acquisition-cleanup-removed',
     common,
     '                discard_unpublished_lease(lock, mine, identity, owner_identity, written)',
     '                pass  # mutant #1064: no acquisition cleanup'),
    # r1 P1-1: an unprovable lease (identity never captured) is deleted again
    # whenever the directory happens to be empty — the pre-fix shape that
    # removed a foreign empty lease left by a crashed predecessor.
    ('cleanup-identity-none-widened',
     common,
     '    if identity is None or (current.st_dev, current.st_ino) != (identity.st_dev, identity.st_ino):\n'
     '        return',
     '    if identity is not None and (current.st_dev, current.st_ino) != (identity.st_dev, identity.st_ino):\n'
     '        return'),
    # The cleanup no longer proves the directory identity: a replaced
    # directory gets deleted.
    ('cleanup-identity-compare-removed',
     common,
     '    if identity is None or (current.st_dev, current.st_ino) != (identity.st_dev, identity.st_ino):\n'
     '        return',
     '    if identity is None:\n'
     '        return'),
    # r1 P1-2: the whole owner proof is replaced by the pre-fix JSON comparator,
    # which treats unparseable or ``null`` foreign bytes as deletable.
    ('cleanup-owner-proof-restored',
     common,
     "        if owner_identity is None:\n"
     "            return\n"
     "        try:\n"
     "            owner = (lock / 'owner.json').lstat()\n"
     "        except OSError:\n"
     "            return\n"
     "        if (owner.st_dev, owner.st_ino) != (owner_identity.st_dev, owner_identity.st_ino):\n"
     "            return\n"
     "        try:\n"
     "            on_disk = (lock / 'owner.json').read_bytes()\n"
     "        except OSError:\n"
     "            return\n"
     "        if on_disk != written:\n"
     "            return\n",
     "        try:\n"
     "            published = json.loads((lock / 'owner.json').read_text())\n"
     "        except (OSError, ValueError):\n"
     "            published = None\n"
     "        if published is not None and published != mine:\n"
     "            return\n"),
    # The owner file inode is no longer proven: a byte-identical replacement
    # copy of our own owner.json gets deleted.
    ('cleanup-owner-identity-compare-removed',
     common,
     "        if (owner.st_dev, owner.st_ino) != (owner_identity.st_dev, owner_identity.st_ino):\n"
     "            return",
     "        if False:\n"
     "            return"),
    # r1 P1-2: the byte proof is faked to accept a fixed prefix-shaped blob, so
    # foreign bytes shaped exactly like this run's write prefix are deleted.
    ('cleanup-written-bytes-hardcoded',
     common,
     '        if on_disk != written:\n'
     '            return',
     '        if on_disk != b\'{"pid": 1\':\n'
     '            return'),
    # A pre-existing foreign directory is no longer exempted from the
    # cancellation cleanup: the empty lease a crashed predecessor left is
    # deleted instead of reported as held.
    ('exists-clause-widened',
     common,
     '        except FileExistsError as error:',
     '        except FileNotFoundError as error:'),
    # r1 P2-1: the acquisition failure path leaks the directory fd instead of
    # closing it on every exit.
    ('acquisition-fd-leaked',
     common,
     "                if directory is not None:\n"
     "                    os.close(directory)\n"
     "                    directory = None\n"
     "                raise",
     "                if directory is not None:\n"
     "                    directory = None\n"
     "                raise"),
    # r1 concurrency P2-1: the outer finally stops covering the window between
    # the guard critical section and the body, so a cancellation there leaves a
    # published lease behind.
    ('release-finally-skipped',
     common,
     "        if directory is not None:\n"
     "            # 不论正常还是任何异常（含 KeyboardInterrupt）：只有本次启动的进程组全部被 stop_group 证实停止，才释放锁",
     "        if False:\n"
     "            # 不论正常还是任何异常（含 KeyboardInterrupt）：只有本次启动的进程组全部被 stop_group 证实停止，才释放锁"),
    # r1 P1-3: the runner guard catches only Exception, so a cancellation
    # (KeyboardInterrupt / SystemExit) during the restore/re-verify window
    # skips the INVALID publication and leaves no final summary.
    ('runner-baseexception-narrowed',
     RUNNER,
     "    except BaseException as error:\n",
     "    except Exception as error:\n"),
    # r1 P1-3: the pre-fix "publish each row, verify later" order — record()
    # writes summary.json while the guarded restore is still unverified, so a
    # SIGINT in the later void publish leaves that all-KILLED file behind.
    ('runner-publish-before-verify',
     RUNNER,
     "    summary.append(row)\n"
     "    print(f\"{row['mutation']}: {row['status']}: {row['reason']}\", flush=True)",
     "    summary.append(row)\n"
     "    print(f\"{row['mutation']}: {row['status']}: {row['reason']}\", flush=True)\n"
     "    write_summary(out, summary)"),
    # r1 P1-3: a SystemExit escaping the guard is re-raised raw instead of the
    # normalized exit 2 (a string code would exit 1, deciding how a coordinator
    # reads this run by accident).
    ('runner-exit-normalization-removed',
     RUNNER,
     "        if isinstance(error, SystemExit):\n"
     "            raise SystemExit(2) from error\n"
     "        if not isinstance(error, Exception):\n"
     "            raise\n"
     "        return 2",
     "        if not isinstance(error, Exception):\n"
     "            raise\n"
     "        return 2"),
    # fix-r2 P1-1: a normal-path INVALID row (a failed positive control) shares
    # SURVIVED's exit 1 instead of the broken-run exit 2.
    ('runner-invalid-exit-shared',
     RUNNER,
     "    if counts['INVALID']:\n"
     "        return 2\n",
     "    if counts['INVALID']:\n"
     "        return 1\n"),
    # fix-r2 P1: an empty row set (an emptied table) is judged a pass again —
    # the zero-denominator shape the old ``0 == 0`` comparison returned 0 for.
    ('runner-empty-summary-pass',
     RUNNER,
     "    if not summary:\n"
     "        return 1\n"
     "    return 0 if counts['KILLED'] == len(summary) else 1",
     "    return 0 if counts['KILLED'] == len(summary) else 1"),
    # fix-r2 P1: the selection gate no longer fails closed on an unknown name,
    # so a fully filtered selection silently runs an empty table.
    ('runner-selection-assert-removed',
     RUNNER,
     "        assert len(rows) == len(args.selection), (\n"
     "            f'#1064 unknown mutation name(s): {sorted(set(args.selection) - {m[0] for m in mutations})}')",
     "        pass  # mutant #1064 fix-r2: a fully filtered selection runs nothing"),
    # fix-r2 P1-2: the real-SIGINT windows stop installing the default SIGINT
    # disposition, so a background parent's inherited SIG_IGN swallows the
    # signal and the bound tests misjudge a correct implementation.
    ('real-sigint-handler-removed',
     test_script,
     "        previous = signal.signal(signal.SIGINT, signal.default_int_handler)\n"
     "        self.addCleanup(signal.signal, signal.SIGINT, previous)\n",
     "        pass  # mutant #1064 fix-r2: keep whatever disposition was inherited\n"),
]
tests = {
    'guard-blocking': 'ContractTests.test_guard_wait_is_bounded_when_a_sibling_holds_it',
    'guard-deadline-removed': 'ContractTests.test_guard_wait_is_bounded_when_a_sibling_holds_it',
    'release-guard-unbounded': 'ContractTests.test_release_guard_wait_is_bounded_when_a_sibling_holds_it',
    'acquisition-guard-unbounded': 'ContractTests.test_guard_wait_is_bounded_when_a_sibling_holds_it',
    'acquisition-cleanup-removed': 'ContractTests.test_interrupt_during_acquisition_leaves_no_lease',
    'cleanup-identity-none-widened': 'ContractTests.test_interrupt_before_identity_capture_retains_the_lease',
    'cleanup-identity-compare-removed': 'ContractTests.test_interrupt_retains_a_replaced_lock_directory',
    'cleanup-owner-proof-restored': 'ContractTests.test_interrupt_retains_foreign_owner_bytes',
    'cleanup-owner-identity-compare-removed': 'ContractTests.test_interrupt_retains_a_replaced_owner_file',
    'cleanup-written-bytes-hardcoded': 'ContractTests.test_interrupt_retains_unproven_owner_bytes',
    'exists-clause-widened': 'ContractTests.test_blocked_acquisition_keeps_a_foreign_empty_lease',
    'acquisition-fd-leaked': 'ContractTests.test_owner_conflict_closes_the_directory_fd_and_retains_the_lease',
    'release-finally-skipped': 'ContractTests.test_interrupt_between_guard_and_body_releases_the_lease',
    'runner-baseexception-narrowed': 'RunnerInvalidationControls.test_final_guard_interrupt_voids_and_reraises',
    'runner-publish-before-verify': 'RunnerInvalidationControls.test_interrupt_during_void_publish_leaves_no_killed_summary',
    'runner-exit-normalization-removed': 'RunnerInvalidationControls.test_final_guard_systemexit_is_normalized_to_two',
    'runner-invalid-exit-shared': 'RunnerInvalidationControls.test_normal_invalid_row_exits_two',
    'runner-empty-summary-pass': 'RunnerInvalidationControls.test_empty_mutation_table_is_not_a_pass',
    'runner-selection-assert-removed': 'RunnerInvalidationControls.test_fully_filtered_selection_fails_closed',
    'real-sigint-handler-removed': 'ContractTests.test_real_sigint_windows_survive_a_sigint_ignoring_parent',
}
# The exclusive assertion message each mutant must turn red.
failure_fragments = {
    'guard-blocking': '#1064: guard acquisition must answer its deadline, not block forever',
    'guard-deadline-removed': '#1064: guard acquisition must answer its deadline, not block forever',
    'release-guard-unbounded': '#1064: guard release must answer its deadline, not block forever',
    'acquisition-guard-unbounded': '#1064: guard acquisition must answer its deadline, not block forever',
    'acquisition-cleanup-removed': '#1064: an interrupted acquisition must not leave a lease',
    'cleanup-identity-none-widened': '#1064: an unprovable lease must be retained, never deleted',
    'cleanup-identity-compare-removed': '#1064: a replaced directory must be retained, never deleted',
    'cleanup-owner-proof-restored': '#1064: a directory with foreign owner bytes must be retained',
    'cleanup-owner-identity-compare-removed': '#1064: a replaced owner file must be retained, never deleted',
    'cleanup-written-bytes-hardcoded': '#1064: a lease with unproven owner bytes must be retained',
    'exists-clause-widened': '#1064: an existing foreign lease must fail with the existing Blocked contract',
    'acquisition-fd-leaked': '#1064: the directory fd must be closed on every acquisition failure',
    'release-finally-skipped': '#1064: a cancellation between the guard and the body must still release the lease',
    'runner-baseexception-narrowed': '#1064 r1 P1-3 a cancellation after the last row must not keep KILLED',
    'runner-publish-before-verify': '#1064 r1 P1-3 an interrupt during the void publish must not leave a readable KILLED summary',
    'runner-exit-normalization-removed': '#1064 r1 P1-3 a SystemExit from the guard must exit 2, never the raw code',
    'runner-invalid-exit-shared': "#1064 fix-r2 an INVALID row must exit 2, never share SURVIVED's 1",
    'runner-empty-summary-pass': '#1064 fix-r2 an empty row set must exit nonzero, never pass',
    'runner-selection-assert-removed': '#1064 fix-r2 an unknown selection must fail closed, never run an empty table',
    'real-sigint-handler-removed': '#1064 fix-r2 the real-SIGINT windows must install the default disposition themselves',
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


def script_for(test):
    """The bound script of a test: the runner self-checks live beside this
    runner; every apparatus test runs from scripts/cli-parity/."""
    if test.startswith('RunnerInvalidationControls.'):
        return SELF_TEST
    return test_script


def run_test(out, label, test):
    # -X pycache_prefix pins this invocation's bytecode cache to a fresh
    # directory: a pyc left next to a (restored) source can otherwise be
    # executed as the mutant whenever its recorded mtime and size match the
    # restored file — same-second, same-size mutants do exactly that, and a
    # "positive" or "restored" control would then judge mutant bytecode.
    with tempfile.TemporaryDirectory(prefix='negctl-pycache-') as pycache:
        result = run_owned([sys.executable, '-I', '-X', f'pycache_prefix={pycache}',
                            str(script_for(test)), test],
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


def _counts(summary):
    return {state: sum(entry['status'] == state for entry in summary)
            for state in ('KILLED', 'SURVIVED', 'INVALID')}


def write_summary(out, summary):
    """Publish the one final summary atomically.

    This is called exactly once per normal run, after every guarded file was
    restored and byte-verified, and once from the failure handler (INVALID).
    The payload keeps the ``{'controls': ..., 'counts': ...}`` shape this
    suite's readers expect; a reader that checks "the file exists" must never
    observe a half-written JSON document, so the payload goes to a sibling
    temp file and is renamed over ``summary.json`` (#166 r8).
    """
    payload = json.dumps({'controls': summary, 'counts': _counts(summary)},
                         indent=2) + '\n'
    target = out / 'summary.json'
    tmp = out / 'summary.json.tmp'
    tmp.write_text(payload)
    tmp.replace(target)


def record(out, summary, row):
    """Append a row in memory; the disk summary is published once, later.

    The outcome of an earlier row must never be readable as the final result
    while the guarded restore is still unverified (#1064 r1 P1-3).
    """
    summary.append(row)
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
        assert len(rows) == len(args.selection), (
            f'#1064 unknown mutation name(s): {sorted(set(args.selection) - {m[0] for m in mutations})}')
    # Nothing is deleted or renamed here: admission (new_output_directory)
    # refuses a non-empty directory with every byte kept, and an admitted
    # directory is always empty, so a previous success can never be inherited
    # (#1038 fix-r4 P0 semantics).
    out = new_output_directory(args.output)
    summary = []
    try:
        # RUNNER and the bound test script are guarded too: six rows mutate
        # this delivered runner (the r1 P1-3 publication rules and the fix-r2
        # exit-code/selection gates) and one mutates the test script's
        # real-SIGINT disposition install (fix-r2 P1-2).
        with mutating_sources(root, [RUNNER, common, test_script],
                              baseline=args.baseline) as snapshots:
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
    except BaseException as error:
        # A baseline/lock/restoration failure makes further mutation unsafe.
        # The rows recorded so far are unproven too: their restores happened
        # inside a guard that failed to verify, so an all-KILLED summary would
        # claim kills the sources never made (#979/#1037 shared semantics).
        # Void them in memory, account for the remaining rows, and publish the
        # INVALID summary — the disk never held a final summary before this
        # point (record() is memory only), so an interrupt inside this publish,
        # even a second SIGINT, leaves no summary at all instead of the old
        # all-KILLED file. Missing summary is the fail-closed signal.
        reason = f'source guard: {type(error).__name__}: {error}'
        invalidate_completed(summary, reason)
        for name, _path, _old, _new in rows[len(summary):]:
            record(out, summary, {'mutation': name, 'test': tests[name], 'status': 'INVALID',
                                  'reason': reason, 'sourceSha256': None})
        write_summary(out, summary)
        # The INVALID summary is on disk, so the exit is normalized instead of
        # judged by the raw code: 256/-256 truncate to a zero process status
        # on POSIX and a string code exits 1, none of which may decide how a
        # coordinator reads this run (#1059 fix-r2 shape).
        if isinstance(error, SystemExit):
            raise SystemExit(2) from error
        if not isinstance(error, Exception):
            raise
        return 2
    # Reaching here means mutating_sources restored every guarded file and
    # verified it against the baseline. Only now may a final summary exist.
    write_summary(out, summary)
    counts = _counts(summary)
    print('summary: ' + ', '.join(f'{state}={count}' for state, count in counts.items()), flush=True)
    # fix-r2 P1-1: an INVALID row means the run itself is broken (a stale
    # anchor, a crashed bound test, an unproven restore): it must not share
    # SURVIVED's exit 1, which reads as "the apparatus held" (#1038/#1063).
    if counts['INVALID']:
        return 2
    # fix-r2 P1: an empty row set is a zero denominator, never a pass — an
    # emptied or fully filtered table fails closed like a missing summary.
    if not summary:
        return 1
    return 0 if counts['KILLED'] == len(summary) else 1


if __name__ == '__main__':
    raise SystemExit(main())
