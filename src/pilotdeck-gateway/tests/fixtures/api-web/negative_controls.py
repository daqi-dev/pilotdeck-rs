#!/usr/bin/env python3
"""#1348 compiled controls: compile every switch once, then run the selected
positive, negative and restored cases with negative_suite verdicts. No source
rewriting, no alternate HEAD and no second target directory. Every switch
mutates exactly one api-web route or file-service gate and is killed by the
replay cell or boundary probe that pins that gate (7 switches). Run one sample
in the authoring session; the coordinator runs the complete list after the
patch is committed.
"""
import argparse
import os
from pathlib import Path
import sys
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from _shared import mutant_switch
ROOT = Path(__file__).resolve().parents[5]
FIXTURES = ROOT / 'src/pilotdeck-gateway/tests/fixtures'
ROUTER = ROOT / 'src/pilotdeck-gateway/src/api_web.rs'
FILES = ROOT / 'src/pilotdeck-gateway/src/api_web/files.rs'
PREFIX = 'api_web::tests::'
switches = [
    # bearer gate off: the missing-authorization cell is served the project
    # list instead of the 401.
    ('api-web-auth-off', ROUTER, 'projects_no_auth', '#1348 cell projects-no-auth normalized response'),
    # GET-only gate off: the POST cell falls into the list branch instead of
    # the trailing 404.
    ('api-web-projects-method-off', ROUTER, 'projects_post', '#1348 cell projects-post normalized response'),
    # describe catch off: the stub throw leaves the handler as a hyper Err
    # (zero-byte close) instead of the 404 project_not_found body.
    ('api-web-describe-catch-off', ROUTER, 'project_describe_throws', '#1348 cell project-describe-throws normalized response'),
    # workspace boundary off: the `..` escape cell serves the parent tree
    # instead of the 403 boundary_error.
    ('api-web-boundary-off', FILES, 'files_tree_escape', '#1348 cell files-tree-escape normalized response'),
    # text-extension gate off: the text cell answers base64 instead of utf8.
    ('api-web-text-ext-off', FILES, 'files_read_text', '#1348 cell files-read-text normalized response'),
    # maxReadBytes gate off: the 1 MiB+1 cell serves the full base64 body
    # instead of metadata only.
    ('api-web-read-limit-off', FILES, 'files_read_large', '#1348 cell files-read-large normalized response'),
    # #1675 NOFOLLOW descriptor hardening off: the write resolve is lexical
    # again, so a planted directory symlink carries the write out of the root,
    # and the read opens by path again, so a component swapped after the
    # stat check serves outside content.
    ('api-web-nofollow-off', FILES,
     ['write_rejects_a_symlink_that_leaves_the_root',
      'read_rejects_a_file_swapped_inside_the_window'],
     ['#1675 mutated: a symlink carried the write out of the workspace',
      '#1675 mutated: a raced read served content from outside the workspace']),
]
GUARDED = [ROUTER, FILES, ROOT / 'src/pilotdeck-gateway/src/api_web/tests.rs',
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
