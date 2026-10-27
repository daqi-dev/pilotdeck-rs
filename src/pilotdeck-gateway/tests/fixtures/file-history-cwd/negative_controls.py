#!/usr/bin/env python3
"""Committed-tree runtime controls; process every selected mutant before failing.

--check validates anchors without touching product sources. Execution requires
the caller's CARGO_TARGET_DIR and the default HEAD guard, including new fixtures.
"""
import difflib
import json
import os
from pathlib import Path
import re
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

ROOT = Path(__file__).resolve().parents[5]
SOURCE = ROOT / 'src/pilotdeck-gateway/src/storage/file_history.rs'
FIXTURE = Path(__file__).resolve().parent
PREFIX = 'storage::file_history::cwd_tests::'
CWD = '''let cwd = std::env::current_dir().map_err(|error| {
            FileHistoryError::Fs(crate::node_io::diagnostic_or_unknown(&error, "uv_cwd", None).1)
        })?;'''
TRACK = '''pub fn track_edit(&self, file_path: &str, message_id: &str) -> FileHistoryResult<()> {
        let mut state = self.state.lock().unwrap();
        let abs_path = resolve_node_path(file_path)?;'''
MARK = '''pub fn mark_edit_committed(&self, file_path: &str, message_id: &str) -> FileHistoryResult<()> {
        let mut state = self.state.lock().unwrap();
        let abs_path = resolve_node_path(file_path)?;'''
MUTATIONS = [
    ('cwd-root-fallback', SOURCE, CWD,
     'let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));',
     [('track_relative_deleted', '#879 outcome matrix: track-relative-deleted'),
      ('mark_relative_deleted', '#879 outcome matrix: mark-relative-deleted'),
      ('track_relative_warmed_deleted', '#879 outcome matrix: track-relative-warmed-deleted'),
      ('mark_relative_warmed_deleted', '#879 outcome matrix: mark-relative-warmed-deleted')]),
    ('cwd-syscall', SOURCE, 'diagnostic_or_unknown(&error, "uv_cwd", None)',
     'diagnostic_or_unknown(&error, "read", None)',
     [('track_relative_deleted', '#879 outcome matrix: track-relative-deleted')]),
    ('cwd-rust-display', SOURCE,
     'FileHistoryError::Fs(crate::node_io::diagnostic_or_unknown(&error, "uv_cwd", None).1)',
     'FileHistoryError::Fs(error.to_string())',
     [('mark_relative_deleted', '#879 outcome matrix: mark-relative-deleted')]),
    ('absolute-reads-cwd', SOURCE, "let combined = if file_path.starts_with('/') {\n        file_path.to_string()",
     "let combined = if file_path.starts_with('/') {\n"
     '        std::env::current_dir().map_err(|error| FileHistoryError::Fs(\n'
     '            crate::node_io::diagnostic_or_unknown(&error, "uv_cwd", None).1))?;\n'
     '        file_path.to_string()',
     [('track_absolute_deleted', '#879 outcome matrix: track-absolute-deleted'),
      ('mark_absolute_deleted', '#879 outcome matrix: mark-absolute-deleted')]),
    ('track-swallow', SOURCE, TRACK, TRACK.replace(
        'let abs_path = resolve_node_path(file_path)?;',
        'let Ok(abs_path) = resolve_node_path(file_path) else { return Ok(()); };'),
     [('track_relative_deleted', '#879 outcome matrix: track-relative-deleted')]),
    ('mark-swallow', SOURCE, MARK, MARK.replace(
        'let abs_path = resolve_node_path(file_path)?;',
        'let Ok(abs_path) = resolve_node_path(file_path) else { return Ok(()); };'),
     [('mark_relative_deleted', '#879 outcome matrix: mark-relative-deleted')]),
    ('mark-early-missing', SOURCE, MARK, MARK.replace(
        'let abs_path = resolve_node_path(file_path)?;',
        'if find_snapshot(&state.snapshots, message_id).is_none() { return Ok(()); }\n'
        '        let abs_path = resolve_node_path(file_path)?;'),
     [('mark_missing_relative_deleted', '#879 outcome matrix: mark-missing-relative-deleted')]),
    ('track-state-before-error', SOURCE, TRACK, TRACK.replace(
        'let abs_path = resolve_node_path(file_path)?;',
        'state.tracked_files.push(file_path.into());\n'
        '        let abs_path = resolve_node_path(file_path)?;'),
     [('track_relative_deleted', '#879 state matrix: track-relative-deleted')]),
    ('normalize-parent', SOURCE, '''".." => {
                parts.pop();
            }''', '''".." => {
                parts.push("..");
            }''',
     [('track_relative_normal', '#879 state named backup'),
      ('track_absolute_normal', '#879 state named backup')]),
    ('normalize-dot', SOURCE, '"" | "." => {}', '"" => {}',
     [('mark_relative_normal', '#879 state matrix: mark-relative-normal')]),
]


def write_new(path, value):
    with path.open('x') as stream:
        stream.write(value)


