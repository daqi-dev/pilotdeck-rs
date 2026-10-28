#!/usr/bin/env python3
# anchor-selfcheck: unsupported apparatus self-test runner, no mutation definitions
"""#969 live-e2e apparatus self-test (no Rust product code involved).

Runs the full window x injection matrix through the real TS sidecar client,
the Python stdio middleman, the real sidecar CLI and the local mock provider:

    selftest.py <output-dir> [--cases text-passthrough ...]

Every cell is asserted field-for-field against EXPECTED below, the exact
transcription of the README expectation table, so a wrong event, terminal,
error or model-call count can never pass as ``verified``. Two additional
direct apparatus checks exercise the middleman alone (recording after the
host closes the output pipe; bounded exit after host-side stdin EOF).

Every assertion carries a dedicated ``#969`` message: a failing check names
itself, and the mutation negative controls (negative_controls.py) bind to
those exact strings. All child processes are process-group owned and reaped
bounded; the mock never listens on 18789 and every spawned process (driver
included) runs with HOME inside its own case sandbox, so ~/.pilotdeck is
never touched.
"""
import argparse
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import sys
import textwrap
import time

FIXTURE = Path(__file__).resolve().parent
# Every driver case has its own fresh sandbox; reuse of a case dir is refused.
DRIVER_TIMEOUT_SECONDS = 90
MOCK_STARTUP_SECONDS = 10
MOCK_REAP_SECONDS = 5
# Bounds for the two direct middleman checks (the EOF-reap path legitimately
# takes a few seconds: grace, TERM, KILL).
MITM_CHECK_SECONDS = 15

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

