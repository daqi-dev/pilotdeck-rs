#!/usr/bin/env python3
"""Run after committing the candidate. Never bypass negative_process's HEAD guard."""
import argparse
import difflib
import hashlib
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('output', type=Path, help='fresh evidence directory')
    parser.add_argument('mutations', nargs='*')
    args = parser.parse_args()
    if not os.environ.get('CARGO_TARGET_DIR'):
        parser.error('CARGO_TARGET_DIR must be supplied by the caller')
    root = Path(__file__).resolve().parents[5]
    adapter_source = root / 'src/pilotdeck-gateway/src/storage/transcript_adapter.rs'
    rewind_source = root / 'src/pilotdeck-gateway/src/runtime/rewind.rs'
    env = os.environ.copy()
    env['CARGO_PROFILE_DEV_DEBUG'] = 'line-tables-only'
    env['CARGO_PROFILE_TEST_DEBUG'] = 'line-tables-only'
    mutations = [
        ('cwd-empty-fallback', adapter_source,
         'std::env::current_dir()\n            .map_err(|error| {\n                let (_, message) = crate::node_io::diagnostic_or_unknown(&error, "uv_cwd", None);\n                AdapterError::Store(io::Error::new(error.kind(), message).into())\n            })?\n            .join(base)',
         'std::env::current_dir().unwrap_or_default().join(base)',
         'relative_base_deleted_cwd', 'transcript resolve matrix: relative-deleted'),
        ('absolute-reads-cwd', adapter_source,
         'let path = if base.is_absolute() {',
         'let cwd = std::env::current_dir().map_err(|error| AdapterError::Store(io::Error::new(error.kind(), "ENOENT: no such file or directory, uv_cwd").into()))?;\n    let path = if base.is_absolute() {\n        drop(cwd);',
         'absolute_base_deleted_cwd', 'transcript resolve matrix: absolute-deleted'),
        ('join-loses-fallback', adapter_source,
         'resolve(&path, &[]).unwrap_or_else(|_| normalize(&path, &[]))',
         'resolve(&path, &[]).expect("mutant requires cwd for path.join")',
         'relative_sidechain_deleted_cwd', 'mutant requires cwd for path.join'),
        ('list-cwd-message', adapter_source,
         '.and_then(|storage| storage.read_transcript(None))',
         '.map_err(|_| AdapterError::Store(invalid("mutant list cwd failure")))\n                .and_then(|storage| storage.read_transcript(None))',
         'relative_base_deleted_cwd', 'list propagates cwd failure'),
        ('rewind-cwd-message', rewind_source,
         '.map_err(|error| error.to_string())?;\n    let read = storage',
         '.map_err(|_| "mutant rewind cwd failure".to_owned())?;\n    let read = storage',
         'relative_base_deleted_cwd', 'rewind propagates cwd failure'),
        ('relative-normalization', adapter_source,
         'Ok(normalize(&path, parts))', 'let _ = parts; Ok(path)',
         'relative_base_normal_cwd', 'transcript resolve matrix: relative-normal'),
        ('absolute-normalization', adapter_source,
         'Ok(normalize(&path, parts))', 'let _ = parts; Ok(path)',
         'absolute_base_normal_cwd', 'transcript resolve matrix: absolute-normal'),
        ('join-normal-changes-layout', adapter_source,
         'resolve(&path, &[]).unwrap_or_else(|_| normalize(&path, &[]))',
         'normalize(&path, &[])',
         'relative_sidechain_normal_cwd', 'transcript resolve matrix: join-relative-normal'),
    ]
    # The same rollback must also exercise the separate projectRoot call site.
    name, source, old, new, _, _ = mutations[0]
    mutations.append(('project-' + name, source, old, new,
                      'relative_project_deleted_cwd', 'transcript resolve matrix: project-relative-deleted'))
    if args.mutations:
        selected = set(args.mutations)
        if not selected <= {row[0] for row in mutations}:
            parser.error('unknown mutation name')
        mutations = [row for row in mutations if row[0] in selected]
    args.output.mkdir(parents=True, exist_ok=False)

    def save(name, text):
        with (args.output / name).open('x') as stream:
            stream.write(text)

    def run(label, test):
        slots = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=600, interval=10)
        save(f'{label}.slots.log', slots.stdout + slots.stderr)
        # No -q: cargo forwards it to libtest as --quiet, and the kill check
        # below needs the per-test "test <name> ... FAILED" line that quiet
        # mode replaces with "<name> --- FAILED".
        # No `--nocapture`: capture keeps a printing test's stdout out of the
        # `test <name> ... ok/FAILED` status lines the positive and kill
        # checks below match exactly (#1053).
        result = run_owned([
            'cargo', 'test', '--manifest-path', 'src/Cargo.toml', '-p', 'pilotdeck-gateway',
            '--test', 'transcript_resolve', test, '--', '--exact',
        ], cwd=root, env=env, timeout=600)
        log = result.stdout + result.stderr
        save(f'{label}.log', log)
        save(f'{label}.exit', f'{result.returncode}\n')
        return result, log

    summary = []
    for name, source, old, new, test, assertion in mutations:
        with mutating_sources(root, [source]) as snapshots:
            original = snapshots[source]
            content = original.decode()
            assert content.count(old) == 1, (name, 'anchor count', content.count(old))
            result, log = run(f'{name}-positive', test)
            assert result.returncode == 0 and '1 passed; 0 failed' in log, 'positive did not run/pass'
            changed = content.replace(old, new, 1)
            save(f'{name}.patch', ''.join(difflib.unified_diff(
                content.splitlines(True), changed.splitlines(True), fromfile=str(source), tofile=str(source))))
            source.write_text(changed)
            result, log = run(f'{name}-negative', test)
            killed = (result.returncode == 101 and f'test {test} ... FAILED' in log
                      and assertion in log and 'could not compile' not in log)
        # Restoration and SHA validation belong to mutating_sources, including on failure.
        assert source.read_bytes() == original, 'source not restored'
        # Keep the clean rerun under the same tree guard used by every runner.
        with mutating_sources(root, [source]):
            result, restored = run(f'{name}-restored', test)
        assert result.returncode == 0 and '1 passed; 0 failed' in restored, 'restored positive failed'
        summary.append({'name': name, 'killed': killed, 'sourceSha256': hashlib.sha256(original).hexdigest()})
        save(f'{name}.json', json.dumps(summary[-1], indent=2) + '\n')
        assert killed, f'{name}: no runtime assertion failure; compile/setup failure is not a kill'
        print(f'{name}: positive PASS, runtime RED, restored PASS', flush=True)
    save('summary.json', json.dumps(summary, indent=2) + '\n')


if __name__ == '__main__':
    main()
