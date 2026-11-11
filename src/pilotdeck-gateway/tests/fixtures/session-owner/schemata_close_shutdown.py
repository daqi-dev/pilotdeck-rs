#!/usr/bin/env python3
"""#996: session-owner close/shutdown controls as compile-time switches.

The #1245/#1365 text-replacement negative_close_shutdown.py is deleted; each
must-kill mutation is now a ``crate::mutants::on`` branch compiled into the
feature-enabled library test binary. The old paired rows
(closing-settle-write-removed, idle-settle-write-removed) keep both sites —
the guard's settle write and the task's redirected ``done`` sender — behind
one switch each. The registered EQUIVALENT row (ticket-release-write-removed)
lives in schemata_equivalent.py, which must measure it SURVIVED on its own.
Kill anchors are the bound tests' own assertion messages the old runner
observed. CARGO_TARGET_DIR must be supplied. Use --check for a read-only
anchor preflight, or NEW_OUTPUT_DIR [switch ...].
"""
import argparse
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from _negkit import non_head
from _shared import mutant_switch

ROOT = Path(__file__).resolve().parents[5]
OWNER = ROOT / 'src/pilotdeck-gateway/src/runtime/session/owner.rs'
TESTS = 'runtime::session::owner::tests::'
DURING_SHUTDOWN = TESTS + 'close_during_shutdown_awaits_the_keys_own_eviction_matches_ts'
SECOND_CLOSE = TESTS + 'second_close_awaits_the_firsts_own_eviction_matches_ts'
IDLE_EVICT = TESTS + 'close_during_idle_eviction_awaits_the_idle_eviction_matches_ts'
CLOSE_THEN_SHUTDOWN = TESTS + 'close_then_shutdown_same_tick_matches_ts'
SHUTDOWN_THEN_CLOSE = TESTS + 'shutdown_then_close_same_tick_fulfills_matches_ts'
SKIP_ONLY_CLOSING = TESTS + 'shutdown_skips_only_closing_keys_and_drains_the_rest_matches_ts'
DRAIN_PANIC = TESTS + 'shutdown_drain_panic_settles_the_keys_closing_entry'
CLOSE_SETTLES = TESTS + 'close_settles_its_closing_entry_before_releasing_the_ticket'
IDLE_SETTLES = TESTS + 'idle_sweep_settles_its_closing_entry_before_releasing_the_ticket'
CLOSE_PANIC = TESTS + 'close_task_panic_settles_its_closing_entry_through_the_completion_guard'
# (name, path, test, assertion): the switch site is counted in the target file.
switches = [
    ('close-during-shutdown-aggregate', OWNER,
     [DURING_SHUTDOWN, SHUTDOWN_THEN_CLOSE],
     ['#1245 a succeeding mid-flight shutdown eviction must fulfill close',
      '#1245 a same-tick close after shutdown must fulfill before the drain '
      'registers']),
    ('close-else-awaits-nothing', OWNER, DURING_SHUTDOWN,
     '#1245 close of a mid-flight shutdown eviction must await it, '
     'not answer early'),
    ('close-call-registration-removed', OWNER,
     [SECOND_CLOSE, CLOSE_THEN_SHUTDOWN],
     ["#1245 each in-flight close of the key must reject with the eviction's "
      'own result',
      '#1245 a close that starts before a same-tick shutdown must reject '
      'with its own dispose failure']),
    ('idle-sweep-registration-removed', OWNER, IDLE_EVICT,
     '#1245 a failing mid-flight idle eviction must reject close'),
    ('shutdown-drain-registration-removed', OWNER, DURING_SHUTDOWN,
     '#1245 close of a mid-flight shutdown eviction must await it, '
     'not answer early'),
    ('shutdown-steals-close-owned-record', OWNER, CLOSE_THEN_SHUTDOWN,
     '#1245 a close that starts before a same-tick shutdown must reject '
     'with its own dispose failure'),
    ('shutdown-skip-stops-draining', OWNER, SKIP_ONLY_CLOSING,
     '#1245 the shutdown drain must still dispose the plain key after '
     'skipping the closing key'),
    ('drain-guard-removed', OWNER, DRAIN_PANIC,
     '#1245 a record-less close after shutdown must fulfill like TS '
     'await undefined'),
    ('settle-after-release', OWNER, [CLOSE_SETTLES, IDLE_SETTLES],
     ['#1245 the close task must settle its closing entry before '
      'releasing the serial ticket',
      '#1245 the idle sweep task must settle its closing entry before '
      'releasing the serial ticket']),
    ('closing-settle-write-removed', OWNER, CLOSE_SETTLES,
     '#1245 the close must fulfill with a succeeding disposal'),
    ('idle-settle-write-removed', OWNER, IDLE_EVICT,
     '#1245 close during an idle eviction transmits the per-key dispose '
     'aggregate'),
    ('close-panic-guard-removed', OWNER, CLOSE_PANIC,
     "#1365 the panicked close settles with the completion guard's dropped "
     'error'),
]
FIXTURES = ROOT / 'src/pilotdeck-gateway/tests/fixtures'
GUARDED = [
    OWNER,
    # All rows bind runtime::session::owner::tests tests defined in
    # owner_tests.rs; guard the bound test source like schemata.py does.
    ROOT / 'src/pilotdeck-gateway/src/runtime/session/owner_tests.rs',
    ROOT / 'src/pilotdeck-gateway/src/mutants.rs',
    ROOT / 'src/pilotdeck-gateway/src/lib.rs',
    ROOT / 'src/pilotdeck-gateway/Cargo.toml',
    ROOT / 'src/Cargo.toml',
    ROOT / 'src/Cargo.lock',
    FIXTURES / 'session-owner/oracle.json',
    FIXTURES / '_shared/mutant_switch.py',
    FIXTURES / '_negkit/non_head.py',
    FIXTURES / 'negative_suite.py',
    FIXTURES / 'negative_process.py', FIXTURES / 'anchor_selfcheck.py',
    Path(__file__).resolve(),
]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--check', action='store_true')
    parser.add_argument('output', nargs='?')
    parser.add_argument('names', nargs='*')
    args = parser.parse_args()
    try:
        rows = [dict(zip(('name', 'path', 'test', 'anchor'), row)) for row in switches]
        chosen = mutant_switch.selected(rows, args.names)
        mutant_switch.preflight(rows)
        if args.check:
            if args.output or args.names:
                raise ValueError('--check does not take output or names')
            print(f'{len(rows)} switch anchors and bindings checked')
            return 0
        if not args.output or not os.environ.get('CARGO_TARGET_DIR'):
            raise ValueError('NEW_OUTPUT_DIR and caller CARGO_TARGET_DIR are required')
        output = Path(args.output).resolve()
        # #1540: the compile-time-switch executor always runs the working
        # tree, so a pinned PDRS_NEGATIVE_BASELINE cannot be honoured: the
        # request is announced as ignored and a green run is still downgraded
        # (exit 42) — it cannot certify the caller-named tree, which is the
        # mistake exit 0 invited (network-timing precedent).
        baseline_source, _ignored_ref, baseline_sha = non_head.gate(
            ROOT, ignored=True)
        output.mkdir(parents=True, exist_ok=False)
        return non_head.non_head_exit(
            baseline_source, baseline_sha,
            code=mutant_switch.run(ROOT, output, chosen,
                                   mutant_switch.clean_environment(os.environ),
                                   guarded=GUARDED, prefix='', script=Path(__file__),
                                   requested=args.names),
            evidence=output, ignored=True)
    except (ValueError, OSError) as error:
        print(f'preflight: {error}', file=sys.stderr)
        return 2


if __name__ == '__main__':
    sys.exit(main())
