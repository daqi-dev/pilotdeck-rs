#!/usr/bin/env python3
"""PR #537 round 7: strip each IO wrapper, require runtime assertion failure."""
import hashlib
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

root = Path(__file__).resolve().parents[5]
out = Path(sys.argv[1]).resolve()
out.mkdir(parents=True, exist_ok=True)
path = root/'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/storage/snapshots.rs'
mutations = [
    ('lstat', 'fs::symlink_metadata(src).map_err(|e| crate::node_io::path_error(e, "lstat", src))?',
     'fs::symlink_metadata(src)?', 'round7_lstat_matches_real_ts_fallback'),
    ('readlink', 'fs::read_link(src).map_err(|e| crate::node_io::path_error(e, "readlink", src))',
     'fs::read_link(src)', 'round7_readlink_matches_native_boundary'),
    ('realpath', 'fs::canonicalize(parent).map_err(|e| crate::node_io::path_error(e, "realpath", parent))',
     'fs::canonicalize(parent)', 'round7_realpath_matches_native_boundary'),
    ('symlink', '''std::os::unix::fs::symlink(&target, dst)
            .map_err(|e| pair_error(e, "symlink", &target, dst))?''',
     'std::os::unix::fs::symlink(&target, dst)?', 'round7_symlink_matches_real_ts_readonly_destination'),
    ('chmod', 'fs::set_permissions(dst, permissions).map_err(|e| crate::node_io::path_error(e, "chmod", dst))',
     'fs::set_permissions(dst, permissions)', 'round7_chmod_matches_native_boundary'),
    ('copyfile', 'fs::copy(src, dst).map_err(|e| pair_error(e, "copyfile", src, dst))?',
     'fs::copy(src, dst)?', 'round7_copyfile_matches_real_ts_readonly_destination'),
    ('rename', 'Err(e) => Err(pair_error(e, "rename", src, dst).into()),',
     'Err(e) => Err(e.into()),', 'round7_rename_matches_real_ts_non_permission_error'),
]
env = os.environ.copy()
target = root/'src/target'
assert Path(env.get('CARGO_TARGET_DIR', target)).resolve() == target.resolve(), 'one build directory only'
env['CARGO_TARGET_DIR'] = str(target)
rows = []
# Read the pristine bytes only under the same flock as every other in-place
# negative runner, and reserve the Cargo slot before exposing each mutation.
with mutating_sources(root, [path]) as snapshots:
    original = snapshots[path]
    try:
        for name, old, new, test in mutations:
            text = original.decode()
            assert text.count(old) == 1, (name, text.count(old))
            slot = wait_for_slot(root/'scripts/cargo-slots.sh')
            path.write_text(text.replace(old, new))
            try:
                command = ['cargo', 'test', '--manifest-path', 'src/Cargo.toml', '--locked', '--offline',
                           '-p', 'pilotdeck-gateway', '--lib', test, '--', '--nocapture']
                run = run_owned(command, env=env, cwd=root, timeout=240)
                log = slot.stdout + slot.stderr + '$ ' + ' '.join(command) + '\n' + run.stdout + run.stderr
                (out/(name+'.log')).write_text(log)
                assert run.returncode == 101 and 'test result: FAILED. 0 passed; 1 failed;' in log and 'could not compile' not in log, log[-2000:]
                failures = [line for line in log.splitlines() if 'panicked at' in line or 'left:' in line or 'right:' in line]
                rows.append({'name': name, 'test': test, 'exit': run.returncode, 'failures': failures})
                print(name+': detected runtime assertion; exit 101; '+'; '.join(failures), flush=True)
            finally:
                path.write_bytes(original)
    finally:
        path.write_bytes(original)
        assert path.read_bytes() == original
        (out/'summary.json').write_text(json.dumps({'results': rows, 'restoredSha256': hashlib.sha256(original).hexdigest()}, indent=2)+'\n')
assert len(rows) == len(mutations), 'partial negative suite'
