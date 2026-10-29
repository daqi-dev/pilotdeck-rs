#!/usr/bin/env python3
"""#1058 HEAD-only controls: --check | [--compile-only] NEW_OUTPUT_DIR [mutation ...].
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
turn = root / 'src/pilotdeck-cli/src/sdk/turn.rs'
output = root / 'src/pilotdeck-cli/src/sdk/output.rs'
transport = root / 'src/pilotdeck-cli/src/sdk/transport.rs'
tests = root / 'src/pilotdeck-cli/tests/sdk_run_exit.rs'
sdk = root / 'src/pilotdeck-cli/src/sdk.rs'
validation = root / 'src/pilotdeck-cli/tests/fixtures/sdk-run-validation'
fixture = root / 'src/pilotdeck-cli/tests/fixtures/sdk-run'
oracle = fixture / 'oracle.json'
source = fixture / 'source.json'
mutations = [
    ('model-oracle-empty', validation / 'oracle.json', '  "models": [', '  "models": [],\n  "discardedModels": [', 'sdk::turn::tests::model_resolution_matches_ts_boundaries', '#1058 registered model validation rows'),
    ('sequence-oracle-empty', validation / 'oracle.json', '  "sequences": [', '  "sequences": [],\n  "discardedSequences": [', 'sdk::transport::tests::stream_sequence_matches_ts_boundaries', '#1058 registered sequence validation rows'),
    ('sequence-received-format', transport, 'number_to_string(sequence)', 'sequence.to_string()', 'sdk::transport::tests::stream_sequence_matches_ts_boundaries', '#1058 stream sequence validation'),
    ('sequence-expected-format', transport, 'number_to_string(previous + 1.0)', '(previous + 1.0).to_string()', 'sdk::transport::tests::stream_sequence_matches_ts_boundaries', '#1058 stream sequence validation'),
    ('stream-correlation', transport, '.as_deref() != Some(id)', '.as_deref() == Some("disabled-stream-filter")', 'run_matches_ts_matrix', '#1058 output bytes and exit'),
    ('model-availability', turn, 'item.get("available").and_then(JsValue::as_bool) != Some(false)', 'true', 'sdk::turn::tests::model_resolution_matches_ts_boundaries', '#1058 model resolution bytes'),
    ('model-unique', turn, 'if let [item] = matches.as_slice()', 'if let Some(item) = matches.first()', 'sdk::turn::tests::model_resolution_matches_ts_boundaries', '#1058 model resolution bytes'),
    ('model-fields', turn, '(string_field(item, "provider"), string_field(item, "model"))', '(Some(string_field(item, "provider").unwrap_or_else(|| "wrong".into())), Some(string_field(item, "model").unwrap_or_else(|| "wrong".into())))', 'sdk::turn::tests::model_resolution_matches_ts_boundaries', '#1058 model resolution bytes'),
    ('model-empty-fields', turn, '!provider.is_empty()\n        && !model.is_empty()', 'true', 'sdk::turn::tests::model_resolution_matches_ts_boundaries', '#1058 model resolution bytes'),
    ('model-pair-format', turn, '"{}/{}"', '"{}:{}"', 'sdk::turn::tests::model_resolution_matches_ts_boundaries', '#1058 model resolution bytes'),
    ('model-undefined-pair', turn, 'string_field(item, "provider").unwrap_or_else(|| "undefined".into())', 'string_field(item, "provider").unwrap_or_default()', 'sdk::turn::tests::model_resolution_matches_ts_boundaries', '#1058 model resolution bytes'),
    ('sequence-gap', transport, 'if sequence != previous + 1.0 {', 'if false && sequence != previous + 1.0 {', 'sdk::transport::tests::stream_sequence_matches_ts_boundaries', '#1058 stream sequence validation'),
    ('sequence-negative', transport, '*seq >= 0.0', '*seq >= -1.0', 'sdk::transport::tests::stream_sequence_matches_ts_boundaries', '#1058 stream sequence validation'),
    ('sequence-fractional', transport, 'seq.fract() == 0.0', 'seq.fract() >= 0.0', 'sdk::transport::tests::stream_sequence_matches_ts_boundaries', '#1058 stream sequence validation'),
    ('can-prompt', turn, 'can_prompt: false,', 'can_prompt: true,', 'run_channel_and_two_connections', '#1058 complete two-connection wire and cleanup'),
    ('can-elicit', turn, 'can_elicit: false,', 'can_elicit: true,', 'run_channel_and_two_connections', '#1058 complete two-connection wire and cleanup'),
    ('run-placeholder', sdk, 'turn::run(&argv[1..], &connection)', 'Err(Failure::Todo("pilotdeck run placeholder regression"))', 'run_token_file_reaches_gateway', '#1058 output bytes and exit'),
    ('validation-oracle-drift', validation / 'oracle.json', '"nodeVersion": "v22.22.0"', '"nodeVersion": "v0.0.0"', 'run_validation_oracle_regenerates_identically', '#1058 identical TS fixture sdk-run-validation'),
    ('split-surrogate', output, 'String::from_utf16_lossy(&self.chunks.concat())', 'self.chunks.iter().map(|chunk| String::from_utf16_lossy(chunk)).collect::<String>()', 'run_matches_ts_matrix', '#1058 output bytes and exit'),
    ('model-override', turn, 'model_override: model,', 'model_override: None,', 'run_matches_ts_matrix', '#1058 complete two-connection wire and cleanup'),
    ('permission-mode', turn, 'if matches!(p, "acceptEdits" | "dontAsk" | "auto") {', 'if false && matches!(p, "acceptEdits" | "dontAsk" | "auto") {', 'run_matches_ts_matrix', '#1058 complete two-connection wire and cleanup'),
    ('permission-base', turn, 'if p == "bypassPermissions" {', 'if false && p == "bypassPermissions" {', 'run_matches_ts_matrix', '#1058 complete two-connection wire and cleanup'),
    ('sdk-permission', turn, 'sdk_permission_mode: sdk_permission,', 'sdk_permission_mode: None,', 'run_channel_and_two_connections', '#1058 complete two-connection wire and cleanup'),
    ('sdk-session-config', turn, 'sdk_session_config: config,', 'sdk_session_config: None,', 'run_matches_ts_matrix', '#1058 complete two-connection wire and cleanup'),
    ('query-resume', turn, '.request("resume_session", json!({"sessionKey": session}))', '.request("wrong_resume", json!({"sessionKey": session}))', 'run_channel_and_two_connections', '#1058 complete two-connection wire and cleanup'),
    ('model-catalog', turn, '.request("model_catalog_list", json!({"projectKey":project}))', '.request("wrong_catalog", json!({"projectKey":project}))', 'run_matches_ts_matrix', '#1058 complete two-connection wire and cleanup'),
    ('drop-stdin', turn, 'if !std::io::stdin().is_terminal() {', 'if false && !std::io::stdin().is_terminal() {', 'run_stdin_contract', '#1058 expected second connection'),
    ('skip-second-connection', turn, 'let mut query = Transport::connect(connection, "pilotdeck-cli", true).await?;\n        let result = query_turn(&mut query, argv, &session, &prompt, project).await;\n        query.close().await;\n        result', 'query_turn(&mut control, argv, &session, &prompt, project).await', 'run_channel_and_two_connections', '#1058 expected second connection'),
    ('overwrite-submit-channel', turn, 'channel_key: "api_server",', 'channel_key: "cli",', 'run_channel_and_two_connections', '#1058 complete two-connection wire and cleanup'),
    ('argv-priority', turn, 'if !prompt.is_empty() {', 'if false && !prompt.is_empty() {', 'run_channel_and_two_connections', '#1058 expected second connection'),
    ('empty-prompt', turn, 'if prompt.is_empty() {', 'if false && prompt.is_empty() {', 'run_empty_prompt_contract', '#1058 no third connection'),
    ('stdin-trim', turn, 'paths::js_trim(&String::from_utf8_lossy(&bytes)).to_owned()', 'String::from_utf8_lossy(&bytes).into_owned()', 'run_stdin_contract', '#1058 complete two-connection wire and cleanup'),
    ('create-channel', turn, 'read_value(argv, "--channel").unwrap_or("cli")', '"cli"', 'run_channel_and_two_connections', '#1058 complete two-connection wire and cleanup'),
    ('create-project', turn, 'CreateSession {\n                    project_key: project,', 'CreateSession {\n                    project_key: None,', 'run_channel_and_two_connections', '#1058 complete two-connection wire and cleanup'),
    ('assistant-filter', output, '"assistant_text_delta" | "assistant.message"', '"assistant_text_delta_DISABLED" | "assistant.message_DISABLED"', 'run_matches_ts_matrix', '#1058 output bytes and exit'),
    ('empty-chunk', output, '!self.chunks.is_empty()', '!self.chunks.concat().is_empty()', 'run_text_contract', '#1058 output bytes and exit'),
    ('stream-newline', output, 'stdout_line("");', 'stdout_line("wrong");', 'run_matches_ts_matrix', '#1058 output bytes and exit'),
    ('text-before-delta', output, 'string_field(event, "text").or_else(|| string_field(event, "delta"))', 'string_field(event, "delta").or_else(|| string_field(event, "text"))', 'run_text_contract', '#1058 output bytes and exit'),
    ('terminal-before-final', turn, 'if final_frame {', 'if final_frame || string_field(&event, "type").as_deref() == Some("turn_completed") {', 'run_waits_for_stream_final', '#1058 output bytes and exit'),
    ('duplicate-sequence', transport, 'if sequence <= previous {', 'if false && sequence <= previous {', 'run_duplicate_event_is_not_printed', '#1058 output bytes and exit'),
    ('success-response-end', transport, 'Some("response") if matches!(frame.get("ok"), Some(JsValue::Bool(true))) => {\n                    return Ok(None);', 'Some("response") if matches!(frame.get("ok"), Some(JsValue::Bool(true))) => {\n                    return Err(error("Gateway success response rejected."));', 'run_success_response_ends_stream', '#1058 output bytes and exit'),
    ('format-refusal', turn, '.any(|arg| arg == "--json" || arg == "--stream-json")', '.any(|arg| arg == "--json_DISABLED" || arg == "--stream-json_DISABLED")', 'run_unimplemented_options_are_explicit', '#1058 preflight never connects'),
    ('oracle-drift', oracle, '"nodeVersion": "v22.22.0"', '"nodeVersion": "v0.0.0"', 'run_oracle_regenerates_identically', '#1058 identical TS fixture sdk-run'),
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
        print('\n'.join(failures) if failures else f'#1058 {len(mutations)} anchors checked')
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
        target = ['--bin','pilotdeck'] if test.startswith('sdk::') else ['--test','sdk_run_exit']
        argv = command + target + (['--no-run'] if compile else [test,'--','--exact','--include-ignored','--show-output','--test-threads=1'])
        result = run_owned(argv,cwd=root,env=run_env,timeout=900)
        (out / f'{label}.stdout').write_text(result.stdout)
        (out / f'{label}.stderr').write_text(result.stderr)
        return result

    guarded = [*sorted((root / 'src/pilotdeck-cli/src').rglob('*.rs')),
               root / 'src/pilotdeck-cli/Cargo.toml', root / 'src/Cargo.lock', Path(__file__).resolve(),
               tests, *sorted(fixture.glob('*.*')), *sorted(validation.glob('*.*')),
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
