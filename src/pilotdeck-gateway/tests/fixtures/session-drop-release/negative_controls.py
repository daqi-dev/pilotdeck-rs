#!/usr/bin/env python3
"""#1010 guarded product mutations. Commit before running; never bypass HEAD.

--audit is read-only. --compile OUT [names] proves every selected mutant builds.
OUT [names] runs the shared baseline/mutant/restored suite, accounting for all.
The full suite is intentionally a coordinator/host job, not a background task.
"""
import json
import os
from pathlib import Path
import sys

root = Path(__file__).resolve().parents[5]
fixtures = root / 'src/pilotdeck-gateway/tests/fixtures'
sys.path[:0] = [str(fixtures), str(fixtures / 'turn-runner')]
from negative_process import mutating_sources, run_owned, wait_for_slot, tree_lock
from negative_suite import Case, classify, invalidate_completed, passed
from _negative_common import mutate

handle = root / 'src/pilotdeck-gateway/src/runtime/session/handle.rs'
factory = root / 'src/pilotdeck-gateway/src/runtime/session/factory.rs'
oracle = Path(__file__).with_name('oracle.json')
generator = Path(__file__).with_name('generate.mts')
drop_tests = root / 'src/pilotdeck-gateway/src/runtime/session/handle_drop_tests.rs'
factory_tests = root / 'src/pilotdeck-gateway/src/runtime/session/factory_tests.rs'
contributions = root / 'src/pilotdeck-gateway/src/runtime/plugins/contributions.rs'
lease_tests = root / 'src/pilotdeck-gateway/src/runtime/plugins/catalog_lease_tests.rs'
baseline = root / 'src/pilotdeck-gateway/tests/common/baseline.rs'
lease_prefix = 'runtime::plugins::catalog::lease_tests::'
drop_prefix = 'runtime::session::handle::drop_contract_tests::'
factory_prefix = 'runtime::session::factory::tests::'
cases = [
    # cell 1: removing the drop-side entry leaves the drain unstarted.
    ('drop-dispose-removed', handle,
     'self.start_dispose("agent_handle_dropped".into());', 'let _ = self;',
     None, drop_prefix+'dropped_handle_runs_session_dispose_once',
     '#1010 dropped handle started the drain'),
    # cells 2/6: ambient tokio::spawn is the "spawn and hope" the issue forbids;
    # the plain-thread panic is swallowed by the shared catch_unwind, so the
    # kill surfaces as the bounded drain wait (or the thread join) failing.
    ('drop-dispose-ambient-spawn', handle,
     'executor.spawn(async move {', 'tokio::spawn(async move {',
     None, drop_prefix+'dropped_handle_from_plain_thread_uses_captured_executor',
     ['#1010 plain-thread drop finished without a runtime',
      '#1010 captured executor ran the drop drain']),
    # cell 3: without the in-future fallback the dead runtime leaks the writer.
    ('drop-fallback-removed', handle,
     'session: Some(session.clone()),', 'session: None,',
     None, drop_prefix+'dropped_handle_without_runtime_lands_writer_fallback',
     '#1010 dead runtime: the fallback still takes the writer guard'),
    # cells 4/5: dropping the idempotency check restarts a completed drain.
    ('drop-restart-removed-idempotency', handle,
     'if let Some(completion) = &progress.disposal {\n            return completion.clone();\n        }',
     'if let Some(_completion) = &progress.disposal {\n        }',
     None, drop_prefix+'explicit_dispose_then_drop_runs_no_second_dispose',
     '#1010 dropping a disposed handle runs no second dispose'),
    # cell 7: ambient-only registration-time executor skips the off-runtime drop.
    ('prepared-drop-ambient-only', factory,
     'self.executor.take()', 'tokio::runtime::Handle::try_current().ok()',
     None, factory_prefix+'prepared_resources_released_when_dropped_off_runtime',
     '#1010 off-runtime release ran bounded wait'),
    # cell 7: never spawning the release leaves the custom async cleanup unrun
    # (the if-let can never match, so the releases drop uninvoked).
    ('prepared-drop-release-removed', factory,
     'if let (Some(executor), false) = (executor, self.releases.is_empty()) {',
     'if let (Some(executor), false) = (executor, true) {',
     None, factory_prefix+'prepared_resources_released_when_dropped_off_runtime',
     '#1010 off-runtime release ran bounded wait'),
    # fixture: a tampered oracle must fail the byte-identical regeneration.
    ('fixture-regeneration', oracle,
     '"name": "handle-drop-gc",', '"name": "wrong-handle-drop-gc",',
     None, drop_prefix+'drop_release_regenerates_identically',
     '#280 session-drop-release/oracle.json differs'),
    # generator: the in-loop deref pins the handle to the current job, so gc()
    # can never observe collection and the fail-closed assert must fire.
    ('generator-gc-deref-in-loop', generator,
     'for(let round=0;round<5;round++){\n  gc();\n  gc({type:\'major\',execution:\'sync\'});\n'
     '  await new Promise(resolve=>setTimeout(resolve,10));\n }\n const collected=weak.deref()===undefined;',
     'let collected=false;\n for(let round=0;round<5&&!collected;round++){\n  gc();\n'
     '  gc({type:\'major\',execution:\'sync\'});\n'
     '  await new Promise(resolve=>setTimeout(resolve,10));\n'
     '  collected=weak.deref()===undefined;\n }',
     None, drop_prefix+'drop_release_regenerates_identically',
     '#280 session-drop-release: generator failed'),
    # generator: the process-exit child must exit non-zero before its pending
    # async cleanup lands; a zero exit trips the generator's own fail-closed
    # assert, so the regeneration guard sees the generator itself fail.
    ('generator-child-exit-code', generator,
     'process.exit(7);', 'process.exit(0);',
     None, drop_prefix+'drop_release_regenerates_identically',
     '#280 session-drop-release: generator failed'),
    # cfg(test) branches selected at compile time by option_env!. Their
    # before/after expressions also let the ordinary anchor auditor verify
    # the exact source location. No source bytes are mutated for them.
    ('drop-dispose-switch', handle,
     'option_env!("PDRS_1010_SKIP_DROP_DISPOSE").is_some()', 'true',
     None, factory_prefix+'dropped_owned_session_after_executor_shutdown_without_runtime',
     '#1010 shutdown writer rejects further bytes before fence release'),
    ('prepared-ambient-switch', factory,
     'option_env!("PDRS_1010_DROP_AMBIENT_ONLY").is_some()', 'true',
     None, factory_prefix+'prepared_resources_released_when_dropped_off_runtime',
     '#1010 off-runtime release ran bounded wait'),
    ('generator-release-gate-bypassed', generator,
     "if(mode==='--child-process-exit-release-pending'){entered();await never;}",
     "if(mode==='--child-process-exit-release-pending'){entered();}",
     None, drop_prefix+'drop_release_regenerates_identically',
     '#280 session-drop-release/oracle.json differs'),
    ('generator-close-gate-bypassed', generator,
     "if(mode==='--child-process-exit-close-pending'){entered();await never;}",
     "if(mode==='--child-process-exit-close-pending'){entered();}",
     None, drop_prefix+'drop_release_regenerates_identically',
     '#280 session-drop-release/oracle.json differs'),
    # P0: replay destructive admission against the original seeded work root.
    ('generator-deletes-existing-work-root', generator,
     'fs.mkdirSync(output);',
     "fs.rmSync(output,{recursive:true,force:true});fs.mkdirSync(output);",
     None, drop_prefix+'drop_release_preserves_unrelated_out_files',
     '#1010 generator preserves existing work-process-exit bytes'),
    ('generator-accepts-existing-output', generator,
     'fs.mkdirSync(output);', 'fs.mkdirSync(output,{recursive:true});',
     None, drop_prefix+'drop_release_preserves_unrelated_out_files',
     '#1010 generator rejects existing output at admission'),
    # Atomic/exclusive oracle contract: the preservation test runs twice.
    ('generator-overwrites-existing-oracle', generator,
     "+'\\n',{flag:'wx'});", "+'\\n',{flag:'w'});",
     None, drop_prefix+'drop_release_preserves_late_oracle',
     '#1010 generator preserves late oracle bytes'),
    ('owned-queued-fallback-removed', handle,
     'session: Some(session.clone()),', 'session: None,',
     None, factory_prefix+'dropped_owned_session_shutdown_before_first_poll',
     '#1010 shutdown writer rejects further bytes before fence release'),
    ('owned-dead-fallback-removed', handle,
     'session: Some(session.clone()),', 'session: None,',
     None, factory_prefix+'dropped_owned_session_after_executor_shutdown_without_runtime',
     '#1010 shutdown writer rejects further bytes before fence release'),
    ('pending-queued-fallback-removed', handle,
     'session: Some(session.clone()),', 'session: None,',
     None, factory_prefix+'pending_owned_session_shutdown_before_first_poll',
     '#1010 shutdown writer rejects further bytes before fence release'),
    ('pending-dead-fallback-removed', handle,
     'session: Some(session.clone()),', 'session: None,',
     None, factory_prefix+'pending_owned_session_after_executor_shutdown_without_runtime',
     '#1010 shutdown writer rejects further bytes before fence release'),
    # Value mutation: keeping the lease in storage leaks the real fence.
    # Product BUSY (3s) arrives before the observation deadline (8s).
    ('owned-release-fence-not-taken', factory,
     '_lease: lock(&self.writer_lease).take(),', '_lease: None,',
     None, factory_prefix+'dropped_owned_session_shutdown_during_release',
     '#1010 successor acquires writer after fence release'),
    ('owned-close-fence-not-taken', factory,
     '_lease: lock(&self.writer_lease).take(),', '_lease: None,',
     None, factory_prefix+'dropped_owned_session_shutdown_during_close',
     '#1010 successor acquires writer after fence release'),
    ('owned-process-fence-released-early', factory,
     'let lease = self._lease.take();',
     'let lease = self._lease.take();\n        drop(lease);\n        let lease: Option<WriterLease> = None;',
     None, factory_prefix+'dropped_owned_session_shutdown_before_first_poll',
     '#1010 pinned close retains the process fence'),
    ('owned-file-fence-released-early', factory,
     'let lease = self._lease.take();',
     'let lease = self._lease.take().map(|lease| { let WriterLease { _file, _process } = lease; drop(_file); _process });',
     None, factory_prefix+'dropped_owned_session_shutdown_before_first_poll',
     '#1010 pinned close retains the file fence'),
    ('command-lease-ambient-spawn', contributions,
     'impl Drop for PluginCommandCatalogLease {\n    fn drop(&mut self) {\n        if !self.released {\n            self.released = true;\n            // Best-effort synchronous decrement; disposal callbacks are async\n            // and will run when the waiters are notified at the next poll point.\n            let state_clone = std::sync::Arc::clone(&self.state);\n            let waiters_clone = std::sync::Arc::clone(&self.waiters);\n            let plugins_clone = self.plugins.clone();\n            self.executor.spawn(async move {\n                let _ = release_lease_plugins(&plugins_clone, &state_clone, &waiters_clone).await;\n            });\n        }\n    }\n}',
     'impl Drop for PluginCommandCatalogLease {\n    fn drop(&mut self) {\n        if !self.released {\n            self.released = true;\n            // Best-effort synchronous decrement; disposal callbacks are async\n            // and will run when the waiters are notified at the next poll point.\n            let state_clone = std::sync::Arc::clone(&self.state);\n            let waiters_clone = std::sync::Arc::clone(&self.waiters);\n            let plugins_clone = self.plugins.clone();\n            tokio::spawn(async move {\n                let _ = release_lease_plugins(&plugins_clone, &state_clone, &waiters_clone).await;\n            });\n        }\n    }\n}',
     None, lease_prefix+'command_lease_drop_without_runtime',
     '#1010 plugin lease drop without a runtime does not panic'),
    ('session-lease-ambient-spawn', contributions,
     'impl Drop for PluginSessionContributionLease {\n    fn drop(&mut self) {\n        if !self.released {\n            self.released = true;\n            let state_clone = std::sync::Arc::clone(&self.state);\n            let waiters_clone = std::sync::Arc::clone(&self.waiters);\n            let plugins_clone = self.plugins.clone();\n            self.executor.spawn(async move {\n                let _ = release_lease_plugins(&plugins_clone, &state_clone, &waiters_clone).await;\n            });\n        }\n    }\n}',
     'impl Drop for PluginSessionContributionLease {\n    fn drop(&mut self) {\n        if !self.released {\n            self.released = true;\n            let state_clone = std::sync::Arc::clone(&self.state);\n            let waiters_clone = std::sync::Arc::clone(&self.waiters);\n            let plugins_clone = self.plugins.clone();\n            tokio::spawn(async move {\n                let _ = release_lease_plugins(&plugins_clone, &state_clone, &waiters_clone).await;\n            });\n        }\n    }\n}',
     None, lease_prefix+'session_lease_drop_without_runtime',
     '#1010 plugin lease drop without a runtime does not panic'),
    ('legacy-lease-ambient-spawn', contributions,
     'impl Drop for PluginContributionLease {\n    fn drop(&mut self) {\n        if !self.released {\n            self.released = true;\n            let state_clone = std::sync::Arc::clone(&self.state);\n            let waiters_clone = std::sync::Arc::clone(&self.waiters);\n            let plugins_clone = self.plugins.clone();\n            self.executor.spawn(async move {\n                let _ = release_lease_plugins(&plugins_clone, &state_clone, &waiters_clone).await;\n            });\n        }\n    }\n}',
     'impl Drop for PluginContributionLease {\n    fn drop(&mut self) {\n        if !self.released {\n            self.released = true;\n            let state_clone = std::sync::Arc::clone(&self.state);\n            let waiters_clone = std::sync::Arc::clone(&self.waiters);\n            let plugins_clone = self.plugins.clone();\n            tokio::spawn(async move {\n                let _ = release_lease_plugins(&plugins_clone, &state_clone, &waiters_clone).await;\n            });\n        }\n    }\n}',
     None, lease_prefix+'legacy_lease_drop_without_runtime',
     '#1010 plugin lease drop without a runtime does not panic'),
    ('fallback-state-not-published', handle,
     '.state = HandleState::Disposed;\n            self.result.send_replace',
     '.state = HandleState::Draining;\n            self.result.send_replace',
     None, drop_prefix+'explicit_dispose_on_dead_executor_finishes',
     '#1010 unpolled drain publishes Disposed'),
    ('fallback-result-fakes-success', handle,
     'self.result.send_replace(Some(Err(RuntimeError::new(\n                "Session lifecycle executor stopped.",\n            ))));',
     'self.result.send_replace(Some(Ok(())));',
     None, drop_prefix+'queued_dispose_shutdown_finishes',
     '#1010 shutdown disposal cannot claim success'),
    ('closed-channel-waits-forever', handle,
     'return T::lifecycle_stopped();', 'std::future::pending::<()>().await; return T::lifecycle_stopped();',
     None, drop_prefix+'closed_lifecycle_channel_returns_error',
     '#1010 closed completion channel returns boundedly'),
    ('cancellation-reported-as-panic', handle,
     '"Agent session dispose task cancelled."', '"Agent session dispose task panicked."',
     None, drop_prefix+'polled_dispose_shutdown_reports_cancellation',
     '#1010 shutdown cancellation is not a panic'),
    ('panic-reported-as-cancellation', handle,
     '"Agent session dispose task panicked."', '"Agent session dispose task cancelled."',
     None, drop_prefix+'dispose_panic_is_distinct_from_cancellation',
     '#1010 actual panic remains distinguishable'),
    ('poison-progress-not-recovered', handle,
     'self\n            .progress\n            .lock()\n            .unwrap_or_else(std::sync::PoisonError::into_inner)',
     'self.progress.lock().expect("poison progress mutant")',
     None, drop_prefix+'poisoned_handle_drop_still_drains',
     '#1010 poisoned handle Drop does not panic'),
    ('poison-lease-not-recovered', handle,
     'self\n            .lease\n            .lock()\n            .unwrap_or_else(std::sync::PoisonError::into_inner)',
     'self.lease.lock().expect("poison lease mutant")',
     None, drop_prefix+'poisoned_handle_drop_still_drains',
     '#1010 poisoned handle Drop does not panic'),
    ('poison-activity-not-recovered', handle,
     'self.0\n            .0\n            .progress\n            .lock()\n            .unwrap_or_else(std::sync::PoisonError::into_inner)',
     'self.0.0.progress.lock().expect("poison activity mutant")',
     None, drop_prefix+'poisoned_activity_drop_still_drains',
     '#1010 poisoned activity Drop does not panic'),
    ('poison-dispose-guard-not-recovered', handle,
     'self\n                                .handle_progress\n                                .lock()\n                                .unwrap_or_else(std::sync::PoisonError::into_inner)',
     'self.handle_progress.lock().expect("poison dispose guard mutant")',
     None, drop_prefix+'poisoned_dispose_guard_still_completes',
     '#1010 shutdown cancellation is not a panic'),
    ('regeneration-uses-precreated-output', baseline,
     'matches!(fixture, "session-stale-handoff" | "session-drop-release")',
     'fixture == "session-stale-handoff"',
     None, drop_prefix+'drop_release_regenerates_identically',
     '#280 session-drop-release: generator failed'),

    ('fallback-result-not-published', handle,
     'self.result.send_replace(Some(Err(RuntimeError::new(\n                "Session lifecycle executor stopped.",\n            ))));',
     'self.result.send_replace(None);',
     None, drop_prefix+'queued_dispose_shutdown_finishes',
     '#1010 fallback explicitly publishes completion'),
    ('poison-drain-not-recovered', handle,
     'progress\n                        .lock()\n                        .unwrap_or_else(std::sync::PoisonError::into_inner)',
     'progress.lock().expect("poison drain mutant")',
     None, drop_prefix+'poisoned_handle_drop_still_drains',
     '#1010 poisoned handle drain still reaches dispose'),
    ('poison-fallback-not-recovered', handle,
     'self.progress\n                .lock()\n                .unwrap_or_else(std::sync::PoisonError::into_inner)',
     'self.progress.lock().expect("poison fallback mutant")',
     None, drop_prefix+'poisoned_unpolled_fallback_still_completes',
     '#1010 poisoned fallback publishes Disposed'),
    ('generator-skips-owned-cleanup', generator,
     'for(const workRoot of workRoots)fs.rmSync(workRoot,{recursive:true,force:true,maxRetries:5});',
     'for(const workRoot of workRoots)void workRoot;',
     None, drop_prefix+'drop_release_preserves_late_oracle',
     '#1010 generator cleans owned work roots on failure'),

    ('generator-admission-wrong-error', generator,
     'fs.mkdirSync(output);', 'throw new Error("#1010 unexpected claim failure");',
     None, drop_prefix+'drop_release_preserves_unrelated_out_files',
     '#1010 rejection identifies existing output'),
    ('generator-touches-existing-output', generator,
     'fs.mkdirSync(output);',
     'try{fs.mkdirSync(output);}catch(error){fs.writeFileSync(path.join(output,"unexpected"),"changed",{flag:"wx"});throw error;}',
     None, drop_prefix+'drop_release_preserves_unrelated_out_files',
     '#1010 admission refusal leaves directory entries unchanged'),

]

