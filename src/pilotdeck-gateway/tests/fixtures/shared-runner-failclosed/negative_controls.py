#!/usr/bin/env python3
# anchor-selfcheck: unsupported #1076 shared-runner mutation executor; anchors are checked below
"""#1076 paired mutations for the shared negative-runner cancellation fix.

The two delivered Python executors keep their summary in memory until the
source guard restored and re-verified every guarded file.  The pre-fix shapes
published each row directly and caught only ``Exception``; a SIGINT or a raw
``SystemExit(256)`` in the guard's restore window therefore left an all-KILLED
``summary.json`` behind with an unnormalized exit.

#1076 fix-r1 adds the second half of the contract: the widened handler must
not turn a guard *refusal to run* (HEAD/baseline mismatch, busy lock, expired
anchor) into rc=2.  Refusals keep the historical rc=1 that callers already
assert; only a cancellation or an unverified restore is rc=2.  The four rows
added below pin both refusal phases (before and after the guard opened) for
both executors.

#1076 fix-r2 closes the unwinding hole: a body refusal followed by a *different*
exception from the guard's ``__exit__`` restore/re-verify is void -> rc=2, and
a cancellation raised inside the body (not only at guard exit) is rc=2 too.
The rows below pin the exception-identity check, the ``not
isinstance(error, Exception)`` cancellation branch, both phase transitions and
both halves of the ``('restore', 'exit')`` tuple for each executor.

Each mutant is syntax-compiled before its positive control runs.  A stale
anchor aborts the whole selected run as INVALID (never SURVIVED).  All selected
rows finish before the summary is published; any non-KILLED row is a nonzero
exit.  This script mutates tracked files, so it must run post-commit
(``mutating_sources`` refuses a working-tree baseline mismatch).  No cargo is
launched; the bound unittest imports the mutated Python module in a child
process.  ``RUNNER`` itself is not mutated, so an untracked pre-commit copy is
not part of the guarded set.
"""
import argparse
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, new_output_directory, run_owned

ROOT = Path(__file__).resolve().parents[5]
FIXTURES = ROOT / 'src/pilotdeck-gateway/tests/fixtures'
SUITE = FIXTURES / 'negative_suite.py'
SQLITE = FIXTURES / '_shared/negative_sqlite_header.py'

