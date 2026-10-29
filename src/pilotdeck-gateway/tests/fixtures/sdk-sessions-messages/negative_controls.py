#!/usr/bin/env python3
"""#1028 HEAD-only CLI controls. Every mutant compiles before its bound test.

Usage: --check | --node-runtime NEW_OUTPUT_DIR | [--compile-only] NEW_OUTPUT_DIR [mutation ...]
Requires caller CARGO_TARGET_DIR. New/uncommitted source deliberately refuses.
"""
import difflib
import json
import os
from pathlib import Path
import signal
import shutil
import sys

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import ReceiptRun, judge, passed

root = Path(__file__).resolve().parents[5]
sessions = root / 'src/pilotdeck-cli/src/sdk/sessions.rs'
fixture = root / 'src/pilotdeck-cli/tests/fixtures/sdk-sessions-messages'
oracle = fixture / 'oracle.json'
source = fixture / 'source.json'
generator = fixture / 'generate.mjs'
INTEGRITY = 'sessions_messages_extraction_rejects_unbound_evidence'
MATRIX = 'sessions_messages_matches_ts_matrix'
mutations = [
    ('fixture-cell-coverage', oracle, '    {\n      "input": {\n        "id": "sessions-messages--argv",\n        "leaf": "sessions-messages",\n        "axis": "argv",\n        "entries": [\n          "cli:pilotdeck/sessions/messages"\n        ],\n        "argv": [\n          "sessions",\n          "messages",\n          "d1-session"\n        ],\n        "stdinHex": "",\n        "env": {\n          "PILOTDECK_SDK_AUTH_TOKEN": "d1-fixture-not-a-secret"\n        },\n        "mockGateway": true,\n        "mode": "success",\n        "acceptToken": "d1-fixture-not-a-secret",\n        "timeoutSeconds": 12\n      },\n      "source": "raw/host-b10/cases/sessions-messages--argv",\n      "sha256": {\n        "case.json": "6fa27d4cd29157f203a966be17c3b4b215f4c4192142cd7ec0f242c98d77a848",\n        "result.json": "643aabdb0df59d5d934ba1f0d84e84b98b1348f36542f7e2c0dc50955b65839a",\n        "stdout.bin": "258d1b01bdb1b24af3d0743b235909f8b920eda3ea68e5bb27fc606c9107720e",\n        "stderr.bin": "c41047869f7eca59f4145e3c0daec0f570aa16f8f930a4ca935b86366e5f4b2c",\n        "initial.json": "e509b48da2489daacd8760b333a906e91a07910a911fab27bcb541a404b92a9a",\n        "final.json": "e509b48da2489daacd8760b333a906e91a07910a911fab27bcb541a404b92a9a",\n        "events.json": "69c471af58ac38f851e30247128d48e96fc788d52554da49b8862197fcaa9058",\n        "mock.stdout.jsonl": "286b508750eff632ca64e9378bcd53586edd376e1447ade5286325cf27cdc14a"\n      },\n      "stdout": "[\\n  {\\n    \\"type\\": \\"user\\",\\n    \\"text\\": \\"fixture input\\",\\n    \\"id\\": \\"m1\\"\\n  },\\n  {\\n    \\"type\\": \\"assistant.message\\",\\n    \\"text\\": \\"fixture output\\",\\n    \\"id\\": \\"m2\\"\\n  }\\n]\\n",\n      "stderrRaw": "(node:54575) ExperimentalWarning: SQLite is an experimental feature and might change at any time\\n(Use `node --trace-warnings ...` to show where the warning was created)\\n",\n      "stderr": "",\n      "exit": 0,\n      "signal": null,\n      "connections": 1,\n      "wire": [\n        {\n          "kind": "receive",\n          "frame": {\n            "type": "hello",\n            "protocolVersion": "1.2",\n            "clientName": "sdk",\n            "clientVersion": "pilotdeck-cli",\n            "token": "d1-fixture-not-a-secret"\n          },\n          "raw": "{\\"type\\":\\"hello\\",\\"protocolVersion\\":\\"1.2\\",\\"clientName\\":\\"sdk\\",\\"clientVersion\\":\\"pilotdeck-cli\\",\\"token\\":\\"d1-fixture-not-a-secret\\"}"\n        },\n        {\n          "kind": "send",\n          "frame": {\n            "type": "hello_ok",\n            "protocolVersion": "1.2",\n            "serverVersion": "0.1.0-mock",\n            "serverInfo": {\n              "mode": "remote",\n              "projectKey": "d1-project",\n              "sessionCount": 1,\n              "capabilities": []\n            }\n          },\n          "raw": "{\\"type\\":\\"hello_ok\\",\\"protocolVersion\\":\\"1.2\\",\\"serverVersion\\":\\"0.1.0-mock\\",\\"serverInfo\\":{\\"mode\\":\\"remote\\",\\"projectKey\\":\\"d1-project\\",\\"sessionCount\\":1,\\"capabilities\\":[]}}"\n        },\n        {\n          "kind": "receive",\n          "frame": {\n            "type": "request",\n            "id": "<request>",\n            "method": "read_session_messages",\n            "params": {\n              "sessionKey": "d1-session"\n            }\n          },\n          "raw": "{\\"type\\":\\"request\\",\\"id\\":\\"<request>\\",\\"method\\":\\"read_session_messages\\",\\"params\\":{\\"sessionKey\\":\\"d1-session\\"}}"\n        },\n        {\n          "kind": "send",\n          "frame": {\n            "type": "response",\n            "id": "<request>",\n            "ok": true,\n            "result": {\n              "messages": [\n                {\n                  "type": "user",\n                  "text": "fixture input",\n                  "id": "m1"\n                },\n                {\n                  "type": "assistant.message",\n                  "text": "fixture output",\n                  "id": "m2"\n                }\n              ]\n            }\n          },\n          "raw": "{\\"type\\":\\"response\\",\\"id\\":\\"<request>\\",\\"ok\\":true,\\"result\\":{\\"messages\\":[{\\"type\\":\\"user\\",\\"text\\":\\"fixture input\\",\\"id\\":\\"m1\\"},{\\"type\\":\\"assistant.message\\",\\"text\\":\\"fixture output\\",\\"id\\":\\"m2\\"}]}}"\n        }\n      ],\n      "close": [\n        {\n          "code": 1005,\n          "reason": ""\n        }\n      ],\n      "disk": {\n        ".": {\n          "type": "directory",\n          "mode": 448\n        },\n        "bin": {\n          "mode": 448,\n          "type": "directory"\n        },\n        "home": {\n          "mode": 448,\n          "type": "directory"\n        },\n        "openssl.cnf": {\n          "mode": 384,\n          "type": "file",\n          "size": 0,\n          "sha256": "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"\n        },\n        "pilot": {\n          "mode": 448,\n          "type": "directory"\n        },\n        "pilot/pilotdeck.yaml": {\n          "mode": 384,\n          "type": "file",\n          "size": 17,\n          "sha256": "ed15348afbcf3cc6b29b6c45a706ac1e7d627dddf3df014a10f560fe97ba3d04"\n        },\n        "project": {\n          "mode": 448,\n          "type": "directory"\n        },\n        "project/keep.txt": {\n          "mode": 384,\n          "type": "file",\n          "size": 13,\n          "sha256": "b68f092bcfb9c25c602bad1a6491f15f779c8af47eddf990cfa528e9feb429a3"\n        },\n        "tmp": {\n          "mode": 448,\n          "type": "directory"\n        },\n        "xdg-cache": {\n          "mode": 448,\n          "type": "directory"\n        },\n        "xdg-config": {\n          "mode": 448,\n          "type": "directory"\n        },\n        "xdg-data": {\n          "mode": 448,\n          "type": "directory"\n        },\n        "xdg-state": {\n          "mode": 448,\n          "type": "directory"\n        }\n      }\n    },\n', '', MATRIX, '#1028 39 registered messages cells plus 13 live shape and ID cells'),
    ('rpc-method', sessions, '"read_session_messages",', '"wrong_messages",', MATRIX, '#1028 hello and request wire sessions-messages--argv'),
    ('session-key', sessions, 'json!({"sessionKey": session_id})', 'json!({"wrongSession": session_id})', MATRIX, '#1028 hello and request wire sessions-messages--argv'),
    ('project-key', sessions, 'message_params["projectKey"] = project_key.into();', 'message_params["wrongProject"] = project_key.into();', MATRIX, '#1028 hello and request wire sessions-messages--flag-repeat'),
    ('first-id', sessions, 'ids.first().filter', 'ids.last().filter', MATRIX, '#1028 hello and request wire sessions-messages--missing-h'),
    ('id-whitespace', sessions, '!id.is_empty()', '!id.trim().is_empty()', MATRIX, '#1028 expected gateway lifecycle sessions-messages--id-whitespace'),
    ('missing-id-usage', sessions, 'Usage: pilotdeck sessions messages <session-id>', 'wrong messages usage', MATRIX, '#1028 TS output and exit sessions-messages--missing-session'),
    ('messages-field', sessions, 'result.get("messages")', 'result.get("missingMessages")', MATRIX, '#1028 TS output and exit sessions-messages--argv'),
    ('null-messages', sessions, 'None | Some(JsValue::Null | JsValue::Undefined) => &empty_messages,', 'None | Some(JsValue::Undefined) => &empty_messages,', MATRIX, '#1028 TS output and exit sessions-messages--messages-null'),
    ('missing-messages', sessions, 'None | Some(JsValue::Null | JsValue::Undefined) => &empty_messages,', 'None => &JsValue::Null, Some(JsValue::Null | JsValue::Undefined) => &empty_messages,', MATRIX, '#1028 TS output and exit sessions-messages--messages-missing'),
    ('scalar-messages', sessions, 'Some(messages) => messages,', 'Some(messages) => if matches!(messages, JsValue::Array(_)) { messages } else { &empty_messages },', MATRIX, '#1028 TS output and exit sessions-messages--messages-false'),
    ('full-message-shape', sessions, 'Some(messages) => messages,', 'Some(messages) => if let JsValue::Array(items) = messages { items.first().unwrap_or(&empty_messages) } else { messages },', MATRIX, '#1028 TS output and exit sessions-messages--argv'),
    ('null-result-kind', sessions, 'JsValue::Null => Some("null"),', 'JsValue::Null => Some("undefined"),', MATRIX, '#1028 TS output and exit sessions-messages--result-null'),
    ('missing-result-kind', sessions, 'JsValue::Undefined => Some("undefined"),', 'JsValue::Undefined => Some("null"),', MATRIX, '#1028 TS output and exit sessions-messages--result-missing'),
    ('result-error-property', sessions, "(reading 'messages')", "(reading 'sessions')", MATRIX, '#1028 TS output and exit sessions-messages--result-null'),
    ('messages-success-exit', sessions, 'stdout_line(&pretty_json(messages)?);\n            Ok(0)', 'stdout_line(&pretty_json(messages)?);\n            Ok(1)', MATRIX, '#1028 TS output and exit sessions-messages--argv'),
    ('original-extraction', oracle, '"nodeVersion": "v22.22.0"', '"nodeVersion": "v0.0.0"', 'sessions_messages_oracle_regenerates_identically', '#1028 identical TS fixture sdk-sessions-messages'),
    ('source-extraction', source, '"raw/host-b10/cases/sessions-messages--argv/stdout.bin": "[', '"raw/host-b10/cases/sessions-messages--argv/stdout.bin": "Z', 'sessions_messages_oracle_regenerates_identically', '#1028 identical TS fixture sdk-sessions-messages'),
    ('provenance-entry', generator, "  'dist/src/cli/pilotdeck.js',", '', INTEGRITY, '#1028 extraction rejects missing provenance dist/src/cli/pilotdeck.js'),
    ('provenance-cli', generator, "  'dist/src/cli/sdkCli.js',", '', INTEGRITY, '#1028 extraction rejects missing provenance dist/src/cli/sdkCli.js'),
    ('provenance-sdk-index', generator, "  'packages/sdk/dist/index.js',", '', INTEGRITY, '#1028 extraction rejects missing provenance packages/sdk/dist/index.js'),
    ('provenance-sdk-client', generator, "  'packages/sdk/dist/client.js',", '', INTEGRITY, '#1028 extraction rejects missing provenance packages/sdk/dist/client.js'),
    ('events-stdout', generator, "[['stdout', stdout], ['stderr', stderr]]", "[['stderr', stderr]]", INTEGRITY, '#1028 extraction rejects host stdout events'),
    ('events-stderr', generator, "[['stdout', stdout], ['stderr', stderr]]", "[['stdout', stdout]]", INTEGRITY, '#1028 extraction rejects host stderr events'),
    ('events-host', generator, "  verifyEvents(JSON.parse(load('events.json')), stdout, stderr, input.id);", '  // Host event validation removed.', INTEGRITY, '#1028 extraction rejects host stdout events'),
    ('events-supplemental', generator, '  verifyEvents(item.events, Buffer.from(item.stdout), Buffer.from(item.stderrRaw), item.input.id);', '  // Supplemental event validation removed.', INTEGRITY, '#1028 extraction rejects supplemental stdout events'),
]