compile_switches = {
    'drop-dispose-switch': 'PDRS_1010_SKIP_DROP_DISPOSE',
    'prepared-ambient-switch': 'PDRS_1010_DROP_AMBIENT_ONLY',
}


def audit():
    rows = []
    bodies = {drop_tests, factory_tests, lease_tests}
    sources = {path.read_text() for path in bodies}
    for name, path, old, new, scope, test, assertion in cases:
        try:
            original = path.read_text()
            changed = mutate(original, scope, [(old, new)])
            if changed == original:
                raise ValueError('mutation leaves source unchanged')
            body = next((text for text in sources
                         if ('fn ' + test.rsplit('::', 1)[1] + '(') in text
                         or (test.rsplit('::', 1)[1] + ' =>') in text), None)
            if body is None:
                raise ValueError('bound test not found: ' + test)
            test_name = test.rsplit('::', 1)[1]
            if not test_name.endswith('regenerates_identically'):
                literals = [piece.split('"')[0] for piece in
                            next(t for t in sources if f'fn {test_name}(' in t)
                            .split('"#')[1:]]
                anchors = assertion if isinstance(assertion, list) else [assertion]
                if not any(any(literal in anchor or anchor.startswith(literal)
                               for literal in literals) for anchor in anchors):
                    raise ValueError('bound failure message not found: ' + repr(assertion))
            rows.append(dict(mutation=name, status='CHECKED', test=test, assertion=assertion))
        except Exception as error:
            rows.append(dict(mutation=name, status='INVALID', reason=str(error)))
    print(json.dumps(rows, indent=2))
    return int(any(row['status'] != 'CHECKED' for row in rows))


