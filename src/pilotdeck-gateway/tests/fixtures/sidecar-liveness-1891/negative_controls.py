#!/usr/bin/env python3
"""#1891: the relaxed liveness margins must stay load-bearing, not advisory.

Every row shrinks one #1891 watchdog to a window no healthy fixture can ever
fit inside, so the kill is deterministic without any ambient load. The A-group
rows shrink ``FIRST_BYTE`` to ``Duration::ZERO`` — ``elapsed() < ZERO`` is
always false, so the capture loop never runs and the empty capture must die on
the cell's own oracle (fix-r1: ``from_nanos(1)`` left a clock-granularity
race — a response already sitting in the socket buffer could be read inside
the 1 ns window and the row would not kill). The B-group rows shrink their
watchdog's timeout argument to ``from_nanos(1)``: the first peer-dependent
await inside each wrapped future is provably pending (the event or spawn it
waits on requires a Python round trip that has not started), so the elapsed
timer always wins the race. What each row proves is the direction the
relaxation could have silently broken: the first-byte capture loops must still
notice a peer that produced no bytes and stayed open, the
``bounded_liveness``/``bounded``/``next`` watchdogs must still fire when the
peer never progresses, and the ``Limits`` margins must still be enforced (an
execution deadline of 1 ns must fail the happy path, a handshake deadline of
1 ns must fail the spawn). The kill assertion of every row is the site's own
stable oracle text (receipt-wired in the same #1891 change; without the
receipt environment the helpers behave exactly as before).

Rows run the bound test only (exact mode, one target each); the positive and
restored phases pin that the un-mutated bytes pass the same test. The table is
HEAD-guarded: run it after committing the candidate, from a clean tree, with
``CARGO_TARGET_DIR`` supplied. ``--check`` verifies every anchor against the
working tree; a stale anchor is an error exit, never a SURVIVED verdict.
"""
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import matrix_main

ROOT = Path(__file__).resolve().parents[5]
GATEWAY = ROOT / 'src/pilotdeck-gateway/src'
API_WEB = GATEWAY / 'api_web/tests.rs'
APPROVALS = GATEWAY / 'module_host/approvals/tests.rs'
NATIVE_HTTP = GATEWAY / 'native_http/tests.rs'
NATIVE_WS = GATEWAY / 'native_ws/tests.rs'
HOST_FACTORY = GATEWAY / 'runtime/turn/host_factory_tests.rs'
LIVE_TAIL = GATEWAY / 'runtime/turn/live_tail_tests.rs'
INTERACTION_HOST = GATEWAY / 'runtime/project/interaction_host_tests.rs'
LIVE_STREAM = GATEWAY / 'runtime/turn/live_stream_tests.rs'
RESUME = GATEWAY / 'host/tests/resume.rs'
HOST_TESTS = GATEWAY / 'host/tests.rs'
MODULE_HOST_TESTS = GATEWAY / 'module_host/tests.rs'

