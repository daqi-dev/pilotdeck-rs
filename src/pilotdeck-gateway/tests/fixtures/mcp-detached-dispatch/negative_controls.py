#!/usr/bin/env python3
"""#1114 HEAD-only nonce receipt controls; account for every selected mutant.

Each mutation reverts one detached-send site to a bare `tokio::spawn` — the
pre-fix code that let a synchronous `close` swallow an undispatched request
(the merge-queue CI flake). Every selected test passes on HEAD, the mutated
test must fail its bound `#1114` assertion receipt (not compilation), then
the same test passes again after guarded restore.

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
PREFIX = 'runtime::mcp::http::encoding_tests::dispatch_'
GET = 'get_before_close'
CANCEL = 'request_and_cancel_before_close'
REPLY = 'reply_before_resolve'
GET_ASSERTION = '#1114 the SSE GET is dispatched before close aborts it'
CANCEL_ASSERTION = '#1114 the request POST and the timeout cancel are dispatched inline'
REPLY_ASSERTION = '#1114 the reply to a server request is dispatched inline'
MUTATIONS = [
    # The #1114 flake site itself: the SSE GET after a 202 for the
    # initialized notification goes back to a bare spawn.
    ('get-bare-spawn', HTTP,
     '                detach(start_sse(Arc::downgrade(wire), None, before_abort));',
     '                tokio::spawn(start_sse(Arc::downgrade(wire), None, before_abort));',
     [(GET, GET_ASSERTION)]),
    # `Protocol.request` fires transport.send() before returning the promise.
    ('request-send-bare-spawn', WIRE,
     '                detach(async move {\n'
     '                    if let Err(error) = wire.send(message).await {\n'
     '                        wire.fail(id, error);\n'
     '                    }\n'
     '                });',
     '                tokio::spawn(async move {\n'
     '                    if let Err(error) = wire.send(message).await {\n'
     '                        wire.fail(id, error);\n'
     '                    }\n'
     '                });',
     [(CANCEL, CANCEL_ASSERTION)]),
    # The timeout cancel and the reply to a server request both dispatch
    # through send_detached's HTTP branch.
    ('detached-send-bare-spawn', WIRE,
     '                detach(async move {\n'
     '                    let _ = wire.send(value).await;\n'
     '                });',
     '                tokio::spawn(async move {\n'
     '                    let _ = wire.send(value).await;\n'
     '                });',
     [(CANCEL, CANCEL_ASSERTION), (REPLY, REPLY_ASSERTION)]),
]


def main():
    return matrix_main(ROOT, __file__, PREFIX, MUTATIONS,
                       passed_count=3, child_count=0,
                       paths=[HTTP, WIRE])


if __name__ == '__main__':
    raise SystemExit(main())
