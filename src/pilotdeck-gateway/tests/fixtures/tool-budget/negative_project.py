#!/usr/bin/env python3
# anchor-selfcheck: unsupported imperative function-scoped replacements
"""Bounded #344 project-root / mkdir regression controls; always restore bytes."""
import hashlib
import os
from pathlib import Path
import signal
import subprocess
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, target_environment

REPO = Path(__file__).resolve().parents[5]
LOGS = REPO / '_tmp/20261023-issue-344-spill-toctou'
# A fresh clone (cb8) has no historical evidence directories yet.
LOGS.mkdir(parents=True, exist_ok=True)
BUDGET = REPO / 'src/pilotdeck-gateway/src/runtime/context/tool_budget.rs'
FILES = REPO / 'src/pilotdeck-gateway/src/project_files.rs'
ENV = target_environment(REPO)
with mutating_sources(REPO, [BUDGET, FILES]) as snapshots:
    ORIGINAL = {p: snapshots[p] for p in (BUDGET, FILES)}
    DEADLINE = time.monotonic() + 540


    def run(name, args):
        subprocess.run([str(REPO / 'scripts/cargo-slots.sh')], check=True, timeout=10)
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
        # Reproduce the pre-fix spill-root algorithm: it is byte-identical to
        # this file's own #[cfg(not(unix))] fallback (verified against the
        # reviewed pre-fix revision). Reading it from the working bytes keeps
        # the control usable in clones that do not carry the historical object
        # (#1166 redesign: no hardcoded baseline SHA).
        fallback = budget[budget.index('// Non-Unix keeps'):]
        old = fallback[fallback.index('fn ensure_directory('):
                       fallback.index('// Keep the checked project root')]
        old = old.replace('    if !within(root, target) {', '''    let root = target; // regression: trust spill root instead of project
        #[cfg(test)]
        fire_spill_hook(&BEFORE_SPILL_MKDIR, target);
        if !within(root, target) {''', 1)
        old = old.replace('create_directories(', 'fs::create_dir_all(')
        start = budget.index('fn ensure_directory(')
        end = budget.index('// Non-Unix keeps', start)
        BUDGET.write_text(budget[:start] + old + budget[end:])
        run('spill-root', ['--lib', 'runtime::context::tool_budget::tests::spill_project_rejects_'])
        restore()

        files = ORIGINAL[FILES].decode()
        for anchor in ('pub(crate) fn create_directories_beneath(',
                       'pub(crate) fn create_directories_at(', 'fn directory_flags()'):
            assert files.count(anchor) == 1, anchor
        start = files.index('pub(crate) fn create_directories_beneath(')
        # End the span at the wrapper's own closing brace, not at `directory_flags`:
        # #597 added `create_directories_at` between the two, and swapping the whole
        # span would delete it (E0425 at the file_memory/projector call sites).
        depth = 0
        end = files.index('{', start)
        while True:
            depth += (files[end] == '{') - (files[end] == '}')
            end += 1
            if depth == 0:
                break
        assert files[start:end].count('fn ') == 1
        FILES.write_text(files[:start] + '''pub(crate) fn create_directories_beneath(_root: &Path, target: &Path) -> io::Result<()> {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(target)
    }''' + files[end:])
        mutated = FILES.read_text()
        for anchor in ('pub(crate) fn create_directories_beneath(',
                       'pub(crate) fn create_directories_at(', 'fn directory_flags()'):
            assert mutated.count(anchor) == 1, ('path-mkdir', anchor)
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
