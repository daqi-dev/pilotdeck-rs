#!/usr/bin/env python3
"""Serial mutations; explicit source hash supports uncommitted sandbox work."""
import hashlib
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import run_owned, target_environment, tree_lock

root = Path(__file__).resolve().parents[5]
evidence = Path(sys.argv[1]).resolve()
expected_sha = sys.argv[2]
evidence.mkdir(parents=True, exist_ok=False)
source = root / 'src/pilotdeck-gateway/src/runtime/hooks/exec.rs'
env = target_environment(root)
command = ['cargo', 'test', '--offline', '--manifest-path', str(root / 'src/Cargo.toml'),
           '-p', 'pilotdeck-gateway', '--lib', 'runtime::hooks::exec::output_tests', '--', '--nocapture']
cap = 'if self.units.saturating_add(count) > limit {'
mutants = [
    ('no-cap', cap, 'if false && self.units.saturating_add(count) > limit {'),
    ('byte-count', 'let count = text.encode_utf16().count();', 'let count = text.len();'),
    ('exclusive-bound', cap, 'if self.units.saturating_add(count) >= limit {'),
    ('forget-count', 'self.units += count;', 'self.units += 0 * count;'),
    ('sticky-drop', cap, 'if self.truncated || self.units.saturating_add(count) > limit {'),
    ('missing-marker', 'self.truncated = true;', 'self.truncated = false;'),
    ('skip-flush', 'output.append(limit, &tail);', 'let _ = tail;'),
    ('stop-draining', 'output.append(limit, &text);',
     'output.append(limit, &text); if output.truncated { break; }'),
    ('wrong-default', 'output_limit: crate::fetch_redirect::MAX_STRING_LENGTH,',
     'output_limit: crate::fetch_redirect::MAX_STRING_LENGTH - 1,'),
]

def run(name, must_pass):
    result = run_owned(command, cwd=root, env=env, timeout=300)
    log = result.stdout + result.stderr
    (evidence / f'{name}.log').write_text(log)
    if must_pass:
        assert result.returncode == 0 and '3 passed; 0 failed' in log, name
    else:
        # Compilation failures and watchdog kills cannot masquerade as detection.
        assert result.returncode != 0 and 'test result: FAILED' in log, name
        assert 'error[E' not in log, name
    print(f'{name}: exit={result.returncode}, {"PASS" if must_pass else "DETECTED"}', flush=True)

with tree_lock(root):
    original = source.read_bytes()
    assert hashlib.sha256(original).hexdigest() == expected_sha, 'source hash mismatch; refusing mutation'
    (evidence / 'source.sha256').write_text(expected_sha + '\n')
    run('baseline', True)
    try:
        for name, old, new in mutants:
            assert source.read_bytes() == original, 'unexpected concurrent source edit'
            assert original.count(old.encode()) == 1, f'{name}: anchor must occur once'
            source.write_bytes(original.replace(old.encode(), new.encode()))
            try:
                run(name, False)
            finally:
                source.write_bytes(original)
            run(name + '-restored', True)
    finally:
        assert source.read_bytes() == original, 'source was not restored'