MUTATIONS = [
    # A-group first-byte capture loops: the margin shrinks to ZERO, so the
    # loop condition is never true, the loop exits with no bytes and the cell
    # oracle must fire (fix-r1: no clock-granularity assumption at all).
    ('api-web-first-byte-shrunk', API_WEB,
     'const FIRST_BYTE: Duration = Duration::from_secs(60);',
     'const FIRST_BYTE: Duration = Duration::ZERO;',
     [('api_web::tests::projects_ok',
       '#1348 cell projects-ok produced no bytes and stayed open')]),
    ('approvals-first-byte-shrunk', APPROVALS,
     'const FIRST_BYTE: Duration = Duration::from_secs(60);',
     'const FIRST_BYTE: Duration = Duration::ZERO;',
     [('module_host::approvals::tests::auth_missing',
       '#1350 cell auth-missing produced no bytes and stayed open')]),
    ('native-http-first-byte-shrunk', NATIVE_HTTP,
     'const FIRST_BYTE: Duration = Duration::from_secs(60);',
     'const FIRST_BYTE: Duration = Duration::ZERO;',
     [('native_http::tests::health_get',
       '#1209 cell health-get produced no bytes and stayed open')]),
    ('native-ws-first-byte-shrunk', NATIVE_WS,
     'const FIRST_BYTE: Duration = Duration::from_secs(60);',
     'const FIRST_BYTE: Duration = Duration::ZERO;',
     [('native_ws::tests::upgrade_budget_boundary_matches_ts',
       '#1351 every boundary cell produced bytes or closed')]),
    # fix-r2 (review sibling#6): the fifth same-source FIRST_BYTE site the
    # fix-r1 table left uncovered, so its margin was advisory here.
    ('module-host-first-byte-shrunk', MODULE_HOST_TESTS,
     'const FIRST_BYTE: Duration = Duration::from_secs(60);',
     'const FIRST_BYTE: Duration = Duration::ZERO;',
     [('module_host::tests::auth_missing',
       '#1349 cell auth-missing produced no bytes and stayed open')]),
    # B-group watchdog helpers: the timeout argument itself shrinks, so the
    # first peer-driven await must die on the helper's own panic text.
    ('host-factory-liveness-shrunk', HOST_FACTORY,
     'timeout(SIDECAR_LIVENESS, future)',
     'timeout(Duration::from_nanos(1), future)',
     [('runtime::turn::host_factory::tests::'
       'live_session_shares_recorder_and_reads_current_history',
       'sidecar liveness wait timed out')]),
    ('live-tail-bounded-shrunk', LIVE_TAIL,
     'tokio::time::timeout(Duration::from_secs(60), future)',
     'tokio::time::timeout(Duration::from_nanos(1), future)',
     [('runtime::turn::live::tail_tests::live_tail_progress_and_drop',
       '#742 live tail bounded')]),
    ('interaction-host-liveness-shrunk', INTERACTION_HOST,
     'tokio::time::timeout(SIDECAR_LIVENESS, future)',
     'tokio::time::timeout(Duration::from_nanos(1), future)',
     [('runtime::project::factory::tests::interaction_host::'
       'host_consumes_actual_generations_across_reload_and_live_stdio',
       'sidecar liveness wait timed out')]),
    ('live-stream-next-shrunk', LIVE_STREAM,
     'timeout(Duration::from_secs(60), turn.next())',
     'timeout(Duration::from_nanos(1), turn.next())',
     [('runtime::turn::live::stream_tests::'
       'host_normal_lifecycle_streams_events_and_projects_authoritative_final_once',
       '#741i/#855: LiveTurn must make progress (timeout is not a mutation '
       'kill)')]),
    # B-group peer Limits: the margin must still be an enforced deadline.
    # Handshake at 1 ns fails the spawn before any turn runs; execution at
    # 1 ns fails the first happy-path mode through the wired run() label.
    ('resume-liveness-shrunk', RESUME,
     'handshake_timeout: Duration::from_secs(60),',
     'handshake_timeout: Duration::from_nanos(1),',
     [('host::tests::resume::'
       'resume_sends_accepted_cursor_and_replays_from_next_sequence',
       '#1891 resume peer spawn failed')]),
    ('host-limits-execution-shrunk', HOST_TESTS,
     '        handshake_timeout: Duration::from_secs(60),\n'
     '        execution_timeout: Some(Duration::from_secs(60)),\n'
     '        close_timeout: Duration::from_secs(1),',
     '        handshake_timeout: Duration::from_secs(60),\n'
     '        execution_timeout: Some(Duration::from_nanos(1)),\n'
     '        close_timeout: Duration::from_secs(1),',
     [('host::tests::handshake_execute_ack_and_terminal',
       '#1891 host fixture run failed')]),
]


def main():
    # Runtime-read inputs of the bound rows: every B-group test spawns the
    # real `peer.py` (live-session fixture) or `scripts/a6-host/fixture.py`,
    # and fixture.py imports `lifetime.py` unconditionally at its top (the
    # #2228 r13 precedent). fix-r1 (sibling P1, #2228 r11/r12 precedent):
    # the resume-bound row runs `reconnect-*` modes, which fixture.py hands
    # to `resume_fixture.py` via runpy — a spawned input of this fixture's
    # bound rows, so it belongs in the write-guard set like its two siblings.
    return matrix_main(ROOT, __file__, '', MUTATIONS,
                       passed_count=1, child_count=0,
                       paths=[API_WEB, APPROVALS, NATIVE_HTTP, NATIVE_WS,
                              MODULE_HOST_TESTS,
                              HOST_FACTORY, LIVE_TAIL, INTERACTION_HOST,
                              LIVE_STREAM, RESUME, HOST_TESTS,
                              ROOT / 'src/pilotdeck-gateway/tests/fixtures/'
                                     'live-session/peer.py',
                              ROOT / 'scripts/a6-host/fixture.py',
                              ROOT / 'scripts/a6-host/lifetime.py',
                              ROOT / 'scripts/a6-host/resume_fixture.py'],
                       exact=True)


if __name__ == '__main__':
    raise SystemExit(main())
