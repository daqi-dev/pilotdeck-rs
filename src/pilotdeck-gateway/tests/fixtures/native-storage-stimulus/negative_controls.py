#!/usr/bin/env python3
"""#1370: commit first, then compile and run every storage-stimulus mutation.

Usage: negative_controls.py --check
       negative_controls.py [--compile-only] NEW_OUTPUT_DIR [mutation ...]
Every mutation must compile, fail at its bound nonce receipt, and pass again
after restoration. Source bytes must equal HEAD; no alternate baseline exists.
"""
import argparse
import json
import hashlib
import re
import os
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[5]
FIXTURES = Path(__file__).resolve().parents[1]
STIMULUS = ROOT / 'src/pilotdeck-gateway/src/storage/stimulus.rs'
PUBLISH = ROOT / 'src/pilotdeck-gateway/src/runtime/turn/publish.rs'
RUN_REGISTRY = ROOT / 'src/pilotdeck-gateway/src/storage/run_registry.rs'
TRANSCRIPT_ADAPTER = ROOT / 'src/pilotdeck-gateway/src/storage/transcript_adapter.rs'
ASSEMBLY = ROOT / 'src/pilotdeck-gateway/src/native_process/assembly.rs'
TESTS = ROOT / 'src/pilotdeck-gateway/src/storage/stimulus_tests.rs'
PROCESS_TEST = ROOT / 'src/pilotdeck-gateway/tests/native_storage_stimulus.rs'
PROCESS = Path(__file__).resolve().parent / 'process.mts'
sys.path.insert(0, str(FIXTURES))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import ReceiptRun, invalidate_completed, judge, passed

# The entry-tick receipt write, and the lazy variant of it that defers the
# `spawn_blocking` (and with it the `queued` receipt) into the returned future.
# Shared by the two mutations that must each be bound to their own case.
LAZY_DISPATCH_FROM = '''            let write = handle.spawn_blocking(move || {
                let outcome = write();
                let _ = settled.send(outcome.is_ok());
                outcome
            });
            Box::pin(async move {'''
LAZY_DISPATCH_TO = '''            Box::pin(async move {
                let write = handle.spawn_blocking(move || {
                    let outcome = write();
                    let _ = settled.send(outcome.is_ok());
                    outcome
                });'''

