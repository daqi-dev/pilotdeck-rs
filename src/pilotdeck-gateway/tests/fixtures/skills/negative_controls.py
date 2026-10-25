#!/usr/bin/env python3
"""#241/#251: mutate fixture inputs/expected fields, never product sources."""
import argparse
from contextlib import contextmanager
import copy
import hashlib
import json
import os
from pathlib import Path
import signal
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import run_owned, tree_lock, wait_for_slot


@contextmanager
def restore_on_termination():
    def terminate(signum, _frame):
        raise SystemExit(128 + signum)

    previous = signal.signal(signal.SIGTERM, terminate)
    try:
        yield
    finally:
        signal.signal(signal.SIGTERM, previous)


@contextmanager
def fixture_variant(path, original, changed):
    # Caller owns the shared tree lock; do not snapshot or mutate product code.
    assert path.read_bytes() == original, f"unexpected fixture edit: {path}"
    try:
        path.write_bytes(changed)
        yield
    finally:
        path.write_bytes(original)
        assert path.read_bytes() == original, f"fixture restore failed: {path}"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    fixture = Path(__file__).resolve().parent
    root = fixture.parents[4]
    out = args.out.resolve()
    assert out.is_relative_to(root / "_tmp"), "evidence must stay in this tree's _tmp"
    out.mkdir(parents=True, exist_ok=False)
    env = os.environ.copy()
    env["CARGO_TARGET_DIR"] = str(root / "_build/cargo-target")
    env["LANG"] = "en_US.UTF-8"
    env.pop("LC_ALL", None)
    env.pop("LC_MESSAGES", None)
    baseline = Path(env["PDRS_BASELINE"])
    test = "skills::tests::real_ts_scenarios_match_outcomes_and_file_trees"
    cargo = ["cargo", "test", "--offline", "--manifest-path", str(root / "src/Cargo.toml"),
             "-p", "pilotdeck-gateway", "--lib", test, "--", "--exact", "--nocapture", "--test-threads=1"]

    def execute(label, command, expected_failure=None):
        if command[0] == "cargo":
            slot = wait_for_slot(root / "scripts/cargo-slots.sh", timeout=900)
            (out / f"{label}.slots.log").write_text(slot.stdout + slot.stderr)
        result = run_owned(command, cwd=root, env=env, timeout=900)
        output = result.stdout + result.stderr
        with (out / f"{label}.log").open("x") as log:
            log.write(output)
            log.write(f"\nexit={result.returncode}\n")
        if expected_failure is None:
            assert result.returncode == 0, f"{label} failed; see log"
            assert "NOT_RUN" not in output, f"{label}: permission control not exercised; see reason in log"
        else:
            assert result.returncode != 0 and expected_failure in output, f"{label}: did not reach expected failure"
            if command[0] == "cargo":
                assert "test result: FAILED" in output and f"\n    {test}\n" in output, f"{label}: not a replay assertion failure"
            else:
                assert "AssertionError" in output, f"{label}: not a generator assertion failure"
        print(f"{label}: {'DETECTED' if expected_failure else 'PASS'}", flush=True)

    oracle_path = fixture / "oracle.json"
    generator = fixture / "generate.mts"
    with restore_on_termination(), tree_lock(root):
        original = oracle_path.read_bytes()
        generator_bytes = generator.read_bytes()
        # SIGKILL cannot run finally. Keep exclusive originals for recovery;
        # ordinary completion, assertion failures and SIGTERM restore in place.
        for name, data in [("oracle.original.json", original), ("generate.original.mts", generator_bytes)]:
            with (out / name).open("xb") as saved:
                saved.write(data)
            with (out / f"{name}.sha256").open("x") as digest:
                digest.write(hashlib.sha256(data).hexdigest() + "\n")
        oracle = json.loads(original)
        execute("baseline", cargo)
        assert "native readdir EACCES" in (out / "baseline.log").read_text()
        changes = []
        for name in ["import-unknown-type", "import-symlink-to-subdirectory"]:
            for field in ["name", "code", "message"]:
                mutant = copy.deepcopy(oracle)
                scenario = next(s for s in mutant["scenarios"] if s["name"] == name)
                scenario["steps"][0]["outcome"]["error"][field] += "-wrong"
                changes.append((f"{name}-{field}", mutant, f"{name} step 0"))
        for field in ["fileCount", "totalSize"]:
            mutant = copy.deepcopy(oracle)
            scenario = next(s for s in mutant["scenarios"] if s["name"] == "scan-unreadable-child")
            scenario["steps"][1]["outcome"]["result"]["folders"][0][field] = 1
            changes.append((f"denied-{field}", mutant, "scan-unreadable-child step 1"))
        for name in ["import-unknown-type", "import-symlink-to-subdirectory", "scan-unreadable-child"]:
            mutant = copy.deepcopy(oracle)
            mutant["scenarios"] = [s for s in mutant["scenarios"] if s["name"] != name]
            changes.append((f"missing-{name}", mutant, f"required scenario {name}"))
            mutant = copy.deepcopy(oracle)
            scenario = next(s for s in mutant["scenarios"] if s["name"] == name)
            scenario["steps"].pop()
            changes.append((f"missing-step-{name}", mutant, "all error and recovery steps required"))
        mutant = copy.deepcopy(oracle)
        scenario = next(s for s in mutant["scenarios"] if s["name"] == "import-unknown-type")
        scenario["steps"][0]["unknownType"] += "-not-reached"
        changes.append(("unknown-hook-not-reached", mutant, "unknown-type hook was not reached"))
        mutant = copy.deepcopy(oracle)
        scenario = next(s for s in mutant["scenarios"] if s["name"] == "scan-unreadable-child")
        del scenario["steps"][1]["denyReadDir"]
        changes.append(("missing-denial", mutant, "scan-unreadable-child step 1"))
        for label, mutant, failure in changes:
            data = (json.dumps(mutant, ensure_ascii=True, indent=2) + "\n").encode()
            with fixture_variant(oracle_path, original, data):
                execute(label, cargo, failure)
        execute("restored", cargo)

        mutations = [
            ("ts-unknown-hook", "unknownType: 'src/unknown/odd'", "unknownType: 'src/unknown/absent'", "unknown-type lstat hook was not reached"),
            ("ts-symlink-topology", "target: '../../missing/../src/real'", "target: '../../src/real'", "import-symlink-to-subdirectory"),
            ("ts-denial", "denyReadDir: 'bundle/blocked'", "", "Expected values to be strictly deep-equal"),
        ]
        for label, old, new, failure in mutations:
            assert generator_bytes.count(old.encode()) == 1, label
            with fixture_variant(generator, generator_bytes, generator_bytes.replace(old.encode(), new.encode())):
                execute(label, ["node", "--import", str(baseline / "node_modules/tsx/dist/loader.mjs"),
                                str(generator), str(out / label)], failure)
        execute("ts-restored", ["node", "--import", str(baseline / "node_modules/tsx/dist/loader.mjs"),
                                str(generator), str(out / "ts-restored")])
        assert (out / "ts-restored/oracle.json").read_bytes() == original, "restored generation changed oracle"
        assert oracle_path.read_bytes() == original and generator.read_bytes() == generator_bytes
    print(f"PASS: {len(changes)} Rust replay controls + {len(mutations)} TS generator controls; restored byte-for-byte")


if __name__ == "__main__":
    main()
