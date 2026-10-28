#!/usr/bin/env python3
"""#946 HEAD-only nonce receipt controls for the projector cwd matrix.

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
PROJECTOR = ROOT / 'src/pilotdeck-gateway/src/runtime/turn/projector.rs'
REGRESSION = ROOT / 'src/pilotdeck-gateway/src/runtime/turn/projector_cwd_tests.rs'
FIXTURE = Path(__file__).resolve().parent
PREFIX = 'runtime::turn::projector_cwd_tests::'
MATRIX = '#879 projector cwd matrix:'
MUTATIONS = [
    ('root-cwd-fallback', PROJECTOR,
     '''            std::env::current_dir()
                .map_err(node_cwd_error)?
                .join(root_dir)''',
     '''            std::env::current_dir()
                .map(|path| path.join(&root_dir))
                .unwrap_or_else(|_| PathBuf::from(".").join(&root_dir))''',
     [('relative_deleted_cwd', MATRIX + ' relative-deleted'),
      ('relative_warmed_deleted_cwd', MATRIX + ' relative-warmed-deleted')]),
    ('tmpdir-cwd-fallback', PROJECTOR,
     '''    if tmp.is_absolute() {
        Ok(tmp)
    } else {
        Ok(std::env::current_dir().map_err(node_cwd_error)?.join(tmp))
    }''',
     '''    if tmp.is_absolute() {
        Ok(tmp)
    } else {
        Ok(std::env::current_dir()
            .map(|path| path.join(&tmp))
            .unwrap_or_else(|_| PathBuf::from(".").join(&tmp)))
    }''',
     [('tmpdir_relative_deleted_cwd', MATRIX + ' tmpdir-relative-deleted'),
      ('tmpdir_relative_warmed_deleted_cwd',
       MATRIX + ' tmpdir-relative-warmed-deleted')]),
    ('cwd-syscall', PROJECTOR, 'diagnostic_or_unknown(&error, "uv_cwd", None)',
     'diagnostic_or_unknown(&error, "read", None)',
     [('relative_deleted_cwd', MATRIX + ' relative-deleted'),
      ('tmpdir_relative_deleted_cwd', MATRIX + ' tmpdir-relative-deleted')]),
    ('cwd-rust-display', PROJECTOR,
     '''fn node_cwd_error(error: io::Error) -> io::Error {
    let (_, message) = crate::node_io::diagnostic_or_unknown(&error, "uv_cwd", None);
    io::Error::new(error.kind(), message)
}''',
     '''fn node_cwd_error(error: io::Error) -> io::Error {
    io::Error::new(error.kind(), error.to_string())
}''',
     [('relative_deleted_cwd', MATRIX + ' relative-deleted'),
      ('tmpdir_relative_deleted_cwd', MATRIX + ' tmpdir-relative-deleted')]),
    ('absolute-root-reads-cwd', PROJECTOR,
     '''        let root_dir = if root_dir.is_absolute() {
            root_dir
        } else {''',
     '''        let root_dir = if root_dir.is_absolute() {
            std::env::current_dir().map_err(node_cwd_error)?;
            root_dir
        } else {''',
     [('absolute_deleted_cwd', MATRIX + ' absolute-deleted')]),
    ('absolute-tmpdir-reads-cwd', PROJECTOR,
     '''    if tmp.is_absolute() {
        Ok(tmp)
    } else {''',
     '''    if tmp.is_absolute() {
        std::env::current_dir().map_err(node_cwd_error)?;
        Ok(tmp)
    } else {''',
     [('tmpdir_absolute_deleted_cwd', MATRIX + ' tmpdir-absolute-deleted')]),
    ('relative-root-base', PROJECTOR,
     'let root_dir = resolve_lexically(root_dir);',
     'let root_dir = resolve_lexically(root_dir).join("mutated");',
     [('relative_normal_cwd', MATRIX + ' relative-normal'),
      ('tmpdir_relative_normal_cwd', MATRIX + ' tmpdir-relative-normal')]),
    ('tmpdir-name', PROJECTOR,
     'const TOOL_RESULT_DIR_NAME: &str = "pilotdeck-tool-results";',
     'const TOOL_RESULT_DIR_NAME: &str = "mutated-tool-results";',
     [('tmpdir_relative_normal_cwd', MATRIX + ' tmpdir-relative-normal'),
      ('tmpdir_absolute_deleted_cwd', MATRIX + ' tmpdir-absolute-deleted')]),
]


def main():
    return matrix_main(ROOT, __file__, PREFIX, MUTATIONS,
                       passed_count=10, child_count=10,
                       paths=[PROJECTOR, REGRESSION,
                              FIXTURE / "oracle.json", FIXTURE / "generate.mts"],
                       any_cell=(), exact=False)


if __name__ == '__main__':
    raise SystemExit(main())
