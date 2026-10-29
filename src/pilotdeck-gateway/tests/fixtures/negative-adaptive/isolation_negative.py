#!/usr/bin/env python3
# anchor-selfcheck: unsupported in-memory Python loader mutations, no product source writes
"""#994r: compile Python mutants in memory, bind exact unittest failures, restore.

No source files, Git repository or build directory are copied or mutated.
Every selected control runs before the summary; only its named assertion kills.
"""
import argparse
import contextlib
import importlib.machinery
import importlib.util
import io
import json
import os
from pathlib import Path
import sys
import unittest
from unittest.mock import patch

HERE = Path(__file__).resolve().parent
FIXTURES = HERE.parent
ADAPTIVE = HERE / 'negative_controls.py'
NETWORK = FIXTURES / 'network-timing/negative_controls.py'
CONTROLS = [
    ('inherit-pythonpath', ADAPTIVE, "        env.pop('PYTHONPATH', None)",
     '        pass  # retain caller PYTHONPATH',
     'IsolationTests.test_copied_children_ignore_caller_python_paths',
     '#994 copied children must reject caller PYTHONPATH'),
    ('inherit-pythonhome', ADAPTIVE, "        env.pop('PYTHONHOME', None)",
     '        pass  # retain caller PYTHONHOME',
     'IsolationTests.test_copied_children_ignore_caller_python_paths',
     '#994 all copied child phases must clear PYTHONHOME'),
    ('omit-transitive-imports', ADAPTIVE, 'pending.append(dependency)',
     "pending.extend([dependency] if dependency in ('_shared.negative_control', 'negative_process', 'test_negative_adaptive') else [])",
     'IsolationTests.test_new_shared_imports_are_copied_and_executable',
     '#994 import closure must include transitive relative and function imports'),
    ('three-file-copy', ADAPTIVE,
     '        for path, data in contents.items():\n            copied =',
     '        for path, data in contents.items():\n            if path not in (shared, process, tests):\n                continue\n            copied =',
     'IsolationTests.test_new_shared_imports_are_copied_and_executable', '#994 new shared dependency must be copied'),
    ('skip-bootstrap-check', ADAPTIVE, 'if not path.is_file() or path.read_bytes() != data:',
     'if False and (not path.is_file() or path.read_bytes() != data):',
     'IsolationTests.test_bootstrap_rejects_dirty_code_before_execution_and_ignores_cached_module',
     '#994 dirty bootstrap must never yield'),
    ('trust-cached-module', ADAPTIVE, 'name: sys.modules.pop(name)', 'name: sys.modules[name]',
     'IsolationTests.test_bootstrap_rejects_dirty_code_before_execution_and_ignores_cached_module',
     '#994 bootstrap must not execute cached unverified helpers'),
    ('allow-unknown-import', ADAPTIVE, 'if absolute not in names and', 'if False and absolute not in names and',
     'IsolationTests.test_bootstrap_refuses_unverified_dependencies_even_if_cached',
     '#994 unverified dependency must never load'),
    ('empty-controls-pass', ADAPTIVE, 'return int(not records or len(records) != len(cases) or',
     'return int(len(records) != len(cases) or',
     'IsolationTests.test_empty_controls_fail_closed', '#994 empty controls must fail closed'),
    ('ignore-head-change', ADAPTIVE, "if git_bytes('rev-parse', 'HEAD').decode().strip() != commit:",
     "if False and git_bytes('rev-parse', 'HEAD').decode().strip() != commit:",
     'IsolationTests.test_head_change_invalidates_copy_without_rewriting_inputs',
     '#994 HEAD movement must invalidate isolated controls'),
    ('omit-candidate-commit', NETWORK, "commit=git('rev-parse', 'HEAD').strip()", "commit='unrecorded'",
     'CandidateBaselineTests.test_identity_distinguishes_clean_and_dirty_candidates',
     '#994 provenance must distinguish dirty candidates'),
    ('hide-dirty-candidate', NETWORK, 'candidateDirty=bool(dirty)', 'candidateDirty=False',
     'CandidateBaselineTests.test_identity_distinguishes_clean_and_dirty_candidates',
     '#994 provenance must distinguish dirty candidates'),
    ('omit-untracked-provenance', NETWORK, "'--untracked-files=all'", "'--untracked-files=no'",
     'CandidateBaselineTests.test_identity_distinguishes_clean_and_dirty_candidates',
     '#994 candidate provenance must include all src changes'),
    ('skip-candidate-lock', NETWORK, 'with tree_lock(root, timeout=BASELINE_LOCK_SECONDS):',
     "with __import__('contextlib').nullcontext():",
     'CandidateBaselineTests.test_candidate_holds_source_lock_during_cargo',
     '#994 locked candidate must pass its bound test'),
]
for key in ('PDRS_RECORDER_ORACLE', 'PDRS_SESSION_DOMAIN_ORACLE', 'PDRS_TRANSCRIPT_ADAPTER_ORACLE'):
    CONTROLS.append((f'inherit-{key.lower()}', NETWORK, 'env = environment(output)',
                     f"env = environment(output)\n    env['{key}'] = 'injected'",
                     'CandidateBaselineTests.test_candidate_environment_clears_oracles_and_wrapper',
                     '#994 candidate must clear oracle injection'))
