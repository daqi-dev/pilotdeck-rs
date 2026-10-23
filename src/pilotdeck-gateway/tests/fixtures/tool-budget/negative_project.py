#!/usr/bin/env python3
"""Bounded #344 project-root / mkdir regression controls; always restore bytes."""
import hashlib
import os
from pathlib import Path
import signal
import subprocess
import sys
import time

REPO = Path(__file__).resolve().parents[5]
LOGS = REPO / '_tmp/20261023-issue-344-spill-toctou'
BUDGET = REPO / 'src/pilotdeck-gateway/src/runtime/context/tool_budget.rs'
FILES = REPO / 'src/pilotdeck-gateway/src/project_files.rs'
TARGET = REPO / 'src/target'
assert Path(os.environ.get('CARGO_TARGET_DIR', TARGET)).resolve() == TARGET.resolve()
ENV = dict(os.environ, CARGO_TARGET_DIR=str(TARGET))
ORIGINAL = {p: p.read_bytes() for p in (BUDGET, FILES)}
DEADLINE = time.monotonic() + 540


def run(name, args):
    subprocess.run([str(REPO / 'scripts/cargo-slots.sh')], check=True)
    command = ['cargo', 'test', '--manifest-path', str(REPO / 'src/Cargo.toml'),
               '--locked', '--offline', '-p', 'pilotdeck-gateway', *args,
               '--', '--test-threads=4']
    log = LOGS / f'r1-negative-{name}.log'
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
    for path, data in ORIGINAL.items():
        path.write_bytes(data)
        assert path.read_bytes() == data


try:
    budget = ORIGINAL[BUDGET].decode()
    # Reproduce the previous spill-root algorithm from the reviewed a656ee8e;
    # retain the test hook at the same pre-mkdir/check boundary.
    old = subprocess.check_output(['git', 'show', 'a656ee8ebb73e7043186eb3461a4c51d84c2fc98:src/pilotdeck-gateway/src/runtime/context/tool_budget.rs'], cwd=REPO).decode()
    old = old[old.index('fn ensure_directory('):old.index('// Keep the canonical paths')]
    old = old.replace('    if !within(root, target) {', '''    let root = target; // regression: trust spill root instead of project
    #[cfg(test)]
    BEFORE_SPILL_MKDIR.with(|hook| {
        if let Some(hook) = hook.borrow_mut().as_mut() { hook(target); }
    });
    if !within(root, target) {''', 1)
    old = old.replace('create_directories(', 'fs::create_dir_all(')
    start = budget.index('fn ensure_directory(')
    end = budget.index('// Non-Unix keeps', start)
    BUDGET.write_text(budget[:start] + old + budget[end:])
    run('spill-root', ['--lib', 'runtime::context::tool_budget::tests::spill_project_rejects_'])
    restore()

    files = ORIGINAL[FILES].decode()
    start = files.index('pub(crate) fn create_directories_beneath(')
    end = files.index('\nfn directory_flags()', start)
    FILES.write_text(files[:start] + '''pub(crate) fn create_directories_beneath(_root: &Path, target: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(target)
}
''' + files[end:])
    run('path-mkdir', ['--lib', 'runtime::context::tool_budget::tests::spill_project_mkdir_swap_'])
    restore()

    needle = 'access | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW'
    assert files.count(needle) == 1
    FILES.write_text(files.replace(needle, 'access | OFlags::DIRECTORY | OFlags::CLOEXEC'))
    run('follow-directories', ['--lib', 'runtime::context::tool_budget::tests::spill_project_rejects_'])
    restore()

    # Skip only the new production layout: older golden cases must still pass.
    actual = files[start:end].replace('fn create_directories_beneath(', 'fn create_directories_baseline(', 1)
    FILES.write_text(files[:start] + '''pub(crate) fn create_directories_beneath(root: &Path, target: &Path) -> io::Result<()> {
    if target.components().any(|c| c.as_os_str() == ".pilotdeck") { return Ok(()); }
    create_directories_baseline(root, target)
}
#[cfg(unix)]
''' + actual + files[end:])
    run('skip-mkdir', ['--test', 'tool_budget', 'real_ts_tool_budget_and_apply_tool_results_replay'])
finally:
    restore()
    for path, data in ORIGINAL.items():
        print('RESTORED', path.relative_to(REPO), hashlib.sha256(data).hexdigest(), flush=True)
