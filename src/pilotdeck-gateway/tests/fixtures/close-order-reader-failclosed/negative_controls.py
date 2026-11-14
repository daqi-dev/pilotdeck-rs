#!/usr/bin/env python3
# anchor-selfcheck: unsupported #3043 close-order pin reader-grid mutation meta-controls; anchors are checked by --check below
"""#3043 paired mutations for the close-order pin runner's fourth grid cell.

The #1800 test file ``sdk-signals-close-order-pin/test_negative_controls.py``
pins the runner's exit contract as a state-by-reader grid; its #3043 addition
is the fourth cell — an aborting run read by a real ``head -1`` subprocess
through a real OS pipe, whose EPIPE lands on the abort accounting's own
``record()`` print (the nested ``except BaseException as nested`` handler in
``sdk-signals-close-order-pin/negative_controls.py``), which must fail closed:
a stderr diagnostic, rc 2, and no summary at all. Until #3043 that cell had
no pin anywhere in the repo, and no text-injection fake could express it (the
counts-line fakes key on ``'"counts"'``, which a record line never contains).

Each row below re-opens one half of that cell in the runner and requires the
bound test to fail at its own #3043 assertion:

  - accounting-epipe-continued: the nested handler swallows the accounting
    failure, so the run resumes the tail and publishes a summary — it
    "completes" over an accounting that never happened;
  - accounting-diagnostic-dropped: the handler still fails closed but names
    nothing on stderr — a silent rc 2 over a reader that left;
  - accounting-resumes-after-diagnosis: the handler diagnoses and then falls
    through instead of exiting, so the tail's own "#1382 run did not
    complete" appears and the summary is still published;
  - accounting-exit-green / accounting-exit-survived-code: the fail-closed
    code 2 is rerouted to 0 (a coordinator reads all green) or to 1 (it
    reads "a control survived") — the rc shape, both directions;
  - record-epipe-guarded: ``record()`` itself swallows print failures, so the
    EPIPE never reaches the accounting — the same resume shape one level
    down, which leaves the handler's own branches untested.

Every selected row runs to completion (no abort on the first failure); a
stale anchor, a mutant that does not parse, or a bound test that fails
anywhere other than its #3043 assertion is INVALID, never KILLED, and any
non-KILLED row is a nonzero exit. Each mutant is compiled before its bound
test runs, and the bound test must pass on the pristine source first (the
positive control) and again after restoration.

Judging is the canonical #991/#1060 four-condition judge set, copied verbatim
from ``gateway-entry-guard/negative_controls.py`` (fix-r4 a4): the bound
test's own assertion block only -- both observed unittest description
layouts, exactly one ``Ran 1 test`` line, rc 1 plus ``FAILED (failures=N)``
with no ``ERROR:``. A skip-shaped green log is not a pass, and the marker
echoed anywhere but the bound test's own AssertionError block is not a kill.
Each mutant is published and restored through
``negative_process.write_source_atomic`` -- one rename per transition, never
a truncating in-place write (the #1903 SIGKILL half-file hazard; the same
#3088 fix-r6 shape) -- so a full run performs exactly 2N atomic writes on
the runner.

This script mutates tracked files, so it must run post-commit
(``mutating_sources`` refuses a working-tree baseline mismatch). No cargo is
launched: the bound unittest drives the runner over a temporary tree in
child processes, so neither a slot nor a build directory is needed (the
sibling ``shared-runner-failclosed`` shape).
"""
import argparse
import json
import re
from pathlib import Path
import os
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import (mutating_sources, new_output_directory,
                              run_owned, write_source_atomic)
from negative_selection import refuse, registered

ROOT = Path(__file__).resolve().parents[5]
RUNNER = ROOT / 'src/pilotdeck-gateway/tests/fixtures/sdk-signals-close-order-pin/negative_controls.py'
TEST = ROOT / 'src/pilotdeck-gateway/tests/fixtures/sdk-signals-close-order-pin/test_negative_controls.py'

