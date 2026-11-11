#!/usr/bin/env python3
"""#1214 negative controls: run every mutant, then summarize.

Each mutant is applied to one source file, compile-checked, run against the
integration test that must turn red, and restored byte-for-byte. The runner
never stops at the first failure: it records KILLED / SURVIVED / INVALID with
a reason for every row and exits non-zero if any row is not KILLED.

#1226 rows bind the exhaustive `js_is_space_unit` / `require_management_project_key`
lib unit tests (`runtime::memory::gateway::tests`), so the bound-test runner and
the compile check cover the `--lib` target next to the integration replay.

The new files this lane adds (`runtime/memory/gateway.rs`, the fixture and
`tests/memory_gateway.rs`) are not in HEAD yet, so ``mutating_sources`` refuses
to open its window until the delivery is committed; the coordinator runs this
table on the host afterwards. Run from the worktree root with a caller-supplied
``CARGO_TARGET_DIR``:

    CARGO_TARGET_DIR=/abs/cargo-target \
    python3 src/pilotdeck-gateway/tests/fixtures/memory-gateway/negative_controls.py out/
"""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_panic import named_test_killed
from negative_selection import (baseline_passed, claim_dir, missing_target,
                                refuse_unusable_target, registered)
from negative_process import (mutating_sources, run_owned, wait_for_slot)  # noqa: E402

ROOT = Path(__file__).resolve().parents[5]
SOURCE = ROOT / "src/pilotdeck-gateway/src"
FIXTURES = ROOT / "src/pilotdeck-gateway/tests/fixtures"
TEST_TARGET = "memory_gateway"
TEST = "gateway_replays_real_ts_wrapper_oracle"
# #1226: full libtest names of the two lib unit tests bound by the rows below.
LIB_TESTS = {
    "runtime::memory::gateway::tests::js_is_space_unit_pins_the_ts_trim_whitespace_set",
    "runtime::memory::gateway::tests::require_management_project_key_only_trims_for_emptiness",
}

# #2133: neg-for-diff.sh finds this table by the literal `dir/file` of every
# changed .rs file. Every row below is written as `SOURCE / "runtime/memory/x.rs"`,
# so the discovery key is already spelled inline and needs no anchor; the
# shared fixtures its import chain reaches do (the loop keys any change under
# `fixtures/**`, and the import alone does not put `fixtures/negative_*.py` in
# this file's text). A row rewritten as `SOURCE / "mod.rs"` makes its key depend
# on the constant and turns fixtures/test_negative_discovery_anchors.py red
# until the anchor is declared here.
DISCOVERY_ANCHORS = (
    'fixtures/negative_panic.py',
    'fixtures/negative_process.py',
    'fixtures/negative_selection.py',
    'fixtures/negative_scope.py',
)

