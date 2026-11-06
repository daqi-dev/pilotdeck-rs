#!/usr/bin/env python3
"""Force the descendant watchdog's pre-install window (#1779).

The `grandchild-*` sleeper is handed the pid it expects to outlive. If it
sampled `os.getppid()` instead, a parent that died before the sleeper was first
scheduled would have been reaped already, the sampling would record the reaper
as the baseline, and the watch would never fire for the death it exists to
catch. This script makes that ordering happen every time: a middle process
spawns the sleeper with `preexec_fn` SIGKILLing itself, and the sleeper waits
to be reparented before it execs -- so the death is complete, not merely
requested, by the time the watchdog installs.

It reports only whether the sleeper's own parent-death receipt appeared, so the
caller needs no process scanning and no timing luck: a watchdog that watched
the wrong pid never writes one and the sleeper just sleeps out its 60 s.
"""
import ast
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
# The healthy case arming the watch takes ~0.1 s; the bound only decides how
# long a broken watchdog keeps the test waiting before it fails.
RECEIPT_LIMIT = 5.0

MIDDLE = r'''
import os, signal, subprocess, sys, time
program, armed, receipt, marker = sys.argv[1:5]
def die_before_the_child_execs():
    parent = os.getppid()
    os.kill(parent, signal.SIGKILL)
    # Sending SIGKILL only queues it: the sleeper could still reach `exec` while
    # the middle is alive, and a watchdog that sampled its own parent at install
    # would then record the right pid and the probe would pass for the wrong
    # reason. Waiting for the kernel to reparent us makes the ordering the test
    # claims -- we are only reparented once the parent is gone -- a fact instead
    # of a race we happen to win (#1779 review).
    deadline = time.monotonic() + 5.0
    while os.getppid() == parent and time.monotonic() < deadline:
        time.sleep(0.0005)
    if os.getppid() == parent:
        with open(marker, 'w') as out:
            out.write(str(parent))
        os._exit(66)
subprocess.Popen(
    [sys.executable, '-c', program, str(os.getpid()), armed, receipt],
    preexec_fn=die_before_the_child_execs)
os._exit(0)
'''


def program():
    """The real `-c` program, read out of `fixture.py` rather than retyped."""
    source = (HERE / 'fixture.py').read_text()
    for node in ast.parse(source).body:
        target = getattr(node, 'targets', [None])[0]
        if isinstance(node, ast.Assign) and getattr(target, 'id', None) == '_GRANDCHILD_WATCH':
            return ast.literal_eval(node.value).format(str(HERE))
    raise SystemExit('preinstall_probe: _GRANDCHILD_WATCH not found in fixture.py')


def main():
    work = Path(tempfile.mkdtemp(prefix='a6-preinstall-'))
    armed = work / 'sleeper.armed'
    descendant = work / 'sleeper.descendant.txt'
    reason = descendant.with_suffix('.orphan')
    middle = work / 'middle.py'
    marker = work / 'preexec-timeout'
    middle.write_text(MIDDLE)
    with open(work / 'sleeper.stderr', 'w') as err:
        middle_status = subprocess.run(
            [sys.executable, str(middle), program(), str(armed), str(descendant), str(marker)],
            # The sleeper inherits these. Leave stdout on the caller's pipe and a
            # broken watchdog keeps the caller waiting for its full 60 s sleep
            # even though this script already gave up.
            stdout=subprocess.DEVNULL, stderr=err, timeout=30).returncode
    print('middle exited with {} (killed by its own child: {})'.format(
        middle_status, middle_status < 0))
    if marker.exists():
        print('the sleeper never exec\'d: the middle (pid {}) was still alive after 5 s'.format(
            marker.read_text().strip()))
        print('artifacts kept in {}'.format(work))
        return 2
    deadline = time.monotonic() + RECEIPT_LIMIT
    waited = time.monotonic()
    while not reason.exists() and time.monotonic() < deadline:
        time.sleep(0.01)
    print('sleeper stderr: {}'.format((work / 'sleeper.stderr').read_text().strip() or '(empty)'))
    if reason.exists():
        print('parent-death receipt after {:.0f} ms: {}'.format(
            (time.monotonic() - waited) * 1000, reason.read_text().strip()))
        shutil.rmtree(work, ignore_errors=True)
        return 0
    print('NO parent-death receipt within {:.0f}s: the sleeper watched the wrong pid'.format(
        RECEIPT_LIMIT))
    print('the sleeper is orphaned by design here: it exits on its own within its 60 s sleep')
    print('artifacts kept in {}'.format(work))
    return 1


if __name__ == '__main__':
    sys.exit(main())
