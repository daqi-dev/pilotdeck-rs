#!/usr/bin/env python3
# anchor-selfcheck: unsupported process-group behavior controls with no source mutation definitions
"""#2916 Linux cleanup controls through the registered session_domain entry and loader.

Run python3 -I -B check_linux.py --output <fresh-directory>.
Output binds executing apparatus bytes, real process controls and source stability.
"""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

HERE=Path(__file__).resolve().parent
_pyc_root=tempfile.TemporaryDirectory(prefix='pdrs-linux-cleanup-pyc-')
sys.pycache_prefix=_pyc_root.name
sys.dont_write_bytecode=True
parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--root',type=Path,default=HERE.parents[4])
parser.add_argument('--output',type=Path,required=True)
args=parser.parse_args()
ROOT=args.root.resolve();OUT=args.output.resolve();OUT.mkdir(parents=True,exist_ok=False)
# Use the same explicit source loader as test_safety.py. No AST extraction,
# temporary source copy, frozen guard replacement or sys.path insertion.
guard_path=ROOT/'scripts/gateway-parity/_guard.py'
spec=importlib.util.spec_from_file_location('_guard',guard_path)
guard=importlib.util.module_from_spec(spec);guard._apparatus_source=guard_path.read_bytes()
exec(compile(guard._apparatus_source,str(guard_path),'exec'),guard.__dict__)
sys.modules['_guard']=guard
_load=guard.load
common=_load('common',ROOT/'scripts/gateway-parity/common.py')
e1=guard.load('linux_cleanup_control_session',ROOT/'scripts/gateway-parity/session_domain.py')
module=e1._load('linux_group_cleanup',ROOT/'scripts/gateway-parity/linux_group_cleanup.py')
e1._verify_cli_imports()
SOURCE=ROOT/'scripts/gateway-parity/session_domain.py'
HELPER=ROOT/'scripts/gateway-parity/linux_group_cleanup.py'
executed_sources={SOURCE:e1._apparatus_source,HELPER:module._apparatus_source,guard_path:guard._apparatus_source,Path(__file__).resolve():Path(__file__).read_bytes()}
source_hashes={str(p):hashlib.sha256(source).hexdigest() for p,source in executed_sources.items()}
WAIT=e1.wait_exit_unreaped;HELD=e1.HeldProcessGroup
ROWS=[]

def probe(child):
    return e1.probe_process_group(child,wait=10)

def cleanup(child,fd=None):
    if fd is not None:
        try:signal.pidfd_send_signal(fd,signal.SIGKILL)
        except ProcessLookupError:pass
        os.close(fd)
    if child.returncode is None:
        child.wait(timeout=10)

