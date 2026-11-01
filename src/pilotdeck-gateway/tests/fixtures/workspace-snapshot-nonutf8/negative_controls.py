#!/usr/bin/env python3
"""#283 WorkspaceSnapshot non-utf8 negative controls: product/gold mutations
that must turn the dedicated Linux gate's assertions red, then restore.

Run with no other writers/builds in this worktree. Every mutant runs (never
aborting on the first failure), is compile-checked first — a mutant that
cannot compile is INVALID, not KILLED — and the summary exits nonzero on
anything but KILLED.

The bound test is `non_utf8_capture_fails_with_lossy_lstat_enoent_on_linux`
(`#[cfg(target_os = "linux")]` in tests/workspace_snapshot.rs): it replays the
committed non-utf8 scenario against the raw `bad-\\xff.txt` workspace and
asserts every field of the failed capture against the gold. Each mutant below
removes exactly one behaviour that assertion chain depends on: the lossy
decoding keeping U+FFFD, the Node-formatted lstat error, the failure itself
(instead of a silent skip), and the gold actually driving the comparison.
fix-r1 adds the same contract for the new preconditions: the gold's raw files
staying inside `workspace`, the gate locating its capture step by `op`, and
the regen guard pinning libuv (`guard-uv-drift` runs the ignored
generator_provenance guard and therefore needs PDRS_BASELINE, which
run-linux.sh exports before calling this script).

Because `mutating_sources` proves the tree equals HEAD, this must run after
the #283 patch is committed; a pre-commit working tree is refused on purpose.

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
ORACLE = "src/pilotdeck-gateway/tests/fixtures/workspace-snapshot/oracle.json"
GATE = "src/pilotdeck-gateway/tests/workspace_snapshot.rs"
GUARD = "src/pilotdeck-gateway/tests/generator_provenance.rs"

# scan_visit's name decoding (workspaceSnapshot.ts:291-310): the lossy name is
# what every later syscall re-encodes, so the U+FFFD must survive into the
# lstat path for the ENOENT to carry Node's exact wording.
LOSSY_NAMES = """    let mut names: Vec<String> = listed
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
"""
# The lstat of the lossy-decoded name: Node's ENOENT wording comes from the
# measured node_io table; both product mutants below target this one call.
LSTAT = """        let info = fs::symlink_metadata(&absolute)
            .map_err(|e| io_message(&e, "lstat", Some(absolute.as_path())))?;
"""
# The gold's recorded error for the non-utf8 scenario (U+FFFD stored raw by
# JSON.stringify). Drifting the syscall name proves the gold drives the
# dedicated test's comparison instead of a self-agreeing echo.
GOLD_ERROR = "ENOENT: no such file or directory, lstat '<dir>/workspace/bad-�.txt'"

DEDICATED = "non_utf8_capture_fails_with_lossy_lstat_enoent_on_linux"
GUARD_TEST = "workspace_snapshot_regenerates_identically"

# (name, file, old, new, cargo test args, kill criterion substring)
MUTATIONS = [
    # The lossy decode folds U+FFFD out of the name: the lstat then targets
    # `bad-.txt` and fails with a DIFFERENT path in the error, so the wording
    # assertion must catch the silent rename.
    ("decode-strip-fffd", LIB, LOSSY_NAMES,
     """    let mut names: Vec<String> = listed
        .flatten()
        .map(|entry| String::from(entry.file_name().to_string_lossy().replace('\\u{FFFD}', "")))
        .collect();
""",
     ["--test", "workspace_snapshot", DEDICATED],
     "#283 the error keeps Node's ENOENT wording for the lossy-decoded name"),
    # The lstat error reverts to Rust's Display: the capture still fails on
    # the same path, but with "No such file or directory (os error 2)".
    ("lstat-rust-display", LIB, LSTAT,
     """        let info = fs::symlink_metadata(&absolute).map_err(|e| e.to_string())?;
""",
     ["--test", "workspace_snapshot", DEDICATED],
     "#283 the error keeps Node's ENOENT wording for the lossy-decoded name"),
    # The lstat failure is swallowed: the bad name is skipped, the capture
    # COMMITS without it, and the state assertion must catch the silent drop.
    ("lstat-skip-entry", LIB, LSTAT,
     """        let Ok(info) = fs::symlink_metadata(&absolute) else {
            continue;
        };
""",
     ["--test", "workspace_snapshot", DEDICATED],
     "#283 the lossy lstat ENOENT fails the capture instead of committing"),
    # The gold's error text drifts (lstat -> stat): the product is right, the
    # comparison is not — the dedicated test must fail against the corrupted
    # gold, proving the recorded error really drives the assertion.
    ("oracle-error-drift", ORACLE, GOLD_ERROR,
     GOLD_ERROR.replace("lstat ", "stat ", 1),
     ["--test", "workspace_snapshot", DEDICATED],
     "#283 the error keeps Node's ENOENT wording for the lossy-decoded name"),
    # The gold records the raw file outside `workspace`: the gate's flat
    # raw self-check compares the listing of `workspace`, so its precondition
    # assert must fire instead of silently comparing the wrong set.
    ("oracle-raw-dir-drift", ORACLE, '"dir": "workspace",',
     '"dir": "elsewhere",',
     ["--test", "workspace_snapshot", DEDICATED],
     "#283 the non-utf8 scenario records its raw files inside the workspace"),
    # The gate locates its capture step by something other than op: the find
    # must come back empty (the scenario's only other step is a tick) and the
    # locating expect must fail, proving the locator discriminates by op.
    ("gate-capture-locator-op", GATE,
     '        .find(|step| s(field(step, "op")) == "capture")',
     '        .find(|step| s(field(step, "op")) == "list")',
     ["--test", "workspace_snapshot", DEDICATED],
     "#283 the non-utf8 scenario records its capture"),
    # The regen guard's libuv pin is loosened: the guard must BLOCK on the uv
    # assertion (the recorded provider-order case is readdir order, a libuv
    # property) instead of drifting into the byte comparison.
    ("guard-uv-drift", GUARD,
     '''        "1.52.1",
        "#283 pinned libuv build (the runner pins the same version)"
''',
     '''        "0.0.0",
        "#283 pinned libuv build (the runner pins the same version)"
''',
     ["--test", "generator_provenance", GUARD_TEST, "--", "--exact", "--ignored"],
     "#283 pinned libuv build"),
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
