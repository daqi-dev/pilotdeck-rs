#!/usr/bin/env python3
"""#1097 HEAD-only controls: --check | [--compile-only] NEW_OUTPUT_DIR [mutation ...].
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
tests = root / 'src/pilotdeck-cli/tests/sdk_format_exit.rs'
resume_tests = root / 'src/pilotdeck-cli/tests/sdk_resume_exit.rs'
sdk = root / 'src/pilotdeck-cli/src/sdk.rs'
fixture = root / 'src/pilotdeck-cli/tests/fixtures/sdk-format'
oracle = fixture / 'oracle.json'
source = fixture / 'source.json'
mutations = [
    ('disable-json', turn, '.any(|arg| arg == "--json")', '.any(|arg| arg == "--json_DISABLED")', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('disable-jsonl', turn, '.any(|arg| arg == "--stream-json")', '.any(|arg| arg == "--stream-json_DISABLED")', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('drop-event', output, 'if self.stream_json {', 'if self.stream_json && !raw_type.equals_str("input_accepted") {', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('extra-newline', output, '            stdout_line(&text);\n            return Ok(());', '            stdout_line(&text);\n            stdout_line("");\n            return Ok(());', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('json-newline', output, 'if completed && self.streamed {', 'if false && completed && self.streamed {', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('json-usage', output, '("usage", self.usage),', '("usage_missing", self.usage),', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('json-finish-reason', output, '("finishReason", self.finish_reason),', '("finishReason_missing", self.finish_reason),', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('json-output', output, '("output", output.unwrap_or(JsValue::Undefined)),', '("output_missing", output.unwrap_or(JsValue::Undefined)),', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('json-session', output, '("sessionId", self.session.as_str().into()),\n                ("result", result),', '("sessionId_missing", self.session.as_str().into()),\n                ("result", result),', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('json-exit', output, 'return Ok(u8::from(!completed));', 'return Ok(0);', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('error-code', output, '("code",', '("code_missing",', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('error-details', output, '("details",', '("details_missing",', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('error-retryable', output, '("retryable",', '("retryable_missing",', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('error-name', output, '("name",', '("name_missing",', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('error-order', output, '("code", JsValue::String(code)),\n                    ("details", details),', '("details", details),\n                    ("code", JsValue::String(code)),', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('error-status', output, 'object.insert("status".into(), "failed".into());', 'object.insert("status".into(), "completed".into());', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('terminal-output', output, 'event.get("result").cloned().unwrap_or(JsValue::Undefined)', 'event.get("absent_result").cloned().unwrap_or(JsValue::Undefined)', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('terminal-status', output, 'object.insert("status".into(), "completed".into());', 'object.insert("status".into(), "failed".into());', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('structured-output', output, 'self.structured = event.get("payload").cloned();', 'self.structured = None;', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('structured-null', output, '.structured\n                .as_ref()\n                .filter(|value| !matches!(value, JsValue::Null | JsValue::Undefined))', '.structured\n                .as_ref()\n                .filter(|_value| true)', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('duplicate-terminal', output, 'if self.terminal.is_some()', 'if false && self.terminal.is_some()', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('partial-assistant', output, 'raw_type.equals_str("assistant_text_delta") || raw_type.equals_str("assistant.message")', 'raw_type.equals_str("assistant_text_delta_DISABLED") || raw_type.equals_str("assistant.message_DISABLED")', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('partial-thinking', output, 'raw_type.equals_str("assistant_thinking_delta")', 'raw_type.equals_str("assistant_thinking_delta_DISABLED")', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('type-coercion', output, 'js_string(value)?', 'JsString::from("wrong")', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('type-default', output, '_ => JsString::from("unknown"),', '_ => JsString::from("wrong"),', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('prototype-object', output, 'if raw.equals_str("__proto__") {', 'if raw.equals_str("__proto___DISABLED") {', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('prototype-function', output, '        "toString",', '        "toString_DISABLED",', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('prototype-text-error', output, 'if !matches!(public_type(&raw_type, event), JsValue::String(_)) {', 'if false && !matches!(public_type(&raw_type, event), JsValue::String(_)) {', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('usage-arrays', output, 'fn as_record(value: Option<&JsValue>) -> JsValue {\n    value\n        .filter(|value| matches!(value, JsValue::Object(_) | JsValue::Array(_)))', 'fn as_record(value: Option<&JsValue>) -> JsValue {\n    value\n        .filter(|value| matches!(value, JsValue::Object(_)))', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('usage-scalars', output, 'fn as_record(value: Option<&JsValue>) -> JsValue {\n    value\n        .filter(|value| matches!(value, JsValue::Object(_) | JsValue::Array(_)))', 'fn as_record(value: Option<&JsValue>) -> JsValue {\n    value\n        .filter(|_value| true)', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('tool-failed', output, 'event.get("ok") == Some(&JsValue::Bool(false))', 'event.get("ok") == Some(&JsValue::Bool(true))', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('map-turn_started', output, 'raw.equals_str("turn_started")', 'raw.equals_str("turn_started_DISABLED")', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('map-input_accepted', output, 'raw.equals_str("input_accepted")', 'raw.equals_str("input_accepted_DISABLED")', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('map-subagent_text_delta', output, 'raw.equals_str("subagent_text_delta")', 'raw.equals_str("subagent_text_delta_DISABLED")', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('map-tool_call_started', output, 'raw.equals_str("tool_call_started")', 'raw.equals_str("tool_call_started_DISABLED")', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('map-tool_progress', output, 'raw.equals_str("tool_progress")', 'raw.equals_str("tool_progress_DISABLED")', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('map-prompt_suggestion', output, 'raw.equals_str("prompt_suggestion")', 'raw.equals_str("prompt_suggestion_DISABLED")', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('map-tool_result_detail_available', output, 'raw.equals_str("tool_result_detail_available")', 'raw.equals_str("tool_result_detail_available_DISABLED")', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('map-permission_request', output, 'raw.equals_str("permission_request")', 'raw.equals_str("permission_request_DISABLED")', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('map-hook_started', output, 'raw.equals_str("hook_started")', 'raw.equals_str("hook_started_DISABLED")', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('map-hook_response', output, 'raw.equals_str("hook_response")', 'raw.equals_str("hook_response_DISABLED")', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('map-hook_async_result', output, 'raw.equals_str("hook_async_result")', 'raw.equals_str("hook_async_result_DISABLED")', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('map-elicitation_request', output, 'raw.equals_str("elicitation_request")', 'raw.equals_str("elicitation_request_DISABLED")', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('map-elicitation_cancelled', output, 'raw.equals_str("elicitation_cancelled")', 'raw.equals_str("elicitation_cancelled_DISABLED")', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('map-user_dialog_request', output, 'raw.equals_str("user_dialog_request")', 'raw.equals_str("user_dialog_request_DISABLED")', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('map-user_dialog_cancelled', output, 'raw.equals_str("user_dialog_cancelled")', 'raw.equals_str("user_dialog_cancelled_DISABLED")', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('map-structured_output', output, 'raw.equals_str("structured_output")', 'raw.equals_str("structured_output_DISABLED")', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('map-context_budget', output, 'raw.equals_str("context_budget")', 'raw.equals_str("context_budget_DISABLED")', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('map-turn_completed', output, 'raw.equals_str("turn_completed")', 'raw.equals_str("turn_completed_DISABLED")', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('map-error', output, 'raw.equals_str("error")', 'raw.equals_str("error_DISABLED")', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('map-permission_denied', output, 'raw.equals_str("permission_denied")', 'raw.equals_str("permission_denied_DISABLED")', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('map-dotted', output, 'if contains_sub(raw, ".") {', 'if false && contains_sub(raw, ".") {', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('map-prefix', output, '"pilotdeck.".encode_utf16()', '"wrong.".encode_utf16()', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('context-used', output, '    "used",', '    "used_missing",', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('context-displayUsed', output, '    "displayUsed",', '    "displayUsed_missing",', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('context-budgetUsed', output, '    "budgetUsed",', '    "budgetUsed_missing",', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('context-localEstimateTokens', output, '    "localEstimateTokens",', '    "localEstimateTokens_missing",', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('context-displayTokens', output, '    "displayTokens",', '    "displayTokens_missing",', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('context-estimateSource', output, '    "estimateSource",', '    "estimateSource_missing",', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('context-usageTokens', output, '    "usageTokens",', '    "usageTokens_missing",', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('context-calibrationActualInputTokens', output, '    "calibrationActualInputTokens",', '    "calibrationActualInputTokens_missing",', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('context-calibrationEstimatedInputTokens', output, '    "calibrationEstimatedInputTokens",', '    "calibrationEstimatedInputTokens_missing",', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('context-total', output, '    "total",', '    "total_missing",', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('context-totalContextTokens', output, '    "totalContextTokens",', '    "totalContextTokens_missing",', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('context-maxContextTokens', output, '    "maxContextTokens",', '    "maxContextTokens_missing",', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('context-effectiveTotal', output, '    "effectiveTotal",', '    "effectiveTotal_missing",', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('context-effectiveContextTokens', output, '    "effectiveContextTokens",', '    "effectiveContextTokens_missing",', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('context-maxOutputTokens', output, '    "maxOutputTokens",', '    "maxOutputTokens_missing",', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('context-reservedOutputTokens', output, '    "reservedOutputTokens",', '    "reservedOutputTokens_missing",', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('context-warningRatio', output, '    "warningRatio",', '    "warningRatio_missing",', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('context-blockingRatio', output, '    "blockingRatio",', '    "blockingRatio_missing",', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('context-ratio', output, '    "ratio",', '    "ratio_missing",', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('context-state', output, '    "state",', '    "state_missing",', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('context-source', output, '    "source",', '    "source_missing",', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('context-exact', output, '    "exact",', '    "exact_missing",', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('context-estimatorError', output, '    "estimatorError",', '    "estimatorError_missing",', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('context-breakdown', output, '    "breakdown",', '    "breakdown_missing",', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('context-usage', output, 'object.insert("usage".into(), JsValue::Object(usage));', 'object.insert("usage_missing".into(), JsValue::Object(usage));', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('public-session', output, 'object.insert("sessionId".into(), JsValue::String(JsString::from(session)));', 'object.insert("sessionId".into(), JsValue::String(JsString::from("wrong")));', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('public-sequence', output, 'event.get("sequence").cloned().unwrap_or(JsValue::Undefined)', 'event.get("absent_sequence").cloned().unwrap_or(JsValue::Undefined)', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('public-runid', output, 'event.get("runId").cloned().unwrap_or(JsValue::Undefined)', 'event.get("absent_runId").cloned().unwrap_or(JsValue::Undefined)', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('sequence-ordinal', output, 'self.sequence += 1.0;', 'self.sequence += 2.0;', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('sequence-override', output, 'projected.insert("sequence".into(), JsValue::Number(sequence));', 'projected.insert("sequence_missing".into(), JsValue::Number(sequence));', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('spread-object', output, 'for (key, value) in value.entries() {', 'for (key, value) in value.entries().iter().take(0) {', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('spread-array', output, 'for (index, value) in values.iter().enumerate() {', 'for (index, value) in values.iter().enumerate().skip(1) {', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('spread-utf16', output, 'JsString::from_utf16(vec![*unit])', 'JsString::from_utf16(vec![0xfffd])', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('sequence-duplicate', transport, 'if sequence <= previous {', 'if false && sequence <= previous {', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('bind-public-id', tests, 'if jsonl\n', 'if false && jsonl\n', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('bind-recovery-id', tests, 'if let Some(index) = expected.rfind(marker)', 'if let Some(index) = expected.rfind("disabled-envelope-marker")', 'format_matches_ts_matrix', '#1097 output bytes and exit'),
    ('oracle-empty', oracle, '  "cases": [', '  "cases": [],\n  "discardedCases": [', 'format_matches_ts_matrix', '#1097 registered 84 cells'),
    ('oracle-drift', oracle, '"nodeVersion": "v22.22.0"', '"nodeVersion": "v0.0.0"', 'format_oracle_regenerates_identically', '#1097 identical TS fixture sdk-format'),
    ('format-late-connection', turn, '    control.close().await;\n    result', '    control.close().await;\n    tokio::time::sleep(std::time::Duration::from_millis(100)).await;\n    let _ = tokio::net::TcpStream::connect("127.0.0.1:28999").await;\n    result', 'format_no_late_connection', '#1097 no third connection'),
    ('format-stdin-synchronous', tests, '        let handle = std::thread::spawn(move || {', '        write_fixture_stdin(&mut stdin, &data);\n        let handle = std::thread::spawn(move || {', 'fixture_stdin_writer_yields_before_child_reads', '#1097 stdin writer yields before child reads'),
    ('format-stdin-cancel', tests, '.drain(..) {', '.drain(0..0) {', 'fixture_stdin_cancel_reaps_writer', '#1097 cancellation joins stdin writer'),
    ('resume-late-connection', turn, '    control.close().await;\n    result', '    control.close().await;\n    tokio::time::sleep(std::time::Duration::from_millis(100)).await;\n    let _ = tokio::net::TcpStream::connect("127.0.0.1:28999").await;\n    result', 'resume_no_late_connection', '#1086 no third connection'),
    ('resume-stdin-synchronous', resume_tests, '        let handle = std::thread::spawn(move || {', '        write_fixture_stdin(&mut stdin, &data);\n        let handle = std::thread::spawn(move || {', 'fixture_stdin_writer_yields_before_child_reads', '#1097 stdin writer yields before child reads'),
    ('resume-stdin-cancel', resume_tests, '.drain(..) {', '.drain(0..0) {', 'fixture_stdin_cancel_reaps_writer', '#1097 cancellation joins stdin writer'),
    ('resume-preflight-connect', sdk, '    if session.is_empty() {', '    if session.is_empty() {\n        let _ = std::net::TcpStream::connect("127.0.0.1:28999");', 'resume_preflight_no_late_connection', '#1086 preflight never connects'),
    ('format-preflight-connect', sdk, '            stdout_line(HELP);', '            let _ = std::net::TcpStream::connect("127.0.0.1:28999");\n            stdout_line(HELP);', 'format_preflight_no_late_connection', '#1097 preflight never connects'),
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
        print('\n'.join(failures) if failures else f'#1097 {len(mutations)} anchors checked')
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
        target = ['--test', 'sdk_resume_exit' if label.startswith('resume-') else 'sdk_format_exit']
        argv = command + target + (['--no-run'] if compile else [test,'--','--exact','--include-ignored','--show-output','--test-threads=1'])
        result = run_owned(argv,cwd=root,env=run_env,timeout=900)
        (out / f'{label}.stdout').write_text(result.stdout)
        (out / f'{label}.stderr').write_text(result.stderr)
        return result

    guarded = [*sorted((root / 'src/pilotdeck-cli/src').rglob('*.rs')),
               root / 'src/pilotdeck-cli/Cargo.toml', root / 'src/Cargo.lock', Path(__file__).resolve(),
               tests, resume_tests, *sorted(fixture.glob('*.*')),
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