def check(row):
    count = row[1].read_text().count(row[2])
    if count != 1 or row[2] == row[3]:
        raise ValueError(f'{row[0]}: stale anchor count={count}, expected 1')


def node_runtime_controls(out, env, run, passed):
    """Environment mutants: real extraction, no edits to uncommitted sources."""
    node = env['NODE22'] if 'NODE22' in env else shutil.which('node', path=env.get('PATH'))
    if not node:
        raise RuntimeError('#1028 Node v22.22.0 must be available through NODE22 or PATH')
    node = shutil.which(node, path=env.get('PATH')) or node
    version = run_owned([node, '--version'], env=env, timeout=30)
    (out / 'real-node.stdout').write_text(version.stdout)
    (out / 'real-node.stderr').write_text(version.stderr)
    if version.returncode != 0 or version.stdout != 'v22.22.0\n':
        raise RuntimeError('#1028 runtime controls require real Node v22.22.0')
    test = 'sessions_messages_oracle_regenerates_identically'
    rows = [
        ('path-version', False, 'v22.22.1', 0,
         '#1028 selected Node must report v22.22.0 before extraction'),
        ('override-version', True, 'v22.22.1', 0,
         '#1028 selected Node must report v22.22.0 before extraction'),
        ('version-exit', True, 'v22.22.0', 23,
         '#1028 selected Node --version must exit successfully'),
    ]

    def environment(label, override, reported, code):
        directory = out / label
        directory.mkdir()
        marker = directory / 'generator.called'

        def wrapper(path, reported, code):
            # Only --version is varied. Generation always executes real Node.
            path.write_text(f'#!{sys.executable}\n' +
                'import os, sys\nfrom pathlib import Path\n' +
                f'if sys.argv[1:] == ["--version"]:\n    print({reported!r})\n    sys.exit({code})\n' +
                f'marker = Path({str(marker)!r})\n' +
                'temporary = marker.with_suffix(".tmp")\n' +
                'temporary.write_text("generator invoked\\n")\ntemporary.replace(marker)\n' +
                f'os.execv({node!r}, [{node!r}, *sys.argv[1:]])\n')
            path.chmod(0o700)

        wrapper(directory / 'node', 'v0.0.0' if override else reported,
                0 if override else code)
        changed = dict(env)
        changed['PATH'] = str(directory) + os.pathsep + env.get('PATH', '')
        changed.pop('NODE22', None)
        if override:
            wrapper(directory / 'explicit-node', reported, code)
            changed['NODE22'] = str(directory / 'explicit-node')
        return changed, marker

    results = []
    for name, override, reported, code, assertion in rows:
        item = dict(name=name, test=test, assertion=assertion, status='INVALID',
                    reason='not run', compiled=False, restoredPass=False)
        try:
            positive, marker = environment(name + '-positive-env', override, 'v22.22.0', 0)
            if not passed(run(name + '-positive', test, positive), test) or not marker.is_file():
                raise RuntimeError('positive bound test did not extract through selected Node')
            negative, marker = environment(name + '-negative-env', override, reported, code)
            if run(name + '-compile', test, negative, compile=True).returncode != 0:
                raise RuntimeError('environment mutant compile failed; not evidence')
            item['compiled'] = True
            receipt = ReceiptRun(out, name + '-negative', [(test, assertion)])
            result = run(name + '-negative', test, receipt.environment(negative))
            killed, reason, _ = judge(result, '', [(test, assertion)], receipt)
            item.update(status='KILLED' if killed else ('SURVIVED' if passed(result, test) else 'INVALID'), reason=reason)
            if marker.exists():
                item.update(status='INVALID', reason='generator ran before Node validation completed')
        except Exception as error:
            item.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
        try:
            restored, marker = environment(name + '-restored-env', override, 'v22.22.0', 0)
            item['restoredPass'] = passed(run(name + '-restored', test, restored), test) and marker.is_file()
            if not item['restoredPass']:
                raise RuntimeError('restored bound test did not extract through selected Node')
        except Exception as error:
            item.update(status='INVALID', reason=item['reason'] + f'; restored: {error}')
        results.append(item)
        print(f'{name}: {item["status"]}: {item["reason"]}', flush=True)
    counts = {status: sum(row['status'] == status for row in results)
              for status in ['KILLED', 'SURVIVED', 'INVALID']}
    (out / 'summary.json').write_text(json.dumps(dict(controls=results, counts=counts), indent=2) + '\n')
    print(json.dumps(counts), flush=True)
    return int(any(row['status'] != 'KILLED' for row in results))


