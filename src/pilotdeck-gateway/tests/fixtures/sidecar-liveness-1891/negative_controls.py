#!/usr/bin/env python3
"""#1891: the relaxed liveness margins must stay load-bearing, not advisory.

Every row shrinks one #1891 watchdog to ``from_nanos(1)`` — a window no
healthy fixture can ever fit inside, so the kill is deterministic without any
ambient load. What each row proves is the direction the relaxation could have
silently broken: the first-byte capture loops must still notice a peer that
produced no bytes and stayed open, the ``bounded_liveness``/``bounded``/
``next`` watchdogs must still fire when the peer never progresses, and the
``Limits`` margins must still be enforced (an execution deadline of 1 ns must
fail the happy path, a handshake deadline of 1 ns must fail the spawn). The
kill assertion of every row is the site's own stable oracle text (receipt-
wired in the same #1891 change; without the receipt environment the helpers
behave exactly as before).

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

MUTATIONS = [
    # A-group first-byte capture loops: 60 s margin shrunk below any possible
    # response, so the loop exits with no bytes and the cell oracle must fire.
    ('api-web-first-byte-shrunk', API_WEB,
     'const FIRST_BYTE: Duration = Duration::from_secs(60);',
     'const FIRST_BYTE: Duration = Duration::from_nanos(1);',
     [('api_web::tests::projects_ok',
       '#1348 cell projects-ok produced no bytes and stayed open')]),
    ('approvals-first-byte-shrunk', APPROVALS,
     'const FIRST_BYTE: Duration = Duration::from_secs(60);',
     'const FIRST_BYTE: Duration = Duration::from_nanos(1);',
     [('module_host::approvals::tests::auth_missing',
       '#1350 cell auth-missing produced no bytes and stayed open')]),
    ('native-http-first-byte-shrunk', NATIVE_HTTP,
     'const FIRST_BYTE: Duration = Duration::from_secs(60);',
     'const FIRST_BYTE: Duration = Duration::from_nanos(1);',
     [('native_http::tests::health_get',
       '#1209 cell health-get produced no bytes and stayed open')]),
    ('native-ws-first-byte-shrunk', NATIVE_WS,
     'const FIRST_BYTE: Duration = Duration::from_secs(60);',
     'const FIRST_BYTE: Duration = Duration::from_nanos(1);',
     [('native_ws::tests::upgrade_budget_boundary_matches_ts',
       '#1351 every boundary cell produced bytes or closed')]),
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
    # #2228 r13 precedent), so all three belong in the write-guard set.
    return matrix_main(ROOT, __file__, '', MUTATIONS,
                       passed_count=1, child_count=0,
                       paths=[API_WEB, APPROVALS, NATIVE_HTTP, NATIVE_WS,
                              HOST_FACTORY, LIVE_TAIL, INTERACTION_HOST,
                              LIVE_STREAM, RESUME, HOST_TESTS,
                              ROOT / 'src/pilotdeck-gateway/tests/fixtures/'
                                     'live-session/peer.py',
                              ROOT / 'scripts/a6-host/fixture.py',
                              ROOT / 'scripts/a6-host/lifetime.py'],
                       exact=True)


if __name__ == '__main__':
    raise SystemExit(main())
