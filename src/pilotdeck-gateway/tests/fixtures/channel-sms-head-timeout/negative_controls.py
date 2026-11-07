#!/usr/bin/env python3
"""#2020 compiled controls for the sms listener's head-deadline module.

`tests/fixtures/channel-sms/` pins one module-level
``PREFIX = 'channel::sms::tests::'`` for all of its rows, and
`mutant_switch.execute_suite` prepends that prefix to every bound test
identity. The cases live in
src/pilotdeck-gateway/src/channel/sms/head_timeout_tests.rs (module
``channel::sms::head_timeout_tests``), so that table cannot reach them — a row
added there would fail its own positive control (libtest runs nothing,
``0 passed``) and never reach a verdict. #2020 therefore gives this module its
own table instead of rewriting the 71 existing rows and the frozen ledger
sentence that counts them.

Both switches are compiled `crate::mutants::on(..)` reads in
`src/pilotdeck-gateway/src/channel/sms/mod.rs`; the source is never rewritten,
so the executor precompiles once with `--features pdrs-mutants` and runs every
phase in its own process.

`#2020` registers `sms-header-read-no-timeout` (#1955's request-head deadline,
killed by the half-head case) and `sms-stop-waits-for-connections` (#2035's
detach semantics, killed by the paired `stop()` case). The second switch is
test-only bookkeeping: `SmsHandle::stop()` drains the detached connections
under the mutation, and the paired case must then miss its 1 s bound.
"""
import argparse
import os
from pathlib import Path
import sys
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from _shared import mutant_switch
ROOT = Path(__file__).resolve().parents[5]
FIXTURES = ROOT / 'src/pilotdeck-gateway/tests/fixtures'
SMS = ROOT / 'src/pilotdeck-gateway/src/channel/sms'
PREFIX = 'channel::sms::head_timeout_tests::'
switches = [
    # #1955 / #2020: the head deadline is not installed (no hyper `Timer` +
    # `header_read_timeout`), so a half-sent request head keeps its connection
    # task and socket past the injected 300 ms deadline. Killed by the case
    # that requires the server-side EOF inside the bracket.
    ('sms-header-read-no-timeout', SMS / 'mod.rs',
     'half_sent_head_is_closed_at_the_deadline_and_leaves_no_task',
     '#1955 the server retired the stalled head at the deadline'),
    # #2035: `stop()` waits for every detached connection task before clearing
    # the client, i.e. the pre-fix shape of the TS `server.close()` drain.
    # Killed by the paired case, which requires `stop()` to return inside 1 s
    # while a connection with a 30 s head deadline is still open.
    ('sms-stop-waits-for-connections', SMS / 'mod.rs',
     'stop_returns_before_the_detached_connection_ends',
     '#2035 stop() must return while the detached connection still runs'),
]
GUARDED = [
    SMS / 'mod.rs',
    SMS / 'head_timeout_tests.rs',
    ROOT / 'src/pilotdeck-gateway/src/channel/mod.rs',
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
