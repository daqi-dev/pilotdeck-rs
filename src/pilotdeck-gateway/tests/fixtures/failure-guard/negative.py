#!/usr/bin/env python3
"""Mutate guard/submit product bytes, replay real TS gold, restore exact bytes.
Run exclusively. Compile failures, signals, empty matches and deadlines are INVALID.
The launcher's one target is mandatory. Each child process group has a deadline.
"""
import hashlib
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import sys
import time

fixture = Path(__file__).resolve().parent
workspace = fixture.parents[3]
repo = workspace.parent
turn = workspace / 'pilotdeck-gateway/src/runtime/turn'
guard = turn / 'failure_guard.rs'
submit = turn / 'submit.rs'
out = Path(sys.argv[1]).resolve()
out.mkdir(parents=True, exist_ok=True)
env = dict(os.environ, CARGO_NET_OFFLINE='true')
assert Path(env['CARGO_TARGET_DIR']).resolve() == workspace / 'target'
assert env.get('PDRS_BASELINE'), 'set PDRS_BASELINE'
for key in ['PDRS_SUBMIT_CASE', 'PDRS_SUBMIT_CAPABILITY']:
    env.pop(key, None)
original = {p: p.read_bytes() for p in [guard, submit]}
pure = 'runtime::turn::failure_guard::tests::real_ts_failure_guard_and_classify'
replay = 'runtime::turn::submit::tests::real_ts_submit_scenarios'

def mutation(name, path, before, after, case=None, marker=None):
    assert original[path].decode().count(before) == 1, (name, 'anchor not unique')
    return dict(name=name, path=path, before=before, after=after, test=replay if case else pure,
        case=case, marker=marker or (f'{case}: step' if case else 'failure guard'))

mutants = []
for offset, label in [('+ 1.0', 'plus-one'), ('- 1.0', 'minus-one')]:
    mutants.append(mutation('tool-threshold-' + label, guard, '*failures < limit', f'*failures < limit {offset}'))
    mutants.append(mutation('model-threshold-' + label, guard, 'self.model_failures < limit', f'self.model_failures < limit {offset}'))
mutants += [
    mutation('counted-classification-wrong', guard, '"tool_unavailable",', '"permission_denied",'),
    mutation('prefix-shortest-not-longest', guard, '.max_by_key(|(unit, _)| unit.units().len())', '.min_by_key(|(unit, _)| unit.units().len())'),
    mutation('exact-unit-skipped', guard, 'if let Some(limit) = limits.get_key(name)', 'if let Some(limit) = limits.get_key(name).filter(|_| false)'),
    mutation('success-clears-window', guard, 'if !is(result, "type", "error") {', 'if !is(result, "type", "error") { self.tool_failures.clear();'),
    mutation('trip-latch-ignored', guard, 'if self.tripped || ok ||', 'if (self.tripped && false) || ok ||'),
    mutation('trip-message-changed', guard, '次，已自动停止，请稍后重试', '次，已自动停止，请重试'),
    mutation('disabled-policy-observed', submit, '.filter(|config| truthy(field(config, "enabled")))', '.filter(|config| { let _ = config; true })', 'guard-disabled'),
    mutation('model-not-observed', submit, '&& let Some(trip) = guard.observe(&event)', '&& !is(&event, "type", "turn_continued") && let Some(trip) = guard.observe(&event)', 'guard-model-trip'),
    mutation('tool-not-observed', submit, '&& let Some(trip) = guard.observe(&event)', '&& !is(&event, "type", "tool_result") && let Some(trip) = guard.observe(&event)', 'guard-tool-trip'),
    mutation('subagent-not-observed', submit, '&& let Some(trip) = guard.observe(&event)', '&& !is(&event, "type", "subagent_tool_result") && let Some(trip) = guard.observe(&event)', 'guard-subagent-trip'),
    mutation('trip-still-admitted', submit, 'let _ = handle.abort(&format!("run_policy:{code}").into());', 'let _ = (&handle, &code);', 'guard-model-trip'),
    mutation('trip-status-not-recorded', submit, 'let _ = self.host.record_failure(run, &record).await;\n        publisher.gateway_event(object([', 'let _ = record;\n        publisher.gateway_event(object([', 'guard-tool-trip'),
    mutation('record-error-propagated', submit, 'let _ = self.host.record_failure(run, &record).await;\n        publisher.gateway_event(object([', 'self.host.record_failure(run, &record).await?;\n        publisher.gateway_event(object([', 'guard-trip-status-record-failed'),
    mutation('model-metadata-dropped', submit, 'build_guard_trip_message(trip, model.as_ref())', 'build_guard_trip_message(trip, model.as_ref().filter(|_| false))', 'guard-model-trip'),
    mutation('trip-abort-reason-wrong', submit, 'format!("run_policy:{code}")', 'format!("policy:{code}")', 'guard-tool-trip'),
]
mutants += [
    mutation('guard-wire-order-changed', submit, '("runId", JsValue::String(run.run_id.clone())),\n            ("event", "run_policy_stopped".into()),', '("event", "run_policy_stopped".into()),\n            ("runId", JsValue::String(run.run_id.clone())),', 'guard-model-trip'),
    mutation('next-window-keeps-model-count', submit, '.map(FailureGuard::new);', '.map(|config| { let mut guard = FailureGuard::new(config); if run.run_id.equals_str("r2") { let _ = guard.on_model_failure(); } guard });', 'guard-reset-next-turn'),
]
# Take the real configuration snapshot before refresh rather than after it.
submit_text = original[submit].decode()
start = submit_text.index('        let _ = self.host.refresh().await;')
end = submit_text.index('        let handle = self', start)
block = submit_text[start:end]
config_start = block.index('        let mut failure_guard = self')
before_refresh = block[config_start:] + block[:config_start]
mutants.append(mutation('config-snapshotted-before-refresh', submit, block, before_refresh, 'guard-refresh-enables'))

