"""Bounded process ownership for the connection-context negative controls."""
import os
import signal
import subprocess
import time


def run_owned(command, *, env=None, timeout=900):
    # cargo can leave test binaries (and inherited output pipes) behind when
    # killed alone. Every invocation owns a fresh process group, including gate.
    with subprocess.Popen(command, env=env, stdout=subprocess.PIPE,
                          stderr=subprocess.PIPE, text=True,
                          start_new_session=True) as process:
        try:
            stdout, stderr = process.communicate(timeout=timeout)
        except BaseException as error:
            for attempt in range(2):
                try:
                    os.killpg(process.pid, signal.SIGKILL)
                    break
                except ProcessLookupError:
                    break
                except PermissionError:
                    # macOS sandbox can report EPERM for an unreaped exited
                    # leader. Reap it, then retry the group signal: descendants
                    # must still be killed even when their leader has exited.
                    if attempt or process.poll() is None:
                        raise
            stdout, stderr = process.communicate()
            if isinstance(error, subprocess.TimeoutExpired):
                error.output, error.stderr = stdout, stderr
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
