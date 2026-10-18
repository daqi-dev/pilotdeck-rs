"""Review regressions: malformed evidence and bounded owned-process cleanup."""
import contextlib
import importlib.util
import io
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import Mock, patch

SCRIPT = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location('exp003_run', SCRIPT / 'run.py')
run = importlib.util.module_from_spec(spec)
spec.loader.exec_module(run)


class ReviewRegressions(unittest.TestCase):
    def test_missing_driver_client_is_rejected_before_evidence_or_processes(self):
        client = 'dist/src/gateway/client/GatewayWsClient.js'
        with tempfile.TemporaryDirectory() as directory:
            directory = Path(directory)
            baseline, output = directory / 'baseline', directory / 'evidence'
            for name in ('dist/src/cli/pilotdeck.js', 'node_modules/tsx/dist/loader.mjs', 'ui/server/index.js'):
                path = baseline / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.touch()
            stderr = io.StringIO()
            with patch.object(sys, 'argv', ['run.py', '--baseline', str(baseline)]), \
                    patch.object(run, 'OUT', output), patch.object(run, 'start') as start, \
                    patch.object(run.socket, 'socket') as socket, contextlib.redirect_stderr(stderr):
                with self.assertRaises(SystemExit) as error:
                    run.main()
            self.assertEqual(error.exception.code, 2)
            self.assertIn(client, stderr.getvalue())
            self.assertFalse(output.exists())
            start.assert_not_called()
            socket.assert_not_called()

    def test_started_descendant_belongs_to_owned_process_group(self):
        with tempfile.TemporaryDirectory() as directory, patch.object(run, 'OUT', Path(directory)), \
                patch.object(run, 'PROCESSES', []), patch.object(run, 'STOPPED', set()):
            ready = Path(directory) / 'ready.json'
            source = '''import json, os, subprocess, sys, time
child = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(30)'])
with open(sys.argv[1], 'w') as output:
    json.dump({'parentGroup': os.getpgrp(), 'childGroup': os.getpgid(child.pid)}, output)
time.sleep(30)
'''
            proc = run.start('group-test', [sys.executable, '-c', source, str(ready)], dict(os.environ))
            try:
                deadline = time.monotonic() + 5
                while not ready.exists() and time.monotonic() < deadline:
                    time.sleep(.02)
                groups = json.loads(ready.read_text())
                self.assertEqual(groups, {'parentGroup': proc.pid, 'childGroup': proc.pid})
                self.assertNotEqual(proc.pid, os.getpgrp())
            finally:
                run.stop(proc)
                for _, log, _ in run.PROCESSES:
                    log.close()
            self.assertIsNotNone(proc.poll())

    def test_comparator_reports_invalid_evidence_instead_of_crashing(self):
        valid = {'seq': 1, 'connection': 1, 'direction': 'send',
                 'raw': '{"type":"hello"}', 'frame': {'type': 'hello'}}
        invalid = dict(valid, raw='{invalid json', frame='{invalid json')
        cases = [('identical valid', [valid], [valid], 0),
                 ('non-JSON raw on both sides', [invalid], [invalid], 1),
                 ('empty on both sides', [], [], 1),
                 ('duplicate sequence', [valid, valid], [valid, valid], 1),
                 ('raw/parsed mismatch', [dict(valid, frame={})], [valid], 1)]
        with tempfile.TemporaryDirectory() as directory:
            directory = Path(directory)
            for name, left, right, expected in cases:
                with self.subTest(name=name):
                    for filename, rows in [('left', left), ('right', right)]:
                        (directory / filename).write_text(''.join(json.dumps(row) + '\n' for row in rows))
                    proc = subprocess.run([sys.executable, SCRIPT / 'compare.py', directory / 'left',
                                           directory / 'right', '--out', directory / 'result.json'],
                                          capture_output=True, text=True, timeout=5)
                    self.assertEqual(proc.returncode, expected, proc.stderr)
                    result = json.loads((directory / 'result.json').read_text())
                    self.assertEqual(result['status'], 'PASS' if expected == 0 else 'FAIL')
                    self.assertEqual(bool(result['differences']), expected == 1)
                    self.assertNotIn('Traceback', proc.stderr)

    def test_cleanup_timeouts_do_not_mask_original_failure(self):
        proc = Mock(pid=12345)
        proc.poll.return_value = None
        proc.wait.side_effect = subprocess.TimeoutExpired('owned-child', 5)
        with patch.object(run, 'STOPPED', set()), patch.object(run, 'CLEANUP_ERRORS', []), \
                patch.object(run.os, 'killpg') as killpg, contextlib.redirect_stdout(io.StringIO()):
            with self.assertRaisesRegex(RuntimeError, 'original scenario failure'):
                try:
                    raise RuntimeError('original scenario failure')
                finally:
                    run.stop(proc)
            self.assertEqual([call.args for call in killpg.call_args_list],
                             [(12345, signal.SIGTERM), (12345, signal.SIGKILL)])
            self.assertEqual(len(run.CLEANUP_ERRORS), 1)

    def test_exited_parent_still_has_its_owned_group_terminated(self):
        proc = Mock(pid=12346)
        proc.poll.return_value = 0
        with patch.object(run, 'STOPPED', set()), patch.object(run.os, 'killpg') as killpg:
            run.stop(proc)
            self.assertEqual([call.args for call in killpg.call_args_list],
                             [(12346, signal.SIGTERM), (12346, signal.SIGKILL)])
            run.stop(proc)
            self.assertEqual(killpg.call_count, 2)

    def test_listener_assertion_rejects_wildcard_even_with_working_health(self):
        with tempfile.TemporaryDirectory() as directory, patch.object(run, 'OUT', Path(directory)), \
                patch.object(run, 'LISTENERS', []), patch.object(run.subprocess, 'run') as lsof:
            lsof.return_value = Mock(returncode=0, stdout='p12345\nn*:3301\n')
            with self.assertRaisesRegex(RuntimeError, 'must listen only on 127.0.0.1'):
                run.assert_loopback(3301, Mock(pid=12345))
            lsof.return_value.stdout = 'p12345\nn127.0.0.1:3301\n'
            run.assert_loopback(3301, Mock(pid=12345))
            self.assertTrue(run.LISTENERS[-1]['loopbackOnly'])


if __name__ == '__main__':
    unittest.main()
