#!/usr/bin/env python3
"""#293 WorkspaceSnapshot gate negative controls: product/test mutations that
must turn the dedicated assertions red, then restore.

Run with no other writers/builds in this worktree. Evidence goes in a new
output folder. Every mutant runs (never aborting on the first failure), is
compile-checked first — a mutant that cannot compile is INVALID, not KILLED —
and the summary exits nonzero on anything but KILLED.

Every bound failure point is deterministic (a missing directory, a planted
manifest.json directory, a held seam): no mode-000 dependency, so the controls
also fail identically under root (fix-r2, boundary#1: the old root refusal
and its indistinguishable exit code are gone with the EACCES tests). One
fix-r4 exception: `early-exit-unreadable-manifest` binds a permission-000
manifest, and its test skips under root (chmod cannot make the manifest
unreadable there), so that control needs a non-root run.

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

# fix-r3: the fallback per-file publish over an occupied partition — the
# manifest rename (first) and the committed-marker rename (last: the commit
# point) are one anchor. Mutants swap the order wholesale or drop one leg.
FALLBACK_ORDER = """            fs::rename(&staged_manifest, &paths.manifest_path).map_err(|e| {
                two_path_message(&e, "rename", &staged_manifest, &paths.manifest_path).to_string()
            })?;
            fs::rename(&staged_marker, &paths.committed_path).map_err(|e| {
                two_path_message(&e, "rename", &staged_marker, &paths.committed_path).to_string()
            })?;
"""
# fix-r3: the manifest leg alone, for the drop-one-leg mutant.
FALLBACK_MANIFEST = """            fs::rename(&staged_manifest, &paths.manifest_path).map_err(|e| {
                two_path_message(&e, "rename", &staged_manifest, &paths.manifest_path).to_string()
            })?;
"""
# fix-r3/fix-r4: the committed early-exit — the marker plus a manifest in
# `list()`'s own readable state (fix-r4: `fs::read`, not mere file presence).
EARLY_EXIT = """        if exists(&paths.committed_path) && fs::read(&paths.manifest_path).is_ok() {
"""
# fix-r4 (r4 review P1-2): the tree comparison's one private path —
# `<snapshot_root>/.staging`, never "any .staging at the recorded root".
TREE_STAGING = """        if staging == Some(path.as_path()) {
            continue;
        }
"""
# fix-r4 (r4 review P1-2): the temporaries check's residue arm — a `.staging`
# outside the snapshot root is reported, not excused.
TEMPORARIES_RESIDUE = """            } else {
                problems.push(format!(
                    "#293 fix-r4 .staging outside the snapshot root is residue: {}",
                    path.display()
                ));
            }
"""
# fix-r3: no settle path may remove the shared .staging parent (P1-B race).
FAILURE_CLEANUP = """fn failure_cleanup(staging: &Path) {
    let _ = fs::remove_dir_all(staging);
}
"""
COMMIT_TAIL = """    let _ = fs::remove_dir(staging);
    Ok(())
"""

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
    # fix-r2: the TS catch-block deletion, restored on the failure arm — the
    # unconditional unlink of the shared publish paths that every r1/r2 P0
    # was a rediscovery of. M4's survivor assertion must catch it.
    ("cleanup-unconditional-delete", LIB,
     """                failure_cleanup(&staging);
""",
     """                let _ = fs::remove_file(&paths.manifest_path);
                let _ = fs::remove_file(&paths.committed_path);
                failure_cleanup(&staging);
