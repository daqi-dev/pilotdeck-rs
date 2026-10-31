#!/usr/bin/env python3
"""#1145 HEAD-only product controls: [--check] | NEW_OUTPUT_DIR [mutation ...].

The catalog has exactly nine controls. Each selected control runs its named
positive, compiles the mutant, runs the named negative, restores, and reruns the
positive. All verdicts are published only after the source guard has verified
restoration. New/dirty source and test files must first be committed by the
coordinator; this script never creates a second checkout or bypasses HEAD.
"""
import difflib
import json
import os
from pathlib import Path
import subprocess
import sys

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from negative_process import (
    exclusive_write,
    mutating_sources,
    new_output_directory,
    run_owned,
    wait_for_slot,
)
from negative_suite import (
    Case,
    classify,
    invalidate_completed,
    passed,
    publish_summary,
)

root = Path(__file__).resolve().parents[5]
turn = root / "src/pilotdeck-cli/src/sdk/turn.rs"
signals = root / "src/pilotdeck-cli/src/sdk/signals.rs"
output = root / "src/pilotdeck-cli/src/sdk/output.rs"

EXPECTED_TOTAL = 9
BUILD_TIMEOUT = 900
TEST_TIMEOUT = 90

# Declarative whole-file anchors are understood by main's anchor_selfcheck.
# Every old fragment must occur exactly once; mutants are never cumulative.
mutations = [
    (
        "listener-installed-before-control",
        turn,
        '    let mut control = Transport::connect(connection, "pilotdeck-cli", true).await?;',
        '    let _early_signals = Signals::install()?;\n'
        '    let mut control = Transport::connect(connection, "pilotdeck-cli", true).await?;',
        "signal_cases::run_signal_sigint",
        "#1145 run--signal-sigint four surfaces, abort count, listeners and connections match TS",
    ),
    (
        "listeners-not-removed",
        signals,
        "    pub(super) fn remove(&mut self) {\n"
        "        if !self.removed {",
        "    pub(super) fn remove(&mut self) {\n"
        "        if false && !self.removed {",
        "signal_cases::run_complete_cleanup",
        "#1145 run--complete-cleanup four surfaces, abort count, listeners and connections match TS",
    ),
    (
        "abort-does-not-await-reply",
        turn,
        "        let next = next_query_frame(query, &initialized.request_id, &abort_ids, signals);",
        "        if !abort_ids.is_empty() {\n"
        "            return QueryOutcome::Closed(client_closed());\n"
        "        }\n"
        "        let next = next_query_frame(query, &initialized.request_id, &abort_ids, signals);",
        "signal_cases::run_abort_hold_sigint",
        "#1145 run--abort-hold-sigint four surfaces, abort count, listeners and connections match TS",
    ),
    (
        "held-predecessor-exits-early",
        turn,
        "            initialize_turn(query, argv, &session, &prompt, project).await",
        "            match tokio::time::timeout(\n"
        "                std::time::Duration::from_secs(2),\n"
        "                initialize_turn(query, argv, &session, &prompt, project),\n"
        "            ).await {\n"
        "                Ok(result) => result,\n"
        "                Err(_) => Err(Failure::Error(\"mutant predecessor deadline\".into())),\n"
        "            }",
        "signal_cases::run_query_resume_hold_sigint",
        "#1145 run--query-resume-hold-sigint four surfaces, abort count, listeners and connections match TS",
    ),
    (
        "completed-query-connection-retained",
        turn,
        "        QueryOutcome::Complete(output) => {\n"
        "            if let Some(query) = &mut query {\n"
        "                query.close().await;",
        "        QueryOutcome::Complete(output) => {\n"
        "            if let Some(query) = &mut query {\n"
        "                query.wait_peer_closed().await;\n"
        "                query.close().await;",
        "signal_cases::run_complete_cleanup",
        "#1145 run--complete-cleanup four surfaces, abort count, listeners and connections match TS",
    ),
    ('abort-stale-run-id', output, 'self.run_id = run_id.clone();', 'self.run_id = "requested-run".into();', 'product::sdk::turn::tests::abort_uses_consumed_run_id_without_utf16_loss', '#1145 abort uses latest lossless runId'),
    ('consumer-error-retains-query', output, 'return Err(EventError::Consumer(Failure::Error(\n                "event.type.includes is not a function".into(),', 'return Err(EventError::Iterator(Failure::Error(\n                "event.type.includes is not a function".into(),', 'product::sdk::turn::tests::output_failure_preserves_iterator_vs_consumer_lifetime', '#1145 event error matches TS iterator or consumer lifetime'),
    ('pending-abort-reader-finishes-early', turn, ') -> Result<(), Failure> {\n    loop {\n        match timeout_at(deadline, query.query_frame(None, abort_ids)).await {', ') -> Result<(), Failure> {\n    if abort_ids.len() == 1 { return Ok(()); }\n    loop {\n        match timeout_at(deadline, query.query_frame(None, abort_ids)).await {', 'product::sdk::turn::tests::pending_abort_keeps_query_until_reply', '#1145 abort cannot finish before its reply'),
    ('abort-send-extends-predecessor-deadline', turn, 'timeout_at(deadline, request)', 'timeout_at(deadline + transport::REQUEST_TIMEOUT, request)', 'product::sdk::turn::tests::abort_send_respects_predecessor_deadline', '#1145 blocked second abort cannot extend first deadline'),
]


