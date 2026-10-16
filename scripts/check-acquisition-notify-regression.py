#!/usr/bin/env python3
"""CI-only actual-Tap negative control; always restore the exact tracked source."""
import hashlib
import json
import os
from pathlib import Path
import re
import signal
import subprocess
import sys
import tempfile
import time

SOURCE = "src/pilotdeck-gateway/src/pairing/result_acquisition.rs"
TEST = "pairing::result_acquisition::notify_controls::actual_tap_pending_bypass_cannot_steal_admission_barrier_wake"
MARKER = "R80_SHARED_NOTIFY_ROUTE_WAKE_STOLEN"
MUTATIONS = (
    (b"state.bypass_changed.notify_one();", b"state.changed.notify_one();"),
    (b"let changed = state.bypass_changed.notified();", b"let changed = state.changed.notified();"),
)
CAP = 1024 * 1024
DEADLINE = 180


def git(root, *args):
    return subprocess.run(
        ["git", "--no-replace-objects", *args], cwd=root,
        check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    ).stdout


def emit_raw(stream, label):
    """Emit original child bytes, with explicit framing and any truncation."""
    stream.flush()
    size = os.fstat(stream.fileno()).st_size
    stream.seek(0)
    raw = stream.read(CAP)
    sys.stdout.flush()
    output = sys.stdout.buffer
    output.write((f"\nR80_RAW_{label}_BEGIN bytes={len(raw)} observed={size} cap={CAP}\n").encode())
    output.write(raw)
    output.write((f"\nR80_RAW_{label}_END truncated={str(size > CAP).lower()}\n").encode())
    output.flush()


def run_bounded(argv, root):
    """Sample runtime/output limits; preserve bounded raw even on failure."""
    with tempfile.TemporaryFile() as out, tempfile.TemporaryFile() as err:
        child = subprocess.Popen(argv, cwd=root, stdout=out, stderr=err, start_new_session=True)
        started = time.monotonic()
        try:
            while True:
                if os.fstat(out.fileno()).st_size > CAP or os.fstat(err.fileno()).st_size > CAP:
                    raise RuntimeError("negative-control output limit")
                if time.monotonic() - started > DEADLINE:
                    raise RuntimeError("negative-control timeout; not a detected regression")
                code = child.poll()
                if code is not None:
                    break
                time.sleep(0.05)
            out.seek(0)
            err.seek(0)
            stdout, stderr = out.read(CAP + 1), err.read(CAP + 1)
            if len(stdout) > CAP or len(stderr) > CAP:
                raise RuntimeError("negative-control output limit")
            return code, stdout, stderr
        finally:
            try:
                try:
                    # A live or residual group belongs only to this new session.
                    os.killpg(child.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                except PermissionError as exc:
                    # A reaped leader does not prove that its group is empty.
                    if child.poll() is None:
                        child.kill()
                    raise RuntimeError("negative-control group cleanup EPERM; infrastructure failure") from exc
                finally:
                    child.wait()
            finally:
                # Read these files before closing them on every exit path,
                # including timeouts, overflow, cleanup and semantic failures.
                emit_raw(out, "STDOUT")
                emit_raw(err, "STDERR")


def verify_failure(code, stdout, stderr):
    out, err = stdout.decode("utf-8", "strict"), stderr.decode("utf-8", "strict")
    status = f"test {TEST} ... FAILED"
    summary = re.fullmatch(
        r"test result: FAILED\. 0 passed; 1 failed; 0 ignored; 0 measured; \d+ filtered out; finished in [^\n]+",
        next((line for line in out.splitlines() if line.startswith("test result:")), ""),
    )
    # Compile/infra errors and another assertion are never evidence of detection.
    valid = (
        code == 101
        and out.splitlines().count("running 1 test") == 1
        and out.splitlines().count(status) == 1
        and len([line for line in out.splitlines() if line.startswith("test result:")]) == 1
        and summary is not None
        and err.count(MARKER) == 1
        and re.search(r"assertion `left == right` failed: " + MARKER + r"\n\s*left: 0\n\s*right: 1(?:\n|$)", err) is not None
        and "could not compile" not in err
        and re.search(r"error\[E\d+\]", err) is None
        and len(re.findall(r"thread '[^\n]+'(?: \(\d+\))? panicked at", err)) == 1
    )
    if not valid:
        raise RuntimeError("negative control did not execute exactly one test with the designated route-wake assertion")
    return ["running 1 test", status, summary.group(0), *[
        line for line in err.splitlines()
        if MARKER in line or re.fullmatch(r"\s*(left: 0|right: 1)", line)
    ]]


def check(root, runner=run_bounded):
    path = root / SOURCE
    original = path.read_bytes()
    blob = git(root, "show", f"HEAD:{SOURCE}")
    if original != blob:
        raise RuntimeError("source differs from HEAD before mutation")
    git(root, "diff", "--exit-code", "HEAD", "--")
    mutant = original
    for before, after in MUTATIONS:
        if mutant.count(before) != 1:
            raise RuntimeError("negative-control exact source anchor count changed")
        mutant = mutant.replace(before, after, 1)
    original_sha = hashlib.sha256(original).hexdigest()
    try:
        path.write_bytes(mutant)
        code, stdout, stderr = runner([
            "cargo", "test", "--locked", "--manifest-path", "src/Cargo.toml", "-p", "pilotdeck-gateway",
            "--lib", TEST, "--", "--exact", "--nocapture", "--test-threads=1",
        ], root)
        evidence = verify_failure(code, stdout, stderr)
    finally:
        path.write_bytes(original)
        restored = path.read_bytes()
        if restored != original or restored != git(root, "show", f"HEAD:{SOURCE}"):
            raise RuntimeError("negative-control source restoration failed")
        git(root, "diff", "--exit-code", "HEAD", "--")
        print(json.dumps({"sourceRestored": True, "sha256": original_sha}), flush=True)
    for line in evidence:
        print(line)
    print("R80_ACTUAL_SHARED_NOTIFY_MUTANT_REJECTED")


def interrupted(signum, _frame):
    raise RuntimeError(f"negative control interrupted by signal {signum}")


if __name__ == "__main__":
    signal.signal(signal.SIGTERM, interrupted)
    check(Path(__file__).resolve().parents[1])
