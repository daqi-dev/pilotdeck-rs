#!/usr/bin/env python3
"""#1086 HEAD-only controls: --check | [--compile-only] NEW_OUTPUT_DIR [mutation ...].
Requires caller CARGO_TARGET_DIR; refuses uncommitted/new sources. Every mutant
compiles, gets a nonce-bound verdict, restores, and reruns its exact positive.
"""
import difflib
import json
import os
from pathlib import Path
import sys

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot, write_source_atomic, tree_lock

from negative_suite import ReceiptRun, judge, passed

root = Path(__file__).resolve().parents[5]
turn = root / 'src/pilotdeck-cli/src/sdk/turn.rs'
output = root / 'src/pilotdeck-cli/src/sdk/output.rs'
transport = root / 'src/pilotdeck-cli/src/sdk/transport.rs'
tests = root / 'src/pilotdeck-cli/tests/sdk_resume_exit.rs'
common = root / 'src/pilotdeck-cli/tests/common/sdk_exit.rs'
sdk = root / 'src/pilotdeck-cli/src/sdk.rs'
fixture = root / 'src/pilotdeck-cli/tests/fixtures/sdk-terminal'
oracle = fixture / 'oracle.json'
source = fixture / 'source.json'
mutations = [
    ('predecessor-early-timeout', transport, 'const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);', 'const REQUEST_TIMEOUT: Duration = Duration::from_secs(1);', 'predecessor_never_is_an_observation_timeout', '#1086 predecessor stays pending until observation ends'),
    ('failed-as-completed', output, 'Terminal::Failed(error) => {', 'Terminal::Failed(error) => { if true { return Ok(0); }', 'resume_matches_ts_matrix', '#1086 output bytes and exit'),
    ('unknown-as-completed', output, 'Terminal::Unknown(run_id) => {', 'Terminal::Unknown(run_id) => { if true { return Ok(0); }', 'resume_matches_ts_matrix', '#1086 output bytes and exit'),
    ('missing-terminal-as-completed', output, '.unwrap_or_else(|| Terminal::Unknown(self.run_id.clone()))', '.unwrap_or_else(|| Terminal::Completed(None))', 'resume_matches_ts_matrix', '#1086 output bytes and exit'),
    ('completed-overwrites-error', output, 'self.terminal.is_some()\n            && (raw_type.equals_str("turn_completed") || raw_type.equals_str("error"))', 'self.terminal.is_some() && raw_type.equals_str("error")', 'resume_matches_ts_matrix', '#1086 output bytes and exit'),
    ('error-overwrites-completed', output, 'self.terminal.is_some()\n            && (raw_type.equals_str("turn_completed") || raw_type.equals_str("error"))', 'self.terminal.is_some() && raw_type.equals_str("turn_completed")', 'resume_matches_ts_matrix', '#1086 output bytes and exit'),
    ('unknown-code-failed', output, 'code.equals_str("result_unknown")', 'code.equals_str("disabled_result_unknown")', 'resume_matches_ts_matrix', '#1086 output bytes and exit'),
    ('unknown-snapshot', output, 'Terminal::Unknown(run_id) => {', 'Terminal::Unknown(run_id) => { let run_id = { let _ = run_id; self.run_id.clone() };', 'resume_matches_ts_matrix', '#1086 output bytes and exit'),
    ('public-runid', output, 'raw_type.equals_str("turn_started") || raw_type.equals_str("turn.started")', 'raw_type.equals_str("turn_started") || raw_type.equals_str("turn.started_DISABLED")', 'resume_matches_ts_matrix', '#1086 output bytes and exit'),
    ('runid-utf16', output, 'self.run_id = run_id.clone();', 'self.run_id = String::from_utf16_lossy(run_id.units()).into();', 'resume_matches_ts_matrix', '#1086 output bytes and exit'),
    ('structured-payload', output, 'self.structured = event.get("payload").cloned();', 'self.structured = None;', 'resume_matches_ts_matrix', '#1086 output bytes and exit'),
    ('structured-nullish', output, '.structured\n                .as_ref()\n                .filter(|value| !matches!(value, JsValue::Null | JsValue::Undefined))', '.structured\n                .as_ref()\n                .filter(|_value| true)', 'resume_matches_ts_matrix', '#1086 output bytes and exit'),
    ('details-arrays', output, '.get("detail")\n                    .filter(|value| matches!(value, JsValue::Object(_) | JsValue::Array(_)))', '.get("detail")\n                    .filter(|value| matches!(value, JsValue::Object(_)))', 'resume_matches_ts_matrix', '#1086 output bytes and exit'),
    ('details-scalars', output, '.get("detail")\n                    .filter(|value| matches!(value, JsValue::Object(_) | JsValue::Array(_)))', '.get("detail")\n                    .filter(|value| !matches!(value, JsValue::Undefined))', 'resume_matches_ts_matrix', '#1086 output bytes and exit'),
    ('error-message-conversion', output, 'js_string(message)?;', 'let _ = message;', 'resume_matches_ts_matrix', '#1086 output bytes and exit'),
    ('error-object-conversion', output, 'if object.get("toString").is_some() {', 'if false && object.get("toString").is_some() {', 'resume_matches_ts_matrix', '#1086 output bytes and exit'),
    ('error-array-conversion', output, 'if index > 0 {', 'if index > 1 {', 'resume_matches_ts_matrix', '#1086 output bytes and exit'),
    ('error-default-code', output, '"server_error".into()', '"wrong_code".into()', 'resume_matches_ts_matrix', '#1086 output bytes and exit'),
    ('error-json-order', output, '("code", JsValue::String(code)),\n                    ("details", details),', '("details", details),\n                    ("code", JsValue::String(code)),', 'resume_matches_ts_matrix', '#1086 output bytes and exit'),
    ('resume-control-method', turn, '            control\n                .request("resume_session", json!({"sessionKey": session}))', '            control\n                .request("new_session", json!({"sessionKey": session}))', 'resume_matches_ts_matrix', '#1086 complete two-connection wire and cleanup'),
    ('resume-original-session', turn, '            session.to_owned()', '            "wrong-session".to_owned()', 'resume_matches_ts_matrix', '#1086 complete two-connection wire and cleanup'),
    ('resume-prompt-offset', turn, 'usize::from(resume.is_some())', '0', 'resume_matches_ts_matrix', '#1086 complete two-connection wire and cleanup'),
    ('resume-query-method', turn, '    query\n        .request("resume_session", json!({"sessionKey": session}))', '    query\n        .request("wrong_resume", json!({"sessionKey": session}))', 'resume_matches_ts_matrix', '#1086 complete two-connection wire and cleanup'),
    ('resume-placeholder', sdk, 'turn::run(argv, &connection, Some(session))', 'Err(Failure::Todo("resume regression"))', 'resume_token_reaches_gateway', '#1086 output bytes and exit'),
    ('oracle-empty', oracle, '  "cases": [', '  "cases": [],\n  "discardedCases": [', 'resume_matches_ts_matrix', '#1086 registered 143 cells'),
    ('oracle-drift', oracle, '"nodeVersion": "v22.22.0"', '"nodeVersion": "v0.0.0"', 'terminal_oracle_regenerates_identically', '#1086 identical TS fixture sdk-terminal'),
    ('fixture-stdin-skip', common, 'match stdin.write_all(data) {', 'match { let _ = (stdin, data); Ok::<(), io::Error>(()) } {', 'fixture_stdin_writes_all_bytes', '#1075 complete stdin write'),
    ('fixture-stdin-other-error', common, 'Err(error) => panic!("#1075 fixture stdin: {error:?}"),', 'Err(_error) => {},', 'fixture_stdin_other_errors_still_fail', '#1075 other stdin errors must fail'),
    ('fixture-stdin-broken-pipe', common, 'Err(error) if error.kind() == io::ErrorKind::BrokenPipe => {}', 'Err(error) if false && error.kind() == io::ErrorKind::BrokenPipe => {}', 'fixture_stdin_broken_pipe_preserves_output', '#1075 broken pipe must not panic'),
]

