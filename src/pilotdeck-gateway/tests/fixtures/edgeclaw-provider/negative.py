#!/usr/bin/env python3
"""#777 Provider controls: feature-compiled switches, no source rewriting.

Uses the main #988/#996 executor. It compiles every branch in one binary,
runs each switch with its exact test and assertion, and restores the selector
for a positive replay. This does not use or bypass mutating_sources: it never
edits product files. Working-tree hashes are recorded as such, not as HEAD.
"""
import argparse
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from _shared import mutant_switch

ROOT = Path(__file__).resolve().parents[5]
SOURCE = ROOT / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/provider.rs'
TESTS = ROOT / 'src/pilotdeck-gateway/tests/edgeclaw_provider.rs'
FIXTURES = ROOT / 'src/pilotdeck-gateway/tests/fixtures'
PREFIX = 'runtime::memory::edgeclaw::provider::'
switches = [
    ('provider-oracle-count', TESTS, 'tests::oracle_scenario_count', '#777 oracle scenario count'),
    ('provider-missing-cell', TESTS, 'tests::oracle_scenario_count', '#777 Rust cell registration count'),
    ('provider-wrong-cell', TESTS, 'tests::oracle_scenario_count', '#777 Rust cell registrations match oracle'),
    ('provider-sync-capture', SOURCE, 'sync_turn_tests::sync_capture_defers_completion', '#777 capture must finish before retrieval continuation'),
    ('provider-nonreentrant', SOURCE, 'sync_turn_tests::capture_callback_can_reenter_provider', '#777 same-thread capture callback can reenter provider'),
    ('provider-lazy-service', SOURCE, 'tests::native_service_starts_before_immediate_close', '#777 real LLM skipped'),
    ('provider-context-error', SOURCE, 'tests::malformed_context', '#777 provider cell malformed-context'),
    ('provider-capture-preview-swallow', SOURCE, 'tests::malformed_capture', '#777 provider cell malformed-capture'),
    ('provider-save-error-propagates', SOURCE, 'tests::pending_save_error', '#777 capture must swallow service errors'),
    ('provider-missing-save-drops', SOURCE, 'tests::pending_save_missing', '#777 provider cell pending-save-missing'),
    ('provider-keep-pending', SOURCE, 'tests::pending_capture', '#777 provider cell pending-capture'),
    ('provider-errored-status', SOURCE, 'tests::pending_errored', '#777 provider cell pending-errored'),
    ('provider-empty-diagnostic', SOURCE, 'tests::none_empty', '#777 provider cell none-empty'),
    ('provider-capture-error-skips-trace', SOURCE, 'tests::pending_capture_error', '#777 provider cell pending-capture-error'),
    ('provider-drop-late', SOURCE, 'tests::direct_timeout_keeps_retrieval_alive', '#777 direct builder late trace'),
    ('provider-owned-continuation', SOURCE, 'tests::direct_abort_keeps_retrieval_alive', '#777 retrieval remains alive after timeout/abort'),
    ('provider-reject-clears', SOURCE, 'tests::pending_reject', '#777 provider cell pending-reject'),
    ('provider-first-write', SOURCE, 'tests::none_overwrite', '#777 provider cell none-overwrite'),
    ('provider-empty-fallback', SOURCE, 'tests::empty_precedence', '#777 provider cell empty-precedence'),
    ('provider-capture-fork', SOURCE, 'tests::none_capture', '#777 provider cell none-capture'),
    ('provider-workspace', SOURCE, 'tests::none_resolve', '#777 provider cell none-resolve'),
    ('provider-signal', SOURCE, 'tests::direct_pre_aborted', '#777 provider cell direct-pre-aborted'),
    ('provider-telemetry', SOURCE, 'tests::none_resolve', '#777 provider cell none-resolve'),
]
GUARDED = [SOURCE,
    ROOT / 'src/pilotdeck-gateway/src/runtime/context/memory.rs',
    ROOT / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/service/mod.rs',
    ROOT / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/llm/recall.rs',
    TESTS,
    ROOT / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/mod.rs',
    ROOT / 'src/pilotdeck-gateway/src/lib.rs',
    ROOT / 'src/pilotdeck-gateway/tests/common/baseline.rs',
    FIXTURES / '_shared/mutant_switch.py',
    FIXTURES / 'negative_suite.py',
    FIXTURES / 'negative_process.py',
    ROOT / 'src/pilotdeck-gateway/src/mutants.rs',
    ROOT / 'src/pilotdeck-gateway/Cargo.toml', ROOT / 'src/Cargo.toml', ROOT / 'src/Cargo.lock',
    Path(__file__).with_name('oracle.json'), Path(__file__),
    Path(__file__).with_name('generate.mts'),
    Path(__file__).with_name('observe.mts'),
    Path(__file__).with_name('verify.mts'),
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
            mutant_switch.clean_environment(os.environ), guarded=GUARDED,
            prefix=PREFIX, script=Path(__file__), requested=args.names)
    except (ValueError, OSError) as error:
        print(f'preflight: {error}', file=sys.stderr)
        return 2


if __name__ == '__main__':
    sys.exit(main())
