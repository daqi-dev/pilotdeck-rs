#!/usr/bin/env python3
"""#947 negative controls: every JS `Math.max` / `Math.min` mirror must fail
when the shared signed-zero projection is replaced by Rust `f64::max` / `min`.

The anchors are one import/helper line per site.  For the shared helper the
mutant replaces the explicit zero-tie branch with the platform method, which
is exactly the regression this issue exists to prevent.  A mutant must first
compile (`cargo test --no-run`), then the named test must fail on a `#947`
assertion, then restoring the bytes must make the same test pass again.

The control does not stop at the first survivor: it records KILLED / SURVIVED
/ INVALID for every selected case, writes `summary.json`, and exits non-zero
unless every case is KILLED.  A stale anchor is a harness defect, so it exits
2 before compiling instead of being classified as SURVIVED.

`mutating_sources` proves every target equals HEAD and restores it on exit.
A source tree with uncommitted #947 changes therefore refuses to run; that is
the intended guard, and the coordinator runs this after the commit.
"""
import json
import os
import re
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

root = Path(__file__).resolve().parents[5]
protocol = root / 'src/pilotdeck-protocol/src/js_math.rs'
compaction = root / 'src/pilotdeck-gateway/src/runtime/context/compaction/mod.rs'
token_accounting = root / 'src/pilotdeck-gateway/src/runtime/model/token_accounting.rs'
recall = root / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/llm/recall.rs'
sdk_session = root / 'src/pilotdeck-gateway/src/runtime/sdk_session/mod.rs'
agent_policy = root / 'src/pilotdeck-agent/src/request_materialization_policy.rs'

# (name, source, before, after, exact test)
cases = [
    # Shared helper: both explicit tie branches go back to platform max/min.
    ('shared-max-f64', protocol,
     '''        return if left.is_sign_negative() && right.is_sign_negative() {
            -0.0
        } else {
            0.0
        };''',
     '        return left.max(right);',
     'js_math::tests::max_projects_positive_zero_for_every_signed_zero_tie'),
    ('shared-min-f64', protocol,
     '''        return if left.is_sign_negative() || right.is_sign_negative() {
            -0.0
        } else {
            0.0
        };''',
     '        return left.min(right);',
     'js_math::tests::min_projects_negative_zero_for_every_signed_zero_tie'),
    # Per-site imports: a local f64 helper is what a future refactor would
    # reintroduce if the site stopped routing through the shared module.
    ('compaction-f64', compaction,
     'pub(crate) use pilotdeck_protocol::js_math::{max as js_max, min as js_min};',
     'pub(crate) fn js_max(left: f64, right: f64) -> f64 { left.max(right) }\n'
     'pub(crate) fn js_min(left: f64, right: f64) -> f64 { left.min(right) }',
     'runtime::context::compaction::tests::js_math_signed_zero_ties_match_ts'),
    ('token-accounting-f64', token_accounting,
     'use pilotdeck_protocol::js_math::{max as js_max, min as js_min};',
     'fn js_max(left: f64, right: f64) -> f64 { left.max(right) }\n'
     'fn js_min(left: f64, right: f64) -> f64 { left.min(right) }',
     'runtime::model::token_accounting::tests::js_math_signed_zero_ties_match_ts'),
    ('recall-f64', recall,
     'use pilotdeck_protocol::js_math::{max as js_max, min as js_min};',
     'fn js_max(left: f64, right: f64) -> f64 { left.max(right) }\n'
     'fn js_min(left: f64, right: f64) -> f64 { left.min(right) }',
     'runtime::memory::edgeclaw::llm::recall::tests::js_math_signed_zero_ties_match_ts'),
    ('sdk-session-f64', sdk_session,
     'use pilotdeck_protocol::js_math::min as math_min;',
     'fn math_min(left: f64, right: f64) -> f64 { left.min(right) }',
     'runtime::sdk_session::tests::math_min_signed_zero_ties_match_ts'),
    ('agent-policy-f64', agent_policy,
     'use pilotdeck_protocol::js_math::min as js_min;',
     'fn js_min(left: f64, right: f64) -> f64 { left.min(right) }',
     'request_materialization_policy::unit_tests::js_min_signed_zero_ties_match_ts'),
]

ASSERTIONS = {
    'shared-max-f64': '#947 js_math::max tie',
    'shared-min-f64': '#947 js_math::min tie',
    'compaction-f64': '#947 compaction js_max tie',
    'token-accounting-f64': '#947 token_accounting js_max tie',
    'recall-f64': '#947 recall js_max tie',
    'sdk-session-f64': '#947 sdk_session math_min tie',
    'agent-policy-f64': '#947 request_materialization js_min tie',
}