CONTROLS.append(('inherit-build-wrapper', NETWORK, 'env = environment(output)',
                 "env = environment(output)\n    env['RUSTC_WRAPPER'] = 'injected'",
                 'CandidateBaselineTests.test_candidate_environment_clears_oracles_and_wrapper',
                 '#994 candidate must use normalized build environment'))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('output', type=Path)
    parser.add_argument('controls', nargs='*')
    args = parser.parse_args()
    if not os.environ.get('CARGO_TARGET_DIR'):
        parser.error('CARGO_TARGET_DIR must be supplied by the caller (no Cargo is launched)')
    selected = [row for row in CONTROLS if not args.controls or row[0] in args.controls]
    if args.controls and (len(selected) != len(args.controls) or len(set(args.controls)) != len(args.controls)):
        parser.error('unknown/duplicate control')
    args.output.mkdir(parents=True, exist_ok=False)
    spec = importlib.util.spec_from_file_location('isolation_control_tests', FIXTURES / 'test_negative_adaptive_isolation.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    records = []
    original_get_code = importlib.machinery.SourceFileLoader.get_code
    for name, path, before, after, test, marker in selected:
        row = dict(name=name, test=f'{module.__name__}.{test}', assertion=marker,
                   status='INVALID', reason='not run', compiled=False, restoredPass=False)
        def run(label, code=None):
            def get_code(loader, fullname):
                if Path(loader.path).resolve() == path and code is not None:
                    return code
                return original_get_code(loader, fullname)
            stream = io.StringIO()
            with patch.object(importlib.machinery.SourceFileLoader, 'get_code', get_code), \
                    contextlib.redirect_stdout(stream):
                suite = unittest.defaultTestLoader.loadTestsFromName(test, module)
                result = unittest.TextTestRunner(stream=stream, verbosity=2).run(suite)
            (args.output / f'{name}-{label}.log').write_text(stream.getvalue())
            return result
        def passed(result):
            return result.testsRun == 1 and result.wasSuccessful() and not result.skipped
        try:
            text = path.read_text()
            if text.count(before) != 1:
                raise ValueError('#994 stale Python mutation anchor')
            code = compile(text.replace(before, after, 1), str(path), 'exec')
            row['compiled'] = True
            if not passed(run('positive')):
                raise RuntimeError('#994 bound positive test did not pass')
            result = run('negative', code)
            failures = [(item.id().split(' (', 1)[0], trace) for item, trace in result.failures]
            if (result.testsRun == 1 and not result.errors and failures
                    and all(full_name == row['test'] and marker in trace for full_name, trace in failures)):
                row.update(status='KILLED', reason='exact bound unittest assertion')
            elif passed(result):
                row.update(status='SURVIVED', reason='bound unittest passed')
            else:
                row['reason'] = 'unrelated error or missing bound assertion'
        except Exception as error:
            row['reason'] = f'{type(error).__name__}: {error}'
        finally:
            try:
                row['restoredPass'] = passed(run('restored'))
            except Exception as error:
                row['reason'] += f'; restore error: {type(error).__name__}: {error}'
            if not row['restoredPass']:
                row.update(status='INVALID', reason=row['reason'] + '; restored test failed')
        records.append(row)
        print(f'{name}: {row["status"]}: {row["reason"]}', flush=True)
    counts = {status: sum(row['status'] == status for row in records)
              for status in ('KILLED', 'SURVIVED', 'INVALID')}
    (args.output / 'summary.json').write_text(json.dumps(dict(controls=records, counts=counts), indent=2) + '\n')
    print(f'summary: {counts}', flush=True)
    return int(not records or any(row['status'] != 'KILLED' for row in records))


if __name__ == '__main__':
    raise SystemExit(main())
