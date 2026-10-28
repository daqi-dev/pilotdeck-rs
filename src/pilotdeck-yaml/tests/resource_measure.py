#!/usr/bin/env python3
"""One fresh wrapper + one child: wait4/getrusage process peak, never prior samples.

macOS /usr/bin/time -l needs a sandbox-blocked sysctl. Python's child rusage
uses wait4 instead and reports the same kernel ru_maxrss, without that sysctl.
"""
import json
import platform
import resource
import subprocess
import sys

child = subprocess.run(sys.argv[1:], text=True, capture_output=True)
sys.stderr.write(child.stderr)
if child.returncode:
    sys.stdout.write(child.stdout)
    raise SystemExit(child.returncode)
rss = resource.getrusage(resource.RUSAGE_CHILDREN).ru_maxrss
for line in child.stdout.splitlines():
    if line.startswith('RESOURCE_SAMPLE '):
        row = json.loads(line.removeprefix('RESOURCE_SAMPLE '))
        row['rss_bytes'] = rss * (1 if platform.system() == 'Darwin' else 1024)
        print('RESOURCE_SAMPLE ' + json.dumps(row))
    else:
        # #942: forward the child's other stdout lines (libtest's
        # `running 1 test`, per-test and summary lines) so the caller can
        # check the exact-receipt criterion on the measured reruns.
        print(line)
