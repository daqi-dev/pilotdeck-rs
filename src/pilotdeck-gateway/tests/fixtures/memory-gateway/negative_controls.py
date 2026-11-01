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
from negative_selection import baseline_passed
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
        "#1214 list-project-key-validation step 1 (memory_list): wire error must match the TS fixture",
        "gateway_replays_real_ts_wrapper_oracle",
    ),
    (
        "session-key-validation-relaxed",
        SOURCE / "runtime/memory/gateway.rs",
        "        JsValue::String(text) if !js_trim_units(text.units()).is_empty() => Ok(Some(text.clone())),\n",
        "        JsValue::String(text) => Ok(Some(text.clone())),\n",
        "#1214 list-session-key-validation step 0 (memory_list): wire error must match the TS fixture",
        "gateway_replays_real_ts_wrapper_oracle",
    ),
    (
        "scope-discrimination-wrong",
        SOURCE / "runtime/memory/gateway.rs",
        '        .is_some_and(|scope| scope.equals_str("session"))',
        '        .is_some_and(|scope| scope.equals_str("project"))',
        "#1214 wipe-scope-discrimination step 2 (memory_wipe): seam calls must match the TS fixture",
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
        "#1214 wipe-scope-discrimination step 2 (memory_wipe): seam calls must match the TS fixture",
        "gateway_replays_real_ts_wrapper_oracle",
    ),
    (
        "params-typeerror-dropped",
        SOURCE / "runtime/memory/gateway.rs",
        "    if let JsValue::Undefined | JsValue::Null = params {",
        "    if let JsValue::Null = params {",
        "#1214 list-project-key-validation step 7 (memory_list): wire error must match the TS fixture",
        "gateway_replays_real_ts_wrapper_oracle",
    ),
    (
        "wire-field-wipe-renamed",
        SOURCE / "runtime/memory/mod.rs",
        '        object.insert("wiped".into(), JsValue::Bool(self.wiped));',
        '        object.insert("cleared".into(), JsValue::Bool(self.wiped));',
        "#1214 wipe-scope-discrimination step 0 (memory_wipe): wire result must match the TS fixture",
        "gateway_replays_real_ts_wrapper_oracle",
    ),
    (
        "wire-field-list-renamed",
        SOURCE / "runtime/memory/mod.rs",
        '        object.insert("items".into(), JsValue::Array(self.items.clone()));',
        '        object.insert("entries".into(), JsValue::Array(self.items.clone()));',
        "#1214 list-session-key-validation step 7 (memory_list): wire result must match the TS fixture",
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
        "#1214 whitespace-trim-boundary step 1 (memory_list): wire error must match the TS fixture",
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
    if not os.environ.get("CARGO_TARGET_DIR"):
        print("CARGO_TARGET_DIR must be set by the caller", file=sys.stderr)
        return 2
    out = Path(sys.argv[1]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    selected = set(sys.argv[2:])
    data_names = {"memory-gateway-oracle-truncated"}
    unknown = selected - ({row[0] for row in mutations} | data_names)
    if unknown:
        print(f"unknown mutation names: {sorted(unknown)}", file=sys.stderr)
        return 2
    rows = [row for row in mutations if not selected or row[0] in selected]
    paths = sorted({row[1] for row in rows} | {ORACLE})
    results = []

    def record(name, status, reason=""):
        results.append({"name": name, "status": status, "reason": reason})
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
            record(f"baseline-{test}", "INVALID", "unmutated bound test did not pass")

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
