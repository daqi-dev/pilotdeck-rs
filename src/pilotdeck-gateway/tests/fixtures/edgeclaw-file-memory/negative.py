#!/usr/bin/env python3
"""Run three real product mutations in the exclusively owned worktree; restore it."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys


def main():
    root = Path(__file__).resolve().parents[5]
    output = Path(sys.argv[1]).resolve()
    output.mkdir(parents=True, exist_ok=True)
    source = root / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/file_memory.rs'
    original = source.read_text()
    env = dict(os.environ)
    env.setdefault('CARGO_TARGET_DIR', str(root / '_build/target'))
    command = ['cargo', 'test', '--offline', '--locked', '--manifest-path', str(root / 'src/Cargo.toml'),
               '-p', 'pilotdeck-gateway', '--test', 'edgeclaw_file_memory']
    mutants = [
        ('provider-reentry-deadlocks',
         'if HELD_OPERATIONS.with(|held| held.borrow().contains(&key)) {',
         'if false {',
         'same_root_provider_reentry_finishes_like_synchronous_ts',
         'same-root provider operation must finish within 5s'),
        ('operation-lock-dropped',
         '        self.core.execute(method, input)',
         '        drop(_guard);\n        self.core.execute(method, input)',
         'concurrent_same_origin_keeps_one_record_like_real_ts',
         'same-origin concurrent writes created extra files'),
        ('edit-meta-passes-dream-field',
         'copy(&mut next, input, &["projectName", "description", "status"]);',
         'next = input.clone();',
         'rust_reads_and_writes_the_real_ts_transcripts',
         'edit-meta-whitelist operation 0 editProjectMeta'),
    ]
    for name, before, _, _, _ in mutants:
        assert original.count(before) == 1, (name, 'mutation anchor must be unique')
    results = []

    def run(name, test):
        with (output / f'{name}.log').open('w') as log:
            subprocess.run([str(root / 'scripts/cargo-slots.sh')], cwd=root,
                           stdout=log, stderr=subprocess.STDOUT, check=True)
            return subprocess.run(command + ([test, '--', '--exact', '--nocapture'] if test else []),
                                  cwd=root, env=env, stdout=log, stderr=subprocess.STDOUT,
                                  timeout=600).returncode

    assert run('positive', '') == 0, 'positive tests must pass before mutating'
    try:
        for name, before, after, test, marker in mutants:
            assert source.read_text() == original, 'another writer changed the product source'
            mutated = original.replace(before, after, 1)
            source.write_text(mutated)
            try:
                code = run(name, test)
                raw = (output / f'{name}.log').read_text()
                killed = code == 101 and 'test result: FAILED' in raw and marker in raw
                results.append(dict(name=name, test=test, exit=code, marker=marker, killed=killed))
                print(f'{name}: killed={killed}, exit={code}', flush=True)
                assert killed, f'{name}: expected a compiled test assertion failure'
            finally:
                assert source.read_text() == mutated, 'another writer changed the mutant'
                source.write_text(original)
    finally:
        restored = source.read_text() == original
        (output / 'summary.json').write_text(json.dumps(dict(
            results=results, restored=restored,
            beforeSha256=hashlib.sha256(original.encode()).hexdigest(),
            afterSha256=hashlib.sha256(source.read_bytes()).hexdigest()), indent=2) + '\n')
        assert restored
    assert run('restored', '') == 0, 'restored product must pass'


if __name__ == '__main__':
    main()
