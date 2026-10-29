#!/usr/bin/env python3
# anchor-selfcheck: unsupported fixture unit tests, no source mutation definitions
"""#748 acceptance inventory and reader ownership, including deterministic negative inputs."""
import subprocess
import inspect
import unittest
from unittest.mock import Mock, patch
import run_host
import stdio_mitm


class AcceptanceTests(unittest.TestCase):
    def test_exact_test_inventory(self):
        for target, names in run_host.EXPECTED_TESTS.items():
            rows = [f'test {name} ... ok\n' for name in sorted(names)]
            def accepted(log, code=0):
                return run_host.completed_tests(target, subprocess.CompletedProcess([], code, log, ''))['passed']
            self.assertTrue(accepted(''.join(rows)), '#748 complete host test inventory accepted')
            for text, code in [('', 0), ('0 passed; 0 failed', 0), (''.join(rows[1:]), 0),
                               (''.join(rows + rows[:1]), 0), (''.join(rows) + 'test unexpected ... ok\n', 0),
                               (''.join(rows), 1), (''.join(rows).replace(' ... ok', ' ... ignored', 1), 0)]:
                self.assertFalse(accepted(text, code), '#748 host acceptance requires all expected tests')

    def test_reader_owns_stdout_close(self):
        process = Mock()
        process.poll.return_value = 0
        with patch.object(stdio_mitm, 'kill_child_group') as reap:
            stdio_mitm.reap_after_host_eof(process)
        self.assertEqual(reap.call_count, 1, '#748 EOF still reaps child group')
        self.assertEqual(process.stdin.close.call_count, 1, '#748 EOF closes owned input')
        self.assertEqual(process.stdout.close.call_count, 0, '#748 only reader closes stdout')


    def test_negative_controls_kill_old_behaviors(self):
        probes = [
            (run_host, 'completed_tests',
             'result.returncode == 0 and len(names) == len(expected) and set(names) == expected',
             'result.returncode == 0', 'test_exact_test_inventory',
             '#748 host acceptance requires all expected tests', 'host-empty-set'),
            (stdio_mitm, 'reap_after_host_eof', 'for stream in (process.stdin,):',
             'for stream in (process.stdout, process.stdin):', 'test_reader_owns_stdout_close',
             '#748 only reader closes stdout', 'stdout-close-race'),
        ]
        outcomes = []
        for module, function, old, new, test, message, label in probes:
            try:
                source = inspect.getsource(getattr(module, function))
                self.assertEqual(source.count(old), 1, '#748 Python negative anchor unique')
                namespace = vars(module).copy()
                if module is stdio_mitm:
                    namespace['kill_child_group'] = lambda process: stdio_mitm.kill_child_group(process)
                # In-memory code only: source files, HEAD and running cargo are untouched.
                exec(compile(source.replace(old, new), '<#748 ' + label + '>', 'exec'), namespace)
                with patch.object(module, function, namespace[function]):
                    result = unittest.TestResult()
                    AcceptanceTests(test).run(result)
                killed = not result.errors and len(result.failures) == 1 and message in result.failures[0][1]
                outcomes.append((label, killed, result.errors, result.failures))
            except Exception as error:
                killed = False
                outcomes.append((label, False, repr(error)))
            print(f'{label}: {"KILLED" if killed else "INVALID"}', flush=True)
        self.assertTrue(all(row[1] for row in outcomes), '#748 all Python negative controls killed: ' + repr(outcomes))


if __name__ == '__main__':
    unittest.main(verbosity=2)