mutations = [
    # The barrier must actually hold the write: skipping the bounded wait
    # releases instantly, so the bound-expiry release receipt loses its
    # `bounded: true` marker first.
    ('writer-barrier-never-blocks', STIMULUS,
     '.and_then(|()| self.wait_for_release(seq, bound));',
     '.and_then(|()| Ok(true));',
     'storage::stimulus_tests::writer_barrier_bound_releases_without_release_file', '#1370 bound expiry releases with bounded true'),
    # The bound is the safety release: without the deadline the barrier never
    # self-releases, so the release receipt wait fails at its observation cap.
    ('writer-barrier-bound-unbounded', STIMULUS,
     'if Instant::now() >= deadline {',
     'if false {',
     'storage::stimulus_tests::writer_barrier_bound_releases_without_release_file', '#1370 bound expiry self-releases the barrier'),
    # The single injected error must be EIO (raw os error 5): any other kind
    # could be confused with a provider failure family.
    ('writer-fault-kind-degraded', STIMULUS,
     'io::Error::from_raw_os_error(rustix::io::Errno::IO.raw_os_error())',
     'io::Error::other("degraded")',
     'storage::stimulus_tests::barrier_and_fault_ids_are_single_shot', '#1370 writer fault is EIO (raw os error 5), not another kind'),
    # Fault ids are single-shot: without the fired marker the same fault fires
    # on every matching write.
    ('writer-fault-refires', STIMULUS,
     'site_state(&mut inner, site).fault_fired = Some(fault.id.clone());',
     'let _ = &fault;',
     'storage::stimulus_tests::barrier_and_fault_ids_are_single_shot', '#1370 the fault fires only once'),
    # The registry barrier must hold the mutation chain lock: hoisting the
    # wait outside the lock lets a plain get() proceed while the persist is
    # held.
    ('registry-barrier-outside-lock', RUN_REGISTRY,
     '''let _chain = self.chain.lock().unwrap_or_else(PoisonError::into_inner);
        let _lock = self.acquire_file_lock()?;
        let mut snapshot = read_snapshot(&self.path)?;
        let (value, changed) = operation(&mut snapshot)?;
        if changed {
            let mut blocked = None;
            if let Some(key) = stimulus_key
                && let Some(stimulus) = self.stimulus.as_ref()
                && stimulus.before_write(super::stimulus::Site::Registry, key)?
                    == super::stimulus::Applied::Blocked
            {
                blocked = Some((stimulus, key));
            }''',
     '''let mut pre_blocked = None;
        if let Some(key) = stimulus_key
            && let Some(stimulus) = self.stimulus.as_ref()
            && stimulus.before_write(super::stimulus::Site::Registry, key)?
                == super::stimulus::Applied::Blocked
        {
            pre_blocked = Some((stimulus, key));
        }
        let _chain = self.chain.lock().unwrap_or_else(PoisonError::into_inner);
        let _lock = self.acquire_file_lock()?;
        let mut snapshot = read_snapshot(&self.path)?;
        let (value, changed) = operation(&mut snapshot)?;
        if changed {
            let blocked = pre_blocked;''',
     'storage::stimulus_tests::registry_barrier_holds_chain_lock', "#1370 get() is held behind the barrier's chain lock"),
    # fix-r1 isolation: the registry site must read its instance handle, not the
    # process-wide entry — degraded back to the global, an armed stimulus can
    # never reach the test's registry (env unset in the test process), so no
    # block receipt ever appears.
    ('registry-handle-degraded-to-global', RUN_REGISTRY,
     '''&& let Some(stimulus) = self.stimulus.as_ref()
                && stimulus.before_write(super::stimulus::Site::Registry, key)?''',
     '''&& let Some(stimulus) = super::stimulus::StorageStimulus::global()
                && stimulus.before_write(super::stimulus::Site::Registry, key)?''',
     'storage::stimulus_tests::registry_barrier_holds_chain_lock', '#1370 registry barrier blocks the in-lock persist'),
    # fix-r1 isolation, writer twin: the store's underlying append must read
    # its instance handle. Degraded to the global, the writer barrier never
    # blocks and the block receipt wait fails at its observation cap.
    ('writer-store-handle-degraded-to-global', TRANSCRIPT_ADAPTER,
     '''let stimulus = self.stimulus.clone();''',
     '''let stimulus = super::stimulus::StorageStimulus::global();''',
     'storage::stimulus_tests::writer_barrier_blocks_underlying_append', '#1370 writer barrier blocks the underlying append'),
    # The FIFO witness: the queued receipt is decided and sequenced at the
    # entry tick, while the barrier is held, before the caller queues on the
    # chain lock.
    ('registry-queued-after-release', STIMULUS,
     '''        let held = site_state_ref(&inner, site).held.is_some();
        if !held {
            return None;
        }''',
     '''        let held = site_state_ref(&inner, site).held.is_none();
        if !held {
            return None;
        }''',
     'storage::stimulus_tests::registry_port_appends_queue_with_witness_receipts', '#1370 registry queued receipt is published while held'),
    # fix-r2: the receipt is dispatched on the entry tick, not from inside the
    # returned future. Publication only first polls an append after its
    # predecessor settles, so a lazy dispatch writes the receipt after the
    # barrier already released — the collector's wait then never sees it.
    ('registry-witness-lazily-dispatched', STIMULUS,
     LAZY_DISPATCH_FROM,
     LAZY_DISPATCH_TO,
     'storage::stimulus_tests::publication_queued_receipt_survives_predecessor_serialization', '#1370 publication publishes the queued receipt while the barrier is held'),
    # #1648: the same entry-tick dispatch is also the FIFO case's entry-tick
    # proof. Its future is deliberately withheld, so a lazy receipt write never
    # appears while the barrier is held and the `queued` wait times out.
    ('registry-witness-lazily-dispatched-fifo', STIMULUS,
     LAZY_DISPATCH_FROM,
     LAZY_DISPATCH_TO,
     'storage::stimulus_tests::witnessed_append_keeps_its_fifo_slot_before_a_later_plain_append', '#1370 fifo fixture owes the second append a receipt'),
    # #1648: the settle must come from the receipt write, not from the caller's
    # first poll. Keeping the sender alive without ever sending models that
    # deferred settle: the witnessed job sits on the single worker for the whole
    # `WITNESS_SETTLE`, so the append behind a withheld witnessed future starves
    # past the observation cap and the receipt-bound assertion fires.
    ('registry-witness-settle-not-from-write', STIMULUS,
     '''                let _ = settled.send(outcome.is_ok());
                outcome''',
     '''                std::mem::forget(settled);
                outcome''',
     'storage::stimulus_tests::withheld_witnessed_future_does_not_park_the_registry_worker', '#1370 a withheld witnessed future does not park the registry worker'),
    # fix-r2: a barrier that fails while held must release the hold, or every
    # later entry at the site is stamped with a queued receipt it does not owe.
    ('registry-hold-not-cleared-on-error', STIMULUS,
     '''    fn clear_hold(&self, site: Site) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        site_state(&mut inner, site).held = None;
    }''',
     '''    fn clear_hold(&self, site: Site) {
        let _ = site;
    }''',
     'storage::stimulus_tests::failed_barrier_releases_its_hold_before_later_entries', '#1370 a failed barrier releases its hold before later entries'),
    # fix-r2: the queue slot is taken on the entry tick and only the job's
    # execution waits for the receipt; queueing the job from inside the returned
    # future lets a later plain append overtake it.
    ('registry-witness-deferred-past-entry', PUBLISH,
     '''        let append = Self::witnessed_within(self.jobs.clone(), gate, run, event, WITNESS_SETTLE);
        Box::pin(async move {
            witness.await?;
            append.await
        })''',
     '''        let parts = (self.jobs.clone(), gate, run, event);
        Box::pin(async move {
            witness.await?;
            Self::witnessed_within(parts.0, parts.1, parts.2, parts.3, WITNESS_SETTLE).await
        })''',
     'storage::stimulus_tests::witnessed_append_keeps_its_fifo_slot_before_a_later_plain_append',
     '#1370 a witnessed append keeps its FIFO slot before a later plain append'),
    # fix-r2: the job runs only once the receipt it is stamped with settled, so
    # a witness failure fails the append before the durable write can happen.
    ('registry-witness-gate-ignored', PUBLISH,
     'match gate.recv_timeout(settle) {',
     'match { let _ = gate.recv_timeout(settle); Ok::<bool, ()>(true) } {',
     'storage::stimulus_tests::failed_queued_witness_fails_the_append_before_the_worker', '#1370 a failed queued witness never dispatches the durable append'),
    # Production default: an unset entry must stay unarmed.
    ('stimulus-unset-activates', STIMULUS,
     'Err(_) => None,',
     'Err(_) => Some(Arc::new(StorageStimulus::control("/nonexistent-pdrs-stimulus-control"))),',
     'storage::stimulus_tests::stimulus_unset_is_inert', '#1370 unset stimulus keeps the production default'),
    # A malformed control file must fail the operation closed, not arm nothing.
    ('invalid-config-accepted', STIMULUS,
     '''let value = parse_json(&text)
            .map_err(|error| invalid(format!("stimulus.json is not valid JSON: {error}")))?;
        parse_config(&value)''',
     '''let _ = parse_json(&text);
        Ok(Config::default())''',
     'storage::stimulus_tests::invalid_control_directory_fails_closed', '#1370 malformed stimulus.json fails the writer closed'),
    # The construction receipt is the stimulus boundary witness: the assembly
    # must state the resolved control directory on stderr at startup.
    ('assembly-receipt-not-published', ASSEMBLY,
     'eprintln!("native storage stimulus on {}", dir.display());',
     'let _ = dir;',
     'executable_native_storage_stimulus', '#1370 executable armed stimulus receipt'),
    # Fail closed: a value that does not name an existing directory is
    # rejected before any listening advertisement.
    ('assembly-invalid-dir-accepted', ASSEMBLY,
     'if !path.is_dir() {',
     'if false {',
     'executable_native_storage_stimulus', '#1370 executable invalid stimulus dir'),
    # #1805 item 1: the release guard's budget may only be extended by a
    # receipt that actually advanced. The fix-r5 shape refreshed the deadline
    # unconditionally inside the release-writing branch, so a stalled barrier
    # (block receipt on disk, release and complete forever absent — the worker
    # died inside wait_for_release) spun `Drop` forever instead of ending.
    ('drop-budget-refreshed-in-release-branch', TESTS,
     '''            if observed.0.is_none()
                && let Some(seq) = observed.1
            {
                // Non-panicking release: a `.unwrap()` here could abort during unwind.
                let temporary = self.control.join("release.json.guard.tmp");
                if fs::write(&temporary, format!("{{\\"seq\\":{seq}}}")).is_ok() {
                    let _ = fs::rename(&temporary, self.control.join("release.json"));
                }
            }
            // Only a receipt that actually advanced earns the durable write behind
            // it a fresh budget — the barrier the worker reaches late must not
            // leave that write with whatever is left of the block-receipt wait.
            // A stalled or failing barrier extends nothing, so `Drop` still ends:
            // at most the `block` and `release` transitions can push it out.
            let current = sample(observe("block", 0));
            if current != observed {
                observed = current;
                deadline = Instant::now() + Duration::from_secs(2);
            }''',
     '''            if observed.0.is_none()
                && let Some(seq) = observed.1
            {
                // Non-panicking release: a `.unwrap()` here could abort during unwind.
                let temporary = self.control.join("release.json.guard.tmp");
                if fs::write(&temporary, format!("{{\\"seq\\":{seq}}}")).is_ok() {
                    let _ = fs::rename(&temporary, self.control.join("release.json"));
                }
                deadline = Instant::now() + Duration::from_secs(2);
            }
            // Only a receipt that actually advanced earns the durable write behind
            // it a fresh budget — the barrier the worker reaches late must not
            // leave that write with whatever is left of the block-receipt wait.
            // A stalled or failing barrier extends nothing, so `Drop` still ends:
            // at most the `block` and `release` transitions can push it out.
            let current = sample(observe("block", 0));
            if current != observed {
                observed = current;
            }''',
     'storage::stimulus_tests::stalled_barrier_guard_still_ends_within_its_budget', '#1805 the stalled release guard ends within its budget'),
    # #1805 item 2: receipts pair with the current barrier by sequence. With
    # the floor neutralized, a key that completed an earlier round reads as
    # already released: the guard skips the release.json write the current
    # barrier is waiting on and exits on the stale round's receipts.
    ('release-guard-receipts-unpaired', TESTS,
     '&& seq > after as f64',
     '&& seq > after as f64 * 0.0',
     'storage::stimulus_tests::release_guard_pairs_receipts_with_the_current_barrier', '#1805 the release guard releases the current barrier'),
]


