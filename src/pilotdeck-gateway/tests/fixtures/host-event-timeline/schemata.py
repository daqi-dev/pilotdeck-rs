#!/usr/bin/env python3
"""#1220: host-event timeline controls as compile-time switches.

The coordinate-projection mutations (allocator order, boundary emission,
derived-status projection, compaction terminal positioning, the #821
wire-drain deviation, and the two submit() timeline wirings) are
``crate::mutants::on`` branches compiled into the feature-enabled library
test binary; sources are never written, so the run is a pure
precompiled-switch execution. CARGO_TARGET_DIR must be supplied. Use --check
for a read-only anchor preflight, or NEW_OUTPUT_DIR [switch ...] for the
controlled run.
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
# Assertions are bound to one case-level cell each, so no variant is judged by
# another case's panic.
REPLAY = ('runtime::turn::live::stream_tests::host_timeline_tests::'
          'host_timeline_replay_matches_ts_golden')
SUBMIT = ('runtime::turn::live::stream_tests::'
          'submit_assembles_one_timeline_across_wire_host_and_terminal')
CANCEL_WINDOW = ('runtime::turn::live::stream_tests::'
                 'session_abort_still_drains_and_positions_post_cancel_events')
switches = [
    ('timeline-order-frozen', TIMELINE, REPLAY, '#1220 timeline: wire-deltas event 5'),
    ('timeline-boundary-omitted', TIMELINE, REPLAY, '#1220 timeline: wire-boundaries event 2'),
    ('timeline-derived-unprojected', LIVE, REPLAY, '#1220 timeline: subagent-derived event 3'),
    ('timeline-compaction-unpositioned', LIVE, REPLAY,
     '#1220 timeline: compaction-incomplete-final terminal messages'),
    ('wire-drains-host', LIVE, REPLAY,
     '#1220 timeline: host-pending-wire-same-tick wire event body (crossed)'),
    ('timeline-forwarding-unwired', LIVE, SUBMIT,
     '#1220 submit timeline: the wire block takes the first position'),
    ('timeline-terminal-unwired', LIVE, SUBMIT,
     '#1220 submit timeline: the terminal reuses the wire position without a revision bump'),
    # fix-r2 P1-1: the aborted terminal must report the execute-time snapshot
    # (TS abortedResult(input).messages), never the runner's later in-turn
    # durable loop messages.
    ('live-abort-finish-messages', LIVE, REPLAY,
     '#1220 timeline: compaction-incomplete-final terminal messages'),
    # fix-r2 P2-1: a cancelled turn must keep pumping — post-abort host events
    # are still drained and positioned at the next tick.
    ('timeline-cancel-stops-pump', LIVE, CANCEL_WINDOW,
     '#1220 cancel window: the host event queued after the abort is still drained and positioned'),
]
FIXTURES = ROOT / 'src/pilotdeck-gateway/tests/fixtures'
GUARDED = [
    TIMELINE,
    LIVE,
    ROOT / 'src/pilotdeck-gateway/src/runtime/turn/live_stream_tests.rs',
    ROOT / 'src/pilotdeck-gateway/src/runtime/turn/host_timeline_tests.rs',
    ROOT / 'src/pilotdeck-gateway/src/host/mod.rs',
    ROOT / 'src/pilotdeck-gateway/src/host/execution.rs',
    ROOT / 'src/pilotdeck-gateway/src/host/tests.rs',
    ROOT / 'src/pilotdeck-gateway/src/mutants.rs',
    ROOT / 'src/pilotdeck-gateway/src/lib.rs',
    ROOT / 'src/pilotdeck-gateway/Cargo.toml',
    ROOT / 'src/Cargo.toml',
    ROOT / 'src/Cargo.lock',
    ROOT / 'scripts/a6-host/fixture.py',
    FIXTURES / '_shared/mutant_switch.py',
    FIXTURES / 'negative_suite.py',
    FIXTURES / 'negative_process.py',
    FIXTURES / 'host-event-timeline/oracle.json',
    FIXTURES / 'host-event-timeline/generate.mts',
    FIXTURES / 'live-session/peer.py',
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
