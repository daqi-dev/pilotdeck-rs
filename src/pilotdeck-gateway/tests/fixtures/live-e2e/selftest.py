#!/usr/bin/env python3
# anchor-selfcheck: unsupported apparatus self-test runner, no mutation definitions
"""#969 live-e2e apparatus self-test (no Rust product code involved).

Runs the full window x injection matrix through the real TS sidecar client,
the Python stdio middleman, the real sidecar CLI and the local mock provider:

    selftest.py <output-dir> [--cases text-passthrough ...]

Every cell compares complete raw JSON with independently generated expected/
files. Context tokens are expanded only from the orchestrator's known inputs;
actual evidence is never projected, counted or normalized before comparison.
Failures always carry verified:false and a dedicated #969 message. Child pipe
I/O and cleanup are bounded; HOME stays in the owned case sandbox.
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

sys.path.insert(0, str(Path(__file__).resolve().parent))
from io_deadline import Lines, DeadlineExceeded

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

class SelftestError(RuntimeError):
    pass


def check(condition, message):
    if not condition:
        raise SelftestError(message)


JSON_TYPE_NAMES = {bool: "boolean", int: "integer", float: "number",
                   str: "string", type(None): "null"}


def json_type(value):
    return JSON_TYPE_NAMES.get(type(value), type(value).__name__)


def strict_mismatch(actual, expected, path):
    """Type-strict deep comparison of two JSON values; None when equal.

    Python's ``==`` accepts ``True == 1`` and ``1 == 1.0``, so a forged
    driver result could verify against the expectation table with
    JSON-incompatible types. Every level must agree on type first, then on
    value; dicts must carry exactly the same key set.
    """
    if type(actual) is not type(expected):
        return (f"{path}: expected {json_type(expected)} {expected!r}, "
                f"got {json_type(actual)} {actual!r}")
    if isinstance(expected, dict):
        for key in sorted(set(expected) | set(actual)):
            if key not in expected:
                return (f"{path}.{key}: unexpected key "
                        f"(expected exactly {sorted(expected)})")
            if key not in actual:
                return (f"{path}.{key}: missing key "
                        f"(expected exactly {sorted(expected)})")
            mismatch = strict_mismatch(actual[key], expected[key], f"{path}.{key}")
            if mismatch:
                return mismatch
        return None
    if isinstance(expected, (list, tuple)):
        if len(actual) != len(expected):
            return (f"{path}: expected {len(expected)} items, "
                    f"got {len(actual)}: {actual!r} vs {expected!r}")
        for index, (item, wanted) in enumerate(zip(actual, expected)):
            mismatch = strict_mismatch(item, wanted, f"{path}[{index}]")
            if mismatch:
                return mismatch
        return None
    if actual != expected:
        return f"{path}: expected {expected!r}, got {actual!r}"
    return None


def assert_strict(name, what, actual, expected):
    """Assert complete frozen evidence with a dedicated, type-strict message."""
    mismatch = strict_mismatch(actual, expected, what)
    check(mismatch is None,
          f"#969 {name}: TS {what} must match the frozen expected file: {mismatch}")


def pid_alive(pid):
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


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
        "--import", str(FIXTURE / "determinism.mjs"),
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
                stdout, stderr = process.communicate(timeout=MOCK_REAP_SECONDS)
            raise SelftestError(f"#969 {name}: driver timed out after {DRIVER_TIMEOUT_SECONDS}s")
        # Archived before the exit check: a failing driver's raw logs are the
        # evidence for what went wrong (the message excerpt keeps the tail,
        # where the driver's own dedicated #969 lines land).
        (case_dir / "driver-stdout.log").write_text(stdout)
        (case_dir / "driver-stderr.log").write_text(stderr)
        check(process.returncode == 0,
              f"#969 {name}: driver exited {process.returncode}: {stderr.strip()[-500:]}")
        result = json.loads((case_dir / "driver-result.json").read_text())
        frames = json.loads((case_dir / "frames.json").read_text())
        summary = json.loads((case_dir / "mitm-summary.json").read_text())
        (case_dir / "driver-stdout.log").write_text(stdout)
        (case_dir / "driver-stderr.log").write_text(stderr)
        return result, frames, summary, case_dir
    finally:
        # Every exit path (a timeout SelftestError, a Ctrl+C between
        # communicate() and the reads, any read error) still reaps the driver
        # group bounded; a no-op once communicate() already reaped it.
        terminate_group(process)
        close_mitm_pipes(process)


def case_paths(name, out_root):
    case_dir = out_root / name
    return case_dir, case_dir / "mock.log"


def install_termination_handler():
    def terminate(_signum, _frame):
        raise KeyboardInterrupt('#969 runner: termination requested')
    signal.signal(signal.SIGTERM, terminate)


def main(argv):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", help="evidence output directory (must not exist)")
    parser.add_argument("--cases", nargs="*", help="restrict to named cases like text-drop")
    args = parser.parse_args(argv)
    check("PDRS_BASELINE" in os.environ, "#969 selftest: set PDRS_BASELINE to the read-only TS baseline root")
    baseline = Path(os.environ["PDRS_BASELINE"])
    check((baseline / "src/cli/pilotdeck-agent-loop-sidecar.ts").is_file(),
          f"#969 selftest: PDRS_BASELINE does not look like the TS baseline: {baseline}")
    node_version = subprocess.run(["node", "--version"], capture_output=True, text=True, timeout=10)
    check(node_version.stdout.strip() == "v22.22.0",
          f"#969 selftest: requires Node v22.22.0, found {node_version.stdout.strip()}")

    out_root = Path(args.output).resolve()
    check(not out_root.exists(), f"#969 selftest: output directory must not exist: {out_root}")
    out_root.mkdir(parents=True)

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
        # The three direct middleman checks run on every invocation (also when
        # --cases restricts the matrix): they are part of the ruler.
        mitm_checks = {}
        for check_name, check_fn in (("mitm-pipe-close", verify_mitm_pipe_close),
                                     ("mitm-eof-reap", verify_mitm_eof_reap),
                                     ("mitm-eof-reap-group", verify_mitm_eof_reap_group)):
            try:
                outcome = check_fn(out_root)
                mitm_checks[check_name] = "ok" if outcome is None else outcome
            except Exception as error:
                # A broken middleman is a failing check with a dedicated
                # message, never a crash of the whole selftest (attack E3).
                message = (str(error) if isinstance(error, SelftestError)
                           else f"#969 {check_name}: check raised "
                                f"{type(error).__name__}: {error}")
                failures.append(message)
                print(f"FAIL {check_name}: {message}", file=sys.stderr, flush=True)
                mitm_checks[check_name] = message
        for scenario, injection in cells:
            name = f"{scenario}-{injection}"
            mock_port = free_port()
            check(mock_port != 18789, "#969 selftest: forbidden port")
            case_dir, mock_log = case_paths(name, out_root)
            check(not case_dir.exists(), f"#969 {name}: case dir already exists: {case_dir}")
            case_dir.mkdir(parents=True)
            if injection != "passthrough":
                (case_dir / "plan.json").write_text(json.dumps(INJECTION_PLANS[injection]) + "\n")
            mock_env = os.environ.copy()
            mock_env.update({
                "LIVE_E2E_MOCK_PORT": str(mock_port),
                "LIVE_E2E_MOCK_LOG": str(mock_log),
                "LIVE_E2E_INJECTION": injection,
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
                        stderr = mock.communicate(timeout=MOCK_REAP_SECONDS)[1]
                        raise SelftestError(f"#969 {name}: mock provider exited at startup: {stderr.strip()}")
                    if mock_log.exists() and "listening" in mock_log.read_text():
                        break
                    check(time.monotonic() < deadline, f"#969 {name}: mock provider did not become ready")
                    time.sleep(0.05)
                cell = verify_caught(name, scenario, injection, mock_port,
                                     out_root, case_dir, mock_log)
                if cell.get("verified") is not True:
                    failures.append(cell["failed"])
                    print(f"FAIL {name}: {cell['failed']}", file=sys.stderr, flush=True)
                matrix.append(cell)
            finally:
                terminate_group(mock)
            check(port_free(mock_port),
                  f"#969 {name}: mock provider port {mock_port} was not released after reaping")
    finally:
        home_touched, home_unreadable = [], []
        write_report(out_root, matrix, failures, time.monotonic() - started,
                     home_touched, home_unreadable, mitm_checks)
    if failures:
        print(f"#969 selftest: {len(failures)} failing assertion(s) of {len(cells)} cases", file=sys.stderr)
        return 1
    print(f"#969 selftest: {len(cells)} cases passed; evidence in {out_root}")
    return 0


def verify_caught(name, scenario, injection, mock_port, out_root, case_dir, mock_log):
    """Run verify_case; any failure becomes one failed cell with verified:false.

    Malformed driver output (missing keys, wrong shapes, broken JSON) is a
    failed cell with its own ``#969`` message, never a crash of the whole
    selftest: the README contract is that any mismatch ends in
    ``verified:false``, and attack_suite.py binds to exactly that.
    """
    try:
        return verify_case(name, scenario, injection, mock_port, out_root,
                           case_dir, mock_log)
    except (SelftestError, DeadlineExceeded) as error:
        return {"case": name, "failed": str(error), "verified": False}
    except Exception as error:
        return {"case": name,
                "failed": f"#969 {name}: case verification raised "
                          f"{type(error).__name__}: {error}",
                "verified": False}


def verify_case(name, scenario, injection, mock_port, out_root, case_dir, mock_log):
    result, frames, summary, _ = run_case(name, scenario, injection, mock_port, case_dir)
    from evidence import observation, load_expected, instantiate
    actual = observation(result, frames, summary, mock_log, case_dir)
    expected = instantiate(load_expected(name), case_dir, mock_port)
    assert_strict(name, "complete evidence", actual, expected)
    return {"case": name, "verified": True}


def spawn_mitm(log_path, summary_path, child_command):
    """Spawn the real middleman in its own session, like the driver does."""
    error_log = open(log_path.parent / "mitm-stderr.log", "w+")
    process = subprocess.Popen(
        [sys.executable, str(FIXTURE / "stdio_mitm.py"),
         "--log", str(log_path), "--summary", str(summary_path),
         "--", *child_command],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=error_log,
        text=True, start_new_session=True)
    process.error_log = error_log
    return process


def close_mitm_pipes(process):
    for stream in (process.stdin, process.stdout, getattr(process, 'error_log', None)):
        if stream is not None and not stream.closed:
            stream.close()


def mitm_stderr(process):
    process.error_log.seek(0)
    return process.error_log.read().strip()


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
        deadline = time.monotonic() + MITM_CHECK_SECONDS
        forwarded = Lines(process.stdout).readline(deadline, name)
        check(forwarded.strip() and strict_mismatch(json.loads(forwarded)["sequence"], 0, "sequence") is None,
              f"#969 {name}: MITM must forward the pre-close frame to the host: {forwarded!r}")
        process.stdout.close()  # the host goes away mid-stream
        gate.write_text("go\n")
        process.stdin.close()  # host-side EOF; the child exits on its own
        try:
            process.wait(timeout=max(0.001, deadline - time.monotonic()))
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
        close_mitm_pipes(process)


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
        deadline = time.monotonic() + MITM_CHECK_SECONDS
        forwarded = Lines(process.stdout).readline(deadline, name)
        check(forwarded.strip() and strict_mismatch(json.loads(forwarded)["sequence"], 0, "sequence") is None,
              f"#969 {name}: MITM must forward the child's frame to the host: {forwarded!r}")
        started = time.monotonic()
        process.stdin.close()  # the host disappears; the child never exits alone
        try:
            process.wait(timeout=max(0.001, deadline - time.monotonic()))
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
        close_mitm_pipes(process)


def verify_mitm_eof_reap_group(out_root):
    """A descendant that outlives a TERM-obedient leader must not block the reap.

    The leader emits its frame, spawns a TERM-ignoring descendant that holds
    the output pipe, and exits on its own: the leader's exit proves nothing
    about the group. After host-side stdin EOF the middleman must still sweep
    the whole group (grace, TERM, KILL), so the blocked reader ends, both
    frames are recorded, and the summary is written — all bounded.
    """
    name = "mitm-eof-reap-group"
    check_dir = out_root / name
    check_dir.mkdir(parents=True)
    log = check_dir / "frames.jsonl"
    summary = check_dir / "mitm-summary.json"
    pids = check_dir / "pids.json"
    descendant = check_dir / "descendant.py"
    script = check_dir / "child.py"
    descendant.write_text(textwrap.dedent(f"""
        import json, signal, sys, time
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
        sys.stdout.write(json.dumps({{"kind": "event", "sequence": 1, "final": False}}) + "\\n")
        sys.stdout.flush()
        time.sleep(300)
    """))
    script.write_text(textwrap.dedent(f"""
        import json, os, subprocess, sys
        from pathlib import Path
        sys.stdout.write(json.dumps({{"kind": "event", "sequence": 0, "final": False}}) + "\\n")
        sys.stdout.flush()
        child = subprocess.Popen([sys.executable, "-B", {str(descendant)!r}])
        Path({str(pids)!r}).write_text(
            json.dumps({{"leader": os.getpid(), "descendant": child.pid}}) + "\\n")
    """))
    process = spawn_mitm(log, summary, [sys.executable, str(script)])
    group = None
    try:
        deadline = time.monotonic() + MITM_CHECK_SECONDS
        # Both frames must be forwarded before the host disappears: the
        # descendant prints only after installing its TERM handler, so this
        # also proves the group sweep below fights a TERM-immune process.
        reader = Lines(process.stdout)
        for expected in (0, 1):
            forwarded = reader.readline(deadline, name)
            check(forwarded.strip() and strict_mismatch(json.loads(forwarded)["sequence"], expected, "sequence") is None,
                  f"#969 {name}: MITM must forward frame {expected} to the host: {forwarded!r}")
        while not pids.exists():
            check(time.monotonic() < deadline,
                  f"#969 {name}: leader must record its descendant before exiting")
            time.sleep(0.01)
        identified = json.loads(pids.read_text())
        # The descendant inherited the leader's process group (the leader was
        # itself started in its own session by the middleman).
        group = identified["leader"]
        started = time.monotonic()
        process.stdin.close()  # host EOF; the leader is already gone
        while pid_alive(identified["descendant"]):
            check(time.monotonic() < deadline,
                  f"#969 {name}: MITM must kill the TERM-ignoring descendant that "
                  f"outlives the leader (still alive after {MITM_CHECK_SECONDS}s)")
            time.sleep(0.05)
        elapsed = time.monotonic() - started
        try:
            process.wait(timeout=max(0.001, deadline - time.monotonic()))
        except subprocess.TimeoutExpired:
            raise SelftestError(
                f"#969 {name}: MITM must exit after sweeping the descendant that "
                f"outlives the leader (timed out after {MITM_CHECK_SECONDS}s)")
        check(process.returncode == 0,
              f"#969 {name}: MITM must exit cleanly after the group reap: "
              f"rc={process.returncode}: {mitm_stderr(process)}")
        records = read_frame_log(log)
        check(len(records) == 2,
              f"#969 {name}: MITM must record both the leader's and the descendant's "
              f"frames around the reap: {[record['frame']['sequence'] for record in records]}")
        check(summary.is_file(), f"#969 {name}: MITM must write its summary on the group reap path")
        return {"elapsedSeconds": round(elapsed, 1)}
    finally:
        # The descendant lives in the leader's group, which a signal to the
        # middleman's own group can never reach; sweep it directly.
        if group is not None:
            try:
                os.killpg(group, signal.SIGKILL)
            except (ProcessLookupError, PermissionError):
                pass
        terminate_group(process)
        close_mitm_pipes(process)


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


def write_report(out_root, matrix, failures, elapsed, home_touched, home_unreadable, mitm_checks):
    # The README contract: every case cell — passing or failing — states its
    # verdict explicitly; a failure without verified:false is a broken report.
    for entry in matrix:
        check("verified" in entry,
              f"#969 selftest: report case {entry.get('case')} must carry an "
              f"explicit verified field")
    report = {
        "cases": matrix,
        "mitmChecks": mitm_checks,
        "failures": failures,
        "elapsedSeconds": round(elapsed, 1),
        "node": subprocess.run(["node", "--version"], capture_output=True, text=True, timeout=10).stdout.strip(),
        "python": sys.version.split()[0],
        "homePilotdeckWrittenDuringRun": home_touched,
        "homePilotdeckUnreadableDuringRun": home_unreadable,
    }
    (out_root / "report.json").write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
    for entry in matrix:
        print(f"{entry.get('case')}: {'ok' if entry.get('verified') else 'FAILED'}")


if __name__ == "__main__":
    install_termination_handler()
    sys.exit(main(sys.argv[1:]))
