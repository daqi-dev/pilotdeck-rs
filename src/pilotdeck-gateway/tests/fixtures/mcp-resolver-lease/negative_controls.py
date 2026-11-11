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
assertion receipt, never on prose or a sibling failure. The bound assertion is
the deterministic lease_count claim that runs right after the invalidate, ahead
of the 500 ms window and of the refresh, so the verdict cannot drift with the
cleanup task's scheduling. CARGO_TARGET_DIR must be supplied and pass the
shared #1166 target rule (unset, blank, whitespace, dot, filesystem or
repository root, and inside src are refused with rc 2); the admitted target is
normalized to its repo-root-anchored absolute value and written back before
the run, so the shared artifact() judge reads the directory cargo actually
built in. Use --check for a read-only anchor preflight, or NEW_OUTPUT_DIR
[switch ...].
"""
import argparse
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import negative_process
from _shared import mutant_switch

ROOT = Path(__file__).resolve().parents[5]
MCP = ROOT / 'src/pilotdeck-gateway/src/native_process/assembly/mcp.rs'
TEST = ('native_process::assembly::mcp::tests::'
        'resolved_project_holds_the_generation_lease_through_refresh')
# (name, path, test, assertion): the switch site is counted in the target file.
# With the switch selected the resolver returns without holding the lease, so
# the bound assertion — the deterministic lease_count claim right after the
# invalidate, ahead of the 500 ms window and of the refresh — fails on the
# missing lease under either cleanup timing.
switches = [
    ('mcp-resolver-lease', MCP, TEST,
     '#2157 the resolved project must own the generation lease'),
]
FIXTURES = ROOT / 'src/pilotdeck-gateway/tests/fixtures'
GUARDED = [
    MCP,
    # The bound test's judgement semantics live in the lease runtime, the
    # plugin disposal it awaits, and the module chain up to the assembly:
    # concurrent edits to any of them during a run must be caught by the
    # source guard (the host-factory GUARDED precedent).
    ROOT / 'src/pilotdeck-gateway/src/runtime/project.rs',
    ROOT / 'src/pilotdeck-gateway/src/runtime/plugins.rs',
    ROOT / 'src/pilotdeck-gateway/src/runtime/mcp/sdk.rs',
    ROOT / 'src/pilotdeck-gateway/src/native_process.rs',
    # r4 DS sibling P1: the bound test's whole setup is the factory —
    # registry() assembles ProjectResourcesFactory::new(...).with_plugins(...)
    # and the resources it builds feed the lease runtime, so a concurrent
    # edit to factory.rs during a run must be caught by the source guard
    # (the #1274 MM r1 P1 precedent guards it for the same assembly reason).
    ROOT / 'src/pilotdeck-gateway/src/runtime/project/factory.rs',
    # The guarded files' declarators and remaining judgement-chain members:
    # runtime/mod.rs declares mcp / plugins / project, runtime/mcp/mod.rs
    # declares sdk, project/plugins.rs is the PluginResources the awaited
    # disposal resolves through, and cleanup_task.rs hosts the wait_disposed
    # completion semantics, capping the chain at the declarator depth the
    # host-factory precedent registers.
    ROOT / 'src/pilotdeck-gateway/src/runtime/mod.rs',
    ROOT / 'src/pilotdeck-gateway/src/runtime/mcp/mod.rs',
    ROOT / 'src/pilotdeck-gateway/src/runtime/project/plugins.rs',
    ROOT / 'src/pilotdeck-gateway/src/runtime/cleanup_task.rs',
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
        # Shared #1166 target rule, checked before the evidence directory is
        # claimed: dot / filesystem root / repository root / inside-src (and
        # whitespace-only) targets are refused with rc 2. The shared executor
        # itself never re-validates the target directory, and its artifact()
        # judge resolves CARGO_TARGET_DIR against this process's cwd while
        # cargo resolves it against the repo root - so the normalized absolute
        # value is written back before the run, keeping admission and judging
        # on one target instead of handing the raw relative string on.
        try:
            os.environ['CARGO_TARGET_DIR'] = negative_process.target_environment(
                ROOT, os.environ)['CARGO_TARGET_DIR']
        except RuntimeError as error:
            raise ValueError(str(error)) from error
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
