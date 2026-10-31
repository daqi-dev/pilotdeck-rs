#!/usr/bin/env python3
"""#1145 Real TS capture; reuse the existing D0 parity lease/process ownership."""
import sys
if not sys.flags.isolated:
    raise SystemExit('#1145 use python3 -I capture.py OUTPUT [case-id ...]')
sys.dont_write_bytecode = True
import importlib.util
import json
import os
from pathlib import Path
import signal
import subprocess

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[4]
spec = importlib.util.spec_from_file_location('pdrs_cli_round', ROOT / 'scripts/cli-parity/run_round.py')
rounds = importlib.util.module_from_spec(spec)
spec.loader.exec_module(rounds)

if len(sys.argv) < 2:
    raise SystemExit('#1145 capture OUTPUT [case-id ...]')
output = Path(sys.argv[1]).resolve()
output.mkdir(parents=True, exist_ok=False)
# Same correction as the coordinator's sigint-default.py; before spawning the
# owned group, restore the disposition possibly inherited from a background job.
signal.signal(signal.SIGINT, signal.SIG_DFL)

def interrupted(signum, _frame):
    raise KeyboardInterrupt(f'#1145 capture interrupted by signal {signum}')

# Python unwinds through stop_group and the lease on parent interruption. Exec
# resets these caught dispositions to default in the owned Node subprocess.
signal.signal(signal.SIGINT, interrupted)
signal.signal(signal.SIGTERM, interrupted)
node = os.environ.get('NODE22', 'node')
env = dict(os.environ, PDRS_SIGNAL_LEASE=str(os.getpid()))
# Conservative derived bound: two 12s phases plus five 3s cleanup/setup bounds
# per selected cell. Suspended cells in the all-case denominator only add slack.
cell_count = len(sys.argv[2:]) or len(json.loads((HERE / 'cases.json').read_text()))
batch_seconds = cell_count * (2 * 12 + 5 * 3) + 30
with rounds._common.parity_lock():
    proc = None
    with (output / 'capture.log').open('xb') as log:
        try:
            with rounds.deferred_signals():
                proc = rounds.spawn([node, str(HERE / 'probe.mjs'), str(output), *sys.argv[2:]],
                                    env=env, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
            marker = output / 'owned-process.json'
            temporary = marker.with_suffix('.tmp')
            with temporary.open('x') as pidfile:
                json.dump({'pid': proc.pid, 'owner': os.getpid()}, pidfile)
            temporary.replace(marker)
            code = proc.wait(timeout=batch_seconds)
        finally:
            if proc is not None:
                cleanup = rounds.stop_group(proc)
                with (output / 'group-cleanup.json').open('x') as result:
                    json.dump(cleanup, result, indent=2)
                if not cleanup['allStopped']:
                    raise RuntimeError('#1145 owned process group not fully stopped; lease retained')
raise SystemExit(code)
