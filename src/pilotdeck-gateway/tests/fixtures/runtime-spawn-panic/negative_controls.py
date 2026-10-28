#!/usr/bin/env python3
# anchor-selfcheck: unsupported conditional sequential re.subn rewrites of changed content with different flags, not whole-file table anchors
"""Run after the patch is committed; mutate only under the shared source guard."""
import argparse
import json
import os
from pathlib import Path
import re
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

root = Path(__file__).resolve().parents[5]
if not os.environ.get('CARGO_TARGET_DIR'):
    sys.exit('CARGO_TARGET_DIR must be supplied by the caller')
parser = argparse.ArgumentParser()
parser.add_argument('output')
parser.add_argument('--only', action='append', default=[])
args = parser.parse_args()
out = Path(args.output).resolve()
out.mkdir(parents=True, exist_ok=False)
helper = 'src/pilotdeck-gateway/src/runtime/cleanup_task.rs'
mcp = 'src/pilotdeck-gateway/src/runtime/mcp/registry.rs'
pub = 'src/pilotdeck-gateway/src/runtime/turn/publish.rs'
background = 'src/pilotdeck-gateway/src/runtime/background.rs'
fixture = 'src/pilotdeck-gateway/tests/fixtures/runtime-spawn-panic/oracle.json'
tail_fixture = 'src/pilotdeck-gateway/tests/fixtures/background-clock/oracle.json'
shared = 'shared_spawn_panic_matrix'
durable = 'durable_spawn_panic_matrix'
payload = 'shared_spawn_outcomes_and_cancelled_gate'
construction = 'durable_construction_panic_releases_fifo_and_all_waiters'
reservation = 'initial_snapshot_construction_panic_releases_all_reservations'
# Every mutant has its own test and runtime assertion anchor. No compile failures count.
mutants = [
    ('executor-drop', pub, r'impl Drop for PublicationWrite \{.*?\n\}', 'impl Drop for PublicationWrite { fn drop(&mut self) {} }', 'durable_executor_drop_before_first_poll', 'executor drop pending cleanup'),
    ('executor-pending-predecessor', pub, r'impl Drop for PublicationWrite \{.*?\n\}', 'impl Drop for PublicationWrite { fn drop(&mut self) {} }', 'durable_executor_drop_with_pending_predecessor', 'executor drop pending cleanup'),
    ('executor-payload-drop', pub, r'impl Drop for PublicationWrite \{.*?\n\}', 'impl Drop for PublicationWrite { fn drop(&mut self) {} }', 'durable_executor_drop_payload_behind_pending_predecessor', 'executor drop pending cleanup'),
    ('executor-notify', pub, r'(?m)^        publication\.changed\.notify_waiters\(\);', '        publication.changed.notify_one();', 'durable_executor_drop_before_first_poll', 'executor drop wakes every observer'),
    ('executor-stopped', pub, r'impl Drop for PublicationWrite \{.*?\n\}', 'impl Drop for PublicationWrite { fn drop(&mut self) {} }', 'durable_executor_already_closed_releases_new_writes', 'spawn completion deadline'),
    ('executor-closed', pub, re.escape('Err(_) if !state.failure_published => {\n                state.failure_published = true;\n                if !state.closed {'), 'Err(_) if !state.failure_published => {\n                state.failure_published = true;\n                if true {', 'durable_executor_drop_closed_before_first_poll', 'closed executor cannot reopen live delivery'),
    ('pump-poison-unwrap', pub, re.escape('pub fn emit(&self, event: &JsValue) -> bool {\n        let _pump = lock(&self.pump);'), 'pub fn emit(&self, event: &JsValue) -> bool {\n        let _pump = self.pump.lock().unwrap();', 'pump_poison_does_not_disable_publication', 'PoisonError'),
    ('durable-construction', pub, re.escape('Err(_) if !state.failure_published =>'), 'Err(_) if false && !state.failure_published =>', construction, 'construction complete visible trace'),
    ('shared-join', mcp, r'let outcome = task.await.unwrap_or_else\(\|error\| \{.*?\n        \}\);', 'let outcome = task.await.unwrap();', shared, 'shared failure:'),
    ('shared-retain', mcp, r'sender.send_replace\(Some\(outcome\)\);', 'drop((sender, outcome));', shared, 'shared failure:'),
    ('shared-owned-payload', helper, re.escape('.or_else(|| payload.downcast_ref::<String>().map(String::as_str))'), '', payload, 'panic payload'),
    ('shared-fallback', mcp, re.escape('.unwrap_or("MCP lifecycle task ended unexpectedly")'), '.unwrap_or("wrong fallback")', payload, 'panic payload'),
    ('durable-join', pub, r'let outcome = match future \{.*?\n            \};', 'let outcome = match future { Ok(future) => { let _ = publication.executor.spawn(future).await; Ok(Ok(())) }, Err(payload) => Err(payload) };', durable, 'failure event:'),
    ('durable-pending', pub, re.escape('state.pending.remove(&write);'), 'let _ = write;', durable, 'spawn completion deadline'),
    ('durable-notify', pub, r'(?m)^        publication\.changed\.notify_waiters\(\);', '            publication.changed.notify_one();', durable, 'spawn completion deadline|every drain waiter notified'),
    ('durable-failure', pub, re.escape('Err(_) if !state.failure_published =>'), 'Err(_) if false && !state.failure_published =>', durable, 'spawn completion deadline'),
    ('durable-once', pub, re.escape('Err(_) if !state.failure_published =>'), 'Err(_) =>', durable, 'single failure:'),
    ('supervisor-closed', mcp, re.escape('Err(_) => Err(RuntimeError::new("MCP lifecycle task ended unexpectedly"))'), 'Err(_) => Ok(())', 'shared_supervisor_panics_before_send_close_all_observers', 'called `Result::unwrap_err()`'),
    ('supervisor-dropped', mcp, re.escape('Err(_) => Err(RuntimeError::new("MCP lifecycle task ended unexpectedly"))'), 'Err(_) => Ok(())', 'shared_supervisor_dropped_before_send_closes_all_observers', 'called `Result::unwrap_err()`'),
    ('durable-poison-cleanup', pub, re.escape('let mut state = lock(&publication.state);'), 'let Ok(mut state) = publication.state.lock() else { return; };', 'durable_poisoned_state_cleans_pending_and_wakes_all', 'spawn completion deadline'),
    ('durable-poison-wait', pub, re.escape('if lock(&self.state).pending.is_disjoint(writes)'), 'if self.state.lock().unwrap().pending.is_disjoint(writes)', 'durable_poisoned_state_cleans_pending_and_wakes_all', 'PoisonError'),
    ('durable-poison-enqueue', pub, re.escape('let mut state = lock(&self.state);\n        let write = state.next_write;'), 'let mut state = self.state.lock().unwrap();\n        let write = state.next_write;', 'durable_poisoned_state_cleans_pending_and_wakes_all', 'PoisonError'),
    ('durable-construction-catch', pub, re.escape('let future = catch_unwind(AssertUnwindSafe(|| registry.append(&self.run, &event)));'), 'let future: Result<_, Box<dyn std::any::Any + Send>> = Ok(registry.append(&self.run, &event));', construction, 'injected construction panic'),
    ('durable-construction-fifo', pub, re.escape('let _ = previous.await;'), 'drop(previous);', construction, 'construction successor cannot overtake pending predecessor'),
    ('durable-payload-cleanup', pub, re.escape('completion.finish(result, event);'), 'std::mem::forget(completion); let _ = (result, event);', 'durable_panic_payload_drop_runs_after_cleanup', 'spawn completion deadline'),
    ('background-construction', background, re.escape('match catch_port(callback) {'), 'match Ok::<_, Box<dyn std::any::Any + Send>>(callback()) {', reservation, 'injected construction panic'),
    ('background-capacity', background, re.escape('lock(&self.inner.state).pending_starts -= 1;'), 'let _ = &self.inner;', reservation, 'construction reservation matrix'),
    ('background-start-drain', background, r'(?m)^        self\.inner\.starts\.send_modify\(\|starts\| \*starts -= 1\);', '        let _ = &self.inner.starts;', reservation, 'construction dispose deadline'),
    ('background-str-payload', helper, re.escape('.downcast_ref::<&str>()\n        .copied()'), '.downcast_ref::<&str>().and(None)', reservation, 'construction panic error'),
    ('background-owned-payload', helper, re.escape('.or_else(|| payload.downcast_ref::<String>().map(String::as_str))'), '', 'initial_snapshot_construction_panic_payloads', 'construction panic payload'),
    ('background-fallback', background, re.escape('            "Background initial snapshot construction failed.",'), '            "wrong fallback",', 'initial_snapshot_construction_panic_payloads', 'construction panic payload'),
    ('clock-catch', background, re.escape('match catch_port(callback) {'), 'match Ok::<_, Box<dyn std::any::Any + Send>>(callback()) {', 'clock_start_panic_releases_reservations_and_observers', 'injected clock panic'),
    ('clock-locked', background, re.escape('started_at: reservation.call(|| (self.inner.now)(), "Background clock failed.")?,'), 'started_at: reservation.call(|| { let _state = lock(&self.inner.state); (self.inner.now)() }, "Background clock failed.")?,', 'clock_start_panic_releases_reservations_and_observers', 'WouldBlock'),
    ('clock-payload-order', background, re.escape('                self.release();'), '                let _ = self;', 'clock_start_payload_drop_happens_after_release', 'payload sees released starts'),
    ('clock-pending-snapshot', background, re.escape('        let task = result?;'), '', 'clock_start_failure_does_not_wait_for_snapshot', 'clock pending snapshot deadline'),
    ('clock-transfer', background, re.escape('        reservation.pending = false;'), '        reservation.pending = true;', 'clock_start_panic_releases_reservations_and_observers', 'attempt to subtract with overflow'),
    ('clock-idempotent', background, re.escape('        if self.released {\n            return;\n        }'), '', 'clock_start_panic_releases_reservations_and_observers', 'attempt to subtract with overflow'),
    ('clock-drop', background, re.escape("impl Drop for StartReservation<'_> {\n    fn drop(&mut self) {\n        self.release();"), "impl Drop for StartReservation<'_> {\n    fn drop(&mut self) {\n        let _ = self;", 'clock_start_panic_releases_reservations_and_observers', 'clock dispose deadline'),
    ('tail-shell-clock', background, re.escape('let clock = catch_port(|| (inner.now)());\n                    task.ended_at'), 'let clock: std::thread::Result<f64> = Ok((inner.now)());\n                    task.ended_at', 'clock_tail_shell_exit', 'tail observer deadline'),
    ('tail-no-pid-clock', background, re.escape('let clock = catch_port(|| (inner.now)());\n        state.task.ended_at'), 'let clock: std::thread::Result<f64> = Ok((inner.now)());\n        state.task.ended_at', 'clock_tail_no_pid', 'tail observer deadline'),
    ('tail-subagent-clock', background, re.escape('let clock = catch_port(|| now());'), 'let clock: std::thread::Result<f64> = Ok(now());', 'clock_tail_subagent_finish', 'tail observer deadline'),
    ('tail-stop-clock', background, re.escape('let clock = catch_port(|| (self.now)());'), 'let clock: std::thread::Result<f64> = Ok((self.now)());', 'clock_tail_subagent_stop', 'tail observer deadline'),
    ('tail-shell-fallback', background, re.escape('let clock = catch_port(|| (inner.now)());\n                    task.ended_at = Some(*clock.as_ref().unwrap_or(&task.started_at));'), 'let clock = catch_port(|| (inner.now)());\n                    task.ended_at = Some(*clock.as_ref().unwrap_or(&0.0));', 'clock_tail_shell_exit', 'tail fallback timestamp'),
    ('tail-finish-fallback', background, re.escape('let clock = catch_port(|| now());\n                    task.ended_at = Some(*clock.as_ref().unwrap_or(&task.started_at));'), 'let clock = catch_port(|| now());\n                    task.ended_at = Some(*clock.as_ref().unwrap_or(&0.0));', 'clock_tail_subagent_finish', 'tail fallback timestamp'),
    ('tail-stop-fallback', background, re.escape('let clock = catch_port(|| (self.now)());\n        task.ended_at = Some(*clock.as_ref().unwrap_or(&task.started_at));'), 'let clock = catch_port(|| (self.now)());\n        task.ended_at = Some(*clock.as_ref().unwrap_or(&0.0));', 'clock_tail_subagent_stop', 'tail fallback timestamp'),
    ('tail-no-pid-fallback', background, re.escape('let clock = catch_port(|| (inner.now)());\n        state.task.ended_at = Some(*clock.as_ref().unwrap_or(&state.task.started_at));'), 'let clock = catch_port(|| (inner.now)());\n        state.task.ended_at = Some(*clock.as_ref().unwrap_or(&0.0));', 'clock_tail_no_pid', 'tail fallback timestamp'),
    ('tail-shutdown-payload', background, re.escape('let _ = catch_port(|| self.stop(&task_id));'), 'self.stop(&task_id);', 'clock_tail_shutdown_survives_payload_drop', 'tail shutdown isolates payload panic'),
    ('tail-pending-body', background, re.escape('if task.status == TaskStatus::Cancelled {\n                    None'), 'if false && task.status == TaskStatus::Cancelled {\n                    None', 'clock_tail_subagent_pending_stop', 'tail pending body must not run'),
    ('clock-failed-snapshot', background, r'                // The initial Pending write is already owned by the store\..*?                task.ended_at =', '                task.ended_at =', 'clock_start_failure_persists_terminal_snapshot', 'clock durable terminal snapshot'),
    ('clock-pending-evidence', tail_fixture, None, '', 'clock_pending_snapshot_oracle_preserves_ts_observation', 'clock pending TS evidence'),
    ('clock-durable-evidence', tail_fixture, None, '', 'clock_durable_oracle_preserves_ts_observation', 'clock durable TS evidence'),
    ('tail-matrix', tail_fixture, None, 'B-', 'tail_oracle_has_exact_matrix', 'complete tail matrix'),
    ('tail-duplicate', tail_fixture, None, 'S-', 'tail_oracle_has_exact_matrix', 'complete tail matrix'),
    ('tail-shell-evidence', tail_fixture, None, 'B-reject', 'tail_oracle_has_exact_matrix', 'tail TS shell evidence'),
    ('tail-payload', background, re.escape('    inner.settle(&entry);\n    drop(clock);'), '    drop(clock);\n    inner.settle(&entry);', 'clock_tail_payload_drop', 'tail payload sees done'),
    ('tail-shell-done', background, re.escape('        entry.done.send_replace(true);\n    }\n}'), '        entry.done.send_replace(false);\n    }\n}', 'clock_tail_shell_exit', 'tail observer deadline'),
    ('tail-subagent-done', background, re.escape('            entry.done.send_replace(true);\n            drop(clock);'), '            entry.done.send_replace(false);\n            drop(clock);', 'clock_tail_subagent_finish', 'tail observer deadline'),
    ('tail-stop-signal', background, re.escape('        entry.controller.cancel_synchronously();\n        drop(clock);'), '        drop(clock);', 'clock_tail_subagent_stop', 'tail observer deadline'),
    ('tail-shell-event', background, re.escape('        if let (Some(event), Some(handler)) = (event, &self.on_completion) {'), '        if let (Some(event), Some(handler)) = (event.filter(|_| false), &self.on_completion) {', 'clock_tail_shell_exit', 'tail completion exactly once'),
    ('tail-subagent-payload', background, re.escape('            entry.done.send_replace(true);\n            drop(clock);'), '            drop(clock);\n            entry.done.send_replace(true);', 'clock_tail_payload_drop', 'tail payload sees completion or cancellation'),
    ('tail-stop-payload', background, re.escape('        entry.controller.cancel_synchronously();\n        drop(clock);'), '        drop(clock);\n        entry.controller.cancel_synchronously();', 'clock_tail_payload_drop', 'tail payload sees completion or cancellation'),
    ('tail-payload-leak', background, re.escape('    inner.settle(&entry);\n    drop(clock);'), '    inner.settle(&entry);\n    std::mem::forget(clock);', 'clock_tail_payload_drop', 'tail observer deadline'),
    ('construction-matrix', fixture, None, 'construct-', 'construction_oracle_has_exact_matrix', 'complete construction matrix'),
    ('construction-duplicate', fixture, None, 'construct-', 'construction_oracle_has_exact_matrix', 'complete construction matrix'),
    ('shared-matrix', fixture, None, 'shared-', shared, 'complete shared matrix'),
    ('durable-matrix', fixture, None, 'durable-', durable, 'complete durable matrix'),
    ('shared-duplicate', fixture, None, 'shared-', shared, 'assertion failed: ids.insert(id)'),
    ('durable-duplicate', fixture, None, 'durable-', durable, 'assertion failed: ids.insert(id)'),
]