# The 12-cell expectation table, transcribed cell-by-cell from the README
# table (every value measured on Node v22.22.0 against baseline cc7c4e65;
# each README row cites its matrix-run evidence file). Events are
# (type, modelEventType) pairs; modelEventType is None for every event the
# TS client does not wrap from a canonical model event.
EXPECTED = {
    "text-passthrough": {
        "events": [
            ("model_request_started", None),
            ("model_event", "request_started"),
            ("model_event", "message_start"),
            ("model_event", "text_delta"),
            ("model_event", "text_delta"),
            ("model_event", "message_end"),
            ("assistant_message", None),
            ("stop_requested", None),
            ("turn_completed", None),
        ],
        "terminal": {"type": "success", "stopReason": "completed", "turns": 1, "messageCount": 2},
        "error": None,
        "modelCalls": 1,
    },
    "text-drop": {
        "events": [("model_request_started", None)],
        "terminal": None,
        "error": MISMATCH_DROP,
        "modelCalls": 1,
    },
    "text-duplicate": {
        "events": [("model_request_started", None)],
        "terminal": None,
        "error": MISMATCH_DUPLICATE,
        "modelCalls": 0,
    },
    "text-early-final": {
        "events": [("model_request_started", None), ("turn_completed", None)],
        "terminal": {"type": "success", "stopReason": "mitm_premature_final", "turns": 1, "messageCount": 0},
        "error": None,
        "modelCalls": 1,
    },
    "tool-passthrough": {
        "events": [
            ("model_request_started", None),
            ("model_event", "request_started"),
            ("model_event", "message_start"),
            ("model_event", "tool_call_start"),
            ("model_event", "tool_call_delta"),
            ("model_event", "tool_call_end"),
            ("model_event", "message_end"),
            ("assistant_message", None),
            ("tool_calls_detected", None),
            ("tool_result", None),
            ("tool_results_projected", None),
            ("turn_continued", None),
            ("model_request_started", None),
            ("model_event", "request_started"),
            ("model_event", "message_start"),
            ("model_event", "text_delta"),
            ("model_event", "message_end"),
            ("assistant_message", None),
            ("stop_requested", None),
            ("turn_completed", None),
        ],
        "terminal": {"type": "success", "stopReason": "completed", "turns": 2, "messageCount": 4},
        "error": None,
        "modelCalls": 2,
    },
    "tool-drop": {
        "events": [("model_request_started", None)],
        "terminal": None,
        "error": MISMATCH_DROP,
        "modelCalls": 1,
    },
    "tool-duplicate": {
        "events": [("model_request_started", None)],
        "terminal": None,
        "error": MISMATCH_DUPLICATE,
        "modelCalls": 0,
    },
    "tool-early-final": {
        "events": [("model_request_started", None), ("turn_completed", None)],
        "terminal": {"type": "success", "stopReason": "mitm_premature_final", "turns": 1, "messageCount": 0},
        "error": None,
        "modelCalls": 1,
    },
    "cancel-passthrough": {
        "events": [
            ("model_request_started", None),
            ("model_event", "request_started"),
            ("turn_completed", None),
        ],
        "terminal": {"type": "aborted", "stopReason": "aborted_streaming", "turns": 1, "messageCount": 1},
        "error": None,
        "modelCalls": 1,
    },
    "cancel-drop": {
        "events": [("model_request_started", None)],
        "terminal": None,
        "error": MISMATCH_DROP,
        "modelCalls": 1,
    },
    "cancel-duplicate": {
        "events": [("model_request_started", None)],
        "terminal": None,
        "error": MISMATCH_DUPLICATE,
        "modelCalls": 0,
    },
    "cancel-early-final": {
        "events": [("model_request_started", None), ("turn_completed", None)],
        "terminal": {"type": "success", "stopReason": "mitm_premature_final", "turns": 1, "messageCount": 0},
        "error": None,
        "modelCalls": 1,
    },
}


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
    # The driver itself runs with HOME inside the case sandbox, like the
    # middleman and sidecar it spawns: a baseline that resolves config
    # through HOME must never see the real ~/.pilotdeck. driver.mts asserts
    # the value back at us, and negative_controls.py mutates this isolation.
    home = case_dir / "home"
    for directory in (home, home / ".config", home / ".cache",
                      home / ".local", home / ".local" / "share", home / ".state"):
        directory.mkdir(parents=True, exist_ok=True)
    env.update({
        "HOME": str(home),
        "XDG_CONFIG_HOME": str(home / ".config"),
        "XDG_CACHE_HOME": str(home / ".cache"),
        "XDG_DATA_HOME": str(home / ".local" / "share"),
        "XDG_STATE_HOME": str(home / ".state"),
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
        # Archived before the exit check: a failing driver's raw logs are the
        # evidence for what went wrong (the message excerpt keeps the tail,
        # where the driver's own dedicated #969 lines land).
        (case_dir / "driver-stdout.log").write_text(stdout)
        (case_dir / "driver-stderr.log").write_text(stderr)
        check(process.returncode == 0,
              f"#969 {name}: driver exited {process.returncode}: {stderr.strip()[-500:]}")
        result = json.loads((case_dir / "driver-result.json").read_text())
        frames = [json.loads(line) for line in (case_dir / "frames.jsonl").read_text().splitlines()]
        summary = json.loads((case_dir / "mitm-summary.json").read_text())
        (case_dir / "driver-stdout.log").write_text(stdout)
        (case_dir / "driver-stderr.log").write_text(stderr)
        return result, frames, summary, case_dir
    finally:
        # Every exit path (a timeout SelftestError, a Ctrl+C between
        # communicate() and the reads, any read error) still reaps the driver
        # group bounded; a no-op once communicate() already reaped it.
        terminate_group(process)


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


def assert_clean_passthrough(name, frames, scenario):
    """MITM-side passthrough invariants; the TS-side values are in EXPECTED."""
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
        # The two direct middleman checks run on every invocation (also when
        # --cases restricts the matrix): they are part of the ruler.
        mitm_checks = {}
        for check_name, check_fn in (("mitm-pipe-close", verify_mitm_pipe_close),
                                     ("mitm-eof-reap", verify_mitm_eof_reap)):
            try:
                outcome = check_fn(out_root)
                mitm_checks[check_name] = "ok" if outcome is None else outcome
            except SelftestError as error:
                failures.append(str(error))
                print(f"FAIL {check_name}: {error}", file=sys.stderr, flush=True)
                mitm_checks[check_name] = str(error)
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
                # Same structural isolation as the driver: every spawned
                # process of the apparatus runs with HOME in the case sandbox.
                "HOME": str(case_dir / "home"),
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
            home_touched, home_unreadable = assert_home_untouched(home_root, run_started_at)
        except SelftestError as error:
            failures.append(str(error))
            print(f"FAIL home: {error}", file=sys.stderr, flush=True)
            home_touched, home_unreadable = ["check failed"], ["check failed"]
        write_report(out_root, matrix, failures, time.monotonic() - started,
                     home_touched, home_unreadable, mitm_checks)
    if failures:
        print(f"#969 selftest: {len(failures)} failing assertion(s) of {len(cells)} cases", file=sys.stderr)
        return 1
    print(f"#969 selftest: {len(cells)} cases passed; evidence in {out_root}")
    return 0


def verify_case(name, scenario, injection, mock_port, out_root, case_dir, mock_log):
    result, frames, summary, _ = run_case(name, scenario, injection, mock_port, case_dir)
    expected = EXPECTED[name]
    observed = [(entry["type"], entry.get("modelEventType")) for entry in result["eventDetails"]]
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

    # The recorded details and the event list are two views of one stream: a
    # driver that lets them disagree would let a forged list slip through.
    check([entry["type"] for entry in result["eventDetails"]] == result["events"],
          f"#969 {name}: driver event details must agree with the event list: "
          f"{result['eventDetails']!r} vs {result['events']!r}")

    # MITM-side structural evidence first: the mutations that turn an
    # injection off bind to these dedicated messages.
    if injection != "passthrough":
        assert_injection_fired(name, summary, injection.replace("-", "_"))
    if injection == "drop":
        dropped = [record for record in events_of(frames) if "drop" in record["action"]]
        check(len(dropped) == 1, f"#969 {name}: MITM must record exactly one dropped frame, got {len(dropped)}")
        check(dropped[0]["delivered"] is False, f"#969 {name}: dropped frame must be recorded as not delivered")
    if injection == "duplicate":
        duplicated = [record for record in events_of(frames) if "duplicate" in record["action"]]
        check(len(duplicated) == 1, f"#969 {name}: MITM must record exactly one duplicated frame, got {len(duplicated)}")
        check(duplicated[0]["frame"]["sequence"] == 0,
              f"#969 {name}: duplicate must target sequence 0")
    if injection == "early-final":
        rewritten = [record for record in events_of(frames) if "early_final" in record["action"]]
        check(len(rewritten) == 1, f"#969 {name}: MITM must record exactly one early-final rewrite, got {len(rewritten)}")
        frame = rewritten[0]["frame"]
        check(frame["final"] is True and frame["outcome"] == "completed",
              f"#969 {name}: early-final rewrite must mark the frame final/completed")
        check(frame["payload"]["result"]["stopReason"] == "mitm_premature_final",
              f"#969 {name}: early-final rewrite must carry the marker stopReason")

    # The ruler: every README-registered TS observable, field for field. A
    # wrong event order, a missing modelEventType, a wrong terminal count, an
    # unexpected error or model-call count each fail their own cell.
    check(observed == expected["events"],
          f"#969 {name}: TS event sequence must match the README expectation: "
          f"{observed} != {expected['events']}")
    check(result["terminal"] == expected["terminal"],
          f"#969 {name}: TS terminal must match the README expectation: "
          f"{result['terminal']!r} != {expected['terminal']!r}")
    check(result["error"] == expected["error"],
          f"#969 {name}: TS run() error must match the README expectation: "
          f"{result['error']!r} != {expected['error']!r}")
    check(result["modelCalls"] == expected["modelCalls"],
          f"#969 {name}: TS model call count must match the README expectation: "
          f"{result['modelCalls']} != {expected['modelCalls']}")

    if injection == "passthrough":
        assert_clean_passthrough(name, frames, scenario)
        if scenario == "tool":
            check(len(result["toolCalls"]) == 1,
                  f"#969 {name}: tool window must execute exactly one read call")
            check(result["toolResults"] and result["toolResults"][0]["text"] == "live-e2e read tool payload\n",
                  f"#969 {name}: read tool must return the sandbox file payload")
            entries = mock_entries(mock_log)
            check(len(entries) == 2,
                  f"#969 {name}: mock provider must observe two requests, got {entries}")
            check(all(entry["connection"] == "completed" for entry in entries),
                  f"#969 {name}: both mock provider connections must complete")
        if scenario == "cancel":
            check(result["modelInterrupted"] is True,
                  f"#969 {name}: cancel window must interrupt the hanging provider connection")
            entries = mock_entries(mock_log)
            check(entries and entries[0]["connection"] == "aborted",
                  f"#969 {name}: mock provider must record the interrupted connection as aborted: {entries}")
    cell["verified"] = True
    return cell


def spawn_mitm(log_path, summary_path, child_command):
    """Spawn the real middleman in its own session, like the driver does."""
    return subprocess.Popen(
        [sys.executable, str(FIXTURE / "stdio_mitm.py"),
         "--log", str(log_path), "--summary", str(summary_path),
         "--", *child_command],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        text=True, start_new_session=True)


def mitm_stderr(process):
    try:
        return process.stderr.read().strip()
    except (OSError, ValueError):
        return ""


def read_frame_log(log_path):
    return [json.loads(line) for line in log_path.read_text().splitlines() if line.strip()]


def verify_mitm_pipe_close(out_root):
    """A host that closes our stdout early must not stop the recording.

    The child emits one frame, waits until the host is really gone, then
    emits two more: only a middleman that keeps observing after the output
    pipe broke can ever log them, and ``delivered`` must tell the truth
    about which writes went out.
    """
    name = "mitm-pipe-close"
    check_dir = out_root / name
    check_dir.mkdir(parents=True)
    log = check_dir / "frames.jsonl"
    summary = check_dir / "mitm-summary.json"
    gate = check_dir / "gate"
    script = check_dir / "child.py"
    script.write_text(textwrap.dedent(f"""
        import json, sys, time
        def emit(sequence):
            sys.stdout.write(json.dumps({{"kind": "event", "sequence": sequence, "final": False}}) + "\\n")
            sys.stdout.flush()
        emit(0)
        while not __import__("pathlib").Path({str(gate)!r}).exists():
            time.sleep(0.01)
        emit(1)
        emit(2)
    """))
    process = spawn_mitm(log, summary, [sys.executable, str(script)])
    try:
        forwarded = process.stdout.readline()
        check(forwarded.strip() and json.loads(forwarded)["sequence"] == 0,
              f"#969 {name}: MITM must forward the pre-close frame to the host: {forwarded!r}")
        process.stdout.close()  # the host goes away mid-stream
        gate.write_text("go\n")
        process.stdin.close()  # host-side EOF; the child exits on its own
        try:
            process.wait(timeout=MITM_CHECK_SECONDS)
        except subprocess.TimeoutExpired:
            raise SelftestError(
                f"#969 {name}: MITM must exit after the host closed both pipes "
                f"(timed out after {MITM_CHECK_SECONDS}s)")
        check(process.returncode == 0,
              f"#969 {name}: MITM must exit cleanly after the host closed the output pipe: "
              f"rc={process.returncode}: {mitm_stderr(process)}")
        records = read_frame_log(log)
        sequences = [record["frame"]["sequence"] for record in records]
        check(len(records) == 3,
              f"#969 {name}: MITM must keep recording after the host closes the output pipe: "
              f"{len(records)} frames recorded ({sequences}), expected 3")
        check(records[0]["delivered"] is True,
              f"#969 {name}: the frame forwarded before the close must be recorded as delivered: {records[0]!r}")
        check(all(record["delivered"] is False for record in records[1:]),
              f"#969 {name}: frames after the host closed the pipe must be recorded as not "
              f"delivered: {records[1:]!r}")
        check(summary.is_file(), f"#969 {name}: MITM must write its summary after the pipe close")
    finally:
        terminate_group(process)


def verify_mitm_eof_reap(out_root):
    """Host stdin EOF with a TERM-ignoring child must end in a bounded exit.

    A child stuck on module calls to the departed host must not keep the
    middleman's stdout reader blocked forever: the EOF path reaps it with a
    grace, then TERM, then KILL.
    """
    name = "mitm-eof-reap"
    check_dir = out_root / name
    check_dir.mkdir(parents=True)
    log = check_dir / "frames.jsonl"
    summary = check_dir / "mitm-summary.json"
    script = check_dir / "child.py"
    script.write_text(textwrap.dedent(f"""
        import json, signal, sys, time
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
        sys.stdout.write(json.dumps({{"kind": "event", "sequence": 0, "final": False}}) + "\\n")
        sys.stdout.flush()
        time.sleep(300)
    """))
    process = spawn_mitm(log, summary, [sys.executable, str(script)])
    try:
        forwarded = process.stdout.readline()
        check(forwarded.strip() and json.loads(forwarded)["sequence"] == 0,
              f"#969 {name}: MITM must forward the child's frame to the host: {forwarded!r}")
        started = time.monotonic()
        process.stdin.close()  # the host disappears; the child never exits alone
        try:
            process.wait(timeout=MITM_CHECK_SECONDS)
        except subprocess.TimeoutExpired:
            raise SelftestError(
                f"#969 {name}: MITM must exit after host stdin EOF even when the child "
                f"ignores SIGTERM (timed out after {MITM_CHECK_SECONDS}s)")
        elapsed = time.monotonic() - started
        check(process.returncode == 0,
              f"#969 {name}: MITM must exit cleanly after reaping the EOF-stuck child: "
              f"rc={process.returncode}: {mitm_stderr(process)}")
        records = read_frame_log(log)
        check(len(records) == 1,
              f"#969 {name}: MITM must record the child's frame before the EOF reap: {records!r}")
        check(summary.is_file(), f"#969 {name}: MITM must write its summary on the EOF reap path")
        return {"elapsedSeconds": round(elapsed, 1)}
    finally:
        terminate_group(process)


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
    """Audit ~/.pilotdeck for harness-identifiable writes.

    The primary guarantee is structural: every process this harness spawns
    (driver, middleman, sidecar, mock) runs with HOME inside its case
    sandbox, and driver.mts asserts that value back. This audit is the
    secondary net: concurrent processes (the user's real install)
    legitimately write to ~/.pilotdeck during the run, so every file modified
    since the run started is checked for harness identifiers. Files that
    cannot be read are reported instead of silently passed.
    """
    if not home_root.is_dir():
        return [], []
    harness_markers = ("live-e2e", "mitm_premature_final", "LIVE_E2E_")
    touched, unreadable = [], []
    for path in home_root.rglob("*"):
        try:
            if not path.is_file() or path.stat().st_mtime < run_started_at:
                continue
        except OSError:
            unreadable.append(str(path))
            continue
        touched.append(str(path))
        try:
            content = path.read_text(errors="ignore")
        except OSError:
            unreadable.append(str(path))
            continue
        for marker in harness_markers:
            check(marker not in content,
                  f"#969 selftest: harness wrote into ~/.pilotdeck: {path} mentions {marker!r}")
    return touched, unreadable


def write_report(out_root, matrix, failures, elapsed, home_touched, home_unreadable, mitm_checks):
    report = {
        "cases": matrix,
        "mitmChecks": mitm_checks,
        "failures": failures,
        "elapsedSeconds": round(elapsed, 1),
        "node": subprocess.run(["node", "--version"], capture_output=True, text=True).stdout.strip(),
        "python": sys.version.split()[0],
        "homePilotdeckWrittenDuringRun": home_touched,
        "homePilotdeckUnreadableDuringRun": home_unreadable,
    }
    (out_root / "report.json").write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
    for entry in matrix:
        print(f"{entry.get('case')}: {'ok' if entry.get('verified') else 'FAILED'}")


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
