#!/usr/bin/env python3
"""#983 HEAD-only CLI controls. Every mutant compiles before its bound test.

Usage: --check | [--compile-only] NEW_OUTPUT_DIR [mutation ...]
Requires caller CARGO_TARGET_DIR. New/uncommitted source deliberately refuses.
"""
import difflib
import json
import os
from pathlib import Path
import sys

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import ReceiptRun, judge

root = Path(__file__).resolve().parents[5]
transport = root / 'src/pilotdeck-cli/src/sdk/transport.rs'
sessions = root / 'src/pilotdeck-cli/src/sdk/sessions.rs'
sdk = root / 'src/pilotdeck-cli/src/sdk.rs'
MATRIX = 'sessions_list_matches_ts_matrix'
LIFECYCLE = 'sessions_list_transport_boundaries'
oracle = root / 'src/pilotdeck-cli/tests/fixtures/sdk-sessions-list/oracle.json'
mutations = [
    ('hello-version', transport, '"protocolVersion": "1.2"', '"protocolVersion": "0.0"', MATRIX, '#983 hello and request wire sessions-default--argv'),
    ('hello-client-name', transport, '"clientName": "sdk"', '"clientName": "cli"', MATRIX, '#983 hello and request wire sessions-default--argv'),
    ('hello-client-version', transport, '"clientVersion": "pilotdeck-cli"', '"clientVersion": "wrong-cli"', MATRIX, '#983 hello and request wire sessions-default--argv'),
    ('hello-token', transport, '"token": connection.auth_token', '"token": "wrong-token"', MATRIX, '#983 hello and request wire sessions-default--argv'),
    ('rpc-method', sessions, '"list_sessions", params', '"wrong_method", params', MATRIX, '#983 hello and request wire sessions-default--argv'),
    ('project-param', sessions, 'params["projectKey"] = project.into();', 'params["wrongProject"] = project.into();', MATRIX, '#983 hello and request wire sessions-list--flag-repeat'),
    ('request-ok', transport, '== Some(true)', '== Some(false)', MATRIX, '#983 TS output and exit sessions-default--argv'),
    ('rpc-error-message', transport, 'string_field(value, "message")', 'string_field(value, "missingMessage")', MATRIX, '#983 TS output and exit sessions-default--gateway-rpc-error'),
    ('list-projection', sessions, '.get("sessions")', '.get("missingSessions")', MATRIX, '#983 TS output and exit sessions-default--argv'),
    ('list-default', sessions, 'map_or("list", String::as_str)', 'map_or("unknown", String::as_str)', MATRIX, '#983 expected gateway lifecycle sessions-default--argv'),
    ('pretty-indent', sessions, '&"  ".repeat(depth)', '&" ".repeat(depth)', MATRIX, '#983 TS output and exit sessions-default--argv'),
    ('close-handshake', transport, 'SinkExt::close(&mut socket)', 'socket.flush()', MATRIX, '#983 terminal connection closed sessions-default--argv'),
    ('unreachable-message', transport, 'error("Gateway connection failed.")', 'error("Gateway unreachable.")', MATRIX, '#983 TS output and exit sessions-default--gateway-unreachable'),
    ('auth-close-reason', transport, 'frame.reason.to_string()', 'format!("{frame:?}")', MATRIX, '#983 TS output and exit sessions-default--gateway-auth-rejected'),
    ('request-correlation', transport, '.as_deref() != Some(&id)', '.as_deref() == Some("")', LIFECYCLE, '#983 transport boundary output wrong-id-and-ping'),
    ('connect-and-hello-deadline', transport, 'CONNECT_TIMEOUT: Duration = Duration::from_secs(10)', 'CONNECT_TIMEOUT: Duration = Duration::from_secs(1)', LIFECYCLE, '#983 transport deadline duration upgrade-never'),
    ('rpc-deadline', transport, 'REQUEST_TIMEOUT: Duration = Duration::from_secs(30)', 'REQUEST_TIMEOUT: Duration = Duration::from_secs(1)', LIFECYCLE, '#983 transport deadline duration rpc-never'),
    ('close-fallback', transport, '"Gateway WebSocket closed."\n                } else', '"wrong close"\n                } else', LIFECYCLE, '#983 transport boundary output rpc-close'),
    ('invalid-json-error', transport, 'parse_json(&raw).map_err(|_| error("Invalid Gateway JSON frame."))', 'parse_json(&raw).map_err(|_| error("wrong JSON error"))', LIFECYCLE, '#983 transport boundary output rpc-invalid-json'),
    ('invalid-binary-error', transport, 'Some(Ok(_)) => return Err(error("Invalid Gateway JSON frame."))', 'Some(Ok(_)) => return Err(error("wrong binary error"))', LIFECYCLE, '#983 transport boundary output rpc-binary'),
    ('rpc-error-default', transport, 'unwrap_or("Gateway request failed.")', 'unwrap_or("wrong default error")', LIFECYCLE, '#983 transport boundary output error-default'),
    ('rpc-error-empty', transport, 'message.as_deref().unwrap_or("Gateway request failed.")', 'message.as_deref().filter(|s| !s.is_empty()).unwrap_or("Gateway request failed.")', LIFECYCLE, '#983 transport boundary output error-empty'),
    ('nullish-sessions', sessions, '!matches!(value, JsValue::Null | JsValue::Undefined)', '!matches!(value, JsValue::Undefined)', LIFECYCLE, '#983 transport boundary output null-sessions'),
    ('scalar-sessions', sessions, '.unwrap_or(&empty);', '.filter(|value| matches!(value, JsValue::Array(_))).unwrap_or(&empty);', LIFECYCLE, '#983 transport boundary output false-sessions'),
    ('null-result-error', sessions, 'if matches!(result, JsValue::Null | JsValue::Undefined)', 'if false && matches!(result, JsValue::Null | JsValue::Undefined)', LIFECYCLE, '#983 transport boundary output null-result'),
    ('pong-payload', transport, '.flush()', '.send(Message::Pong(vec![0].into()))', LIFECYCLE, '#983 exact pong wrong-id-and-ping'),
    ('codec-number-order', sessions, 'let raw = stringify_json(value)', 'let raw = stringify_json(&JsValue::Null)', MATRIX, '#983 TS output and exit sessions-default--argv'),
    ('original-extraction', oracle, '"nodeVersion": "v22.22.0"', '"nodeVersion": "v0.0.0"', 'sessions_list_oracle_regenerates_identically', '#983 identical extraction from original TS records'),
]


