#!/usr/bin/env python3
# anchor-selfcheck: unsupported bounded fixture process owner, no mutation definitions
"""Run one real TS cell with a deadline and reclaim its detached sidecar group."""
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time


def group_alive(group):
    try:
        os.killpg(group, 0)
    except ProcessLookupError:
        return False
    return True


def signal_group(group, number):
    try:
        os.killpg(group, number)
    except ProcessLookupError:
        pass


def read_identity(output):
    identity = json.loads((output / 'process.json').read_text())
    if (set(identity) != {'mitmPid', 'childPid'} or
            any(type(pid) is not int or pid <= 1 for pid in identity.values())):
        raise RuntimeError('#748 invalid process identity')
    return identity


def reclaim(process, output):
    # Ignore a second interrupt while reclaiming; even malformed evidence must
    # not skip the group whose identity comes directly from Popen.
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
    signal.signal(signal.SIGINT, signal.SIG_IGN)
    if process.poll() is None:
        process.terminate()
    # Let the surviving MITM observe driver EOF and reap its detached child.
    # This also covers a signal before the MITM has published process.json.
    deadline = time.monotonic() + 4
    while group_alive(process.pid) and time.monotonic() < deadline:
        process.poll()
        time.sleep(0.025)
    identity = None
    try:
        if (output / 'process.json').exists():
            identity = read_identity(output)
    except (OSError, ValueError, TypeError, RuntimeError) as error:
        print('#748 cleanup cannot read process identity: ' + str(error), file=sys.stderr)
    # The MITM normally completed its own reap above. An outer deadline can
    # still sweep the detached sidecar group using its atomically published ID.
    if identity:
        signal_group(identity['childPid'], signal.SIGKILL)
    signal_group(process.pid, signal.SIGKILL)
    process.communicate(timeout=5)
    groups = [process.pid] + ([identity['childPid']] if identity else [])
    deadline = time.monotonic() + 2
    while any(group_alive(group) for group in groups) and time.monotonic() < deadline:
        time.sleep(0.025)
    if any(group_alive(group) for group in groups):
        raise RuntimeError('#748 cleanup process group still alive')


def interrupted(number, _frame):
    # selectors.select retries InterruptedError as EINTR, swallowing that
    # exception while communicate() is blocked. A RuntimeError escapes it.
    raise RuntimeError('#748 capture interrupted by signal ' + str(number))


def main(driver=None):
    fixture = Path(__file__).resolve().parent
    scenario, output = sys.argv[1:]
    output = Path(output).resolve()
    output.mkdir(parents=True, exist_ok=False)
    baseline = Path(os.environ['PDRS_BASELINE'])
    env = dict(os.environ, HOME=str(output / 'home'))
    command = ['node', '--import', str(fixture / 'determinism.mjs'), '--import',
               str(baseline / 'node_modules/tsx/dist/loader.mjs'), str(driver or fixture / 'driver.mts'), scenario, str(output)]
    # Install before Popen so a signal cannot fall into a window between spawn
    # and ownership. Every operation after spawn is within the same finally.
    signal.signal(signal.SIGTERM, interrupted)
    signal.signal(signal.SIGINT, interrupted)
    process = None
    try:
        process = subprocess.Popen(command, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                   text=True, start_new_session=True)
        stdout, stderr = process.communicate(timeout=70)
        (output / 'stdout.log').write_text(stdout)
        (output / 'stderr.log').write_text(stderr)
        print(stdout, end='')
        print(stderr, end='', file=sys.stderr)
        if process.returncode:
            raise SystemExit(process.returncode)
        reaped = json.loads((output / 'reaped.json').read_text())
        if reaped != {'childReaped': True, 'groupAlive': False}:
            raise RuntimeError('#748 sidecar group not reaped')
        identity = read_identity(output)
        for pid in identity.values():
            try:
                os.kill(pid, 0)
            except ProcessLookupError:
                continue
            raise RuntimeError('#748 descendant still alive: ' + str(pid))
    finally:
        if process is not None:
            reclaim(process, output)


if __name__ == '__main__':
    main()