# (name, source, before, after, bound unittest, exact #1076 assertion message)
MUTANT_ROWS = [
    (
        'runner-baseexception-narrowed',
        SUITE,
        '\n    except BaseException as error:',
        '\n    except Exception as error:',
        'test_negative_suite.CancellationTests.test_guard_sigint_voids_last_kill_and_exits_2',
        '#1076 a SIGINT in the guard exit must return 2',
    ),
    (
        'runner-publish-before-verify',
        SUITE,
        "        print(f'{case.mutation}: {status}: {reason}', flush=True)",
        "        print(f'{case.mutation}: {status}: {reason}', flush=True)\n"
        "        publish_summary(out, results)",
        'test_negative_suite.CancellationTests.'
        'test_guard_systemexit_256_voids_last_kill_and_exits_2',
        '#1076 no summary may exist while the guard is unverified',
    ),
    (
        'runner-nonatomic-publish',
        SUITE,
        "    temporary.write_text(json.dumps({\n"
        "        'controls': results,\n"
        "        'counts': pending.counts(results),\n"
        "    }, indent=2) + '\\n')\n"
        "    temporary.replace(target)",
        "    target.write_text(json.dumps({\n"
        "        'controls': results,\n"
        "        'counts': pending.counts(results),\n"
        "    }, indent=2) + '\\n')",
        'test_negative_suite.CancellationTests.test_publish_summary_publishes_atomically',
        '#1076 an interrupted publish must preserve the previous summary',
    ),
    (
        'sqlite-baseexception-narrowed',
        SQLITE,
        '\n    except BaseException as error:',
        '\n    except Exception as error:',
        'test_negative_suite.SqliteHeaderCancellationTests.'
        'test_sqlite_guard_sigint_voids_last_kill_and_exits_2',
        '#1076 a sqlite SIGINT in the guard exit must return 2',
    ),
    (
        'sqlite-publish-in-loop',
        SQLITE,
        "                print(f'{name}: {status}: {reason}', flush=True)",
        "                print(f'{name}: {status}: {reason}', flush=True)\n"
        "                write_summary(output, results)",
        'test_negative_suite.SqliteHeaderCancellationTests.'
        'test_sqlite_guard_systemexit_256_voids_last_kill_and_exits_2',
        '#1076 no summary may exist while the sqlite guard is unverified',
    ),
    (
        'sqlite-nonatomic-publish',
        SQLITE,
        "    temporary.write_text(json.dumps({'controls': results, 'counts': counts}, "
        "indent=2) + '\\n')\n"
        "    temporary.replace(target)",
        "    target.write_text(json.dumps({'controls': results, 'counts': counts}, "
        "indent=2) + '\\n')",
        'test_negative_suite.SqliteHeaderCancellationTests.'
        'test_write_summary_publishes_atomically',
        '#1076 an interrupted publish must preserve the previous summary',
    ),
    (
        # fix-r1: the refusal branch must stay rc=1; a reviewer found that the
        # first delivery returned 2 here and broke a real caller assertion.
        'runner-refusal-exit-two',
        SUITE,
        "            return 2\n        return 1",
        "            return 2\n        return 2",
        'test_negative_suite.SuiteTests.test_guard_refusal_accounts_for_every_case',
        '#1076 a HEAD guard refusal must keep exit code 1',
    ),
    (
        'sqlite-refusal-exit-two',
        SQLITE,
        "            return 2\n        return 1",
        "            return 2\n        return 2",
        'test_negative_suite.SqliteHeaderExecutorTests.test_guard_refusal_keeps_exit_one',
        '#1076 a sqlite HEAD guard refusal must keep exit code 1',
    ),
    (
        # In-window refusals (expired anchor / malformed registration) are
        # refusals too: rc=1, never the restore/cancel rc=2.
        'runner-body-refusal-exit-two',
        SUITE,
        "        if (phase in ('restore', 'exit') or not isinstance(error, Exception)",
        "        if (phase in ('restore', 'exit', 'body') or not isinstance(error, Exception)",
        'test_negative_suite.SuiteTests.test_in_window_refusal_keeps_exit_one',
        '#1076 an in-window refusal must keep exit code 1',
    ),
    (
        'sqlite-body-refusal-exit-two',
        SQLITE,
        "        if (phase in ('restore', 'exit') or not isinstance(error, Exception)",
        "        if (phase in ('restore', 'exit', 'body') or not isinstance(error, Exception)",
        'test_negative_suite.SqliteHeaderExecutorTests.test_stale_anchor_refusal_keeps_exit_one',
        '#1076 a sqlite stale-anchor refusal must keep exit code 1',
    ),
    (
        # fix-r2 / Codex P1: the body already raised and the guard's own
        # restore/re-verify raises a *different* object while unwinding. The
        # identity check must keep that void -> rc=2 even though phase is 'body'.
        'runner-body-error-overwritten-exit-one',
        SUITE,
        "                or (body_error is not None and error is not body_error)):",
        "                or (False)):",
        'test_negative_suite.SuiteTests.'
        'test_body_refusal_with_guard_restore_failure_exits_two',
        '#1076 a body refusal plus an unverified guard exit must return 2',
    ),
    (
        'sqlite-body-error-overwritten-exit-one',
        SQLITE,
        "                or (body_error is not None and error is not body_error)):",
        "                or (False)):",
        'test_negative_suite.SqliteHeaderExecutorTests.'
        'test_stale_anchor_with_guard_restore_failure_exits_two',
        '#1076 a sqlite stale anchor plus an unverified guard exit must return 2',
    ),
    (
        # fix-r2: cancellation inside the body must stay rc=2 on its own, not
        # only while the guard is exiting.
        'runner-body-cancellation-exit-one',
        SUITE,
        "        if (phase in ('restore', 'exit') or not isinstance(error, Exception)",
        "        if (phase in ('restore', 'exit') or False",
        'test_negative_suite.SuiteTests.test_body_cancellation_keeps_exit_two',
        '#1076 a body cancellation with a clean guard exit must return 2',
    ),
    (
        'sqlite-body-cancellation-exit-one',
        SQLITE,
        "        if (phase in ('restore', 'exit') or not isinstance(error, Exception)",
        "        if (phase in ('restore', 'exit') or False",
        'test_negative_suite.SqliteHeaderExecutorTests.test_body_cancellation_keeps_exit_two',
        '#1076 a sqlite body cancellation must return 2',
    ),
    (
        # fix-r2: the per-row restore phase is what makes an unverified byte
        # restore rc=2; folding it back into 'body' must be killed.
        'runner-restore-phase-body',
        SUITE,
        "                        phase = 'restore'",
        "                        phase = 'body'",
        'test_negative_suite.SuiteTests.test_byte_restore_failure_stops_unsafe_mutation',
        '#1076 an unverified byte restore must exit 2',
    ),
    (
        'sqlite-restore-phase-body',
        SQLITE,
        "                        phase = 'restore'",
        "                        phase = 'body'",
        'test_negative_suite.SqliteHeaderExecutorTests.test_row_restore_failure_returns_two',
        '#1076 an unverified sqlite row restore must return 2',
    ),
    (
        # Both halves of the phase tuple are load-bearing: dropping 'exit'
        # hides a guard-exit restore failure, dropping 'restore' hides a
        # per-row restore failure.
        'runner-exit-branch-one',
        SUITE,
        "phase in ('restore', 'exit')",
        "phase in ('restore',)",
        'test_negative_suite.SuiteTests.test_guard_exit_invalidates_previous_kills',
        '#1076 guard restoration failure must exit 2',
    ),
    (
        'sqlite-exit-branch-one',
        SQLITE,
        "phase in ('restore', 'exit')",
        "phase in ('restore',)",
        'test_negative_suite.SqliteHeaderExecutorTests.'
        'test_guard_restore_failure_invalidates_all_completed_kills',
        '#1076 a failed guard exit must return exit code 2',
    ),
    (
        'runner-restore-branch-one',
        SUITE,
        "phase in ('restore', 'exit')",
        "phase in ('exit',)",
        'test_negative_suite.SuiteTests.test_byte_restore_failure_stops_unsafe_mutation',
        '#1076 an unverified byte restore must exit 2',
    ),
    (
        'sqlite-restore-branch-one',
        SQLITE,
        "phase in ('restore', 'exit')",
        "phase in ('exit',)",
        'test_negative_suite.SqliteHeaderExecutorTests.test_row_restore_failure_returns_two',
        '#1076 an unverified sqlite row restore must return 2',
    ),
]