# #2133: neg-for-diff.sh derives the host's negative-control list from the
# diff and matches tables by the literal ``dir/file`` key of every changed
# file. The imports above spell only module names, so neither executor's key
# reaches this text without an anchor: a change to ``negative_process`` or to
# ``negative_selection`` (fix-r2 made the roster guard depend on its
# ``registered()``) could not schedule this table — it was registered blind
# until fix-r3 measured exactly these two keys and moved it into ``TABLES``.
DISCOVERY_ANCHORS = (
    'fixtures/negative_process.py',
    'fixtures/negative_selection.py',
)
BOUND = ('RealPipeReaderTests.'
         'test_cancelled_run_read_by_a_real_head_fails_closed_in_the_accounting')

# The three bound #3043 assertion prefixes of the fourth-cell test, in the
# order the test checks them: the rc shape, the stderr diagnostic, and the
# fail-closed-before-the-tail discriminator.
MESSAGE_RC = ('#3043 an aborting run whose accounting record line hits a '
              'closed pipe must still exit 2')
MESSAGE_STDERR = ('#3043 the EPIPE on the abort accounting record line must '
                  'be diagnosed on stderr')
MESSAGE_TAIL = ('#3043 the accounting failure must fail closed before the '
                'tail')

# (name, before, after, bound assertion message)
ROWS = [
    (
        'accounting-epipe-continued',
        '    except BaseException as nested:\n'
        '        _stderr(f"#1382 abort accounting failed: '
        '{type(nested).__name__}: {nested}")\n'
        '        os._exit(2)\n',
        '    except BaseException as nested:\n'
        '        pass\n',
        MESSAGE_STDERR,
    ),
    (
        'accounting-diagnostic-dropped',
        '    except BaseException as nested:\n'
        '        _stderr(f"#1382 abort accounting failed: '
        '{type(nested).__name__}: {nested}")\n'
        '        os._exit(2)\n',
        '    except BaseException as nested:\n'
        '        os._exit(2)\n',
        MESSAGE_STDERR,
    ),
    (
        'accounting-resumes-after-diagnosis',
        '    except BaseException as nested:\n'
        '        _stderr(f"#1382 abort accounting failed: '
        '{type(nested).__name__}: {nested}")\n'
        '        os._exit(2)\n',
        '    except BaseException as nested:\n'
        '        _stderr(f"#1382 abort accounting failed: '
        '{type(nested).__name__}: {nested}")\n',
        MESSAGE_TAIL,
    ),
    (
        'accounting-exit-green',
        '    except BaseException as nested:\n'
        '        _stderr(f"#1382 abort accounting failed: '
        '{type(nested).__name__}: {nested}")\n'
        '        os._exit(2)\n',
        '    except BaseException as nested:\n'
        '        _stderr(f"#1382 abort accounting failed: '
        '{type(nested).__name__}: {nested}")\n'
        '        os._exit(0)\n',
        MESSAGE_RC,
    ),
    (
        'accounting-exit-survived-code',
        '    except BaseException as nested:\n'
        '        _stderr(f"#1382 abort accounting failed: '
        '{type(nested).__name__}: {nested}")\n'
        '        os._exit(2)\n',
        '    except BaseException as nested:\n'
        '        _stderr(f"#1382 abort accounting failed: '
        '{type(nested).__name__}: {nested}")\n'
        '        os._exit(1)\n',
        MESSAGE_RC,
    ),
    (
        'record-epipe-guarded',
        '    print(f"{name}: {status}: {reason}", flush=True)\n',
        '    try:\n'
        '        print(f"{name}: {status}: {reason}", flush=True)\n'
        '    except BaseException:\n'
        '        pass\n',
        MESSAGE_STDERR,
    ),
]

# #3043 fix-r2（sibling#3 + boundary#1 + simplify#1 三角色同撞）：配对
# 变异表的分母是名册，不是行数。fix-r1 的 EXPECTED_ROWS = 6 只比长度：
# 同数替换一行后 `--check` 照样绿（改前实测：把 record-epipe-guarded 换
# 成 accounting-epipe-continued 的副本，仍 ``6 anchors: ok``、rc 0）——
# 被替掉的那半个格子从此无人排程。改用仓内正本
# negative_selection.registered()（handle-path-macos 同款调用）：有序名册
# 比对，拒 dropped / renamed / duplicated / reordered；加行、删行、改名、
# 重排都必须同步改 EXPECTED_NAMES，否则拒绝运行（含 --check 路径）。
EXPECTED_NAMES = (
    'accounting-epipe-continued',
    'accounting-diagnostic-dropped',
    'accounting-resumes-after-diagnosis',
    'accounting-exit-green',
    'accounting-exit-survived-code',
    'record-epipe-guarded',
)


