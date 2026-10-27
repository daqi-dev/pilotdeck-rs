#!/usr/bin/env python3
"""#886 committed-source controls; never substitute a different Git baseline."""
import argparse
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import Case, run_cases

root = Path(__file__).resolve().parents[5]
source = root / 'src/pilotdeck-gateway/src/runtime/mcp/http.rs'
encoding = root / 'src/pilotdeck-gateway/src/runtime/model/http/encoding.rs'
# Whole-file, unique anchors understood by anchor_selfcheck.py.
controls = [
    ('no-decode', source,
     'response.map(|body| body.map_err(|_| BodyFailure::Terminated).boxed()),',
     '{ let mut response = response; response.headers_mut().remove(header::CONTENT_ENCODING); response.map(|body| body.map_err(|_| BodyFailure::Terminated).boxed()) },',
     'mcp_json_decodes', '#886 JSON decoding'),
    ('no-layer-limit', encoding,
     'encodings.len() > MAX_CONTENT_ENCODINGS', 'encodings.len() > usize::MAX',
     'mcp_encoding_limit', '#886 encoding layer cap'),
    ('sse-no-decode', source,
     'response.map(|body| body.map_err(|_| BodyFailure::Terminated).boxed()),',
     '{ let mut response = response; if header_value(response.headers(), "content-type").is_some_and(|value| value.contains("text/event-stream")) { response.headers_mut().remove(header::CONTENT_ENCODING); } response.map(|body| body.map_err(|_| BodyFailure::Terminated).boxed()) },',
     'mcp_sse_decodes', '#886 SSE decoding'),
    ('get-sse-no-decode', source,
     'response.map(|body| body.map_err(|_| BodyFailure::Terminated).boxed()),',
     '{ let mut response = response; if method == Method::GET { response.headers_mut().remove(header::CONTENT_ENCODING); } response.map(|body| body.map_err(|_| BodyFailure::Terminated).boxed()) },',
     'mcp_get_sse_decodes', '#886 GET SSE decoding'),
    ('json-body-error-projection', source,
     '        fetch_redirect::BodyError::Terminated | fetch_redirect::BodyError::Aborted => {\n            McpClientError::plain("TypeError", "terminated")',
     '        fetch_redirect::BodyError::Terminated | fetch_redirect::BodyError::Aborted => {\n            network_error("network_connection_reset", "terminated".into())',
     'mcp_json_decodes', '#886 JSON decoding'),
    ('legacy-sse-no-decode', source,
     'response.map(|body| body.map_err(|_| BodyFailure::Terminated).boxed()),',
     '{ let mut response = response; if method == Method::GET { response.headers_mut().remove(header::CONTENT_ENCODING); } response.map(|body| body.map_err(|_| BodyFailure::Terminated).boxed()) },',
     'mcp_legacy_sse_decodes', '#886 legacy SSE decoding'),
    ('streamable-error-no-decode', source,
     'response.map(|body| body.map_err(|_| BodyFailure::Terminated).boxed()),',
     '{ let mut response = response; if !response.status().is_success() { response.headers_mut().remove(header::CONTENT_ENCODING); } response.map(|body| body.map_err(|_| BodyFailure::Terminated).boxed()) },',
     'mcp_streamable_error_decodes', '#886 streamable error decoding'),
    ('legacy-error-no-decode', source,
     'response.map(|body| body.map_err(|_| BodyFailure::Terminated).boxed()),',
     '{ let mut response = response; if !response.status().is_success() { response.headers_mut().remove(header::CONTENT_ENCODING); } response.map(|body| body.map_err(|_| BodyFailure::Terminated).boxed()) },',
     'mcp_legacy_error_decodes', '#886 legacy error decoding'),
    ('decode-intermediate-redirect', source,
     '        follow_redirects,\n    )\n    .map_err(|error|',
     '        false && follow_redirects,\n    )\n    .map_err(|error|',
     'mcp_redirect_decodes', '#886 redirect decoding'),
    ('legacy-layer-cause', source,
     '                encoding::MAX_CONTENT_ENCODINGS,',
     '                encoding::MAX_CONTENT_ENCODINGS + 1,',
     'mcp_legacy_sse_decodes', '#886 legacy SSE decoding'),
]


def mutate(content, scope, replacements):
    for old, new in replacements:
        count = content.count(old)
        if count != 1:
            raise ValueError(f'#886 stale anchor count={count}, expected=1: {old!r}')
        content = content.replace(old, new, 1)
    return content


def audit(rows):
    for _, path, old, new, _, _ in rows:
        mutate(path.read_text(), None, [(old, new)])


def compile_controls(rows, out, env):
    results = []
    try:
        with mutating_sources(root, list(dict.fromkeys(row[1] for row in rows))) as originals:
            for name, path, old, new, _, _ in rows:
                result = {'mutation': name, 'compiled': False}
                try:
                    path.write_text(mutate(originals[path].decode(), None, [(old, new)]))
                    slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=600)
                    (out / f'{name}-compile.slots.log').write_text(slot.stdout + slot.stderr)
                    run = run_owned(['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
                                     '-p', 'pilotdeck-gateway', '--lib', '--no-run'],
                                    cwd=root, env=env, timeout=600)
                    (out / f'{name}-compile.log').write_text(run.stdout + run.stderr)
                    result.update(compiled=run.returncode == 0, returncode=run.returncode)
                except Exception as error:
                    result['reason'] = f'{type(error).__name__}: {error}'
                finally:
                    path.write_bytes(originals[path])
                results.append(result)
    except Exception as error:
        for name, *_ in rows[len(results):]:
            results.append({'mutation': name, 'compiled': False,
                            'reason': f'source guard: {type(error).__name__}: {error}'})
    (out / 'compile-summary.json').write_text(json.dumps(results, indent=2) + '\n')
    for result in results:
        print(json.dumps(result), flush=True)
    return all(result['compiled'] for result in results)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--audit', action='store_true')
    parser.add_argument('--compile-only', action='store_true')
    parser.add_argument('output', nargs='?')
    parser.add_argument('selected', nargs='*')
    args = parser.parse_args()
    rows = controls
    if args.selected:
        unknown = set(args.selected) - {row[0] for row in rows}
        if unknown:
            parser.error(f'unknown mutations: {sorted(unknown)}')
        rows = [row for row in rows if row[0] in args.selected]
    audit(rows)  # Stale anchors are errors, never survivors.
    if args.audit:
        print(json.dumps([{'mutation': row[0], 'anchor': 'unique'} for row in rows], indent=2))
        return 0
    env = os.environ.copy()
    if not env.get('CARGO_TARGET_DIR'):
        parser.error('CARGO_TARGET_DIR must be supplied by the caller')
    if not args.output:
        parser.error('provide a fresh evidence directory')
    env.update(CARGO_PROFILE_DEV_DEBUG='line-tables-only', CARGO_PROFILE_TEST_DEBUG='line-tables-only',
               CARGO_TERM_QUIET='false')
    out = Path(args.output).resolve()
    out.mkdir(parents=True, exist_ok=False)
    compiled = compile_controls(rows, out, env)
    if args.compile_only:
        return int(not compiled)
    bound_cases = [Case(name, path, old, new, 'runtime::mcp::http::encoding_tests::' + test, assertion)
             for name, path, old, new, test, assertion in rows]
    status = run_cases(root, bound_cases, out, env, mutate, timeout=600)
    return int(not compiled or status != 0)


if __name__ == '__main__':
    raise SystemExit(main())