def apply_mutation(name, original, before, after):
    """Return the mutant bytes, or fail closed on a stale/no-op anchor."""
    count = original.count(before)
    if count != 1:
        raise ValueError(f'#1076 {name} anchor expired: count={count}, expected=1')
    if before == after:
        raise ValueError(f'#1076 {name} is a no-op mutation')
    mutant = original.replace(before, after, 1)
    compile(mutant, f'{name}.py', 'exec')
    return mutant


def run_test(test, env):
    return run_owned([sys.executable, '-m', 'unittest', '-v', test],
                     cwd=FIXTURES, env=env, timeout=180)


def passed(result):
    log = result.stdout + result.stderr
    return result.returncode == 0 and 'Ran 1 test' in log and '\nOK' in log


def classify(result, test, message):
    log = result.stdout + result.stderr
    short = test.rsplit('.', 1)[-1]
    if result.returncode == 0:
        return 'SURVIVED', 'bound test passed the mutant'
    if f'FAIL: {short}' in log and message in log and 'AssertionError' in log:
        return 'KILLED', message
    return 'INVALID', f'failed outside the bound assertion: {message}'


def publish(output, results):
    counts = {state: sum(row['status'] == state for row in results)
              for state in ('KILLED', 'SURVIVED', 'INVALID')}
    target = output / 'summary.json'
    temporary = target.with_name(target.name + '.tmp')
    temporary.write_text(json.dumps({'controls': results, 'counts': counts},
                                    indent=2) + '\n')
    temporary.replace(target)
    print('summary: ' + ', '.join(f'{state}={count}' for state, count in counts.items()),
          flush=True)


