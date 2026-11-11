#!/usr/bin/env python3
"""#996: the session-owner registered EQUIVALENT row as a compile-time switch.

The #1365 fix-r1 row ``ticket-release-write-removed`` deletes the serial
ticket's release write, which no product observable can witness: the sender is
dropped at the end of ``Ticket::drop`` either way, and ``Ticket::enter``'s
``wait_for`` treats ``Ok(true)`` and the closed channel's ``Err(RecvError)``
identically, while the ``ticket-released`` probe records unconditionally after
the deletion site (measured SURVIVED in the review round and the fix-r2 full
run — see the old negative_close_shutdown.py EQUIVALENT registry this replaces).
The switch stays registered so the anchor preflight still counts it, and this
runner re-measures the equivalence: the mutant must compile, leave its bound
test green (SURVIVED) and restore cleanly. A SURVIVED row is the green
outcome here — unlike the must-kill schemata, ``mutant_switch.run`` exits
nonzero for it, so this entry re-reads the published summary and maps exactly
one SURVIVED row to exit 0. If the test ever grows a real witness, the row
reports KILLED, the run fails, and the row belongs back in the must-kill set
(schemata_close_shutdown.py). Use --check for a read-only anchor preflight,
or NEW_OUTPUT_DIR.
"""
import argparse
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from _negkit import non_head
from _shared import mutant_switch

ROOT = Path(__file__).resolve().parents[5]
OWNER = ROOT / 'src/pilotdeck-gateway/src/runtime/session/owner.rs'
QUEUED_DRAIN = ('runtime::session::owner::tests::'
                'queued_close_drains_through_the_ticket_release_write')
# (name, path, test, assertion): the switch site is counted in the target file.
switches = [
    ('ticket-release-write-removed', OWNER, QUEUED_DRAIN,
     "#1365 the queued close must drain after the creation's ticket release "
     'write'),
]
FIXTURES = ROOT / 'src/pilotdeck-gateway/tests/fixtures'
GUARDED = [
    OWNER,
    # The one row binds a runtime::session::owner::tests test defined in
    # owner_tests.rs; guard the bound test source like schemata.py does.
    ROOT / 'src/pilotdeck-gateway/src/runtime/session/owner_tests.rs',
    ROOT / 'src/pilotdeck-gateway/src/mutants.rs',
    ROOT / 'src/pilotdeck-gateway/src/lib.rs',
    ROOT / 'src/pilotdeck-gateway/Cargo.toml',
    ROOT / 'src/Cargo.toml',
    ROOT / 'src/Cargo.lock',
    FIXTURES / 'session-owner/oracle.json',
    FIXTURES / '_shared/mutant_switch.py',
    FIXTURES / '_negkit/non_head.py',
    FIXTURES / 'negative_suite.py',
    FIXTURES / 'negative_process.py', FIXTURES / 'anchor_selfcheck.py',
    Path(__file__).resolve(),
]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--check', action='store_true')
    parser.add_argument('output', nargs='?')
    args = parser.parse_args()
    try:
        rows = [dict(zip(('name', 'path', 'test', 'anchor'), row)) for row in switches]
        chosen = mutant_switch.selected(rows, [])
        mutant_switch.preflight(rows)
        if args.check:
            if args.output:
                raise ValueError('--check does not take output')
            print(f'{len(rows)} switch anchors and bindings checked')
            return 0
        if not args.output or not os.environ.get('CARGO_TARGET_DIR'):
            raise ValueError('NEW_OUTPUT_DIR and caller CARGO_TARGET_DIR are required')
        output = Path(args.output).resolve()
        # #1540: the compile-time-switch executor always runs the working
        # tree, so a pinned PDRS_NEGATIVE_BASELINE cannot be honoured: the
        # request is announced as ignored and a green run is still downgraded
        # (exit 42) — it cannot certify the caller-named tree, which is the
        # mistake exit 0 invited (network-timing precedent).
        baseline_source, _ignored_ref, baseline_sha = non_head.gate(
            ROOT, ignored=True)
        output.mkdir(parents=True, exist_ok=False)
        mutant_switch.run(ROOT, output, chosen,
                          mutant_switch.clean_environment(os.environ),
                          guarded=GUARDED, prefix='', script=Path(__file__))
        # The shared executor demands KILLED rows, so its exit code is
        # expected nonzero for a surviving equivalent; the published summary
        # is the verdict, not that code.
        summary = json.loads((output / 'summary.json').read_text())
        results = summary['controls']
        verdict = 1
        if len(results) != 1:
            print(f'equivalent summary: expected one row, got {len(results)}',
                  file=sys.stderr)
        else:
            row = results[0]
            if row['status'] == 'SURVIVED':
                print('equivalent summary: SURVIVED=1, INVALID=0 '
                      '(registered equivalent survives)')
                verdict = 0
            else:
                print(f"equivalent summary: {row['status']}: {row['reason']}",
                      file=sys.stderr)
                if row['status'] == 'KILLED':
                    print('registered equivalent is now killed by its bound '
                          'test; promote it to the must-kill set '
                          '(schemata_close_shutdown.py)', file=sys.stderr)
        return non_head.non_head_exit(baseline_source, baseline_sha,
                                      code=verdict, evidence=output,
                                      ignored=True)
    except (ValueError, OSError) as error:
        print(f'preflight: {error}', file=sys.stderr)
        return 2


if __name__ == '__main__':
    sys.exit(main())
