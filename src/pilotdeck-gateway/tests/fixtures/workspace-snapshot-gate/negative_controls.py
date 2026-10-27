#!/usr/bin/env python3
"""#293 WorkspaceSnapshot gate negative controls: product/test mutations that
must turn the dedicated assertions red, then restore.

Run with no other writers/builds in this worktree. Evidence goes in a new
output folder. Every mutant runs (never aborting on the first failure), is
compile-checked first — a mutant that cannot compile is INVALID, not KILLED —
and the summary exits nonzero on anything but KILLED.

Because `mutating_sources` proves the tree equals HEAD, this must run after
the #293 patch is committed; a pre-commit working tree is refused on purpose.

Usage:
  CARGO_TARGET_DIR=<dir> python3 -I negative_controls.py <out-dir> [name ...]
"""
import difflib
import hashlib
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

root = Path(__file__).resolve().parents[5]
if not os.environ.get("CARGO_TARGET_DIR"):
    sys.exit("CARGO_TARGET_DIR must be passed by the caller (rule: no second build tree)")
if hasattr(os, "geteuid") and os.geteuid() == 0:
    # Root reads through mode-000 directories, so the EACCES-based tests the
    # cleanup mutations are judged on cannot fail under root (boundary#3).
    sys.exit("refusing to run as root: the mode-000 based controls cannot fail there")

LIB = "src/pilotdeck-gateway/src/storage/workspace_snapshot.rs"
TEST = "src/pilotdeck-gateway/tests/workspace_snapshot.rs"

# fix-r1: the escape guard is the lexical check AND the NOFOLLOW component
# walk, so the mutant must revert both layers at once to reproduce the
# pre-hardening read (a single-layer revert is its own mutation below).
ESCAPE_CHECK = """                if !units_within(&snapshot_root, source_units.units()) {
                    return Err(WorkspaceSnapshotError::ObjectKeyEscape);
                }
                let source_text = String::from_utf16_lossy(source_units.units());
                // #293 fix-r1 (A6.6-SNAPSHOT-OBJECTKEY-001-R1): the lexical
                // check above cannot see symlinks, and a canonicalize-then-
                // read would leave a swap-link race. The read below opens the
                // root once and then every remaining component with
                // O_NOFOLLOW, so a link in the tree — final component,
                // dangling, or a mid-directory link — is refused instead of
                // read, and the boundary check and the open are the same
                // syscall.
                let contents =
                    read_object_within_root(&snapshot_root, source_units.units(), &source_text)?;
"""
INODE_CHECK = """    if !owned_file(&graveyard).is_some_and(|current| current.is(file)) {
        // Someone else's publish sat at the path; put it back unless a newer
        // file has taken the path in the meantime.
        if fs::symlink_metadata(&file.path).is_err() {
            let _ = fs::rename(&graveyard, &file.path);
        }
        return;
    }
    run_round_hook(hook, SnapshotRoundStage::BeforeOwnedUnlink);
"""
GRAVEYARD = """    let graveyard = graveyard_name(&file.path);
    // Move off the shared publish path first: whatever sits there goes to a
    // name only this round can reach, so the delete below can never hit a
    // concurrent round's file however it interleaves.
    if fs::rename(&file.path, &graveyard).is_err() {
        return; // already gone, or not movable: nothing provably ours to delete
    }
    if !owned_file(&graveyard).is_some_and(|current| current.is(file)) {
        // Someone else's publish sat at the path; put it back unless a newer
        // file has taken the path in the meantime.
        if fs::symlink_metadata(&file.path).is_err() {
            let _ = fs::rename(&graveyard, &file.path);
        }
        return;
    }
    run_round_hook(hook, SnapshotRoundStage::BeforeOwnedUnlink);
    let _ = fs::remove_file(&graveyard);
"""
MANIFEST_IDENTITY = """    markers.manifest = Some(owned_manifest.renamed_to(&paths.manifest_path));
"""
MARKER_IDENTITY = """    Ok(owned.renamed_to(path))
"""
FAILURE_CLEANUP = """                failure_cleanup(&markers, &temporary_dir, self.hook.as_ref());
"""
NOFOLLOW_FLAGS = """        let mut flags = OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW;
"""
HOST_BOUNDARY = """fn host_boundary_io() -> WorkspaceSnapshotError {
    WorkspaceSnapshotError::Io {
        code: None,
        message: String::new(),
    }
}
"""
ARR_PANIC = """    if let JsValue::Array(items) = value {
        items
    } else {
        panic!("#293 oracle field {context} is not an array: {value:?}")
    }"""

