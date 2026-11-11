#!/usr/bin/env python3
"""#996: host-factory lease-wait controls as compile-time switches.

The #842 text-replacement negative.py is deleted; each mutation is now a
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
TESTS = ROOT / 'src/pilotdeck-gateway/src/runtime/turn/host_factory_tests.rs'
PREFIX = 'runtime::turn::host_factory::tests::'
# (name, path, test, assertion): the switch site is counted in the target file.
switches = [
    ('old-instantaneous-assertion', TESTS,
     'lease_wait_observes_delayed_host_cleanup',
     '#996 lease must already be released at stream end'),
    ('skip-observation', TESTS,
     'lease_wait_observes_delayed_host_cleanup', 'lease wait returned before cleanup'),
    ('late-deadline', TESTS,
     'lease_wait_times_out_while_host_cleanup_is_held',
     'held cleanup must reach the 10-second deadline'),
]
FIXTURES = ROOT / 'src/pilotdeck-gateway/tests/fixtures'
GUARDED = [
    TESTS,
    # #2228 DS r2 P1: the bindings inherit via `use super::*` from
    # host_factory.rs, which assembles host_factory_tests.rs itself
    # (`#[path]` at the `mod tests` boundary), and every bound lease-wait
    # test pulls the paused-cleanup peer through PausedHostCleanup —
    # concurrent edits to either during a run must be caught by the source
    # guard.
    ROOT / 'src/pilotdeck-gateway/src/runtime/turn/host_factory.rs',
    # #2228 r9 (MM r8 sibling P2 + DS r8, both families): the registered
    # carrier host_factory_tests.rs bakes three oracles into the same --lib
    # binary — host-factory/oracle.json (:463/:562/:614, this fixture's own),
    # tool-budget-cwd/oracle.json (:803) and host-manifest/oracle.json
    # (:1495) — so concurrent edits to any of them during a run must be
    # caught by the source guard.
    FIXTURES / 'host-factory/oracle.json',
    FIXTURES / 'tool-budget-cwd/oracle.json',
    FIXTURES / 'host-manifest/oracle.json',
    # #2228 DS r3 P2 / zai r3 P2 (independent hit): the top of the turn chain —
    # runtime/turn/mod.rs declares the host_factory module the bindings resolve
    # through, and runtime/mod.rs declares turn itself, capping the chain at
    # the same declarator depth the project chain registers elsewhere.
    ROOT / 'src/pilotdeck-gateway/src/runtime/turn/mod.rs',
    ROOT / 'src/pilotdeck-gateway/src/runtime/mod.rs',
    FIXTURES / 'host-factory/peer.py',
    ROOT / 'src/pilotdeck-gateway/src/mutants.rs',
    ROOT / 'src/pilotdeck-gateway/src/lib.rs',
    ROOT / 'src/pilotdeck-gateway/Cargo.toml',
    ROOT / 'src/Cargo.toml',
    ROOT / 'src/Cargo.lock',
    ROOT / 'src/pilotdeck-gateway/tests/common/baseline.rs',
    # #2228 r6 (MM boundary P1 class, no regeneration binding here): the
    # baseline module is unconditionally compiled with its #[path] submodule
    # tests/common/regeneration_masks.rs, so a concurrent edit to it during a
    # run must be caught by the source guard; the mask table itself stays
    # unregistered because no factory binding runs a regeneration compare
    # over it (the table is a dead input for this fixture).
    ROOT / 'src/pilotdeck-gateway/tests/common/regeneration_masks.rs',
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
                                 guarded=GUARDED, prefix=PREFIX,
                                 script=Path(__file__), requested=args.names)
    except (ValueError, OSError) as error:
        print(f'preflight: {error}', file=sys.stderr)
        return 2


if __name__ == '__main__':
    sys.exit(main())
