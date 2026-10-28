#!/usr/bin/env python3
# anchor-selfcheck: unsupported real subprocess and cancel-eligibility unit tests, no source mutation definitions
"""Real pipe regressions and structured cancel-verdict adversarial controls."""
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

from io_deadline import Lines, DeadlineExceeded, write
import selftest
from cancel_evidence import abort_evidence_ok, EVENTS, DETAILS


class PipeDeadline(unittest.TestCase):
    def child(self, code):
        p = subprocess.Popen([sys.executable, '-B', '-c', code], stdin=subprocess.PIPE,
                             stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, start_new_session=True)
        self.addCleanup(lambda: (selftest.terminate_group(p), p.stdin.close(), p.stdout.close()))
        return p

    def assert_deadline(self, action, label):
        start = time.monotonic()
        with self.assertRaisesRegex(DeadlineExceeded, f'#969 {label}: pipe deadline expired',
                                    msg=f'#969 {label}: must reject blocked pipe'):
            action(start + .2)
        self.assertLess(time.monotonic() - start, 2, f'#969 {label}: deadline must be bounded')

    def test_no_frame(self):
        p = self.child('import time; time.sleep(.7)')
        self.assert_deadline(lambda d: Lines(p.stdout).readline(d, 'no-frame'), 'no-frame')

    def test_partial_line(self):
        p = self.child("import os,time; os.write(1,b'{'); time.sleep(.7)")
        self.assert_deadline(lambda d: Lines(p.stdout).readline(d, 'partial-line'), 'partial-line')

    def test_missing_second_frame(self):
        p = self.child("import os,time; os.write(1,b'{}\\n'); time.sleep(.7)")
        reader = Lines(p.stdout)
        self.assertEqual(reader.readline(time.monotonic()+2, 'first-frame'), '{}\n', '#969 first-frame: real first line')
        self.assert_deadline(lambda d: reader.readline(d, 'second-frame'), 'second-frame')

    def test_write_backpressure(self):
        p = self.child('import time; time.sleep(.7)')
        self.assert_deadline(lambda d: write(p.stdin, 'x' * 2_000_000, d, 'backpressure'), 'backpressure')

    def test_all_direct_checks_reach_cleanup(self):
        for fn in (selftest.verify_mitm_pipe_close, selftest.verify_mitm_eof_reap, selftest.verify_mitm_eof_reap_group):
            with self.subTest(check=fn.__name__), tempfile.TemporaryDirectory() as temp:
                p = self.child('import time; time.sleep(.7)')
                with patch.object(selftest, 'spawn_mitm', return_value=p), patch.object(selftest, 'MITM_CHECK_SECONDS', .2):
                    with self.assertRaisesRegex(DeadlineExceeded, '#969 mitm-', msg='#969 direct check must reach named read deadline'):
                        fn(Path(temp))
                self.assertIsNotNone(p.poll(), '#969 direct read deadline must reap process in finally')

    def test_invalid_wire_bytes_fail_closed(self):
        for payload in (b'\xff\n', b'{\n', b'{'):
            with self.subTest(payload=payload), tempfile.TemporaryDirectory() as temp:
                root = Path(temp)
                p = selftest.spawn_mitm(root/'frames.jsonl', root/'summary.json',
                    [sys.executable, '-B', '-c', f'import os; os.write(1, {payload!r})'])
                try:
                    p.wait(timeout=5)
                    self.assertEqual(p.returncode, 4, '#969 invalid-wire: protocol errors must fail closed')
                    self.assertIn('#969 mitm: bounded I/O failure:', selftest.mitm_stderr(p),
                                  '#969 invalid-wire: failure must have the dedicated diagnostic')
                finally:
                    selftest.terminate_group(p)
                    p.stdin.close()
                    p.stdout.close()
                    p.error_log.close()


class CancelVerdict(unittest.TestCase):
    def evidence(self, temp):
        p = Path(temp)
        r = {'scenario':'cancel', 'injection':'passthrough', 'events':EVENTS, 'eventDetails':DETAILS,
             'terminal':{'type':'success','stopReason':'completed','turns':1,'messageCount':2},
             'error':None, 'modelCalls':1, 'modelEventCounts':[None,3], 'modelInterrupted':False,
             'cancelWindowEnteredAtEvents':2, 'abortAfterEvents':None, 'abortReason':None,
             'toolCalls':[], 'toolResults':[]}
        mock = [{'listening': True, 'port': 12345},
                {'index':1,'scenario':'hang','connection':'completed','sent':3,
                 'request':{'roles':['user'],'userText':'LIVE_E2E_HANG run the cancel window'}}]
        (p/'mock.log').write_text(''.join(json.dumps(x)+'\n' for x in mock))
        return p, r

    def test_window_external_connection_fault_is_invalid(self):
        with tempfile.TemporaryDirectory() as temp:
            p, r = self.evidence(temp)
            (p/'driver-result.json').write_text(json.dumps(r))
            self.assertTrue(abort_evidence_ok(p)[0], '#969 cancel eligibility positive control')
            for key, value in [('events', []), ('cancelWindowEnteredAtEvents', None),
                               ('error', 'Sidecar connection closed before first event'),
                               ('modelCalls', 0), ('terminal', None), ('modelInterrupted', True),
                               ('modelEventCounts', [None, 1]), ('timeout', True)]:
                with self.subTest(field=key):
                    (p/'driver-result.json').write_text(json.dumps({**r, key:value}))
                    self.assertFalse(abort_evidence_ok(p)[0], f'#969 cancel {key}: unrelated fault must be INVALID')
            (p/'driver-result.json').write_text(json.dumps(r))
            (p/'mock.log').write_text('{}\n' + json.dumps({'connection':'aborted'})+'\n')
            self.assertFalse(abort_evidence_ok(p)[0], '#969 cancel mock: incomplete request must be INVALID')


if __name__ == '__main__':
    unittest.main(verbosity=2)
