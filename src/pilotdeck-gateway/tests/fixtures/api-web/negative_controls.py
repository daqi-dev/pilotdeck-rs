#!/usr/bin/env python3
"""#1348 compiled controls: compile every switch once, then run the selected
positive, negative and restored cases with negative_suite verdicts. No source
rewriting, no alternate HEAD and no second target directory. Every switch
mutates exactly one api-web route, file-service gate or #2354 dispatch seam
and is killed by the replay cell or boundary probe that pins that gate
(14 switches). Run one sample in the authoring session; the coordinator runs
the complete list after the patch is committed.
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
NATIVE_HTTP = ROOT / 'src/pilotdeck-gateway/src/native_http.rs'
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
    # again, so a planted directory symlink — in the root or replacing the
    # root itself — carries the write out of the root; the read opens by path
    # again, so a leaf or a middle component swapped after the stat check
    # serves outside content; the enumeration lists the swapped directory; and
    # the missing-root path mkdir follows a swapped ancestor out of the root.
    ('api-web-nofollow-off', FILES,
     ['write_rejects_a_symlink_that_leaves_the_root',
      'write_rejects_a_component_swapped_inside_the_window',
      'write_rejects_a_root_swapped_for_a_link',
      'write_rejects_a_leaf_swapped_inside_the_window',
      'write_rejects_a_missing_root_below_a_swapped_ancestor',
      'write_rejects_a_component_swapped_to_an_outside_directory_leaf',
      'read_rejects_a_file_swapped_inside_the_window',
      'read_rejects_a_middle_component_swapped_inside_the_window',
      'read_tree_rejects_a_directory_swapped_inside_the_window'],
     ['#1675 mutated: a symlink carried the write out of the workspace',
      '#1675 mutated: the race window carried the write out of the workspace',
      '#1675 mutated: the swapped root carried the write out of the workspace',
      '#1675 mutated: the race window carried the leaf write out of the workspace',
      '#1675 mutated: the swapped ancestor carried the created root out of the workspace',
      # #2354 fix-r1: the outside leaf is a directory, so the leak assert
      # still holds — the first failing assertion is the boundary match arm.
      '#2354 a raced component to an outside directory answers workspace_boundary, not a write error',
      '#1675 mutated: a raced read served content from outside the workspace',
      '#1675 mutated: the race window resolved a middle component outside the workspace',
      '#1675 mutated: the race window listed outside the workspace']),
    # #2354 dispatch off: the whole `/api/web/` branch in the native HTTP
    # entry declines, so the hit cell falls through to the static 404 instead
    # of the project list.
    ('native-http-api-web-off', NATIVE_HTTP, 'projects_ok', '#1348 cell projects-ok normalized response'),
    # #1765 admission off: the branch runs without the llhttp-equivalent
    # gates, so the bad-method cell reaches the router's 404 instead of the
    # parser's bare 400 + close.
    ('api-web-admission-off', NATIVE_HTTP, 'gate_bad_method_is_parser_bare_400',
     '#1765 the bad-method gate answers the parser bare 400 before the route group'),
    # #1764 write refusal off: the NUL path falls through to the write's own
    # open, whose EINVAL diagnostic replaces Node's argument-validation text.
    # #2354 fix-r1: the EISDIR shape moved out of the switch — the refusal now
    # answers EISDIR from the write's own open, so turning the check off can no
    # longer redden an EISDIR pin.
    ('api-web-node-write-refusal-off', FILES,
     'write_to_a_nul_path_answers_node_argument_validation',
     "#1764 the NUL-path write answers Node's argument-validation text"),
    # #2153 fix-r1 (review r1): the `content-length: 0` exemption is the two
    # parser bare-400 gates' only — the replay normalizes every oracle cell
    # with the drop off. The unscoped mutant re-widens the mask inside the
    # normalize wrapper, so the injected-line unit test fails; the off
    # mutant drops the gates' exemption, so the gate replay fails; the two
    # product mutants drop or grow the wire line the raw pin holds in both
    # directions.
    ('api-web-replay-cl0-unscoped', ROOT / 'src/pilotdeck-gateway/src/api_web/tests.rs',
     'only_the_registered_gates_mask_a_content_length_zero',
     '#2153 an extra content-length: 0 on a replayed response must not normalize away'),
    ('api-web-replay-cl0-off', ROOT / 'src/pilotdeck-gateway/src/api_web/tests.rs',
     'gate_bad_method_is_parser_bare_400',
     '#1765 the bad-method gate answers the parser bare 400 before the route group'),
    ('native-http-parser-400-cl0-off', NATIVE_HTTP,
     'content_length_zero_stays_scoped_to_the_registered_gates',
     '#2153 pin gate-bad-method: the registered bare 400 carries content-length: 0'),
    ('api-web-head-404-cl0', ROUTER,
     'content_length_zero_stays_scoped_to_the_registered_gates',
     '#2153 pin projects-head: the HEAD 404 carries no content-length at all'),
]
GUARDED = [ROUTER, FILES, NATIVE_HTTP, ROOT / 'src/pilotdeck-gateway/src/api_web/tests.rs',
    ROOT / 'src/pilotdeck-gateway/src/native_http/tests.rs',
    ROOT / 'src/pilotdeck-gateway/src/mutants.rs',
    ROOT / 'src/pilotdeck-gateway/src/lib.rs',
    ROOT / 'src/pilotdeck-gateway/tests/common/baseline.rs',
    ROOT / 'src/pilotdeck-gateway/tests/common/replay_normalize.rs',
    ROOT / 'src/pilotdeck-gateway/Cargo.toml', ROOT / 'src/Cargo.toml', ROOT / 'src/Cargo.lock',
    FIXTURES / '_shared/mutant_switch.py', FIXTURES / 'negative_suite.py', FIXTURES / 'negative_process.py', FIXTURES / 'anchor_selfcheck.py',
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