if sys.argv[1:] == ['--audit']:
    raise SystemExit(audit())
compile_only = len(sys.argv) > 1 and sys.argv[1] == '--compile'
args = sys.argv[2:] if compile_only else sys.argv[1:]
if not args:
    raise SystemExit('usage: negative_controls.py [--compile] OUT [mutation names]')
env = os.environ.copy()
if not env.get('CARGO_TARGET_DIR') or not env.get('PDRS_BASELINE'):
    raise SystemExit('caller must set CARGO_TARGET_DIR and PDRS_BASELINE')
env.update(CARGO_PROFILE_DEV_DEBUG='line-tables-only', CARGO_PROFILE_TEST_DEBUG='line-tables-only',
            CARGO_TERM_QUIET='false')
for switch in compile_switches.values():
    env.pop(switch, None)
names = set(args[1:])
if names - {c[0] for c in cases}:
    raise SystemExit('unknown mutation')
chosen = [c for c in cases if not names or c[0] in names]
out = Path(args[0]); out.mkdir(parents=True, exist_ok=False)
command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
           '-p', 'pilotdeck-gateway', '--lib']


def run(label, test, compile=False):
    slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=900)
    (out / (label + '.slots.log')).write_text(slot.stdout + slot.stderr)
    args = ['--no-run'] if compile else [test, '--', '--exact', '--include-ignored', '--nocapture']
    result = run_owned(command + args, cwd=root, env=env, timeout=900)
    with (out / (label + '.log')).open('x') as log:
        log.write(result.stdout + result.stderr)
    return result


