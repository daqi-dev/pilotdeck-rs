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
    # #2228 r6 (MM boundary P1 / DS sibling+concurrency P2, same class as
    # turn-runner-tail): baseline.rs unconditionally compiles its #[path]
    # submodule tests/common/regeneration_masks.rs; the notices-regeneration
    # row's compare() executes that code, so it is a runtime judge of this
    # fixture's verdict. r8 (DS r7 concurrency P2#2, code-verified): the shared
    # mask table stays OUT — regeneration_masks.rs normalized() early-returns
    # for every fixture but provider-timing and run-registry, so the table is
    # a dead input here and guarding a cross-fixture shared file would only
    # invite needless INVALID.
    ROOT / 'src/pilotdeck-gateway/tests/common/regeneration_masks.rs',
    # #2228 MM r7 sibling P1: the regeneration row's TS-side judge closure —
    # generate.mts imports assertBaseline/baselineSourceHashes from
    # _shared/baseline.mts, which itself imports _shared/generator_guard.mjs
    # and tests/baseline_source_guard.mjs; the regeneration row executes them
    # as its judge (TS-tree clean check and hash recording; r9 reword, MM
    # conc P3: the oracle records hashes of the TS baseline tree's files, it
    # does not bake these Rust-tree bytes), so concurrent edits during a run
    # must be caught by the source guard (same closure as turn-runner-tail).
    FIXTURES / '_shared/baseline.mts',
    FIXTURES / '_shared/generator_guard.mjs',
    ROOT / 'src/pilotdeck-gateway/tests/baseline_source_guard.mjs',
    # #1274 opus-P2 follow-along: the notices-wiring binding runs through
    # factory_tests.rs's #[path] assembly of this module (live_notice and its
    # Scratch/registry helpers live here), so a concurrent edit during a run
    # must be caught by the source guard.
    ROOT / 'src/pilotdeck-gateway/src/runtime/project/interaction_host_tests.rs',
    # #1274 MM r1 P1: the parents of that #[path] chain — factory_tests.rs
    # assembles this module and defines the Scratch/registry helpers the
    # binding inherits via `use super::*`, and factory.rs assembles
    # factory_tests itself, so concurrent edits to either must be caught by
    # the source guard.
    ROOT / 'src/pilotdeck-gateway/src/runtime/project/factory_tests.rs',
    ROOT / 'src/pilotdeck-gateway/src/runtime/project/factory.rs',
    # #2228 r11 (DS r10 sibling P1): the registered carrier factory_tests.rs
    # bakes two more oracles into the same --lib binary —
    # project-runtime-factory/oracle.json (:595) and interaction-events/
    # oracle.json (:3997) — no fixture had registered either, so concurrent
    # edits during a run must be caught by the source guard here too.
    FIXTURES / 'project-runtime-factory/oracle.json',
    FIXTURES / 'interaction-events/oracle.json',
    # #2228 MM r2 P2: the top of that chain — factory.rs inherits its names
    # from project.rs via `use super::{...}` (mod factory in project.rs), and
    # runtime/mod.rs declares the project module, so concurrent edits to
    # either must be caught by the source guard.
    ROOT / 'src/pilotdeck-gateway/src/runtime/project.rs',
    ROOT / 'src/pilotdeck-gateway/src/runtime/mod.rs',
    # #2228 DS r2 P2: the NOTICES bindings run through runner.rs's #[path]
    # assembly of runner_tests.rs, which assembles runner_notice_tests.rs
    # itself; the bindings inherit via `use super::*` from both parents, so
    # concurrent edits to either must be caught by the source guard.
    SRC / 'runner_tests.rs',
    # #2228 DS r7 concurrency P2 / MM r7 sibling P2 (two families): the twin
    # carrier of the chain above — runner_tests.rs also assembles
    # runner_prompt_tests.rs, whose row() bakes turn-prompt-hooks/oracle.json
    # into the same binary, so concurrent edits to either must be caught by
    # the source guard (turn-prompt-hooks registers the mirror pair).
    SRC / 'runner_prompt_tests.rs',
    FIXTURES / 'turn-prompt-hooks/oracle.json',
    # #2228 r9 (MM r8 boundary P2 + DS r8, both families): the registered
    # carriers bake more inputs into the same --lib binary — runner_tests.rs
    # bakes turn-runner/oracle.json (:141/:352/:858 include_str!) and the
    # source text of live_stream_tests.rs (:864 include_str!, consumed by the
    # oracle_consumers_exist existence check; it also bakes runner_tests.rs
    # itself at :863, which is registered above); live.rs (registered above)
    # assembles live_stream_tests.rs (:2608) and live_tail_tests.rs (:2612)
    # via `#[path]` and bakes live-e2e/terminal-boundaries/oracle.json
    # (:2143). Concurrent edits to any of them must be caught by the source
    # guard. Faces of these newly registered carriers (e.g. gates_tests.rs
    # under live_stream_tests.rs) stay out — they are registered by the
    # fixtures whose bound rows execute them, and the mechanical closure of
    # the whole compiled chain is the #2241 consolidation target. r9 (DS r8
    # P1, in-diff): native-metadata-cwd / native-process / live-e2e register
    # only baseline.mts of the judge closure — their gap is tracked in
    # #2346, not a precedent to copy.
    FIXTURES / 'turn-runner/oracle.json',
    SRC / 'live_stream_tests.rs',
    SRC / 'live_tail_tests.rs',
    FIXTURES / 'live-e2e/terminal-boundaries/oracle.json',
    # #2228 r10 (MM r9 sibling P1#2 + DS r9 concurrency P2, both families):
    # live_stream_tests.rs (registered above) bakes four more oracles into the
    # same --lib binary — live-recording/oracle.json (:3653/:3958),
    # live-history/oracle.json (:4002), sidecar-payload/oracle.json (:4273)
    # and live-write-order/oracle.json (:4816) — beyond turn-runner/
    # oracle.json registered in the r9 entry above. Same carrier, same
    # direct-oracle set as the schemata fixtures registering it;
    # live-recording/negative.py (a run_cases runner) puts the carrier in
    # its compile-only paths list but guards none of these oracles — its
    # judge-closure gap is tracked in #2346, and host-event-timeline/
    # turn-gates additionally register their own extra faces — #2241
    # consolidation target. Concurrent edits must be caught by the source
    # guard.
    FIXTURES / 'live-recording/oracle.json',
    FIXTURES / 'live-history/oracle.json',
    FIXTURES / 'sidecar-payload/oracle.json',
    FIXTURES / 'live-write-order/oracle.json',
    # #2228 DS r3 P1: the row bindings bake turn-start-notices/oracle.json
    # into the binary (include_str! via runner_notice_tests.rs), the
    # notices-regeneration binding runs turn-start-notices/generate.mts
    # through tests/common/baseline.rs, and the notices-wiring binding runs
    # live-session/peer.py as a python3 subprocess through
    # interaction_host_tests.rs's client(), so concurrent edits to any of them
    # during a run must be caught by the source guard.
    FIXTURES / 'turn-start-notices/oracle.json',
    FIXTURES / 'turn-start-notices/generate.mts',
    FIXTURES / 'live-session/peer.py',
    # #2228 DS r3 P2 / zai r3 P2 (independent hit): the top of the turn chain —
    # runtime/turn/mod.rs declares the live/runner modules the bindings
    # resolve through, capping the chain at the same declarator depth the
    # project chain above already registers.
    ROOT / 'src/pilotdeck-gateway/src/runtime/turn/mod.rs',
    ROOT / 'src/pilotdeck-gateway/src/mutants.rs',
    ROOT / 'src/pilotdeck-gateway/src/lib.rs',
    ROOT / 'src/pilotdeck-gateway/Cargo.toml',
    ROOT / 'src/Cargo.toml',
    ROOT / 'src/Cargo.lock',
    FIXTURES / '_shared/mutant_switch.py',
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
