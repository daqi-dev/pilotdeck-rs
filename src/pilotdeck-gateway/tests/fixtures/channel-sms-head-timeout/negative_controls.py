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

Every switch is a compiled `crate::mutants::on(..)` read in
`src/pilotdeck-gateway/src/channel/sms/mod.rs`; the source is never rewritten,
so the executor precompiles once with `--features pdrs-mutants` and runs every
phase in its own process.

`#2020` registers `sms-header-read-no-timeout` (#1955's request-head deadline,
killed by the half-head case; #2499 rewired its switch site from hyper's
builder arm to the read gate). `#2499` registers the five gate switches:
`sms-headers-408-bytes-off` (the 51-byte Node 408 on the socket),
`sms-headers-grid-off` (the strictly-after connections-checker landing),
`sms-headers-first-byte-anchor-off` (the first-byte re-anchor),
`sms-headers-idle-followup-408` (the zero-byte follow-up head's silent close,
the registered residual) and `sms-headers-stop-no-disarm` (the stop watch
disarming a draining connection's deadline). `#2499` fix-r1 adds two more:
`sms-headers-grid-inclusive` (an on-grid deadline holding for its successor
grid point) and `sms-headers-chunked-body-withheld` (a chunked request body
staying "open" instead of self-locking the handler). `#2499` fix-r3 widens
three of those rows into all-cells list bindings for the two same-read
swallowed-head cells (residual 3's expansion to the shape where the follow-up
head's bytes never re-cross the gate's accounting): `sms-header-read-no-timeout`
(the close never comes), `sms-headers-grid-off` (the close lands at the
deadline itself) and `sms-headers-idle-followup-408` (the close writes the 408
instead of staying byte-silent). `#2026` (decision C) replaced
#2035's
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
`#2499` fix-r5 registers `sms-headers-withheld-park-no-wake` (the release
wake that re-feeds a head parked at the withholding cap when the response
completes — the fix-r4 empty-buffer poll was empirically inert on a
production TcpStream, per the fix-r5 tokio probe). Killed by both fix-r5
cells: the gate-level cell's counting waker never fires, and the end-to-end
cell's second read — the first withheld head's response — never arrives (the
parked request's own 404 still does; it never needed the re-feed). The
never-completing-response 408 cell is not bound to *this* switch (the wake
is not its discriminator) — it is bound to the three deadline switches its
own doc names (`sms-headers-grid-off`: the mutant's 408 at the +15 s
deadline itself fails the +29.46 s still-open probe;
`sms-headers-408-bytes-off`: the landing answers with the silent close the
EOF probe reads; `sms-header-read-no-timeout`: with the deadline gone the
408 never comes at all), per the fix-r5 review's apparatus-vs-doc drift
finding on the fix-r4 draft's unbacked sensitivity claims.
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
    # #1955 / #2020: the head deadline is not installed, so a half-sent
    # request head keeps its connection task and socket past the injected
    # 300 ms deadline. Killed by the case that requires the server-side EOF
    # inside the bracket. #2499 rewired the switch from hyper's
    # `.timer(..).header_read_timeout(..)` builder arm to the read gate's
    # own deadline (`serve_sms_connection` reads it as `None`); the kill
    # shape is unchanged — the close never comes and the read times out.
    # #2499 fix-r3 adds the two same-read swallowed-head cells (the
    # registered residual 3's fix-r3 expansion): without the deadline the
    # standby window never arms either, so their EOF probes read a window
    # budget instead of the close.
    ('sms-header-read-no-timeout', SMS / 'mod.rs',
     ['half_sent_head_is_closed_at_the_deadline_and_leaves_no_task',
      'a_follow_up_head_swallowed_in_the_same_read_as_the_previous_request_closes_on_the_standby_landing',
      'a_follow_up_head_swallowed_with_a_read_ahead_body_segment_closes_on_the_standby_landing',
      'a_capped_withheld_head_behind_a_never_completing_response_gets_its_408_at_the_landing'],
     ['#1955 the server retired the stalled head at the deadline',
      'swallowed in the same read as the previous request: the idle keep-alive '
      'connection was never closed',
      'swallowed with a read-ahead body segment: the idle keep-alive '
      'connection was never closed',
      'next head parked at the withholding cap: the 51-byte 408 never '
      'arrived']),
    # #2499: the expiry answers with the pre-#2499 zero-byte close instead of
    # Node's 51-byte 408. Killed by the same wall-clock case's byte
    # assertion — the close still comes (at the landing), so only the
    # missing 408 bytes distinguish the mutant — and by the fix-r5
    # never-completing cap cell, whose landing answer is the same silent
    # close (EOF where the 408's bytes belong).
    ('sms-headers-408-bytes-off', SMS / 'mod.rs',
     ['half_sent_head_is_closed_at_the_deadline_and_leaves_no_task',
      'a_capped_withheld_head_behind_a_never_completing_response_gets_its_408_at_the_landing'],
     ["#2499 the head deadline's answer is Node's canned 51-byte 408",
      'next head parked at the withholding cap: EOF before the 408 arrived']),
    # #2499: the landing degenerates to the deadline itself instead of the
    # first connections-checker grid point strictly after it. Killed by both
    # paused-clock landing cells: the off-grid cell's mutant 408 fires at
    # the +15 s deadline, so the probe between the deadline and the +30 s
    # landing reads it; the on-grid cell's fires at the +30 s deadline the
    # strictly-greater comparison holds for +60 s, so its probe past the
    # deadline reads it too. #2499 fix-r3 adds the two same-read
    # swallowed-head cells (the registered residual 3's fix-r3 expansion):
    # their standby windows anchor at +0.06 s / +10.06 s, so the mutant
    # closes at the +15.06 s / +25.06 s deadline itself and the +29.46 s
    # still-open probes read the already-latched EOF.
    ('sms-headers-grid-off', SMS / 'mod.rs',
     ['the_408_lands_on_the_first_grid_point_after_the_deadline',
      'an_on_grid_deadline_fires_at_its_successor_grid_point',
      'a_follow_up_head_swallowed_in_the_same_read_as_the_previous_request_closes_on_the_standby_landing',
      'a_follow_up_head_swallowed_with_a_read_ahead_body_segment_closes_on_the_standby_landing',
      'a_capped_withheld_head_behind_a_never_completing_response_gets_its_408_at_the_landing'],
     ['stays unanswered between its +15 s deadline and the +30 s grid landing',
      'stays unanswered past its own +30 s deadline',
      'the follow-up head swallowed in the same read as the previous request '
      'keeps the connection open',
      'the follow-up head swallowed with a read-ahead body segment '
      'keeps the connection open',
      "keeps the connection open before its deadline's +30 s grid landing"]),
    # #2499: the deadline never re-anchors at the awaited head's first byte —
    # the connect-anchored or standby arm survives. Killed by all three
    # re-anchor cells: the paused-clock delayed-first-head cell (the
    # mutant's 408 lands at the connect anchor's +90 s grid point, and the
    # probe past it reads the bytes) and both follow-up cells (the standby
    # window's +30 s landing closes the follow-up head one grid point
    # before its own first-byte-anchored +60 s one).
    ('sms-headers-first-byte-anchor-off', SMS / 'mod.rs',
     ['a_delayed_first_head_re_anchors_the_deadline_at_its_first_byte',
      'a_follow_up_half_head_after_a_served_response_is_answered_with_the_408',
      'a_follow_up_head_after_a_bodied_request_re_anchors_at_its_first_byte'],
     ['stays unanswered past the connect-anchored +90 s landing',
      'the follow-up half head stays unanswered past the standby',
      'the follow-up head stays unanswered past the standby']),
    # #2499: a zero-byte follow-up head on a connection that already served a
    # response is answered with the 408 too, instead of Node's
    # `keepAliveTimeout` silent close (the registered residual). Killed by
    # the idle keep-alive cell, whose EOF probe must read zero bytes.
    # #2499 fix-r3 adds the two same-read swallowed-head cells (the
    # registered residual 3's fix-r3 expansion): their closes ride the same
    # follow-up-idle path — no first-byte anchor was ever recorded for the
    # swallowed head — so the mutant writes the 408 there too and their EOF
    # probes read the bytes instead of the silence.
    ('sms-headers-idle-followup-408', SMS / 'mod.rs',
     ['an_idle_keep_alive_connection_closes_silently_without_a_408',
      'a_follow_up_head_swallowed_in_the_same_read_as_the_previous_request_closes_on_the_standby_landing',
      'a_follow_up_head_swallowed_with_a_read_ahead_body_segment_closes_on_the_standby_landing'],
     ['the idle follow-up head must close byte-silent',
      'swallowed in the same read as the previous request: 51 byte(s) arrived '
      'before the EOF',
      'swallowed with a read-ahead body segment: 51 byte(s) arrived '
      'before the EOF']),
    # #2499: the stop watch no longer disarms the armed deadline, so a
    # draining connection gets its 408 mid-drain (Node clears the checker
    # in `httpServerPreClose`). Killed by both stop cells — the paused-clock
    # disarm cell (the probe past the would-be landing reads the 408) and
    # the wall-clock drain case (the draining connection carries 408 bytes
    # and EOF where both must be absent).
    ('sms-headers-stop-no-disarm', SMS / 'mod.rs',
     ['stop_disarms_the_head_deadline_for_connections_still_draining',
      'stop_drains_a_half_sent_header_once_the_header_timeout_fires'],
     ['the stop disarmed the half head\'s deadline',
      'stop() disarmed the head deadline']),
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
    # #2499 fix-r1 (review sibling#3): an on-grid deadline fires at its own
    # grid point instead of waiting for the strictly-greater comparison's
    # successor. Killed by the paused-clock on-grid cell: the mutant's 408
    # arrives at +30 s, so the probe past the deadline reads it where the
    # cell requires silence until +60 s.
    ('sms-headers-grid-inclusive', SMS / 'mod.rs',
     'an_on_grid_deadline_fires_at_its_successor_grid_point',
     'stays unanswered past its own +30 s deadline'),
    # #2499 fix-r1 (review boundary#1/concurrency#1): a chunked request
    # body reads as "no body pending", so the gate parks its bytes as if
    # they were the next request's head and the handler starves into the
    # body 408. Killed by the paused-clock chunked cell: the starved body
    # read's 408-with-`Connection: close` lands instead of the TwiML, so
    # the cell reads EOF before its answer.
    ('sms-headers-chunked-body-withheld', SMS / 'mod.rs',
     'a_chunked_request_body_reaches_the_handler_and_is_answered_not_408',
     'EOF before the TwiML response arrived'),
    # #2269: the r1 counting shape — the truncation warn reads a pre-drain
    # snapshot, so the connection that retired during the drain is still
    # counted as aborted. Killed by the bound case's warn-count assertion
    # (3 where the aborted set holds 2).
    ('sms-stop-drain-predrain-count', SMS / 'mod.rs',
     'stop_returns_at_the_drain_bound_when_a_connection_outlives_it',
     '#2026 the truncation warn counts only the connections still in the'),
    # #2499 fix-r5 (review boundary#1/concurrency#1): `RearmBody::release`
    # wakes the gate parked at the withholding cap through the release slot,
    # so the parked head re-feeds the moment the response completes — no
    # successor bytes needed (the fix-r4 empty-buffer poll it replaces was
    # empirically inert on a production TcpStream, and hyper does not re-poll
    # the parked read on its own). The mutant skips only the wake, so the
    # gate-level cell's counting waker never fires and the end-to-end cell's
    # second read — the first withheld head's response — never arrives (the
    # paused clock freezes every timer, so the kill is deterministic).
    ('sms-headers-withheld-park-no-wake', SMS / 'mod.rs',
     ['a_next_head_parked_at_the_withholding_cap_wakes_at_the_response_release',
      'a_capped_withheld_head_is_re_fed_and_served_when_the_response_completes'],
     ['#2499 fix-r5: the RearmBody release woke the parked gate',
      'the withheld head bytes were never re-fed to hyper']),
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