def main():
    signal.signal(signal.SIGINT, signal.SIG_DFL)
    args = sys.argv[1:]
    if args == ['--check']:
        failures = []
        for row in mutations:
            try:
                check(row)
            except Exception as error:
                failures.append(str(error))
        print('\n'.join(failures) if failures else f'#1028 {len(mutations)} anchors checked')
        return int(bool(failures))
    compile_only = '--compile-only' in args
    node_runtime = '--node-runtime' in args
    args = [arg for arg in args if arg not in ('--compile-only', '--node-runtime')]
    if not args:
        raise SystemExit('usage: --check | [--compile-only] NEW_OUTPUT_DIR [mutation ...]')
    selected = [row for row in mutations if not args[1:] or row[0] in args[1:]]
    if not selected or (args[1:] and len(selected) != len(args[1:])):
        raise SystemExit('unknown/duplicate mutation selection')
    if node_runtime and (compile_only or len(args) != 1):
        raise SystemExit('--node-runtime requires exactly one new output directory')
    env = dict(os.environ)
    if not env.get('CARGO_TARGET_DIR'):
        raise SystemExit('CARGO_TARGET_DIR must be supplied by the caller')
    env.pop(ReceiptRun.ENV, None)
    env.pop(ReceiptRun.PARENT_ENV, None)
    out = Path(args[0]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    command = ['cargo','test','--offline','--manifest-path','src/Cargo.toml','-p','pilotdeck-cli','--test','sdk_sessions_messages_exit']

    def run(label, test, run_env, compile=False):
        slots = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=600, interval=15)
        (out / f'{label}.slots.log').write_text(slots.stdout + slots.stderr)
        argv = command + (['--no-run'] if compile else [test,'--','--exact','--include-ignored','--show-output','--test-threads=1'])
        result = run_owned(argv,cwd=root,env=run_env,timeout=900)
        (out / f'{label}.stdout').write_text(result.stdout)
        (out / f'{label}.stderr').write_text(result.stderr)
        return result

    if node_runtime:
        return node_runtime_controls(out, env, run, passed)

    guarded = [*sorted((root / 'src/pilotdeck-cli/src').rglob('*.rs')),
               root / 'src/pilotdeck-cli/Cargo.toml', root / 'src/Cargo.lock', Path(__file__).resolve(),
               root / 'src/pilotdeck-cli/tests/sdk_sessions_messages_exit.rs',
               oracle, fixture / 'generate.mjs', fixture / 'probe.mjs', source,
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
