#!/usr/bin/env python3
"""Run against a committed tree, or an explicit isolated index/tree baseline."""
import hashlib
import json
import os
from pathlib import Path
import sys
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot
root = Path(__file__).resolve().parents[5]
out = Path(sys.argv[1]).resolve()
out.mkdir(parents=True, exist_ok=False)
baseline = sys.argv[2] if len(sys.argv) > 2 else 'HEAD'
helper = root / 'src/pilotdeck-gateway/src/runtime/cleanup_task.rs'
source = root / 'src/pilotdeck-gateway/src/runtime/memory/maintenance.rs'
refresh = root / 'src/pilotdeck-gateway/src/runtime/refresh.rs'
env = os.environ.copy()
if not env.get('CARGO_TARGET_DIR'):
    sys.exit('CARGO_TARGET_DIR must be supplied by the caller')
mutations = [
    ('cancel-reported-as-error', source, 'Err(_) => return, // Executor cancelled this invocation.', 'Err(_) => Err(MemoryProviderError::new("cancelled")),', 'cancelled_invocation_releases_flight_and_retries'),
    ('cancel-flight-leaked', source, 'Flight(Some(state.clone()))', 'Flight(None)', 'cancelled_invocation_releases_flight_and_retries'),
    ('panic-swallowed', source, 'Ok(payload) => Err(panic_error(payload)),', 'Ok(_payload) => return,', 'panic_after_await'),
    ('panic-no-telemetry', source, 'inner.telemetry.track_error(&error, JsValue::Object(input));', 'let _ = input;', 'panic_after_await'),
    ('panic-no-diagnostic', source, '(inner.diagnostic)(&JsString::from_utf16(message), &error);', 'let _ = message;', 'panic_after_await'),
    ('panic-lost-request', source, 'state.requested = true;', 'state.requested = false;', 'panic_requested'),
    ('panic-stuck-flight', source, 'state.in_flight = false;\n                    // Disarm', 'state.in_flight = true;\n                    // Disarm', 'panic_retry'),
    ('panic-string-lost', helper, 'payload.downcast_ref::<String>().map(String::as_str)', 'None', 'panic_string'),
    ('panic-str-lost', helper, '.downcast_ref::<&str>()\n        .copied()', '.downcast_ref::<&str>()\n        .and(None)' , 'panic_after_await'),
    ('panic-fallback-lost', source, '.unwrap_or("memory maintenance panicked")', '.unwrap_or("wrong panic")', 'panic_opaque'),
    ('repoll-completed', source, 'first = Box::pin(std::future::ready(result));', 'let _ = result;', 'before_await'),
    ('production-root', source, 'self.project_root().to_string_lossy().as_ref().into()', '"/wrong".into()', 'runtime::project::factory::tests::memory_bundle_tests::memory_factory_injects_provider_and_publishes_three_handles'),
    ('production-handle', source, 'resource(Resource::MemoryService)?', 'resource(Resource::Memory)?', 'runtime::project::factory::tests::memory_bundle_tests::memory_factory_injects_provider_and_publishes_three_handles'),
    ('late-first-poll', source, 'first.as_mut().poll(&mut Context::from_waker(Waker::noop()))', 'Poll::<Result<(), MemoryProviderError>>::Pending', 'before_await'),
    ('missing-is-error', source, 'let Some(service) = runtime.memory_service() else {\n            return Ok(());', 'let Some(service) = runtime.memory_service() else {\n            panic!("missing service");', 'absent'),
    ('wrong-success', source, '"outcome":"success"', '"outcome":"failure"', 'idle'),
    ('no-unwind-guard', source, 'Flight(Some(state.clone()))', 'Flight(None)', 'concurrent_requests_and_first_poll_unwind'),
    ('no-cancel-guard', source, 'Flight(Some(state.clone()))', 'Flight(None)', 'never'),
    ('no-coalescing', source, 'if state.in_flight {', 'if false && state.in_flight {', 'coalesce'),
    ('lost-request', source, 'state.requested = true;', 'state.requested = false;', 'coalesce'),
    ('no-finally-restart', source, 'if !std::mem::take(&mut state.requested) {', 'if true || !std::mem::take(&mut state.requested) {', 'settling'),
    ('cross-generation', source, 'key.ptr_eq(&Arc::downgrade(&runtime))', 'key.strong_count() > 0', 'generations'),
    ('failure-escapes', source, 'Err(error) => {\n                        let mut input', 'Err(error) => { panic!("maintenance escaped: {error:?}");\n                        let mut input', 'reject_requested'),
    ('no-error-telemetry', source, 'inner.telemetry.track_error(&error, JsValue::Object(input));', 'let _ = input;', 'reject'),
    ('no-diagnostic', source, '(inner.diagnostic)(&JsString::from_utf16(message), &error);', 'let _ = message;', 'reject'),
    ('wrong-reason', source, 'run_due_scheduled_maintenance("scheduled")', 'run_due_scheduled_maintenance("manual")', 'idle'),
    ('refresh-swallows', refresh, 'options.memory_maintenance.schedule(project_key)', 'let _ = options.memory_maintenance.schedule(project_key); Ok(())', 'resolver_error'),
]
selected = sys.argv[3:]
if selected:
    mutations = [m for m in mutations if m[0] in selected]
    assert len(mutations) == len(selected), 'unknown mutation'
summary = []
def run(name, test):
    slots = wait_for_slot(root / 'scripts/cargo-slots.sh')
    (out / (name + '.slots.log')).write_text(slots.stdout + slots.stderr)
    test = test if test.startswith('runtime::') else 'runtime::memory::maintenance::tests::' + test
    result = run_owned(['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml', '-p', 'pilotdeck-gateway', '--lib', test, '--', '--exact'], cwd=root, env=env, timeout=240)
    log = result.stdout + result.stderr
    with (out / (name + '.log')).open('x') as f: f.write(log)
    return result, log
with mutating_sources(root, [source, refresh, helper], baseline=baseline) as snapshots:
    for name, path, old, new, test in mutations:
        original = snapshots[path]
        text = original.decode()
        assert text.count(old) == 1, (name, text.count(old))
        try:
            path.write_text(text.replace(old, new))
            result, log = run(name, test)
            killed = result.returncode == 101 and f'{test} ... FAILED' in log and 'test result: FAILED' in log
            summary.append({'name':name, 'killed':killed, 'sourceSha256':hashlib.sha256(original).hexdigest()})
            print(name, 'KILLED' if killed else 'NOT KILLED', flush=True)
            assert killed, 'compile failure/signal/zero tests is not a negative control'
        finally:
            path.write_bytes(original)
# The context manager verifies restoration against the same Git tree.
with mutating_sources(root, [source, refresh, helper], baseline=baseline):
    slots = wait_for_slot(root / 'scripts/cargo-slots.sh')
    result = run_owned(['cargo','test','--offline','--manifest-path','src/Cargo.toml','-p','pilotdeck-gateway','--lib','runtime::memory::maintenance::tests'],cwd=root,env=env,timeout=240)
    with (out / 'restored.log').open('x') as f: f.write(result.stdout + result.stderr)
    assert result.returncode == 0
    if any(m[0] == 'production-handle' for m in mutations):
        result, log = run('restored-production-handle', next(m[4] for m in mutations if m[0] == 'production-handle'))
        assert result.returncode == 0 and '1 passed' in log
with (out / 'summary.json').open('x') as f: json.dump(summary,f,indent=2)
print(f'{len(summary)}/{len(mutations)} KILLED; restored PASS',flush=True)
