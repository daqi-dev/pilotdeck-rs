#!/usr/bin/env python3
# anchor-selfcheck: unsupported real kernel cancellation regression, not a source mutation table
"""Exercise the actual CLI timeout supervisor while a registered fixture group lives.
Run python3 -I -B test_kernel_cancellation.py --output <fresh-directory>.
The private worker verifies the same HEAD imports as the standalone kernel CLI.
"""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import time
import unittest
sys.dont_write_bytecode=True
HERE=Path(__file__).resolve().parent
ROOT=HERE.parents[4]

def load(name,path):
    spec=importlib.util.spec_from_file_location(name,path)
    module=importlib.util.module_from_spec(spec);sys.modules[name]=module
    source=path.read_bytes();exec(compile(source,str(path),'exec'),module.__dict__)
    module._apparatus_source=source
    return module

def worker(output):
    fixture=load('kernel_cancellation_controls',HERE/'check_linux.py')
    md=fixture.guard.load('kernel_cancellation_session',ROOT/'scripts/gateway-parity/session_domain.py')
    md._load('linux_group_cleanup',ROOT/'scripts/gateway-parity/linux_group_cleanup.py')
    md._verify_cli_imports()
    base=fixture.Controls
    class ActiveGroup(unittest.TestCase):
        child=base.child
        leaky=base.leaky
        def test_active_group(self):
            child,fd=self.leaky()
            member=json.loads(next(fixture.OUT.glob('child-*.json')).read_text())['pid']
            (output/'ready.json').write_text(json.dumps({'leader':child.pid,'member':member,'pgid':child.pid}))
            print('CANCELLATION_READY',flush=True)
            time.sleep(30)
            self.fail('the timeout supervisor did not interrupt the owned group')
    fixture.Controls=ActiveGroup
    cache=fixture._pyc_root.name
    try:
        fixture.execute_controls(md,output/'control')
    except KeyboardInterrupt:
        (output/'worker.json').write_text(json.dumps({'interrupted':True,
            'ownedChildrenEmpty':not fixture._OWNED_CHILDREN,
            'ownedMemberFdsEmpty':not fixture._OWNED_MEMBER_FDS,
            'privateCacheRemoved':not Path(cache).exists()}))
        return
    raise AssertionError('worker did not observe timeout cancellation')

@unittest.skipUnless(sys.platform=='linux','Linux pidfd and held-group cancellation')
class Cancellation(unittest.TestCase):
    def test_timeout_reaps_owned_groups(self):
        supervisor=load('kernel_cancellation_supervisor',HERE/'check_cleanup.py')
        command=[sys.executable,'-I','-B',str(Path(__file__).resolve()),'--worker','--output',str(OUTPUT)]
        try:
            result=supervisor.run_kernel_cli_process(command,timeout=5,ready=OUTPUT/'ready.json')
        except subprocess.TimeoutExpired as error:
            (OUTPUT/'worker.stdout').write_text(error.stdout or '')
            (OUTPUT/'worker.stderr').write_text(error.stderr or '')
        else:
            (OUTPUT/'worker.stdout').write_text(result.stdout)
            (OUTPUT/'worker.stderr').write_text(result.stderr)
            self.fail(f'expected timeout with a live fixture; worker exit={result.returncode}')
        self.assertTrue((OUTPUT/'ready.json').is_file(),'must reach an actual leaky group before cancellation')
        state=json.loads((OUTPUT/'worker.json').read_text());self.assertTrue(all(state.values()),state)
        owned=json.loads((OUTPUT/'ready.json').read_text());deadline=time.monotonic()+5
        while True:
            try:os.killpg(owned['pgid'],0)
            except ProcessLookupError:break
            self.assertLess(time.monotonic(),deadline,'owned fixture group survived cancellation')
            time.sleep(.02)
        (OUTPUT/'result.json').write_text(json.dumps({'testOnly':True,'pass':True,
            'actualTimeoutSupervisor':True,'actualLeakyGroup':owned,'cleanup':state,
            'kernelGroupProbe':'ESRCH','formalAcceptance':False},indent=2))

if __name__=='__main__':
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output',type=Path,required=True)
    parser.add_argument('--worker',action='store_true')
    args=parser.parse_args();OUTPUT=args.output.resolve()
    if args.worker:worker(OUTPUT)
    else:
        OUTPUT.mkdir(parents=True,exist_ok=False)
        unittest.main(argv=[sys.argv[0]],verbosity=2)
