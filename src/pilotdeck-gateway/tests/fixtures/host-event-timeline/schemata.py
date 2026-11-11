#!/usr/bin/env python3
"""#1220: host-event timeline controls as compile-time switches.

The coordinate-projection mutations (allocator order, boundary emission,
derived-status projection, compaction terminal positioning, the #821
wire-drain deviation, the two submit() timeline wirings, the aborted-terminal
message snapshot, and the two cancel-window pump checkpoints) are
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
# #1220 fix-r2 P2-1 / fix-r3: both cancel-window mutants park the pump
# (`pending()`), so the negative control dies at the timeout anchor inside the
# bounded wait. #1274 P2-1 widened the set for the break shape — an assembly
# that leaves the loop instead of parking makes next() resolve with the
# synthesized turn_failed terminal, the bounded wait's three expects pass, and
# the test dies at the `drained and positioned` assert_eq below them (or at
# the must-not-fail / must-not-end expects for the stream-error/None shapes).
# No break-form mutant is compiled in the current tree, so those anchors are
# the defensive alternate set: they keep a hypothetical break form judged
# KILLED instead of INVALID. #2228 r3: both rows share this list verbatim by
# design (either death anchor of the race is a valid kill for either mutant);
# adding a break-form mutant with runtime evidence is a tracked follow-up.
CANCEL_WINDOW_ANCHORS = [
    '#1220 cancel window: the first post-abort host event is still drained by the pump',
    '#1220 cancel window: the first host event queued after the abort is drained and positioned',
    '#1220 cancel window: draining the first post-abort host event must not fail',
    '#1220 cancel window: the first post-abort host event must not end the stream',
    '#1220 cancel window: the second post-abort host event is still drained by the pump',
    '#1220 cancel window: the second host event queued after the abort is drained and positioned',
    '#1220 cancel window: draining the second post-abort host event must not fail',
    '#1220 cancel window: the second post-abort host event must not end the stream',
]
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
    # are still drained and positioned at the next tick. fix-r3: the failing
    # assertion moved into the bounded wait itself, so the mutated pump dies at
    # the timeout anchor, not a 15 s silent no-message timeout. The anchor set
    # is the shared CANCEL_WINDOW_ANCHORS above.
    ('timeline-cancel-stops-pump', LIVE, CANCEL_WINDOW, CANCEL_WINDOW_ANCHORS),
    # fix-r3 DS P2-1: the loop-top twin of the in-tick checkpoint above — the
    # shape a cancel assembly would most naturally take, checked beside the
    # `stopped` flag. The iteration already in flight when the abort lands
    # still drains one more host batch, so the second post-abort host event
    # catches it; the first-event anchors cover the race where the abort is
    # observed at the very top and the first event is lost instead. The anchor
    # set is the same shared CANCEL_WINDOW_ANCHORS above.
    ('timeline-cancel-parks-pump', LIVE, CANCEL_WINDOW, CANCEL_WINDOW_ANCHORS),
]
FIXTURES = ROOT / 'src/pilotdeck-gateway/tests/fixtures'
GUARDED = [
    TIMELINE,
    LIVE,
    ROOT / 'src/pilotdeck-gateway/src/runtime/turn/live_stream_tests.rs',
    ROOT / 'src/pilotdeck-gateway/src/runtime/turn/host_timeline_tests.rs',
    # #2228 DS r7 sibling P2 (same class as the turn-gates entry): the
    # live_stream_tests.rs assembly above also compiles gates_tests.rs
    # (:4156), host_pump_tests.rs (:4159) and forward_tests.rs (:4319)
    # through `#[path]`, whose bodies bake turn-gates/oracle.json
    # (gates_tests.rs GATES_ORACLE), host-event-pump/oracle.json
    # (host_pump_tests.rs:4) and turn-runner/oracle.json (forward_tests.rs)
    # into the same --lib binary, so concurrent edits to the carriers or
    # their baked inputs must be caught by the source guard. turn-gates/
    # peer.py is executed only by turn-gates's own bound rows, not by this
    # fixture's, so it stays out here; the peer.py this fixture's bound rows
    # do run (live-session/peer.py) is registered below.
    ROOT / 'src/pilotdeck-gateway/src/runtime/turn/gates_tests.rs',
    ROOT / 'src/pilotdeck-gateway/src/runtime/turn/host_pump_tests.rs',
    ROOT / 'src/pilotdeck-gateway/src/runtime/turn/forward_tests.rs',
    FIXTURES / 'turn-gates/oracle.json',
    FIXTURES / 'host-event-pump/oracle.json',
    FIXTURES / 'turn-runner/oracle.json',
    # #2228 r9 (DS r8 boundary P1 class + MM r8 sibling/concurrency P2, both
    # families): the already-registered carriers bake further inputs into the
    # same --lib binary — gates_tests.rs:753 bakes pump-order-cases/oracle.json;
    # live_stream_tests.rs bakes live-recording/oracle.json (:3653/:3958),
    # live-history/oracle.json (:4002), sidecar-payload/oracle.json (:4273)
    # and live-write-order/oracle.json (:4816); live.rs:2612 assembles
    # live_tail_tests.rs via `#[path]`, and live.rs:2143 bakes live-e2e/
    # terminal-boundaries/oracle.json. Concurrent edits to any of them must be
    # caught by the source guard.
    FIXTURES / 'pump-order-cases/oracle.json',
    FIXTURES / 'live-recording/oracle.json',
    FIXTURES / 'live-history/oracle.json',
    FIXTURES / 'sidecar-payload/oracle.json',
    FIXTURES / 'live-write-order/oracle.json',
    ROOT / 'src/pilotdeck-gateway/src/runtime/turn/live_tail_tests.rs',
    FIXTURES / 'live-e2e/terminal-boundaries/oracle.json',
    # #2228 zai r4 P2 (fixed in-round, same class as the turn-gates P1): the
    # bound replay test executes the shared baseline module —
    # host_timeline_tests.rs:14-15 golden() is crate::test_baseline::oracle
    # — the same input the five sibling fixtures already register. (r6
    # correction, MM boundary P3: the :425 regenerates_identically test is
    # #[ignore] and not among this fixture's bound rows, so this entry covers
    # the bound replays only.)
    ROOT / 'src/pilotdeck-gateway/tests/common/baseline.rs',
    # #2228 r6 (MM boundary P1 class, no regeneration binding here): the
    # baseline module is unconditionally compiled with its #[path] submodule
    # tests/common/regeneration_masks.rs, so a concurrent edit to it during a
    # run must be caught by the source guard; the mask table itself stays
    # unregistered because no bound row runs a regeneration compare over it
    # (the table is a dead input for this fixture).
    ROOT / 'src/pilotdeck-gateway/tests/common/regeneration_masks.rs',
    # #2228 DS r3 P2 / zai r3 P2 (independent hit): the top of the turn chain —
    # runtime/turn/mod.rs declares the live/timeline modules the bindings
    # resolve through, and runtime/mod.rs declares turn itself, capping the
    # chain at the same declarator depth the project chain registers elsewhere.
    ROOT / 'src/pilotdeck-gateway/src/runtime/turn/mod.rs',
    ROOT / 'src/pilotdeck-gateway/src/runtime/mod.rs',
    ROOT / 'src/pilotdeck-gateway/src/host/mod.rs',
    ROOT / 'src/pilotdeck-gateway/src/host/execution.rs',
    ROOT / 'src/pilotdeck-gateway/src/host/tests.rs',
    # #2228 r10 (MM r9 sibling P2#5): host/tests.rs (registered above)
    # assembles its six `mod` faces into the same --lib binary
    # (src/host/tests.rs:6-11) — binding.rs, cancel_exit.rs, event.rs,
    # model.rs, modules.rs, resume.rs — and those faces bake five more
    # oracles: host-module-binding/oracle.json (binding.rs:22),
    # host-cancel-timeout/oracle.json (tests.rs:920), host-event/oracle.json
    # (event.rs:12), host-model/oracle.json (model.rs:18) and
    # host-modules/oracle.json (modules.rs:130). r9 registered
    # sidecar-payload/oracle.json (an input of event.rs) without the face
    # that bakes it; this closes the direct set. Faces of the faces (deeper
    # `#[path]` levels) stay out — #2241 consolidation target.
    ROOT / 'src/pilotdeck-gateway/src/host/tests/binding.rs',
    ROOT / 'src/pilotdeck-gateway/src/host/tests/cancel_exit.rs',
    ROOT / 'src/pilotdeck-gateway/src/host/tests/event.rs',
    ROOT / 'src/pilotdeck-gateway/src/host/tests/model.rs',
    ROOT / 'src/pilotdeck-gateway/src/host/tests/modules.rs',
    ROOT / 'src/pilotdeck-gateway/src/host/tests/resume.rs',
    FIXTURES / 'host-module-binding/oracle.json',
    FIXTURES / 'host-cancel-timeout/oracle.json',
    FIXTURES / 'host-event/oracle.json',
    FIXTURES / 'host-model/oracle.json',
    FIXTURES / 'host-modules/oracle.json',
    ROOT / 'src/pilotdeck-gateway/src/mutants.rs',
    ROOT / 'src/pilotdeck-gateway/src/lib.rs',
    ROOT / 'src/pilotdeck-gateway/Cargo.toml',
    ROOT / 'src/Cargo.toml',
    ROOT / 'src/Cargo.lock',
    ROOT / 'scripts/a6-host/fixture.py',
    # #2228 r12 (DS r11 boundary P1 + MM r11 concurrency P2/boundary P3,
    # criterion unified): runtime-read (spawned/imported) inputs are
    # registered iff this fixture's bound rows execute them — baked
    # (include_str!) inputs follow the carrier closure instead. The
    # timeline-submit bound row runs fixture.py (live_stream_tests.rs:829
    # session() -> :636-655 spawn_client_with assembles the peer command;
    # host/tests.rs:291-295 command() is the host::tests-local helper other
    # rows use), and fixture.py imports lifetime unconditionally at its
    # top (fixture.py:11, the #1779 parent-death/lifetime watchdog), so
    # both must be caught. The remaining a6-host scripts are dead inputs
    # here: orphan_probe.py (spawned at host/tests.rs:1219/:1270) and
    # preinstall_probe.py (:1347) are launched only by host::tests rows this
    # fixture's bound rows never run, and resume_fixture.py (fixture.py:
    # 46-49, reconnect-* modes only) has no such bound row either — r11's
    # registration of the two probes is reverted with this criterion, and
    # host-cancel-timeout/peer.py (cancel_exit.rs:11-17) stays out the same
    # way turn-gates/peer.py does (above).
    ROOT / 'scripts/a6-host/lifetime.py',
    FIXTURES / '_shared/mutant_switch.py',
    FIXTURES / 'negative_suite.py',
    FIXTURES / 'negative_process.py', FIXTURES / 'anchor_selfcheck.py',
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
