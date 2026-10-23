#!/usr/bin/env python3
"""#595 tree_files negative controls: always restore original bytes."""
import hashlib
import os
from pathlib import Path
import signal
import subprocess
import sys
import time

REPO = Path(__file__).resolve().parents[5]
LOGS = REPO / '_tmp/20261024-issue-595-tree-files'
IMPORT = REPO / 'src/pilotdeck-gateway/src/skills/import.rs'
TARGET = REPO / 'src/target'
assert Path(os.environ.get('CARGO_TARGET_DIR', TARGET)).resolve() == TARGET.resolve()
ENV = dict(os.environ, CARGO_TARGET_DIR=str(TARGET))
ORIGINAL = IMPORT.read_bytes()
DEADLINE = time.monotonic() + 540

def run(name, args):
    subprocess.run([str(REPO / 'scripts/cargo-slots.sh')], check=True)
    command = ['cargo', 'test', '--manifest-path', str(REPO / 'src/Cargo.toml'),
               '--locked', '--offline', '-p', 'pilotdeck-gateway', *args,
               '--', '--test-threads=4']
    log = LOGS / f'negative-{name}.log'
    LOGS.mkdir(parents=True, exist_ok=True)
    with log.open('w') as output:
        output.write(' '.join(command) + '\n')
        output.flush()
        child = subprocess.Popen(command, cwd=REPO, env=ENV, stdout=output,
                                 stderr=subprocess.STDOUT, start_new_session=True)
        try:
            rc = child.wait(timeout=min(180, max(1, DEADLINE - time.monotonic())))
        except subprocess.TimeoutExpired:
            os.killpg(child.pid, signal.SIGKILL)
            child.wait()
            raise AssertionError(f'{name}: deadline exceeded; not a valid negative')
    text = log.read_text()
    summaries = [line for line in text.splitlines() if line.startswith('test result:')]
    assert rc == 101 and summaries and 'FAILED' in summaries[-1], (name, rc, text[-2000:])
    assert 'could not compile' not in text, text[-2000:]
    print(name, f'exit={rc}', *summaries, flush=True)

def restore():
    IMPORT.write_bytes(ORIGINAL)
    assert IMPORT.read_bytes() == ORIGINAL

try:
    # NC-1: Remove cap check → test that should fail now passes
    text = ORIGINAL.decode()
    start = text.index('if listed.len() > cap {')
    end = text.index('}\n        }', start) + 1
    mutated = text[:start] + text[end:]
    IMPORT.write_text(mutated)
    run('no-cap', ['--lib', 'skills::import::tests::tree_files_exceeds_cap_132'])
    restore()

    # NC-2: Change > to >= → boundary case fails
    text = ORIGINAL.decode()
    mutated = text.replace('if listed.len() > cap {', 'if listed.len() >= cap {', 1)
    assert mutated != text
    IMPORT.write_text(mutated)
    run('boundary', ['--lib', 'skills::import::tests::tree_files_respects_cap_133'])
    restore()

    print('All negative controls passed (NC-1, NC-2)')
except Exception:
    restore()
    raise
