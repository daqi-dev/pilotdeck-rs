#!/usr/bin/env python3
"""#1114 nonce receipt controls; account for every mutant.

Six mutations guard the detach contract from both sides: reverting a
detached site to a bare `tokio::spawn` (the pre-#1114 code that let a
synchronous `close` swallow an undispatched request), re-widening the
request POST into `detach` (the fix-r1 scope narrowing, whose timeout
budget TS includes, probe A), withdrawing the inline first poll, spawning
even a first-poll-Ready continuation (fix-r3 observes the resumed-after-
completion panic through the stashed join handle — no process-level hook
is left to mutate), and letting the dispatch abort stop retracting in-flight
I/O (the fresh-connection zero-bytes parity of probe C's fresh mode).
Every selected test passes on HEAD, the mutated test must fail its bound
`#1114` assertion receipt (not compilation), then the same test passes
again after guarded restore.

--check validates anchors. --compile-only compiles every selected mutant.
Provide a new output directory and optional mutation names; CARGO_TARGET_DIR
must be supplied. Commit first: the source guard is never bypassed.
"""
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
# negative_suite owns mutating_sources, run_owned and wait_for_slot.
from negative_suite import matrix_main

ROOT = Path(__file__).resolve().parents[5]
HTTP = ROOT / 'src/pilotdeck-gateway/src/runtime/mcp/http.rs'
WIRE = ROOT / 'src/pilotdeck-gateway/src/runtime/mcp/wire.rs'
NETWORK = ROOT / 'src/pilotdeck-gateway/src/runtime/network.rs'
PREFIX = 'runtime::mcp::http::encoding_tests::dispatch_'
GET = 'get_before_close'
GET_CONNECTS = 'get_connects_before_close'
GET_LINE = 'get_line_lands_before_close'
READY_POLL = 'ready_continuation_is_not_polled_again'
READY_SEND = 'ready_send_detached_does_not_panic'
REQUEST = 'request_background_and_cancel_inline'
REPLY = 'reply_before_resolve'
GET_ASSERTION = '#1114 the SSE GET is dispatched before close aborts it'
EXCHANGE_ASSERTION = '#1114 the SSE GET enters the network exchange before close aborts it'
ZERO_BYTES_ASSERTION = ('#1114 the fresh-connection GET delivers zero bytes '
                        'before close, matching the TS baseline')
POOLED_ASSERTION = '#1114 the pooled-socket GET request line lands before close aborts it'
READY_POLL_ASSERTION = '#1114 a Ready continuation is not polled again'
READY_SEND_ASSERTION = ('#1114 a first-poll-Ready detached continuation '
                        'is not spawned and polled again')
REQUEST_ASSERTION = ('#1114 only the timeout cancel is dispatched inline; '
                     'the request POST keeps its background send')
REPLY_ASSERTION = '#1114 the reply to a server request is dispatched inline'
MUTATIONS = [
    # The #1114 flake site itself: the SSE GET after a 202 for the
    # initialized notification goes back to a bare spawn. All three GET
    # tests trip their dispatch-count assertion first under this mutant.
    ('get-bare-spawn', HTTP,
     '                detach(start_sse(Arc::downgrade(wire), None, before_abort));',
     '                tokio::spawn(start_sse(Arc::downgrade(wire), None, before_abort));',
     [(GET, GET_ASSERTION), (GET_CONNECTS, GET_ASSERTION),
      (GET_LINE, GET_ASSERTION)]),
    # Fix-r1's scope narrowing: the request POST must keep its background
    # send (TS arms _setupTimeout before transport.send, so an inline
    # first poll would eat the request's timeout budget). Re-detaching it
    # counts two inline dispatches where the test pins one.
    ('request-send-redetached', WIRE,
     '                tokio::spawn(async move {\n'
     '                    if let Err(error) = wire.send(message).await {\n'
     '                        wire.fail(id, error);\n'
     '                    }\n'
     '                });',
     '                detach(async move {\n'
     '                    if let Err(error) = wire.send(message).await {\n'
     '                        wire.fail(id, error);\n'
     '                    }\n'
     '                });',
     [(REQUEST, REQUEST_ASSERTION)]),
    # The timeout cancel and the reply to a server request both dispatch
    # through send_detached's HTTP branch; a bare spawn leaves nothing
    # dispatched when the assertions run.
    ('detached-send-bare-spawn', WIRE,
     '                detach(async move {\n'
     '                    let _ = wire.send(value).await;\n'
     '                });',
     '                tokio::spawn(async move {\n'
     '                    let _ = wire.send(value).await;\n'
     '                });',
     [(REQUEST, REQUEST_ASSERTION), (REPLY, REPLY_ASSERTION)]),
    # The reviewer's variant: keep the wrapper (and its counting), withdraw
    # only the inline first poll. The GET count still reads 1, so only the
    # server-side observations catch the swallowed request — the fresh
    # connection never opens, and the pooled socket never sees the line.
    ('detach-no-inline-poll', HTTP,
     '    let mut continuation = Box::pin(continuation);\n'
     '    let mut context = std::task::Context::from_waker(std::task::Waker::noop());\n'
     '    if continuation.as_mut().poll(&mut context).is_pending() {',
     '    let mut continuation = Box::pin(continuation);\n'
     '    drop(std::task::Context::from_waker(std::task::Waker::noop()));\n'
     '    if true {',
     [(GET_CONNECTS, EXCHANGE_ASSERTION), (GET_LINE, POOLED_ASSERTION)]),
    # The P0 regression itself: spawn even a first-poll-Ready continuation,
    # so the finished future is polled again. The poll-count probe reads 2
    # and the real send_detached site's stashed join handle reports the
    # resumed-after-completion panic.
    ('detach-unconditional-spawn', HTTP,
     '    let mut context = std::task::Context::from_waker(std::task::Waker::noop());\n'
     '    if continuation.as_mut().poll(&mut context).is_pending() {',
     '    let mut context = std::task::Context::from_waker(std::task::Waker::noop());\n'
     '    let _ = continuation.as_mut().poll(&mut context);\n'
     '    if true {',
     [(READY_POLL, READY_POLL_ASSERTION), (READY_SEND, READY_SEND_ASSERTION)]),
    # Probe C fresh mode (Node v22.22.0, 50/50): an abort landing while the
    # connect is in flight retracts the request there too — zero bytes. If
    # the dispatch's synchronous-abort gate stops retracting in-flight I/O,
    # the remainder's next poll finishes the connect (the test's barrier
    # let the handshake complete) and writes the request head, so the fresh
    # connection delivers a line instead of zero bytes.
    ('dispatch-abort-never-wins', NETWORK,
     '        let aborted = abort.as_mut().poll(cx).is_ready();\n'
     '        let parent_at = aborted.then(&parent_event_at).flatten();\n'
     '        if aborted && parent_at.is_none() {\n'
     '            return Poll::Ready(Exchange::Aborted);\n'
     '        }',
     '        let aborted = false;\n'
     '        let parent_at = aborted.then(&parent_event_at).flatten();\n'
     '        if aborted && parent_at.is_none() {\n'
     '            return Poll::Ready(Exchange::Aborted);\n'
     '        }',
     [(GET_CONNECTS, ZERO_BYTES_ASSERTION)]),
]


def main():
    return matrix_main(ROOT, __file__, PREFIX, MUTATIONS,
                       passed_count=7, child_count=0,
                       paths=[HTTP, WIRE, NETWORK])


if __name__ == '__main__':
    raise SystemExit(main())
