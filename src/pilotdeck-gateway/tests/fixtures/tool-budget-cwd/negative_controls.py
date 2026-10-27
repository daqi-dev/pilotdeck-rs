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
SOURCE = ROOT / 'src/pilotdeck-gateway/src/runtime/context/tool_budget.rs'
FIXTURE = Path(__file__).resolve().parent
PREFIX = 'runtime::context::tool_budget::cwd_tests::'
CWD = '''let cwd = std::env::current_dir().map_err(|error| {
                ToolBudgetError::new(
                    crate::node_io::diagnostic_or_unknown(&error, "uv_cwd", None).1,
                )
            })?;
            join(&JsString::from(cwd.to_string_lossy().as_ref()), &raw)'''
ABSOLUTE = '''let tool_results_dir = if is_absolute(&raw) {
            normalize(&raw)'''
MUTATIONS = [
    ('cwd-root-fallback', SOURCE, CWD,
     'let cwd = std::env::current_dir()\n'
     '                .map(|path| JsString::from(path.to_string_lossy().as_ref()))\n'
     '                .unwrap_or_else(|_| JsString::from("/"));\n'
     '            join(&cwd, &raw)',
     [('relative_deleted', '#885 outcome matrix: relative-deleted'),
      ('relative_warmed_deleted', '#885 outcome matrix: relative-warmed-deleted')]),
    ('cwd-syscall', SOURCE, 'diagnostic_or_unknown(&error, "uv_cwd", None)',
     'diagnostic_or_unknown(&error, "read", None)',
     [('relative_deleted', '#885 outcome matrix: relative-deleted')]),
    ('cwd-rust-display', SOURCE,
     'ToolBudgetError::new(\n'
     '                    crate::node_io::diagnostic_or_unknown(&error, "uv_cwd", None).1,\n'
     '                )',
     'ToolBudgetError::new(error.to_string())',
     [('relative_deleted', '#885 outcome matrix: relative-deleted')]),
    ('absolute-reads-cwd', SOURCE, ABSOLUTE,
     'let tool_results_dir = if is_absolute(&raw) {\n'
     '            let _ = std::env::current_dir().map_err(|error| {\n'
     '                ToolBudgetError::new(\n'
     '                    crate::node_io::diagnostic_or_unknown(&error, "uv_cwd", None).1,\n'
     '                )\n'
     '            })?;\n'
     '            normalize(&raw)',
     [('absolute_deleted', '#885 outcome matrix: absolute-deleted')]),
]



def main():
    return matrix_main(ROOT, __file__, PREFIX, MUTATIONS,
                       passed_count=6, child_count=6,
                       paths=[SOURCE, FIXTURE / "regression.rs", FIXTURE / "oracle.json", FIXTURE / "generate.mts"],
                       any_cell=(), exact=False)


if __name__ == '__main__':
    raise SystemExit(main())
