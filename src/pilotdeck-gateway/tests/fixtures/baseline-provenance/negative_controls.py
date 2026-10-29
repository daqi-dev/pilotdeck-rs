#!/usr/bin/env python3
# anchor-selfcheck: unsupported mixed compile/positive/custom-assertion runner for one process-lifecycle helper; no declarative whole-file table
"""Negative controls for the shared generator deadline helper (#776 fix-r4).

One mutant replaces the process-group signal with a leader-only signal. The
helper still returns its deadline error (the drain is bounded), but the
descendant that inherited stdout/stderr stays alive, so the named regression
test must fail at its ``#776 process group ... survived`` assertion rather than
hang or die at an unrelated unwrap.

Every selected row runs a positive control, a compile-only check of the mutant,
the bound negative run, and a restored positive control. A row that times out,
fails to compile, or fails at any other line is INVALID. All rows run before
the summary; any non-KILLED row makes the exit code non-zero.

Usage:
    CARGO_TARGET_DIR=... python3 negative_controls.py NEW_OUTPUT_DIR [row ...]
"""
import difflib
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

root = Path(__file__).resolve().parents[5]
out = Path(sys.argv[1]).resolve()
out.mkdir(parents=True, exist_ok=False)
source = root / "src/pilotdeck-gateway/tests/common/baseline.rs"
test = "generator_deadline_kills_descendants_that_hold_the_pipes"
base = [
    "cargo", "test", "--offline", "--manifest-path", "src/Cargo.toml",
    "-p", "pilotdeck-gateway", "--test", "baseline_provenance",
]
test_command = base + [test, "--", "--exact", "--nocapture"]

# Each row is (name, anchor, replacement). The anchor must occur exactly once.
ROWS = [
    (
        "leader-only-kill",
        "rustix::process::kill_process_group(group, rustix::process::Signal::KILL);",
        "rustix::process::kill_process(group, rustix::process::Signal::KILL);",
    ),
]

selected = sys.argv[2:]
rows = [row for row in ROWS if not selected or row[0] in selected]
if selected and len(rows) != len(selected):
    raise SystemExit("unknown row name")
if not rows:
    raise SystemExit("no rows selected")

env = os.environ.copy()
if not env.get("CARGO_TARGET_DIR"):
    raise SystemExit("CARGO_TARGET_DIR must be supplied by the caller")
baseline = env.get("PDRS_NEGATIVE_BASELINE", "HEAD")


def run(label, command, *, timeout=300):
    slot = wait_for_slot(root / "scripts/cargo-slots.sh", timeout=900)
    (out / f"{label}.slots.log").write_text(slot.stdout + slot.stderr)
    result = run_owned(command, cwd=root, env=env, timeout=timeout)
    (out / f"{label}.stdout").write_text(result.stdout)
    (out / f"{label}.stderr").write_text(result.stderr)
    return result


def passed(result):
    log = result.stdout + result.stderr
    return (result.returncode == 0
            and f"test {test} ... ok" in log
            and "test result: ok. 1 passed;" in log)


def classify(result):
    log = result.stdout + result.stderr
    if passed(result):
        return "SURVIVED", "bound test still passed"
    if (result.returncode == 101
            and f"test {test} ... FAILED" in log
            and "panicked at" in log
            and "#776 process group" in log
            and "survived the generator deadline" in log):
        return "KILLED", "bound test failed at its #776 residual-process assertion"
    return "INVALID", "missing bound runtime failure (compile error, hang, or wrong assertion)"


summary = []
with mutating_sources(root, [source], baseline=baseline) as originals:
    original_bytes = originals[source]
    for name, anchor, replacement in rows:
        status, reason = "INVALID", "not run"
        try:
            original = original_bytes.decode()
            if original.count(anchor) != 1:
                raise RuntimeError(f"stale anchor: {original.count(anchor)} matches")
            if not passed(run(f"{name}-positive", test_command)):
                raise RuntimeError("positive control did not pass the bound test")
            changed = original.replace(anchor, replacement, 1)
            (out / f"{name}.patch").write_text("".join(difflib.unified_diff(
                original.splitlines(True), changed.splitlines(True),
                fromfile=str(source.relative_to(root)),
                tofile=str(source.relative_to(root)))))
            source.write_text(changed)
            if run(f"{name}-compile", base + ["--no-run"]).returncode != 0:
                raise RuntimeError("mutant did not compile; never a kill")
            status, reason = classify(run(f"{name}-negative", test_command))
        except Exception as error:
            status, reason = "INVALID", f"{type(error).__name__}: {error}"
        finally:
            source.write_bytes(original_bytes)
            if source.read_bytes() != original_bytes:
                raise RuntimeError("source restoration failed")
        try:
            if not passed(run(f"{name}-restored", test_command)):
                status = "INVALID"
                reason += "; restored bound test did not pass"
        except Exception as error:
            status = "INVALID"
            reason += f"; restored: {type(error).__name__}: {error}"
        summary.append({"name": name, "test": test, "status": status, "reason": reason})
        (out / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
        print(f"{name}: {status}: {reason}", flush=True)

counts = {state: sum(row["status"] == state for row in summary)
          for state in ("KILLED", "SURVIVED", "INVALID")}
(out / "summary.json").write_text(json.dumps(
    {"controls": summary, "counts": counts}, indent=2) + "\n")
print("summary: " + ", ".join(f"{state}={count}" for state, count in counts.items()),
      flush=True)
raise SystemExit(int(any(row["status"] != "KILLED" for row in summary)))