def check_catalog():
    if len(mutations) != EXPECTED_TOTAL:
        raise RuntimeError("#1145 fixed product mutation denominator")
    names = [row[0] for row in mutations]
    if len(set(names)) != EXPECTED_TOTAL:
        raise RuntimeError("#1145 unique product mutation identities")
    stale = []
    for name, path, old, new, test, assertion in mutations:
        if not old or old == new:
            stale.append(f"{name}: empty or unchanged mutation")
        count = path.read_text(encoding="utf-8").count(old)
        if count != 1:
            stale.append(f"{name}: expected one anchor, found {count}")
        if not test.startswith(("signal_cases::", "product::sdk::turn::tests::")) or not assertion.startswith("#1145 "):
            stale.append(f"{name}: invalid named-test/assertion binding")
    if stale:
        raise RuntimeError("#1145 stale product controls: " + "; ".join(stale))


def raw_text(value):
    if isinstance(value, bytes):
        return value.decode("utf-8", errors="replace")
    return value or ""


def main():
    check_catalog()
    args = sys.argv[1:]
    if args == ["--check"]:
        print(f"#1145 {EXPECTED_TOTAL} product anchors: PASS")
        return 0
    if not args:
        raise SystemExit(
            "usage: negative_controls.py --check | NEW_OUTPUT_DIR [mutation ...]"
        )

    selected_names = args[1:]
    if (
        len(selected_names) != len(set(selected_names))
        or any(name not in {row[0] for row in mutations} for name in selected_names)
    ):
        raise SystemExit("#1145 unknown or duplicate mutation selection")
    selected = [
        row for row in mutations
        if not selected_names or row[0] in selected_names
    ]
    if not selected:
        raise SystemExit("#1145 selected product denominator must be nonzero")

    env = os.environ.copy()
    if not env.get("CARGO_TARGET_DIR"):
        raise SystemExit("#1145 CARGO_TARGET_DIR must be supplied by the caller")
    if not env.get("PDRS_BASELINE"):
        raise SystemExit("#1145 PDRS_BASELINE must identify the real TS checkout")

    wrapper = Path(
        env.get(
            "PDRS_SIGINT_WRAPPER",
            "/Users/da/ws/pilotdeck-rs/_tmp/20261025-claude-loop/sigint-default.py",
        )
    ).resolve()
    if not wrapper.is_file():
        raise SystemExit(f"#1145 missing SIGINT default wrapper: {wrapper}")

    # No receipt state may leak from a caller's unrelated negative invocation.
    env.pop("PDRS_NEGATIVE_RECEIPTS", None)
    env.pop("PDRS_NEGATIVE_PARENT_TEST", None)
    env.pop("PDRS_SIGNAL_CHILD", None)
    env.pop("PDRS_SIGNAL_ARGV", None)
    env.pop("PDRS_SIGNAL_OBSERVER", None)

    out = new_output_directory(args[0])
    exclusive_write(
        out / "run.json",
        json.dumps(
            {
                "issue": 1145,
                "registeredTotal": EXPECTED_TOTAL,
                "selectedTotal": len(selected),
                "selected": [row[0] for row in selected],
                "cargoTargetDir": env["CARGO_TARGET_DIR"],
                "baseline": env["PDRS_BASELINE"],
                "sigintWrapper": str(wrapper),
            },
            indent=2,
        ) + "\n",
    )

    command = [
        sys.executable, str(wrapper),
        "cargo", "test", "--offline",
        "--manifest-path", "src/Cargo.toml",
        "-p", "pilotdeck-cli",
        "--test", "sdk_signal_exit",
    ]

    def run(label, test, compile_only=False):
        run_env = dict(env)
        # The replay owns its case directory exclusively. Each invocation gets
        # a different root, including positive and restored runs of one test.
        run_env["PDRS_SIGNAL_RESULTS"] = str(out / f"{label}.observations")
        try:
            slot = wait_for_slot(
                root / "scripts/cargo-slots.sh",
                timeout=900,
                interval=15,
            )
            exclusive_write(
                out / f"{label}.slots.log", slot.stdout + slot.stderr
            )
            suffix = (
                ["--no-run"] if compile_only else
                [test, "--", "--exact", "--include-ignored", "--test-threads=1"]
            )
            result = run_owned(
                command + suffix,
                cwd=root,
                env=run_env,
                timeout=BUILD_TIMEOUT if compile_only else TEST_TIMEOUT,
            )
            exclusive_write(out / f"{label}.stdout", result.stdout)
            exclusive_write(out / f"{label}.stderr", result.stderr)
            return result
        except Exception as error:
            raw = ""
            if isinstance(error, subprocess.TimeoutExpired):
                raw = raw_text(error.stdout) + raw_text(error.stderr)
            exclusive_write(
                out / f"{label}.error.log",
                raw + f"\n{type(error).__name__}: {error}\n",
            )
            raise

    fixture = root / "src/pilotdeck-cli/tests/fixtures/sdk-signals"
    guarded = [
        *sorted((root / "src/pilotdeck-cli/src").rglob("*.rs")),
        root / "src/pilotdeck-cli/Cargo.toml",
        root / "src/Cargo.toml",
        root / "src/Cargo.lock",
        root / "src/pilotdeck-cli/tests/sdk_signal_exit.rs",
        root / "src/pilotdeck-cli/tests/common/signal_replay.rs",
        root / "src/pilotdeck-cli/tests/common/negative_receipt.rs",
        *sorted(path for path in fixture.iterdir() if path.is_file()),
        *sorted((fixture.parent / "sdk-signal-abort-state").glob("*")),
        Path(__file__).resolve(),
        Path(__file__).resolve().parents[1] / "negative_process.py",
        Path(__file__).resolve().parents[1] / "negative_suite.py",
    ]
    results = []
    phase = "enter"
    interrupted = False

    def record(name, test, assertion, status, reason, compiled):
        results.append(
            {
                "mutation": name,
                "test": test,
                "assertion": assertion,
                "status": status,
                "reason": reason,
                "compiled": compiled,
            }
        )
        print(f"{name}: {status}: {reason}", flush=True)

    try:
        with mutating_sources(root, guarded) as originals:
            phase = "body"
            for name, path, old, new, test, assertion in selected:
                case = Case(name, path, old, new, test, assertion)
                status, reason, compiled = "INVALID", "not run", False
                stage = "anchor"
                try:
                    original = originals[path].decode("utf-8")
                    if original.count(old) != 1:
                        raise RuntimeError(
                            "#1145 bound mutation anchor must occur exactly once"
                        )
                    stage = "positive"
                    positive = run(name + "-positive", test)
                    if not passed(positive, test):
                        reason = "positive control did not pass its named test"
                    else:
                        changed = original.replace(old, new, 1)
                        exclusive_write(
                            out / f"{name}.patch",
                            "".join(
                                difflib.unified_diff(
                                    original.splitlines(True),
                                    changed.splitlines(True),
                                    fromfile=str(path.relative_to(root)),
                                    tofile=str(path.relative_to(root)),
                                )
                            ),
                        )
                        path.write_text(changed, encoding="utf-8")
                        stage = "compile"
                        build = run(name + "-compile", test, compile_only=True)
                        compiled = (
                            build.returncode == 0
                            and "could not compile" not in build.stdout + build.stderr
                        )
                        if not compiled:
                            reason = "mutant failed no-run compilation; not a kill"
                        else:
                            stage = "negative"
                            status, reason = classify(
                                run(name + "-negative", test), case
                            )
                except Exception as error:
                    status = "INVALID"
                    reason = f"{stage}: {type(error).__name__}: {error}"
                finally:
                    # Always restore this case before any successor. The outer
                    # guard independently restores and checks HEAD on exit.
                    phase = "restore"
                    path.write_bytes(originals[path])
                    if path.read_bytes() != originals[path]:
                        raise RuntimeError(
                            f"#1145 restoration mismatch: {path.relative_to(root)}"
                        )
                    phase = "body"

                try:
                    restored = run(name + "-restored", test)
                    if not passed(restored, test):
                        status = "INVALID"
                        reason += "; restored named positive did not pass"
                except Exception as error:
                    status = "INVALID"
                    reason += (
                        f"; restored: {type(error).__name__}: {error}"
                    )
                record(name, test, assertion, status, reason, compiled)
            phase = "exit"

    except BaseException as error:
        interrupted = not isinstance(error, Exception)
        reason = f"source guard or interruption: {type(error).__name__}: {error}"
        # Completed kills become invalid if the guarded run cannot verify its
        # final state. Every unrun selected case is also explicitly accounted.
        invalidate_completed(results, reason)
        for name, _path, _old, _new, test, assertion in selected[len(results):]:
            record(name, test, assertion, "INVALID", reason, False)

    if len(results) != len(selected):
        raise RuntimeError("#1145 fixed selected result denominator")
    if {row["mutation"] for row in results} != {row[0] for row in selected}:
        raise RuntimeError("#1145 exact selected result identities")

    # The summary does not exist while product sources are mutated.
    publish_summary(out, results)
    counts = {
        state: sum(row["status"] == state for row in results)
        for state in ("KILLED", "SURVIVED", "INVALID")
    }
    print(
        json.dumps(
            {
                "registeredTotal": EXPECTED_TOTAL,
                "selectedTotal": len(selected),
                "total": len(results),
                "counts": counts,
            }
        ),
        flush=True,
    )
    if interrupted or phase in ("restore", "exit") and any(
        row["status"] == "INVALID"
        and row["reason"].startswith("source guard or interruption:")
        for row in results
    ):
        return 2
    return int(any(row["status"] != "KILLED" for row in results))


if __name__ == "__main__":
    sys.exit(main())
