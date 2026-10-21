#!/usr/bin/env python3
"""Mutate private /tmp copies of the production session module, never the tree.

Only existing workspace dependencies are used. Every mutant must compile and
fail the real TS oracle or deterministic race test; a build error or timeout is not a caught mutant.
"""
import argparse
import json
import os
import re
from pathlib import Path
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("output", type=Path)
    args = parser.parse_args()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    fixture = Path(__file__).resolve().parent
    rust = fixture.parents[3]
    source = rust / "pilotdeck-gateway/src/runtime/session"
    originals = {path.name: path.read_text() for path in source.glob("*.rs")}
    originals["oracle.json"] = (fixture / "oracle.json").read_text()
    complete_guard = """if state
            .get(session_key)
            .is_some_and(|current| Arc::ptr_eq(&current.0, &handle.0))"""
    mutants = [
        ("skip-busy", "admission.rs", "            || state.turns.contains_key(session_key)\n", ""),
        ("global-key", "admission.rs",
         "pub fn begin_turn(&self, session_key: &JsString, run_id: &JsString) -> bool {",
         "pub fn begin_turn(&self, session_key: &JsString, run_id: &JsString) -> bool {\n"
         "        let session_key = &JsString::from(\"global\");"),
        ("ignore-end-run", "admission.rs",
         "if run_id.is_none_or(|id| id.is_empty() || state.turns.get(session_key) == Some(id))", "if true"),
        ("skip-shutdown", "admission.rs", "if state.shutdown\n            || state.turns.contains_key(session_key)",
         "if state.turns.contains_key(session_key)"),
        ("skip-maintenance", "admission.rs", "\n            || state.maintenance.contains(session_key)", ""),
        ("maintenance-ignores-active-turn", "admission.rs",
         "if state.shutdown || state.turns.contains_key(session_key)", "if state.shutdown"),
        ("run-id-is-identity", "completion.rs", "Arc::ptr_eq(&current.0, &handle.0)",
         "current.run_id() == handle.run_id()"),
        ("old-complete-deletes-new", "completion.rs", complete_guard, "if true"),
        ("ignore-cancel-run", "completion.rs", "if run_id.is_some_and(|id| handle.run_id() != id)", "if false"),
        ("empty-cancel-is-absent", "completion.rs", "if run_id.is_some_and(|id| handle.run_id() != id)",
         "if run_id.is_some_and(|id| !id.is_empty() && handle.run_id() != id)"),
        ("overwrite-first-reason", "completion.rs", "if progress.reason.is_some() {", "if false {"),
        ("lose-completion", "completion.rs", "progress.done = true;", "progress.done = false;"),
        ("discard-is-completion", "completion.rs", "            let _handle = handle;", "            drop(handle);"),
        ("skip-drain", "mod.rs", "self.completion.wait_for_completion(&session_key).await;", ""),
        ("abort-unconditionally", "mod.rs", "let allowed = run_id.is_none()", "let allowed = true || run_id.is_none()"),
        ("drop-admitted-fallback", "mod.rs",
         " || active.as_ref() == run_id", ""),
    ]
    # Restore the original two-read, session-scoped abort prefix in /tmp.
    # The None argument adapts the old reason-only sink to the new signature;
    # it intentionally carries no run fence, just as the original did.
    fixed_prefix = originals["mod.rs"].split("        // One admission snapshot", 1)[1].split(
        "        let session_key = session_key.clone();", 1)[0]
    old_prefix = """        if let Some(active) = self.admission.active_turn_run_id(session_key)
            && !active.is_empty()
            && run_id.is_none_or(|id| id.is_empty() || id == &active)
        {
            self.aborted_reasons
                .lock()
                .unwrap()
                .insert(turn_key(session_key, &active), reason.clone());
        }
        let cancelled = self.completion.cancel(session_key, Some(&reason), run_id);
        let allowed = run_id.is_none()
            || cancelled
            || self.admission.active_turn_run_id(session_key).as_ref() == run_id;
        let operation = allowed.then(|| abort(None, reason));
"""
    mutants.append(("restore-original-abort", "mod.rs",
                    "        // One admission snapshot" + fixed_prefix, old_prefix))
    recorded_hash = json.loads(originals["oracle.json"])["sourceSha256"]["src/gateway/SessionRouter.ts"]
    mutants.append(("tampered-source-hash", "oracle.json", recorded_hash, "0" * 64))
    if not os.environ.get("CARGO_HOME"):
        parser.error("CARGO_HOME must be set to the existing offline cargo cache")
    if not os.environ.get("PDRS_BASELINE"):
        parser.error("PDRS_BASELINE must be set to verify the tampered source hash against the fixed baseline")
    env = dict(os.environ, CARGO_NET_OFFLINE="true",
               CARGO_HOME=os.environ["CARGO_HOME"],
               CARGO_TARGET_DIR=str(rust / "target"))
    summary = []
    with tempfile.TemporaryDirectory(prefix="pdrs-a71-negative-", dir="/tmp") as directory:
        scratch = Path(directory)
        (scratch / "src/runtime/session").mkdir(parents=True)
        (scratch / "tests/fixtures/session-admission").mkdir(parents=True)
        (scratch / "src/lib.rs").write_text(
            '#[path = "runtime/session/mod.rs"] pub mod session;\n'
            f'#[cfg(test)]\n#[path = {json.dumps(str(rust / "pilotdeck-gateway/tests/common/baseline.rs"))}] '
            'mod test_baseline;\n')
        # Same versions/features already present in pilotdeck-gateway. This
        # temporary harness introduces no repository manifests or dependencies.
        (scratch / "Cargo.toml").write_text(
            '[package]\nname="pdrs-a71-negative"\nversion="0.0.0"\nedition="2024"\n'
            '[dependencies]\n'
            f'pilotdeck-protocol={{path={json.dumps(str(rust / "pilotdeck-protocol"))}}}\n'
            'serde_json={version="=1.0.151",features=["float_roundtrip"]}\n'
            'sha2="=0.10.9"\n'
            'uuid={version="=1.27.0",features=["v4"]}\n'
            'tokio={version="=1.48.0",features=["macros","rt-multi-thread","net","signal",'
            '"time","io-util","sync","process","test-util"]}\n')
        cases = [("baseline", None, None, None)] + mutants
        for name, filename, before, after in cases:
            for file, content in originals.items():
                if file == filename:
                    count = content.count(before)
                    assert count == (2 if name in ("run-id-is-identity", "discard-is-completion") else 1), (name, count)
                    content = content.replace(before, after)
                destination = (scratch / "tests/fixtures/session-admission" if file == "oracle.json"
                               else scratch / "src/runtime/session")
                (destination / file).write_text(content)
            gate = subprocess.run(
                ["bash", "-c", "until bash /Users/da/ws/pilotdeck-rs/scripts/cargo-slots.sh; do sleep 30; done"],
                capture_output=True, text=True, check=True)
            test = ("session::tests::old_abort_never_targets_replacement_across_threads"
                    if name == "restore-original-abort" else
                    "session::tests::" if name == "baseline" else
                    "session::tests::real_ts_admission_completion_and_gateway_abort")
            command = ["cargo", "test", "--offline", "--manifest-path", str(scratch / "Cargo.toml"),
                       "--lib", test, "--", "--nocapture"]
            result = subprocess.run(command, env=env, capture_output=True, text=True, timeout=120)
            raw = result.stdout + result.stderr
            (output / f"negative-{name}.log").write_text(gate.stdout + gate.stderr + raw + f"\nexit={result.returncode}\n")
            passed = result.returncode == 0 if name == "baseline" else (
                result.returncode == 101 and "test result: FAILED" in raw and "could not compile" not in raw)
            hits = re.search(r"replacement hits: (\d+)/(\d+)", raw)
            if name == "restore-original-abort":
                passed = passed and hits is not None and int(hits[1]) == int(hits[2]) == 2_000
            if name == "tampered-source-hash":
                passed = passed and "oracle hash for src/gateway/SessionRouter.ts does not match" in raw
            if name == "maintenance-ignores-active-turn":
                passed = passed and '"active-turn-rejects-maintenance" step 1' in raw
            summary.append({"replacement_hits": int(hits[1]) if hits else None,
                            "rounds": int(hits[2]) if hits else None,
                            "name": name, "file": filename, "before": before, "after": after,
                            "exit": result.returncode, "caught": passed if name != "baseline" else None,
                            "baseline_passed": passed if name == "baseline" else None})
            (output / "negative-summary.json").write_text(json.dumps(summary, ensure_ascii=False, indent=2) + "\n")
            print(name, "PASS" if passed else "INVALID", f"exit={result.returncode}", flush=True)
            if not passed:
                raise SystemExit(1)
    print("all mutants compiled and were rejected by oracle/race assertions; scratch removed")


if __name__ == "__main__":
    main()
