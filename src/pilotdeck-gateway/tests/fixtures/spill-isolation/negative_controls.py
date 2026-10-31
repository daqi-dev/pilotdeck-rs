#!/usr/bin/env python3
# anchor-selfcheck: unsupported name-dependent function slicing and sequential rewrites
"""Candidate-baseline checked regressions; compilation errors never count as red."""
import difflib
import os
from pathlib import Path
import sys
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot, target_environment

root = Path(__file__).resolve().parents[5]
source = root / 'src/pilotdeck-gateway/src/runtime/context/tool_budget.rs'
out = Path(sys.argv[1]).resolve()
out.mkdir(parents=True, exist_ok=True)
baseline = os.environ.get('PDRS_NEGATIVE_BASELINE', 'HEAD')
env = target_environment(root)

def mutate(text, name):
    if name == 'namespace':
        old = 'context_token: format!("context-{}", uuid::Uuid::new_v4()),'
        assert text.count(old) == 1
        return text.replace(old, 'context_token: "context-fixed".to_owned(),')
    if name == 'physical-generation':
        start = text.index('        state.spill_paths.insert(')
        end = text.index('        ticket.resolve();', start)
        return text[:start] + text[end:]
    if name == 'wait-eviction':
        old = 'retire_spill(&mut self.spills.lock().unwrap(), path, &ticket);'
        assert text.count(old) == 1
        return text.replace(old, '// mutant leaves the expired ticket registered')
    if name == 'claim-expiry':
        old = '|| self.elapsed() >= self.bound'
        assert text.count(old) == 1
        return text.replace(old, '|| false')
    if name == 'late-publication':
        old = 'if !resolve.current('
        assert text.count(old) == 2
        return text.replace(old, 'if false && !resolve.current(')
    if name == 'owner-identity':
        start = text.index('impl Drop for ResolveSpill {')
        end = text.index('/// Replacement cache', start)
        return text[:start] + '''impl Drop for ResolveSpill {
    fn drop(&mut self) {
        if let Some(ticket) = self.state.lock().unwrap().in_flight_spills.remove(&self.path) { ticket.resolve(); }
    }
}

''' + text[end:]
    if name == 'media-return-path':
        old = 'let path = self.persist_artifact(&path, &data).await?;'
        assert text.count(old) == 1
        return text.replace(old, 'self.persist_artifact(&path, &data).await?;')
    if name == 'context-drop':
        start = text.index('impl Drop for ToolResultBudget {')
        end = text.index('impl ToolResultBudget {', start)
        return text[:start] + text[end:]
    if name == 'leader-expiry':
        old = '&& !self.ticket.abandoned.load(std::sync::atomic::Ordering::Acquire)'
        # rustfmt may wrap the load expression; use the single current() tail.
        start = text.index('    fn current(')
        end = text.index('\n}\n', start)
        part = text[start:end]
        assert '.abandoned' in part
        part = part.replace('&& !self.ticket.abandoned.load(std::sync::atomic::Ordering::Acquire)', '&& !self.ticket.wait_expired()')
        if part == text[start:end]:
            pos = part.index('            && !self')
            part = part[:pos] + '            && !self.ticket.wait_expired()\n    }'
        return text[:start] + part + text[end:]
    if name == 'registry-sharing':
        old = 'spills: spill_registry(&tool_results_dir),'
        assert text.count(old) == 1
        return text.replace(old, 'spills: std::sync::Arc::new(Mutex::new(SpillState::default())),')
    if name == 'global-bound':
        old = 'self.spill_wait_timeout = bound;'
        assert text.count(old) == 1
        text = text.replace(old, old + ' MUTANT_BOUND_MS.store(bound.as_millis() as u64, std::sync::atomic::Ordering::SeqCst);')
        text = text.replace('const EXACT_TOKEN_COUNT_MAX_BYTES:', 'static MUTANT_BOUND_MS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(30_000);\nconst EXACT_TOKEN_COUNT_MAX_BYTES:', 1)
        return text.replace('spill_wait_timeout: SPILL_WAIT_TIMEOUT,', 'spill_wait_timeout: Duration::from_millis(MUTANT_BOUND_MS.load(std::sync::atomic::Ordering::SeqCst)),')
    if name == 'nested-layout':
        old = '&self.tool_results_dir,\n            &JsString::from(format!('
        assert text.count(old) == 1
        return text.replace(old, '&join(&self.tool_results_dir, &self.context_token.as_str().into()),\n            &JsString::from(format!(')
    if name == 'heartbeat':
        start = text.index('let record = tokio::task::spawn_blocking(move || {')
        end = text.index('return Ok((to_reference_block(&record), true));', start)
        block = text[start:end]
        block = block.replace('tokio::task::spawn_blocking(move || {', '(move || {', 1)
        tail = block.index('                    })\n                    .await')
        block = block[:tail] + '                    })()?;\n                    '
        return text[:start] + block + text[end:]
    raise ValueError(name)