# (name, path, before, after, marker, bound test). The marker is the
# dedicated per-cell message that proves the intended assertion fired, not
# an unrelated unwrap/compile error; the bound test is the full libtest name
# whose own panic block must carry that marker.
mutations = [
    (
        "project-key-validation-relaxed",
        SOURCE / "runtime/memory/gateway.rs",
        "        JsValue::String(text) if !js_trim_units(text.units()).is_empty() => Ok(text.clone()),\n",
        "        JsValue::String(text) => Ok(text.clone()),\n",
        "#1214 list-project-key-validation step 1 (memory_list):",
        "gateway_replays_real_ts_wrapper_oracle",
    ),
    (
        "session-key-validation-relaxed",
        SOURCE / "runtime/memory/gateway.rs",
        "        JsValue::String(text) if !js_trim_units(text.units()).is_empty() => Ok(Some(text.clone())),\n",
        "        JsValue::String(text) => Ok(Some(text.clone())),\n",
        "#1214 list-session-key-validation step 0 (memory_list):",
        "gateway_replays_real_ts_wrapper_oracle",
    ),
    (
        "scope-discrimination-wrong",
        SOURCE / "runtime/memory/gateway.rs",
        '        .is_some_and(|scope| scope.equals_str("session"))',
        '        .is_some_and(|scope| scope.equals_str("project"))',
        "#1214 wipe-scope-discrimination step 2 (memory_wipe):",
        "gateway_replays_real_ts_wrapper_oracle",
    ),
    (
        "wipe-scope-check-dropped",
        SOURCE / "runtime/memory/gateway.rs",
        "        if scope_is_session(params)\n"
        "            && session_key\n"
        "                .as_ref()\n"
        "                .map(|key| js_trim_units(key.units()).is_empty())\n"
        "                .unwrap_or(true)\n"
        "        {\n"
        "            return Err(MemoryProjectionError::gateway(\n"
        "                \"INVALID_MEMORY_SCOPE\",\n"
        "                \"sessionKey is required for a session-scoped wipe.\",\n"
        "            ));\n"
        "        }\n",
        "        let _ = scope_is_session(params);\n",
        "#1214 wipe-scope-discrimination step 2 (memory_wipe):",
        "gateway_replays_real_ts_wrapper_oracle",
    ),
    (
        "params-typeerror-dropped",
        SOURCE / "runtime/memory/gateway.rs",
        "    if let JsValue::Undefined | JsValue::Null = params {",
        "    if let JsValue::Null = params {",
        "#1214 list-project-key-validation step 7 (memory_list):",
        "gateway_replays_real_ts_wrapper_oracle",
    ),
    (
        "wire-field-wipe-renamed",
        SOURCE / "runtime/memory/mod.rs",
        '        object.insert("wiped".into(), JsValue::Bool(self.wiped));',
        '        object.insert("cleared".into(), JsValue::Bool(self.wiped));',
        "#1214 wipe-scope-discrimination step 0 (memory_wipe):",
        "gateway_replays_real_ts_wrapper_oracle",
    ),
    (
        "wire-field-list-renamed",
        SOURCE / "runtime/memory/mod.rs",
        '        object.insert("items".into(), JsValue::Array(self.items.clone()));',
        '        object.insert("entries".into(), JsValue::Array(self.items.clone()));',
        "#1214 list-session-key-validation step 7 (memory_list):",
        "gateway_replays_real_ts_wrapper_oracle",
    ),
    (
        "forward-mapped-to-response",
        SOURCE / "runtime/memory/gateway.rs",
        "            Err(error) if error.requires_host() => MemoryDispatch::Forward,\n",
        "            Err(error) if error.requires_host() => {\n"
        "                MemoryDispatch::Response(error.to_js())\n"
        "            }\n",
        "#1214 memory_list must forward a requires_host boundary to the Node host",
        "dispatch_forwards_requires_host_boundary",
    ),
    (
        "from_projection-disabled",
        SOURCE / "runtime/memory/gateway.rs",
        "    pub fn from_projection(projection: MemoryProjection) -> Self {\n"
        "        Self {\n"
        "            projection: Some(projection),\n"
        "        }\n"
        "    }\n",
        "    pub fn from_projection(_projection: MemoryProjection) -> Self {\n"
        "        Self { projection: None }\n"
        "    }\n",
        "#1214 from_projection must serve memory_list through the wired projection",
        "from_projection_serves_both_methods",
    ),
    (
        "whitespace-unit-dropped",
        SOURCE / "runtime/memory/mod.rs",
        "            | 0xFEFF\n",
        "",
        "#1214 whitespace-trim-boundary step 1 (memory_list):",
        "gateway_replays_real_ts_wrapper_oracle",
    ),
    # #1226: the replay corpus only pinned 6 of the 25 trim units; these rows
    # bind the exhaustive lib unit test so every dropped unit or moved range
    # bound is caught by name.
    (
        "whitespace-range-end-shrunk",
        SOURCE / "runtime/memory/mod.rs",
        "0x2000..=0x200A",
        "0x2000..=0x2009",
        "#1226 js_is_space_unit drifted from the TS trim whitespace set",
        "runtime::memory::gateway::tests::js_is_space_unit_pins_the_ts_trim_whitespace_set",
    ),
    (
        "whitespace-range-start-shrunk",
        SOURCE / "runtime/memory/mod.rs",
        "0x0009..=0x000D",
        "0x000A..=0x000D",
        "#1226 js_is_space_unit drifted from the TS trim whitespace set",
        "runtime::memory::gateway::tests::js_is_space_unit_pins_the_ts_trim_whitespace_set",
    ),
    (
        "whitespace-unit-1680-dropped",
        SOURCE / "runtime/memory/mod.rs",
        "            | 0x1680\n",
        "",
        "#1226 js_is_space_unit drifted from the TS trim whitespace set",
        "runtime::memory::gateway::tests::js_is_space_unit_pins_the_ts_trim_whitespace_set",
    ),
    (
        "whitespace-unit-202f-dropped",
        SOURCE / "runtime/memory/mod.rs",
        "            | 0x202F\n",
        "",
        "#1226 js_is_space_unit drifted from the TS trim whitespace set",
        "runtime::memory::gateway::tests::js_is_space_unit_pins_the_ts_trim_whitespace_set",
    ),
    (
        "whitespace-unit-205f-dropped",
        SOURCE / "runtime/memory/mod.rs",
        "            | 0x205F\n",
        "",
        "#1226 js_is_space_unit drifted from the TS trim whitespace set",
        "runtime::memory::gateway::tests::js_is_space_unit_pins_the_ts_trim_whitespace_set",
    ),
    # #1456: the replay used to flatten both dispatch arms into one JSON value,
    # so answering a thrown wrapper error as a successful body still matched
    # every recorded cell. The replay now pins the recorded step kind; this row
    # proves that pin fires.
    (
        "error-mapped-to-response",
        SOURCE / "runtime/memory/gateway.rs",
        "            Err(error) => MemoryDispatch::Error(error),\n",
        "            Err(error) => MemoryDispatch::Response(error.to_js()),\n",
        "the TS wrapper threw; answering",
        "gateway_replays_real_ts_wrapper_oracle",
    ),
    # #1226: the validator must return the raw key, not the trimmed one
    # (requireManagementProjectKey only trims to check emptiness).
    (
        "project-key-returned-trimmed",
        SOURCE / "runtime/memory/gateway.rs",
        "        JsValue::String(text) if !js_trim_units(text.units()).is_empty() => Ok(text.clone()),\n",
        "        JsValue::String(text) if !js_trim_units(text.units()).is_empty() =>\n"
        "            Ok(JsString::from_utf16(js_trim_units(text.units()).to_vec())),\n",
        "#1226 U+0009-padded projectKey must keep its raw units",
        "runtime::memory::gateway::tests::require_management_project_key_only_trims_for_emptiness",
    ),
]

