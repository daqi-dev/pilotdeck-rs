#!/usr/bin/env python3
"""#1220: host-event timeline controls as compile-time switches.

The three coordinate-projection mutations are ``crate::mutants::on`` branches
compiled into the feature-enabled library test binary; sources are never
written, so the run is a pure precompiled-switch execution. CARGO_TARGET_DIR
must be supplied. Use --check for a read-only anchor preflight, or
NEW_OUTPUT_DIR [switch ...] for the controlled run.
"""
import argparse
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from _shared import mutant_switch

ROOT = Path(__file__).resolve().parents[5]
TIMELINE = ROOT / 'src/pilotdeck-gateway/src/runtime/turn/timeline.rs'
LIVE = ROOT / 'src/pilotdeck-gateway/src/runtime/turn/live.rs'
# (name, path, test, assertion): the switch site is counted in the target file.
REPLAY = ('runtime::turn::live::stream_tests::host_timeline_tests::'
          'host_timeline_replay_matches_ts_golden')
switches = [
    ('timeline-order-frozen', TIMELINE, REPLAY, '#1220 timeline'),
    ('timeline-boundary-omitted', TIMELINE, REPLAY, '#1220 timeline'),
    ('timeline-derived-unprojected', LIVE, REPLAY, '#1220 timeline'),
]
FIXTURES = ROOT / 'src/pilotdeck-gateway/tests/fixtures'
GUARDED = [
    TIMELINE,
    LIVE,
    ROOT / 'src/pilotdeck-gateway/src/runtime/turn/live_stream_tests.rs',
    ROOT / 'src/pilotdeck-gateway/src/runtime/turn/host_timeline_tests.rs',
    ROOT / 'src/pilotdeck-gateway/src/mutants.rs',
    ROOT / 'src/pilotdeck-gateway/src/lib.rs',
    ROOT / 'src/pilotdeck-gateway/Cargo.toml',
    ROOT / 'src/Cargo.toml',
    ROOT / 'src/Cargo.lock',
    FIXTURES / '_shared/mutant_switch.py',
    FIXTURES / 'negative_suite.py',
    FIXTURES / 'negative_process.py',
    FIXTURES / 'host-event-timeline/oracle.json',
    FIXTURES / 'host-event-timeline/generate.mts',
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
