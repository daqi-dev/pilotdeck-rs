#!/usr/bin/env python3
"""#2186: the #2157 MCP resolver lease as a compiled switch mutation.

#2157 added `_lease` to `LeasedMcpProject` so `NativeMcpResolver::resolve`
keeps the generation leased for the SDK MCP state machine, and
`resolved_project_holds_the_generation_lease_through_refresh` claimed in
prose that removing the field makes it red. Two non-author reviews found the
claim was never registered as an executable control, so nothing automatic
stopped the comment and the code from drifting apart. This fixture registers
it: the switch is a compiled `crate::mutants::on` read in
src/pilotdeck-gateway/src/native_process/assembly/mcp.rs; the source is never
rewritten, so the executor precompiles once with `--features pdrs-mutants`
and runs every phase in its own process.

Judged through the #946 nonce receipt protocol (receipts=True): the mutation
only counts as killed on the bound test's named FAILED line plus this run's
assertion receipt, never on prose or a sibling failure. CARGO_TARGET_DIR must
be supplied. Use --check for a read-only anchor preflight, or
NEW_OUTPUT_DIR [switch ...].
"""
import argparse
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from _shared import mutant_switch

ROOT = Path(__file__).resolve().parents[5]
MCP = ROOT / 'src/pilotdeck-gateway/src/native_process/assembly/mcp.rs'
TEST = ('native_process::assembly::mcp::tests::'
        'resolved_project_holds_the_generation_lease_through_refresh')
# (name, path, test, assertion): the switch site is counted in the target file.
# With the switch selected the resolver returns without holding the lease, so
# the generation is disposed before the caller's refresh and the bound
# assertion fails on the drained plugin runtime.
switches = [
    ('mcp-resolver-lease', MCP, TEST,
     '#2157 refresh must run on the generation the resolver leased'),
]
FIXTURES = ROOT / 'src/pilotdeck-gateway/tests/fixtures'
GUARDED = [
    MCP,
    ROOT / 'src/pilotdeck-gateway/src/native_process/assembly.rs',
    # Workers are re-executed with env_clear; the selector forwarding and the
    # receipt recording live here and must not change under a running suite.
    ROOT / 'src/pilotdeck-gateway/tests/common/negative_receipt.rs',
    ROOT / 'src/pilotdeck-gateway/src/test_process.rs',
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
                                 requested=args.names, receipts=True)
    except (ValueError, OSError) as error:
        print(f'preflight: {error}', file=sys.stderr)
        return 2


if __name__ == '__main__':
    sys.exit(main())
