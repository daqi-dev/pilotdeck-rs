#!/usr/bin/env python3
"""#595 tree_files negative controls: always restore original bytes.

Each control mutates src/pilotdeck-gateway/src/skills/import.rs, expects the
target test to turn red, then restores. NC-5 needs macOS (its target test is
cfg-gated to darwin).
"""
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

def cap_check_span(text):
    """The `if listed.len() > cap { ... }` block inside the listing loop."""
    start = text.index('if listed.len() > cap {')
    end = text.index('}\n        }', start) + 1
    return start, end

try:
    # NC-1: Remove the cap check → the over-cap test passes, turning red.
    text = ORIGINAL.decode()
    start, end = cap_check_span(text)
    IMPORT.write_text(text[:start] + text[end:])
    run('no-cap', ['--lib', 'skills::import::tests::tree_files_exceeds_cap_132'])
    restore()

    # NC-2: Change > to >= → the boundary test fails, turning red.
    text = ORIGINAL.decode()
    mutated = text.replace('if listed.len() > cap {', 'if listed.len() >= cap {', 1)
    assert mutated != text
    IMPORT.write_text(mutated)
    run('boundary', ['--lib', 'skills::import::tests::tree_files_respects_cap_133'])
    restore()

    # NC-3: Check the cap only after the whole tree was listed (collect first)
    # → the wide-directory stop-point assertion turns red: the counter records
    # every entry instead of stopping at cap+1.
    text = ORIGINAL.decode()
    start, end = cap_check_span(text)
    block = '\n'.join(line[8:] if line.startswith(' ' * 12) else line
                      for line in text[start:end].split('\n'))
    without = text[:start] + text[end:]
    anchor = without.index('    Ok(listed')
    IMPORT.write_text(without[:anchor] + block + '\n' + without[anchor:])
    run('collect-first',
        ['--lib', 'skills::import::tests::tree_files_wide_dir_stops_listing_at_cap'])
    restore()

    # NC-4: Count per directory instead of accumulating across the tree → the
    # five-subdirectory test no longer errors, turning red.
    text = ORIGINAL.decode()
    mutated = text.replace(
        'while let Some(next) = queue.pop() {',
        'while let Some(next) = queue.pop() {\n        let start_len = listed.len();', 1)
    mutated = mutated.replace('if listed.len() > cap {',
                              'if listed.len() - start_len > cap {', 1)
    assert mutated != text and 'start_len' in mutated
    IMPORT.write_text(mutated)
    run('per-dir-count',
        ['--lib', 'skills::import::tests::tree_files_accumulates_cap_across_directories'])
    restore()

    # NC-5: Canonicalize the root before walking (drops the /tmp ->
    # /private/tmp prefix symlink, freeing one unit of the kernel's ELOOP
    # budget) → the macOS golden values shift, turning red. Rust-only
    # observability: dag-16 goes from 1/58 (truncated) to 65537/196666.
    text = ORIGINAL.decode()
    signature = 'fn tree_files_capped(dir: &str, cap: usize) -> Result<(f64, f64)> {\n'
    canonicalize = ('    let dir = match fs::canonicalize(dir) '
                    '{ Ok(p) => p.to_string_lossy().into_owned(), Err(_) => dir.to_owned() };\n')
    mutated = text.replace(signature, signature + canonicalize, 1)
    assert mutated != text
    IMPORT.write_text(mutated)
    run('canonicalize-root',
        ['--lib', 'skills::import::tests::tree_files_matches_ts_baseline_macos'])
    restore()

    print('All negative controls passed (NC-1..NC-5)')
except Exception:
    restore()
    raise
