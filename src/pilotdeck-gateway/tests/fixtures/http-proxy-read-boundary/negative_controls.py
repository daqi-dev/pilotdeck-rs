#!/usr/bin/env python3
"""#2499 fix-r5 compiled controls for the native lane's withholding cap.

`tests/fixtures/native-http-basic/` pins one module-level
``PREFIX = 'native_http::tests::'`` for all of its rows, and
`mutant_switch.execute_suite` prepends that prefix to every bound test
identity. #2499 fix-r5's cells live in `src/pilotdeck-gateway/src/http_proxy_tests.rs`
(module ``http_proxy::read_boundary_tests``), so that table cannot reach them —
a row added there would fail its own positive control (libtest runs nothing,
``0 passed``) and never reach a verdict. Following the #2020 precedent
(``tests/fixtures/channel-sms-head-timeout/``, created for the same prefix
mismatch), this module gets its own table instead of touching the frozen
native-http-basic ledger sentence that counts its rows.

Every switch is a compiled `crate::mutants::on(..)` read in
`src/pilotdeck-gateway/src/http_proxy.rs`; the source is never rewritten, so
the executor precompiles once with `--features pdrs-mutants` and runs every
phase in its own process.

`#2499` fix-r5 registers `native-http-withheld-park-no-wake` (the release wake
that re-feeds a head parked at the withholding cap when the response
completes). Killed by the gate-level cell: with the wake skipped the counting
waker never fires, so the cell dies at its release-count assertion. The
end-to-end re-feed cell
(`a_capped_withheld_head_is_re_fed_and_served_once_the_response_completes`)
is deliberately **not** bound: under this mutant hyper's dispatcher can still
incidentally re-poll the parked gate after a body completes (its dispatch loop
swallows a Pending read and may iterate once more), so the end-to-end outcome
is racy (4/10 mutant runs completed all 513 responses on cb8) and cannot serve
as a deterministic witness; the gate-level cell pins the wake itself.
"""
import argparse
import os
from pathlib import Path
import sys
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from _shared import mutant_switch
ROOT = Path(__file__).resolve().parents[5]
FIXTURES = ROOT / 'src/pilotdeck-gateway/tests/fixtures'
PROXY = ROOT / 'src/pilotdeck-gateway/src/http_proxy.rs'
PREFIX = 'http_proxy::read_boundary_tests::'
switches = [
    # #2499 fix-r5 (review sibling#1, the native lane's mirror of the sms
    # release wake): `IdleBody::release` wakes the gate parked at the
    # withholding cap through the release slot, so the parked head re-feeds
    # the moment the response completes — no successor bytes needed (hyper
    # does not re-poll the parked read on its own; the fix-r5 tokio probe
    # showed the socket cannot carry it either). The mutant skips only the
    # wake, so the gate-level cell's counting waker never fires and the cell
    # dies at its release-count assertion. The end-to-end re-feed cell stays
    # unbound on purpose — see the module docstring for the racy reason.
    ('native-http-withheld-park-no-wake', PROXY,
     'a_next_head_parked_at_the_withholding_cap_wakes_at_the_response_release',
     '#2499 fix-r5: the IdleBody release woke the parked gate'),
]
GUARDED = [
    PROXY,
    ROOT / 'src/pilotdeck-gateway/src/http_proxy_tests.rs',
    ROOT / 'src/pilotdeck-gateway/src/mutants.rs',
    ROOT / 'src/pilotdeck-gateway/src/lib.rs',
    ROOT / 'src/pilotdeck-gateway/tests/common/baseline.rs',
    ROOT / 'src/pilotdeck-gateway/Cargo.toml', ROOT / 'src/Cargo.toml', ROOT / 'src/Cargo.lock',
    FIXTURES / '_shared/mutant_switch.py', FIXTURES / 'negative_suite.py', FIXTURES / 'negative_process.py',
    Path(__file__),
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
            prefix=PREFIX, script=Path(__file__), requested=args.names,
            test_timeout=480)
    except (ValueError, OSError) as error:
        print(f'preflight: {error}', file=sys.stderr)
        return 2


if __name__ == '__main__':
    sys.exit(main())