names = sys.argv[2:] or ['namespace','physical-generation','wait-eviction','claim-expiry','late-publication','owner-identity','media-return-path','heartbeat','leader-expiry','registry-sharing','global-bound','nested-layout','context-drop']
for name in names:
    with mutating_sources(root, [source], baseline=baseline) as snapshots:
        original = snapshots[source].decode()
        changed = mutate(original, name)
        with (out / f'{name}.patch').open('x') as f:
            f.write(''.join(difflib.unified_diff(original.splitlines(True),changed.splitlines(True),fromfile=str(source),tofile=str(source))))
        source.write_text(changed)
        slot = wait_for_slot(root / 'scripts/cargo-slots.sh')
        test = {
            'namespace': 'isolation::same_id_sequential_two',
            'physical-generation': 'isolation::media_stale_owner_cannot_revoke_successor',
            'wait-eviction': 'isolation::same_id_never_one',
            'claim-expiry': 'isolation::slow_leader_with_follower',
            'late-publication': 'isolation::slow_leader_with_follower',
            'owner-identity': 'isolation::media_stale_owner_cannot_revoke_successor',
            'media-return-path': 'isolation::media_stale_owner_cannot_revoke_successor',
            'heartbeat': 'context_storage_spill_does_not_starve_same_runtime_tasks',
            'leader-expiry': 'isolation::slow_',
            'registry-sharing': 'isolation::shared_registry_serializes_same_final_path',
            'global-bound': 'isolation::wait_bound_is_per_budget',
            'nested-layout': 'native_archive::tests::budget_spills_are_readable_by_archive',
            'context-drop': 'isolation::dropping_context_releases_retry_paths',
        }[name]
        tests = [test]
        if name == 'namespace':
            tests.append('context_storage_shared_directory_isolates_sanitized_filenames')
        if name == 'leader-expiry':
            tests = ['isolation::slow_leader_without_follower', 'isolation::slow_media_without_follower']
        if name == 'late-publication':
            tests = ['isolation::slow_leader_with_follower', 'isolation::slow_media_with_follower']
        logs = []
        for test in tests:
            slot = wait_for_slot(root / 'scripts/cargo-slots.sh')
            command = ['cargo','test','--manifest-path',str(root/'src/Cargo.toml'),'--offline','--locked','-p','pilotdeck-gateway','--lib',test,'--','--nocapture','--test-threads=1']
            result = run_owned(command, cwd=root, env=env, timeout=300)
            log = result.stdout + result.stderr
            logs.append(slot.stdout+slot.stderr+' '.join(command)+'\n'+log)
            with (out / f'{name}-{test.split("::")[-1]}.log').open('x') as f: f.write(logs[-1])
            assert result.returncode == 101 and 'test result: FAILED' in log and 'could not compile' not in log, (name,result.returncode,log[-3000:])
            if name == 'heartbeat': assert 'heartbeat starved:' in log, log[-3000:]
            print(name, test, *[line for line in log.splitlines() if line.startswith('test result:')], flush=True)
        with (out / f'{name}.log').open('x') as f: f.write('\n'.join(logs))
