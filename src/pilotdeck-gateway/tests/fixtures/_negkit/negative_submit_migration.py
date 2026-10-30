#!/usr/bin/env python3
# anchor-selfcheck: unsupported M6 meta-controls compile memory-only Python mutants, no product writes
"""Withdraw migration fixes and require their exact unittest assertions to fail."""
import argparse
from contextlib import redirect_stdout, redirect_stderr
import io
from pathlib import Path
import sys
import tempfile
from types import ModuleType
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parent))
import test_submit_migration as tests
from negative_contract import BoundResult

# module, name, unique source span, replacement, test, assertion
VARIANTS = (
    ('C', 'shared-scope', 'return (isinstance(marker, str)', 'return True or (isinstance(marker, str)',
     'test_scope_fallback', '#935 shared scoped check rejects generic markers'),
    ('C', 'bare-step', "and not marker.rstrip().endswith(': step')", 'and True',
     'test_bare_step_scope', '#935 bare step prefix never kills'),
    ('C', 'payload-first-line', 're.fullmatch(case.payload_pattern, payloads[0]) is not None', "re.search(case.payload_pattern, '\\n'.join(lines)) is not None",
     'test_first_line_ownership', '#1110 only one owned first-line message counts'),
    ('C', 'payload-substring', 're.fullmatch(case.payload_pattern, payloads[0]) is not None', 'case.assertion in payloads[0]',
     'test_real_legacy_payload_shapes', '#935 incidental first-line substring cannot kill'),
    ('A', 'oracle-step', "or rows[0]['steps'][index] != step", 'or False',
     'test_submit_oracle_binding', '#935 wrong case index or step must be refused'),
    ('A', 'baseline-commit', "if result.returncode != 0 or result.stdout.strip() != BASELINE_COMMIT:", 'if False:',
     'test_baseline_commit', '#782 wrong baseline commit is refused'),
    ('A', 'baseline-path', 'env = C.environment(environ)', 'env = dict(environ)',
     'test_baseline_paths_and_override', '#782 path and HEAD-only source constraints are enforced'),
    ('A', 'git-object-override', "if 'PDRS_NEGATIVE_BASELINE' in env:", 'if False:',
     'test_baseline_paths_and_override', '#782 path and HEAD-only source constraints are enforced'),
    ('A', 'unwrap-start', "if (not isinstance(before, str) or not before.startswith('unwrap_or_else(')\n            or not isinstance(after, str) or not after.startswith('unwrap()')):", 'if False:',
     'test_unwrap_invariants', '#835 location derivation rejects shifted or ambiguous unwrap spans'),
    ('A', 'unwrap-unique', 'if text.count(before) != 1:', 'if False:',
     'test_unwrap_invariants', '#835 location derivation rejects shifted or ambiguous unwrap spans'),
    ('A', 'unicode-column', "len(head.rsplit(chr(10), 1)[-1])", "len(head.rsplit(chr(10), 1)[-1].encode())",
     'test_unwrap_invariants', '#835 Rust columns count Unicode code points'),
    ('C', 'poison-location', "and lines[headers[-1][0]].endswith(' panicked at ' + case.assertion + ':')", 'and True',
     'test_poison_binding', '#835 poison requires both payloads and exact site'),
    ('C', 'positive-inventory', 'and set(passed) == expected', 'and True',
     'test_positive_inventory', '#1110 positive baseline checks complete exact inventory'),
    ('A', 'stale-anchor', 'if not entry.before or entry.path.read_text().count(entry.before) != 1:', 'if False:',
     'test_complete_inventory', '#1110 stale mutation anchors refuse before execution'),
    ('V', 'legacy-marker', "assertion = required_string(row['anchor'], field + '.anchor')", "assertion = row['anchor'] if row['anchorKind'] == 'location' else 'constant'",
     'test_real_raw_summary_adapters', '#1110 adapter preserves legacy binding markers'),
    ('V', 'legacy-unknown', "if name not in pure_names | submit_names.keys() | {'baseline-pure', 'baseline-submit'}:", 'if False:',
     'test_summary_rejects_unknown_or_contradictory', '#1110 unknown or contradictory legacy verdict is refused'),
    ('V', 'legacy-kind', "or row['anchorKind'] != ('location' if location else 'message')", 'or False',
     'test_summary_rejects_unknown_or_contradictory', '#1110 turn verdict types exits and anchor kinds are preserved'),
    ('V', 'legacy-turn-exit', "if expected_exit is not None and (type(row['exit']) is not int or row['exit'] != expected_exit):", 'if False:',
     'test_summary_rejects_unknown_or_contradictory', '#1110 turn verdict types exits and anchor kinds are preserved'),
    ('V', 'guard-exit-type', "or type(row['exit']) is not int or row['exit'] != (0 if baseline else 101)",
     "or row['exit'] != (0 if baseline else 101)", 'test_summary_rejects_unknown_or_contradictory',
     '#1110 failure-guard exit code cannot be a boolean'),
    ('V', 'guard-survival', "                status = 'SURVIVED'", "                status = 'INVALID'",
     'test_legacy_guard_unsuccessful_states', '#1110 legacy passed mutant remains SURVIVED'),
    ('V', 'guard-invalid', "            status = 'INVALID'\n            if not baseline", "            status = 'KILLED'\n            if not baseline",
     'test_legacy_guard_unsuccessful_states', '#1110 failed legacy evidence remains INVALID'),
    ('V', 'guard-survival-exit-type', "if type(row['exit']) is not int or row['exit'] != 0 or row['timed_out'] or not row['reaped']:",
     "if row['exit'] != 0 or row['timed_out'] or not row['reaped']:", 'test_legacy_guard_unsuccessful_states',
     '#1110 legacy survival requires an integer zero and owned completion'),
    ('A', 'baseline-exception', '            except BaseException:\n                failed_baselines.update(binding.positive_tests)',
     '            except BaseException:\n                pass', 'test_baseline_exception_dominates',
     '#1110 baseline exception invalidates dependent mutations'),
    ('A', 'restored-after-error', "        restored = cargo(binding.mutation, 'restored', binding.test, run_env)",
     "        if failure is not None:\n            raise failure\n        restored = cargo(binding.mutation, 'restored', binding.test, run_env)",
     'test_executor_lifecycle', '#1110 failed compilation or execution still runs restored positive'),
    ('CHECK', 'runtime-inventory', 'if len(rows) != len(declared) or [row[0] for row in rows] != [row[0] for row in declared]:',
     'if False:', 'test_runtime_anchor_drift', '#1110 scanner rejects executed case mutation or location drift'),
    ('CHECK', 'runtime-mutation', 'if actual[1:4] != [str(target), declaration[1], declaration[2]]:',
     'if False:', 'test_runtime_anchor_drift', '#1110 scanner rejects executed case mutation or location drift'),
    ('CHECK', 'runtime-location', "if row[4:] != ['location', derived]:", 'if False:',
     'test_runtime_anchor_drift', '#1110 scanner rejects executed case mutation or location drift'),
)


