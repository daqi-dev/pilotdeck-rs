#!/usr/bin/env python3
"""#888 / #322 negative controls: run every mutant, then summarize.

Each mutant is applied to one source or fixture file, compile-checked, run
against the test that must turn red, and restored byte-for-byte. The runner
never stops at the first failure: it records KILLED / SURVIVED / INVALID with a
reason for every row and exits non-zero if any row is not KILLED.

Requires an already-committed tree (``mutating_sources`` refuses a dirty
baseline by design) and a caller-provided ``CARGO_TARGET_DIR``. Run from the
worktree root, e.g.:

    CARGO_TARGET_DIR=/abs/cargo-target \
    python3 src/pilotdeck-gateway/tests/fixtures/snapshot-projection/negative_controls.py out/

The long table is intentionally not run inside the delivery sandbox; the
coordinator runs it on the host after commit (see RESULT.md).
"""
import hashlib
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot  # noqa: E402

ROOT = Path(__file__).resolve().parents[5]
SOURCE = ROOT / "src/pilotdeck-gateway/src"
FIXTURES = ROOT / "src/pilotdeck-gateway/tests/fixtures"

# (name, path, before, after, target, test, expected marker)
# `target` is "lib" or the integration-test file stem. The marker is the
# dedicated message that proves the intended assertion fired, not an unrelated
# unwrap/compile error.
mutations = [
    (
        "mkdtemp-eexist-fabricated",
        SOURCE / "runtime/snapshot.rs",
        "    Err(SnapshotProjectionError::node_io_required())\n}",
        '    Err(SnapshotProjectionError::plain_code(\n'
        '        "Error",\n'
        '        "EEXIST",\n'
        '        format!("EEXIST: file already exists, mkdtemp \'{template}\'"),\n'
        "    ))\n}",
        "lib",
        "runtime::snapshot::tests::mkdtemp_exhaustion_is_a_host_boundary",
        "#888 host boundary",
    ),
    (
        "requires-host-false",
        SOURCE / "runtime/snapshot.rs",
        "    pub fn requires_host(&self) -> bool {\n        self.requires_host\n    }",
        "    pub fn requires_host(&self) -> bool {\n        false\n    }",
        "lib",
        "runtime::snapshot::tests::requires_host_covers_both_boundary_shapes",
        "#888 errno shape forwards",
    ),
    (
        "resolver-general-always",
        SOURCE / "metadata.rs",
        "        if requested.units() == general_units {",
        "        if true {",
        "lib",
        "runtime::snapshot::tests::native_project_resolver_matches_ts_registry_rules",
        "#888 absolute marker resolves",
    ),
    (
        "storage-id-ignores-marker",
        SOURCE / "metadata.rs",
        "            if marker_path == target",
        "            if false && marker_path == target",
        "lib",
        "runtime::snapshot::tests::native_project_resolver_matches_ts_registry_rules",
        "#888 marker owns storage id",
    ),
    (
        "project-key-validation-skipped",
        SOURCE / "runtime/snapshot.rs",
        '        _ => Err(SnapshotProjectionError::gateway(\n'
        '            "PROJECT_NOT_FOUND",\n'
        '            "projectKey is required.",\n'
        "        )),",
        "        _ => Ok(()),",
        "lib",
        "runtime::snapshot::tests::gateway_validators_match_inprocess_order",
        "#888 key code",
    ),
    (
        "session-validation-skipped",
        SOURCE / "runtime/snapshot.rs",
        '        _ => Err(SnapshotProjectionError::gateway(\n'
        '            "INVALID_SESSION_KEY",\n'
        '            "sessionKey must be non-empty when provided.",\n'
        "        )),",
        "        _ => Ok(()),",
        "lib",
        "runtime::snapshot::tests::gateway_validators_match_inprocess_order",
        "#888 session code",
    ),
    (
        "identifier-validation-skipped",
        SOURCE / "runtime/snapshot.rs",
        '        _ => Err(SnapshotProjectionError::gateway(\n'
        '            "INVALID_MANAGEMENT_IDENTIFIER",\n'
        '            format!("{label} is required."),\n'
        "        )),",
        "        _ => Ok(()),",
        "lib",
        "runtime::snapshot::tests::gateway_validators_match_inprocess_order",
        "#888 identifier code",
    ),
    (
        "target-validation-skipped",
        SOURCE / "runtime/snapshot.rs",
        '        let target = field(params, "targetProjectKey");\n'
        "        if !target.is_undefined() {\n"
        "            require_management_project_key(target)?;\n"
        "        }",
        '        let target = field(params, "targetProjectKey");\n'
        "        let _ = target;",
        "lib",
        "runtime::snapshot::tests::gateway_validators_match_inprocess_order",
        "#888 target code",
    ),
    (
        "list-validation-skipped",
        SOURCE / "runtime/snapshot.rs",
        '        require_management_project_key(parameter(params, "projectKey")?)?;\n'
        '        require_optional_session_key(parameter(params, "sessionKey")?)?;\n'
        "        self.projection.list(params)",
        "        self.projection.list(params)",
        "lib",
        "runtime::snapshot::tests::gateway_validators_match_inprocess_order",
        "#888 params name",
    ),
    (
        "limit-null-not-default",
        SOURCE / "runtime/snapshot.rs",
        "    let limit = if nullish(value) {\n        100.0",
        "    let limit = if value.is_undefined() {\n        100.0",
        "snapshot_projection",
        "scenarios_match_real_ts_projection",
        "#888 projection op #16 (snapshotList): expected success",
    ),
    (
        "cursor-null-treated-as-omitted",
        SOURCE / "runtime/snapshot.rs",
        "        if !cursor.is_undefined() && start == 0 {",
        "        if !nullish(cursor) && start == 0 {",
        "snapshot_projection",
        "scenarios_match_real_ts_projection",
        "#888 projection op #24 (snapshotList): expected an error",
    ),
    (
        "workspace-filter-dropped",
        SOURCE / "runtime/snapshot.rs",
        "            .filter(|item| item.workspace_id.equals_str(&workspace_id))",
        "            .filter(|_| true)",
        "snapshot_projection",
        "scenarios_match_real_ts_projection",
        "#888 projection op #0 (snapshotList): value mismatch",
    ),
    (
        "restore-target-fallback-undefined",
        SOURCE / "runtime/snapshot.rs",
        "        let target_key = if nullish(target) {",
        "        let target_key = if false {",
        "snapshot_projection",
        "scenarios_match_real_ts_projection",
        "#888 projection op #37 (snapshotRestore): restore directory count",
    ),
    (
        "provider-not-found-lossy",
        SOURCE / "storage/workspace_snapshot.rs",
        '            let mut units = "Snapshot ".encode_utf16().collect::<Vec<_>>();\n'
        "            units.extend_from_slice(snapshot_id);\n"
        '            units.extend(" was not found.".encode_utf16());\n'
        "            return Err(WorkspaceSnapshotError::NotFound(JsString::from_utf16(\n"
        "                units,\n"
        "            )));",
        "            return Err(WorkspaceSnapshotError::NotFound(JsString::from(format!(\n"
        '                "Snapshot {} was not found.",\n'
        "                String::from_utf16_lossy(snapshot_id)\n"
        "            ))));",
        "lib",
        "storage::workspace_snapshot::provider_not_found_keeps_raw_snapshot_units",
        "#888 raw units survive",
    ),
]