# #1456 (codex sibling P1): the runner builds its selection set and denominator
# from `mutations` itself, so deleting a row (or a mutation silently losing its
# anchor) shrinks the table while every remaining row still reports KILLED.
# Pin the roster the way the native-management ledger does; `main` refuses to
# run when the live table differs from this list.
EXPECTED_NAMES = (
    "project-key-validation-relaxed",
    "session-key-validation-relaxed",
    "scope-discrimination-wrong",
    "wipe-scope-check-dropped",
    "params-typeerror-dropped",
    "wire-field-wipe-renamed",
    "wire-field-list-renamed",
    "forward-mapped-to-response",
    "from_projection-disabled",
    "whitespace-unit-dropped",
    "whitespace-range-end-shrunk",
    "whitespace-range-start-shrunk",
    "whitespace-unit-1680-dropped",
    "whitespace-unit-202f-dropped",
    "whitespace-unit-205f-dropped",
    "error-mapped-to-response",
    "project-key-returned-trimmed",
    "memory-gateway-oracle-truncated",
)

ORACLE = FIXTURES / "memory-gateway/oracle.json"


def cargo(target, args):
    wait_for_slot(ROOT / "scripts/cargo-slots.sh")
    command = [
        "cargo",
        "test",
        "--manifest-path",
        "src/Cargo.toml",
        "-p",
        "pilotdeck-gateway",
        "--locked",
        "--offline",
    ]
    command += ["--lib"] if target == "lib" else ["--test", target]
    return run_owned(command + args, cwd=ROOT, env=ENV, timeout=1800)


def run_bound_test(test):
    # #1226: two bound tests live in the lib's unit-test binary, not in the
    # integration replay target.
    if test in LIB_TESTS:
        return cargo("lib", [test, "--", "--exact"])
    return cargo(TEST_TARGET, [test, "--", "--exact"])


