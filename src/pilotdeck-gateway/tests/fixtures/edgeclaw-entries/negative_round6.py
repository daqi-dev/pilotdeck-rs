#!/usr/bin/env python3
"""Round 6: compile each regression, require its runtime test to fail, restore bytes."""
import argparse
import contextlib
import hashlib
import io
import json
import os
from pathlib import Path
import runpy
import subprocess
import sys
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

p = argparse.ArgumentParser()
p.add_argument('--out', required=True)
p.add_argument('--only')
args = p.parse_args()
root = Path(__file__).resolve().parents[5]
out = Path(args.out).resolve()
out.mkdir(parents=True, exist_ok=True)
base = root/'src/pilotdeck-gateway/src/runtime/memory/edgeclaw'
e, a, f, s = [base/path for path in ['storage/entries.rs', 'storage/accessors.rs', 'file_memory.rs', 'storage/snapshots.rs']]
o = base/'storage/overview.rs'
n, n1, n2 = [Path(__file__).with_name(path)
             for path in ['negative.py', 'negative_fix1.py', 'negative_ids.py']]
mutations = [
    ('global-panic', e, 'fn map_record(v: JsValue) -> Result<JsValue> {',
     'fn map_record(v: JsValue) -> Result<JsValue> { let _ = v.as_object().expect("file record");', 'malformed_global_record_returns_internal_error'),
    ('external-panic', a, 'fn external_record_path(record: &JsObject) -> Result<&JsString> {',
     'fn external_record_path(record: &JsObject) -> Result<&JsString> { let _ = record.get("relativePath").and_then(JsValue::as_string).unwrap();', 'malformed_external_record_returns_internal_error'),
    # #597 moved the scandir behind `regular_files_at` on unix (the old
    # read_dir anchor only compiled on non-unix, so it stopped killing the
    # test); unwrap the live mutation_error mapping instead.
    ('file-scandir', f, 'Err(e) => Err(self.mutation_error(dir, "scandir", e)),',
     'Err(e) => Err(e.into()),', 'file_scandir_matches_real_ts'),
    # Exact reversion to the original unwrapped external read_dir / collection.
    ('external-scandir', a, '''fs::read_dir(root)
                .map_err(|e| crate::node_io::path_error(e, "scandir", root))?
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|e| crate::node_io::path_error(e, "scandir", root))?''',
     'fs::read_dir(root)?.collect::<std::result::Result<Vec<_>, _>>()?', 'external_scandir_matches_real_ts'),
    ('snapshot-scandir', s, '''fs::read_dir(p)
            .map_err(|e| crate::node_io::path_error(e, "scandir", p))?''', 'fs::read_dir(p)?', 'snapshot_scandir_matches_real_ts'),
    ('snapshot-read', s, 'fs::read(&p).map_err(|e| crate::node_io::path_error(e, "open", &p))?',
     'fs::read(&p)?', 'snapshot_read_matches_real_ts'),
    # snapshots.rs reads metadata.json in two stages (#656); each stage keeps
    # its own guard mutation.
    ('metadata-read', s, 'fs::File::open(&path).map_err(|e| crate::node_io::path_error(e, "open", &path))?',
     'fs::File::open(&path)?', 'metadata_read_matches_real_ts'),
    ('metadata-read-stage', s, '.map_err(|e| crate::node_io::descriptor_error(e, "read"))?',
     '.map_err(|e| crate::node_io::path_error(e, "open", &path))?',
     # module-qualified: the bare name also matches upload's sibling test
     'round6_tests::metadata_directory_reports_the_read_stage'),
    ('snapshot-mkdir', s, 'io_mkdir(p)?;', 'fs::create_dir_all(p)?;', 'snapshot_mkdir_matches_real_ts'),
    ('snapshot-write', s, 'io_write(p, content)?;', 'fs::write(p, content)?;', 'snapshot_write_matches_real_ts'),
    ('copy-unlink', s, 'fs::remove_file(dst).map_err(|e| crate::node_io::path_error(e, "unlink", dst))?',
     'fs::remove_file(dst)?', 'copy_tree_unlink_matches_real_ts_delete_boundary'),
    ('copy-mkdir', s, 'io_mkdir(dst)?;', 'fs::create_dir_all(dst)?;', 'copy_tree_mkdir_matches_node_boundary'),
    ('copy-scandir', s, 'fs::read_dir(src).map_err(|e| crate::node_io::path_error(e, "scandir", src))?',
     'fs::read_dir(src)?', 'copy_tree_scandir_matches_node_boundary'),
    ('mkdir-hook', s, '''h(SnapshotIoOperation::Mkdir, p, None)
        .map_err(|e| crate::node_io::path_error(e, "mkdir", p))?;''',
     'h(SnapshotIoOperation::Mkdir, p, None)?;', 'mkdir_hook_matches_real_node_error'),
    ('write-hook', s, 'h(SnapshotIoOperation::Write, p, None).map_err(|e| crate::node_io::path_error(e, "open", p))?;',
     'h(SnapshotIoOperation::Write, p, None)?;', 'write_hook_matches_real_node_error'),
]
# Runner meta-controls compile verified snapshots in memory. The real runner
# files stay at HEAD so a nested workspace guard never accepts dirty inputs.
runner_mutations = []
for path, test, run_anchor, slot_anchor in [
    (n, 'test_negative', 'run_owned(cmd,',
     "wait_for_slot(root/'scripts/cargo-slots.sh')"),
    (n1, 'test_negative_fix1', 'run_owned(cmd,',
     "wait_for_slot(root/'scripts/cargo-slots.sh')"),
    (n2, 'test_negative_ids',
     'target.write_text(text.replace(old, new))\n            try:\n                run = run_owned(command,',
     "assert text.count(old) == 1, (name, text.count(old))\n            slot = wait_for_slot(root/'scripts/cargo-slots.sh')"),
]:
    runner_mutations.extend([
        (path.stem+'-unowned', path, run_anchor,
         run_anchor.replace('run_owned(', '__import__("subprocess").run('), test),
        (path.stem+'-slot', path, slot_anchor,
         slot_anchor.replace("wait_for_slot(root/'scripts/cargo-slots.sh')",
                             "__import__('subprocess').run([str(root/'scripts/cargo-slots.sh')])"),
         test),
    ])
