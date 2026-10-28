#!/usr/bin/env python3
# anchor-selfcheck: unsupported apparatus self-test runner, no mutation definitions
"""#969 live-e2e apparatus self-test (no Rust product code involved).

Runs the full window x injection matrix through the real TS sidecar client,
the Python stdio middleman, the real sidecar CLI and the local mock provider:

    selftest.py <output-dir> [--cases text-passthrough ...]

Every assertion carries a dedicated ``#969`` message: a failing check names
itself, and the mutation negative controls (negative_controls.py) bind to
those exact strings. All child processes are process-group owned and reaped
bounded; the mock never listens on 18789 and every case runs with HOME inside
its own sandbox, so ~/.pilotdeck is never touched.
"""
import argparse
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import sys
import time

FIXTURE = Path(__file__).resolve().parent
# Every driver case has its own fresh sandbox; reuse of a case dir is refused.
DRIVER_TIMEOUT_SECONDS = 90
MOCK_STARTUP_SECONDS = 10
MOCK_REAP_SECONDS = 5

SCENARIOS = ("text", "tool", "cancel")
INJECTIONS = ("passthrough", "drop", "duplicate", "early-final")

INJECTION_PLANS = {
    # Injection points are declared as "the Nth matching frame" and are
    # deterministic: the text window always emits the same event sequence
    # (model_request_started, 5x model_event, assistant_message, ...).
    "drop": {"injections": [
        {"action": "drop", "direction": "s2h", "kind": "event", "final": False, "nth": 2},
    ]},
    "duplicate": {"injections": [
        {"action": "duplicate", "direction": "s2h", "kind": "event", "final": False, "nth": 1},
    ]},
    "early-final": {"injections": [
        {"action": "early_final", "direction": "s2h", "kind": "event", "final": False, "nth": 2},
    ]},
}

# Measured TS client reactions (Node v22.22.0, baseline cc7c4e65). These are
# the expectation values #748b will hold the Rust side to; README.md documents
# each cell's raw-output provenance.
MISMATCH_DROP = "Sidecar event sequence mismatch: expected 1, got 2."
MISMATCH_DUPLICATE = "Sidecar event sequence mismatch: expected 1, got 0."


class SelftestError(RuntimeError):
    pass


def check(condition, message):
    if not condition:
        raise SelftestError(message)


def free_port():
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


def port_free(port):
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as probe:
        probe.settimeout(0.5)
        try:
            probe.connect(("127.0.0.1", port))
        except OSError:
            return True
        return False