def invalidate(results, reason):
    for row in results:
        row.update(status='INVALID', reason=row['reason'] + '; ' + reason)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path)
    parser.add_argument('--only', action='append')
    parser.add_argument('--check', action='store_true')
    args = parser.parse_args()
    known = {row[0] for row in MUTANT_ROWS}
    if args.only and (set(args.only) - known or len(args.only) != len(set(args.only))):
        print('unknown or duplicate --only name', file=sys.stderr)
        return 2
    selected = [row for row in MUTANT_ROWS if not args.only or row[0] in args.only]

    if args.check:
        for name, path, before, after, _test, _message in selected:
            apply_mutation(name, path.read_text(), before, after)
        print(f'{len(selected)} anchors: ok')
        return 0
    if args.output is None:
        print('--output is required unless --check is used', file=sys.stderr)
        return 2

    output = new_output_directory(args.output)
    env = dict(os.environ)
    env['PYTHONDONTWRITEBYTECODE'] = '1'
    # No cargo subprocess is launched: the bound unittest is a Python child.
    results = []
    try:
        with mutating_sources(ROOT, [SUITE, SQLITE]) as originals:
            texts = {path: originals[path].decode() for path in originals}
            # Preflight every selected anchor before any positive/mutant run:
            # an expired anchor aborts as INVALID, never SURVIVED.
            for name, path, before, after, _test, _message in selected:
                apply_mutation(name, texts[path], before, after)
            for name, path, before, after, test, message in selected:
                status, reason = 'INVALID', 'not executed'
                try:
                    if not passed(run_test(test, env)):
                        reason = 'positive control did not pass'
                    else:
                        path.write_text(texts[path].replace(before, after, 1))
                        status, reason = classify(run_test(test, env), test, message)
                except Exception as error:
                    status, reason = 'INVALID', f'{type(error).__name__}: {error}'
                finally:
                    path.write_bytes(originals[path])
                    if path.read_bytes() != originals[path]:
                        raise RuntimeError(f'#1076 byte restoration failed: {path}')
                try:
                    if not passed(run_test(test, env)):
                        status, reason = 'INVALID', reason + '; restored bound test did not pass'
                except Exception as error:
                    status, reason = (
                        'INVALID', reason + f'; restored: {type(error).__name__}: {error}')
                results.append({'mutation': name, 'test': test,
                                'status': status, 'reason': reason})
                print(f'{name}: {status}: {reason}', flush=True)
    except BaseException as error:
        reason = f'source guard: {type(error).__name__}: {error}'
        invalidate(results, reason)
        for name, _path, _before, _after, test, _message in selected[len(results):]:
            results.append({'mutation': name, 'test': test,
                            'status': 'INVALID', 'reason': reason})
        publish(output, results)
        if isinstance(error, SystemExit):
            raise SystemExit(2) from error
        return 2
    publish(output, results)
    return int(not results or any(row['status'] != 'KILLED' for row in results))


if __name__ == '__main__':
    raise SystemExit(main())
