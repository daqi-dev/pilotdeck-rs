#!/usr/bin/env python3
"""#2501 clock-edge tier controls on the shared suite runner
(negative_suite.run_cases): each mutation must be killed by its named test's
own #2501 panic marker, then the tree is restored and re-verified.

Usage: negative_controls.py --check
       negative_controls.py NEW_OUT [mutation ...]
``CARGO_TARGET_DIR`` is caller-owned. Every selected row is reported,
including anchor/build/source-guard failures.

Rows pin the #1949 tier decision `deadline_or_waiver` extracted for #2501:
the deterministic tier test feeds it one captured ``now`` and asserts that a
budget whose deadline is the clock's ceiling (inside
``TICK_CONVERSION_HEADROOM``) maps to the waiver while one backed off two
seconds maps back to a deadline — the regression discriminator the e2e
clock-edge twins cannot be (their poll site reads its own, later clock, and
their 500 ms backoff sits far outside the ~999_999 ns band tokio actually
panics in). Three rows kill at the tier test: deleting the
tick-conversion filter (the mutation the r4 tests passed under), shrinking
the headroom to nothing, and inflating it tenfold so an in-band−2 s budget
is waived too. The fourth row backs the registration probe's deadline off,
so the probe no longer panics and fails at its guard marker instead of
hanging — the probe is what pins the measured mechanism (tokio 1.48's
unchecked ``deadline + Duration::from_nanos(999_999)`` tick conversion)
that the headroom is sized against.
"""
import os
from pathlib import Path
import sys

# `python3 -I` does not stop bytecode writes; keep the shared harness imports
# from leaving a __pycache__ inside the fixture tree.
sys.dont_write_bytecode = True

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_suite import Case, run_cases

root = Path(__file__).resolve().parents[5]
source = root / 'src/pilotdeck-gateway/src/runtime/turn/gateway.rs'
tests = root / 'src/pilotdeck-gateway/src/runtime/turn/gateway_tests.rs'
TESTS = 'runtime::turn::gateway::tests::'
mutations = [
    # #2501: the issue's named mutation — the tick-conversion filter is
    # deleted, so an in-band budget maps to a deadline again and the tier
    # test's first assertion reds: this is the kill the r4 e2e twins lacked.
    ('headroom-filter-removed', source,
     '    now.checked_add(budget)\n'
     '        .filter(|deadline| deadline.checked_add(TICK_CONVERSION_HEADROOM).is_some())',
     '    now.checked_add(budget)',
     TESTS + 'the_clock_edge_tier_decision_is_deterministic',
     '#2501 a budget whose deadline lands within the tick-conversion headroom '
     "of the clock's ceiling must take the waiver, not a deadline"),
    # #2501: the headroom is shrunk to nothing, so even the ceiling deadline
    # passes the filter and the in-band budget gets a deadline — the guard
    # exists but no longer keeps tokio's ~999_999 ns conversion band open.
    ('tick-headroom-shrunk-to-zero', source,
     'const TICK_CONVERSION_HEADROOM: Duration = Duration::from_secs(1);',
     'const TICK_CONVERSION_HEADROOM: Duration = Duration::from_nanos(0);',
     TESTS + 'the_clock_edge_tier_decision_is_deterministic',
     '#2501 a budget whose deadline lands within the tick-conversion headroom '
     "of the clock's ceiling must take the waiver, not a deadline"),
    # #2501: the headroom is inflated tenfold, so a budget legitimately
    # backed off past the real headroom is waived too — the tier test's
    # second assertion reds (the first still holds: the ceiling stays
    # inside either headroom). This is the direction that silently widens
    # the #1949 waiver past what the embedding declared.
    ('tick-headroom-inflated', source,
     '        .filter(|deadline| deadline.checked_add(TICK_CONVERSION_HEADROOM).is_some())',
     '        .filter(|deadline| deadline.checked_add(Duration::from_secs(10)).is_some())',
     TESTS + 'the_clock_edge_tier_decision_is_deterministic',
     '#2501 a budget backed off past the tick-conversion headroom must still get its deadline'),
    # #2501: the registration probe's deadline is backed off out of the
    # panic band, so the spawned registration settles pending forever and
    # the probe fails at its guard marker instead of hanging — proving the
    # probe observes the mechanism, not a vacuous should_panic.
    ('probe-deadline-backed-off', tests,
     '        let _ = tokio::time::timeout_at(now + lo, std::future::pending::<()>()).await;',
     '        let _ = tokio::time::timeout_at(\n'
     '            now + lo - std::time::Duration::from_secs(2),\n'
     '            std::future::pending::<()>(),\n'
     '        )\n'
     '        .await;',
     TESTS + 'a_maximal_deadline_panics_on_timer_registration',
     '#2501 the timer-registration probe must settle within the guard'),
]


