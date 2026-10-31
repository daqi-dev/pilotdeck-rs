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
PREFIX = 'native_http::tests::'
switches = [
    ('native-http-method-allowlist-off', SOURCE, 'method_outside_llhttp_table_rejected', '#1209 unknown method is a bare 400'),
    ('native-http-target-form-off', SOURCE, 'authority_form_target_rejected', '#1209 authority-form target is a bare 400'),
    ('native-http-target-bytes-off', SOURCE, 'non_ascii_path', '#1209 cell non-ascii-path normalized response'),
    ('native-http-normalization-off', SOURCE, 'traversal_collapses_before_the_static_root', '#1209 traversal normalizes to the target file'),
    ('native-http-token-loopback-skip', SOURCE, 'local_token_requires_loopback_binding', '#1209 non-loopback local-token is a 404'),
    ('native-http-404-message', SOURCE, 'not_found_body_matches_ts', '#1209 404 body bytes'),
    ('native-http-containment-off', SOURCE, 'static_guard_rejects_escape', '#1209 containment guard rejects escape'),
]
GUARDED = [SOURCE, ROOT / 'src/pilotdeck-gateway/src/native_http/tests.rs',
    ROOT / 'src/pilotdeck-gateway/src/native_ws.rs',
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
            prefix=PREFIX, script=Path(__file__), requested=args.names)
    except (ValueError, OSError) as error:
        print(f'preflight: {error}', file=sys.stderr)
        return 2

if __name__ == '__main__':
    sys.exit(main())
