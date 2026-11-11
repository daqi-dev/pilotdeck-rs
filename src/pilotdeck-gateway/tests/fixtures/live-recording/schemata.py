#!/usr/bin/env python3
"""#996: live-recording controls as compile-time switches.

The #743/#855-era text-replacement negative.py is deleted; its 16 static rows
and 6 generated payload-fields rows are now ``crate::mutants::on`` branches
compiled into the feature-enabled library test binary. The old script judged
through the legacy ``run_cases``/``classify`` API (named FAILED line + the
bound test's own assertion in the panic payload — the old baseline's
``bound test failed at its assertion`` reasons and its receipt-less output
dirs are the evidence), which is exactly the shared executor's default
verdict, so no ``receipts=True`` here. The shared-lease row's anchor is the
mutation's own HostError message the bound test unwraps. The two
``*-failure-not-success`` rows and each payload-fields cancel/resubmit pair
mutate one site each — both names gate the same branch so every row keeps its
own switch. CARGO_TARGET_DIR must be supplied. Use --check for a read-only
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
TURN = ROOT / 'src/pilotdeck-gateway/src/runtime/turn'
LIVE = TURN / 'live.rs'
RUNNER = TURN / 'runner.rs'
DRIVER = TURN / 'runner_recording.rs'
INTERACTION = TURN / 'interaction.rs'
STREAM = 'runtime::turn::live::stream_tests::'
HOST = 'runtime::project::factory::tests::interaction_host::'
PAYLOAD = '#743 original payload survives cancelled next'
GATE = 'recording gate must precede delivery'
# (name, path, test, assertion): the switch site is counted in the target file.
switches = [
    ('disposed-admission', LIVE, STREAM + 'live_recording_start_dispose',
     'disposed session stays closed'),
    ('active-admission', LIVE, STREAM + 'live_recording_start_resubmit',
     'pending write blocks resubmit'),
    ('start-error-propagates', RUNNER, STREAM + 'live_recording_start_failure',
     '#743 recording expected failure'),
    ('shared-lease-exclusive-only', INTERACTION,
     HOST + 'recording_shared_lease_outlives_handler_cleanup',
     'shared recorder lease incorrectly blocks handler cleanup'),
    ('accept-failure-not-success', RUNNER, STREAM + 'live_recording_accept_failure',
     'integrity failure terminal must be error'),
    ('complete-failure-not-success', RUNNER, STREAM + 'live_recording_complete_failure',
     'integrity failure terminal must be error'),
    ('real-slot', LIVE, STREAM + 'live_recording_start_drop',
     'worker owns active slot'),
    ('worker-slot', RUNNER, STREAM + 'live_recording_accept_drop',
     'worker owns active slot'),
    ('worker-lease', RUNNER, STREAM + 'live_recording_complete_dispose',
     'worker retains lease'),
    ('waiter-takes-transition', DRIVER, STREAM + 'live_recording_accept_cancel',
     'pending predecessor'),
    ('pending-payload', LIVE, STREAM + 'live_recording_complete_cancel', PAYLOAD),
    ('terminal-releases-runner', LIVE, STREAM + 'live_recording_complete_cancel',
     'EOF stream releases slot'),
    ('accepted-marker', LIVE, STREAM + 'live_recording_complete_cancel', GATE),
    ('complete-dispatch', LIVE, STREAM + 'live_recording_complete_cancel', GATE),
    ('accept-dispatch', LIVE, STREAM + 'live_recording_accept_cancel', GATE),
    ('start-dispatch', LIVE, STREAM + 'live_recording_start_cancel', GATE),
    ('payload-fields-start-cancel', LIVE, STREAM + 'live_recording_start_cancel',
     PAYLOAD),
    ('payload-fields-start-resubmit', LIVE, STREAM + 'live_recording_start_resubmit',
     PAYLOAD),
    ('payload-fields-accept-cancel', LIVE, STREAM + 'live_recording_accept_cancel',
     PAYLOAD),
    ('payload-fields-accept-resubmit', LIVE, STREAM + 'live_recording_accept_resubmit',
     PAYLOAD),
    ('payload-fields-complete-cancel', LIVE, STREAM + 'live_recording_complete_cancel',
     PAYLOAD),
    ('payload-fields-complete-resubmit', LIVE,
     STREAM + 'live_recording_complete_resubmit', PAYLOAD),
]
FIXTURES = ROOT / 'src/pilotdeck-gateway/tests/fixtures'
GUARDED = [
    LIVE,
    RUNNER,
    DRIVER,
    INTERACTION,
    # 21 of 22 rows bind tests in live::stream_tests, and one binds a
    # factory::tests::interaction_host test (schemata `switches`): the bound
    # test sources are hash-guarded like the old script's compile-only paths.
    ROOT / 'src/pilotdeck-gateway/src/runtime/turn/live_stream_tests.rs',
    ROOT / 'src/pilotdeck-gateway/src/runtime/project/interaction_host_tests.rs',
    ROOT / 'src/pilotdeck-gateway/src/mutants.rs',
    ROOT / 'src/pilotdeck-gateway/src/lib.rs',
    ROOT / 'src/pilotdeck-gateway/Cargo.toml',
    ROOT / 'src/Cargo.toml',
    ROOT / 'src/Cargo.lock',
    FIXTURES / 'live-recording/oracle.json',
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