selected = [dict(name='baseline-pure', test=pure, marker=None), dict(name='baseline-submit', test=replay, marker=None)] + mutants
if len(sys.argv) > 2:
    selected = [m for m in selected if m['name'] in sys.argv[2:]]
    assert selected, 'unknown controls'
command = ['cargo', 'test', '--offline', '--manifest-path', str(workspace / 'Cargo.toml'), '-p', 'pilotdeck-gateway', '--lib']
summary = []
lock = repo / '_tmp/turn-submit-mutation.lock'
lock.mkdir()
deadline = time.monotonic() + 1800
try:
    (out / 'original-sha256.json').write_text(json.dumps({str(p.relative_to(repo)): hashlib.sha256(data).hexdigest() for p, data in original.items()}, indent=2) + '\n')
    for control in selected:
        assert time.monotonic() < deadline, 'overall 30 minute deadline'
        assert all(p.read_bytes() == data for p, data in original.items()), 'concurrent source edit'
        slots = subprocess.run(['bash', str(repo / 'scripts/cargo-slots.sh')], capture_output=True, text=True, timeout=10)
        while slots.returncode == 1:
            assert time.monotonic() < deadline, 'cargo slots deadline'
            time.sleep(20)
            slots = subprocess.run(['bash', str(repo / 'scripts/cargo-slots.sh')], capture_output=True, text=True, timeout=10)
        assert slots.returncode == 0
        modified = dict(original)
        if 'path' in control:
            path = control['path']
            modified[path] = original[path].decode().replace(control['before'], control['after'], 1).encode()
            path.write_bytes(modified[path])
        run_env = dict(env)
        if control.get('case'): run_env['PDRS_SUBMIT_CASE'] = control['case']
        cmd = command + [control['test'], '--', '--exact', '--nocapture']
        process = subprocess.Popen(cmd, cwd=repo, env=run_env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, start_new_session=True)
        timed_out = False
        try:
            raw, _ = process.communicate(timeout=min(240, max(1, deadline - time.monotonic())))
        except subprocess.TimeoutExpired:
            timed_out = True
            os.killpg(process.pid, signal.SIGKILL)
            raw, _ = process.communicate()
        finally:
            for path, data in original.items():
                assert path.read_bytes() == modified[path], f'concurrent edit preserved: {path}'
                if modified[path] != data: path.write_bytes(data)
        marker = control['marker']
        valid = not timed_out and ((process.returncode == 0 and 'test result: ok. 1 passed; 0 failed;' in raw
            and f'test {control["test"]} ... ok' in raw) if marker is None else (
            process.returncode == 101 and 'test result: FAILED' in raw and 'assertion' in raw
            and marker in raw and 'could not compile' not in raw))
        (out / f'{control["name"]}.log').write_text('COMMAND ' + ' '.join(cmd) + '\n' + slots.stdout + slots.stderr + raw + f'\nexit={process.returncode} timed_out={timed_out}\n')
        summary.append(dict(name=control['name'], case=control.get('case'), marker=marker, before=control.get('before'), after=control.get('after'), pid=process.pid, reaped=True, exit=process.returncode, timed_out=timed_out, valid=valid))
        (out / 'summary.json').write_text(json.dumps(summary, ensure_ascii=False, indent=2) + '\n')
        print(f'{control["name"]}: {"PASS" if valid else "INVALID"} exit={process.returncode}', flush=True)
        if not valid: break
finally:
    lock.rmdir()
    assert all(p.read_bytes() == data for p, data in original.items()), 'original bytes not restored'
    (out / 'restored-sha256.json').write_text(json.dumps({str(p.relative_to(repo)): hashlib.sha256(data).hexdigest() for p, data in original.items()}, indent=2) + '\n')
raise SystemExit(0 if len(summary) == len(selected) and all(row['valid'] for row in summary) else 1)