class Controls(unittest.TestCase):
    def run_control(self,name,child,fn=probe,fd=None):
        try:
            value=fn(child);ROWS.append({'case':name,'result':value});return value
        finally:cleanup(child,fd)
    def child(self,code='pass',args=()):
        return subprocess.Popen([sys.executable,'-I','-c',code,*args],start_new_session=True,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
    def leaky(self):
        marker=OUT/f'child-{os.getpid()}-{time.monotonic_ns()}.json'
        code="import subprocess,sys,json; p=subprocess.Popen(['sleep','30'],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL);open(sys.argv[1],'w').write(json.dumps({'pid':p.pid}))"
        child=self.child(code,[str(marker)]);self.assertEqual(WAIT(child.pid,10),'EXITED');pid=json.loads(marker.read_text())['pid'];fd=os.pidfd_open(pid,0);return child,fd
    def test_no_descendants_uses_kernel_esrch(self):
        r=self.run_control('empty',self.child());self.assertEqual(r['status'],'VERIFIED');self.assertTrue(r['leaderReaped']);self.assertEqual(r['kernelGroupProbe'],'ESRCH');self.assertIsNone(r['signal'])
    def test_visible_leftover_is_a_finding_even_after_remediation(self):
        child,fd=self.leaky();r=self.run_control('visible-leftover',child,fd=fd);self.assertEqual(r['status'],'GROUP_NOT_EMPTY');self.assertTrue(r['observedGroupMembers']);self.assertIn(r['signal'],('SIGTERM','SIGKILL'))
    def test_hidden_proc_member_cannot_pass(self):
        child,fd=self.leaky()
        def hidden(c):
            with patch.object(module,'_observed_members',return_value=([],['simulated hidepid filter'])):return probe(c)
        r=self.run_control('hidden-live-member',child,hidden,fd);self.assertEqual(r['status'],'GROUP_NOT_EMPTY');self.assertIsNone(r['signal']);self.assertEqual(r['kernelGroupProbe'],'alive')
    def test_denied_probe_never_means_empty(self):
        def denied(c):
            with patch.object(module.os,'killpg',side_effect=PermissionError('injected EPERM')):return probe(c)
        r=self.run_control('permission-denied',self.child(),denied);self.assertEqual(r['status'],'BLOCKED');self.assertEqual(r['kernelGroupProbe'],'denied')
    def test_unknown_probe_error_never_passes(self):
        def denied(c):
            with patch.object(module.os,'killpg',side_effect=OSError('injected unknown')):return probe(c)
        r=self.run_control('unknown-error',self.child(),denied);self.assertEqual(r['status'],'BLOCKED')
    def test_lost_leader_identity_sends_nothing(self):
        child=self.child();self.assertEqual(WAIT(child.pid,10),'EXITED');child.wait(timeout=5)
        with patch.object(module.os,'killpg',side_effect=AssertionError('must not call killpg after lost ownership')):
            r=e1.probe_process_group(child,wait=1)
        ROWS.append({'case':'lost-identity','result':r});self.assertEqual(r['status'],'NOT_VERIFIED')
    def test_proc_unavailable_is_not_itself_evidence_of_absence(self):
        def unavailable(c):
            with patch.object(module,'_observed_members',return_value=([],['proc unreadable'])):return probe(c)
        r=self.run_control('proc-unavailable-empty',self.child(),unavailable);self.assertEqual(r['status'],'VERIFIED');self.assertEqual(r['kernelGroupProbe'],'ESRCH');self.assertFalse(r['memberObservationComplete'])
    def test_no_real_group_signal_after_reaping(self):
        real=os.killpg;calls=[]
        child=self.child()
        def recording(pgid,sig):
            calls.append({'sig':sig,'leaderReturnCode':child.returncode});self.assertEqual(sig,0);return real(pgid,sig)
        def tracked(c):
            with patch.object(module.os,'killpg',side_effect=recording):return probe(c)
        r=self.run_control('no-real-signal-after-reap',child,tracked);self.assertEqual(r['status'],'VERIFIED');self.assertTrue(calls);self.assertTrue(all(c['leaderReturnCode'] is not None for c in calls))

    def direct(self, child, held):
        return module.probe(child,held,{'pid':child.pid,'pgid':child.pid,'signal':None},
            verified=e1.CLEANUP_VERIFIED,not_empty=e1.CLEANUP_GROUP_NOT_EMPTY,
            blocked=e1.CLEANUP_BLOCKED,not_verified=e1.CLEANUP_NOT_VERIFIED)
    def test_helper_initial_identity_gate_precedes_proc(self):
        child=self.child();self.assertEqual(WAIT(child.pid,10),'EXITED');child.wait(timeout=5);held=HELD(child);calls=[]
        def observed(_):calls.append(True);return [],[]
        with patch.object(module,'_observed_members',side_effect=observed):r=self.direct(child,held)
        ROWS.append({'case':'helper-initial-identity','result':r,'observerCalls':len(calls)})
        self.assertEqual(r['status'],'NOT_VERIFIED');self.assertEqual(calls,[])
    def test_helper_pre_reap_identity_gate(self):
        child=self.child();self.assertEqual(WAIT(child.pid,10),'EXITED');held=HELD(child)
        try:
            with patch.object(held,'_state',side_effect=['EXITED','RUNNING']):r=self.direct(child,held)
            ROWS.append({'case':'helper-pre-reap-identity','result':r});self.assertEqual(r['status'],'NOT_VERIFIED')
        finally:cleanup(child)
    def test_signal_refusal_cannot_erase_observed_finding(self):
        child,fd=self.leaky();held=HELD(child)
        try:
            with patch.object(held,'send',return_value=('refused','injected ownership loss')):r=self.direct(child,held)
            ROWS.append({'case':'signal-refused-positive-finding','result':r})
            self.assertTrue(r['observedGroupMembers']);self.assertEqual(r['status'],'GROUP_NOT_EMPTY');self.assertIn('signalRefused',r)
        finally:cleanup(child,fd)


def summarize_controls(result, stable):
    healthy = result.wasSuccessful() and stable and result.testsRun == 11 and not result.skipped
    status, exit_code = common.aggregate_status('PASS' if healthy else 'FAIL')
    return {'status':status,'exitCode':exit_code,
            'scope':'Linux cleanup regression controls; no product acceptance claim',
            'apparatusSha256':source_hashes,'sourceUnchanged':stable,'rows':ROWS,
            'testsRun':result.testsRun,'skipped':result.skipped,'success':result.wasSuccessful(),
            'failures':[{'test':case.id(),'traceback':trace}for case,trace in result.failures],
            'errors':[{'test':case.id(),'traceback':trace}for case,trace in result.errors]}


def main():
    try:
        if sys.platform != 'linux':
            status, exit_code = common.aggregate_status('BLOCKED')
            report={'status':status,'exitCode':exit_code,'reason':'Linux pidfd controls require Linux'}
        else:
            result=unittest.main(argv=[sys.argv[0]],verbosity=2,exit=False).result
            stable=all(Path(path).read_bytes()==source for path,source in executed_sources.items())
            report=summarize_controls(result,stable)
        with (OUT/'report.json').open('x') as f:json.dump(report,f,indent=2)
        return common.aggregate_status(report['status'])[1]
    finally:
        _pyc_root.cleanup()


if __name__=='__main__':
    raise SystemExit(main())
