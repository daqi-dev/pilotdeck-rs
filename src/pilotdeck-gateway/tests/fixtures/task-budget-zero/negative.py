#!/usr/bin/env python3
"""#1014 negative controls: the task-budget settled-record fold
(`storage/task_budget.rs`, `Math.max(lastActivityAtMs ?? 0, …)` at TS
`ProjectRuntimeRegistry.ts:1499`) must fail when the shared `js_math::max`
call is replaced by an order-dependent or NaN-swallowing maximum.

Two mutants:
- `f64::max` revert: the platform method returns the other side for a NaN
  operand, so the loader would store a finite activity instead of NaN.
  This is the revert the #1014 review asks to be KILLED.
- zero-tie order pick: on a `+0`/`-0` tie the newer input is picked instead
  of the JS-projected `+0`.

Platform caveat, measured on this host (see
`_tmp/20261026-issue-947/fix-r0/f64-probe/probe.out`, #947): Rust
`f64::max(0.0, -0.0)` already projects `+0` here, so a pure zero-tie revert
of `f64::max` is behaviorally equivalent on this host and would SURVIVE.
The NaN cells are therefore the kill for the platform revert, and the
order-pick mutant is the kill for the tie projection itself. The
`js_math::max` helper's own tie/NaN projection is negatively controlled by
`js-math-zero/negative.py` (#947), which stays in the tree.

The anchors are one exact source replacement. A mutant must first compile
(`cargo test --no-run`), then the named test must fail on a `#1014`
assertion, then restoring the bytes must make the same test pass again.

The control does not stop at the first survivor: it records KILLED / SURVIVED
/ INVALID for every case, and exits non-zero unless every case is KILLED. A
stale anchor is a harness defect, so it exits 2 before compiling instead of
being classified as SURVIVED.

#1087 fix-r4 (sibling #3): the verdict rows stay in memory for the whole
guarded window and one final `summary.json` is published by a single rename
only after `mutating_sources` restored and byte-verified the guarded source
(the same skeleton as the five edgeclaw tables). An abort inside the window —
a cancellation, a restore failure, the source guard — voids the recorded
rows, publishes one INVALID summary and exits 3. A red baseline keeps exit 1
but still publishes its row. The old per-row truncating write left a partial
all-KILLED summary behind when the run was cancelled mid-suite.

`mutating_sources` proves every target equals HEAD and restores it on exit.
A source tree with uncommitted #1014 changes therefore refuses to run; that
is the intended guard, and the coordinator runs this after the commit.
"""
import json
import os
import re
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import (dead_stream_print, detach_dead_streams,
                              mutating_sources, run_owned, wait_for_slot,
                              write_source_atomic)
from negative_selection import (claim_dir, missing_target, refuse,
                               refuse_unusable_target, select)

root = Path(__file__).resolve().parents[5]
source = root / 'src/pilotdeck-gateway/src/storage/task_budget.rs'

TEST = 'settled_activity_zeros_and_nan_match_ts_math_max'

# (name, source, before, after)
cases = [
    ('settle-max-nan-revert', source,
     'ledger.last_activity_at_ms = JsValue::Number(js_math_max(old, new));',
     'ledger.last_activity_at_ms = JsValue::Number(f64::max(old, new));'),
    ('settle-max-zero-tie-new', source,
     'ledger.last_activity_at_ms = JsValue::Number(js_math_max(old, new));',
     'ledger.last_activity_at_ms = JsValue::Number('
     'if old == 0.0 && new == 0.0 { new } else { js_math_max(old, new) });'),
]

ASSERTIONS = {
    'settle-max-nan-revert': 'must stay NaN like TS Math.max',
    'settle-max-zero-tie-new': 'must keep the TS Math.max sign',
}


def cargo(extra):
    return ['cargo', 'test', '--offline', '--manifest-path', str(root / 'src/Cargo.toml'),
            '-p', 'pilotdeck-gateway', '--test', 'task_budget', *extra]


