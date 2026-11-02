#!/usr/bin/env python3
"""#1349 compiled controls: compile every switch once, then run the selected
positive, negative and restored cases with negative_suite verdicts. No source
rewriting, no alternate HEAD and no second target directory. Every switch
mutates exactly one module_host transport gate and is killed by the replay
cell that pins that gate's TS bytes (20 switches). Run one sample in the
authoring session; the coordinator runs the complete list after the patch is
committed.
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
]
GUARDED = [SOURCE, ROOT / 'src/pilotdeck-gateway/src/module_host/tests.rs',
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
