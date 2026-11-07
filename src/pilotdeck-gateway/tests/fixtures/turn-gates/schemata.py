#!/usr/bin/env python3
"""#996: turn-gates durability controls as compile-time switches.

The #743 text-replacement negative.py is deleted; each mutation is now a
``crate::mutants::on`` branch compiled into the feature-enabled library test
binary. CARGO_TARGET_DIR must be supplied. Use --check for a read-only anchor
preflight, or NEW_OUTPUT_DIR [switch ...].
"""
import argparse
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from _shared import mutant_switch

ROOT = Path(__file__).resolve().parents[5]
SRC = ROOT / 'src/pilotdeck-gateway/src/runtime/turn'
FACTORY_TESTS = ROOT / 'src/pilotdeck-gateway/src/runtime/turn/host_factory_tests.rs'
RUNNER_TESTS = ROOT / 'src/pilotdeck-gateway/src/runtime/turn/runner_tests.rs'
GATES_TESTS = ROOT / 'src/pilotdeck-gateway/src/runtime/turn/gates_tests.rs'
PEER = ROOT / 'src/pilotdeck-gateway/tests/fixtures/host-factory/peer.py'
STREAM = 'runtime::turn::live::stream_tests::'
GATES = STREAM + 'gates_tests::'
# (name, path, test, assertion): the switch site is counted in the target file.
switches = [
    ('gates-factory-close-timeout', SRC / 'host_factory.rs',
     STREAM + 'live_prepared_cleanup_uses_close_timeout',
     'factory forwards close_timeout'),
    ('gates-prepared-close-timeout', SRC / 'live.rs',
     STREAM + 'live_prepared_cleanup_uses_close_timeout',
     ['cleanup must stop at the configured deadline',
      'cleanup must retain the configured budget']),
    ('gates-consumer-exists', RUNNER_TESTS,
     'runtime::turn::runner::tests::oracle_consumers_exist',
     'missing Rust consumer: runner_nonexistent_consumer'),
    ('gates-skip-start', SRC / 'runner.rs',
     GATES + 'gates_start_failure', '#743 gates start failure emits no event'),
    ('gates-eager-connect', SRC / 'live.rs',
     GATES + 'gates_accept_failure', '#743 gates failed persistence never connects'),
    ('gates-skip-accept', SRC / 'runner.rs',
     GATES + 'gates_accept_failure', '#743 gates execute count'),
    ('gates-skip-complete', SRC / 'runner.rs',
     GATES + 'gates_cancel_ok', 'complete count'),
    ('gates-skip-failure-complete', SRC / 'runner.rs',
     GATES + 'gates_error_ok', 'complete count'),
    ('gates-release-pending-slot', SRC / 'runner.rs',
     GATES + 'gates_drop_pending', 'worker owns slot after drop'),
    ('gates-discard-host-failure', SRC / 'live.rs',
     GATES + 'gates_protocol_ok', '#743 gates complete count'),
    ('gates-original-success-after-fallback', SRC / 'live.rs',
     GATES + 'gates_cancel_once', '#743 gates result kind'),
    ('gates-omit-prepared-cleanup', SRC / 'live.rs',
     GATES + 'gates_start_failure', 'handler cleanup exactly once'),
    ('gates-factory-ready-priority', SRC / 'live.rs',
     GATES + 'gates_factory_ready_cancel', '#743 gates pre-cancel skips factory'),
    ('gates-duplicate-normal-terminal', SRC / 'live.rs',
     GATES + 'gates_cancel_ok', '#743 turn-only terminal once'),
    ('gates-sidecar-lifecycle-leak', SRC / 'live.rs',
     'runtime::turn::live::tests::host_lifecycle_cannot_justify_a_sidecar_final',
     'host lifecycle leak must be rejected'),
    ('gates-duplicate-host-terminal', SRC / 'live.rs',
     GATES + 'gates_cancel_once', '#743 turn-only terminal once'),
    ('gates-multi-turn-peer-marker', FACTORY_TESTS,
     'runtime::turn::host_factory::tests::'
     'live_session_shares_recorder_and_reads_current_history',
     '#741k every history turn must execute successfully in its own sidecar'),
]
FIXTURES = ROOT / 'src/pilotdeck-gateway/tests/fixtures'
GUARDED = [
    SRC / 'live.rs',
    SRC / 'runner.rs',
    SRC / 'host_factory.rs',
    SRC / 'live_stream_tests.rs',
    # #2228 DS r2 P1: the gates bindings run through live_stream_tests.rs's
    # #[path] assembly of this module (gate_cases! defines every bound test
    # and inherits via `use super::*`), so a concurrent edit during a run must
    # be caught by the source guard.
    GATES_TESTS,
    RUNNER_TESTS,
    FACTORY_TESTS,
    PEER,
    # #2228 DS r3 P1: gates_tests.rs compiles turn-gates/oracle.json into the
    # binary (include_str! at both the read-back fixture and GATES_ORACLE) and
    # runs turn-gates/peer.py as a python3 subprocess, so concurrent edits to
    # either during a run must be caught by the source guard.
    FIXTURES / 'turn-gates/oracle.json',
    FIXTURES / 'turn-gates/peer.py',
    # #2228 DS r4 P1: the gates-consumer-exists binding reads
    # turn-runner/oracle.json (runner_tests.rs:859 include_str!, and RUNNER_TESTS
    # above is already guarded without its input), so a concurrent edit during a
    # run must be caught by the source guard.
    FIXTURES / 'turn-runner/oracle.json',
    # #2228 MM r6 concurrency P2 (same class as the entry above): RUNNER_TESTS
    # also assembles runner_notice_tests.rs and runner_prompt_tests.rs
    # (`#[path]` at runner_tests.rs:18-21), whose row() helpers bake
    # turn-start-notices/oracle.json and turn-prompt-hooks/oracle.json into
    # the same --lib binary (include_str! at runner_notice_tests.rs:46 and
    # runner_prompt_tests.rs:160), so concurrent edits to the carriers or the
    # baked inputs must be caught by the source guard.
    ROOT / 'src/pilotdeck-gateway/src/runtime/turn/runner_notice_tests.rs',
    ROOT / 'src/pilotdeck-gateway/src/runtime/turn/runner_prompt_tests.rs',
    FIXTURES / 'turn-start-notices/oracle.json',
    FIXTURES / 'turn-prompt-hooks/oracle.json',
    # #2228 zai r4 P1: every bound gates test executes the shared baseline
    # module — gates_tests.rs:36/:421 matrix()/gate_cases! both start with
    # crate::test_baseline::oracle, and runner_tests.rs:17 `use crate::
    # test_baseline as baseline` — the same input the five sibling fixtures
    # (session-domain / host-factory / turn-start-notices / turn-runner-tail /
    # turn-prompt-hooks) already register.
    ROOT / 'src/pilotdeck-gateway/tests/common/baseline.rs',
    # #2228 r6 (MM boundary P1 class, no regeneration binding here): the
    # baseline module is unconditionally compiled with its #[path] submodule
    # tests/common/regeneration_masks.rs, so a concurrent edit to it during a
    # run must be caught by the source guard; the mask table itself stays
    # unregistered because no gates binding runs a regeneration compare over
    # it (the table is a dead input for this fixture).
    ROOT / 'src/pilotdeck-gateway/tests/common/regeneration_masks.rs',
    # #2228 DS r3 P2 / zai r3 P2 (independent hit): the top of the turn chain —
    # runtime/turn/mod.rs declares the live/runner/host_factory modules the
    # bindings resolve through (live.rs:14 `use super::runner::{...}` goes via
    # this file), and runtime/mod.rs declares turn itself, capping the chain at
    # the same declarator depth the project chain already registers.
    ROOT / 'src/pilotdeck-gateway/src/runtime/turn/mod.rs',
    ROOT / 'src/pilotdeck-gateway/src/runtime/mod.rs',
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
