#!/usr/bin/env python3
"""Force the descendant watchdog's pre-install window (#1779).

The `grandchild-*` sleeper is handed the pid it expects to outlive. If it
sampled `os.getppid()` instead, a parent that died before the sleeper was first
scheduled would have been reaped already, the sampling would record the reaper
as the baseline, and the watch would never fire for the death it exists to
catch. This script makes that ordering happen every time: a middle process
spawns the sleeper with `preexec_fn` SIGKILLing itself before the sleeper
execs.

It reports only whether the sleeper's own parent-death receipt appeared, so the
caller needs no process scanning and no timing luck: a watchdog that watched
the wrong pid never writes one and the sleeper just sleeps out its 60 s.
"""
import ast
import os
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
import os, signal, subprocess, sys
program, armed, receipt = sys.argv[1:4]
def die_before_the_child_execs():
    os.kill(os.getppid(), signal.SIGKILL)
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
    middle.write_text(MIDDLE)
    with open(work / 'sleeper.stderr', 'w') as err:
        middle_status = subprocess.run(
            [sys.executable, str(middle), program(), str(armed), str(descendant)],
            # The sleeper inherits these. Leave stdout on the caller's pipe and a
            # broken watchdog keeps the caller waiting for its full 60 s sleep
            # even though this script already gave up.
            stdout=subprocess.DEVNULL, stderr=err, timeout=30).returncode
    print('middle exited with {} (killed by its own child: {})'.format(
        middle_status, middle_status < 0))
    deadline = time.monotonic() + RECEIPT_LIMIT
    waited = time.monotonic()
    while not reason.exists() and time.monotonic() < deadline:
        time.sleep(0.01)
    print('sleeper stderr: {}'.format((work / 'sleeper.stderr').read_text().strip() or '(empty)'))
    if reason.exists():
        print('parent-death receipt after {:.0f} ms: {}'.format(
            (time.monotonic() - waited) * 1000, reason.read_text().strip()))
        return 0
    print('NO parent-death receipt within {:.0f}s: the sleeper watched the wrong pid'.format(
        RECEIPT_LIMIT))
    print('artifacts kept in {}'.format(work))
    return 1


if __name__ == '__main__':
    sys.exit(main())
