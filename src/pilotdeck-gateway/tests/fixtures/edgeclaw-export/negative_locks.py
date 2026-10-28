#!/usr/bin/env python3
# anchor-selfcheck: unsupported imperative sequential once/write mutations
"""Round 9 product mutations, serialized, bounded and restored byte-for-byte.
Run from any directory; uses the launcher's CARGO_TARGET_DIR or src/target.
"""
import json
import os
from pathlib import Path
import signal
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

repo = Path(__file__).resolve().parents[5]
source = repo / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw'
files = {name: source / name for name in ['file_memory.rs', 'storage/overview.rs', 'storage/accessors.rs', 'storage/snapshots.rs']}
out = Path(sys.argv[1]).resolve()
out.mkdir(parents=True, exist_ok=True)
env = dict(os.environ, RUST_TEST_THREADS='4')

def interrupted(signum, frame):
    raise RuntimeError(f'interrupted by signal {signum}')

signal.signal(signal.SIGTERM, interrupted)

def run(command, label, extra=None, seconds=120):
    # Shared bounded owner: every invocation gets a fresh session and a killed
    # tree is reaped within run_owned's collection bound (#528).
    try:
        result = run_owned(command, cwd=repo / 'src',
                           env=dict(env, **(extra or {})), timeout=seconds)
    except subprocess.TimeoutExpired as error:
        text = (error.output or '') + (error.stderr or '')
        (out / (label + '.log')).write_text(text)
        raise
    text = result.stdout + result.stderr
    (out / (label + '.log')).write_text(text)
    return result.returncode, text

def reserve(label):
    slot = wait_for_slot(repo / 'scripts/cargo-slots.sh', interval=30)
    with (out / (label + '-slots.log')).open('a') as log:
        log.write(f'exit={slot.returncode}\n{slot.stdout}')
    return slot

def build(label):
    reserve(label)
    status, log = run(['cargo', 'test', '-p', 'pilotdeck-gateway', '--lib', '--no-run', '--message-format=json'], label, seconds=180)
    assert status == 0, log[-8000:]
    artifacts = [json.loads(line) for line in log.splitlines() if line.startswith('{')]
    return next(a['executable'] for a in reversed(artifacts) if a.get('reason') == 'compiler-artifact' and a.get('executable') and a['target']['name'] == 'pilotdeck_gateway')

def write(name, text):
    files[name].write_text(text)

def once(text, old, new, count=1):
    assert text.count(old) == count, (old, text.count(old))
    return text.replace(old, new)

