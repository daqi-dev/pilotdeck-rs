#!/usr/bin/env python3
# anchor-selfcheck: unsupported in-memory verifier compilation and mutation checks, no on-disk source changes
"""Compile and kill verifier mutants in memory, without changing worktree sources."""
import ast
import contextlib
import io
import json
from pathlib import Path
import sys
import tempfile
import types

import attack_suite
import test_harness
import unittest

ROOT = Path(__file__).resolve().parent


def main():
    tree = ast.parse((ROOT / 'negative_controls.py').read_text())
    table = next(node.value for node in tree.body
                 if isinstance(node, ast.Assign) and any(isinstance(t, ast.Name) and t.id == 'mutations' for t in node.targets))
    rows = ast.literal_eval(table)
    outcomes = []
    # All Python source replacements compile, including process/middleman ones.
    for name, file, old, new in rows:
        if not file:
            continue
        original = (ROOT / file).read_text()
        if original.count(old) != 1:
            print(f'INVALID {name}: anchor count {original.count(old)}')
            return 2
        changed = original.replace(old, new, 1)
        if file.endswith('.py'):
            compile(changed, str(ROOT / file), 'exec')
            print(f'COMPILES {name}')
        if not (name.startswith('comparator-') or name == 'report-failure-verified-dropped'):
            continue
        module = types.ModuleType('mutated_selftest')
        module.__file__ = str(ROOT / file)
        exec(compile(changed, module.__file__, 'exec'), module.__dict__)
        previous = attack_suite.selftest
        attack_suite.selftest = module
        try:
            with tempfile.TemporaryDirectory(prefix='969-memory-') as temp:
                report = Path(temp) / 'attacks.json'
                code = attack_suite.main(report)
                results = json.loads(report.read_text()) if report.exists() else []
                target = {'comparator-type-blind':'bool-for-int-count',
                          'comparator-keys-blind':'extra-terminal-key',
                          'comparator-length-blind':'events-truncated-together',
                          'report-failure-verified-dropped':'failing-case-carries-verified-false'}[name]
                killed = code == 1 and any(r['name'] == target and r['caught'] is False for r in results)
                outcomes.append((name, killed))
                print(f'{name}: {"KILLED" if killed else "INVALID"}; bound=attack_suite.{target}')
        finally:
            attack_suite.selftest = previous
    # No-output child ends normally after .7s. Removing readiness/deadline must
    # fail test_no_frame's assertion, not an outer timeout or unrelated panic.
    import io_deadline
    previous = io_deadline.ready
    io_deadline.ready = lambda *args, **kwargs: None
    try:
        output = io.StringIO()
        result = unittest.TextTestRunner(stream=output).run(
            test_harness.PipeDeadline('test_no_frame'))
        print(output.getvalue())
        killed = len(result.failures) == 1 and not result.errors and '#969 no-frame: must reject blocked pipe' in output.getvalue()
        outcomes.append(('pipe-deadline-disabled', killed))
        print(f'pipe-deadline-disabled: {"KILLED" if killed else "INVALID"}; bound=test_harness.PipeDeadline.test_no_frame')
    finally:
        io_deadline.ready = previous
    # The exact cancellation field check is deliberately skipped in memory.
    import cancel_evidence
    previous = cancel_evidence.strict_mismatch
    cancel_evidence.strict_mismatch = lambda a, e, p: None if p == 'error' else previous(a, e, p)
    try:
        output = io.StringIO()
        result = unittest.TextTestRunner(stream=output).run(
            test_harness.CancelVerdict('test_window_external_connection_fault_is_invalid'))
        print(output.getvalue())
        killed = len(result.failures) == 1 and not result.errors and '#969 cancel error: unrelated fault must be INVALID' in output.getvalue()
        outcomes.append(('cancel-error-check-disabled', killed))
        print(f'cancel-error-check-disabled: {"KILLED" if killed else "INVALID"}; bound=test_harness.CancelVerdict.test_window_external_connection_fault_is_invalid')
    finally:
        cancel_evidence.strict_mismatch = previous
    print(f'{sum(ok for _, ok in outcomes)}/{len(outcomes)} memory controls KILLED')
    return 0 if all(ok for _, ok in outcomes) else 1


if __name__ == '__main__':
    sys.exit(main())