def classify(log, returncode):
    """Three-way outcome; a renamed/filtered test is INVALID, never SURVIVED."""
    if returncode is None or 'could not compile' in log or 'error[' in log:
        return 'INVALID'
    ran = re.search(r'^running (\d+) tests?$', log, re.MULTILINE)
    if ran is None or int(ran.group(1)) != 1:
        return 'INVALID'
    if returncode == 101 and 'test result: FAILED.' in log and 'panicked at' in log:
        return 'KILLED'
    if returncode == 0 and 'test result: ok.' in log:
        return 'SURVIVED'
    return 'INVALID'


def self_test():
    samples = [
        ('running 0 tests\n\ntest result: ok. 0 passed; 0 failed;', 0, 'INVALID'),
        ('running 1 test\n\ntest result: ok. 1 passed; 0 failed;', 0, 'SURVIVED'),
        ('running 2 tests\n\ntest result: ok. 2 passed; 0 failed;', 0, 'INVALID'),
        ('running 1 test\npanicked at src/x.rs:1:1:\n#1014\n\ntest result: FAILED. 0 passed; 1 failed;', 101, 'KILLED'),
        ('running 1 test\n\ntest result: FAILED. 0 passed; 1 failed;', 0, 'INVALID'),
        ('error: could not compile `pilotdeck-gateway`', 101, 'INVALID'),
        ('error[E0308]: mismatched types', 101, 'INVALID'),
    ]
    for log, returncode, expected in samples:
        actual = classify(log, returncode)
        if actual != expected:
            raise RuntimeError((log, returncode, expected, actual))
    print('classify self-test: PASS')


def check_anchors(texts):
    stale = []
    for name, path, before, _after in cases:
        count = texts[path].count(before)
        if count != 1:
            stale.append((name, path.relative_to(root).as_posix(), count))
    if stale:
        # #1087 fix-r8 (S1-a): the shared guarded print — a dead stderr
        # must not turn this exit-2 refusal into a crash (rc=1) or a
        # shutdown-flush 120; the code is the contract.
        for name, relative, count in stale:
            dead_stream_print(f'STALE ANCHOR {name}: {count} matches in {relative}',
                              stream='stderr')
        dead_stream_print('STALE ANCHORS: negative.py is out of sync with its sources; refusing to run',
                          stream='stderr')
        raise SystemExit(2)
    # #1087 fix-r8 (S1-a): the shared guarded print — the read-only
    # "anchors: ok" preflight keeps exit 0 under a dead stdout.
    dead_stream_print(f'{len(cases)} anchors: ok')


# #1908 (from #1692 r15-4): ONE dispatch rule for every mode. The two flags
# used to be whole-argv equality checks, so `--self-test extra` fell through
# to the main path and treated `--self-test` as an output directory; an
# unknown `--flag` did the same and ran the whole table green. Every mode now
# takes an exact argument list, and only a real OUT_DIR reaches the main path.
USAGE = 'usage: negative.py OUT_DIR [NAME ...] | negative.py --check | negative.py --self-test'
mode, arguments = (sys.argv[1], sys.argv[2:]) if len(sys.argv) > 1 else (None, [])
if mode == '--check':
    if arguments:
        raise SystemExit(f'--check takes no arguments: {arguments!r}\n{USAGE}')
    check_anchors({path: path.read_text() for _name, path, _b, _a in cases})
    raise SystemExit(0)
if mode == '--self-test':
    if arguments:
        raise SystemExit(f'--self-test takes no arguments: {arguments!r}\n{USAGE}')
    self_test()
    raise SystemExit(0)
# Explicit exits, not asserts: `python3 -O` strips asserts and a bare
# IndexError traceback is not a usage diagnostic. Zero arguments and an
# unknown `-*` first argument are both usage errors (non-zero exit).
if mode is None:
    raise SystemExit(USAGE)
if mode.startswith('-'):
    raise SystemExit(f'unknown mode: {mode}\n{USAGE}')
# Judgment checks are explicit exits, not asserts: python3 -O strips asserts
# (same hardening as the mode dispatch above), so a misspelled control name
# must not silently run zero mutants and exit 0.
target = os.environ.get('CARGO_TARGET_DIR')
if missing_target():
    refuse('set CARGO_TARGET_DIR to the caller-owned build tree before running')
