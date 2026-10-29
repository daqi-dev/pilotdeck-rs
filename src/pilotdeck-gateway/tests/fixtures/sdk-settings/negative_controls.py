#!/usr/bin/env python3
"""#1029 HEAD-only controls: --check | [--compile-only] NEW_OUTPUT_DIR [mutation ...].
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
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import ReceiptRun, judge, passed

root = Path(__file__).resolve().parents[5]
settings = root / 'src/pilotdeck-cli/src/sdk/settings.rs'
sdk = root / 'src/pilotdeck-cli/src/sdk.rs'
transport = root / 'src/pilotdeck-cli/src/sdk/transport.rs'
tests = root / 'src/pilotdeck-cli/tests/sdk_settings_exit.rs'
fixture = root / 'src/pilotdeck-cli/tests/fixtures/sdk-settings'
oracle = fixture / 'oracle.json'
source = fixture / 'source.json'
MATRIX = 'settings_matches_ts_matrix'
LOCAL = 'settings_local_preflight_matches_ts_matrix'
REGENERATE = 'settings_oracle_regenerates_identically'
mutations = [
    ('settings-hello-client-name', transport, 'client_name: "sdk",', 'client_name: "cli",', 'settings_hello_matches_ts_bytes', '#1029 hello and request wire settings-default--argv'),
    ('settings-hello-token', transport, 'token: &connection.auth_token,', 'token: "wrong-token",', 'settings_hello_matches_ts_bytes', '#1029 hello and request wire settings-default--argv'),
    ('settings-hello-protocol', transport, 'protocol_version: pilotdeck_protocol::GATEWAY_PROTOCOL_VERSION,', 'protocol_version: "0.0",', 'settings_hello_matches_ts_bytes', '#1029 hello and request wire settings-default--argv'),
    ('request-placeholder-collision', tests, '.expect("#1029 expected wire JSON")\n                .replacen("\\"id\\":\\"<request>\\"", &format!("\\"id\\":\\"{id}\\""), 1)', '.expect("#1029 expected wire JSON")\n                .replace("<request>", &id)', 'settings_request_preserves_placeholder_literals', '#1029 hello and request wire settings-extra--set-request-literal'),
    ('response-placeholder-collision', tests, '.expect("#1029 original wire JSON")\n                .replacen("\\"id\\":\\"<request>\\"", &format!("\\"id\\":\\"{id}\\""), 1)', '.expect("#1029 original wire JSON")\n                .replace("<request>", &id)', 'settings_response_preserves_placeholder_literals', '#1029 TS output and exit settings-extra--get-request-literal'),
    ('settings-hello-order', transport, "    #[serde(rename = \"type\")]\n    kind: &'a str,\n    protocol_version: &'a str,", "    protocol_version: &'a str,\n    #[serde(rename = \"type\")]\n    kind: &'a str,", 'settings_hello_matches_ts_bytes', '#1029 hello and request wire settings-default--argv'),
    ('settings-request-order', transport, '    #[serde(rename = "type")]\n    kind: &\'a str,\n    #[serde(rename = "id")]\n    request_id: &\'a str,', '    #[serde(rename = "id")]\n    request_id: &\'a str,\n    #[serde(rename = "type")]\n    kind: &\'a str,', 'settings_request_matches_ts_bytes', '#1029 hello and request wire settings-set--argv'),
    ('settings-json-escaping', settings, 'RawValue::from_string(encoded)', 'RawValue::from_string(encoded.replace("zh-CN", r"\\u007ah-CN"))', 'settings_request_matches_ts_bytes', '#1029 hello and request wire settings-set--argv'),
    ('dispatch', sdk, 'Some("settings") => run_settings(&argv[1..], env, cwd),', 'Some("setting") => run_settings(&argv[1..], env, cwd),', MATRIX, '#1029 expected gateway lifecycle settings-default--argv'),
    ('default', sdk, 'map_or("get", String::as_str)', 'map_or("unknown", String::as_str)', MATRIX, '#1029 expected gateway lifecycle settings-default--argv'),
    ('get-method', settings, '"resolve_settings",', '"wrong_settings",', MATRIX, '#1029 hello and request wire settings-default--argv'),
    ('set-method', settings, '"update_settings",', '"wrong_settings",', MATRIX, '#1029 hello and request wire settings-set--argv'),
    ('json-precedence', sdk, 'read_value(rest, "--json")', 'read_value(rest, "--wrong-json")', MATRIX, '#1029 expected gateway lifecycle settings-set--json-flag-precedence'),
    ('positional-join', sdk, 'positional(rest).join(" ")', 'positional(rest).join("")', LOCAL, '#1029 TS output and exit settings-set--missing-h'),
    ('empty-json', sdk, 'if raw.is_empty() {', 'if false && raw.is_empty() {', LOCAL, '#1029 TS output and exit settings-set--stdin-text'),
    ('syntax-message', sdk, 'Invalid settings JSON: {message}', 'Bad settings JSON: {message}', LOCAL, '#1029 TS output and exit settings-set--missing-h'),
    ('null-validation', settings, '!matches!(settings, JsValue::Object(_))', '!matches!(settings, JsValue::Object(_) | JsValue::Null)', LOCAL, '#1029 preflight never connects settings-set--json-value-569'),
    ('array-validation', settings, '!matches!(settings, JsValue::Object(_))', '!matches!(settings, JsValue::Object(_) | JsValue::Array(_))', LOCAL, '#1029 preflight never connects settings-set--json-value-570'),
    ('scalar-validation', settings, '!matches!(settings, JsValue::Object(_))', '!matches!(settings, JsValue::Object(_) | JsValue::Number(_))', LOCAL, '#1029 preflight never connects settings-set--json-value-571'),
    ('object-message', settings, 'updateSettings settings must be an object.', 'settings must be an object.', LOCAL, '#1029 TS output and exit settings-set--json-value-569'),
    ('source', settings, 'localSettings', 'wrongSource', MATRIX, '#1029 hello and request wire settings-set--argv'),
    ('whole-object', settings, 'pub(super) fn set(connection: &Connection, settings: Box<RawValue>) -> Result<u8, Failure> {', 'pub(super) fn set(connection: &Connection, settings: Box<RawValue>) -> Result<u8, Failure> { let _ = settings; let settings = RawValue::from_string("{}".into()).map_err(|e| Failure::Error(e.to_string()))?;', MATRIX, '#1029 hello and request wire settings-set--argv'),
    ('json-rounding', settings, 'RawValue::from_string(encoded)', 'RawValue::from_string({ let _ = encoded; raw.to_owned() })', MATRIX, '#1029 hello and request wire settings-extra--set-js-numbers'),
    ('empty-url-gate', sdk, '        "get" => {\n            create_client(&connection)?;', '        "get" => {\n            if false { create_client(&connection)?; }', LOCAL, '#1029 TS output and exit settings-default--gateway-empty'),
    ('full-result', settings, 'stringify_json(&result)', 'stringify_json(result.get("settings").unwrap_or(&JsValue::Null))', MATRIX, '#1029 TS output and exit settings-default--argv'),
    ('undefined-output', settings, 'unwrap_or_else(|| "undefined".into())', 'unwrap_or_else(|| "null".into())', MATRIX, '#1029 TS output and exit settings-extra--get-missing'),
    ('pretty-output', settings, 'stdout_line(&json_pretty(&raw));', 'stdout_line(&if false { json_pretty(&raw) } else { raw.clone() });', MATRIX, '#1029 TS output and exit settings-default--argv'),
    ('fixed-ack', settings, 'stdout_line("{\\"updated\\":true}");', 'stdout_line("{\\"updated\\":false}");', MATRIX, '#1029 TS output and exit settings-set--argv'),
    ('no-local-write', settings, 'stdout_line("{\\"updated\\":true}");', 'std::fs::write(std::path::Path::new(&std::env::var("PILOT_HOME").unwrap_or_default()).join("settings.json"), b"{}").map_err(|e| Failure::Error(e.to_string()))?; stdout_line("{\\"updated\\":true}");', MATRIX, '#1029 complete disk unchanged settings-set--argv'),
    ('settings-client-version', settings, 'connection, method, params, "0.1.0",', 'connection, method, params, "pilotdeck-cli",', MATRIX, '#1029 hello and request wire settings-default--argv'),
    ('rawvalue-surrogate', transport, 'SettingsRequestFrame {\n                        kind: "request",\n                        request_id: &id,\n                        method,\n                        params,\n                    },', 'SettingsRequestFrame {\n                        kind: "request",\n                        request_id: &id,\n                        method,\n                        params: serde_json::to_value(params).map_err(|e| error(&e.to_string()))?,\n                    },', 'settings_raw_params_preserve_lone_surrogates', '#1029 expected text request frame settings-extra--set-surrogate'),
    ('rawvalue-depth', transport, 'SettingsRequestFrame {\n                        kind: "request",\n                        request_id: &id,\n                        method,\n                        params,\n                    },', 'SettingsRequestFrame {\n                        kind: "request",\n                        request_id: &id,\n                        method,\n                        params: serde_json::to_value(params).map_err(|e| error(&e.to_string()))?,\n                    },', 'settings_raw_params_preserve_deep_object', '#1029 expected text request frame settings-extra--set-deep-object'),
    ('selected-cell-coverage', tests, '.count()\n                == 1', '.count()\n                == 2', 'settings_raw_params_preserve_deep_object', '#1029 exactly one selected TS cell settings-extra--set-deep-object'),
    ('coverage', tests, 'cases.len() == 193', 'cases.len() == 192', MATRIX, '#1029 157 registered cells plus 36 live settings boundary cells'),
    ('oracle-drift', oracle, '"nodeVersion": "v22.22.0"', '"nodeVersion": "v0.0.0"', REGENERATE, '#1029 identical TS fixture sdk-settings'),
    ('source-drift', source, '"raw/node-version.stdout.bin": "v22.22.0\\n"', '"raw/node-version.stdout.bin": "v0.0.0\\n"', REGENERATE, '#1029 identical TS fixture sdk-settings'),
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
        print('\n'.join(failures) if failures else f'#1029 {len(mutations)} anchors checked')
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
    command = ['cargo','test','--offline','--manifest-path','src/Cargo.toml','-p','pilotdeck-cli','--test','sdk_settings_exit']

    def run(label, test, run_env, compile=False):
        slots = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=600, interval=15)
        (out / f'{label}.slots.log').write_text(slots.stdout + slots.stderr)
        argv = command + (['--no-run'] if compile else [test,'--','--exact','--include-ignored','--show-output','--test-threads=1'])
        result = run_owned(argv,cwd=root,env=run_env,timeout=900)
        (out / f'{label}.stdout').write_text(result.stdout)
        (out / f'{label}.stderr').write_text(result.stderr)
        return result

    guarded = [*sorted((root / 'src/pilotdeck-cli/src').rglob('*.rs')),
               root / 'src/pilotdeck-cli/Cargo.toml', root / 'src/Cargo.lock', Path(__file__).resolve(),
               tests, *sorted(fixture.glob('*.*')),
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
