#!/usr/bin/env python3
"""#1349 compiled controls: compile every switch once, then run the selected
positive, negative and restored cases with negative_suite verdicts. No source
rewriting, no alternate HEAD and no second target directory. Every switch
mutates exactly one module_host transport gate and is killed by the binding
its switches-table row names: a replay cell's one-line assertion, a component
test's one-line assertion or a #1696 recording-guard unit test (#1813 added
the recording-field guard and its switch binds that pair of component tests;
#1947's module-host-http10-framing-off pins
http10_value_body_is_close_delimited outside the replay_tests! table). Run
one sample in the authoring session; the coordinator runs the complete list
after the patch is committed.
"""
import argparse
import os
from pathlib import Path
import sys
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from _shared import mutant_switch
ROOT = Path(__file__).resolve().parents[5]
FIXTURES = ROOT / 'src/pilotdeck-gateway/tests/fixtures'
SOURCE = ROOT / 'src/pilotdeck-gateway/src/module_host.rs'
PREFIX = 'module_host::tests::'
switches = [
    # keep-alive detection off: every response closes, so the second request
    # of the reuse cell never gets its bytes.
    ('module-host-connection-close-off', SOURCE, 'keep_alive_reuse', '#1349 cell keep-alive-reuse normalized response'),
    # bearer gate off: the missing-authorization cell is served instead of 401.
    ('module-host-auth-off', SOURCE, 'auth_missing', '#1349 cell auth-missing normalized response'),
    # POST gate off: the GET cell falls through to the body gate's 400.
    ('module-host-method-off', SOURCE, 'method_get', '#1349 cell method-get normalized response'),
    # absent resolver off: the unconfigured cell is served an empty describe.
    ('module-host-provider-off', SOURCE, 'unconfigured_describe', '#1349 cell unconfigured-describe normalized response'),
    # wire-operation filter off: describe advertises the provider's own list.
    ('module-host-describe-filter-off', SOURCE, 'describe_default', '#1349 cell describe-default normalized response'),
    # advertised-operation gate off: the unknown operation is served.
    ('module-host-operation-gate-off', SOURCE, 'call_unknown', '#1349 cell call-unknown normalized response'),
    # input-shape gate off: the missing input reaches the provider.
    ('module-host-input-shape-off', SOURCE, 'input_missing', '#1349 cell input-missing normalized response'),
    # scope-override gate off: the smuggled tenantId reaches the provider.
    ('module-host-scope-override-off', SOURCE, 'scope_tenantid', '#1349 cell scope-tenantid normalized response'),
    # failure default code off: the transport failure body loses its code.
    ('module-host-failure-default-off', SOURCE, 'fail_default', '#1349 cell fail-default normalized response'),
    # budget boundary inclusive: exactly 32 MiB becomes the 413.
    ('module-host-input-budget-boundary', SOURCE, 'budget_exact', '#1349 cell budget-exact normalized response'),
    # budget off: the over-budget cell is served through to the provider.
    ('module-host-input-budget-off', SOURCE, 'budget_over', '#1349 cell budget-over normalized response'),
    # stream content-type gate off: the plain-ct stream is served empty.
    ('module-host-stream-ct-off', SOURCE, 'stream_plain_ct', '#1349 cell stream-plain-ct normalized response'),
    # stream-CT 502 wire-header merge off: the 502 loses the provider's
    # copied wire headers (etag, x-request-id).
    ('module-host-502-wire-headers-off', SOURCE, 'stream_plain_ct_headers', '#1349 cell stream-plain-ct-headers normalized response'),
    # JSON default content-type off: the value body carries no content-type.
    ('module-host-json-ct-off', SOURCE, 'ok', '#1349 cell ok normalized response'),
    # empty content-type falsiness off: the empty string is treated as a set
    # content-type, so the string body stays raw and no default lands.
    ('module-host-empty-ct-off', SOURCE, 'empty_ct_string', '#1349 cell empty-ct-string normalized response'),
    # string code-point iteration off: the non-BMP string iterates one UTF-16
    # unit at a time again (two U+FFFD events for one emoji).
    ('module-host-string-codepoint-off', SOURCE, 'ndjson_string_emoji', '#1349 cell ndjson-string-emoji normalized response'),
    # missing-stream gate off: the falsy stream body is destroyed after headers.
    ('module-host-missing-stream-off', SOURCE, 'stream_missing', '#1349 cell stream-missing normalized response'),
    # wire-header allowlist off: the non-wire headers are copied through.
    ('module-host-wire-headers-off', SOURCE, 'wireheaders', '#1349 cell wireheaders normalized response'),
    # principal trim gate off: the all-whitespace principal is served.
    ('module-host-principal-trim-off', SOURCE, 'principal_whitespace', '#1349 cell principal-whitespace normalized response'),
    # NDJSON newline off: every framed NDJSON event loses its terminator.
    ('module-host-ndjson-newline-off', SOURCE, 'ndjson_objects', '#1349 cell ndjson-objects normalized response'),
    # #1813: recording type/absence drift must fail before replay can use the
    # `false` default (the old `as_bool().unwrap_or(false)` read).
    ('module-host-oracle-recorded-half-close',
     ROOT / 'src/pilotdeck-gateway/src/module_host/tests.rs',
     ['recorded_half_close_rejects_wrong_type', 'recorded_half_close_rejects_absence'],
     ['#1349 #1813 rejects halfClose type drift',
      '#1349 #1813 halfClose is a required recording field']),
    # #1696: a recorded pad size past the replay budget must fail the read
    # instead of aborting the test binary on a huge allocation.
    ('module-host-oracle-pad-size', ROOT / 'src/pilotdeck-gateway/src/module_host/budget_guard.rs', 'recorded_pad_size_rejects_drift',
     '#1349 #1696 rejects pad size drift'),
    # #1696: the pad marker must be non-empty and appear exactly once, or the
    # expanded request multiplies the padding instead of replacing it.
    ('module-host-oracle-pad-marker', ROOT / 'src/pilotdeck-gateway/src/module_host/budget_guard.rs', 'recorded_pad_marker_rejects_drift',
     '#1349 #1696 rejects pad marker drift'),
    # #1696: the budget cells' expanded bodies must be the recorded boundary
    # lengths, so a tiny pad cannot leave `budget-exact`/`budget-over` green.
    ('module-host-oracle-boundary-body-length', ROOT / 'src/pilotdeck-gateway/src/module_host/budget_guard.rs', 'recorded_budget_body_length_rejects_drift',
     '#1349 #1696 rejects boundary body length drift'),
    # #1696: a new `budget-*` cell the guard has not been taught must be
    # rejected, not replayed without any length pin.
    ('module-host-oracle-unknown-budget-cell', ROOT / 'src/pilotdeck-gateway/src/module_host/budget_guard.rs', 'recorded_unknown_budget_cell_is_rejected',
     '#1349 #1696 fails closed on a budget cell class with no length guard'),
    # #1947: with the framing fix off, hyper knows the value body's size
    # again and writes a `content-length` line on the 1.0 reply where Node
    # is close-delimited, so the pinned value-body reply turns red.
    ('module-host-http10-framing-off', SOURCE, 'http10_value_body_is_close_delimited',
     "#1947 pin http10 value body: the 1.0 reply is close-delimited, no content-length"),
    # #2153 fix-r1 (review r1): the replay normalization no longer drops the
    # empty-body `content-length: 0` (no cell needed it — the one golden
    # `content-length: 0`, `undefbody`, matches on the wire). The mutant
    # reintroduces the unconditional drop, so the injected-line unit test
    # fails at its own assertion; the HEAD-405 product mutant grows the wire
    # line the raw pin holds.
    ('module-host-normalize-cl0-off', ROOT / 'src/pilotdeck-gateway/src/module_host/tests.rs',
     'content_length_zero_compares_verbatim',
     '#2153 an extra content-length: 0 on the HEAD 405 must not normalize away'),
    ('module-host-head-405-cl0', SOURCE, 'method_head_and_undefined_body_bytes_stay_pinned',
     '#2153 pin method-head: the HEAD 405 carries no framing line'),
]
GUARDED = [SOURCE, ROOT / 'src/pilotdeck-gateway/src/module_host/tests.rs',
    ROOT / 'src/pilotdeck-gateway/src/module_host/budget_guard.rs',
    # DS r3 sibling#1: the bound tests run the shared transport closure
    # (`module_host/tests.rs` builds it through `http_proxy::…` +
    # `native_http::…`), so the receipts cannot prove which transport bytes
    # they ran against without pinning them (the webhook lane's口径).
    ROOT / 'src/pilotdeck-gateway/src/native_http.rs',
    ROOT / 'src/pilotdeck-gateway/src/http_proxy.rs',
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
            prefix=PREFIX, script=Path(__file__), requested=args.names,
            test_timeout=480)
    except (ValueError, OSError) as error:
        print(f'preflight: {error}', file=sys.stderr)
        return 2

if __name__ == '__main__':
    sys.exit(main())
