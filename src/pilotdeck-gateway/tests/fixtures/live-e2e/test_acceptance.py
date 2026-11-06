#!/usr/bin/env python3
# anchor-selfcheck: unsupported fixture unit tests, no source mutation definitions
"""#748 acceptance inventory and reader ownership, including deterministic negative inputs."""
import subprocess
import inspect
import os
import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock, patch
import run_host
import stdio_mitm


class AcceptanceTests(unittest.TestCase):
    def test_exact_test_inventory(self):
        for target, names in run_host.EXPECTED_TESTS.items():
            # Cargo prints `running N tests` before the per-test rows; the
            # runner cross-checks both, so the fixture must carry the header in
            # libtest's exact singular/plural form.
            header = f'running {len(names)} test{"s" if len(names) != 1 else ""}\n'
            rows = [f'test {name} ... ok\n' for name in sorted(names)]
            def accepted(log, code=0):
                return run_host.completed_tests(target, subprocess.CompletedProcess([], code, log, ''))['passed']
            self.assertTrue(accepted(header + ''.join(rows)), '#748 complete host test inventory accepted')
            for text, code in [(header, 0), ('', 0), ('0 passed; 0 failed', 0),
                               (header + ''.join(rows[1:]), 0),
                               (header + ''.join(rows + rows[:1]), 0),
                               (header + ''.join(rows) + 'test unexpected ... ok\n', 0),
                               (header + ''.join(rows), 1),
                               (header + ''.join(rows).replace(' ... ok', ' ... ignored', 1), 0)]:
                self.assertFalse(accepted(text, code), '#748 host acceptance requires all expected tests')

    def test_failure_message_names_missing_tests(self):
        # #589 close-out negative control: a shrinking inventory must report the
        # counts and the missing cell names, not just a faceless FAIL.
        expected = run_host.EXPECTED_TESTS['live_e2e']
        absent = sorted(expected)[-2:]
        present = sorted(expected - set(absent))
        log = (f'running {len(expected)} tests\n'
               + ''.join(f'test {name} ... ok\n' for name in present))
        record = run_host.completed_tests('live_e2e', subprocess.CompletedProcess([], 0, log, ''))
        self.assertFalse(record['passed'])
        self.assertEqual(record['expectedCount'], len(expected))
        self.assertEqual(record['observedCount'], len(present))
        message = run_host.failure_message('live_e2e', record)
        print(f'missing-cell diagnostic: {message}', flush=True)
        self.assertIn(f'expected {len(expected)} / observed {len(present)}', message)
        self.assertIn('missing:', message)
        for name in absent:
            self.assertIn(name, message)
        # An extra cell is named too, and cargo's own count disagreement is
        # surfaced even when the declared inventory happens to match the rows.
        extra = run_host.completed_tests(
            'live_e2e', subprocess.CompletedProcess(
                [], 0, f'running {len(expected) + 1} tests\n'
                + ''.join(f'test {name} ... ok\n' for name in sorted(expected))
                + 'test live_ws_extra ... ok\n', ''))
        self.assertIn('unexpected: live_ws_extra', run_host.failure_message('live_e2e', extra))
        # Announced count drifting on its own (rows exactly match the declared
        # inventory) must still fail: cargo says it ran a different number.
        announced_only = run_host.completed_tests(
            'live_e2e', subprocess.CompletedProcess(
                [], 0, f'running {len(expected) + 1} tests\n'
                + ''.join(f'test {name} ... ok\n' for name in sorted(expected)), ''))
        # Assert the reason line first: `failure_message`'s prefix always says
        # "cargo announced N", so only the reason text proves the cross-check
        # ran (a removed check dies on this named expectation).
        self.assertIn(f'cargo announced {len(expected) + 1} tests but '
                      f'{len(expected)} reported ok',
                      run_host.failure_message('live_e2e', announced_only))
        self.assertFalse(announced_only['passed'])

    def test_slot_gate_discovery(self):
        # #589 close-out: a remote runner host has no ~/ws/pdrs-ops; the gate
        # lives under ~/ws/pdrs-runner-tools. Silence must never mean "skip the
        # quota", so a missing gate is an error with the tried paths.
        def make_gate(path):
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text('#!/bin/sh\nexit 0\n')
            path.chmod(0o755)
            return path

        with tempfile.TemporaryDirectory(prefix='pdrs-run-host-gate-') as temp:
            home = Path(temp)
            env = {'HOME': str(home)}
            with self.assertRaises(RuntimeError) as caught:
                run_host.slot_gate(env)
            self.assertIn('no executable cargo slot gate', str(caught.exception))
            self.assertIn('PDRS_CARGO_SLOT_BIN', str(caught.exception))
            ops = make_gate(home / 'ws/pdrs-ops/bin/cargo-slot')
            self.assertEqual(run_host.slot_gate(env), str(ops))
            runner = make_gate(home / 'ws/pdrs-runner-tools/bin/cargo')
            explicit = make_gate(home / 'explicit-gate')
            self.assertEqual(run_host.slot_gate(dict(env, PDRS_CARGO_SLOT_BIN=str(explicit))), str(explicit))
            with self.assertRaises(RuntimeError):
                run_host.slot_gate(dict(env, PDRS_CARGO_SLOT_BIN=str(home / 'missing-gate')))
            # Both installs present but the ops gate is broken: no silent
            # switch to the runner copy (its slot cap can drift) -> error.
            ops.unlink()
            with self.assertRaises(RuntimeError) as caught:
                run_host.slot_gate(env)
            self.assertNotIn(str(runner), str(caught.exception))
            # A remote runner host has no ~/ws/pdrs-ops at all: fall back.
            (home / 'ws/pdrs-ops/bin').rmdir()
            (home / 'ws/pdrs-ops').rmdir()
            self.assertEqual(run_host.slot_gate(env), str(runner))
            with self.assertRaises(RuntimeError):
                run_host.slot_gate({})

    def test_cargo_slot_requires_the_gate(self):
        # #589 r2 P1: the wait must be strict. `wait_for_slot` maps
        # cargo-slots.sh's 69 ("no gate") to success, so a gate that vanishes
        # while we are polling for a slot would silently skip the quota unless
        # gate_required=True turns it into a hard error.
        with patch.dict(os.environ), \
                patch.object(run_host, 'slot_gate', return_value='/stub/gate'), \
                patch.object(run_host, 'wait_for_slot') as wait:
            gate, _ = run_host.cargo_slot(timeout=7)
            self.assertEqual(gate, '/stub/gate')
            self.assertEqual(os.environ['PDRS_CARGO_SLOT_BIN'], '/stub/gate')
            self.assertIs(wait.call_args.kwargs.get('gate_required'), True)
            self.assertEqual(wait.call_args.kwargs.get('timeout'), 7)

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
            (run_host, 'completed_tests', "'passed': not reasons,",
             "'passed': True,", 'test_exact_test_inventory',
             '#748 host acceptance requires all expected tests', 'host-empty-set'),
            (run_host, 'completed_tests',
             "    elif announced_count != len(names):",
             '    elif False:', 'test_failure_message_names_missing_tests',
             'cargo announced', 'announced-count-crosscheck'),
            (run_host, 'slot_gate',
             "        raise RuntimeError(f'#748 PDRS_CARGO_SLOT_BIN={configured} is not an executable gate')",
             '        return None', 'test_slot_gate_discovery',
             'RuntimeError not raised', 'slot-gate-fail-loud'),
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
