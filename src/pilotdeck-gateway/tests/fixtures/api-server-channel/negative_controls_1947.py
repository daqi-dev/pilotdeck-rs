#!/usr/bin/env python3
"""#1947 compiled controls: compile every switch once, then run the selected
positive, negative and restored cases with negative_suite verdicts. No source
rewriting, no alternate HEAD and no second target directory. The lane's older
controls (negative_controls.py) predate the switch style and require a
HEAD-pristine tree through `mutating_sources`, which a pre-commit batch cannot
provide; this file holds the boundary fail-closed switch, the same shape the
http-store lane's negative_controls_1947.py established. Run one sample in
the authoring session; the coordinator runs the complete list after the patch
is committed.
"""
import argparse
import os
from pathlib import Path
import sys
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from _shared import mutant_switch
ROOT = Path(__file__).resolve().parents[5]
FIXTURES = ROOT / 'src/pilotdeck-gateway/tests/fixtures'
SERVER_DIR = ROOT / 'src/pilotdeck-gateway/src/channel/api_server'
PREFIX = 'channel::api_server::tests::'
switches = [
    # #1947: the fail-closed id table is disarmed — an undeclared boundary id
    # whose request body reaches the 1 MiB cap goes back to being skipped
    # silently (the old `_ => return`), so the unregistered-cell assertion
    # turns green under mutation and the direct test fails at its anchor.
    ('api-server-oracle-boundary-unregistered', SERVER_DIR / 'tests.rs',
     'recorded_boundary_body_length_rejects_unregistered_cell',
     '#1333 #1947 rejects an undeclared boundary cell'),
]
GUARDED = [SERVER_DIR / 'tests.rs', SERVER_DIR / 'mod.rs', SERVER_DIR / 'server.rs',
    SERVER_DIR / 'render.rs', SERVER_DIR / 'error_body.rs',
    # #1615: the visible-failure event table lives in the shared agentStatus
    # port (the old table mutates this file as `visible-failure-name`), so its
    # pristine guard covers it here too (Grok r1 sibling#2).
    ROOT / 'src/pilotdeck-gateway/src/channel/agent_status.rs',
    # DS r2 sibling#6: this lane's transport closure actually runs through
    # the native parser + proxy (server.rs:118), and the shared module
    # declarations (the webhook lane's口径) — the receipts cannot prove which
    # transport bytes they ran against without pinning them.
    ROOT / 'src/pilotdeck-gateway/src/native_http.rs',
    ROOT / 'src/pilotdeck-gateway/src/http_proxy.rs',
    ROOT / 'src/pilotdeck-gateway/src/channel/mod.rs',
    # DS r3 simplify#2: `api_server/mod.rs:14` re-exports `super::protocol::*`,
    # so the shared request/response protocol bytes are inside this lane's
    # closure — the webhook table pins the same file.
    ROOT / 'src/pilotdeck-gateway/src/channel/protocol.rs',
    ROOT / 'src/pilotdeck-gateway/src/mutants.rs',
    ROOT / 'src/pilotdeck-gateway/src/lib.rs',
    ROOT / 'src/pilotdeck-gateway/tests/common/baseline.rs',
    ROOT / 'src/pilotdeck-gateway/tests/common/negative_receipt.rs',
    ROOT / 'src/pilotdeck-gateway/Cargo.toml', ROOT / 'src/Cargo.toml', ROOT / 'src/Cargo.lock',
    FIXTURES / '_shared/mutant_switch.py', FIXTURES / 'negative_suite.py', FIXTURES / 'negative_process.py',
    FIXTURES / 'negative_panic.py', FIXTURES / 'anchor_selfcheck.py',
    Path(__file__), Path(__file__).with_name('oracle.json'),
    Path(__file__).with_name('generate.mts'), Path(__file__).with_name('README.md'),
    Path(__file__).with_name('negative_controls.py'),
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
            # DS r2 boundary#1: an empty table must fail closed, never print
            # "0 anchors: ok" — that shape was ruled a P0 in #1364.
            if not rows:
                raise ValueError('empty switch table')
            print(f'{len(rows)} switch anchors and bindings checked')
            return 0
        if not args.output or not os.environ.get('CARGO_TARGET_DIR'):
            raise ValueError('NEW_OUTPUT_DIR and caller CARGO_TARGET_DIR are required')
        output = Path(args.output).resolve()
        output.mkdir(parents=True, exist_ok=False)
        return mutant_switch.run(ROOT, output, chosen,
            mutant_switch.clean_environment(os.environ), guarded=GUARDED,
            prefix=PREFIX, script=Path(__file__), requested=args.names,
            test_timeout=300)
    except (ValueError, OSError) as error:
        print(f'preflight: {error}', file=sys.stderr)
        return 2

if __name__ == '__main__':
    sys.exit(main())
