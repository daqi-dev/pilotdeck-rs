#!/usr/bin/env python3
"""#996: turn-prompt-hooks controls as compile-time switches.

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
PROMPT = SRC / 'runner_prompt.rs'
LIVE = SRC / 'live.rs'
# The regeneration switch lives at the library-test call site: the shared
# baseline module is compiled into integration-test crates as well, where the
# library's switches do not exist.
PROMPT_TESTS = SRC / 'runner_prompt_tests.rs'
BASELINE = ROOT / 'src/pilotdeck-gateway/tests/common/baseline.rs'
UNIT = 'runtime::turn::runner::tests::prompts::'
HOST = 'runtime::project::factory::tests::interaction_host::prompt_hooks::'
# (name, path, test, assertion): the switch site is counted in the target file.
switches = [
    ('prompt-wiring', LIVE, HOST + 'prompt_host_pass',
     '#741 prompt live context forwarded'),
    ('prompt-gate', LIVE, HOST + 'prompt_host_pass',
     '#741 prompt live submitted'),
    # fix-r3 DS P2-2: the execute-time snapshot the aborted terminal reports.
    # All-cells: the pass case kills it on the loop payload the sidecar echoes
    # in its final; the #1220 case kills it on the payload the peer itself
    # captured at execute time and echoed to a file, before the synthesized
    # aborted terminal is compared against that capture.
    ('prompt-execute-context', LIVE,
     [HOST + 'prompt_host_pass',
      HOST + 'prompt_host_aborted_terminal_reports_the_execute_payload_with_hook_context'],
     ['#741 prompt live context forwarded',
      '#1220 prompt live execute payload carries the hook context']),
    ('prompt-abort-context', LIVE, HOST + 'prompt_host_context_abort',
     '#741 prompt live context forwarded'),
    ('prompt-block-gate', LIVE, HOST + 'prompt_host_block',
     '#741 prompt live execute gate'),
    ('prompt-accepted-first', PROMPT, UNIT + 'before_accept',
     '#741 prompt requires accepted input'),
    ('prompt-submitted-once', PROMPT, UNIT + 'pending',
     '#741 prompt duplicate rejected'),
    ('prompt-block-effect', PROMPT, UNIT + 'block',
     '#741 prompt loop input'),
    ('prompt-hook-context', PROMPT, UNIT + 'context',
     '#741 prompt loop input'),
    ('prompt-dispatch-reject', PROMPT, UNIT + 'reject',
     ['#741 prompt rejection', '#741 prompt loop input']),
    ('prompt-cancel-pending', PROMPT, UNIT + 'abort_pending',
     '#741 prompt pending bound'),
    ('prompt-cancel-signal', PROMPT, UNIT + 'drop_pending',
     '#741 prompt signal on cancel only'),
    ('prompt-text-coercion', PROMPT, UNIT + 'blocks_coerce',
     '#741 prompt events'),
    ('prompt-null-block', PROMPT, UNIT + 'blocks_null',
     ['#741 prompt rejection', '#741 prompt loop input']),
    ('prompt-regeneration', PROMPT_TESTS, UNIT + 'regenerates_identically',
     '#741 prompt fixture regeneration'),
]
FIXTURES = ROOT / 'src/pilotdeck-gateway/tests/fixtures'
GUARDED = [
    LIVE,
    PROMPT,
    PROMPT_TESTS,
    BASELINE,
    # #2228 r6 (MM boundary P1 / DS sibling+concurrency P2, same class as
    # turn-runner-tail): baseline.rs unconditionally compiles its #[path]
    # submodule tests/common/regeneration_masks.rs; the prompt-regeneration
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
    ROOT / 'src/pilotdeck-gateway/src/runtime/project/interaction_prompt_tests.rs',
    # #1274 opus P2: the HOST bindings run through factory_tests.rs's #[path]
    # assembly of this module — client()/client_with_arg() (peer argument,
    # execution_timeout) live here, so a concurrent edit during a run must be
    # caught by the source guard.
    ROOT / 'src/pilotdeck-gateway/src/runtime/project/interaction_host_tests.rs',
    # #1274 MM r1 P1: the parents of that #[path] chain — factory_tests.rs
    # assembles this module and defines the Scratch/helpers the bindings
    # inherit via `use super::*`, and factory.rs assembles factory_tests
    # itself, so concurrent edits to either must be caught by the source
    # guard.
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
    # #2228 DS r2 P2: the UNIT bindings run through runner.rs's #[path]
    # assembly of runner_tests.rs, which assembles runner_prompt_tests.rs
    # itself; the bindings inherit via `use super::*` from both parents, so
    # concurrent edits to either must be caught by the source guard.
    SRC / 'runner.rs',
    SRC / 'runner_tests.rs',
    # #2228 DS r7 concurrency P2 / MM r7 sibling P2 (two families): the twin
    # carrier of the chain above — runner_tests.rs also assembles
    # runner_notice_tests.rs, whose row() bakes turn-start-notices/oracle.json
    # into the same binary, so concurrent edits to either must be caught by
    # the source guard (turn-start-notices registers the mirror pair).
    SRC / 'runner_notice_tests.rs',
    FIXTURES / 'turn-start-notices/oracle.json',
    # #2228 r9 (MM r8 boundary P2 + DS r8, both families): the registered
    # carriers bake more inputs into the same --lib binary — runner_tests.rs
    # bakes turn-runner/oracle.json (:141/:352/:858 include_str!) and the
    # source text of live_stream_tests.rs (:864, consumed by the
    # oracle_consumers_exist existence check), and this fixture's
    # prompt-regeneration row also reads turn-runner/oracle.json at runtime
    # (turn-prompt-hooks/generate.mts:14 readFileSync); live.rs (registered
    # above) assembles live_stream_tests.rs (:2608) and live_tail_tests.rs
    # (:2612) via `#[path]` and bakes live-e2e/terminal-boundaries/oracle.json
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
    # #2228 DS r3 P1: the row bindings bake turn-prompt-hooks/oracle.json into
    # the binary (include_str! via runner_prompt_tests.rs) and the
    # prompt-regeneration binding runs turn-prompt-hooks/generate.mts through
    # tests/common/baseline.rs, so concurrent edits to either during a run must
    # be caught by the source guard.
    FIXTURES / 'turn-prompt-hooks/oracle.json',
    FIXTURES / 'turn-prompt-hooks/generate.mts',
    # #2228 DS r3 P2 / zai r3 P2 (independent hit): the top of the turn chain —
    # runtime/turn/mod.rs declares the runner module (and live) the bindings
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