""",
     ["--test", "workspace_snapshot", "concurrent_failure_cleanup_keeps"],
     "#293 R1's committed manifest survives"),
    # fix-r2: the registered divergence — the walk skips the root's .staging
    # so an in-flight round is never a listed snapshot. Renaming the skip
    # target lets the staged manifest through, and M7 must catch it.
    ("staging-listed", LIB,
     """        if root_level && name == ".staging" {
""",
     """        if root_level && name == ".staging-listed" {
""",
     ["--test", "workspace_snapshot", "list_ignores_in_flight"],
     "#293 an in-flight staging manifest is not listed"),
    # fix-r3: the fallback publish order — manifest first, marker last (the
    # commit point). The swap is the fix-r2 order whose interrupted publish
    # left a marker-only half-commit the early-exit then reported as
    # Committed; the reworked M6 must catch it at the marker-not-published
    # assertion.
    ("fallback-order", LIB, FALLBACK_ORDER,
     """            fs::rename(&staged_marker, &paths.committed_path).map_err(|e| {
                two_path_message(&e, "rename", &staged_marker, &paths.committed_path).to_string()
            })?;
            fs::rename(&staged_manifest, &paths.manifest_path).map_err(|e| {
                two_path_message(&e, "rename", &staged_manifest, &paths.manifest_path).to_string()
            })?;
""",
     ["--test", "workspace_snapshot", "fallback_publish_writes"],
     "#293 fix-r3 the committed marker is the commit point"),
    # fix-r3: dropping only the manifest leg (r3 review boundary P3: the old
    # mutant replaced both renames while its name said "drops-manifest")
    # still reports Committed while the concurrent round's manifest stays in
    # place; M3's last-writer assertion must catch the silent stale read.
    ("fallback-drops-manifest", LIB, FALLBACK_MANIFEST,
     """            let _ = &staged_manifest;
""",
     ["--test", "workspace_snapshot", "publish_over_a_concurrent"],
     "#293 the last writer's manifest wins"),
    # fix-r3: the early-exit must require the manifest beside the marker —
    # reverting to the marker alone resurrects the false Committed over a
    # marker-only residue; the orphan-marker test must catch it at the
    # republished-manifest read.
    ("early-exit-marker-only", LIB, EARLY_EXIT,
     """        if exists(&paths.committed_path) {
""",
     ["--test", "workspace_snapshot", "orphan_committed_marker"],
     "#293 fix-r3 the republished manifest is readable"),
    # fix-r4 (P1-1): the manifest leg of the early exit is readability in
    # `list()`'s own terms (fs::read), not mere file presence. Reverting to
    # the structural is_file() check resurrects the false Committed over a
    # permission-000 manifest; the unreadable-manifest test must catch it at
    # the republished-manifest read. Needs a non-root run (see the module
    # docstring).
    ("early-exit-unreadable-manifest", LIB, EARLY_EXIT,
     """        if exists(&paths.committed_path) && paths.manifest_path.is_file() {
""",
     ["--test", "workspace_snapshot", "committed_marker_with_unreadable"],
     "#293 fix-r4 the republished manifest is readable"),
    # fix-r4 (P1-2): the tree comparison's staging exception is the exact
    # snapshot-root path. Widening it back to "any .staging at the recorded
    # root" hides a restore target's .staging residue from the comparison;
    # the target-tree assertion must catch it.
    ("tree-skips-target-staging", TEST, TREE_STAGING,
     """        if directory == root && path.ends_with(".staging") {
            let _ = staging;
            continue;
        }
""",
     ["--test", "workspace_snapshot", "restore_target_staging"],
     "#293 fix-r4 the target tree records the restore target's .staging"),
    # fix-r4 (P1-2): the temporaries check's residue arm. Dropping the report
    # excuses an empty .staging anywhere again; the same test's temporaries
    # assertion must catch it.
    ("temporaries-excuse-any-staging", TEST, TEMPORARIES_RESIDUE,
     """            }
""",
     ["--test", "workspace_snapshot", "restore_target_staging"],
     "#293 fix-r4 the temporaries check flags the restore target's .staging"),
    # fix-r3 (P1-B): the failure cleanup must not remove the shared .staging
    # parent — a removal races another round's create_dir_all into a
    # permanent _FAILED.json. The parent-survival test must catch the
    # restored removal on its failed-round arm.
    ("cleanup-removes-staging-parent", LIB, FAILURE_CLEANUP,
     """fn failure_cleanup(staging: &Path) {
    let _ = fs::remove_dir_all(staging);
    if let Some(parent) = staging.parent() {
        let _ = fs::remove_dir(parent);
    }
}
""",
     ["--test", "workspace_snapshot", "staging_parent_survives"],
     "#293 fix-r3 the staging parent survives a failed round"),
    # fix-r3 (P1-B): the committed round's publish tail must not remove the
    # parent either; the same test's committed-round arm must catch it.
    ("commit-removes-staging-parent", LIB, COMMIT_TAIL,
     """    let _ = fs::remove_dir(staging);
    if let Some(parent) = staging.parent() {
        let _ = fs::remove_dir(parent);
    }
    Ok(())
""",
     ["--test", "workspace_snapshot", "staging_parent_survives"],
     "#293 fix-r3 the staging parent survives a committed round"),
    # fix-r2: the failure cleanup must remove this round's own staging
    # directory; skipping the removal leaks the private area into the
    # snapshot root, and M4's cleanup assertion must catch it.
    ("staging-leak", LIB,
     """    let _ = fs::remove_dir_all(staging);
""",
     """    let _ = staging;
""",
     ["--test", "workspace_snapshot", "concurrent_failure_cleanup_keeps"],
     "#293 R2 removed its own staging directory"),
    # fix-r2: the staging rename IS the publish. A rename onto itself succeeds
    # without publishing, so nothing lands on the partition and M1's lib
    # assertion must catch it.
    ("publish-noop", LIB,
     """    match fs::rename(staging, &paths.partition) {
""",
     """    match fs::rename(staging, staging) {
""",
     ["--lib", "commit_publishes_the_staging_directory_atomically"],
     # The capture pre-creates the (empty) partition, so a no-op publish fails
     # at the content assertion, not at the existence `.expect`.
     "#293 the published partition holds exactly the marker and the manifest"),
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
