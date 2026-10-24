#!/usr/bin/env python3
"""Mutation negative controls for the model-complete TS oracle.

Each control mutates this worktree only, compiles the integration test, and
requires a real assertion failure ("test result: FAILED") from the mutated
build. Compile errors do not count.

Recovery protocol (concurrency P2):
- Before any mutation the original bytes and their SHA-256 are persisted to a
  state directory *outside* the worktree, then the source is replaced
  atomically. A SIGTERM/SIGINT/SIGHUP handler restores the original
  synchronously and exits; SIGKILL cannot run a handler, so the next invocation
  refuses to run while the source still hashes to the recorded mutated bytes
  (or to an unknown value). `--recover` restores only a value that matches the
  recorded mutation; unknown edits are never overwritten.
- The cargo slot check runs *before* the mutation, waits a bounded time, and
  uses the same process-name criterion as scripts/cargo-slots.sh. `ps` is the
  primary probe; when the sandbox denies `ps`, `pgrep -x` for the known cargo
  entry points is the fallback, and a failure of both probes is fatal (never
  treated as zero).
- Cargo runs in its own process group; on timeout the whole group is signalled
  (SIGTERM, then SIGKILL) and reaped before the source is restored.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[5]
HTTP = ROOT / "src/pilotdeck-gateway/src/runtime/model/http.rs"
MANIFEST = ROOT / "src/Cargo.toml"
TARGET = os.environ.get("CARGO_TARGET_DIR", str(ROOT / "target"))
TEST = "native_complete_model_matches_real_ts_complete"
RETRY_TEST = "complete_429_is_retryable_but_is_not_retried"
TRANSPORT_TEST = "native_complete_transport_retry_matches_real_ts"

CARGO_CAP = int(os.environ.get("NEGCTL_CARGO_CAP", "4"))
SLOT_WAIT_SECONDS = float(os.environ.get("NEGCTL_SLOT_WAIT_SECONDS", "1800"))
TEST_TIMEOUT = float(os.environ.get("NEGCTL_TEST_TIMEOUT", "600"))
TERM_GRACE = float(os.environ.get("NEGCTL_TERM_GRACE", "10"))
STATE_DIR = Path(
    os.environ.get("NEGCTL_STATE_DIR")
    or (
        Path(tempfile.gettempdir())
        / ("pdrs-negctl-" + hashlib.sha256(str(ROOT).encode()).hexdigest()[:16])
    )
)
STATE_FILE = STATE_DIR / "state.json"
ORIGINAL_FILE = STATE_DIR / "original.http.rs"

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
        # Same-source third protocol: the refusal -> content_filter arm.
        "anthropic-refusal-finish-unknown",
        'Some("refusal") => "content_filter".into(),',
        'Some("refusal") => "unknown".into(),',
        TEST,
    ),
    (
        # The collision case now really reaches nextUniqueToolCallId's `_2`.
        "tool-id-suffix-starts-at-one",
        "            let mut suffix = 2u64;",
        "            let mut suffix = 1u64;",
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
    (
        # The TS bound is `attempt <= maxRetries`; this makes the inner retry
        # predicate inclusive too and changes the 1.5/1 cases from the pinned
        # request counts and exhausted-error shape.
        "complete-retry-inner-boundary-inclusive",
        "                if attempt < max_retries && is_retryable_request_error(&error) {",
        "                if attempt <= max_retries && is_retryable_request_error(&error) {",
        TRANSPORT_TEST,
    ),
]

_active: dict | None = None
_restoring = False


def sha256(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def atomic_write(path: Path, data: bytes) -> None:
    """Write `data` to `path` via a same-directory temp file + fsync + replace."""
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, tmp = tempfile.mkstemp(dir=str(path.parent), prefix=path.name + ".", suffix=".tmp")
    try:
        with os.fdopen(fd, "wb") as handle:
            handle.write(data)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(tmp, path)
        try:
            directory = os.open(str(path.parent), os.O_RDONLY)
            try:
                os.fsync(directory)
            finally:
                os.close(directory)
        except OSError:
            pass
    finally:
        if os.path.exists(tmp):
            os.unlink(tmp)


def load_state() -> dict | None:
    if not STATE_FILE.exists():
        return None
    try:
        return json.loads(STATE_FILE.read_text())
    except (OSError, ValueError) as error:
        raise SystemExit(f"negctl: unreadable recovery state {STATE_FILE}: {error}")


def clear_state() -> None:
    for path in (STATE_FILE, ORIGINAL_FILE):
        try:
            path.unlink()
        except FileNotFoundError:
            pass


def pid_alive(pid: int) -> bool:
    if pid <= 0:
        return False
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


def state_matches_this_worktree(state: dict) -> bool:
    return state.get("root") == str(ROOT) and state.get("http") == str(HTTP)


def startup_check() -> None:
    """Refuse to run on a mutation left by an unclean exit (e.g. SIGKILL)."""
    def refuse(message: str) -> None:
        print(f"negctl: {message}", file=sys.stderr)
        raise SystemExit(3)

    state = load_state()
    if state is None:
        return
    if not state_matches_this_worktree(state):
        refuse(f"recovery state {STATE_FILE} belongs to another worktree")
    pid = int(state.get("pid", 0))
    if pid != os.getpid() and pid_alive(pid):
        refuse(f"a recorded control run is still active (pid {pid})")
    current = sha256(HTTP.read_bytes()) if HTTP.exists() else ""
    if current == state.get("original_sha256"):
        print("negctl: clearing stale recovery state (source is already the original)")
        clear_state()
        return
    if current == state.get("mutated_sha256"):
        refuse(
            "source matches an UNRECOVERED mutation from pid "
            f"{pid} ({state.get('control')}); run --recover before mutating again"
        )
    refuse(
        "source hash matches neither the recorded original nor mutation; "
        "refusing to overwrite unknown changes"
    )


def begin_mutation(control: str, mutated: bytes) -> None:
    global _active
    original = HTTP.read_bytes()
    original_sha = sha256(original)
    mutated_sha = sha256(mutated)
    if original == mutated:
        raise SystemExit(f"negctl: {control}: mutation did not change the source")
    STATE_DIR.mkdir(parents=True, exist_ok=True)
    atomic_write(ORIGINAL_FILE, original)
    atomic_write(
        STATE_FILE,
        json.dumps(
            {
                "pid": os.getpid(),
                "root": str(ROOT),
                "http": str(HTTP),
                "control": control,
                "original_sha256": original_sha,
                "mutated_sha256": mutated_sha,
                "started_at": time.time(),
            },
            indent=1,
        ).encode(),
    )
    _active = {
        "original_sha256": original_sha,
        "mutated_sha256": mutated_sha,
        "control": control,
    }
    atomic_write(HTTP, mutated)


def restore_active() -> bool:
    """Restore only a source matching the recorded mutation or original."""
    global _active, _restoring
    if _active is None:
        return True
    if _restoring:
        return False
    _restoring = True
    try:
        current = sha256(HTTP.read_bytes())
        if current == _active["original_sha256"]:
            pass
        elif current == _active["mutated_sha256"]:
            original = ORIGINAL_FILE.read_bytes()
            if sha256(original) != _active["original_sha256"]:
                raise SystemExit("negctl: sidecar original SHA-256 mismatch; refusing")
            atomic_write(HTTP, original)
            if sha256(HTTP.read_bytes()) != _active["original_sha256"]:
                raise SystemExit("negctl: restore SHA-256 mismatch")
        else:
            print(
                "negctl: source changed to an unknown hash; refusing to overwrite it",
                file=sys.stderr,
            )
            return False
        clear_state()
        _active = None
        return True
    finally:
        _restoring = False


def install_signal_handlers() -> None:
    def handler(signum: int, _frame) -> None:
        try:
            restore_active()
        except BaseException as error:  # noqa: BLE001 - best effort before exit
            print(f"negctl: signal {signum} restore failed: {error}", file=sys.stderr)
        finally:
            os._exit(128 + signum)

    for signum in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
        signal.signal(signum, handler)


def slots_from_ps() -> tuple[int | None, str]:
    """Same criterion as scripts/cargo-slots.sh: first command word is cargo."""
    try:
        result = subprocess.run(["ps", "-axo", "command="], capture_output=True, text=True)
    except OSError as error:
        return None, f"ps failed: {error}"
    if result.returncode != 0:
        return None, f"ps exit {result.returncode}: {result.stderr.strip()}"
    count = 0
    for line in result.stdout.splitlines():
        parts = line.split()
        if parts and re.fullmatch(r"(?:.*/)?cargo(?:-[a-z]+)?", parts[0]):
            count += 1
    return count, ""


def slots_from_pgrep() -> tuple[int | None, str]:
    """Fallback when `ps` is denied: count known cargo entry-point names."""
    names = [
        "cargo",
        "cargo-clippy",
        "cargo-fmt",
        "cargo-test",
        "cargo-build",
        "cargo-check",
        "cargo-run",
        "cargo-bench",
        "cargo-doc",
    ]
    total = 0
    for name in names:
        try:
            result = subprocess.run(["pgrep", "-x", name], capture_output=True, text=True)
        except OSError as error:
            return None, f"pgrep -x {name} failed: {error}"
        if result.returncode == 0:
            total += len([line for line in result.stdout.splitlines() if line.strip()])
        elif result.returncode != 1:
            return None, f"pgrep -x {name} exit {result.returncode}: {result.stderr.strip()}"
    return total, ""


def cargo_slots() -> tuple[int, str, str]:
    # Documented gate; under a denied `ps` it misreports 0, so its output is
    # logged but never trusted as the count.
    script = subprocess.run(
        ["bash", "scripts/cargo-slots.sh", str(CARGO_CAP)],
        cwd=ROOT,
        capture_output=True,
        text=True,
    )
    note = (script.stdout + script.stderr).strip()
    count, error = slots_from_ps()
    source = "ps"
    if count is None:
        count, error = slots_from_pgrep()
        source = "pgrep"
    if count is None:
        raise SystemExit(
            "negctl: cannot count cargo slots (ps denied and pgrep failed: "
            f"{error}); refusing to mutate"
        )
    return count, source, note


def wait_for_slot() -> None:
    deadline = time.monotonic() + SLOT_WAIT_SECONDS
    while True:
        count, source, note = cargo_slots()
        if count < CARGO_CAP:
            print(f"negctl: cargo slot free: {count}/{CARGO_CAP} (probe={source}, {note})")
            return
        if time.monotonic() >= deadline:
            raise SystemExit(
                f"negctl: cargo slots stayed full ({count}/{CARGO_CAP}) for "
                f"{SLOT_WAIT_SECONDS}s; refusing to mutate"
            )
        time.sleep(10)


def _terminate_group(proc: subprocess.Popen) -> str:
    output = ""
    try:
        os.killpg(proc.pid, signal.SIGTERM)
    except ProcessLookupError:
        pass
    try:
        output = proc.communicate(timeout=TERM_GRACE)[0] or ""
    except subprocess.TimeoutExpired:
        try:
            os.killpg(proc.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        try:
            output = proc.communicate(timeout=TERM_GRACE)[0] or ""
        except subprocess.TimeoutExpired:
            output = ""
    return output


def run_bounded(cmd: list[str], timeout: float, env: dict | None = None) -> tuple[int, str, bool]:
    """Run `cmd` in its own process group; on timeout kill and reap the group."""
    proc = subprocess.Popen(
        cmd,
        cwd=ROOT,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        start_new_session=True,
    )
    try:
        output = proc.communicate(timeout=timeout)[0] or ""
        return proc.returncode, output, False
    except subprocess.TimeoutExpired:
        output = _terminate_group(proc)
        return 124, output + f"\n[negctl] timed out after {timeout}s; process group terminated\n", True


def run_test(test_name: str) -> tuple[int, str]:
    # The cargo slot is checked before the mutation; do not mutate without it.
    command = [
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
    ]
    return_code, output, _ = run_bounded(
        command,
        TEST_TIMEOUT,
        env=dict(os.environ, CARGO_TARGET_DIR=TARGET),
    )
    return return_code, output


def check_state_command() -> int:
    state = load_state()
    if state is None:
        print("negctl: clean (no recovery state)")
        return 0
    current = sha256(HTTP.read_bytes()) if HTTP.exists() else ""
    if current == state.get("original_sha256"):
        print("negctl: stale recovery state, source is original")
        return 0
    print(
        f"negctl: UNRECOVERED mutation control={state.get('control')} pid={state.get('pid')}"
    )
    return 3


def recover_command() -> int:
    state = load_state()
    if state is None:
        print("negctl: clean (no recovery state)")
        return 0
    if not state_matches_this_worktree(state):
        print("negctl: recovery state belongs to another worktree; refusing", file=sys.stderr)
        return 5
    pid = int(state.get("pid", 0))
    if pid != os.getpid() and pid_alive(pid):
        print(f"negctl: recorded control run still active (pid {pid}); refusing", file=sys.stderr)
        return 5
    current = sha256(HTTP.read_bytes()) if HTTP.exists() else ""
    if current == state.get("original_sha256"):
        clear_state()
        print("negctl: source already original; cleared recovery state")
        return 0
    if current != state.get("mutated_sha256"):
        print("negctl: source does not match the recorded mutation; refusing", file=sys.stderr)
        return 4
    original = ORIGINAL_FILE.read_bytes()
    if sha256(original) != state.get("original_sha256"):
        print("negctl: sidecar original SHA-256 mismatch; refusing", file=sys.stderr)
        return 6
    atomic_write(HTTP, original)
    if sha256(HTTP.read_bytes()) != state.get("original_sha256"):
        print("negctl: restored SHA-256 mismatch", file=sys.stderr)
        return 6
    clear_state()
    print("negctl: recovered original source from sidecar")
    return 0


def self_test_timeout() -> int:
    """Real process-group reclamation: kill a shell and its background child."""
    code, output, timed_out = run_bounded(
        ["sh", "-c", "sleep 30 & echo GRANDCHILD=$!; wait"], 1.0
    )
    print(output, end="")
    match = re.search(r"GRANDCHILD=(\d+)", output)
    if not match:
        print("negctl: timeout self-test did not observe the grandchild", file=sys.stderr)
        return 1
    pid = int(match.group(1))
    gone = False
    for _ in range(50):
        if not pid_alive(pid):
            gone = True
            break
        time.sleep(0.1)
    print(
        f"negctl: timeout self-test timed_out={timed_out} exit={code} grandchild={pid} gone={gone}"
    )
    return 0 if timed_out and gone else 1


def self_test_hold() -> int:
    """Mutate the real source then wait; a signal handler or --recover restores."""
    install_signal_handlers()
    startup_check()
    text = HTTP.read_text()
    anchor = "/// Normalizes Anthropic finish reason to canonical format."
    if text.count(anchor) != 1:
        raise SystemExit(f"negctl: hold anchor count {text.count(anchor)}")
    mutated = text.replace(anchor, anchor + " (negctl recovery hold)", 1)
    wait_for_slot()
    begin_mutation("self-test-hold", mutated.encode())
    print(f"HELD sha256={sha256(HTTP.read_bytes())}", flush=True)
    while True:
        time.sleep(1)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--recover", action="store_true", help="restore a recorded mutation")
    parser.add_argument("--check-state", action="store_true", help="report unrecovered state")
    parser.add_argument(
        "--self-test-timeout", action="store_true", help="prove process-group reclamation"
    )
    parser.add_argument(
        "--self-test-hold", action="store_true", help="hold a real mutation for signal tests"
    )
    args = parser.parse_args(argv)

    if args.check_state:
        return check_state_command()
    if args.recover:
        return recover_command()
    startup_check()
    if args.self_test_timeout:
        return self_test_timeout()
    if args.self_test_hold:
        return self_test_hold()

    install_signal_handlers()
    baseline_sha = sha256(HTTP.read_bytes())
    failures: list[str] = []
    for name, old, new, test_name in CONTROLS:
        text = HTTP.read_text()
        anchor_count = text.count(old)
        if anchor_count != 1:
            failures.append(f"{name}: mutation anchor count {anchor_count}")
            continue
        mutated = text.replace(old, new, 1)
        if mutated == text:
            failures.append(f"{name}: mutation did not change source")
            continue
        # Slot check must precede the mutation so a waiting script never leaves
        # mutated source visible to another cargo invocation in this worktree.
        wait_for_slot()
        begin_mutation(name, mutated.encode())
        try:
            code, output = run_test(test_name)
            caught = "test result: FAILED" in output and code != 0
            print(f"{name}: {'caught' if caught else 'NOT CAUGHT'} (exit {code})")
            if not caught:
                tail = "\n".join(output.splitlines()[-25:])
                print(tail)
                failures.append(f"{name}: mutated build did not fail the assertion")
        finally:
            restored = restore_active()
        if not restored:
            failures.append(f"{name}: restore refused; recovery state kept")
            break
    if sha256(HTTP.read_bytes()) != baseline_sha:
        failures.append("final source SHA-256 does not match the pre-run bytes")
    if failures:
        print("NEGATIVE CONTROLS FAILED:", *failures, sep="\n  ")
        return 1
    print(f"NEGATIVE CONTROLS PASS ({len(CONTROLS)}/{len(CONTROLS)})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
