#!/usr/bin/env python3
"""#2042: subagent-references projection controls as compile-time switches.

The Rust port of TS ``projectSubagentReferences`` is replayed against the
``subagentReferences`` rows this fixture records from the pinned baseline, so
each control is a ``crate::mutants::on`` branch compiled into the
feature-enabled library test binary and bound to the assertion that names the
field it corrupts. CARGO_TARGET_DIR must be supplied. Use --check for a
read-only anchor preflight, or NEW_OUTPUT_DIR [switch ...] to run them.
"""
import argparse
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from _shared import mutant_switch

ROOT = Path(__file__).resolve().parents[5]
ADAPTER = ROOT / 'src/pilotdeck-gateway/src/storage/transcript_adapter.rs'
# Every row binds the test that asserts the field the switch corrupts: the row
# list for the row-dropping switches, the JS Map index for the deduplication and
# for the two index-order switches (first-wins started / last-wins completed).
switches = [
    ('subagent-drop-started',
     ADAPTER,
     'storage::transcript_adapter_tests::subagent_started_rows_match_ts',
     '#2042 subagent started rows'),
    ('subagent-started-first-only',
     ADAPTER,
     'storage::transcript_adapter_tests::subagent_duplicate_started_rows_kept',
     '#2042 duplicate started rows'),
    ('subagent-completed-deduped',
     ADAPTER,
     'storage::transcript_adapter_tests::subagent_duplicate_completed_rows_kept',
     '#2042 duplicate completed rows'),
    ('subagent-started-index-last',
     ADAPTER,
     'storage::transcript_adapter_tests::subagent_duplicate_started_rows_kept',
     '#2042 duplicate started index keeps the first entry'),
    ('subagent-completed-index-first',
     ADAPTER,
     'storage::transcript_adapter_tests::subagent_duplicate_completed_rows_kept',
     '#2042 duplicate completed index keeps the last entry'),
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
                                 guarded=GUARDED, script=Path(__file__),
                                 requested=args.names)
    except (ValueError, OSError) as error:
        print(f'preflight: {error}', file=sys.stderr)
        return 2


if __name__ == '__main__':
    sys.exit(main())
