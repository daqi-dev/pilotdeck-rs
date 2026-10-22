"""Bounded process ownership shared by the gateway negative controls.

Every invocation owns a fresh session, so a timeout can kill the whole tree.
Collection after that kill is bounded too: a descendant that called setsid()
escapes the process group and can keep the inherited pipes open, which is why
a plain ``with Popen(...)`` plus ``communicate()`` can block forever even after
the leader is dead (#528).
"""
import os
import signal
import subprocess
import time

# Upper bound for collecting a killed tree. Kept short on purpose: the only
# writer that can still hold the pipes is a setsid() descendant that the group
# signal cannot reach.
KILL_TIMEOUT = 10


def _kill_group(process):
    for attempt in range(2):
        try:
            os.killpg(process.pid, signal.SIGKILL)
            return
        except ProcessLookupError:
            return
        except PermissionError:
            # macOS sandbox can report EPERM for an unreaped exited leader.
            # Reap it, then retry the group signal: descendants must still be
            # killed even when their leader has exited.
            if attempt or process.poll() is None:
                raise


def _as_text(process, value):
    # TimeoutExpired.output is built from os.read() chunks, so it stays bytes
    # even in text mode; normalize it like communicate() would (#528).
    if value is None:
        return ""
    if isinstance(value, bytes):
        encoding = getattr(process.stdout, "encoding", None) or "utf-8"
        errors = getattr(process.stdout, "errors", None) or "replace"
        return value.decode(encoding, errors)
    return value


def _collect(process, timeout):
    try:
        return process.communicate(timeout=timeout)
    except subprocess.TimeoutExpired as error:
        # A setsid() descendant escaped the group and holds the pipes, so EOF
        # never arrives. Stop reading and reap the already-killed leader;
        # keep whatever output the bounded read observed.
        for stream in (process.stdout, process.stderr):
            if stream is not None:
                stream.close()
        try:
            process.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            pass
        return _as_text(process, error.output), _as_text(process, error.stderr)


def _prefer_more(first, second):
    # communicate() keeps accumulating across retries, so the longer string is
    # the one that saw more bytes; never drop an earlier partial read.
    first = first or ""
    second = second or ""
    return second if len(second) > len(first) else first


def run_owned(command, *, env=None, timeout=900, cwd=None, kill_timeout=KILL_TIMEOUT):
    # cargo can leave test binaries (and inherited output pipes) behind when
    # killed alone. Do not use `with Popen(...)`: its __exit__ waits forever on
    # a tree that only the group signal can reach.
    process = subprocess.Popen(command, env=env, cwd=cwd,
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                               text=True, start_new_session=True)
    try:
        stdout, stderr = process.communicate(timeout=timeout)
    except BaseException as error:
        _kill_group(process)
        stdout, stderr = _collect(process, kill_timeout)
        if isinstance(error, subprocess.TimeoutExpired):
            error.output = _prefer_more(_as_text(process, error.output), stdout)
            error.stderr = _prefer_more(_as_text(process, error.stderr), stderr)
        raise
    return subprocess.CompletedProcess(command, process.returncode, stdout, stderr)


def wait_for_slot(script, *, timeout=900, interval=30):
    deadline = time.monotonic() + timeout
    stdout, stderr = [], []
    command = ["bash", str(script)]
    while True:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise subprocess.TimeoutExpired(command, timeout,
                                            "".join(stdout), "".join(stderr))
        result = run_owned(command, timeout=remaining)
        stdout.append(result.stdout)
        stderr.append(result.stderr)
        if result.returncode == 0:
            return subprocess.CompletedProcess(command, 0, "".join(stdout), "".join(stderr))
        if result.returncode != 1:
            result.check_returncode()
        time.sleep(min(interval, max(0, deadline - time.monotonic())))
