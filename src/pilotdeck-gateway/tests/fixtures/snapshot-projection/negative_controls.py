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
from negative_panic import named_test_killed
from negative_selection import (baseline_passed, claim_dir, missing_target,
                                refuse_unusable_target, registered,
                                verdict_exit)  # noqa: E402
from negative_process import (mutating_sources, node_version, run_owned,
                              wait_for_slot)  # noqa: E402

ROOT = Path(__file__).resolve().parents[5]
SOURCE = ROOT / "src/pilotdeck-gateway/src"
FIXTURES = ROOT / "src/pilotdeck-gateway/tests/fixtures"
# #2133: neg-for-diff.sh finds this table by the literal `dir/file` of every
# changed .rs file. The rows that name their directory inline
# (`SOURCE / "runtime/snapshot.rs"`, `SOURCE / "storage/workspace_snapshot.rs"`)
# are already discoverable; `SOURCE / "metadata.rs"` takes its directory from
# the constant and needs its anchor. The three shared fixtures are here because
# the loop keys any change under `fixtures/**` and the imports alone do not put
# those literals in this file. Checked row by row by
# fixtures/test_negative_discovery_anchors.py.
DISCOVERY_ANCHORS = (
    'src/metadata.rs',
    'fixtures/negative_panic.py',
    'fixtures/negative_process.py',
    'fixtures/negative_selection.py',
    'fixtures/negative_scope.py',
)

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
        "runtime::snapshot::tests::requires_host_covers_the_errno_boundary_shape",
        "#888 errno shape forwards",
    ),
    # #2194: a dispatch failure is `Error`, never a flattened response body
    # (the memory-gateway family's `error-mapped-to-response` row, mirrored).
    (
        "error-mapped-to-response",
        SOURCE / "runtime/snapshot.rs",
        "            Err(error) => SnapshotDispatch::Error(error),\n",
        "            Err(error) => SnapshotDispatch::Response(error.as_json()),\n",
        "lib",
        "runtime::snapshot::tests::dispatch_routes_only_snapshot_methods",
        "#2194 snapshot_list failure must dispatch as SnapshotDispatch::Error",
    ),
    # #2308: a host boundary is `Forward`, never a success response body
    # (the memory-gateway family's `forward-mapped-to-response` row, mirrored).
    (
        "forward-mapped-to-response",
        SOURCE / "runtime/snapshot.rs",
        "            Err(error) if error.requires_host() => SnapshotDispatch::Forward,\n",
        "            Err(error) if error.requires_host() => {\n"
        "                SnapshotDispatch::Response(error.as_json())\n"
        "            }\n",
        "lib",
        "runtime::snapshot::tests::dispatch_forwards_requires_host_boundary",
        "#2308 snapshot_list must forward a requires_host boundary to the Node host",
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
        "storage-id-stats-normalized-marker",
        SOURCE / "metadata.rs",
        "            let marker_on_disk = self.cwd.join(&marker);",
        "            let marker_on_disk = marker_path.clone();",
        "snapshot_projection",
        "scenarios_match_real_ts_projection",
        "#888 storage-id-missing-dotdot: raw marker existence check keeps the General workspace id",
    ),
    (
        "restore-returns-target-not-mkdtemp",
        SOURCE / "runtime/snapshot.rs",
        '        result.insert("workspaceKey".into(), workspace_key.as_str().into());',
        '        result.insert("workspaceKey".into(), target_project.as_str().into());',
        "snapshot_projection",
        "scenarios_match_real_ts_projection",
        "returned workspaceKey equals the newly created restore directory",
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

# The full roster, in table order, ending with the two data mutations. A run
# that loses a row (deleted or renamed) used to still print `n/n KILLED` and
# exit 0: the denominator shrank silently (#1456 r4, both reviewer lineages).
# `registered` refuses at import time and the run re-checks the recorded
# verdicts against this list before reporting success.
EXPECTED_NAMES = (
    "mkdtemp-eexist-fabricated",
    "requires-host-false",
    "error-mapped-to-response",
    "forward-mapped-to-response",
    "resolver-general-always",
    "storage-id-ignores-marker",
    "storage-id-stats-normalized-marker",
    "restore-returns-target-not-mkdtemp",
    "project-key-validation-skipped",
    "session-validation-skipped",
    "identifier-validation-skipped",
    "target-validation-skipped",
    "list-validation-skipped",
    "limit-null-not-default",
    "cursor-null-treated-as-omitted",
    "workspace-filter-dropped",
    "restore-target-fallback-undefined",
    "provider-not-found-lossy",
    "workspace-snapshot-oracle-truncated",
    "generator-guard-bypassed",
)

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
    args += ["--", "--exact"]
    return run_owned(args, cwd=ROOT, env=env, timeout=1800)


def classify_bound_run(returncode, log, test, marker, *,
                       survived_reason="named test stayed green",
                       killed_reason="named assertion failed",
                       unbound_reason="test failed outside the named assertion"):
    """(status, reason) for one bound-test run.

    #1087: KILLED is only the named test's own FAILED status line plus the
    marker inside that test's own panic payload (negative_panic judges both,
    so a sibling's panic or ordinary stdout is never evidence).
    #1087 fix-r4 (sibling #2): an aborted run is not a kill decision —
    SIGINT/SIGKILL after the bound test's FAILED line leaves a partial
    transcript without libtest's own summary (measured on a real libtest
    process), so the FAILED summary line is a separate requirement, the same
    guard as `_shared/negative_control.py`.
    """
    if returncode == 0:
        return "SURVIVED", survived_reason
    if "test result: FAILED." not in log:
        return "INVALID", "no libtest FAILED summary line (aborted run or harness error)"
    if named_test_killed(log, test, marker):
        return "KILLED", killed_reason
    return "INVALID", unbound_reason


def main():
    requested = sys.argv[2:]
    # A repeated name is a caller-side typo (`a a` for `a b`), not a subset;
    # folding it into a set turns a mistyped selection into a green run over a
    # smaller scope, the same class of false green as an unknown name (#1734).
    repeated = sorted({n for n in requested if requested.count(n) > 1})
    if repeated:
        print(f"repeated mutation names: {repeated}", file=sys.stderr)
        return 2
    selected = set(requested)
    rows = [row for row in mutations if not selected or row[0] in selected]
    data_names = {"workspace-snapshot-oracle-truncated", "generator-guard-bypassed"}
    # Refusals happen before the evidence directory is claimed: a drifted
    # roster is a "never started" (2), which must not leave `out/` behind.
    try:
        registered(mutations, EXPECTED_NAMES[:-len(data_names)])
        if tuple(EXPECTED_NAMES[-len(data_names):]) != (
            "workspace-snapshot-oracle-truncated",
            "generator-guard-bypassed",
        ):
            raise ValueError("static roster must end with the data mutations")
    except ValueError as error:
        print(f"mutation roster drifted: {error}", file=sys.stderr)
        return 2
    unknown = selected - ({row[0] for row in mutations} | data_names)
    if unknown:
        print(f"unknown mutation names: {sorted(unknown)}", file=sys.stderr)
        return 2
    # The family contract, not a local copy: absent/empty/blank targets and the
    # dot/root/src values are refused by the shared rule, and an already
    # existing directory is a "never started" (2) rather than a bare traceback
    # that reads as "it ran and did not come out green" (#1666 fix-r7/r9, #1734).
    if missing_target():
        print("CARGO_TARGET_DIR must be set by the caller", file=sys.stderr)
        return 2
    refuse_unusable_target()
    out = claim_dir(sys.argv[1])
    env = os.environ.copy()
    paths = sorted({row[1] for row in rows} | {ORACLE, GENERATOR})
    # Verdicts and baseline diagnostics are kept apart by an explicit flag, not
    # by a name prefix: a verdict recorded under any name -- including one that
    # happens to start with "baseline-" -- must show up in the roster check
    # (r6 boundary P2).
    results = []
    diagnostics = []

    def record(name, status, reason="", diagnostic=False):
        row = {"name": name, "status": status, "reason": reason}
        (diagnostics if diagnostic else results).append(row)
        print(f"{name}: {status} {reason}", flush=True)

    # #930/#819: prove every selected named test is green before any mutant
    # verdict counts. One baseline per (target, test), not per mutant. The
    # oracle-truncation data mutation has no row in `mutations`, so it needs
    # its own baseline or it was always reported INVALID "baseline failed".
    data_targets = []
    if not selected or "workspace-snapshot-oracle-truncated" in selected:
        data_targets.append(
            ("workspace_snapshot", "scenarios_match_ts_outcomes_and_trees")
        )
    baseline_ok = set()
    for target, test in dict.fromkeys(
        [(row[4], row[5]) for row in rows] + data_targets
    ):
        result = run_test(target, test, env)
        log = result.stdout + result.stderr
        (out / f"baseline-{test.replace('::', '_')}.log").write_text(log)
        if result.returncode == 0 and baseline_passed(log, test):
            baseline_ok.add((target, test))
        else:
            record(
                f"baseline-{test}",
                "INVALID",
                "unmutated target did not pass",
                diagnostic=True,
            )

    with mutating_sources(ROOT, paths) as snapshots:
        for name, path, before, after, target, test, marker in rows:
            if (target, test) not in baseline_ok:
                record(name, "INVALID", "baseline failed")
                continue
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
                record(name, *classify_bound_run(result.returncode, log, test, marker))
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
            if ("workspace_snapshot", "scenarios_match_ts_outcomes_and_trees") not in baseline_ok:
                record(name, "INVALID", "baseline failed")
                return
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
                record(name, *classify_bound_run(
                    result.returncode, log,
                    "scenarios_match_ts_outcomes_and_trees", marker,
                    survived_reason=f"oracle shrank from {expected_scenarios} to 1",
                    killed_reason="exact corpus pin failed",
                    unbound_reason="failed outside the corpus pin"))
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
                version = error = None
                if count == 1:
                    version, error = node_version(env)
                if count != 1:
                    record(
                        name,
                        "INVALID",
                        f"stale anchor: expected 1 occurrence, found {count}",
                    )
                elif error:
                    record(name, "INVALID", f"node prerequisite failed: {error}")
                elif not version.startswith("v22."):
                    # generate.mts:26 itself refuses every other Node major. A
                    # host outside that contract cannot reach the ownership
                    # guard, so the control is INVALID, never SKIPPED
                    # (#818 fix-r3, fail-closed).
                    record(
                        name,
                        "INVALID",
                        f"host node {version} does not meet the Node 22 "
                        f"generator guard (generate.mts:26); refusing to skip",
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

    # A lost row must not read as a clean run: every selected name has to carry
    # exactly one verdict, and the check runs *before* anything that reads as
    # success is published -- a shrunken run used to leave an all-green
    # summary.json and a green count line behind, so a consumer that only reads
    # the artifact never saw the mismatch (r5 concurrency P1). Every verdict
    # counts regardless of its name, so a row recorded under an unregistered
    # name is a mismatch too (r6 boundary P2), and a mismatch publishes no
    # summary.json at all -- only a file whose name says what happened (r6
    # concurrency/boundary P1).
    expected = [n for n in EXPECTED_NAMES if not selected or n in selected]
    recorded = [row["name"] for row in results]
    if recorded != expected:
        print(
            f"verdict roster mismatch: recorded={recorded} expected={expected}",
            file=sys.stderr,
        )
        (out / "roster-mismatch.json").write_text(
            json.dumps(
                {
                    "verdict": "ROSTER_MISMATCH",
                    "expected": expected,
                    "recorded": recorded,
                    "rows": results,
                    "baseline": diagnostics,
                },
                ensure_ascii=True,
                indent=2,
            )
            + "\n",
            encoding="utf-8",
        )
        return 1
    (out / "summary.json").write_text(
        json.dumps(results, ensure_ascii=True, indent=2) + "\n", encoding="utf-8"
    )
    killed = sum(row["status"] == "KILLED" for row in results)
    print(
        f"{killed}/{len(results)} KILLED; see {out / 'summary.json'}",
        flush=True,
    )
    # Every row must KILLED and at least one row must exist: an empty run or a
    # zero-kill run is never success (#818 fix-r3 P0).
    return verdict_exit(results, key="status")


if __name__ == "__main__":
    raise SystemExit(main())