def compile_check():
    """Compile every target the bound tests run in.

    #1226: the table binds lib unit tests next to the integration replay, and
    the lib's ``#[cfg(test)]`` code only compiles under ``--lib``.
    """
    results = [cargo(target, ["--no-run"]) for target in ("lib", TEST_TARGET)]
    return subprocess.CompletedProcess(
        ["compile-check"],
        max(result.returncode for result in results),
        "".join(result.stdout + result.stderr for result in results),
        "",
    )


ENV = os.environ.copy()


def main():
    if len(sys.argv) < 2:
        print("usage: negative_controls.py <output-dir> [mutation ...]", file=sys.stderr)
        return 2
    # The family contract, not a local copy: absent/empty/blank targets and the
    # dot/root/src values are refused by the shared rule, and an already
    # existing directory is a "never started" (2) rather than a bare traceback
    # that reads as "it ran and did not come out green" (#1666 fix-r7/r9, #1734).
    if missing_target():
        print("CARGO_TARGET_DIR must be set by the caller", file=sys.stderr)
        return 2
    refuse_unusable_target()
    # Refusals happen before the evidence directory is claimed: a drifted
    # roster is a "never started" (2), which must not leave `out/` behind.
    requested = sys.argv[2:]
    # A repeated name is a caller-side typo (`a a` for `a b`), not a subset;
    # folding it into a set turns a mistyped selection into a green run over a
    # smaller scope, the same class of false green as an unknown name (#1734).
    repeated = sorted({n for n in requested if requested.count(n) > 1})
    if repeated:
        print(f"repeated mutation names: {repeated}", file=sys.stderr)
        return 2
    selected = set(requested)
    data_names = {"memory-gateway-oracle-truncated"}
    # #1456: a row deleted from this table must not silently shrink the run.
    # Reuse the shared roster check (`negative_selection.registered`, the same
    # one `memory-maintenance` uses) over the source rows; the data mutation is
    # the last pinned name because it runs outside `mutations`.
    try:
        registered(mutations, EXPECTED_NAMES[:-1])
        if EXPECTED_NAMES[-1] != "memory-gateway-oracle-truncated":
            raise ValueError("static roster must end with the data mutation")
    except ValueError as error:
        print(f"mutation roster drifted: {error}", file=sys.stderr)
        return 2
    unknown = selected - ({row[0] for row in mutations} | data_names)
    if unknown:
        print(f"unknown mutation names: {sorted(unknown)}", file=sys.stderr)
        return 2
    out = claim_dir(sys.argv[1])
    rows = [row for row in mutations if not selected or row[0] in selected]
    paths = sorted({row[1] for row in rows} | {ORACLE})
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

    # #819: every unmutated bound test must be green before any verdict counts.
    bound_tests = {row[5] for row in rows}
    if not selected or "memory-gateway-oracle-truncated" in selected:
        bound_tests.add(TEST)
    baselines = {}
    for test in sorted(bound_tests):
        baseline = run_bound_test(test)
        log = baseline.stdout + baseline.stderr
        (out / f"baseline-{test}.log").write_text(log)
        baselines[test] = baseline.returncode == 0 and baseline_passed(log, test)
        if not baselines[test]:
            record(
                f"baseline-{test}",
                "INVALID",
                "unmutated bound test did not pass",
                diagnostic=True,
            )

    with mutating_sources(ROOT, paths) as snapshots:
        for name, path, before, after, marker, test in rows:
            if not baselines.get(test):
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
                path.write_text(content.replace(before, after, 1), encoding="utf-8")
                compiled = compile_check()
                compile_log = compiled.stdout + compiled.stderr
                (out / f"{name}.compile.log").write_text(compile_log)
                if compiled.returncode != 0:
                    record(name, "INVALID", "mutant does not compile")
                    continue
                result = run_bound_test(test)
                log = result.stdout + result.stderr
                (out / f"{name}.log").write_text(log)
                if result.returncode == 0:
                    record(name, "SURVIVED", "bound test stayed green")
                elif named_test_killed(log, test, marker):
                    record(name, "KILLED", "named assertion failed")
                else:
                    record(name, "INVALID", "test failed outside the named assertion")
            except Exception as error:
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
            # The restored source must keep the bound test green.
            restored = run_bound_test(test)
            log = restored.stdout + restored.stderr
            (out / f"{name}.restored.log").write_text(log)
            if not (restored.returncode == 0 and baseline_passed(log, test)):
                record(name, "INVALID", "restored bound test did not pass")

        if not selected or "memory-gateway-oracle-truncated" in selected:
            # Data mutation: shrinking the committed corpus must trip the exact
            # pin, not silently replay one case. No compile step.
            name = "memory-gateway-oracle-truncated"
            if not baselines.get(TEST):
                record(name, "INVALID", "baseline failed")
            else:
                original = snapshots[ORACLE]
                before_bytes = hashlib.sha256(original).hexdigest()
                try:
                    data = json.loads(original)
                    expected_cases = len(data.get("gatewayCases", []))
                    data["gatewayCases"] = data.get("gatewayCases", [])[:1]
                    # The corpus carries a lone UTF-16 surrogate project key;
                    # escape it so the bytes survive the round trip.
                    ORACLE.write_text(
                        json.dumps(data, ensure_ascii=True, indent=2) + "\n",
                        encoding="utf-8",
                    )
                    result = run_bound_test(TEST)
                    log = result.stdout + result.stderr
                    (out / f"{name}.log").write_text(log)
                    marker = "#1214 the committed gateway case corpus is pinned"
                    if result.returncode == 0:
                        record(
                            name,
                            "SURVIVED",
                            f"oracle shrank from {expected_cases} to 1",
                        )
                    elif named_test_killed(log, TEST, marker):
                        record(name, "KILLED", "exact corpus pin failed")
                    else:
                        record(name, "INVALID", "failed outside the corpus pin")
                except Exception as error:
                    record(
                        name,
                        "INVALID",
                        f"runner error: {type(error).__name__}: {error}",
                    )
                finally:
                    ORACLE.write_bytes(original)
                    if hashlib.sha256(ORACLE.read_bytes()).hexdigest() != before_bytes:
                        record(name, "INVALID", "restore mismatch")
                        raise SystemExit(2)
                restored = run_bound_test(TEST)
                log = restored.stdout + restored.stderr
                (out / f"{name}.restored.log").write_text(log)
                if not (restored.returncode == 0 and baseline_passed(log, TEST)):
                    record(name, "INVALID", "restored bound test did not pass")

    # #1456 (codex sibling P1, second round): the roster literal alone only
    # proves the *source* table is intact. The data mutation runs in its own
    # branch, so deleting that branch shrank the run while the roster still
    # matched. Require exactly one verdict per selected name, and run the check
    # *before* publishing anything that reads as success: a shrunken run used to
    # leave an all-green summary.json and a green count line behind, so a
    # consumer that only reads the artifact never saw the mismatch (r5
    # concurrency P1). A mismatch publishes no summary.json at all -- only a
    # file whose name says what happened (r6 concurrency/boundary P1).
    expected = [n for n in EXPECTED_NAMES if not selected or n in selected]
    recorded = [row["name"] for row in results]
    if recorded != expected:
        message = f"verdict roster mismatch: recorded={recorded} expected={expected}"
        print(message, file=sys.stderr)
        (out / "roster-mismatch.json").write_text(
            json.dumps(
                {
                    "verdict": "ROSTER_MISMATCH",
                    "expected": expected,
                    "recorded": recorded,
                    "rows": results,
                    "baseline": diagnostics,
                },
                ensure_ascii=False,
                indent=2,
            )
            + "\n",
            encoding="utf-8",
        )
        return 1
    (out / "summary.json").write_text(
        json.dumps(results, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )
    counts = {
        state: sum(row["status"] == state for row in results)
        for state in ("KILLED", "SURVIVED", "INVALID")
    }
    print(
        "summary: " + ", ".join(f"{state}={count}" for state, count in counts.items()),
        flush=True,
    )
    if not results or any(row["status"] != "KILLED" for row in results):
        return 1
    killed = sum(row["status"] == "KILLED" for row in results)
    scope = "partial run over a selected subset; " if selected else ""
    print(
        f"{killed}/{len(results)} killed; {scope}every source restored byte-for-byte",
        flush=True,
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