results = []
source_cases = [c for c in chosen if c[0] not in compile_switches]
switch_selected = [c for c in chosen if c[0] in compile_switches]


def compile_and_classify(row, case):
    name, path, old, new, scope, test, assertion = case
    if run(name + '-compile', test, compile=True).returncode:
        raise RuntimeError('mutant compile failed')
    row['compiled'] = True
    if compile_only:
        row.update(status='COMPILED', reason='mutant no-run passed')
    else:
        status, reason = classify(run(name + '-negative', test),
                                  Case(name, path, old, new, test, assertion, scope))
        row.update(status=status, reason=reason)


def restored(row, name, test):
    if not compile_only:
        try:
            if not passed(run(name + '-restored', test), test):
                row.update(status='INVALID', reason=row['reason'] + '; restored test failed')
        except Exception as error:
            row.update(status='INVALID', reason=row['reason'] + f'; restored: {error}')
    results.append(row)
    print(f'{name}: {row["status"]}: {row["reason"]}', flush=True)


def guard_failed(group, error):
    # A failed guard means the tree may not be restored: every row recorded so
    # far is untrusted, not only this group's (#979 P0 / #1037, shared
    # invalidate_completed also revokes ``compiled``). Unrun rows stay INVALID.
    invalidate_completed(results, f'source guard: {error}')
    recorded = {row['mutation'] for row in results}
    for name, path, old, new, scope, test, assertion in group:
        if name not in recorded:
            results.append(dict(mutation=name, test=test, assertion=assertion,
                               status='INVALID', compiled=False,
                               reason=f'guard: {error}'))