# Data mutations that are not source anchors. (name, path, apply, target, test,
# marker) is built below; `mutating_sources` still snapshots the bytes.
ORACLE = FIXTURES / "workspace-snapshot/oracle.json"
GENERATOR = FIXTURES / "snapshot-projection/generate.mts"


def compile_check(target, env):
    wait_for_slot(ROOT / "scripts/cargo-slots.sh")
    args = [
        "cargo",
        "test",
        "--manifest-path",
        "src/Cargo.toml",
        "-p",
        "pilotdeck-gateway",
        "--locked",
        "--offline",
    ]
    args += ["--lib"] if target == "lib" else ["--test", target]
    args.append("--no-run")
    return run_owned(args, cwd=ROOT, env=env, timeout=1800)


def run_test(target, test, env):
    wait_for_slot(ROOT / "scripts/cargo-slots.sh")
    args = [
        "cargo",
        "test",
        "--manifest-path",
        "src/Cargo.toml",
        "-p",
        "pilotdeck-gateway",
        "--locked",
        "--offline",
    ]
    args += ["--lib"] if target == "lib" else ["--test", target]
    args.append(test)
    return run_owned(args, cwd=ROOT, env=env, timeout=1800)


def main():
    out = Path(sys.argv[1]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    if not os.environ.get("CARGO_TARGET_DIR"):
        print("CARGO_TARGET_DIR must be set by the caller", file=sys.stderr)
        return 2
    env = os.environ.copy()
    selected = set(sys.argv[2:])
    rows = [row for row in mutations if not selected or row[0] in selected]
    data_names = {"workspace-snapshot-oracle-truncated", "generator-guard-bypassed"}
    unknown = selected - ({row[0] for row in mutations} | data_names)
    if unknown:
        print(f"unknown mutation names: {sorted(unknown)}", file=sys.stderr)
        return 2
    paths = sorted({row[1] for row in rows} | {ORACLE, GENERATOR})
    results = []

    def record(name, status, reason=""):
        results.append({"name": name, "status": status, "reason": reason})
        print(f"{name}: {status} {reason}", flush=True)

    with mutating_sources(ROOT, paths) as snapshots:
        for name, path, before, after, target, test, marker in rows:
            original = snapshots[path]
            before_bytes = hashlib.sha256(original).hexdigest()
            try:
                content = original.decode("utf-8")
                count = content.count(before)
                if count != 1:
                    record(
                        name,
                        "INVALID",
                        f"stale anchor: expected 1 occurrence, found {count}",
                    )
                    continue
                changed = content.replace(before, after, 1)
                path.write_text(changed, encoding="utf-8")
                compiled = compile_check(target, env)
                log = compiled.stdout + compiled.stderr
                (out / f"{name}.compile.log").write_text(log)
                if compiled.returncode != 0:
                    record(name, "INVALID", "mutant does not compile")
                    continue
                result = run_test(target, test, env)
                log = result.stdout + result.stderr
                (out / f"{name}.log").write_text(log)
                if result.returncode == 0:
                    record(name, "SURVIVED", "named test stayed green")
                elif test in log and marker in log:
                    record(name, "KILLED", "named assertion failed")
                else:
                    record(name, "INVALID", "test failed outside the named assertion")
            except Exception as error:
                # One mutant's bookkeeping/IO error must not hide every later
                # row; restore still runs in `finally`. SystemExit from a
                # restore mismatch deliberately escapes this handler.
                record(
                    name,
                    "INVALID",
                    f"runner error: {type(error).__name__}: {error}",
                )
            finally:
                path.write_bytes(original)
                if hashlib.sha256(path.read_bytes()).hexdigest() != before_bytes:
                    record(name, "INVALID", "restore mismatch")
                    raise SystemExit(2)

        def oracle_truncation():
            # #322-3: truncating the committed oracle must trip the exact pin,
            # not silently replay one scenario. This is a data mutation; no
            # compile.
            name = "workspace-snapshot-oracle-truncated"
            original = snapshots[ORACLE]
            before_bytes = hashlib.sha256(original).hexdigest()
            try:
                data = json.loads(original)
                expected_scenarios = len(data.get("scenarios", []))
                data["scenarios"] = data.get("scenarios", [])[:1]
                # The corpus deliberately carries lone UTF-16 surrogates (the
                # surrogate-readback scenario and the cross-check cases). A raw
                # surrogate cannot be encoded as UTF-8, so escape it: json.loads
                # reads \ud800 back as the same lone surrogate, which is exactly
                # what this mutant must preserve.
                ORACLE.write_text(
                    json.dumps(data, ensure_ascii=True, indent=2) + "\n",
                    encoding="utf-8",
                )
                result = run_test(
                    "workspace_snapshot", "scenarios_match_ts_outcomes_and_trees", env
                )
                log = result.stdout + result.stderr
                (out / f"{name}.log").write_text(log)
                marker = "#322 workspace-snapshot corpus is pinned"
                if result.returncode == 0:
                    record(
                        name,
                        "SURVIVED",
                        f"oracle shrank from {expected_scenarios} to 1",
                    )
                elif marker in log:
                    record(name, "KILLED", "exact corpus pin failed")
                else:
                    record(name, "INVALID", "failed outside the corpus pin")
            except Exception as error:
                record(name, "INVALID", f"runner error: {type(error).__name__}: {error}")
            finally:
                ORACLE.write_bytes(original)
                if hashlib.sha256(ORACLE.read_bytes()).hexdigest() != before_bytes:
                    record(name, "INVALID", "restore mismatch")
                    raise SystemExit(2)

        def generator_bypass():
            # #322-4: bypass the shared ownership guard in the generator's own
            # negative control; the existing `assert.rejects` must fail the run.
            name = "generator-guard-bypassed"
            before = (
                "      () => removeOwned(guardRoot, '/tmp/pdrs-a66-pr4-legacy', "
                "{ recursive: true }, recordingRemove),"
            )
            after = (
                "      () => recordingRemove(resolve(guardRoot, "
                "'/tmp/pdrs-a66-pr4-legacy'), { recursive: true, force: true }),"
            )
            original = snapshots[GENERATOR]
            before_bytes = hashlib.sha256(original).hexdigest()
            try:
                content = original.decode("utf-8")
                count = content.count(before)
                if count != 1:
                    record(
                        name,
                        "INVALID",
                        f"stale anchor: expected 1 occurrence, found {count}",
                    )
                elif not os.environ.get("PDRS_BASELINE"):
                    record(name, "INVALID", "PDRS_BASELINE is unset")
                else:
                    GENERATOR.write_text(
                        content.replace(before, after, 1), encoding="utf-8"
                    )
                    node_out = out / "generator-guard-bypassed.out"
                    node_out.mkdir(parents=True, exist_ok=True)
                    result = run_owned(
                        [
                            "node",
                            "--disable-warning=ExperimentalWarning",
                            "--import",
                            str(
                                Path(os.environ["PDRS_BASELINE"])
                                / "node_modules/tsx/dist/loader.mjs"
                            ),
                            str(GENERATOR),
                            str(node_out),
                        ],
                        cwd=ROOT,
                        env=env,
                        timeout=600,
                    )
                    log = result.stdout + result.stderr
                    (out / f"{name}.log").write_text(log)
                    if result.returncode == 0:
                        record(name, "SURVIVED", "generator accepted the bypass")
                    elif (
                        "Missing expected rejection" in log
                        or "outside the run directory" in log
                    ):
                        record(name, "KILLED", "generator ownership self-check failed")
                    else:
                        record(name, "INVALID", "generator failed outside the guard")
            except Exception as error:
                record(name, "INVALID", f"runner error: {type(error).__name__}: {error}")
            finally:
                GENERATOR.write_bytes(original)
                if hashlib.sha256(GENERATOR.read_bytes()).hexdigest() != before_bytes:
                    record(name, "INVALID", "restore mismatch")
                    raise SystemExit(2)

        # The two data mutations are rows too: select them by name like the
        # source mutations instead of always running both.
        if not selected or "workspace-snapshot-oracle-truncated" in selected:
            oracle_truncation()
        if not selected or "generator-guard-bypassed" in selected:
            generator_bypass()

    (out / "summary.json").write_text(
        json.dumps(results, ensure_ascii=True, indent=2) + "\n", encoding="utf-8"
    )
    killed = sum(row["status"] == "KILLED" for row in results)
    print(f"{killed}/{len(results)} KILLED; see {out / 'summary.json'}", flush=True)
    return 0 if killed == len(results) else 1


if __name__ == "__main__":
    raise SystemExit(main())
