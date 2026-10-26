#!/usr/bin/env python3
"""#842 lease observer mutations. Run after committing, with no other tree writers.

Usage: CARGO_TARGET_DIR=... python3 negative.py NEW_EVIDENCE_DIRECTORY
Only assertion failures in the named runtime test count as killed mutations.
"""
from pathlib import Path
import os
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

ROOT = Path(__file__).resolve().parents[5]
SOURCE = Path("src/pilotdeck-gateway/src/runtime/turn/host_factory_tests.rs")
PREFIX = "runtime::turn::host_factory::tests::"
LOOP = """        while runtime.lease_count() != 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }"""
MUTATIONS = [
    ("old-instantaneous-assertion", LOOP,
     "        assert_eq!(runtime.lease_count(), 0);",
     "lease_wait_observes_delayed_host_cleanup", "assertion `left == right` failed"),
    ("skip-observation", LOOP, "        let _ = runtime;",
     "lease_wait_observes_delayed_host_cleanup", "lease wait returned before cleanup"),
    ("late-deadline",
     "    timeout(Duration::from_secs(10), async {\n" + LOOP,
     "    timeout(Duration::from_secs(20), async {\n" + LOOP,
     "lease_wait_times_out_while_host_cleanup_is_held",
     "held cleanup must reach the 10-second deadline"),
]


def main():
    if not os.environ.get("CARGO_TARGET_DIR"):
        raise SystemExit("CARGO_TARGET_DIR must be supplied by the caller")
    out = Path(sys.argv[1]).resolve()
    out.mkdir(parents=True, exist_ok=False)

    def run(label, test):
        slots = wait_for_slot(ROOT / "scripts/cargo-slots.sh", timeout=300)
        (out / f"{label}.slots.log").write_text(slots.stdout + slots.stderr)
        result = run_owned(
            ["cargo", "test", "--locked", "-p", "pilotdeck-gateway", "--lib",
             PREFIX + test, "--", "--nocapture"],
            cwd=ROOT / "src", env=os.environ.copy(), timeout=300,
        )
        log = result.stdout + result.stderr
        (out / f"{label}.log").write_text(log)
        return result.returncode, log

    # Prove the committed source, not an arbitrary dirty snapshot, is the base.
    with mutating_sources(ROOT, [SOURCE]):
        code, log = run("baseline", "")
        assert code == 0 and "test result: ok." in log, log

    for name, before, after, test, failure in MUTATIONS:
        with mutating_sources(ROOT, [SOURCE]) as originals:
            source = originals[SOURCE].decode()
            assert source.count(before) == 1, f"{name}: mutation anchor is not unique"
            (ROOT / SOURCE).write_text(source.replace(before, after, 1))
            code, log = run(name, test)
            assert code == 101 and f"{PREFIX}{test} ... FAILED" in log and failure in log, (
                f"{name}: no matching assertion failure; compile/timeouts do not count\n{log}"
            )
            if name == "old-instantaneous-assertion":
                assert "left: 1" in log and "right: 0" in log, log
            print(f"{name}: KILLED by {test}", flush=True)
        code, log = run(name + "-restored", test)
        assert code == 0 and "test result: ok." in log, log
    print("3/3 killed; committed source restored by mutating_sources", flush=True)


if __name__ == "__main__":
    main()
