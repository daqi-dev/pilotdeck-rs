#!/usr/bin/env python3
"""Mutate private /tmp copies of the production session module, never the tree.

Only existing workspace dependencies are used. Every mutant must compile and
fail the real TS oracle test; a build error or timeout is not a caught mutant.
"""
import argparse
import json
import os
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
         "\n            || self.admission.active_turn_run_id(session_key).as_ref() == run_id", ""),
    ]
    env = dict(os.environ, CARGO_NET_OFFLINE="true",
               CARGO_HOME="/Users/da/ws/pilotdeck-rs/_build/cargo-home",
               CARGO_TARGET_DIR=str(rust / "target"))
    summary = []
    with tempfile.TemporaryDirectory(prefix="pdrs-a71-negative-", dir="/tmp") as directory:
        scratch = Path(directory)
        (scratch / "src/runtime/session").mkdir(parents=True)
        (scratch / "tests/fixtures/session-admission").mkdir(parents=True)
        (scratch / "tests/fixtures/session-admission/oracle.json").write_bytes((fixture / "oracle.json").read_bytes())
        (scratch / "src/lib.rs").write_text('#[path = "runtime/session/mod.rs"] pub mod session;\n')
        # Same versions/features already present in pilotdeck-gateway. This
        # temporary harness introduces no repository manifests or dependencies.
        (scratch / "Cargo.toml").write_text(
            '[package]\nname="pdrs-a71-negative"\nversion="0.0.0"\nedition="2024"\n'
            '[dependencies]\n'
            f'pilotdeck-protocol={{path={json.dumps(str(rust / "pilotdeck-protocol"))}}}\n'
            'serde_json={version="=1.0.151",features=["float_roundtrip"]}\n'
            'tokio={version="=1.48.0",features=["macros","rt-multi-thread","net","signal",'
            '"time","io-util","sync","process","test-util"]}\n')
        cases = [("baseline", None, None, None)] + mutants
        for name, filename, before, after in cases:
            for file, content in originals.items():
                if file == filename:
                    count = content.count(before)
                    assert count == (2 if name in ("run-id-is-identity", "discard-is-completion") else 1), (name, count)
                    content = content.replace(before, after)
                (scratch / "src/runtime/session" / file).write_text(content)
            gate = subprocess.run(
                ["bash", "-c", "until bash /Users/da/ws/pilotdeck-rs/scripts/cargo-slots.sh; do sleep 30; done"],
                capture_output=True, text=True, check=True)
            command = ["cargo", "test", "--offline", "--manifest-path", str(scratch / "Cargo.toml"),
                       "--lib", "session::tests::real_ts_admission_completion_and_gateway_abort", "--", "--nocapture"]
            result = subprocess.run(command, env=env, capture_output=True, text=True, timeout=120)
            raw = result.stdout + result.stderr
            (output / f"negative-{name}.log").write_text(gate.stdout + gate.stderr + raw + f"\nexit={result.returncode}\n")
            passed = result.returncode == 0 if name == "baseline" else (
                result.returncode == 101 and "test result: FAILED" in raw and "could not compile" not in raw)
            summary.append({"name": name, "file": filename, "before": before, "after": after,
                            "exit": result.returncode, "caught": passed if name != "baseline" else None,
                            "baseline_passed": passed if name == "baseline" else None})
            (output / "negative-summary.json").write_text(json.dumps(summary, ensure_ascii=False, indent=2) + "\n")
            print(name, "PASS" if passed else "INVALID", f"exit={result.returncode}", flush=True)
            if not passed:
                raise SystemExit(1)
    print("all mutants compiled and were rejected by the real TS oracle; scratch removed")


if __name__ == "__main__":
    main()