def check_anchor(mutation):
    name, source, old, new, targets = mutation
    count = source.read_text().count(old)
    if count != 1 or old == new or not targets or any(not t or not m for t, m in targets):
        raise RuntimeError(f'{name}: invalid anchor/targets (count={count})')


def run(out, label, env):
    slots = wait_for_slot(ROOT / 'scripts/cargo-slots.sh', timeout=600, interval=15)
    write_new(out / f'{label}.slots.log', slots.stdout + slots.stderr)
    command = ['cargo', 'test', '--offline', '-q', '-p', 'pilotdeck-gateway', '--lib', PREFIX,
               '--', '--nocapture', '--format=pretty', '--test-threads=1']
    write_new(out / f'{label}.command.json', json.dumps({
        'command': command, 'cwd': str(ROOT / 'src'),
        'CARGO_TARGET_DIR': env['CARGO_TARGET_DIR']}, indent=2) + '\n')
    result = run_owned(command, cwd=ROOT / 'src', env=env, timeout=600)
    log = result.stdout + result.stderr
    write_new(out / f'{label}.log', log)
    return result, log


def passed(result, log):
    return (result.returncode == 0 and '12 passed; 0 failed; 2 ignored;' in log
            and log.count('PDRS_VERIFIED_CHILD ') == 12)


def classify(result, log, targets):
    if passed(result, log):
        return 'SURVIVED', 'all matrix tests still passed'
    # A target's own captured panic block must contain its dedicated message;
    # a marker printed by a different failed test never counts as a kill.
    expected_failures = []
    for test, marker in targets:
        full = PREFIX + test
        block = re.search(r"thread '" + re.escape(full)
                          + r"'(?: \(\d+\))? panicked at.*?(?=\nthread '" + re.escape(PREFIX)
                          + r"(?!environment_worker')[^']+'(?: \(\d+\))? panicked at|\Z)",
                          result.stderr, re.S)
        expected_failures.append(
            f'{full} ... FAILED' in log and block is not None and marker in block.group())
    if (result.returncode == 101 and 'test result: FAILED.' in log
            and 'error: could not compile' not in log and all(expected_failures)):
        return 'KILLED', 'named runtime assertions failed'
    return 'INVALID', 'missing named runtime failure; compile/setup/timeout is not a kill'


def main():
    selected = sys.argv[2:] if sys.argv[1:] != ['--check'] else []
    mutations = [m for m in MUTATIONS if not selected or m[0] in selected]
    if not mutations or (selected and len(mutations) != len(selected)):
        raise SystemExit('unknown/duplicate mutation')
    if sys.argv[1:] == ['--check']:
        errors = []
        for mutation in mutations:
            try:
                check_anchor(mutation)
            except Exception as error:
                errors.append(str(error))
        if errors:
            raise SystemExit('\n'.join(errors))
        print(f'{len(mutations)} unique anchors checked')
        return
    env = os.environ.copy()
    if not env.get('CARGO_TARGET_DIR'):
        raise SystemExit('CARGO_TARGET_DIR must be supplied by the caller')
    if len(sys.argv) < 2:
        raise SystemExit('usage: negative_controls.py <new-output-directory> [mutation ...]')
    out = Path(sys.argv[1]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    paths = [SOURCE, FIXTURE / 'regression.rs', FIXTURE / 'oracle.json',
             FIXTURE / 'generate.mts', Path(__file__).resolve()]
    results = []
    for mutation in mutations:
        name, source, old, new, targets = mutation
        status, reason, restored = 'INVALID', 'not run', False
        try:
            check_anchor(mutation)
            with mutating_sources(ROOT, paths) as originals:
                original = originals[source].decode()
                if original.count(old) != 1:
                    raise RuntimeError('stale anchor after locking')
                positive, log = run(out, f'{name}-positive', env)
                if not passed(positive, log):
                    raise RuntimeError('positive matrix did not pass with all receipts')
                changed = original.replace(old, new, 1)
                write_new(out / f'{name}.patch', ''.join(difflib.unified_diff(
                    original.splitlines(True), changed.splitlines(True),
                    fromfile=str(source.relative_to(ROOT)), tofile=str(source.relative_to(ROOT)))))
                source.write_text(changed)
                negative, log = run(out, f'{name}-negative', env)
                status, reason = classify(negative, log, targets)
            with mutating_sources(ROOT, paths):
                result, log = run(out, f'{name}-restored', env)
                restored = passed(result, log)
            if not restored:
                status, reason = 'INVALID', 'restored matrix did not pass with all receipts'
        except Exception as error:
            status, reason = 'INVALID', str(error)
        row = dict(name=name, status=status, reason=reason, restoredPass=restored, targets=targets)
        results.append(row)
        write_new(out / f'{name}.result.json', json.dumps(row, indent=2) + '\n')
        print(f'{name}: {status}: {reason}', flush=True)
    write_new(out / 'summary.json', json.dumps(results, indent=2) + '\n')
    counts = {status: sum(r['status'] == status for r in results)
              for status in ('KILLED', 'SURVIVED', 'INVALID')}
    print(json.dumps(counts), flush=True)
    if counts['SURVIVED'] or counts['INVALID']:
        raise SystemExit(1)


if __name__ == '__main__':
    main()
