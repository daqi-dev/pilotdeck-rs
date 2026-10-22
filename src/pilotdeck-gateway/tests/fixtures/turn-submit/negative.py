#!/usr/bin/env python3
"""Compile temporary product mutations; require a failing real-TS replay, restore bytes.

Run exclusively. No fixture or test is mutated. Compile failures and timeouts
are INVALID evidence. All process cleanup targets this script's own group.
"""
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time

fixture = Path(__file__).resolve().parent
rust = fixture.parents[3]
repo = rust.parent
output = Path(sys.argv[1]).resolve()
output.mkdir(parents=True, exist_ok=True)
source = rust / 'pilotdeck-gateway/src/runtime/turn/submit.rs'
original = source.read_bytes()
lock = repo / '_tmp/turn-submit-mutation.lock'
lock.mkdir()
env = dict(os.environ, CARGO_NET_OFFLINE='true')
env.setdefault('CARGO_TARGET_DIR', str(rust / 'target'))
assert env.get('PDRS_BASELINE'), 'set PDRS_BASELINE'
test = 'runtime::turn::submit::tests'
# Each mutation changes production behavior. The chosen case names are real
# generated scenarios; all golden assertions remain unchanged.
mutants = [
    ('permission-check-skipped', 'let value = field(input, key);', 'let value = &JsValue::Undefined;', 'invalid-mode'),
    ('run-id-check-skipped', 'let id = field(input, "runId");', 'let id = &JsValue::Undefined;', 'invalid-run-id'),
    ('budget-check-skipped', 'let budget = field(input, "maxBudgetUsd");', 'let budget = &JsValue::Undefined;', 'invalid-budget'),
    ('trusted-context-check-skipped', 'if let value = field(input, "trustedContext")', 'if let value = &JsValue::Undefined', 'invalid-trusted-context'),
    ('busy-code-changed', 'if sdk_admission {\n                    "SESSION_BUSY"\n                } else {\n                    "session_busy"\n                }', 'if sdk_admission { "session_busy" } else { "SESSION_BUSY" }', 'session-busy'),
    ('sdk-capability-ignored', 'let error = if !self.host.has_sdk_config() {', 'let error = if false && !self.host.has_sdk_config() {', 'sdk-unavailable'),
    ('sdk-validation-ignored', '.validate_sdk_config(field(&input, "sdkSessionConfig"))\n                    .err()', '.validate_sdk_config(field(&input, "sdkSessionConfig"))\n                    .err().filter(|_| false)', 'sdk-invalid'),
    ('sdk-dirty-skipped', '.mark_session_dirty(&run.session_key, "sdk_session_config_changed");', '.mark_session_dirty(&run.session_key, "");', 'sdk-changed'),
    ('sdk-abort-ignored', 'let event = if completion.is_cancelled() {', 'let event = if false && completion.is_cancelled() {', 'abort-during-sdk-failure'),
    ('refresh-error-propagated', 'let _ = self.host.refresh().await;', 'self.host.refresh().await?;', 'refresh-failed-keeps-snapshot'),
    ('accepted-commit-skipped', 'self.host.commit_accepted_input(run).await?;', 'let _ = run;', 'commit-failed'),
    ('projection-drop-event', '                publisher.gateway_event(event);\n            }\n        }\n        Ok(())', '                let _ = event;\n            }\n        }\n        Ok(())', 'durable-accept-and-session-end'),
    ('projection-duplicate-event', '                publisher.gateway_event(event);\n            }\n        }\n        Ok(())', '                publisher.gateway_event(event.clone()); publisher.gateway_event(event);\n            }\n        }\n        Ok(())', 'durable-accept-and-session-end'),
    ('early-terminal-before-session-end', '                publisher.gateway_event(event);\n            }\n        }\n        Ok(())', '                let terminal = is(&event, "type", "turn_completed"); publisher.gateway_event(event); if terminal { publisher.finish().await; }\n            }\n        }\n        Ok(())', 'durable-accept-and-session-end'),
    ('finally-error-swallowed', 'self.host.after_turn(&run)', 'let _ = self.host.after_turn(&run); Ok(())', 'finally-failed'),
    ('input-ack-before-native-durable', 'let mut stream = handle\n            .submit(prepared.input, prepared.options)', 'publisher.gateway_event(object([("type", "input_accepted".into()), ("runId", JsValue::String(run.run_id.clone()))]));\n        let mut stream = handle\n            .submit(prepared.input, prepared.options)', 'durable-accept-and-session-end'),
    ('stale-completion-filter-skipped', 'if !self\n                .owner\n                .is_current_completion(&run.session_key, completion)', 'if false && !self\n                .owner\n                .is_current_completion(&run.session_key, completion)', 'superseded-completion'),
    ('model-selection-event-skipped', 'publisher.gateway_event(model_event(model, &source, run));', 'let _ = model;', 'actual-model-selection'),
    ('actual-text-model-skipped', 'set(&mut event, "model", model.clone());', 'let _ = model;', 'actual-model-selection'),
    ('lease-release-skipped', 'let _ = self.host.finish_pump(&run).await;', 'let _ = &run;', 'lease-release'),
    ('fingerprint-includes-reconnect-metadata', '["runId", "interactionBinding", "telemetry"]', '["ignoredRunId", "ignoredBinding", "ignoredTelemetry"]', 'execution-fingerprint'),
    ('explicit-null-sdk-treated-absent', 'let sdk_admission = !matches!(field(&input, "sdkSessionConfig"), JsValue::Undefined);', 'let sdk_admission = sdk;', 'sdk-null-busy'),
    ('cancel-hides-recovery-failure', 'Ok(events) if events.is_empty() && completion.is_cancelled()', 'Ok(events) if completion.is_cancelled()', 'abort-and-recovery-failed'),
    ('history-epoch-error-hidden', 'if let Err(error) = outcome', 'if let Err(error) = outcome && error.message != "history epoch changed"', 'history-epoch-failed'),
    ('utf16-mode-lossy', 'JsString::from_utf16(message)', 'String::from_utf16_lossy(&message).into()', 'invalid-mode'),
    ('utf16-busy-lossy', 'JsString::from_utf16(units)', 'String::from_utf16_lossy(&units).into()', 'session-busy'),
]
summary = []
try:
    for name, before, _, _ in mutants:
        assert original.decode().count(before) == 1, (name, 'anchor is not unique')
    (output / 'original-sha256.json').write_text(json.dumps({str(source.relative_to(repo)): hashlib.sha256(original).hexdigest()}, indent=2)+'\n')
    selected = [('baseline', None, None, None)] + mutants
    if len(sys.argv) > 2:
        selected = [entry for entry in selected if entry[0] in sys.argv[2:]]
        assert selected, 'unknown control'
    for name, before, after, case in selected:
        assert source.read_bytes() == original, 'source changed outside this run'
        if before: source.write_text(original.decode().replace(before, after, 1))
        slots = subprocess.run(['bash', str(repo / 'scripts/cargo-slots.sh')], text=True, capture_output=True)
        while slots.returncode == 1:
            time.sleep(30)
            slots = subprocess.run(['bash', str(repo / 'scripts/cargo-slots.sh')], text=True, capture_output=True)
        assert slots.returncode == 0
        command = ['cargo', 'test', '--offline', '--manifest-path', str(rust / 'Cargo.toml'), '-p', 'pilotdeck-gateway', '--lib', test, '--', '--nocapture']
        run_env = dict(env)
        if case: run_env['PDRS_SUBMIT_CASE'] = case
        else: run_env.pop('PDRS_SUBMIT_CASE', None)
        process = subprocess.Popen(command, cwd=repo, env=run_env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, start_new_session=True)
        timed_out = False
        try:
            raw, _ = process.communicate(timeout=240)
        except subprocess.TimeoutExpired:
            timed_out = True
            os.killpg(process.pid, signal.SIGKILL)
            raw, _ = process.communicate()
        finally:
            source.write_bytes(original)
        valid = not timed_out and ((process.returncode == 0 and 'test result: ok. 2 passed' in raw) if name == 'baseline' else (
            process.returncode == 101 and 'test result: FAILED' in raw and 'could not compile' not in raw
            and 'assertion' in raw and test in raw))
        (output / f'{name}.log').write_text('COMMAND '+' '.join(command)+'\n'+slots.stdout+slots.stderr+raw+f'\nexit={process.returncode}\n')
        summary.append(dict(name=name, case=case, before=before, after=after, exit=process.returncode, valid=valid))
        (output/'summary.json').write_text(json.dumps(summary, ensure_ascii=False, indent=2)+'\n')
        print(f'{name}: {"PASS" if valid else "INVALID"} exit={process.returncode}', flush=True)
        if not valid: raise SystemExit(1)
finally:
    source.write_bytes(original)
    assert source.read_bytes() == original
    lock.rmdir()
print(f'{len(summary)} controls checked; original bytes restored', flush=True)
