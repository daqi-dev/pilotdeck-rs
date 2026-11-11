#!/usr/bin/env python3
"""#2044: web-history host-view projection controls as compile-time switches.

The Rust port of TS ``projectWebHistory`` is replayed against the
``webHistory`` rows this fixture records from the pinned baseline, so each
control is a ``crate::mutants::on`` branch compiled into the feature-enabled
library test binary and bound to the assertion that names the field it
corrupts. CARGO_TARGET_DIR must be supplied. Use --check for a read-only
anchor preflight, or NEW_OUTPUT_DIR [switch ...] to run them.
"""
import argparse
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from _shared import mutant_switch

ROOT = Path(__file__).resolve().parents[5]
ADAPTER = ROOT / 'src/pilotdeck-gateway/src/storage/transcript_adapter.rs'
# Every row binds the test that asserts the field the switch corrupts: the
# fileArtifact rows for the two finalize-filter switches and the non-array
# rejection, the turnErrors rows for the visible-failure filter, the
# tokenUsage object for the three latest-* budget switches, and the
# incompleteTurnIds list for the completed-turn filter.
switches = [
    ('web-history-tool-turn-ignored',
     ADAPTER,
     'storage::transcript_adapter_tests::web_history_tool_turn_keeps_diff',
     '#2044 tool-result turns keep their workspace_diff artifacts'),
    ('web-history-workspace-diff-kept',
     ADAPTER,
     'storage::transcript_adapter_tests::web_history_workspace_diff_filtered',
     '#2044 non-tool turns drop their workspace_diff artifacts'),
    ('web-history-finalize-nonarray-empty',
     ADAPTER,
     'storage::transcript_adapter_tests::web_history_finalize_rejects_nonarray',
     '#2044 non-array artifacts reject in finalize'),
    ('web-history-visible-error-kept',
     ADAPTER,
     'storage::transcript_adapter_tests::web_history_visible_failures_filtered',
     '#2044 turns with a visible failure status leave the error rows'),
    ('web-history-context-budget-dropped',
     ADAPTER,
     'storage::transcript_adapter_tests::web_history_context_budget_matches_ts',
     '#2044 the latest context budget is exposed'),
    ('web-history-compact-budget-dropped',
     ADAPTER,
     'storage::transcript_adapter_tests::web_history_compact_budget_matches_ts',
     '#2044 the latest compact budget is exposed'),
    ('web-history-turn-usage-dropped',
     ADAPTER,
     'storage::transcript_adapter_tests::web_history_turn_usage_matches_ts',
     '#2044 the latest turn usage is exposed'),
    ('web-history-incomplete-turn-kept',
     ADAPTER,
     'storage::transcript_adapter_tests::web_history_incomplete_turns_filtered',
     '#2044 completed turns leave the incomplete list'),
]
FIXTURES = ROOT / 'src/pilotdeck-gateway/tests/fixtures'
GUARDED = [
    ADAPTER,
    ROOT / 'src/pilotdeck-gateway/src/storage/transcript_adapter_tests.rs',
    ROOT / 'src/pilotdeck-gateway/src/storage/mod.rs',
    ROOT / 'src/pilotdeck-gateway/src/mutants.rs',
    ROOT / 'src/pilotdeck-gateway/src/lib.rs',
    ROOT / 'src/pilotdeck-gateway/Cargo.toml',
    ROOT / 'src/Cargo.toml',
    ROOT / 'src/Cargo.lock',
    ROOT / 'src/pilotdeck-gateway/tests/common/baseline.rs',
    ROOT / 'src/pilotdeck-gateway/tests/fixtures/session-factory/oracle.json',
    ROOT / 'src/pilotdeck-gateway/tests/fixtures/session-factory/projections.mts',
    ROOT / 'src/pilotdeck-gateway/tests/fixtures/session-factory/sources.json',
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
                                 guarded=GUARDED, script=Path(__file__),
                                 requested=args.names)
    except (ValueError, OSError) as error:
        print(f'preflight: {error}', file=sys.stderr)
        return 2


if __name__ == '__main__':
    sys.exit(main())
