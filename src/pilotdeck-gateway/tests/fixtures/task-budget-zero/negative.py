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
/ INVALID for every case, writes `summary.json`, and exits non-zero unless
every case is KILLED. A stale anchor is a harness defect, so it exits 2
before compiling instead of being classified as SURVIVED.

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
from negative_process import mutating_sources, run_owned, wait_for_slot

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
        assert actual == expected, (log, returncode, expected, actual)
    print('classify self-test: PASS')


def check_anchors(texts):
    stale = []
    for name, path, before, _after in cases:
        count = texts[path].count(before)
        if count != 1:
            stale.append((name, path.relative_to(root).as_posix(), count))
    if stale:
        for name, relative, count in stale:
            print(f'STALE ANCHOR {name}: {count} matches in {relative}', file=sys.stderr)
        print('STALE ANCHORS: negative.py is out of sync with its sources; refusing to run',
              file=sys.stderr)
        raise SystemExit(2)
    print(f'{len(cases)} anchors: ok')


if sys.argv[1:] == ['--check']:
    check_anchors({path: path.read_text() for _name, path, _b, _a in cases})
    raise SystemExit(0)
if sys.argv[1:] == ['--self-test']:
    self_test()
    raise SystemExit(0)

assert len(sys.argv) >= 2, 'usage: negative.py OUT_DIR [NAME ...] | negative.py --check | negative.py --self-test'
target = os.environ.get('CARGO_TARGET_DIR')
assert target, 'negative.py requires CARGO_TARGET_DIR from the caller'
names = [case[0] for case in cases]
assert len(names) == len(set(names)), 'duplicate control name'
selected = sys.argv[2:]
unknown = set(selected) - set(names)
assert not unknown, f'unknown controls: {sorted(unknown)}'
if selected:
    cases = [case for case in cases if case[0] in selected]
assert cases, 'no controls selected'

out = Path(sys.argv[1]).resolve()
out.mkdir(parents=True, exist_ok=False)
env = dict(os.environ)
env['CARGO_TARGET_DIR'] = target
results = []


def write_summary():
    (out / 'summary.json').write_text(json.dumps({'cases': results}, indent=2) + '\n')


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


with mutating_sources(root, [source]) as originals:
    # Stale anchors are a harness defect: fail before any compile.
    check_anchors({source: originals[source].decode()})

    # Positive baseline: the unmutated bound test must pass.
    # No `--nocapture` in either run: it splices the test's stdout into the
    # `test <name> ... ok/FAILED` status line (#938 r14) that the baseline
    # check and the classify() kill check match exactly (#1053).
    baseline_ok = True
    result, log = run('baseline', [TEST, '--', '--exact'])
    if (result is None or result.returncode != 0 or f'test {TEST} ... ok' not in log
            or 'running 0 tests' in log):
        baseline_ok = False
        print('baseline: FAIL', flush=True)
    else:
        print('baseline: PASS', flush=True)
    if not baseline_ok:
        raise RuntimeError('unmutated baseline did not pass; mutants not run')

    for name, path, before, after in cases:
        original = originals[path].decode()
        status, reason = 'INVALID', 'not run'
        try:
            # Apply exactly one anchor replacement.
            changed = original.replace(before, after, 1)
            if changed == original:
                raise RuntimeError('anchor replacement did not change the source')
            path.write_text(changed)

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
            path.write_bytes(originals[path])
            if path.read_bytes() != originals[path]:
                raise RuntimeError(f'restoration failed: {path}')

        # Restored bytes must pass the same named test.
        if status == 'KILLED':
            result, log = run(name + '-restored', [TEST, '--', '--exact'])
            if (result is None or result.returncode != 0
                    or f'test {TEST} ... ok' not in log
                    or 'running 0 tests' in log):
                status, reason = 'INVALID', 'restored source did not pass the bound test'
        results.append({'name': name, 'source': path.relative_to(root).as_posix(),
                        'test': TEST, 'status': status, 'reason': reason})
        write_summary()
        print(f'{name}: {status}: {reason}', flush=True)

counts = {state: sum(row['status'] == state for row in results)
          for state in ('KILLED', 'SURVIVED', 'INVALID')}
write_summary()
print('summary: ' + ', '.join(f'{state}={count}' for state, count in counts.items()), flush=True)
raise SystemExit(0 if results and all(row['status'] == 'KILLED' for row in results) else 1)