# specs/negative-controls/README.md: the denominator is a static literal, not
# the runtime table length — dropping a row must fail loudly instead of making
# the round quietly shorter (still reporting N/N killed). #1946 review P2.
EXPECTED_NAMES = (
    'headroom-filter-removed',
    'tick-headroom-shrunk-to-zero',
    'tick-headroom-inflated',
    'probe-deadline-backed-off',
)


def enforce_denominator():
    names = tuple(row[0] for row in mutations)
    if names != EXPECTED_NAMES:
        missing = [name for name in EXPECTED_NAMES if name not in names]
        extra = [name for name in names if name not in EXPECTED_NAMES]
        raise SystemExit(
            f'#1946 fixed mutation denominator: expected {len(EXPECTED_NAMES)} '
            f'rows in order, found {len(names)} '
            f'(missing={missing}, extra={extra}, '
            f'order_ok={sorted(names) == sorted(EXPECTED_NAMES)})')


def check_anchor(row, original):
    if original.count(row[2]) != 1 or row[2] == row[3]:
        raise ValueError(f'{row[0]}: stale or unchanged anchor (count={original.count(row[2])})')


def mutate(content, scope, replacements):
    for old, new in replacements:
        if content.count(old) != 1 or old == new:
            raise RuntimeError('#2501 expired or ineffective mutation anchor')
        content = content.replace(old, new, 1)
    return content


def main():
    args = sys.argv[1:]
    enforce_denominator()
    if args == ['--check']:
        errors = []
        names = [row[0] for row in mutations]
        duplicates = sorted({name for name in names if names.count(name) > 1})
        if duplicates:
            errors.append(f'duplicate row names (by-name selection needs unique names): {duplicates}')
        for row in mutations:
            try:
                check_anchor(row, row[1].read_text())
            except Exception as error:
                errors.append(str(error))
        if errors:
            print('\n'.join(errors))
            return 1
        distinct = len({(str(row[1]), row[2]) for row in mutations})
        print(f'#2501 {len(mutations)} rows / {distinct} distinct anchors checked')
        return 0
    if not args:
        raise SystemExit('provide NEW_OUT and optional mutation names, or --check')
    names = args[1:]
    if names:
        # Requested names first: a duplicate request would silently run one
        # row per name twice (or, with a duplicate row name, expand one name
        # into several rows) — both refuse rather than misreport.
        duplicates = sorted({name for name in names if names.count(name) > 1})
        if duplicates:
            raise SystemExit(f'duplicate mutation names requested: {duplicates}')
        known = {row[0] for row in mutations}
        unknown = [name for name in names if name not in known]
        if unknown:
            raise SystemExit(f'unknown mutation names: {unknown}')
        wanted = set(names)
        selected = [row for row in mutations if row[0] in wanted]
        if len(selected) != len(wanted):
            raise SystemExit('unknown or duplicate mutation')
    else:
        selected = list(mutations)
    env = dict(os.environ)
    if not env.get('CARGO_TARGET_DIR'):
        raise SystemExit('CARGO_TARGET_DIR must be supplied by the caller')
    # cb8's zh_CN default locale has produced false reds in libtest output;
    # an explicit caller choice is never overridden.
    env.setdefault('LANG', 'en_US.UTF-8')
    out = Path(args[0]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    bound_cases = [Case(name, path, old, new, test, assertion)
                   for name, path, old, new, test, assertion in selected]
    return run_cases(root, bound_cases, out, env, mutate,
                     timeout=900, baseline='HEAD', script=__file__)


if __name__ == '__main__':
    raise SystemExit(main())