UNIT = "two_path_message_uses_the_shared_diagnostic"

# (name, file, old, new, cargo test args, kill criterion substring)
MUTATIONS = [
    ("objectkey-escape-allowed", LIB, ESCAPE_CHECK,
     """                let source_text = String::from_utf16_lossy(source_units.units());
                let contents =
                    fs::read(&source_text).map_err(|e| read_object_error(&e, &source_text))?;
""",
     ["--test", "workspace_snapshot", "--test", "snapshot_projection"],
     "#293 expected ObjectKeyEscape"),
    ("objectkey-symlink-followed", LIB, NOFOLLOW_FLAGS,
     "        let mut flags = OFlags::RDONLY | OFlags::CLOEXEC;",
     ["--test", "workspace_snapshot", "restore_refuses_object_keys_through_symlinks"],
     "#293 expected ObjectKeyEscape; a symlinked objectKey must be refused"),
    ("cleanup-unconditional-delete", LIB, FAILURE_CLEANUP,
     """                let _ = fs::remove_file(&paths.manifest_path);
                let _ = fs::remove_file(&paths.committed_path);
                let _ = fs::remove_dir_all(&temporary_dir);""",
     ["--test", "workspace_snapshot", "concurrent_failure_cleanup_keeps"],
     "#293 R1's committed manifest survives"),
    ("cleanup-no-ownership-check", LIB, INODE_CHECK,
     "    run_round_hook(hook, SnapshotRoundStage::BeforeOwnedUnlink);",
     ["--lib", "remove_if_owned_checks_inode_ownership"],
     "the foreign manifest is still readable"),
    # fix-r1: stat-then-unlink on the shared path with the identity check and
    # the seam kept — exactly the old check-then-unlink shape (Codex P0-3).
    ("cleanup-unlink-by-shared-path", LIB, GRAVEYARD,
     """    if !owned_file(&file.path).is_some_and(|current| current.is(file)) {
        return;
    }
    run_round_hook(hook, SnapshotRoundStage::BeforeOwnedUnlink);
    let _ = fs::remove_file(&file.path);
""",
     ["--test", "workspace_snapshot", "owned_delete_window_cannot_hit"],
     "#293 the concurrent round's manifest is readable"),
    # fix-r1: the manifest identity taken from the shared path after the
    # rename — the old post-rename stat (Codex P0-2).
    ("ownership-capture-after-rename", LIB, MANIFEST_IDENTITY,
     "    markers.manifest = owned_file(&paths.manifest_path);",
     ["--test", "workspace_snapshot", "failing_round_keeps_manifest_renamed_over"],
     "#293 the concurrent round's manifest is readable"),
    ("ownership-committed-capture-after-rename", LIB, MARKER_IDENTITY,
     '    Ok(owned_file(path).expect("#293 atomic marker identity"))',
     ["--test", "workspace_snapshot", "committed_marker_ownership_survives"],
     "#293 the concurrent round's marker is readable"),
    ("host-boundary-rust-display", LIB, HOST_BOUNDARY,
     """fn host_boundary_io() -> WorkspaceSnapshotError {
    WorkspaceSnapshotError::Io {
        code: None,
        message: "host io failure".into(),
    }
}""",
     ["--lib", UNIT],
     "#293 out-of-table errno is the empty-message host boundary"),
    ("two-path-no-suffix", LIB,
     """            message: format!("{message} -> '{}'", to.display()),
""",
     "            message,",
     ["--lib", UNIT],
     "#293 two-path rename message"),
    ("arr-silent-fold", TEST, ARR_PANIC,
     """    let _ = context;
    if let JsValue::Array(items) = value {
        items
    } else {
        &[]
    }""",
     ["--test", "workspace_snapshot", "arr_panics_on_missing_or_non_array"],
     "#293 a non-array oracle field must panic"),
    ("expected-shrink", TEST,
     """    ("nul-capture-root", 2, 0),
];""",
     "];",
     ["--test", "workspace_snapshot", "scenarios_match_ts_outcomes"],
     "#322 workspace-snapshot corpus is pinned"),
]

