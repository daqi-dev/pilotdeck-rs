#!/usr/bin/env python3
"""A host stand-in that dies the way a SIGKILLed test process does (#1779).

The a6-host fixtures are spawned into their own process group
(``connection.rs:65``), so when the test process dies they are reparented to
init and no signal ever reaches them. Reproducing that inside the Rust suite
needs a real parent to lose, which is what this probe is: it performs the
hello/capabilities handshake, records the fixture pid, then SIGKILLs itself.
From the fixture's point of view this is indistinguishable from a killed host.

Usage: orphan_probe.py <fixture.py> <mode> <receipt.txt> <pid-dir>
The fixture pid lands in <pid-dir>/fixture.pid before this process dies.
"""
import json
import os
import select
import signal
import subprocess
import sys


def main() -> int:
    fixture, mode, receipt, pid_dir = sys.argv[1:5]
    # stderr must be a real pipe with no reader after this process dies: that is
    # what `connection.rs:71` `Stdio::piped()` gives the peer, and it is the only
    # way the watchdog's own diagnostics can raise BrokenPipeError. DEVNULL here
    # would hide exactly the failure this probe exists to catch (#1779).
    child = subprocess.Popen(
        [sys.executable, fixture, mode, receipt],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        bufsize=1,
        preexec_fn=os.setpgrp,  # connection.rs:65 `command.process_group(0)`
    )
    with open(os.path.join(pid_dir, 'fixture.pid'), 'w') as handle:
        handle.write(str(child.pid))

    def rpc(message):
        child.stdin.write(json.dumps(message) + '\n')
        child.stdin.flush()
        ready, _, _ = select.select([child.stdout], [], [], 10)
        if not ready:
            raise SystemExit('fixture did not answer ' + message['method'])
        return json.loads(child.stdout.readline())

    rpc({'kind': 'request', 'messageId': 'hello-1', 'method': 'hello'})
    rpc({'kind': 'request', 'messageId': 'cap-1', 'method': 'capabilities'})
    # Die without closing stdin: the kernel closes the pipe for us, and nothing
    # signals the fixture's group. Any peer that survives this is the leak.
    os.kill(os.getpid(), signal.SIGKILL)
    return 1


if __name__ == '__main__':
    sys.exit(main())
