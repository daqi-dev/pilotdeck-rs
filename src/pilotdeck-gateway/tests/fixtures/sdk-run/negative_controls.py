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
fixture = root / 'src/pilotdeck-cli/tests/fixtures/sdk-run'
oracle = fixture / 'oracle.json'
source = fixture / 'source.json'
mutations = [
    ('stream-correlation', transport, '.as_deref() != Some(id)', '.as_deref() == Some("disabled-stream-filter")', 'run_matches_ts_matrix', '#1058 output bytes and exit'),
    ('model-availability', turn, 'item.get("available").and_then(JsValue::as_bool) != Some(false)', 'item.get("available").and_then(JsValue::as_bool) != Some(true)', 'run_matches_ts_matrix', '#1058 expected gateway lifecycle'),
    ('split-surrogate', output, 'String::from_utf16_lossy(&self.chunks.concat())', 'self.chunks.iter().map(|chunk| String::from_utf16_lossy(chunk)).collect::<String>()', 'run_matches_ts_matrix', '#1058 output bytes and exit'),
    ('model-override', turn, 'model_override: model,', 'model_override: None,', 'run_matches_ts_matrix', '#1058 complete two-connection wire and cleanup'),
    ('permission-mode', turn, 'if matches!(p, "acceptEdits" | "dontAsk" | "auto") {', 'if false && matches!(p, "acceptEdits" | "dontAsk" | "auto") {', 'run_matches_ts_matrix', '#1058 complete two-connection wire and cleanup'),
    ('permission-base', turn, 'if p == "bypassPermissions" {', 'if false && p == "bypassPermissions" {', 'run_matches_ts_matrix', '#1058 complete two-connection wire and cleanup'),
    ('sdk-permission', turn, 'sdk_permission_mode: sdk_permission,', 'sdk_permission_mode: None,', 'run_channel_and_two_connections', '#1058 complete two-connection wire and cleanup'),
    ('sdk-session-config', turn, 'sdk_session_config: config,', 'sdk_session_config: None,', 'run_matches_ts_matrix', '#1058 complete two-connection wire and cleanup'),
    ('query-resume', turn, '.request("resume_session", json!({"sessionKey": session}))', '.request("wrong_resume", json!({"sessionKey": session}))', 'run_channel_and_two_connections', '#1058 complete two-connection wire and cleanup'),
    ('model-catalog', turn, '.request("model_catalog_list", json!({"projectKey":project}))', '.request("wrong_catalog", json!({"projectKey":project}))', 'run_matches_ts_matrix', '#1058 complete two-connection wire and cleanup'),
    ('drop-stdin', turn, 'const READ_STDIN: bool = true;', 'const READ_STDIN: bool = false;', 'run_stdin_contract', '#1058 expected gateway lifecycle'),
    ('skip-second-connection', turn, 'const SECOND_CONNECTION: bool = true;', 'const SECOND_CONNECTION: bool = false;', 'run_channel_and_two_connections', '#1058 expected gateway lifecycle'),
    ('overwrite-submit-channel', turn, 'const SUBMIT_CHANNEL: &str = "api_server";', 'const SUBMIT_CHANNEL: &str = "cli";', 'run_channel_and_two_connections', '#1058 complete two-connection wire and cleanup'),
    ('argv-priority', turn, 'if !prompt.is_empty() {', 'if false && !prompt.is_empty() {', 'run_argv_does_not_wait_for_stdin', '#1058 expected gateway lifecycle'),
    ('empty-prompt', turn, 'if prompt.is_empty() {', 'if false && prompt.is_empty() {', 'run_empty_prompt_contract', '#1058 no third connection'),
    ('stdin-trim', turn, 'paths::js_trim(&String::from_utf8_lossy(&bytes)).to_owned()', 'String::from_utf8_lossy(&bytes).into_owned()', 'run_stdin_contract', '#1058 complete two-connection wire and cleanup'),
    ('create-channel', turn, 'read_value(argv, "--channel").unwrap_or("cli")', '"cli"', 'run_channel_and_two_connections', '#1058 complete two-connection wire and cleanup'),
    ('create-project', turn, 'CreateSession {\n                    project_key: project,', 'CreateSession {\n                    project_key: None,', 'run_channel_and_two_connections', '#1058 complete two-connection wire and cleanup'),
    ('assistant-filter', output, '"assistant_text_delta" | "assistant.message"', '"assistant_text_delta_DISABLED" | "assistant.message_DISABLED"', 'run_matches_ts_matrix', '#1058 output bytes and exit'),
    ('empty-chunk', output, '!self.chunks.is_empty()', '!self.chunks.concat().is_empty()', 'run_text_contract', '#1058 output bytes and exit'),
    ('stream-newline', output, 'stdout_line("");', 'stdout_line("wrong");', 'run_matches_ts_matrix', '#1058 output bytes and exit'),
    ('text-before-delta', output, 'string_field(event, "text").or_else(|| string_field(event, "delta"))', 'string_field(event, "delta").or_else(|| string_field(event, "text"))', 'run_text_contract', '#1058 output bytes and exit'),
    ('terminal-before-final', turn, 'if final_frame {', 'if final_frame || string_field(&event, "type").as_deref() == Some("turn_completed") {', 'run_waits_for_stream_final', '#1058 output bytes and exit'),
    ('duplicate-sequence', transport, 'if sequence <= self.stream_sequence {', 'if false && sequence <= self.stream_sequence {', 'run_duplicate_event_is_not_printed', '#1058 output bytes and exit'),
    ('success-response-end', transport, 'return Ok(None);', 'continue;', 'run_success_response_ends_stream', '#1058 expected gateway lifecycle'),
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
    command = ['cargo','test','--offline','--manifest-path','src/Cargo.toml','-p','pilotdeck-cli','--test','sdk_run_exit']

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
