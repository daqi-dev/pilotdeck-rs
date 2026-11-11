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
killed by the half-head case). `#2026` (decision C) replaced #2035's
`sms-stop-waits-for-connections` — its detach semantics is now the *mutation*,
not the production shape — with `sms-stop-no-drain` (the pre-#2026 detach
shape, killed by the drain case's during-drain client assertion) and
`sms-stop-drain-unbounded` (the TS unbounded-block shape, killed by the
`#[ignore]`d bound case; the executor's `--include-ignored` runs it, one phase
costs up to ~80 s of the 480 s budget).

`#2257` registers `sms-stop-drain-on-sender-drop` (the closed-channel path
drains unobserved and holds the port, killed by both port-rebind cases) and
`#2269` `sms-stop-drain-predrain-count` (the r1 pre-drain snapshot the
truncation warn used to count with, killed by the bound case's warn count).
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
    # #2026 (decision C): the drain is skipped, i.e. the pre-#2026 detach
    # shape — stop() returns while the half-head connection still lives and
    # the client is cleared immediately. The branch forgets the JoinSet, so
    # its drop never aborts the connections: they stay detached, like the
    # pre-#2026 bare spawns. Killed by the drain case's
    # during-drain client assertion (the first statement that can observe the
    # early return: a stop that came back cleared the client before the
    # 100 ms probe).
    ('sms-stop-no-drain', SMS / 'mod.rs',
     'stop_drains_a_half_sent_header_once_the_header_timeout_fires',
     '#2026 the Twilio client stays usable while stop() drains'),
    # #2026 (decision C): the drain has no bound, i.e. the TS shape — Node's
    # `server.close()` blocks until the client leaves. Killed by the bound
    # case, which requires stop() to return while the connection (90 s head
    # deadline, longer than the 60 s bound) is still open.
    ('sms-stop-drain-unbounded', SMS / 'mod.rs',
     'stop_returns_at_the_drain_bound_when_a_connection_outlives_it',
     '#2026 the drain bound must return stop() while the connection outlives it'),
    # #2257: the closed-channel path (every watch sender gone — a stop task
    # that panicked at its first log line, or a handle dropped without stop)
    # drains unobserved and holds the listener's port for the whole bound.
    # Killed by both port-rebind cases: with the drain restored the rebind
    # cannot land inside the 2 s bound while the stalled connection (90 s
    # head deadline) outlives the 60 s one.
    ('sms-stop-drain-on-sender-drop', SMS / 'mod.rs',
     ['a_stop_task_that_panics_at_its_log_line_frees_the_port_without_a_drain',
      'a_dropped_handle_frees_the_port_without_a_drain'],
     ['did not rebind inside', 'did not rebind inside']),
    # #2269: the r1 counting shape — the truncation warn reads a pre-drain
    # snapshot, so the connection that retired during the drain is still
    # counted as aborted. Killed by the bound case's warn-count assertion
    # (3 where the aborted set holds 2).
    ('sms-stop-drain-predrain-count', SMS / 'mod.rs',
     'stop_returns_at_the_drain_bound_when_a_connection_outlives_it',
     '#2026 the truncation warn counts only the connections still in the'),
]
GUARDED = [
    SMS / 'mod.rs',
    SMS / 'head_timeout_tests.rs',
    ROOT / 'src/pilotdeck-gateway/src/channel/mod.rs',
    ROOT / 'src/pilotdeck-gateway/src/mutants.rs',
    ROOT / 'src/pilotdeck-gateway/src/lib.rs',
    ROOT / 'src/pilotdeck-gateway/tests/common/baseline.rs',
    ROOT / 'src/pilotdeck-gateway/Cargo.toml', ROOT / 'src/Cargo.toml', ROOT / 'src/Cargo.lock',
    FIXTURES / '_shared/mutant_switch.py', FIXTURES / 'negative_suite.py', FIXTURES / 'negative_process.py', FIXTURES / 'anchor_selfcheck.py',
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
