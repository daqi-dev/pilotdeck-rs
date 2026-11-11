#!/usr/bin/env python3
"""#1553 committed-source controls; never substitute a different Git baseline."""
import argparse
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot, tree_lock
from negative_suite import Case, invalidate_completed, run_cases

root = Path(__file__).resolve().parents[5]
encoding = root / 'src/pilotdeck-gateway/src/runtime/model/http/encoding.rs'
model = root / 'src/pilotdeck-gateway/src/runtime/model/http.rs'
mcp = root / 'src/pilotdeck-gateway/src/runtime/mcp/http.rs'
cause_test = 'runtime::model::http::encoding_tests::codec_rejections_keep_the_zlib_cause'
# Whole-file, unique anchors understood by anchor_selfcheck.py.
controls = [
    ('decode-cause-removed', encoding,
     '        "Invalid gzip header" => BodyFailure::Decode("incorrect header check"),',
     '        "Invalid gzip header" => BodyFailure::Terminated,',
     cause_test, '#1553-5 gzip garbage: the codec cause text matches Node'),
    ('decode-cause-wrong-literal', encoding,
     '        "Invalid gzip header" => BodyFailure::Decode("incorrect header check"),',
     '        "Invalid gzip header" => BodyFailure::Decode("bad header"),',
     cause_test, '#1553-5 gzip garbage: the codec cause text matches Node'),
    ('wire-class-reversed', encoding,
     '        return *failure;',
     '        return BodyFailure::Decode("Decompression failed");',
     cause_test, '#1553-13 wire failure through the decoder stays Terminated'),
    ('legacy-settle-removed', mcp,
     '''    if let Err(error) = outcome
        && ready.is_some()
    {
        let cause = match error.into() {
            BodyFailure::Decode(cause) => format!(": {cause}"),
            _ => String::new(),
        };
        settle(
            ready,
            Err(McpClientError::plain(
                "Error",
                format!("SSE error: TypeError: terminated{cause}"),
            )),
        );
    }''',
     '    let _ = outcome;',
     'runtime::mcp::http::encoding_tests::mcp_legacy_start_failure_matrix',
     '#1553 legacy start failure matrix: loopback watchdog'),
    # response_text's own bare-terminated mapping (model provider error
    # bodies) has its direct unit test.
    ('response-text-decode-arm', model,
     '''            Exchange::Timeout { .. }
            | Exchange::Response(Some(Err(BodyFailure::Terminated | BodyFailure::Decode(_))), _) => {
                return Err(ModelStreamError::thrown("TypeError", "terminated"));
            }''',
     '''            Exchange::Timeout { .. }
            | Exchange::Response(Some(Err(BodyFailure::Terminated)), _) => {
                return Err(ModelStreamError::thrown("TypeError", "terminated"));
            }
            Exchange::Response(Some(Err(BodyFailure::Decode(_))), _) => {
                return Err(ModelStreamError::thrown("Error", crate::fetch_redirect::STRING_TOO_LONG));
            }''',
     'runtime::model::http::encoding_tests::response_text_keeps_terminated_on_codec_rejection',
     '#1553-16 response.text() keeps the bare terminated message'),
]


def mutate(content, scope, replacements):
    for old, new in replacements:
        count = content.count(old)
        if count != 1:
            raise ValueError(f'#1553 stale anchor count={count}, expected=1: {old!r}')
        content = content.replace(old, new, 1)
    return content


def audit(rows):
    # #2795 fix-r3（sibling P1 同族跟改，manager-gateway fix-r2 先例）：锚点核对
    # 也要持树锁读——兄弟变异运行器在自己的 write 与 restore 之间可能把变异体
    # 留在盘上，无锁读会把核对错在它不拥有的源上；锁忙 / 读失败是拒跑（2），
    # 不是宿主会读成 rc 1 的裸 traceback。audit 与后续变异轮是顺序关系，
    # 锁先释放再由 mutating_sources 重取，无同进程嵌套。
    try:
        with tree_lock(root, timeout=60):
            for _, path, old, new, _, _ in rows:
                mutate(path.read_text(), None, [(old, new)])
    except (TimeoutError, OSError, UnicodeDecodeError) as error:
        print(f'--audit tree lock or read failed: '
              f'{type(error).__name__}: {error}', file=sys.stderr)
        return 2
    return 0


def compile_controls(rows, out, env):
    results = []
    try:
        with mutating_sources(root, list(dict.fromkeys(row[1] for row in rows))) as originals:
            for name, path, old, new, _, _ in rows:
                result = {'mutation': name, 'compiled': False, 'status': 'INVALID', 'reason': ''}
                try:
                    path.write_text(mutate(originals[path].decode(), None, [(old, new)]))
                    slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=600)
                    (out / f'{name}-compile.slots.log').write_text(slot.stdout + slot.stderr)
                    run = run_owned(['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
                                     '-p', 'pilotdeck-gateway', '--lib', '--no-run'],
                                    cwd=root, env=env, timeout=600)
                    (out / f'{name}-compile.log').write_text(run.stdout + run.stderr)
                    result.update(compiled=run.returncode == 0, returncode=run.returncode,
                                  status='COMPILED' if run.returncode == 0 else 'INVALID')
                except Exception as error:
                    result['reason'] = f'{type(error).__name__}: {error}'
                finally:
                    path.write_bytes(originals[path])
                results.append(result)
    except Exception as error:
        reason = f'source guard: {type(error).__name__}: {error}'
        invalidate_completed(results, reason)
        for name, *_ in rows[len(results):]:
            results.append({'mutation': name, 'compiled': False, 'status': 'INVALID',
                            'reason': reason})
    (out / 'compile-summary.json').write_text(json.dumps(results, indent=2) + '\n')
    for result in results:
        print(json.dumps(result), flush=True)
    return all(result['compiled'] and result['status'] == 'COMPILED' for result in results)


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
    if audit(rows):  # Stale anchors are errors, never survivors.
        return 2
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
    bound_cases = [Case(name, path, old, new, test, assertion)
                   for name, path, old, new, test, assertion in rows]
    status = run_cases(root, bound_cases, out, env, mutate, timeout=600)
    return int(not compiled or status != 0)


if __name__ == '__main__':
    raise SystemExit(main())
