#!/usr/bin/env python3
"""#1209 compiled controls: compile every switch; run the selected positive,
negative and restored cases with negative_suite verdicts. No source rewriting,
no alternate HEAD and no second target directory. Run one sample in the
authoring session; the coordinator can run the complete list after reviewing
the patch.
"""
import argparse
import os
from pathlib import Path
import sys
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from _shared import mutant_switch
ROOT = Path(__file__).resolve().parents[5]
FIXTURES = ROOT / 'src/pilotdeck-gateway/tests/fixtures'
SOURCE = ROOT / 'src/pilotdeck-gateway/src/native_http.rs'
SOURCE_PROXY = ROOT / 'src/pilotdeck-gateway/src/http_proxy.rs'
PREFIX = 'native_http::tests::'
switches = [
    ('native-http-method-allowlist-off', SOURCE, 'method_outside_llhttp_table_rejected', '#1209 unknown method is a bare 400'),
    ('native-http-target-form-off', SOURCE, 'authority_form_target_rejected', '#1209 authority-form target is a bare 400'),
    ('native-http-target-bytes-off', SOURCE, 'non_ascii_path', '#1209 cell non-ascii-path normalized response'),
    ('native-http-normalization-off', SOURCE, 'traversal_collapses_before_the_static_root', '#1209 traversal normalizes to the target file'),
    ('native-http-token-loopback-skip', SOURCE, 'local_token_requires_loopback_binding', '#1209 non-loopback local-token is a 404'),
    ('native-http-404-message', SOURCE, 'not_found_body_matches_ts', '#1209 404 body bytes'),
    ('native-http-containment-off', SOURCE, 'static_guard_rejects_escape', '#1209 containment guard rejects escape'),
    # fix-r1: keep-alive/close; the mutant disables close detection so every
    # response closes, which diverges on a keep-alive cell, not on the close
    # cell whose bytes it reproduces.
    ('native-http-connection-close-off', SOURCE, 'health_get', '#1209 cell health-get normalized response'),
    ('native-http-root-normalize-off', SOURCE, 'trailing_separator_root_still_serves', '#1209 trailing-separator root serves after normalization'),
    ('native-http-idle-close-off', SOURCE_PROXY, 'stop_closes_idle_keep_alive_connections', '#1209 stop closes idle keep-alive connections within the drain budget'),
    # fix-r2: the count cap is a bound on hyper's per-parse allocation, pinned
    # at the 2049-header TS-served boundary (Node has no count limit; its
    # binding constraint is the byte budget the row below covers).
    ('native-http-header-count-off', SOURCE_PROXY, 'header_counts_within_ts_bounds_stay_served', '#1209 2049 small headers stay served like TS'),
    # fix-r4: the 16384/20 KiB deep oversize cells live in their own test;
    # under this mutant the shared test's 16363 boundary cell stays served
    # and panics there first, so the old anchor inside it was unreachable.
    ('native-http-header-budget-off', SOURCE, 'header_budget_deep_oversize_431s_like_ts', '#1209 a 16384-byte single header is the TS 431'),
    # fix-r2: the served-only guard is what keeps a connected-but-silent
    # connection open across stop like TS server.close(); the mutant
    # force-closes it, so the late request never gets its response.
    ('native-http-unserved-close', SOURCE_PROXY, 'stop_serves_late_request_and_waits_like_ts_close', '#1209 the late request is still served after stop'),
    # fix-r3: Node's effective 6 s keep-alive idle close (keepAliveTimeout 5 s
    # + the fixed 1 s keepAliveTimeoutBuffer) is in force with or without a
    # stop (probe `probe-fixr3-a.out` A); the mutant disables it, so a served
    # connection never reaches EOF.
    ('native-http-keep-alive-timeout-off', SOURCE_PROXY, 'keep_alive_idle_close_matches_ts', '#1209 the served connection reaches EOF at the keep-alive idle close'),
    # fix-r3: a request genuinely in flight at stop is answered normally and
    # its socket ends at the idle close, never at the stop or the response
    # (probe `probe-fixr3-b.out` B1); the mutant disables keep-alive
    # mid-flight, and hyper then rewrites the in-flight response to
    # `Connection: close`.
    ('native-http-inflight-close', SOURCE_PROXY, 'stop_serves_in_flight_request_and_waits_like_ts_close', '#1209 the in-flight response keeps the connection alive'),
    # fix-r3: the span-budget boundary is inclusive — exactly 16384 tracked
    # bytes is the 431 (probe `probe-fixr3-c.out` C1); the mutant needs
    # strictly more, so the boundary cell stays served.
    ('native-http-header-budget-boundary', SOURCE, 'header_budget_431s_like_ts', '#1209 a 16363-byte single header (tracked 16384) is the TS 431'),
    # fix-r3: the count cap must never bind before the byte budget (probe
    # `probe-fixr3-c.out` C4 serves 16363 empty-value headers); the mutant
    # lowers the cap to 3000, so the 8000-header cell 431s.
    ('native-http-header-cap-low', SOURCE_PROXY, 'header_counts_within_ts_bounds_stay_served', '#1209 8000 small headers stay served like TS'),
    # #1250: Node closes an idle keep-alive connection at keepAliveTimeout 5 s
    # plus the fixed 1 s keepAliveTimeoutBuffer (6 s, while the response still
    # advertises the bare timeout=5); the mutant drops the buffer, so the close
    # lands at the bare 5 s — inside the test's still-open probe window.
    ('native-http-idle-close-buffer-off', SOURCE_PROXY, 'keep_alive_idle_close_lands_at_six_seconds', "#1250 the idle connection stays open at +5.99 s: Node closes at keepAliveTimeout + the fixed 1 s keepAliveTimeoutBuffer (6 s), never at the bare 5 s or a shortened buffer"),
    # #1250 fix-r1: the buffer is the fixed 1 s default, not any value in
    # (0, 2) s; a 500 ms buffer closes at 5.5 s, again inside the test's
    # still-open probe window (the pre-#1250 mutant above pins the 5 s floor,
    # this one pins the full second).
    ('native-http-idle-close-buffer-half', SOURCE_PROXY, 'keep_alive_idle_close_lands_at_six_seconds', "#1250 the idle connection stays open at +5.99 s: Node closes at keepAliveTimeout + the fixed 1 s keepAliveTimeoutBuffer (6 s), never at the bare 5 s or a shortened buffer"),
    # #1250: the idle deadline arms at the response's end-of-frames, not at
    # handler return; the mutant releases the in-flight slot and re-arms at
    # handler return, so the connection closes right after the body ends.
    ('native-http-inflight-body-off', SOURCE_PROXY, 'idle_close_waits_for_streaming_body', "#1250 the streaming connection stays open at +5.99 s after its body ended: Node arms the idle deadline at response finish, not at handler return"),
    # #1250: every read segment refreshes the idle deadline; the mutant stops
    # refreshing on reads, so the previous response's deadline closes the
    # connection while the follow-up head is still arriving.
    ('native-http-read-refresh-off', SOURCE_PROXY, 'idle_close_refreshes_on_read_segments', "#1250 the half-arrived follow-up head keeps the connection open past the pre-segment deadline: a read segment refreshes Node's idle deadline"),
    # #1250 fix-r1: a read before any request was served (a first head
    # arriving in segments) arms no keep-alive deadline — Node's timer exists
    # only after a response finished, and the first head is bounded by
    # headersTimeout instead (probe `node-first-head-segmented.out`); the
    # mutant refreshes on any read, so the deadline poisons the socket and the
    # completed first head is answered `Connection: close`.
    ('native-http-unserved-read-arms-idle', SOURCE_PROXY, 'idle_state_no_request_stays_open', "#1250 the segmented first head is answered keep-alive: Node arms no idle timer before the first response finish, got"),
]
GUARDED = [SOURCE, ROOT / 'src/pilotdeck-gateway/src/native_http/tests.rs',
    ROOT / 'src/pilotdeck-gateway/src/native_ws.rs',
    ROOT / 'src/pilotdeck-gateway/src/http_proxy.rs',
    ROOT / 'src/pilotdeck-gateway/src/http_proxy_tests.rs',
    ROOT / 'src/pilotdeck-gateway/src/mutants.rs',
    ROOT / 'src/pilotdeck-gateway/src/lib.rs',
    ROOT / 'src/pilotdeck-gateway/tests/common/baseline.rs',
    ROOT / 'src/pilotdeck-gateway/Cargo.toml', ROOT / 'src/Cargo.toml', ROOT / 'src/Cargo.lock',
    FIXTURES / '_shared/mutant_switch.py', FIXTURES / 'negative_suite.py', FIXTURES / 'negative_process.py',
    Path(__file__), Path(__file__).with_name('oracle.json'),
    Path(__file__).with_name('generate.mts'), Path(__file__).with_name('README.md'),
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
            prefix=PREFIX, script=Path(__file__), requested=args.names)
    except (ValueError, OSError) as error:
        print(f'preflight: {error}', file=sys.stderr)
        return 2

if __name__ == '__main__':
    sys.exit(main())
