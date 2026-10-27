#!/usr/bin/env python3
"""#946 HEAD-only nonce receipt controls; account for every selected mutant.

--check validates anchors. --compile-only compiles every selected mutant.
Provide a new output directory and optional mutation names; CARGO_TARGET_DIR
must be supplied. Commit first: the source guard is never bypassed.
"""
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
# negative_suite owns mutating_sources, run_owned and wait_for_slot.
from negative_suite import matrix_main

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



def main():
    return matrix_main(ROOT, __file__, PREFIX, MUTATIONS,
                       passed_count=12, child_count=12,
                       paths=[SOURCE, FIXTURE / "regression.rs", FIXTURE / "oracle.json", FIXTURE / "generate.mts"],
                       any_cell=(), exact=False)


if __name__ == '__main__':
    raise SystemExit(main())
