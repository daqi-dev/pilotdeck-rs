#!/usr/bin/env python3
"""Mutate guard/submit product bytes, replay real TS gold, restore exact bytes.
Run exclusively. Compile failures, signals, empty matches and deadlines are INVALID.
The launcher's tree target (src/target or _build/cargo-target) is mandatory.
Each child process group has a deadline.
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

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_panic import named_test_killed
from negative_process import mutating_sources, run_owned
from negative_selection import select

fixture = Path(__file__).resolve().parent
workspace = fixture.parents[3]
repo = workspace.parent
turn = workspace / 'pilotdeck-gateway/src/runtime/turn'
guard = turn / 'failure_guard.rs'
submit = turn / 'submit.rs'
out = Path(sys.argv[1]).resolve()
out.mkdir(parents=True, exist_ok=True)
env = dict(os.environ, CARGO_NET_OFFLINE='true')
# Accept the cargo workspace's own target and the repository's shared
# `_build/cargo-target`, like the other #813 runners; resolve both sides so a
# symlinked src/target still matches either spelling.
target_dir = Path(env['CARGO_TARGET_DIR']).resolve()
assert target_dir in {(workspace / 'target').resolve(), (repo / '_build/cargo-target').resolve()}, \
    'use this tree src/target or _build/cargo-target'
assert env.get('PDRS_BASELINE'), 'set PDRS_BASELINE'
for key in ['PDRS_SUBMIT_CASE', 'PDRS_SUBMIT_CAPABILITY']:
    env.pop(key, None)
with mutating_sources(repo, [guard, submit]) as snapshots:
    original = {p: snapshots[p] for p in [guard, submit]}
    pure = 'runtime::turn::failure_guard::tests::real_ts_failure_guard_and_classify'
    replay = 'runtime::turn::submit::tests::real_ts_submit_scenarios'

    def submit_marker(case, index, step):
        return f'#813 {case}: step {index} "{step}": submit scenario must match'

    def guard_trip_marker(case, action):
        return (f'#813 failure guard trip: {case} action {action}: '
                'trip result bytes must match')

    def guard_message_marker(case, action):
        return (f'#813 failure guard message: {case} action {action}: '
                'trip message bytes must match')

    # #813 fix-r8: every mutant names the complete message of the one assertion
    # that must fail. There is no `case: step` / `failure guard` fallback: those
    # prefixes also matched another assertion in the same test (sibling P1).
    def mutation(name, path, before, after, case=None, marker=None):
        assert original[path].decode().count(before) == 1, (name, 'anchor not unique')
        assert marker is not None, (name, 'needs an explicit target assertion message')
        assert marker.startswith('#813 '), (name, 'marker must be a #813 assertion message', marker)
        assert marker not in ('#813 failure guard', '#813 assertion'), (name, marker)
        return dict(name=name, path=path, before=before, after=after, test=replay if case else pure,
            case=case, marker=marker, target=marker)

    accumulate = 'accumulate-success-does-not-reset-and-trip-latches'
    model_window = 'model-window-and-reset'
    longest = 'longest-prefix-shared-across-subagent'
    mutants = [
        mutation('tool-threshold-plus-one', guard, '*failures < limit', '*failures < limit + 1.0',
            marker=guard_trip_marker(accumulate, 4)),
        mutation('tool-threshold-minus-one', guard, '*failures < limit', '*failures < limit - 1.0',
            marker=guard_trip_marker(accumulate, 2)),
        mutation('model-threshold-plus-one', guard, 'self.model_failures < limit',
            'self.model_failures < limit + 1.0', marker=guard_trip_marker(model_window, 3)),
        mutation('model-threshold-minus-one', guard, 'self.model_failures < limit',
            'self.model_failures < limit - 1.0', marker=guard_trip_marker(model_window, 2)),
    ]
    mutants += [
        mutation('counted-classification-wrong', guard, '"tool_unavailable",', '"permission_denied",',
            marker=guard_trip_marker(accumulate, 4)),
        mutation('prefix-shortest-not-longest', guard, '.max_by_key(|(unit, _)| unit.units().len())', '.min_by_key(|(unit, _)| unit.units().len())',
            marker=guard_trip_marker(longest, 2)),
        mutation('exact-unit-skipped', guard, 'if let Some(limit) = limits.get_key(name)', 'if let Some(limit) = limits.get_key(name).filter(|_| false)',
            marker=guard_trip_marker(accumulate, 4)),
        mutation('success-clears-window', guard, 'if !is(result, "type", "error") {', 'if !is(result, "type", "error") { self.tool_failures.clear();',
            marker=guard_trip_marker(accumulate, 4)),
        # #813 fix-r10: the disabled latch cannot show on a fresh guard
        # (`tripped` is already false, so `&& false` is a no-op). It first
        # shows after the accumulate trip: action 6 re-counts mcp__db__exact
        # and returns a second trip where the gold is null.
        mutation('trip-latch-ignored', guard, 'if self.tripped || ok ||', 'if (self.tripped && false) || ok ||',
            marker=guard_trip_marker(accumulate, 6)),
        mutation('trip-message-changed', guard, '次，已自动停止，请稍后重试', '次，已自动停止，请重试',
            marker=guard_message_marker(accumulate, 4)),
        mutation('disabled-policy-observed', submit, '.filter(|config| truthy(field(config, "enabled")))', '.filter(|config| { let _ = config; true })', 'guard-disabled',
            submit_marker('guard-disabled', 2, 'terminal')),
        mutation('model-not-observed', submit, '&& let Some(trip) = guard.observe(&event)', '&& !is(&event, "type", "turn_continued") && let Some(trip) = guard.observe(&event)', 'guard-model-trip',
            submit_marker('guard-model-trip', 2, 'terminal')),
        mutation('tool-not-observed', submit, '&& let Some(trip) = guard.observe(&event)', '&& !is(&event, "type", "tool_result") && let Some(trip) = guard.observe(&event)', 'guard-tool-trip',
            submit_marker('guard-tool-trip', 2, 'terminal')),
        mutation('subagent-not-observed', submit, '&& let Some(trip) = guard.observe(&event)', '&& !is(&event, "type", "subagent_tool_result") && let Some(trip) = guard.observe(&event)', 'guard-subagent-trip',
            submit_marker('guard-subagent-trip', 2, 'terminal')),
        mutation('trip-still-admitted', submit, 'let _ = handle.abort(&format!("run_policy:{code}").into());', 'let _ = (&handle, &code);', 'guard-model-trip',
            submit_marker('guard-model-trip', 2, 'terminal')),
        mutation('trip-status-not-recorded', submit, 'let _ = self.host.record_failure(run, &record).await;\n        publisher.gateway_event(object([', 'let _ = record;\n        publisher.gateway_event(object([', 'guard-tool-trip',
            submit_marker('guard-tool-trip', 2, 'terminal')),
        mutation('record-error-propagated', submit, 'let _ = self.host.record_failure(run, &record).await;\n        publisher.gateway_event(object([', 'self.host.record_failure(run, &record).await?;\n        publisher.gateway_event(object([', 'guard-trip-status-record-failed',
            submit_marker('guard-trip-status-record-failed', 2, 'terminal')),
        mutation('model-metadata-dropped', submit, 'build_guard_trip_message(trip, model.as_ref())', 'build_guard_trip_message(trip, model.as_ref().filter(|_| false))', 'guard-model-trip',
            submit_marker('guard-model-trip', 2, 'terminal')),
        mutation('trip-abort-reason-wrong', submit, 'format!("run_policy:{code}")', 'format!("policy:{code}")', 'guard-tool-trip',
            submit_marker('guard-tool-trip', 2, 'terminal')),
    ]
    mutants += [
        mutation('guard-wire-order-changed', submit, '("runId", JsValue::String(run.run_id.clone())),\n            ("event", "run_policy_stopped".into()),', '("event", "run_policy_stopped".into()),\n            ("runId", JsValue::String(run.run_id.clone())),', 'guard-model-trip',
            submit_marker('guard-model-trip', 2, 'terminal')),
        mutation('next-window-keeps-model-count', submit, '.map(FailureGuard::new);', '.map(|config| { let mut guard = FailureGuard::new(config); if run.run_id.equals_str("r2") { let _ = guard.on_model_failure(); } guard });', 'guard-reset-next-turn',
            submit_marker('guard-reset-next-turn', 4, 'second')),
    ]
    # Take the real configuration snapshot before refresh rather than after it.
    submit_text = original[submit].decode()
    start = submit_text.index('        let _ = self.host.refresh().await;')
    end = submit_text.index('        let handle = self', start)
    block = submit_text[start:end]
    config_start = block.index('        let mut failure_guard = self')
    before_refresh = block[config_start:] + block[:config_start]
    mutants.append(mutation('config-snapshotted-before-refresh', submit, block, before_refresh, 'guard-refresh-enables',
        submit_marker('guard-refresh-enables', 2, 'terminal')))

    # #813 fix-r4: both baseline rows survive a subset; a failing baseline
    # breaks before any mutant verdict.
    selected = select(
        [dict(name='baseline-pure', test=pure, marker=None),
         dict(name='baseline-submit', test=replay, marker=None)] + mutants,
        sys.argv[2:], name=lambda row: row['name'],
        always=('baseline-pure', 'baseline-submit'))
    command = ['cargo', 'test', '--offline', '--manifest-path', str(workspace / 'Cargo.toml'), '-p', 'pilotdeck-gateway', '--lib']
    summary = []
    deadline = time.monotonic() + 1800
    try:
        (out / 'original-sha256.json').write_text(json.dumps({str(p.relative_to(repo)): hashlib.sha256(data).hexdigest() for p, data in original.items()}, indent=2) + '\n')
        for control in selected:
            # #813 fix-r10: a failure must not shorten the run. Record the
            # remaining controls as INVALID at the deadline instead of
            # aborting, so one host round exposes every mismatch (#813 r8
            # stopped at the first INVALID and hid the rest).
            if time.monotonic() >= deadline:
                summary.append(dict(name=control['name'], case=control.get('case'), marker=control.get('marker'),
                    before=control.get('before'), after=control.get('after'), pid=None, reaped=False,
                    exit=None, timed_out=False, valid=False, reason='overall 30 minute deadline'))
                (out / 'summary.json').write_text(json.dumps(summary, ensure_ascii=False, indent=2) + '\n')
                print(f'{control["name"]}: INVALID (overall 30 minute deadline)', flush=True)
                continue
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
            timed_out = False
            budget = min(240, max(1, deadline - time.monotonic()))
            try:
                run = run_owned(cmd, cwd=repo, env=run_env, timeout=budget)
                raw = run.stdout + run.stderr
                returncode = run.returncode
            except subprocess.TimeoutExpired as error:
                timed_out = True
                raw = (error.output or '') + (error.stderr or '')
                returncode = -signal.SIGKILL
            finally:
                for path, data in original.items():
                    assert path.read_bytes() == modified[path], f'concurrent edit preserved: {path}'
                    if modified[path] != data: path.write_bytes(data)
            marker = control['marker']
            valid = not timed_out and ((returncode == 0 and 'test result: ok. 1 passed; 0 failed;' in raw
                and f'test {control["test"]} ... ok' in raw) if marker is None else (
                returncode == 101 and 'test result: FAILED' in raw
                and named_test_killed(raw, control['test'], marker, control.get('target'))
                and 'could not compile' not in raw))
            (out / f'{control["name"]}.log').write_text('COMMAND ' + ' '.join(cmd) + '\n' + slots.stdout + slots.stderr + raw + f'\nexit={returncode} timed_out={timed_out}\n')
            if valid:
                reason = 'killed' if marker is not None else 'baseline passed'
            elif timed_out:
                reason = 'timeout (watchdog), not an assertion failure'
            elif 'could not compile' in raw:
                reason = 'compile error'
            elif returncode == 0:
                reason = 'survived (test passed)'
            elif returncode != 101:
                reason = f'unexpected exit {returncode}'
            else:
                reason = 'failed without the declared marker'
            # run_owned reaps the group itself, so the leader pid is not exported.
            summary.append(dict(name=control['name'], case=control.get('case'), marker=marker, before=control.get('before'), after=control.get('after'), pid=None, reaped=True, exit=returncode, timed_out=timed_out, valid=valid, reason=reason))
            (out / 'summary.json').write_text(json.dumps(summary, ensure_ascii=False, indent=2) + '\n')
            print(f'{control["name"]}: {"PASS" if valid else "INVALID"} exit={returncode} ({reason})', flush=True)
    finally:
        assert all(p.read_bytes() == data for p, data in original.items()), 'original bytes not restored'
        (out / 'restored-sha256.json').write_text(json.dumps({str(p.relative_to(repo)): hashlib.sha256(data).hexdigest() for p, data in original.items()}, indent=2) + '\n')
    raise SystemExit(0 if len(summary) == len(selected) and all(row['valid'] for row in summary) else 1)