def record(name, data):
    with (out / name).open('x') as file:
        file.write(data)

def run(test, name):
    slots = wait_for_slot(root / 'scripts/cargo-slots.sh')
    record(name + '.slots.log', slots.stdout + slots.stderr)
    result = run_owned(['cargo', 'test', '--manifest-path', 'src/Cargo.toml', '-p',
        'pilotdeck-gateway', '--lib', test], cwd=root, env=os.environ.copy(), timeout=900)
    record(name + '.log', result.stdout + result.stderr)
    return result, result.stdout + result.stderr

if args.only:
    unknown = set(args.only) - {m[0] for m in mutants}
    if unknown:
        sys.exit(f'unknown mutants: {sorted(unknown)}')
    mutants = [m for m in mutants if m[0] in args.only]

for name, relative, pattern, replacement, test, anchor in mutants:
    path = root / relative
    with mutating_sources(root, [path]) as original:
        content = original[path].decode()
        if pattern is None:
            oracle = json.loads(content)
            if name == 'clock-pending-evidence':
                oracle['pendingSnapshotCase']['settled'] = True
            elif name == 'clock-durable-evidence':
                oracle['durableCase']['persisted'][0]['status'] = 'unknown'
            else:
                cases = oracle['tailCases' if relative == tail_fixture else 'cases']
                index = next(i for i, c in enumerate(cases) if c['id'].startswith(replacement))
                if name.endswith('-duplicate'): cases.append(cases[index].copy())
                elif name == 'tail-shell-evidence': cases[index]['done'] = True
                else: cases.pop(index)
            changed = json.dumps(oracle) + '\n'
        else:
            changed, count = re.subn(pattern, replacement, content, flags=re.S)
            assert count == 1, (name, count)
        if name == 'clock-pending-snapshot':
            changed, count = re.subn(re.escape('        Ok(task)\n    }\n\n    fn insert('), '        result\n    }\n\n    fn insert(', changed)
            assert count == 1, (name, 'return anchor', count)
        path.write_text(changed)
        result, log = run(test, name)
        assert result.returncode == 101 and f'{test} ... FAILED' in log and any(part in log for part in anchor.split('|')) and 'test result: FAILED' in log, (name, 'not an anchored runtime failure')
        print(f'{name}: KILLED ({test}: {anchor})', flush=True)
    with mutating_sources(root, [path]):
        result, log = run(test, name + '-restored')
        assert result.returncode == 0 and f'{test} ... ok' in log, (name, 'restored test failed')
print(f'{len(mutants)} mutants killed and restored', flush=True)
