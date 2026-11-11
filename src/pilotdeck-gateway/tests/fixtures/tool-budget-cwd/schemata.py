#!/usr/bin/env python3
"""#996: tool-budget-cwd controls as compile-time switches.

The #946 text-replacement negative_controls.py and session_negative.py are
deleted; each mutation is now a ``crate::mutants::on`` branch compiled into
the feature-enabled library test binary. CARGO_TARGET_DIR must be supplied.
Use --check for a read-only anchor preflight, or NEW_OUTPUT_DIR [switch ...].

Both old entries judged kills through the #946 nonce receipt protocol, so the
shared run keeps it (receipts=True): a bound test only counts as killed on a
named FAILED line plus this run's assertion receipt, never on prose.
"""
import argparse
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from _shared import mutant_switch

ROOT = Path(__file__).resolve().parents[5]
BUDGET = ROOT / 'src/pilotdeck-gateway/src/runtime/context/tool_budget.rs'
FACTORY = ROOT / 'src/pilotdeck-gateway/src/runtime/turn/host_factory.rs'
LIVE = ROOT / 'src/pilotdeck-gateway/src/runtime/turn/live.rs'
CWD = 'runtime::context::tool_budget::cwd_tests::'
# (name, path, test, assertion): the switch site is counted in the target file.
# The cwd-root-fallback row binds every outcome-matrix cell, so no cell is
# judged by another cell's anchor.
switches = [
    ('budget-cwd-root-fallback', BUDGET,
     [CWD + 'relative_deleted', CWD + 'relative_warmed_deleted'],
     ['#885 outcome matrix: relative-deleted',
      '#885 outcome matrix: relative-warmed-deleted']),
    ('budget-cwd-syscall', BUDGET, CWD + 'relative_deleted',
     '#885 outcome matrix: relative-deleted'),
    ('budget-cwd-rust-display', BUDGET, CWD + 'relative_deleted',
     '#885 outcome matrix: relative-deleted'),
    ('budget-absolute-reads-cwd', BUDGET, CWD + 'absolute_deleted',
     '#885 outcome matrix: absolute-deleted'),
    ('budget-session-kind', FACTORY,
     'runtime::turn::host_factory::tests::'
     'session_composition_uv_cwd_error_reaches_stream_unwrapped',
     '#885 composition failure must preserve the TS uv_cwd message on the first poll'),
    ('budget-live-prefix', LIVE,
     'runtime::turn::host_factory::tests::'
     'session_composition_uv_cwd_error_reaches_stream_unwrapped',
     '#885 composition failure must preserve the TS uv_cwd message on the first poll'),
]
FIXTURES = ROOT / 'src/pilotdeck-gateway/tests/fixtures'
GUARDED = [
    BUDGET,
    FACTORY,
    LIVE,
    # #2228 DS r2 P1: the cwd_tests bindings are assembled by tool_budget.rs's
    # `#[path]` of the fixture-internal regression.rs (whose include_str! reads
    # oracle.json), and generate.mts keeps the TS oracle in sync — the sibling
    # cwd fixtures (metadata-process-cwd, stats-cwd, file-history-cwd,
    # model-catalog-cwd, native-collation) register all three, so a concurrent
    # edit during a run must be caught by the source guard here too.
    FIXTURES / 'tool-budget-cwd/regression.rs',
    FIXTURES / 'tool-budget-cwd/oracle.json',
    FIXTURES / 'tool-budget-cwd/generate.mts',
    # #2228 zai r4 P2 (fixed in-round, same class as the turn-gates P1): every
    # bound cwd test executes the shared baseline module — regression.rs:9-10
    # matrix() starts with crate::test_baseline::oracle(ORACLE) — the same
    # input the five sibling fixtures already register.
    ROOT / 'src/pilotdeck-gateway/tests/common/baseline.rs',
    # #2228 r6 (MM boundary P1 class, no regeneration binding here): the
    # baseline module is unconditionally compiled with its #[path] submodule
    # tests/common/regeneration_masks.rs, so a concurrent edit to it during a
    # run must be caught by the source guard; the mask table itself stays
    # unregistered because no cwd binding runs a regeneration compare over
    # it (the table is a dead input for this fixture).
    ROOT / 'src/pilotdeck-gateway/tests/common/regeneration_masks.rs',
    ROOT / 'src/pilotdeck-gateway/src/runtime/turn/host_factory_tests.rs',
    # #2228 r9 (MM r8 sibling P2, both families): the registered carrier
    # host_factory_tests.rs bakes host-factory/oracle.json (:463/:562/:614)
    # and host-manifest/oracle.json (:1495) into the same --lib binary
    # (tool-budget-cwd/oracle.json at :803 is registered above), and LIVE
    # (registered above) bakes live-e2e/terminal-boundaries/oracle.json
    # (live.rs:2143) — so concurrent edits to any of them during a run must
    # be caught by the source guard.
    FIXTURES / 'host-factory/oracle.json',
    FIXTURES / 'host-manifest/oracle.json',
    FIXTURES / 'live-e2e/terminal-boundaries/oracle.json',
    # #2228 r10 (MM r9 sibling P2#3 + DS r9 sibling P2, both families): LIVE
    # (registered above) assembles its two `#[path]` faces into the same --lib
    # binary — live_stream_tests.rs (live.rs:2608) and live_tail_tests.rs
    # (live.rs:2612) — and live_stream_tests.rs bakes five more oracles
    # (live-recording :3653/:3958, live-history :4002, sidecar-payload :4273,
    # live-write-order :4816, turn-runner :3056/:3265/:4389); the same
    # direct-oracle set as the other fixtures registering this carrier
    # (DS r10 + MM r10, both families). Concurrent edits must be caught
    # by the source guard.
    ROOT / 'src/pilotdeck-gateway/src/runtime/turn/live_stream_tests.rs',
    ROOT / 'src/pilotdeck-gateway/src/runtime/turn/live_tail_tests.rs',
    FIXTURES / 'live-recording/oracle.json',
    FIXTURES / 'live-history/oracle.json',
    FIXTURES / 'sidecar-payload/oracle.json',
    FIXTURES / 'live-write-order/oracle.json',
    FIXTURES / 'turn-runner/oracle.json',
    # #2228 DS r3 P2 / zai r3 P2 (independent hit): the declarators above the
    # switch sites — runtime/turn/mod.rs declares host_factory/live, and
    # runtime/mod.rs declares turn; context/mod.rs declares tool_budget itself.
    ROOT / 'src/pilotdeck-gateway/src/runtime/turn/mod.rs',
    ROOT / 'src/pilotdeck-gateway/src/runtime/mod.rs',
    ROOT / 'src/pilotdeck-gateway/src/runtime/context/mod.rs',
    # Workers are re-executed with env_clear; the selector forwarding lives
    # here and must not change under a running suite.
    ROOT / 'src/pilotdeck-gateway/tests/common/negative_receipt.rs',
    ROOT / 'src/pilotdeck-gateway/src/test_process.rs',
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
                                 requested=args.names, receipts=True)
    except (ValueError, OSError) as error:
        print(f'preflight: {error}', file=sys.stderr)
        return 2


if __name__ == '__main__':
    sys.exit(main())