def apply_mutation(name, original, before, after):
    """Return the mutant bytes, or fail closed on a stale/no-op anchor."""
    count = original.count(before)
    if count != 1:
        raise ValueError(f'#3043 {name} anchor expired: count={count}, expected=1')
    if before == after:
        raise ValueError(f'#3043 {name} is a no-op mutation')
    mutant = original.replace(before, after, 1)
    compile(mutant, f'{name}.py', 'exec')
    return mutant


def run_bound(env):
    # fix-r10 (P1, four-way review: DS sibling#1/concurrency#1, MM
    # sibling#1/boundary#1): this round holds <root>/.negative-source.lock
    # for its whole case list (the mutating_sources window in main), and
    # the bound test's guarded source reads would wait 2 s on their own
    # ancestor -- no amount of waiting resolves (the Rust read-lease
    # precedent, tests/common/read_lease.rs:61-96). Export the held root
    # exactly like sdk-signals does for its cargo children (:650): this
    # process is the byte writer for the very sources the child reads --
    # the mutation leg must read the parent-published mutant, the
    # pristine/restored legs the restored pristine -- so reading straight
    # through is the correct bytes on all three legs.
    env = dict(env)
    env['PDRS_NEGATIVE_LEASE_HELD_ROOT'] = str(ROOT)
    return run_owned(
        [sys.executable, '-I', '-B', str(TEST), BOUND, '-v'],
        cwd=ROOT, env=env, timeout=180)


# #3043 fix-r4 (a4): the judge set below is copied verbatim from the family's
# canonical entry ``gateway-entry-guard/negative_controls.py`` (#991/#1060).
# The local drift it replaces accepted any rc-0 log containing "Ran 1 test"
# and "\nOK" -- a skip-shaped green log passes it -- and called any log with
# the marker, "FAIL:" and "AssertionError" a KILL, so the marker echoed in
# stdout over an unrelated failure block was judged KILLED. Only the bound
# test's own AssertionError may report a kill, in either unittest layout.
def single_test(log):
    return len(re.findall(r'^Ran 1 test in [0-9.]+s$', log, re.MULTILINE)) == 1


def descriptions(test):
    owner, method = test.rsplit('.', 1)
    # Accept both observed unittest layouts, retaining the exact module,
    # class and method identity in either layout (#1060).
    return (f'{method} (__main__.{test})', f'{method} (__main__.{owner})')


def passed(result, test):
    log = result.stdout + result.stderr
    return (result.returncode == 0
            and any(description + ' ... ok' in log.splitlines()
                    for description in descriptions(test))
            and single_test(log) and log.rstrip().endswith('\nOK')
            and 'skipped' not in log)


def classify(result, test, message):
    log = result.stdout + result.stderr
    if passed(result, test):
        return 'SURVIVED', 'bound test passed the mutant'
    if (result.returncode != 1 or not single_test(log)
            or not re.search(r'^FAILED \(failures=[1-9][0-9]*\)\s*\Z', log, re.MULTILINE)
            or 'ERROR:' in log):
        return 'INVALID', 'missing normal single-test assertion failure'
    # subTest descriptions follow either layout. Only this test's own
    # AssertionError may supply the marker.
    names = '|'.join(re.escape(name) for name in descriptions(test))
    header = re.compile(
        rf'FAIL: (?:{names})(?: \(.*\))?')
    separator = '\n' + '-' * 70 + '\n'
    for block in log.split('\n' + '=' * 70 + '\n')[1:]:
        title, found, body = block.partition(separator)
        if not found or not header.fullmatch(title):
            continue
        body = body.split(separator, 1)[0]
        assertion = re.search(r'^AssertionError: (.*)', body, re.MULTILINE | re.DOTALL)
        if assertion and message in assertion.group(1):
            return 'KILLED', message
    return 'INVALID', 'missing bound assertion (anchor, compile or wrong failure)'


