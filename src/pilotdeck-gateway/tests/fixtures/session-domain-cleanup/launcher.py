#!/usr/bin/env python3
"""Launcher for session_domain cleanup controls.

The production runner starts the A5.2a launcher with start_new_session and then
waits.  This fixture reproduces the two interesting post-wait states without any
ps dependency: an empty group, a group with a sleep grandchild, and a grandchild
that ignores SIGTERM (to exercise the bounded SIGKILL escalation).

For ``ignore`` the grandchild installs ``signal.SIG_IGN`` and writes a ready
marker (temp file + os.replace); this launcher waits for that marker with a
10 s upper bound before it writes the pid/launch.json and exits.  Without the
handshake a correct implementation could be failed by scheduler delay: the
upper SIGTERM would arrive before the handler is installed and the grandchild
would die by default action.

It writes the same ``six-cells/launch.json`` shape the real launcher writes, so
the existing launcherAndFixtureReaped check is exercised unchanged.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import time

READY_TIMEOUT = 10


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--mode', choices=('clean', 'leak', 'ignore'), required=True)
    parser.add_argument('--child-pid-file', type=Path, required=True)
    args = parser.parse_args()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    grandchild = None
    ready = args.child_pid_file.with_name(args.child_pid_file.name + '.ready')
    if args.mode == 'leak':
        grandchild = subprocess.Popen(
            ['sleep', '60'], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    elif args.mode == 'ignore':
        code = ('import os,signal,sys,time\n'
                'signal.signal(signal.SIGTERM, signal.SIG_IGN)\n'
                'tmp = sys.argv[1] + ".tmp"\n'
                'open(tmp, "w").write("ready")\n'
                'os.replace(tmp, sys.argv[1])\n'
                'time.sleep(60)\n')
        grandchild = subprocess.Popen(
            [sys.executable, '-I', '-c', code, str(ready)],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        deadline = time.monotonic() + READY_TIMEOUT
        while not ready.is_file():
            if grandchild.poll() is not None:
                print(f'ignore grandchild exited before ready (rc={grandchild.returncode})',
                      file=sys.stderr)
                return 3
            if time.monotonic() >= deadline:
                print(f'ignore grandchild did not signal SIG_IGN ready within '
                      f'{READY_TIMEOUT}s', file=sys.stderr)
                try:
                    grandchild.kill()
                except OSError:
                    pass
                return 4
            time.sleep(.02)
    args.child_pid_file.write_text(str(grandchild.pid if grandchild else os.getpid()))
    command = [sys.executable, str(Path(__file__).resolve()), '--output', str(output),
               '--mode', args.mode, '--child-pid-file', str(args.child_pid_file)]
    (output / 'launch.json').write_text(json.dumps({
        'command': command,
        'childPid': grandchild.pid if grandchild else os.getpid(),
        'childReaped': True,
        'ignoreReady': args.mode == 'ignore',
        'scope': 'session-domain-cleanup fixture; not A5.2a evidence',
    }, ensure_ascii=False) + '\n')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
