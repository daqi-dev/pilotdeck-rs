#!/usr/bin/env python3
"""#996: turn-start-notices controls as compile-time switches.

The #741 text-replacement negative_controls.py is deleted; each mutation is
now a ``crate::mutants::on`` branch compiled into the feature-enabled library
test binary. CARGO_TARGET_DIR and PDRS_BASELINE must be supplied. Use --check
for a read-only anchor preflight, or NEW_OUTPUT_DIR [switch ...].
"""
import argparse
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from _shared import mutant_switch

ROOT = Path(__file__).resolve().parents[5]
SRC = ROOT / 'src/pilotdeck-gateway/src/runtime/turn'
EVENTS = ROOT / 'src/pilotdeck-gateway/src/runtime/project/interaction_events.rs'
# The regeneration switch lives at the library-test call site: the shared
# baseline module is compiled into integration-test crates as well, where the
# library's switches do not exist.
NOTICE_TESTS = SRC / 'runner_notice_tests.rs'
BASELINE = ROOT / 'src/pilotdeck-gateway/tests/common/baseline.rs'
NOTICES = 'runtime::turn::runner::tests::notices::'
# (name, path, test, assertion): the switch site is counted in the target file.
switches = [
    ('notices-wiring', SRC / 'live.rs',
     'runtime::project::factory::tests::interaction_host::'
     'host_next_turn_surfaces_the_pending_hook_discard_notice',
     '#741 notice live start order'),
    ('notices-notify', SRC / 'runner.rs', NOTICES + 'start_success',
     '#741 notice start output'),
    ('notices-start-order', SRC / 'runner.rs', NOTICES + 'start_success',
     '#741 notice start output'),
    ('notices-after-durable', SRC / 'runner.rs', NOTICES + 'start_failure',
     '#741 notice failed write did not consume'),
    ('notices-once', EVENTS, NOTICES + 'both_start_paths_consume_once',
     '#741 notice both paths once'),
    ('notices-stale', EVENTS, NOTICES + 'stale',
     '#741 notice stale rejected'),
    ('notices-closed', EVENTS, NOTICES + 'closed',
     '#741 notice closed rejected'),
    ('notices-buffer-isolation', EVENTS, NOTICES + 'buffered_events_are_not_stolen',
     '#741 notice unrelated buffer preserved'),
    ('notices-regeneration', NOTICE_TESTS, NOTICES + 'regenerates_identically',
     '#741 notice fixture regeneration'),
]
FIXTURES = ROOT / 'src/pilotdeck-gateway/tests/fixtures'
GUARDED = [
    SRC / 'live.rs',
    SRC / 'runner.rs',
    EVENTS,
    NOTICE_TESTS,
    BASELINE,
    ROOT / 'src/pilotdeck-gateway/src/mutants.rs',
    ROOT / 'src/pilotdeck-gateway/src/lib.rs',
    ROOT / 'src/pilotdeck-gateway/Cargo.toml',
    ROOT / 'src/Cargo.toml',
    ROOT / 'src/Cargo.lock',
    FIXTURES / '_shared/mutant_switch.py',
    FIXTURES / 'negative_suite.py',
    FIXTURES / 'negative_process.py',
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
        output.mkdir(parents=True, exist_ok=False)
        return mutant_switch.run(ROOT, output, chosen,
                                 mutant_switch.clean_environment(os.environ),
                                 guarded=GUARDED, prefix='', script=Path(__file__),
                                 requested=args.names)
    except (ValueError, OSError) as error:
        print(f'preflight: {error}', file=sys.stderr)
        return 2


if __name__ == '__main__':
    sys.exit(main())
