#!/usr/bin/env python3
"""Mutation negative controls for the model-complete TS oracle.

Each control mutates this worktree only, compiles the integration test, and
requires a real assertion failure ("test result: FAILED") from the mutated
build. Compile errors do not count. The source is restored from the in-memory
original and verified by SHA-256 in a finally block.
"""
from __future__ import annotations

import hashlib
import os
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[5]
HTTP = ROOT / "src/pilotdeck-gateway/src/runtime/model/http.rs"
MANIFEST = ROOT / "src/Cargo.toml"
TARGET = os.environ.get("CARGO_TARGET_DIR", str(ROOT / "target"))
TEST = "native_complete_model_matches_real_ts_complete"
RETRY_TEST = "complete_429_is_retryable_but_is_not_retried"

CONTROLS = [
    (
        "responses-delegates-to-chat-completions",
        '"openai-responses" => parse_openai_responses_complete_response(raw, provider_id),',
        '"openai-responses" => parse_openai_complete_response(raw, provider_id),',
        TEST,
    ),
    (
        "openai-finish-reason-uses-tool_calls",
        'Some("tool_calls" | "function_call") => "tool_call".into(),',
        'Some("tool_calls" | "function_call") => "tool_calls".into(),',
        TEST,
    ),
    (
        "responses-failed-finish-reason-unknown",
        'Some("failed" | "cancelled") => "error".into(),',
        'Some("failed" | "cancelled") => "unknown".into(),',
        TEST,
    ),
    (
        "complete-retries-429",
        """            return Err(ModelStreamError::Provider(error));
        }

        let protocol_value = field(&provider, "protocol").clone();""",
        """            if attempt < max_retries {
                let (_, delay_ms) =
                    retry_decide_request(&provider, attempt, (options.random)());
                let _ = delay(delay_ms, options).await?;
                attempt += 1.0;
                continue;
            }
            return Err(ModelStreamError::Provider(error));
        }

        let protocol_value = field(&provider, "protocol").clone();""",
        RETRY_TEST,
    ),
]


def running_cargo() -> int:
    result = subprocess.run(["pgrep", "-x", "cargo"], capture_output=True, text=True)
    if result.returncode not in (0, 1):
        raise SystemExit(f"pgrep -x cargo failed: {result.returncode}")
    return len([line for line in result.stdout.splitlines() if line.strip()])


def wait_for_slot() -> None:
    # scripts/cargo-slots.sh is the documented gate, but `ps` is denied under
    # this sandbox and the script would report 0. `pgrep -x cargo` is the
    # authoritative fallback; never treat the failed `ps` as zero.
    subprocess.run(
        ["bash", "scripts/cargo-slots.sh"],
        cwd=ROOT,
        capture_output=True,
        text=True,
    )
    while running_cargo() >= 4:
        time.sleep(10)


def run_test(test_name: str) -> tuple[int, str]:
    wait_for_slot()
    env = dict(os.environ, CARGO_TARGET_DIR=TARGET)
    result = subprocess.run(
        [
            "cargo",
            "test",
            "--manifest-path",
            str(MANIFEST),
            "-p",
            "pilotdeck-gateway",
            "--test",
            "model_complete",
            test_name,
            "--",
            "--exact",
        ],
        cwd=ROOT,
        env=env,
        capture_output=True,
        text=True,
        timeout=600,
    )
    return result.returncode, result.stdout + result.stderr


def main() -> int:
    original = HTTP.read_bytes()
    original_sha = hashlib.sha256(original).hexdigest()
    failures: list[str] = []
    try:
        for name, old, new, test_name in CONTROLS:
            text = original.decode("utf-8")
            if text.count(old) != 1:
                failures.append(f"{name}: mutation anchor count {text.count(old)}")
                continue
            mutated = text.replace(old, new, 1)
            if mutated == text:
                failures.append(f"{name}: mutation did not change source")
                continue
            HTTP.write_text(mutated)
            try:
                code, output = run_test(test_name)
                passed = "test result: FAILED" in output and code != 0
                print(f"{name}: {'caught' if passed else 'NOT CAUGHT'}")
                if not passed:
                    tail = "\n".join(output.splitlines()[-25:])
                    print(tail)
                    failures.append(f"{name}: mutated build did not fail the assertion")
            finally:
                HTTP.write_bytes(original)
                if hashlib.sha256(HTTP.read_bytes()).hexdigest() != original_sha:
                    raise SystemExit(f"{name}: restore SHA-256 mismatch")
    finally:
        HTTP.write_bytes(original)
    if hashlib.sha256(HTTP.read_bytes()).hexdigest() != original_sha:
        raise SystemExit("final restore SHA-256 mismatch")
    if failures:
        print("NEGATIVE CONTROLS FAILED:", *failures, sep="\n  ")
        return 1
    print(f"NEGATIVE CONTROLS PASS ({len(CONTROLS)}/{len(CONTROLS)})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