names = [case[0] for case in cases]
if len(names) != len(set(names)):
    refuse('duplicate control name')
try:
    run_rows = select(cases, arguments, always=())
except ValueError as error:
    refuse(str(error))
if not run_rows:
    refuse('no controls selected')

refuse_unusable_target()
out = claim_dir(mode)
env = dict(os.environ)
env['CARGO_TARGET_DIR'] = target
results = []
# #1087 fix-r4 (sibling #3): bound before the guarded window so the
# cancellation handler can always publish, even when the abort happened
# before the baseline run recorded anything.
baseline = {'name': 'baseline', 'outcome': 'INVALID', 'returncode': None}


def write_summary(current_baseline):
    """Publish the one final summary atomically (#1087 fix-r4, sibling #3).

    Called exactly once per run, after ``mutating_sources`` restored and
    byte-verified the guarded source — or from the cancellation handler,
    after the same guard unwound. The old per-row truncating ``write_text``
    left a partial all-KILLED summary on disk when a cancellation killed the
    run mid-suite, and an interrupt inside the write could leave an empty or
    half-written JSON document.
    """
    tmp = out / 'summary.json.tmp'
    tmp.write_text(json.dumps(
        {'baseline': current_baseline, 'cases': results}, indent=2) + '\n')
    tmp.replace(out / 'summary.json')


def _void_pending(error):
    """Void the recorded rows and publish one INVALID summary (sibling #3).

    A cancellation or crash inside the guarded window is not a verdict: the
    rows recorded so far are voided, the remaining selected cases are
    recorded INVALID, and the single summary is published after the guard
    restored the tree. The process then exits 3 — deliberately different
    from 2 ("never started", the stale-anchor refusal below) so a reader
    can tell a cancelled run from one that never ran.
    """
    reason = f'runner: {type(error).__name__}: {error}'
    for row in results:
        row['status'] = 'INVALID'
        row['reason'] = (f"{row['reason']}; voided: {reason}"
                         if row['reason'] else f"voided: {reason}")
    for name, path, _before, _after in run_rows[len(results):]:
        results.append({'name': name, 'source': path.relative_to(root).as_posix(),
                        'test': TEST, 'status': 'INVALID', 'reason': reason})
    write_summary(baseline)
    # #1087 fix-r8 (S1-a): the shared guarded print — the summary is
    # already published, "aborted = 3" is the only contract left
    # to keep; a dead stderr must not replace the exit code.
    dead_stream_print(f'voided {len(results)} row(s): {reason}', stream='stderr')
    if isinstance(error, (OSError, ValueError)):
        # #1087 fix-r5 (concurrency #2): a stream-failure abort detaches the
        # dead fds first, so the interpreter-shutdown flush cannot rewrite
        # the exit code below to 120 ("Exception ignored in:").
        detach_dead_streams()
    if isinstance(error, SystemExit):
        # Normalize: 128+signum and any other raw code must not decide how a
        # coordinator reads this run (a 256/-256 code truncates to a zero
        # process status on POSIX).
        raise SystemExit(3) from error
    if not isinstance(error, Exception):
        raise
    raise SystemExit(3) from error


def run(label, extra, timeout=900):
    try:
        slots = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=600)
        (out / (label + '.slots.log')).write_text(slots.stdout + slots.stderr)
        result = run_owned(cargo(extra), cwd=root, env=env, timeout=timeout)
    except Exception as error:  # noqa: BLE001 - one cell must not hide the rest
        (out / (label + '.log')).write_text(f'{type(error).__name__}: {error}\n')
        return None, ''
    log = result.stdout + result.stderr
    (out / (label + '.log')).write_text(log)
    return result, log