# The lock file is shared by every in-place edgeclaw negative runner. Read the
# pristine bytes only after the flock is held and restore them in finally.
with mutating_sources(repo, list(files.values())) as snapshot:
    original = {name: snapshot[path] for name, path in files.items()}

    def reset():
        for name, path in files.items():
            path.write_bytes(original[name])

    try:
        # Old root/provider order, plus the real r6/r7 outer scan guards. Cache
        # shortening remains: the old W/G edges alone must reproduce the bug.
        for revision in [r for r in (sys.argv[2:] or ['r6', 'r7']) if r in ['r6', 'r7']]:
            reset()
            fm = original['file_memory.rs'].decode()
            fm = once(fm, '''        if let Some(dependency) = dependency {
            operations.extend(dependency.operations.iter().cloned());
        }''', '        let _ = dependency;')
            reserve(revision + '-build')
            write('file_memory.rs', fm)
            ov = original['storage/overview.rs'].decode()
            old = '''        let readable_projects = self.general_catalog()?;
        let (_workspace_lock, _global_lock) = self.read_scope()?;'''
            if revision == 'r6':
                new = '''        let (_workspace_lock, _global_lock) = self.read_scope()?;
        let readable_projects = self.general_catalog()?;'''
            else:
                new = '''        let _workspace_lock = self.workspace_memory.operation_scope()?;
        let readable_projects = self.general_catalog()?;
        let _global_lock = self.global_user_memory.operation_scope()?;'''
            write('storage/overview.rs', once(ov, old, new, 2))
            binary = build(revision + '-build')
            for topology in ['general-single', 'two-general', 'three-general']:
                for cache in ['cold', 'warm']:
                    label = f'{revision}-{topology}-{cache}'
                    status, log = run([binary, 'scan_lock_topologies_finish', '--nocapture', '--test-threads=1'], label,
                                      {'PDRS_R9_PARENT_CASE': f'{topology}/{cache}'})
                    if revision == 'r7' and topology == 'general-single':
                        assert status != 0 and 'A held a store while entering a sibling' in log, log
                        print(label + ': invariant FAIL (r7 completes this topology but still holds own W)', flush=True)
                    else:
                        assert status != 0 and 'child exceeded deadlock deadline (killed and reaped)' in log, log
                        print(label + ': deadline FAIL, child killed/reaped', flush=True)
        if not sys.argv[2:] or 'snapshot' in sys.argv[2:]:
            reset()
            sn = original['storage/snapshots.rs'].decode()
            # fix-r2: fix-r1 moved `snapshot_operation` behind
            # `snapshot_operation_scope`; the equivalent defense is that
            # scope's root-ordered acquisition. Reproduce the old
            # workspace-then-global order so the test still fails on its
            # `snapshot/entries must use the same root order` assertion.
            sn = once(sn, """        let workspace = self.workspace_memory.operation_scope()?;
        let global = self.global_user_memory.operation_scope()?;""", """        let roots = [self.workspace_memory.get_root_dir(), self.global_user_memory.get_root_dir()];
        let locks: Vec<&'static std::sync::Mutex<()>> = roots.iter().map(|p| super::super::file_memory::operation_lock(p).map(|lock| {
            let leaked: &'static std::sync::Arc<std::sync::Mutex<()>> = Box::leak(Box::new(lock));
            &**leaked
        })).collect::<Result<Vec<_>>>()?;
        let mut guards = locks.iter().map(|l| super::super::file_memory::OperationGuard::enter(*l)).collect::<Result<Vec<_>>>()?;
        let global = vec![guards.pop().expect("snapshot root order mutation")];
        let workspace = guards;""")
            reserve('snapshot-order-build')
            write('storage/snapshots.rs', sn)
            binary = build('snapshot-order-build')
            status, log = run([binary, 'provider_and_composites_acquire_the_complete_root_set_in_order', '--nocapture'], 'snapshot-order')
            assert status != 0 and 'snapshot/entries must use the same root order' in log, log
            print('snapshot-order: FAIL (standalone snapshot root-order assertion)', flush=True)
            if sys.argv[2:] == ['snapshot']:
                sys.exit(0)
        if not sys.argv[2:] or 'overview' in sys.argv[2:]:
            reset()
            ov = original['storage/overview.rs'].decode()
            ov = once(ov, "    pub fn get_overview(&self) -> Result<JsValue> {", "    pub fn get_overview(&self) -> Result<JsValue> {\n        let _old_outer = self.workspace_memory.operation_scope()?;")
            reserve('standalone-overview-build')
            write('storage/overview.rs', ov)
            binary = build('standalone-overview-build')
            status, log = run([binary, 'standalone_overview_lock_topologies_finish', '--nocapture', '--test-threads=1'], 'standalone-overview', {'PDRS_R9_PARENT_CASE': 'general-single/cold'})
            assert status != 0 and 'child exceeded deadlock deadline (killed and reaped)' in log, log
            print('standalone-overview: deadline FAIL, child killed/reaped', flush=True)
            if sys.argv[2:] == ['overview']:
                sys.exit(0)
        reset()
        fm = once(original['file_memory.rs'].decode(), '        operations.sort_by(|a, b| a.0.cmp(&b.0));', '')
        reserve('unordered-provider-build')
        write('file_memory.rs', fm)
        binary = build('unordered-provider-build')
        status, log = run([binary, 'provider_and_composites_acquire_the_complete_root_set_in_order', '--nocapture'], 'unordered-provider')
        assert status != 0 and 'assertion `left == right` failed' in log, log
        print('unordered-provider: FAIL (constructor/provider root order assertion)', flush=True)
        reset()
        ac = original['storage/accessors.rs'].decode()
        ac = once(ac, '''        let cache = self
            .repository
            .external
            .lock()
            .expect("external cache poisoned")
            .clone();''', '''        let mut cache = self
            .repository
            .external
            .lock()
            .expect("external cache poisoned");''')
        ac = once(ac, '''        *self
            .repository
            .external
            .lock()
            .expect("external cache poisoned") = next;''', '        *cache = next;')
        reserve('held-cache-build')
        write('storage/accessors.rs', ac)
        binary = build('held-cache-build')
        status, log = run([binary, 'external_cache_is_released_before_cold_and_warm_store_locks', '--nocapture'], 'held-cache')
        assert status != 0 and 'E held while acquiring sibling W' in log, log
        print('held-cache: FAIL (actual cold W acquisition observes E held)', flush=True)
    finally:
        reset()
        for name, path in files.items():
            assert path.read_bytes() == original[name]
        print('all source bytes restored', flush=True)