def run_case(name, scenario, injection, mock_port, case_dir):
    """Run one matrix cell; returns (result, frames, mitm_summary)."""
    env = os.environ.copy()
    env.update({
        "LIVE_E2E_MOCK_PORT": str(mock_port),
        # The override exists for manual MITM experiments; the fixture's own
        # middleman is the default.
        "LIVE_E2E_MITM": os.environ.get("LIVE_E2E_MITM", str(FIXTURE / "stdio_mitm.py")),
        "LIVE_E2E_PYTHON": sys.executable,
    })
    command = [
        "node",
        "--import", str(Path(env["PDRS_BASELINE"]) / "node_modules/tsx/dist/loader.mjs"),
        str(FIXTURE / "driver.mts"),
        scenario, injection, str(case_dir),
    ]
    process = subprocess.Popen(command, env=env, cwd=FIXTURE,
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                               text=True, start_new_session=True)
    try:
        stdout, stderr = process.communicate(timeout=DRIVER_TIMEOUT_SECONDS)
    except subprocess.TimeoutExpired:
        # SIGTERM first: the middleman then reaps the sidecar group and can
        # still write its summary before the hard kill.
        try:
            os.killpg(process.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        try:
            stdout, stderr = process.communicate(timeout=MOCK_REAP_SECONDS)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            stdout, stderr = process.communicate()
        raise SelftestError(f"#969 {name}: driver timed out after {DRIVER_TIMEOUT_SECONDS}s")
    check(process.returncode == 0,
          f"#969 {name}: driver exited {process.returncode}: {stderr.strip()[-500:]}")
    result = json.loads((case_dir / "driver-result.json").read_text())
    frames = [json.loads(line) for line in (case_dir / "frames.jsonl").read_text().splitlines()]
    summary = json.loads((case_dir / "mitm-summary.json").read_text())
    (case_dir / "driver-stdout.log").write_text(stdout)
    (case_dir / "driver-stderr.log").write_text(stderr)
    return result, frames, summary, case_dir


def case_paths(name, out_root):
    case_dir = out_root / name
    return case_dir, case_dir / "mock.log"


def events_of(frames, direction="s2h", kind="event"):
    return [record for record in frames
            if record["dir"] == direction and record["frame"].get("kind") == kind]


def delivered_sequences(frames):
    return [record["frame"]["sequence"] for record in events_of(frames) if record["delivered"]]


def h2s_methods(frames):
    return [record["frame"].get("method") for record in frames
            if record["dir"] == "h2s" and record["frame"].get("kind") == "request"]


def mock_entries(mock_log):
    return [json.loads(line) for line in Path(mock_log).read_text().splitlines()
            if line.strip() and '"scenario"' in line]


def assert_clean_passthrough(name, result, frames, scenario):
    check(result["error"] is None, f"#969 {name}: passthrough must not surface a client error: {result['error']!r}")
    check(result["terminal"] is not None, f"#969 {name}: passthrough must complete with a terminal")
    sequences = delivered_sequences(frames)
    check(sequences == list(range(len(sequences))),
          f"#969 {name}: passthrough delivered sequences must be gap-free and duplicate-free: {sequences}")
    for record in frames:
        check(record["action"] == [],
              f"#969 {name}: passthrough must not rewrite any frame, but frame {record['index']} has {record['action']}")
    if scenario == "cancel":
        # The cancel window is the only passthrough cell whose terminal is
        # aborted by design; it must also be the only one sending a cancel.
        check("cancel" in h2s_methods(frames),
              f"#969 {name}: cancel window must send a cancel request: {h2s_methods(frames)}")
    else:
        check(result["terminal"]["type"] == "success",
              f"#969 {name}: passthrough terminal must be success, got {result['terminal']['type']}")
        check(result["terminal"]["stopReason"] == "completed",
              f"#969 {name}: passthrough stopReason must be completed, got {result['terminal']['stopReason']!r}")
        check("cancel" not in h2s_methods(frames),
              f"#969 {name}: passthrough must not send a cancel request: {h2s_methods(frames)}")
    return sequences


def assert_injection_fired(name, summary, action):
    injections = summary["injections"]
    check(len(injections) == 1, f"#969 {name}: expected exactly one declared injection, got {injections}")
    check(injections[0]["fired"] is True,
          f"#969 {name}: MITM must record the {action} injection as fired")
    check(injections[0]["spec"]["action"] == action,
          f"#969 {name}: MITM fired the wrong injection: {injections[0]['spec']}")


def main(argv):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", help="evidence output directory (must not exist)")
    parser.add_argument("--cases", nargs="*", help="restrict to named cases like text-drop")
    args = parser.parse_args(argv)
    check("PDRS_BASELINE" in os.environ, "#969 selftest: set PDRS_BASELINE to the read-only TS baseline root")
    baseline = Path(os.environ["PDRS_BASELINE"])
    check((baseline / "src/cli/pilotdeck-agent-loop-sidecar.ts").is_file(),
          f"#969 selftest: PDRS_BASELINE does not look like the TS baseline: {baseline}")
    node_version = subprocess.run(["node", "--version"], capture_output=True, text=True)
    check(node_version.stdout.strip() == "v22.22.0",
          f"#969 selftest: requires Node v22.22.0, found {node_version.stdout.strip()}")

    out_root = Path(args.output).resolve()
    check(not out_root.exists(), f"#969 selftest: output directory must not exist: {out_root}")
    out_root.mkdir(parents=True)
    # ~/.pilotdeck belongs to the user's real install and other processes write
    # there concurrently, so presence alone proves nothing: what the harness
    # must never do is leave its own identifiers behind (write_report checks
    # every file modified during the run for them).
    home_root = Path.home() / ".pilotdeck"
    run_started_at = time.time()

    matrix = []
    if args.cases:
        all_cells = {(scenario, injection) for scenario in SCENARIOS for injection in INJECTIONS}
        cells = []
        for case in args.cases:
            matches = [cell for cell in all_cells if case == "-".join(cell)]
            check(len(matches) == 1,
                  f"#969 selftest: unknown case name {case!r} (expected <scenario>-<injection>)")
            cells.append(matches[0])
    else:
        cells = [(scenario, injection) for scenario in SCENARIOS for injection in INJECTIONS]

    failures = []
    started = time.monotonic()
    try:
        for scenario, injection in cells:
            name = f"{scenario}-{injection}"
            mock_port = free_port()
            case_dir, mock_log = case_paths(name, out_root)
            check(not case_dir.exists(), f"#969 {name}: case dir already exists: {case_dir}")
            case_dir.mkdir(parents=True)
            if injection != "passthrough":
                (case_dir / "plan.json").write_text(json.dumps(INJECTION_PLANS[injection]) + "\n")
            mock_env = os.environ.copy()
            mock_env.update({
                "LIVE_E2E_MOCK_PORT": str(mock_port),
                "LIVE_E2E_MOCK_LOG": str(mock_log),
            })
            mock = subprocess.Popen(["node", str(FIXTURE / "mock_provider.mjs")],
                                    env=mock_env, stdout=subprocess.DEVNULL,
                                    stderr=subprocess.PIPE, text=True, start_new_session=True)
            try:
                deadline = time.monotonic() + MOCK_STARTUP_SECONDS
                while True:
                    if mock.poll() is not None:
                        stderr = mock.communicate()[1]
                        raise SelftestError(f"#969 {name}: mock provider exited at startup: {stderr.strip()}")
                    if mock_log.exists() and "listening" in mock_log.read_text():
                        break
                    check(time.monotonic() < deadline, f"#969 {name}: mock provider did not become ready")
                    time.sleep(0.05)
                try:
                    cell = verify_case(name, scenario, injection, mock_port, out_root,
                                       case_dir, mock_log)
                except SelftestError as error:
                    failures.append(str(error))
                    print(f"FAIL {name}: {error}", file=sys.stderr, flush=True)
                    cell = {"case": name, "failed": str(error)}
                matrix.append(cell)
            finally:
                terminate_group(mock)
            check(port_free(mock_port),
                  f"#969 {name}: mock provider port {mock_port} was not released after reaping")
    finally:
        try:
            home_touched = assert_home_untouched(home_root, run_started_at)
        except SelftestError as error:
            failures.append(str(error))
            print(f"FAIL home: {error}", file=sys.stderr, flush=True)
            home_touched = ["check failed"]
        write_report(out_root, matrix, failures, time.monotonic() - started, home_touched)
    if failures:
        print(f"#969 selftest: {len(failures)} failing assertion(s) of {len(cells)} cases", file=sys.stderr)
        return 1
    print(f"#969 selftest: {len(cells)} cases passed; evidence in {out_root}")
    return 0


def verify_case(name, scenario, injection, mock_port, out_root, case_dir, mock_log):
    result, frames, summary, _ = run_case(name, scenario, injection, mock_port, case_dir)
    cell = {
        "case": name,
        "scenario": scenario,
        "injection": injection,
        "terminal": result["terminal"],
        "error": result["error"],
        "events": result["events"],
        "modelCalls": result["modelCalls"],
        "toolCalls": len(result["toolCalls"]),
        "deliveredSequences": delivered_sequences(frames),
        "frames": len(frames),
    }

    if injection == "passthrough":
        assert_clean_passthrough(name, result, frames, scenario)
        if scenario == "text":
            check(result["modelCalls"] == 1,
                  f"#969 {name}: text window must make exactly one model call, got {result['modelCalls']}")
            check(result["events"][0] == "model_request_started",
                  f"#969 {name}: text window must start with model_request_started, got {result['events'][:1]}")
            check("assistant_message" in result["events"],
                  f"#969 {name}: text window must surface assistant_message")
        if scenario == "tool":
            check(result["modelCalls"] == 2,
                  f"#969 {name}: tool window must make two model calls, got {result['modelCalls']}")
            check(len(result["toolCalls"]) == 1,
                  f"#969 {name}: tool window must execute exactly one read call")
            check(result["toolResults"] and result["toolResults"][0]["text"] == "live-e2e read tool payload\n",
                  f"#969 {name}: read tool must return the sandbox file payload")
            for event_type in ("tool_calls_detected", "tool_result", "tool_results_projected"):
                check(event_type in result["events"],
                      f"#969 {name}: tool window must surface {event_type}: {result['events']}")
            entries = mock_entries(mock_log)
            check(len(entries) == 2,
                  f"#969 {name}: mock provider must observe two requests, got {entries}")
            check(all(entry["connection"] == "completed" for entry in entries),
                  f"#969 {name}: both mock provider connections must complete")
        if scenario == "cancel":
            check(result["terminal"]["type"] == "aborted",
                  f"#969 {name}: cancel window terminal must be aborted, got {result['terminal']}")
            check(result["terminal"]["stopReason"] == "aborted_streaming",
                  f"#969 {name}: cancel window stopReason must be aborted_streaming")
            check(result["modelInterrupted"] is True,
                  f"#969 {name}: cancel window must interrupt the hanging provider connection")
            entries = mock_entries(mock_log)
            check(entries and entries[0]["connection"] == "aborted",
                  f"#969 {name}: mock provider must record the interrupted connection as aborted: {entries}")
            check(result["modelCalls"] == 1,
                  f"#969 {name}: cancel window must have started exactly one model call")
    else:
        assert_injection_fired(name, summary, injection.replace("-", "_"))

    if injection == "drop":
        dropped = [record for record in events_of(frames) if "drop" in record["action"]]
        check(len(dropped) == 1, f"#969 {name}: MITM must record exactly one dropped frame, got {len(dropped)}")
        check(dropped[0]["delivered"] is False, f"#969 {name}: dropped frame must be recorded as not delivered")
        check(result["error"] == MISMATCH_DROP,
              f"#969 {name}: TS client must reject the sequence gap: {result['error']!r}")
        check(result["terminal"] is None, f"#969 {name}: a dropped frame must not produce a terminal")
    if injection == "duplicate":
        duplicated = [record for record in events_of(frames) if "duplicate" in record["action"]]
        check(len(duplicated) == 1, f"#969 {name}: MITM must record exactly one duplicated frame, got {len(duplicated)}")
        check(duplicated[0]["frame"]["sequence"] == 0,
              f"#969 {name}: duplicate must target sequence 0")
        check(result["error"] == MISMATCH_DUPLICATE,
              f"#969 {name}: TS client must reject the repeated sequence: {result['error']!r}")
        check(result["terminal"] is None, f"#969 {name}: a duplicated frame must not produce a terminal")
    if injection == "early-final":
        rewritten = [record for record in events_of(frames) if "early_final" in record["action"]]
        check(len(rewritten) == 1, f"#969 {name}: MITM must record exactly one early-final rewrite, got {len(rewritten)}")
        frame = rewritten[0]["frame"]
        check(frame["final"] is True and frame["outcome"] == "completed",
              f"#969 {name}: early-final rewrite must mark the frame final/completed")
        check(frame["payload"]["result"]["stopReason"] == "mitm_premature_final",
              f"#969 {name}: early-final rewrite must carry the marker stopReason")
        check(result["terminal"] is not None and result["terminal"]["stopReason"] == "mitm_premature_final",
              f"#969 {name}: TS client must accept the premature final as its terminal: {result['terminal']}")
        check(result["error"] is None,
              f"#969 {name}: accepting the premature final must not surface an error: {result['error']!r}")
        check(result["terminal"]["type"] == "success",
              f"#969 {name}: the premature terminal must be classified success")
    cell["verified"] = True
    return cell


def terminate_group(process):
    if process.poll() is not None:
        return
    try:
        os.killpg(process.pid, signal.SIGTERM)
    except ProcessLookupError:
        return
    deadline = time.monotonic() + MOCK_REAP_SECONDS
    while time.monotonic() < deadline:
        if process.poll() is not None:
            return
        time.sleep(0.05)
    try:
        os.killpg(process.pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    process.wait(timeout=MOCK_REAP_SECONDS)


def assert_home_untouched(home_root, run_started_at):
    """~/.pilotdeck must not receive any harness-identifiable write.

    Concurrent processes (the user's real install) legitimately write to
    ~/.pilotdeck during the run, so only harness identifiers count: every
    file modified since the run started is checked for them.
    """
    if not home_root.is_dir():
        return []
    harness_markers = ("live-e2e", "mitm_premature_final", "LIVE_E2E_")
    touched = []
    for path in home_root.rglob("*"):
        try:
            if not path.is_file() or path.stat().st_mtime < run_started_at:
                continue
        except OSError:
            continue
        touched.append(str(path))
        try:
            content = path.read_text(errors="ignore")
        except OSError:
            continue
        for marker in harness_markers:
            check(marker not in content,
                  f"#969 selftest: harness wrote into ~/.pilotdeck: {path} mentions {marker!r}")
    return touched


def write_report(out_root, matrix, failures, elapsed, home_touched):
    report = {
        "cases": matrix,
        "failures": failures,
        "elapsedSeconds": round(elapsed, 1),
        "node": subprocess.run(["node", "--version"], capture_output=True, text=True).stdout.strip(),
        "python": sys.version.split()[0],
        "homePilotdeckWrittenDuringRun": home_touched,
    }
    (out_root / "report.json").write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
    for entry in matrix:
        print(f"{entry.get('case')}: {'ok' if entry.get('verified') else 'FAILED'}")


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
