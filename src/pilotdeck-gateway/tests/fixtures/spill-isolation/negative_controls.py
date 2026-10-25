#!/usr/bin/env python3
"""Candidate-baseline checked regressions; compilation errors never count as red."""
import difflib
import os
from pathlib import Path
import sys
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

root = Path(__file__).resolve().parents[5]
source = root / 'src/pilotdeck-gateway/src/runtime/context/tool_budget.rs'
out = Path(sys.argv[1]).resolve()
out.mkdir(parents=True, exist_ok=True)
baseline = os.environ.get('PDRS_NEGATIVE_BASELINE', 'HEAD')
env = dict(os.environ)
assert Path(env['CARGO_TARGET_DIR']).resolve() == root / '_build/cargo-target'

def mutate(text, name):
    if name == 'namespace':
        start = text.index('            spill_directory: join(')
        end = text.index('            tool_results_dir,', start)
        return text[:start] + '            spill_directory: tool_results_dir.clone(),\n' + text[end:]
    if name == 'physical-generation':
        start = text.index('        state.spill_paths.insert(')
        end = text.index('        ticket.resolve();', start)
        return text[:start] + text[end:]
    if name == 'wait-eviction':
        old = 'retire_spill(&mut self.state.lock().unwrap(), path, &ticket);'
        assert text.count(old) == 1
        return text.replace(old, '// mutant leaves the expired ticket registered')
    if name == 'claim-expiry':
        old = '|| self.started.elapsed() >= spill_wait_timeout()'
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
    if name == 'heartbeat':
        start = text.index('let record = tokio::task::spawn_blocking(move || {')
        end = text.index('return Ok((to_reference_block(&record), true));', start)
        block = text[start:end]
        block = block.replace('tokio::task::spawn_blocking(move || {', '(move || {', 1)
        tail = block.index('                    })\n                    .await')
        block = block[:tail] + '                    })()?;\n                    '
        return text[:start] + block + text[end:]
    raise ValueError(name)

names = sys.argv[2:] or ['namespace','physical-generation','wait-eviction','claim-expiry','late-publication','owner-identity','media-return-path','heartbeat']
for name in names:
    with mutating_sources(root, [source], baseline=baseline) as snapshots:
        original = snapshots[source].decode()
        changed = mutate(original, name)
        with (out / f'{name}.patch').open('x') as f:
            f.write(''.join(difflib.unified_diff(original.splitlines(True),changed.splitlines(True),fromfile=str(source),tofile=str(source))))
        source.write_text(changed)
        slot = wait_for_slot(root / 'scripts/cargo-slots.sh')
        test = 'context_storage_spill_does_not_starve_same_runtime_tasks' if name == 'heartbeat' else 'runtime::context::tool_budget::isolation'
        command = ['cargo','test','--manifest-path',str(root/'src/Cargo.toml'),'--offline','--locked','-p','pilotdeck-gateway','--lib',test,'--','--nocapture','--test-threads=1']
        result = run_owned(command, cwd=root, env=env, timeout=180)
        log = result.stdout + result.stderr
        with (out / f'{name}.log').open('x') as f: f.write(slot.stdout+slot.stderr+' '.join(command)+'\n'+log)
        assert result.returncode == 101 and 'test result: FAILED' in log and 'could not compile' not in log, (name,result.returncode,log[-3000:])
        if name == 'heartbeat': assert 'heartbeat starved:' in log, log[-3000:]
        print(name, *[line for line in log.splitlines() if line.startswith('test result:')], flush=True)