BASELINE = {
    'pilotdeck-protocol': (['-p', 'pilotdeck-protocol', '--lib'], 'js_math'),
    'pilotdeck-gateway': (['-p', 'pilotdeck-gateway', '--lib'], 'signed_zero_ties_match_ts'),
    'pilotdeck-agent': (
        ['-p', 'pilotdeck-agent', '--test', 'request_materialization'],
        'js_min_signed_zero_ties_match_ts',
    ),
}


def package_for(path):
    if path == protocol:
        return 'pilotdeck-protocol'
    if path == agent_policy:
        return 'pilotdeck-agent'
    return 'pilotdeck-gateway'


def cargo(extra):
    return ['cargo', 'test', '--offline', '--manifest-path', str(root / 'src/Cargo.toml'), *extra]


def classify(log, returncode):
    """Three-way outcome; a renamed/filtered test is INVALID, never SURVIVED."""
    if returncode is None or 'could not compile' in log:
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
        ('running 1 test\npanicked at src/x.rs:1:1:\n#947\n\ntest result: FAILED. 0 passed; 1 failed;', 101, 'KILLED'),
        ('running 1 test\n\ntest result: FAILED. 0 passed; 1 failed;', 0, 'INVALID'),
        ('error: could not compile `pilotdeck-gateway`', 101, 'INVALID'),
    ]
    for log, returncode, expected in samples:
        actual = classify(log, returncode)
        assert actual == expected, (log, returncode, expected, actual)
    print('classify self-test: PASS')


def check_anchors(texts):
    stale = []
    for name, path, before, _after, _test in cases:
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
    check_anchors({path: path.read_text() for _name, path, _b, _a, _t in cases})
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
paths = list(dict.fromkeys(case[1] for case in cases))
results = []


def write_summary():
    (out / 'summary.json').write_text(json.dumps({'cases': results}, indent=2) + '\n')


def run(label, extra, timeout=600):
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


originals = {}
try:
    with mutating_sources(root, paths) as originals:
        # Stale anchors are a harness defect: fail before any compile.
        check_anchors({path: data.decode() for path, data in originals.items()})

        # Positive baseline for every affected target, once each.
        baseline_ok = True
        for package, (extra, filter) in BASELINE.items():
            if not any(package_for(case[1]) == package for case in cases):
                continue
            result, log = run('baseline-' + package, [*extra, filter])
            ok = (result is not None and 'test result: ok.' in log
                  and 'running 0 tests' not in log)
            if not ok:
                baseline_ok = False
                print(f'baseline {package}: FAIL', flush=True)
                break
            print(f'baseline {package}: PASS', flush=True)
        if not baseline_ok:
            raise RuntimeError('unmutated baseline did not pass; mutants not run')

        for name, path, before, after, test in cases:
            package = package_for(path)
            extra, _filter = BASELINE[package]
            original = originals[path].decode()
            status, reason = 'INVALID', 'not run'
            try:
                # Apply exactly one anchor replacement.
                changed = original.replace(before, after, 1)
                if changed == original:
                    raise RuntimeError('anchor replacement did not change the source')
                path.write_text(changed)

                # Requirement: every mutant must compile before its test verdict.
                compiled, compile_log = run(name + '-compile', [*extra, '--no-run'])
                if compiled is None or compiled.returncode != 0:
                    reason = 'mutant did not compile; never a kill'
                else:
                    result, log = run(name + '-negative',
                                      [*extra, test, '--', '--exact', '--nocapture'])
                    status = classify(log, None if result is None else result.returncode)
                    if status == 'KILLED' and (
                            f'test {test} ... FAILED' not in log
                            or ASSERTIONS[name] not in log
                            or 'panicked at' not in log):
                        status, reason = 'INVALID', 'missing named FAILED / #947 assertion / panic'
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
                result, log = run(name + '-restored', [*extra, test, '--', '--exact'])
                if (result is None or result.returncode != 0
                        or f'test {test} ... ok' not in log
                        or 'running 0 tests' in log):
                    status, reason = 'INVALID', 'restored source did not pass the bound test'
            results.append({'name': name, 'source': path.relative_to(root).as_posix(),
                            'test': test, 'status': status, 'reason': reason})
            write_summary()
            print(f'{name}: {status}: {reason}', flush=True)
finally:
    for path, data in originals.items():
        path.write_bytes(data)
        assert path.read_bytes() == data, 'restoration failed: ' + str(path)

counts = {state: sum(row['status'] == state for row in results)
          for state in ('KILLED', 'SURVIVED', 'INVALID')}
write_summary()
print('summary: ' + ', '.join(f'{state}={count}' for state, count in counts.items()), flush=True)
raise SystemExit(0 if results and all(row['status'] == 'KILLED' for row in results) else 1)