source_guard_error = None

if source_cases:
    try:
        with mutating_sources(root, list(dict.fromkeys(c[1] for c in source_cases))) as originals:
            for name, path, old, new, scope, test, assertion in source_cases:
                row = dict(mutation=name, test=test, assertion=assertion,
                           status='INVALID', reason='', compiled=False)
                try:
                    changed = mutate(originals[path].decode(), scope, [(old, new)])
                    if not compile_only and not passed(run(name + '-positive', test), test):
                        raise RuntimeError('bound positive test failed')
                    path.write_text(changed)
                    compile_and_classify(row, (name, path, old, new, scope, test, assertion))
                except Exception as error:
                    row.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
                finally:
                    path.write_bytes(originals[path])
                restored(row, name, test)
    except Exception as error:
        source_guard_error = error
        guard_failed(source_cases, error)

# Compile-time controls never write product sources. They still serialize with
# source mutators through the official tree lock; no HEAD guard is bypassed for
# the source-mutation group above.
if switch_selected and source_guard_error is not None:
    # Never run compile-time controls on a tree the source guard could not
    # prove restored.
    guard_failed(switch_selected, f'source guard failed earlier: {source_guard_error}')
elif switch_selected:
    try:
        with tree_lock(root):
            for name, path, old, new, scope, test, assertion in switch_selected:
                row = dict(mutation=name, test=test, assertion=assertion,
                           status='INVALID', reason='', compiled=False)
                switch = compile_switches[name]
                try:
                    mutate(path.read_text(), scope, [(old, new)])
                    if not compile_only and not passed(run(name + '-positive', test), test):
                        raise RuntimeError('bound positive test failed')
                    env[switch] = '1'
                    compile_and_classify(row, (name, path, old, new, scope, test, assertion))
                except Exception as error:
                    row.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
                finally:
                    env.pop(switch, None)
                restored(row, name, test)
    except Exception as error:
        guard_failed(switch_selected, error)

counts = {status: sum(r['status'] == status for r in results)
          for status in ('KILLED', 'SURVIVED', 'INVALID', 'COMPILED')}
summary = out / 'summary.json'
temporary = summary.with_name(summary.name + '.tmp')
temporary.write_text(json.dumps(dict(controls=results, counts=counts), indent=2) + '\n')
temporary.replace(summary)
print(json.dumps(counts), flush=True)
raise SystemExit(int(any(r['status'] != ('COMPILED' if compile_only else 'KILLED')
                        for r in results)))