def test_body(test, texts=None):
    if texts is None:
        texts = {path: path.read_text() for path in [TESTS, PROCESS_TEST]}
    if test.startswith('storage::stimulus_tests::'):
        path = TESTS
    else:
        path = PROCESS_TEST
    # These files use top-level test functions. A column-zero closing brace is
    # the end of this function, so a sibling's assertion cannot satisfy it.
    pattern = r'^(?:async )?fn ' + re.escape(test.rsplit('::', 1)[-1]) + r'\([^\n]*\).*?^}'
    bodies = re.findall(pattern, texts[path], re.M | re.S)
    return bodies[0] if len(bodies) == 1 else ''


def check_binding_scope():
    fixture = 'fn target() {\n}\nfn sibling() {\n assert!(false, "only sibling");\n}\n'
    if 'only sibling' in test_body('storage::stimulus_tests::target', {TESTS: fixture}):
        raise RuntimeError('#1370 binding checker accepted sibling assertion')
    if 'only sibling' not in test_body('storage::stimulus_tests::sibling', {TESTS: fixture, PROCESS_TEST: ''}):
        raise RuntimeError('#1370 binding checker lost its own assertion')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--check', action='store_true')
    parser.add_argument('--compile-only', action='store_true')
    parser.add_argument('output', nargs='?')
    parser.add_argument('selected', nargs='*')
    args = parser.parse_args()
    check_binding_scope()
    stale = []
    for name, path, old, new, test, assertion in mutations:
        text = path.read_text()
        body = test_body(test)
        if (text.count(old) != 1 or old == new or
                not body or f'"{assertion}"' not in body):
            stale.append(name)
    if stale:
        raise SystemExit('#1370 stale anchor/test/assertion bindings: ' + ', '.join(stale))
    if args.check:
        print(f'#1370 {len(mutations)} unique anchors and assertion bindings: OK')
        return 0
    selected = [row for row in mutations if not args.selected or row[0] in args.selected]
    if not selected or (args.selected and len(selected) != len(args.selected)):
        raise SystemExit('#1370 unknown or repeated mutation')
    if not args.output or not os.environ.get('CARGO_TARGET_DIR'):
        raise SystemExit('#1370 output and caller CARGO_TARGET_DIR required')
    output = Path(args.output).resolve()
    output.mkdir(parents=True, exist_ok=False)
    env = dict(os.environ)
    for key in [ReceiptRun.ENV, ReceiptRun.PARENT_ENV, 'PDRS_MUTANT', 'LIVE_E2E_MUTANT']:
        env.pop(key, None)
    guarded = [STIMULUS, PUBLISH, RUN_REGISTRY, TRANSCRIPT_ADAPTER, ASSEMBLY, TESTS, PROCESS_TEST, PROCESS, Path(__file__).resolve(),
               FIXTURES / 'negative_process.py',
               FIXTURES / 'negative_suite.py',
               ROOT / 'src/pilotdeck-gateway/tests/common/negative_receipt.rs',
               ROOT / 'src/pilotdeck-gateway/src/lib.rs',
               ROOT / 'src/pilotdeck-gateway/src/test_process.rs']
    results = []
    guarded_sha256 = {}
    sidecar_sha256 = None

    def run(label, run_env, test, *, compile_only=False):
        slot = wait_for_slot(ROOT / 'scripts/cargo-slots.sh', timeout=600)
        (output / f'{label}.slots.log').write_text(slot.stdout + slot.stderr)
        command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
                   '-p', 'pilotdeck-gateway']
        command += (['--test', 'native_storage_stimulus']
                    if test == 'executable_native_storage_stimulus' else ['--lib'])
        command += ['--no-run'] if compile_only else [test, '--', '--exact', '--include-ignored']
        run_env = dict(run_env)
        if test == 'executable_native_storage_stimulus':
            run_env['PDRS_1370_PROCESS_OUTPUT'] = str(output / (label + '-process'))
        (output / f'{label}.command.json').write_text(json.dumps(command) + '\n')
        result = run_owned(command, cwd=ROOT, env=run_env, timeout=900)
        (output / f'{label}.log').write_text(result.stdout + result.stderr)
        return result

    try:
        with mutating_sources(ROOT, guarded) as originals:
            guarded_sha256 = {str(path.relative_to(ROOT)): hashlib.sha256(originals[path]).hexdigest() for path in guarded}
            if any(row[4] == 'executable_native_storage_stimulus' for row in selected):
                slot = wait_for_slot(ROOT / 'scripts/cargo-slots.sh', timeout=600)
                (output / 'sidecar-build.slots.log').write_text(slot.stdout + slot.stderr)
                command = ['cargo', 'build', '--offline', '--manifest-path', 'src/Cargo.toml',
                           '-p', 'pilotdeck-transport', '--bin', 'pilotdeck-agent-loop-sidecar']
                (output / 'sidecar-build.command.json').write_text(json.dumps(command) + '\n')
                built = run_owned(command, cwd=ROOT, env=env, timeout=900)
                (output / 'sidecar-build.log').write_text(built.stdout + built.stderr)
                if built.returncode:
                    raise RuntimeError('#1370 current-source sidecar build failed')
                sidecar = Path(env['CARGO_TARGET_DIR']) / 'debug/pilotdeck-agent-loop-sidecar'
                sidecar_sha256 = hashlib.sha256(sidecar.read_bytes()).hexdigest()
            for name, path, old, new, test, assertion in selected:
                item = dict(name=name, test=test, assertion=assertion,
                            status='INVALID', reason='not run', compiled=False)
                try:
                    if not args.compile_only and not passed(run(name + '-positive', env, test), test):
                        raise RuntimeError('positive bound test failed')
                    path.write_text(originals[path].decode().replace(old, new, 1))
                    if run(name + '-compile', env, test, compile_only=True).returncode:
                        raise RuntimeError('mutant did not compile; not a runtime kill')
                    item['compiled'] = True
                    if args.compile_only:
                        item.update(status='COMPILED', reason='mutated source compiled')
                    else:
                        receipt = ReceiptRun(output, name, [(test, assertion)])
                        result = run(name + '-negative', receipt.environment(env), test)
                        killed, reason, hits = judge(result, '', [(test, assertion)], receipt)
                        item.update(status='KILLED' if killed else ('SURVIVED' if passed(result, test) else 'INVALID'),
                                    reason=reason, hits=hits)
                except Exception as error:
                    item.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
                finally:
                    path.write_bytes(originals[path])
                try:
                    if not args.compile_only:
                        if not passed(run(name + '-restored', env, test), test):
                            raise RuntimeError('restored bound test failed')
                        item['restoredPass'] = True
                except Exception as error:
                    item.update(status='INVALID', reason=item['reason'] + f'; restored: {error}')
                results.append(item)
                print(f'{name}: {item["status"]}: {item["reason"]}', flush=True)
    except Exception as error:
        reason = f'source guard: {type(error).__name__}: {error}'
        invalidate_completed(results, reason)
        for name, _, _, _, test, assertion in selected[len(results):]:
            results.append(dict(name=name, test=test, assertion=assertion,
                                status='INVALID', reason=reason, compiled=False))
    counts = {state: sum(row['status'] == state for row in results)
              for state in ['KILLED', 'SURVIVED', 'INVALID', 'COMPILED']}
    temporary = output / 'summary.json.tmp'
    temporary.write_text(json.dumps(
        {'controls': results, 'counts': counts, 'guardedSha256': guarded_sha256, 'sidecarSha256': sidecar_sha256}, indent=2) + '\n')
    temporary.replace(output / 'summary.json')
    print(json.dumps({'controls': results, 'counts': counts}), flush=True)
    success = 'COMPILED' if args.compile_only else 'KILLED'
    return int(any(row['status'] != success for row in results))


if __name__ == '__main__':
    raise SystemExit(main())
