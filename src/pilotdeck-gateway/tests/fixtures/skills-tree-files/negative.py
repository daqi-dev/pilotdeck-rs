#!/usr/bin/env python3
"""#595 controls: mutate disposable source copies, never the working tree.

Uses this tree's existing CARGO_TARGET_DIR. Each mutation must compile, run its
named test and fail an assertion; a restored-source run must then pass. SIGINT,
SIGTERM and timeouts kill/reap the owned child before removing the copy.
"""
import hashlib
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile

REPO = Path(__file__).resolve().parents[5]
LOGS = REPO / '_tmp/20261023-issue-595-tree-files'
RELATIVE = Path('src/pilotdeck-gateway/src/skills/import.rs')
TARGET = REPO / 'src/target'
assert Path(os.environ.get('CARGO_TARGET_DIR', TARGET)).resolve() == TARGET.resolve()
ENV = dict(os.environ, CARGO_TARGET_DIR=str(TARGET))
ORIGINAL = (REPO / RELATIVE).read_bytes()


def stop(signum, _frame):
    raise SystemExit(f'interrupted by signal {signum}')


def run(root, name, test, *, red=False):
    subprocess.run([str(REPO / 'scripts/cargo-slots.sh')], check=True, timeout=10)
    command = ['cargo', 'test', '--manifest-path', str(root / 'src/Cargo.toml'),
               '--locked', '--offline', '-p', 'pilotdeck-gateway', '--lib',
               f'skills::import::tests::{test}', '--', '--exact', '--nocapture',
               '--test-threads=1']
    log = LOGS / f'fix2-{name}.log'
    with log.open('w') as output:
        output.write(f'source-sha256={hashlib.sha256((root / RELATIVE).read_bytes()).hexdigest()}\n')
        output.write(' '.join(command) + '\n')
        output.flush()
        child = subprocess.Popen(command, cwd=root, env=ENV, stdout=output,
                                 stderr=subprocess.STDOUT, start_new_session=True)
        try:
            rc = child.wait(timeout=240)
        finally:
            # Always stop the session, including any test grandchildren, even
            # if the leader already exited. Never leave a build/test behind.
            try:
                os.killpg(child.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            child.wait(timeout=10)
    text = log.read_text()
    assert 'running 1 test' in text and 'could not compile' not in text, (name, rc, text[-4000:])
    expected = 'test result: FAILED. 0 passed; 1 failed' if red else 'test result: ok. 1 passed; 0 failed'
    assert rc == (101 if red else 0) and expected in text, (name, rc, text[-4000:])
    if red:
        assert 'panicked at' in text, (name, text[-4000:])
    print(f'{name}: exit={rc}; pid={child.pid}; reaped={child.poll() is not None}; {expected}', flush=True)


def replace_once(text, old, new):
    assert text.count(old) == 1, (old, text.count(old))
    return text.replace(old, new, 1)


def cap_check_span(text):
    start = text.index('if listed.len() > cap {')
    end = text.index('}\n        }', start) + 1
    return start, end


def mutations(original):
    start, end = cap_check_span(original)
    yield 'no-cap', original[:start] + original[end:], 'tree_files_exceeds_cap_132'
    yield 'boundary', replace_once(original, 'if listed.len() > cap {', 'if listed.len() >= cap {'), 'tree_files_respects_cap_133'
    # NC-3 is precisely the old per-directory eager collect, with the cap
    # check still in the inner loop. The inspect hook must observe all 5001.
    yield 'collect-first', replace_once(original, '        for entry in entries {\n            let Ok(entry) = entry else {',
        '        let entries = entries.collect::<Vec<_>>();\n        for entry in entries {\n            let Ok(entry) = entry else {'), 'tree_files_wide_dir_stops_listing_at_cap'
    per_dir = replace_once(original, 'while let Some(next) = queue.pop() {',
        'while let Some(next) = queue.pop() {\n        let start_len = listed.len();')
    yield 'per-dir-count', replace_once(per_dir, 'if listed.len() > cap {', 'if listed.len() - start_len > cap {'), 'tree_files_accumulates_cap_across_directories'
    signature = 'fn tree_files_capped(dir: &str, cap: usize) -> Result<(f64, f64)> {\n'
    if sys.platform == 'darwin':
        yield 'canonicalize-root', replace_once(original, signature, signature +
            '    let dir = fs::canonicalize(dir).unwrap().to_string_lossy().into_owned();\n'), 'tree_files_matches_ts_baseline_macos'
    else:
        print('NC-5 canonicalize-root: requires macOS; NOT VERIFIED', flush=True)
    dedup = replace_once(original, signature, signature + '    let mut seen = std::collections::HashSet::new();\n')
    dedup = replace_once(dedup, 'while let Some(next) = queue.pop() {',
        'while let Some(next) = queue.pop() {\n        let m = fs::metadata(&next).unwrap();\n        if !seen.insert((m.dev(), m.ino())) { continue; }')
    yield 'inode-dedup', dedup, 'tree_files_matches_ts_baseline'
    if sys.platform == 'darwin':
        # A completed subtree count cached only by inode loses lookup context.
        # The mutual cycle reaches the same directories with different remaining
        # ELOOP budgets; cached shallow/deep results cannot be interchanged.
        body_start = original.index(signature)
        body_end = original.index('// ------------------------------------------------------------------- fs.cp', body_start)
        cached = '''fn tree_files_capped(dir: &str, _cap: usize) -> Result<(f64, f64)> {
    fn walk(dir: &str, cache: &mut std::collections::HashMap<(u64, u64), (f64, f64)>) -> (f64, f64) {
        let m = fs::metadata(dir).unwrap();
        let key = (m.dev(), m.ino());
        if let Some(value) = cache.get(&key) { return *value; }
        let mut result = (0.0, 0.0);
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = join(dir, &entry.file_name().to_string_lossy());
            if let Ok(m) = fs::metadata(&path) {
                if m.is_dir() {
                    let sub = walk(&path, cache);
                    result.0 += sub.0; result.1 += sub.1;
                } else if m.is_file() {
                    result.0 += 1.0; result.1 += m.len() as f64;
                }
            }
        }
        cache.insert(key, result);
        result
    }
    Ok(walk(dir, &mut std::collections::HashMap::new()))
}

'''
        yield 'inode-subtree-cache', original[:body_start] + cached + original[body_end:], 'tree_files_matches_ts_baseline_macos'
    else:
        print('NC-7 inode-subtree-cache: requires macOS; NOT VERIFIED', flush=True)
    retained = replace_once(original, signature, signature + '    let mut held_parents = vec![];\n')
    retained = replace_once(retained, 'while let Some(next) = queue.pop() {',
        'while let Some(next) = queue.pop() {\n        held_parents.push(fs::read_dir(&next).unwrap());')
    yield 'retain-parent-handles', retained, 'tree_files_deep_tree_with_low_fd_limit'
    yield 'swallow-scan-limit', replace_once(original, 'tree_files_capped(dir, cap)?',
        'tree_files_capped(dir, cap).unwrap_or((0.0, 0.0))'), 'scan_propagates_tree_files_limit_without_partial_folders'


def main():
    LOGS.mkdir(parents=True, exist_ok=True)
    handlers = {sig: signal.signal(sig, stop) for sig in (signal.SIGINT, signal.SIGTERM)}
    try:
        with tempfile.TemporaryDirectory(prefix='pdrs595-negative-', dir='/tmp') as name:
            root = Path(name)
            print(f'isolated source copy: {root}', flush=True)
            # Copy tracked inputs, including current uncommitted repairs; never
            # .env, target, git metadata, or any other worktree's evidence.
            files = subprocess.check_output(['git', 'ls-files', '-z', 'src', 'LICENSE'], cwd=REPO)
            for entry in files.split(b'\0'):
                if not entry:
                    continue
                relative = Path(os.fsdecode(entry))
                source = REPO / relative
                if not source.exists():  # intentionally deleted tracked files
                    continue
                destination = root / relative
                destination.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(source, destination)
            target_source = root / RELATIVE
            original = ORIGINAL.decode()
            for label, mutated, test in mutations(original):
                assert mutated != original
                run(root, f'{label}-baseline', test)
                try:
                    target_source.write_text(mutated)
                    run(root, label, test, red=True)
                finally:
                    target_source.write_bytes(ORIGINAL)
                run(root, f'{label}-restored', test)
            print('All applicable controls passed; restored tests green', flush=True)
        assert not root.exists(), f'isolated source copy leaked: {root}'
        print(f'isolated source copy removed: {root}', flush=True)
    finally:
        for sig, handler in handlers.items():
            signal.signal(sig, handler)
        assert (REPO / RELATIVE).read_bytes() == ORIGINAL, 'working source changed'
        print('working source bytes unchanged', flush=True)


if __name__ == '__main__':
    main()
