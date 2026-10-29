#!/usr/bin/env python3
"""#1036 P2-3: the dream-tail per-call-locks regression as a compile-time switch.

The mutation is a ``crate::mutants::on`` branch in the production orchestration
tail (feature ``pdrs-mutants`` + ``PDRS_MUTANT``), so this control needs no
source replacement: the shared precompiled executor builds the feature-enabled
library test binary once, then runs the bound regression per switch row —
positive without a selector, negative with the switch selected, restored in a
fresh process — and reconciles the executed rows with this statically audited
table (#996 denominator check). The kill must land on the deterministic
lock-held proof: with per-call scopes both root locks are free while the first
dream parks, so the test thread grabs the workspace one and fails at the
dedicated ``try_lock`` panic.

Usage: CARGO_TARGET_DIR=/abs/target python3 switch_negative.py NEW_OUTPUT_DIR [switch ...]
"""
import argparse
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from _shared import mutant_switch

ROOT = Path(__file__).resolve().parents[5]
FIXTURES = ROOT / 'src/pilotdeck-gateway/tests/fixtures'
SWITCH_SOURCE = ROOT / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/service/orchestration.rs'
TEST_SOURCE = ROOT / 'src/pilotdeck-gateway/tests/common/dream_tail_race.rs'
# (name, path, test, assertion): the switch site is counted in the target file.
# The workspace lock is checked first in the proof loop, so a per-call-scope
# regression always fails on this exact panic.
switches = [
    ('dream-tail-per-call-locks', SWITCH_SOURCE,
     'dream_tail_race::dream_tail_cannot_interleave_across_threads',
     '#1036 tail race: the workspace root operation lock was free while '
     'the first dream was parked between install and replace'),
]
GUARDED = [
    SWITCH_SOURCE,
    TEST_SOURCE,
    ROOT / 'src/pilotdeck-gateway/src/mutants.rs',
    ROOT / 'src/pilotdeck-gateway/src/lib.rs',
    ROOT / 'src/pilotdeck-gateway/Cargo.toml',
    ROOT / 'src/Cargo.toml',
    ROOT / 'src/Cargo.lock',
    ROOT / 'src/pilotdeck-gateway/tests/common/baseline.rs',
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
