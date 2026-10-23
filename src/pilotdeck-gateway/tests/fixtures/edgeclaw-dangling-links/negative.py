#!/usr/bin/env python3
"""#563 negative control: allow root-internal dangling links; the test must turn red.

Mutates `within` so a failed `fs::canonicalize` falls back to the lexical path
(equivalent to letting a dangling link through), runs the rejection test, then
restores the product source in `finally` and re-runs the positive suite.
"""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import run_owned, wait_for_slot


def main():
    root = Path(__file__).resolve().parents[5]
    output = Path(sys.argv[1]).resolve()
    output.mkdir(parents=True, exist_ok=True)
    source = root / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/file_memory.rs'
    original = source.read_text()
    env = dict(os.environ)
    command = ['cargo', 'test', '--offline', '--locked', '--manifest-path',
               str(root / 'src/Cargo.toml'), '-p', 'pilotdeck-gateway',
               '--test', 'edgeclaw_dangling_links']
    before = '                Ok(_) => fs::canonicalize(path),\n'
    after = '''                Ok(_) => fs::canonicalize(path).or_else(|error| {
                    if error.kind() == std::io::ErrorKind::NotFound {
                        let parent = path.parent().ok_or(error)?;
                        Ok(
                            resolved(parent, boundary, root)?
                                .join(path.file_name().unwrap_or_default()),
                        )
                    } else {
                        Err(error)
                    }
                }),
'''
    assert original.count(before) == 1, 'mutation anchor must be unique'
    mutant = 'allow-dangling-leaf-like-a-missing-path'
    test = 'dangling_links_are_rejected_before_any_io'
    marker = 'must be rejected'
    results = []

    def run(name, selected=None):
        with (output / f'{name}.log').open('w') as log:
            gate = wait_for_slot(root / 'scripts/cargo-slots.sh')
            log.write(gate.stdout + gate.stderr)
            log.flush()
            args = command + (
                ['--', selected, '--exact', '--test-threads', '2', '--nocapture']
                if selected
                else ['--', '--test-threads', '2'])
            result = run_owned(args, cwd=root, env=env, timeout=900)
            log.write(result.stdout + result.stderr)
            return result

    positive = run('positive')
    assert positive.returncode == 0, 'positive tests must pass before mutating'
    try:
        assert source.read_text() == original, 'another writer changed the product source'
        source.write_text(original.replace(before, after, 1))
        try:
            negative = run('negative', test)
            raw = (output / 'negative.log').read_text()
            killed = (negative.returncode == 101
                      and 'test result: FAILED' in raw
                      and marker in raw)
            results.append(dict(name=mutant, test=test, exit=negative.returncode,
                                marker=marker, killed=killed))
            print(f'{mutant}: killed={killed}, exit={negative.returncode}', flush=True)
            assert killed, f'{mutant}: expected a compiled assertion failure'
        finally:
            mutated = original.replace(before, after, 1)
            assert source.read_text() == mutated, 'another writer changed the mutant'
            source.write_text(original)
    finally:
        restored = source.read_text() == original
        (output / 'summary.json').write_text(json.dumps(dict(
            results=results, restored=restored,
            beforeSha256=hashlib.sha256(original.encode()).hexdigest(),
            afterSha256=hashlib.sha256(source.read_bytes()).hexdigest()), indent=2) + '\n')
        assert restored
    restored_run = run('restored')
    assert restored_run.returncode == 0, 'restored product must pass'


if __name__ == '__main__':
    main()