def check(row):
    count = row[1].read_text().count(row[2])
    if count != 1 or row[2] == row[3]:
        raise ValueError(f'{row[0]}: stale anchor count={count}, expected 1')


def main():
    args = sys.argv[1:]
    if args == ['--check']:
        failures = []
        for row in mutations:
            try:
                check(row)
            except Exception as error:
                failures.append(str(error))
        print('\n'.join(failures) if failures else f'#983 {len(mutations)} anchors checked')
        return int(bool(failures))
    compile_only = '--compile-only' in args
    args = [arg for arg in args if arg != '--compile-only']
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
    command = ['cargo','test','--offline','--manifest-path','src/Cargo.toml','-p','pilotdeck-cli','--test','sdk_sessions_list_exit']

    def run(label, test, run_env, compile=False):
        slots = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=600, interval=15)
        (out / f'{label}.slots.log').write_text(slots.stdout + slots.stderr)
        argv = command + (['--no-run'] if compile else [test,'--','--exact','--include-ignored','--show-output','--test-threads=1'])
        result = run_owned(argv,cwd=root,env=run_env,timeout=900)
        (out / f'{label}.stdout').write_text(result.stdout)
        (out / f'{label}.stderr').write_text(result.stderr)
        return result

    def passed(result, test):
        return (result.returncode == 0 and f'test {test} ... ok' in result.stdout
                and 'test result: ok. 1 passed; 0 failed;' in result.stdout)

    guarded = [*sorted((root / 'src/pilotdeck-cli/src').rglob('*.rs')),
               root / 'src/pilotdeck-cli/Cargo.toml', root / 'src/Cargo.lock', Path(__file__).resolve(),
               root / 'src/pilotdeck-cli/tests/sdk_sessions_list_exit.rs',
               oracle, root / 'src/pilotdeck-cli/tests/fixtures/sdk-sessions-list/generate.mjs',
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
                    path.write_bytes(originals[path])
                    if path.read_bytes() != originals[path]:
                        raise RuntimeError('restoration mismatch')
                if not compile_only and not passed(run(name+'-restored',test,env),test):
                    item.update(status='INVALID',reason=item['reason']+'; restored bound test failed')
        except Exception as error:
            item.update(status='INVALID',reason=f'{type(error).__name__}: {error}')
        results.append(item)
        print(f'{name}: {item["status"]}: {item["reason"]}',flush=True)
    counts = {status:sum(row['status']==status for row in results) for status in ['KILLED','SURVIVED','INVALID','COMPILED']}
    (out / 'summary.json').write_text(json.dumps(dict(controls=results,counts=counts),indent=2)+'\n')
    print(json.dumps(counts),flush=True)
    return int(any(row['status'] != ('COMPILED' if compile_only else 'KILLED') for row in results))

if __name__ == '__main__':
    sys.exit(main())