def check(row):
    count = row[1].read_text().count(row[2])
    if count != 1 or row[2] == row[3]:
        raise ValueError(f'{row[0]}: stale anchor count={count}, expected 1')


def main():
    args = sys.argv[1:]
    if args == ['--check']:
        if not mutations:
            # #1364: an empty manifest must fail closed, not "0 anchors checked".
            raise SystemExit('#1086 anchors: empty mutation manifest; refusing to pass')
        # #2795 fix-r4（评审 sibling P1 同族跟改）：sdk 族互变异同一批源
        # （common/sdk_exit.rs、sdk/turn.rs、sdk/transport.rs 等），锚点核对要持
        # 树锁读——兄弟变异运行器在自己的 write 与 restore 之间可能把变异体
        # 留在盘上，无锁读会把核对错在它不拥有的源上；锁忙 / 读失败是拒跑
        # （2）。本窗口只包只读核对面，与后续 mutating_sources 是顺序关系，
        # 锁先释放再重取，无同进程嵌套。
        try:
            with tree_lock(root, timeout=60):
                failures = []
                for row in mutations:
                    try:
                        check(row)
                    except Exception as error:
                        failures.append(str(error))
        except (TimeoutError, OSError, UnicodeDecodeError) as error:
            print(f'tree lock or anchor read failed: '
                  f'{type(error).__name__}: {error}', file=sys.stderr)
            raise SystemExit(2)
        print('\n'.join(failures) if failures else f'#1086 {len(mutations)} anchors checked')
        return int(bool(failures))
    compile_only = '--compile-only' in args
    args = [arg for arg in args if arg not in ('--compile-only',)]
    if not args:
        raise SystemExit('usage: --check | [--compile-only] NEW_OUTPUT_DIR [mutation ...]')
    selected = [row for row in mutations if not args[1:] or row[0] in args[1:]]
    if not selected or (args[1:] and len(selected) != len(args[1:])):
        raise SystemExit('unknown/duplicate mutation selection')
    env = dict(os.environ)
    if not env.get('CARGO_TARGET_DIR'):
        raise SystemExit('CARGO_TARGET_DIR must be supplied by the caller')
    env.pop(ReceiptRun.ENV, None)
    env.pop(ReceiptRun.PARENT_ENV, None)
    out = Path(args[0]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    command = ['cargo','test','--offline','--manifest-path','src/Cargo.toml','-p','pilotdeck-cli']

    def run(label, test, run_env, compile=False):
        slots = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=600, interval=15)
        (out / f'{label}.slots.log').write_text(slots.stdout + slots.stderr)
        target = ['--bin','pilotdeck'] if test.startswith('sdk::') else ['--test','sdk_resume_exit']
        argv = command + target + (['--no-run'] if compile else [test,'--','--exact','--include-ignored','--show-output','--test-threads=1'])
        result = run_owned(argv,cwd=root,env=run_env,timeout=900)
        (out / f'{label}.stdout').write_text(result.stdout)
        (out / f'{label}.stderr').write_text(result.stderr)
        return result

    guarded = [*sorted((root / 'src/pilotdeck-cli/src').rglob('*.rs')),
               root / 'src/pilotdeck-cli/Cargo.toml', root / 'src/Cargo.lock', Path(__file__).resolve(),
               tests, *sorted(fixture.glob('*.*')),
               common,
               root / 'src/pilotdeck-cli/tests/common/negative_receipt.rs',
               Path(__file__).resolve().parents[1] / 'negative_process.py',
               Path(__file__).resolve().parents[1] / 'negative_suite.py']
    results = []
    for name,path,old,new,test,assertion in selected:
        item = dict(name=name,test=test,status='INVALID',reason='not run',compiled=False)
        try:
            check((name,path,old,new,test,assertion))
            with mutating_sources(root,guarded) as originals:
                original = originals[path].decode()
                try:
                    if not compile_only and not passed(run(name+'-positive',test,env),test):
                        raise RuntimeError('positive bound test failed')
                    changed = original.replace(old,new,1)
                    (out / f'{name}.patch').write_text(''.join(difflib.unified_diff(original.splitlines(True),changed.splitlines(True),fromfile=str(path.relative_to(root)),tofile=str(path.relative_to(root)))))
                    path.write_text(changed)
                    if run(name+'-compile',test,env,compile=True).returncode != 0:
                        raise RuntimeError('mutant compile failed; not evidence')
                    item['compiled'] = True
                    if compile_only:
                        item.update(status='COMPILED',reason='mutant no-run passed')
                    else:
                        receipt = ReceiptRun(out,name+'-negative',[(test,assertion)])
                        result = run(name+'-negative',test,receipt.environment(env))
                        killed,reason,_ = judge(result,'',[(test,assertion)],receipt)
                        item.update(status='KILLED' if killed else ('SURVIVED' if passed(result,test) else 'INVALID'),reason=reason)
                finally:
                    # Only restore bytes supplied by the official HEAD guard.
                    # #1903: one atomic rename, never a truncating write -- a
                    # SIGKILL mid-restore must not leave a half-written common
                    # module that fails every sibling suite's baseline.
                    write_source_atomic(root, path, originals[path])
                    if path.read_bytes() != originals[path]:
                        raise RuntimeError('restoration mismatch')
                if not compile_only and not passed(run(name+'-restored',test,env),test):
                    item.update(status='INVALID',reason=item['reason']+'; restored bound test failed')
        except Exception as error:
            item.update(status='INVALID',compiled=False,reason=f'{type(error).__name__}: {error}')
        results.append(item)
        print(f'{name}: {item["status"]}: {item["reason"]}',flush=True)
    counts = {status:sum(row['status']==status for row in results) for status in ['KILLED','SURVIVED','INVALID','COMPILED']}
    temporary = out / 'summary.tmp'
    temporary.write_text(json.dumps(dict(controls=results,counts=counts),indent=2)+'\n')
    temporary.replace(out / 'summary.json')
    print(json.dumps(counts),flush=True)
    return int(any(row['status'] != ('COMPILED' if compile_only else 'KILLED') for row in results))

if __name__ == '__main__':
    sys.exit(main())