try:
    with mutating_sources(root, [source]) as originals:
        # Stale anchors are a harness defect: fail before any compile.
        check_anchors({source: originals[source].decode()})
        baseline_ok = False
        try:
            # Positive baseline: the unmutated bound test must pass.
            # No `--nocapture` in either run: it splices the test's stdout into the
            # `test <name> ... ok/FAILED` status line (#938 r14) that the baseline
            # check and the classify() kill check match exactly (#1053).
            result, log = run('baseline', [TEST, '--', '--exact'])
            baseline_ok = (result is not None and result.returncode == 0
                           and f'test {TEST} ... ok' in log
                           and 'running 0 tests' not in log)
            baseline = {'name': 'baseline',
                        'outcome': 'PASS' if baseline_ok else 'FAIL',
                        'returncode': None if result is None else result.returncode}
            print(f'baseline: {"PASS" if baseline_ok else "FAIL"}', flush=True)
            if not baseline_ok:
                print('baseline failed; mutants not run', flush=True)
                raise SystemExit(1)

            for name, path, before, after in run_rows:
                original = originals[path].decode()
                status, reason = 'INVALID', 'not run'
                try:
                    # Apply exactly one anchor replacement.
                    changed = original.replace(before, after, 1)
                    if changed == original:
                        raise RuntimeError('anchor replacement did not change the source')
                    # #1908: one rename, never a truncating in-place write; the shared
                    # publish helper (#1903) keeps a SIGKILL mid-write from leaving a
                    # source the next preflight would refuse as "differs from HEAD".
                    write_source_atomic(root, path, changed.encode())

                    # Requirement: every mutant must compile before its test verdict.
                    compiled, compile_log = run(name + '-compile', ['--no-run'])
                    if compiled is None or compiled.returncode != 0:
                        reason = 'mutant did not compile; never a kill'
                    else:
                        result, log = run(name + '-negative', [TEST, '--', '--exact'])
                        status = classify(log, None if result is None else result.returncode)
                        if status == 'KILLED' and (
                                f'test {TEST} ... FAILED' not in log
                                or ASSERTIONS[name] not in log
                                or 'panicked at' not in log):
                            status, reason = 'INVALID', 'missing named FAILED / #1014 assertion / panic'
                        else:
                            reason = 'named assertion failed' if status == 'KILLED' else 'classified'
                except Exception as error:  # noqa: BLE001
                    status, reason = 'INVALID', f'{type(error).__name__}: {error}'
                finally:
                    # #1908: same rule on the way back; a truncating write_bytes
                    # restore has the same SIGKILL window as the mutation write.
                    # The guard's own exit verification covers the bytes.
                    write_source_atomic(root, path, originals[path])

                # Restored bytes must pass the same named test.
                if status == 'KILLED':
                    result, log = run(name + '-restored', [TEST, '--', '--exact'])
                    if (result is None or result.returncode != 0
                            or f'test {TEST} ... ok' not in log
                            or 'running 0 tests' in log):
                        status, reason = 'INVALID', 'restored source did not pass the bound test'
                results.append({'name': name, 'source': path.relative_to(root).as_posix(),
                                'test': TEST, 'status': status, 'reason': reason})
                print(f'{name}: {status}: {reason}', flush=True)
        finally:
            # Same #1087 fix-r4 window: the final restore is also one rename,
            # never a truncating write_bytes.
            write_source_atomic(root, source, originals[source])
            if source.read_bytes() != originals[source]:
                raise RuntimeError(f'restoration failed: {source}')
except SystemExit as error:
    if error.code == 1:
        # The deliberate baseline failure: the guard already restored and
        # byte-verified the tree while unwinding the with-block; publish the
        # baseline row and keep the "ran and failed" exit code.
        write_summary(baseline)
        raise
    if error.code == 2:
        # The deliberate stale-anchor refusal: nothing was measured, so
        # nothing is published — "never started" leaves no summary behind.
        raise
    _void_pending(error)
except BaseException as error:  # noqa: BLE001 - void, publish, then re-raise
    _void_pending(error)
# Reaching here means mutating_sources restored the guarded file and
# verified it against the baseline bytes. Only now may a summary exist.
write_summary(baseline)

counts = {state: sum(row['status'] == state for row in results)
          for state in ('KILLED', 'SURVIVED', 'INVALID')}
print('summary: ' + ', '.join(f'{state}={count}' for state, count in counts.items()), flush=True)
raise SystemExit(0 if results and all(row['status'] == 'KILLED' for row in results) else 1)