def bound(method, slot, module, output, label):
    previous = getattr(tests, slot)
    setattr(tests, slot, module)
    stream = io.StringIO()
    try:
        with redirect_stdout(stream), redirect_stderr(stream):
            result = unittest.TextTestRunner(stream=stream, verbosity=2, resultclass=BoundResult).run(
                unittest.TestSuite([tests.MigrationTests(method)]))
    finally:
        setattr(tests, slot, previous)
    with (output / (label + '.log')).open('x') as log:
        log.write(stream.getvalue())
    return result


def controls(output):
    output.mkdir(parents=True, exist_ok=False)
    rows = []
    for slot, name, before, after, method, marker in VARIANTS:
        original = getattr(tests, slot)
        source = Path(original.__file__).read_text()
        identity = 'test_submit_migration.MigrationTests.' + method
        row = dict(mutation=name, test=identity, assertion=marker, status='INVALID', reason='not run', hits=[])
        module_name = '_m6_migration_mutant'
        try:
            if source.count(before) != 1:
                raise ValueError(f'stale anchor for {name}: {source.count(before)}')
            code = compile(source.replace(before, after, 1), original.__file__, 'exec')
            module = ModuleType(module_name)
            module.__file__ = original.__file__
            sys.modules[module_name] = module
            exec(code, module.__dict__)
            positive = bound(method, slot, original, output, name + '-positive')
            negative = bound(method, slot, module, output, name + '-negative')
            restored = bound(method, slot, original, output, name + '-restored')
            healthy = positive.wasSuccessful() and restored.wasSuccessful()
            killed = (negative.testsRun == 1 and not negative.errors and len(negative.assertions) == 1
                      and negative.assertions[0][0] == identity
                      and issubclass(negative.assertions[0][1][0], AssertionError)
                      and str(negative.assertions[0][1][1]).endswith(' : ' + marker))
            if healthy and killed:
                row.update(status='KILLED', reason='compiled; bound assertion failed; restored passed', hits=[identity])
            elif healthy and negative.wasSuccessful():
                row.update(status='SURVIVED', reason='bound test passed')
            else:
                row['reason'] = 'unrelated failure or failed positive; see raw logs'
        except Exception as error:
            row['reason'] = f'{type(error).__name__}: {error}'
        finally:
            sys.modules.pop(module_name, None)
        rows.append(row)
        print(f'{name}: {row["status"]}: {row["reason"]}', flush=True)
    tests.C.publish_summary(output, {'controls': rows, 'mode': 'full', 'total': len(VARIANTS),
                                    'selected': [row['mutation'] for row in rows]})
    return int(any(row['status'] != 'KILLED' for row in rows))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('output', nargs='?', type=Path)
    args = parser.parse_args()
    if args.output:
        return controls(args.output)
    with tempfile.TemporaryDirectory(prefix='m6-migration-negative-') as directory:
        return controls(Path(directory) / 'evidence')


if __name__ == '__main__':
    raise SystemExit(main())
