#!/usr/bin/env python3
"""Launcher for session_domain cleanup controls.

The production runner starts the A5.2a launcher with start_new_session and then
waits.  This fixture reproduces the two interesting post-wait states without any
ps dependency: an empty group, a group with a sleep grandchild, and a grandchild
that ignores SIGTERM (to exercise the bounded SIGKILL escalation).

It writes the same ``six-cells/launch.json`` shape the real launcher writes, so
the existing launcherAndFixtureReaped check is exercised unchanged.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--mode', choices=('clean', 'leak', 'ignore'), required=True)
    parser.add_argument('--child-pid-file', type=Path, required=True)
    args = parser.parse_args()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    grandchild = None
    if args.mode == 'leak':
        grandchild = subprocess.Popen(
            ['sleep', '60'], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    elif args.mode == 'ignore':
        code = ('import signal,time; signal.signal(signal.SIGTERM, signal.SIG_IGN); '
                'time.sleep(60)')
        grandchild = subprocess.Popen(
            [sys.executable, '-I', '-c', code],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    args.child_pid_file.write_text(str(grandchild.pid if grandchild else os.getpid()))
    command = [sys.executable, str(Path(__file__).resolve()), '--output', str(output),
               '--mode', args.mode, '--child-pid-file', str(args.child_pid_file)]
    (output / 'launch.json').write_text(json.dumps({
        'command': command,
        'childPid': grandchild.pid if grandchild else os.getpid(),
        'childReaped': True,
        'scope': 'session-domain-cleanup fixture; not A5.2a evidence',
    }, ensure_ascii=False) + '\n')
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