def publish(output, results):
    counts = {state: sum(row['status'] == state for row in results)
              for state in ('KILLED', 'SURVIVED', 'INVALID')}
    target = output / 'summary.json'
    temporary = target.with_name(target.name + '.tmp')
    temporary.write_text(json.dumps({'controls': results, 'counts': counts},
                                   indent=2) + '\n')
    temporary.replace(target)
    print('summary: ' + ', '.join(f'{state}={count}'
                                  for state, count in counts.items()),
          flush=True)


def invalidate(results, reason):
    for row in results:
        row.update(status='INVALID', reason=row['reason'] + '; ' + reason)


def main():
    # #3043 fix-r1→fix-r2：名册守卫先于一切参数处理——表与静态名册不一致时
    # --check 与真实运行都必须拒绝（rc 2），表格漂移不得静默变绿；fix-r2
    # 起比对升为名册级（registered()，正本在 negative_selection），同数
    # 替换 / 改名 / 重排 / 重复同样拒绝。
    try:
        registered(ROWS, EXPECTED_NAMES)
    except ValueError as error:
        refuse(f'#3043 paired-mutation roster: {error}')
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path)
    parser.add_argument('--only', action='append')
    parser.add_argument('--check', action='store_true')
    args = parser.parse_args()
    known = {row[0] for row in ROWS}
    if args.only and (set(args.only) - known or len(args.only) != len(set(args.only))):
        print('unknown or duplicate --only name', file=sys.stderr)
        return 2
    selected = [row for row in ROWS if not args.only or row[0] in args.only]

    if args.check:
        for name, before, after, _message in selected:
            apply_mutation(name, RUNNER.read_text(encoding='utf-8'),
                          before, after)
        print(f'{len(selected)} anchors: ok')
        return 0
    if args.output is None:
        print('--output is required unless --check is used', file=sys.stderr)
        return 2

    output = new_output_directory(args.output)
    env = dict(os.environ)
    env['PYTHONDONTWRITEBYTECODE'] = '1'
    results = []
    try:
        with mutating_sources(ROOT, [RUNNER]) as originals:
            text = originals[RUNNER].decode('utf-8')
            # Preflight every selected anchor before any positive/mutant run:
            # an expired anchor aborts as INVALID, never SURVIVED.
            for name, before, after, _message in selected:
                apply_mutation(name, text, before, after)
            for name, before, after, message in selected:
                status, reason = 'INVALID', 'not executed'
                try:
                    if not passed(run_bound(env), BOUND):
                        reason = 'positive control did not pass'
                    else:
                        # fix-r4 (a5): publish and restore both go through
                        # write_source_atomic (temp file + one rename), never a
                        # truncating in-place write -- a SIGKILL mid-write
                        # leaves the old or the new bytes, not half a file
                        # (#1903; the #3088 fix-r6 same-family shape).
                        write_source_atomic(
                            ROOT, RUNNER,
                            text.replace(before, after, 1).encode('utf-8'))
                        status, reason = classify(run_bound(env), BOUND, message)
                except Exception as error:
                    status, reason = 'INVALID', f'{type(error).__name__}: {error}'
                finally:
                    write_source_atomic(ROOT, RUNNER, originals[RUNNER])
                    if RUNNER.read_bytes() != originals[RUNNER]:
                        raise RuntimeError(
                            f'#3043 byte restoration failed: {RUNNER}')
                try:
                    if not passed(run_bound(env), BOUND):
                        status, reason = ('INVALID',
                                          reason + '; restored bound test did not pass')
                except Exception as error:
                    status, reason = (
                        'INVALID', reason + f'; restored: {type(error).__name__}: {error}')
                results.append({'mutation': name, 'test': BOUND,
                                'status': status, 'reason': reason})
                print(f'{name}: {status}: {reason}', flush=True)
    except BaseException as error:
        reason = f'source guard: {type(error).__name__}: {error}'
        invalidate(results, reason)
        for name, _before, _after, _message in selected[len(results):]:
            results.append({'mutation': name, 'test': BOUND,
                            'status': 'INVALID', 'reason': reason})
        publish(output, results)
        if isinstance(error, SystemExit):
            raise SystemExit(2) from error
        return 2
    publish(output, results)
    return int(not results or any(row['status'] != 'KILLED' for row in results))


if __name__ == '__main__':
    raise SystemExit(main())