selected = sys.argv[2:]
if selected:
    known = {mutation[0] for mutation in MUTATIONS}
    unknown = [name for name in selected if name not in known]
    if unknown:
        sys.exit(f"unknown mutation names: {unknown}")
    MUTATIONS = [m for m in MUTATIONS if m[0] in selected]

out = Path(sys.argv[1]).resolve()
out.mkdir(parents=True, exist_ok=False)
env = os.environ.copy()
summary = []

files = sorted({root / relative for _, relative, _, _, _, _ in MUTATIONS})
with mutating_sources(root, files) as snapshots:
    for name, relative, old, new, test_args, criterion in MUTATIONS:
        path = root / relative
        original = snapshots[path]
        before = hashlib.sha256(original).hexdigest()
        content = original.decode()
        try:
            if content.count(old) != 1:
                # A stale anchor is an error exit, never a SURVIVED verdict.
                summary.append(dict(name=name, status="INVALID",
                                    reason=f"anchor count {content.count(old)} != 1"))
                print(f"{name}: INVALID anchor count {content.count(old)} != 1", flush=True)
                continue
            changed = content.replace(old, new, 1)
            (out / f"{name}.patch").write_text("".join(difflib.unified_diff(
                content.splitlines(True), changed.splitlines(True),
                fromfile=relative, tofile=relative)))
            # Wait for a cargo slot, like every other cargo invocation here.
            slots = wait_for_slot(root / "scripts/cargo-slots.sh", timeout=1800)
            (out / f"{name}.slots.log").write_text(slots.stdout + slots.stderr)
            path.write_text(changed)
            # Rule 10-27 #4: prove the mutant compiles before judging it.
            check = run_owned(["cargo", "test", "--manifest-path", "src/Cargo.toml",
                               "-p", "pilotdeck-gateway", "--locked", "--offline",
                               "--no-run"] + test_args,
                              cwd=root, env=env, timeout=900)
            (out / f"{name}.check.log").write_text(check.stdout + check.stderr)
            if check.returncode != 0:
                summary.append(dict(name=name, status="INVALID",
                                    reason="mutant failed to compile"))
                print(f"{name}: INVALID compile failure", flush=True)
                continue
            result = run_owned(["cargo", "test", "--manifest-path", "src/Cargo.toml",
                                "-p", "pilotdeck-gateway", "--locked", "--offline"]
                               + test_args,
                               cwd=root, env=env, timeout=900)
            log = result.stdout + result.stderr
            (out / f"{name}.log").write_text(log)
            failed = result.returncode == 101 and "... FAILED" in log
            dedicated = criterion in log
            killed = failed and dedicated
            failure = next((line.strip() for line in log.splitlines()
                            if criterion in line), "")
            status = "KILLED" if killed else "SURVIVED"
            summary.append(dict(name=name, status=status, returncode=result.returncode,
                                dedicated_message=dedicated, failure=failure,
                                sourceSha256=before))
            print(f"{name}: {status} rc={result.returncode} "
                  f"dedicated={dedicated} {failure}", flush=True)
        finally:
            path.write_bytes(original)
            assert hashlib.sha256(path.read_bytes()).hexdigest() == before
            (out / "summary.json").write_text(
                json.dumps(summary, ensure_ascii=False, indent=2) + "\n")

not_killed = [entry for entry in summary if entry["status"] != "KILLED"]
print(f"{len(summary) - len(not_killed)}/{len(MUTATIONS)} killed; "
      f"every source restored byte-for-byte", flush=True)
if not_killed:
    sys.exit(f"non-KILLED mutants: {[entry['name'] for entry in not_killed]}")