rows = []
env = os.environ.copy()
target = root/'_build/cargo-target'
assert Path(env.get('CARGO_TARGET_DIR', target)).resolve() == target.resolve(), '#1166 use this worktree _build/cargo-target only'
env['CARGO_TARGET_DIR'] = str(target)
originals = {}
runner_originals = {}


def record(name, run, log):
    (out/(name+'.log')).write_text(log)
    failures = [line for line in log.splitlines()
                if 'panicked at' in line or 'left:' in line or 'right:' in line
                or 'unowned subprocess.run' in line]
    rows.append({'name': name, 'exit': run.returncode, 'failures': failures})
    print(f'{name}: detected at runtime; exit {run.returncode}; '+ '; '.join(failures)[:500],
          flush=True)


def runner_control(test, path, candidate):
    # #1166 runpy reads its code through io.open_code. Override only that
    # runner's execution stream; Path.read_bytes/Git still observe real HEAD.
    compile(candidate, str(path), 'exec')
    original_open = io.open_code

    def code_reader(filename):
        if Path(filename).resolve() == path.resolve():
            return io.BytesIO(candidate.encode())
        return original_open(filename)

    script = str(Path(__file__).with_name('test_negative_runners.py'))
    argv = [script, 'RunnerControls.' + test]
    stream = io.StringIO()
    previous = sys.argv
    code = 0
    with contextlib.redirect_stdout(stream), contextlib.redirect_stderr(stream), \
            patch.object(io, 'open_code', side_effect=code_reader):
        sys.argv = argv
        try:
            runpy.run_path(script, run_name='__main__')
        except SystemExit as error:
            code = error.code if isinstance(error.code, int) else 1
        finally:
            sys.argv = previous
    return subprocess.CompletedProcess(argv, code, stream.getvalue(), '')


# One lock and one baseline check cover product sources *and* the runner
# scripts executed by the meta-controls (only their in-memory code changes).
# The runner takes the same lock
# reentrantly in this process (see runner_control), so holding the outer
# snapshot cannot deadlock the inner runner.
selected_runner_paths = [n, n1, n2] if not args.only or any(m[0] == args.only for m in runner_mutations) else []
with mutating_sources(root, [e, a, f, s, o] + selected_runner_paths) as originals:
    for name, path, old, new, test in mutations:
        if args.only and args.only != name:
            continue
        text = originals[path].decode()
        assert text.count(old) == 1, (name, text.count(old))
        # Reserve the Cargo slot before exposing the mutation.
        slot = wait_for_slot(root/'scripts/cargo-slots.sh')
        path.write_text(text.replace(old, new))
        try:
            command = ['cargo', 'test', '--manifest-path', 'src/Cargo.toml', '--locked', '--offline',
                       '-p', 'pilotdeck-gateway', '--lib', test, '--', '--nocapture']
            run = run_owned(command, env=env, cwd=root, timeout=180)
            log = slot.stdout + slot.stderr + run.stdout + run.stderr
            assert run.returncode == 101 and 'test result: FAILED. 0 passed; 1 failed;' in log and 'could not compile' not in log, log[-2000:]
            record(name, run, log)
        finally:
            path.write_bytes(originals[path])
    for name, path, old, new, test in runner_mutations:
        if args.only and args.only != name:
            continue
        text = originals[path].decode()
        assert text.count(old) == 1, (name, text.count(old))
        run = runner_control(test, path, text.replace(old, new, 1))
        log = run.stdout + run.stderr
        assert run.returncode and 'unowned subprocess.run' in log and 'FAILED (failures=1)' in log, log[-2000:]
        record(name, run, log)
    (out/'summary.json').write_text(json.dumps({'results': rows, 'restoredSha256': {
        str(path.relative_to(root)): hashlib.sha256(content).hexdigest()
        for path, content in originals.items()
    }}, indent=2)+'\n')
assert len(rows) == (1 if args.only else len(mutations) + len(runner_mutations)), 'empty or partial negative suite'
