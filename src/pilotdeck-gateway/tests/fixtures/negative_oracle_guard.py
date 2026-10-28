#!/usr/bin/env python3
# anchor-selfcheck: unsupported JSON provenance corruption, no source before fragments
"""Tamper one recorded source hash and require the real replay to reject it.

Run only in an exclusively owned worktree, with PDRS_BASELINE set. Each case
first requires a green replay, then mutates its fixture temporarily. Original
bytes are restored in finally, including on compilation failure or timeout.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess

from negative_process import mutating_sources, run_owned, wait_for_slot


CASES = [
    ("network-timing", ["--test", "network_timing"], "fetch_redirect_headers_fulfilled_continuation_first"),
    ("session-domain", ["--lib"], "storage::session_domain::tests::real_ts_domain_logs_and_events_match_bytes"),
    ("transcript-adapter", ["--lib"], "storage::transcript_adapter_tests::real_ts_scenarios_match_outcomes_calls_and_bytes"),
    ("run-registry", ["--lib"], "storage::run_registry_tests::real_ts_scenarios_match_outcomes_and_snapshot_bytes"),
    ("host-tool-context", ["--test", "host_tool_context"], "replay_real_ts_constructors"),
    ("recorder", ["--test", "recorder"], "real_ts_scenarios_match_outcomes_and_bytes"),
    ("host-budget", ["--test", "host_budget"], "budget_dispatch_matches_real_ts_calls_responses_and_capabilities"),
    ("host-event", ["--lib"], "host::tests::event::oracle_comes_from_the_fixed_baseline"),
    ("snapshot-projection", ["--lib"], "node_io::tests::method_whitelists_equal_source_oracle_errno_sets"),
    ("provider-timing", ["--lib"], "runtime::model::http::timing_tests::provider_ts_timing_matrix"),
]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path)
    parser.add_argument("--fixture", choices=[case[0] for case in CASES])
    args = parser.parse_args()
    if not os.environ.get("PDRS_BASELINE"):
        parser.error("set PDRS_BASELINE to verify hashes against the fixed TS commit")
    fixtures = Path(__file__).resolve().parent
    workspace = fixtures.parents[2]
    output = args.output.resolve()
    if output == fixtures or fixtures in output.parents:
        parser.error("evidence output must be outside tests/fixtures")
    output.mkdir(parents=True, exist_ok=True)
    slots = workspace.parent / "scripts/cargo-slots.sh"
    summary = []
    for name, target, test in CASES:
        if args.fixture and args.fixture != name:
            continue
        path = fixtures / name / "oracle.json"
        with mutating_sources(fixtures.parents[3], [path]) as snapshots:
            original = snapshots[path]
            source, recorded = next(iter(json.loads(original)["sourceSha256"].items()))
            assert len(recorded) == 64 and original.count(recorded.encode()) == 1
            forged = "0" * 64 if recorded != "0" * 64 else "1" * 64
            command = ["cargo", "test", "--offline", "-p", "pilotdeck-gateway", *target,
                       test, "--", "--exact", "--nocapture", "--test-threads=4"]
            try:
                for phase in ("baseline", "tampered"):
                    if phase == "tampered":
                        assert path.read_bytes() == original, "fixture changed during baseline test"
                        path.write_bytes(original.replace(recorded.encode(), forged.encode(), 1))
                    gate = wait_for_slot(slots)
                    result = run_owned(command, cwd=workspace, timeout=600)
                    raw = result.stdout + result.stderr
                    (output / f"{name}-{phase}.log").write_text(
                        gate.stdout + gate.stderr + raw + f"\nexit={result.returncode}\n")
                    if phase == "baseline":
                        valid = result.returncode == 0 and "1 passed" in raw
                    else:
                        valid = (result.returncode == 101 and "1 failed" in raw
                                 and f"oracle hash for {source} does not match" in raw
                                 and "could not compile" not in raw)
                    summary.append({"fixture": name, "phase": phase, "exit": result.returncode,
                                    "source": source, "valid": valid,
                                    "originalSha256": hashlib.sha256(original).hexdigest()})
                    (output / "summary.json").write_text(json.dumps(summary, indent=2) + "\n")
                    print(name, phase, "PASS" if valid else "INVALID", flush=True)
                    if not valid:
                        raise SystemExit(1)
            finally:
                path.write_bytes(original)
                assert path.read_bytes() == original, "failed to restore fixture bytes"


if __name__ == "__main__":
    main()
