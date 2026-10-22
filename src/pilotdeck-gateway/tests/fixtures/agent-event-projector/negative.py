#!/usr/bin/env python3
"""Mutation controls for the agent-event projector fixture.

Every mutant rewrites a copy of `projector.rs` in an isolated /tmp crate and
must make the real oracle replay fail with an assertion naming the affected
case. The baseline copy must pass. No source in the worktree is modified.
"""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

output = Path(sys.argv[1]).resolve()
output.mkdir(parents=True, exist_ok=True)
fixture = Path(__file__).resolve().parent
rust = fixture.parents[3]
originals = {
    "projector.rs": (rust / "pilotdeck-gateway/src/runtime/turn/projector.rs").read_text(),
    "projector_tests.rs": (
        rust / "pilotdeck-gateway/src/runtime/turn/projector_tests.rs"
    ).read_text(),
}
oracle = (fixture / "oracle.json").read_text()
env = dict(os.environ, CARGO_NET_OFFLINE="true")
assert env.get("CARGO_HOME") and env.get("PDRS_BASELINE")

mutants = [
    (
        "drop-timeline-projection",
        "projector.rs",
        'if !attachment && truthy(field(event, "timeline")) {',
        'if false && !attachment && truthy(field(event, "timeline")) {',
        "turn-started-with-timeline-and-boundary",
    ),
    (
        "drop-appended-run-id",
        "projector.rs",
        "if !has_run_id {",
        "if false && !has_run_id {",
        "steer-applied-with-block-timeline",
    ),
    (
        "preview-counts-scalars-not-utf16",
        "projector.rs",
        "units.len() - MAX_GATEWAY_TOOL_RESULT_PREVIEW_CHARS",
        "String::from_utf16_lossy(units).chars().count() - MAX_GATEWAY_TOOL_RESULT_PREVIEW_CHARS",
        "tool-result-preview-truncation-utf16",
    ),
    (
        "attach-read-file-images",
        "projector.rs",
        'if !is_type(tool_name, "read_file") {',
        "if true {",
        "tool-result-read-file-image-has-no-attachment",
    ),
    (
        "skip-tool-data-string-limit",
        "projector.rs",
        "if value.len_utf16() <= MAX_GATEWAY_TOOL_DATA_STRING_CHARS {",
        "if true {",
        "tool-result-data-string-truncation",
    ),
    (
        "ignore-forward-subagent-text",
        "projector.rs",
        "if forward_subagent_text {",
        "if !forward_subagent_text {",
        "subagent-model-event-text-forwarded",
    ),
    (
        "null-total-not-nullish",
        "projector.rs",
        "    } else if nullish(explicit_total) {",
        "    } else if explicit_total.is_undefined() {",
        "context-budget-null-fallbacks",
    ),
    (
        "skip-provider-raw-truncation",
        "projector.rs",
        "if units.len() > 1_200 {",
        "if false {",
        "model-recovery-failed-with-truncated-raw",
    ),
    (
        "timeline-on-attachment",
        "projector.rs",
        'if !attachment && truthy(field(event, "timeline")) {',
        'if truthy(field(event, "timeline")) {',
        "tool-result-large-with-images-and-file",
    ),
]

summary = []
with tempfile.TemporaryDirectory(prefix="pdrs-aep-negative-", dir="/tmp") as temp:
    scratch = Path(temp)
    env["CARGO_TARGET_DIR"] = str(scratch / "target")
    (scratch / "src/runtime/turn").mkdir(parents=True)
    (scratch / "tests/fixtures/agent-event-projector").mkdir(parents=True)
    (scratch / "tests/fixtures/agent-event-projector/oracle.json").write_text(oracle)
    manifest = (
        '[package]\nname="pdrs-aep-negative"\nversion="0.0.0"\nedition="2024"\n'
        "[dependencies]\n"
        f"pilotdeck-protocol={{path={json.dumps(str(rust / 'pilotdeck-protocol'))}}}\n"
        'sha2="=0.10.9"\n'
        'uuid={version="=1.27.0",features=["v4"]}\n'
    )
    (scratch / "Cargo.toml").write_text(manifest)
    base_lib = (
        '#[path="runtime/turn/projector.rs"] pub mod projector;\n'
        f'#[cfg(test)] #[path={json.dumps(str(rust / "pilotdeck-gateway/tests/common/baseline.rs"))}] '
        "mod test_baseline;\n"
    )
    (scratch / "src/lib.rs").write_text(base_lib)

    for name, filename, before, after, marker in [("baseline", None, None, None, None)] + mutants:
        for file, content in originals.items():
            if file == filename:
                assert content.count(before) == 1, (name, file, content.count(before))
                content = content.replace(before, after)
            (scratch / "src/runtime/turn" / file).write_text(content)
        gate = subprocess.run(
            [
                "bash",
                "-c",
                'until bash "$1"; do sleep 30; done',
                "cargo-slots",
                str(fixture.parents[4] / "scripts/cargo-slots.sh"),
            ],
            capture_output=True,
            text=True,
            check=True,
        )
        command = [
            "cargo",
            "test",
            "--offline",
            "--manifest-path",
            str(scratch / "Cargo.toml"),
            "--lib",
            "projector::tests::",
            "--",
            "--nocapture",
        ]
        result = subprocess.run(command, env=env, capture_output=True, text=True, timeout=900)
        raw = result.stdout + result.stderr
        if name == "baseline":
            passed = result.returncode == 0 and "3 passed" in raw
        else:
            passed = (
                result.returncode == 101
                and "assertion" in raw
                and "could not compile" not in raw
                and marker in raw
            )
        (output / f"negative-{name}.log").write_text(
            gate.stdout
            + gate.stderr
            + "COMMAND "
            + " ".join(command)
            + "\n"
            + raw
            + f"\nexit={result.returncode}\n"
        )
        summary.append(
            {
                "name": name,
                "mutation": None if filename is None else {"file": filename, "before": before, "after": after},
                "expected_case": marker,
                "exit": result.returncode,
                "passed": passed,
            }
        )
        (output / "negative-summary.json").write_text(
            json.dumps(summary, ensure_ascii=False, indent=2) + "\n"
        )
        print(name, "PASS" if passed else "INVALID", flush=True)
        if not passed:
            raise SystemExit(1)
print("all controls rejected; temporary crate removed")
