#!/usr/bin/env python3
"""#996: session-abort-contract controls as compile-time switches.

The #1010 text-replacement negative.py is deleted; each mutation is now a
``crate::mutants::on`` branch compiled into the feature-enabled library test
binary. ``tampered-source-hash`` is renamed ``abort-tampered-source-hash`` (the
session-owner fixture shares the old name for a different oracle): the switch
zeroes the loaded oracle's recorded hash for src/agent/scope/AgentHandle.ts in
memory, so the provenance panic fires before the data is read — the same bytes
the old on-disk mutation wrote. CARGO_TARGET_DIR must be supplied. Use --check
for a read-only anchor preflight, or NEW_OUTPUT_DIR [switch ...].
"""
import argparse
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from _shared import mutant_switch

ROOT = Path(__file__).resolve().parents[5]
HANDLE = ROOT / 'src/pilotdeck-gateway/src/runtime/session/handle.rs'
ABORT_TESTS = ROOT / 'src/pilotdeck-gateway/src/runtime/session/handle_abort_tests.rs'
TEST = ('runtime::session::handle::abort_contract_tests::'
        'abort_at_lease_and_dispose_windows_matches_ts')
# (name, path, test, assertion): the switch site is counted in the target file.
switches = [
    ('suppress-abort-while-draining', HANDLE, TEST,
     'lease/dispose abort trace; active=false'),
    ('disposed-before-lease-release', HANDLE, TEST,
     'lease/dispose abort trace; active=false'),
    ('forward-abort-after-disposed', HANDLE, TEST,
     'lease/dispose abort trace; active=false'),
    ('reverted-cleanup-order', HANDLE, TEST,
     'controlled dispose/lease interleaving hung'),
    ('abort-tampered-source-hash', ABORT_TESTS, TEST,
     'oracle hash for src/agent/scope/AgentHandle.ts does not match'),
]
FIXTURES = ROOT / 'src/pilotdeck-gateway/tests/fixtures'
GUARDED = [
    HANDLE,
    ABORT_TESTS,
    ROOT / 'src/pilotdeck-gateway/src/mutants.rs',
    ROOT / 'src/pilotdeck-gateway/src/lib.rs',
    ROOT / 'src/pilotdeck-gateway/Cargo.toml',
    ROOT / 'src/Cargo.toml',
    ROOT / 'src/Cargo.lock',
    FIXTURES / 'session-abort-contract/oracle.json',
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
