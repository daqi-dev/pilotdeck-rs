#!/usr/bin/env python3
# anchor-selfcheck: unsupported meta-controls compile in-memory contract mutants, no source writes
"""Remove each protection, compile, run bound unittest, then restore the module.

No git worktree, product mutation, cargo target or HEAD-guard bypass is used.
Every variant runs even if an earlier variant is INVALID. Stale anchors and
unrelated exceptions are INVALID, never SURVIVED or KILLED.
"""
import argparse
from contextlib import redirect_stderr, redirect_stdout
import io
import json
from pathlib import Path
import sys
import tempfile
from types import ModuleType
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parent))
import test_contract as tests


# (name, unique old text, replacement, bound test, exact assertion message)
VARIANTS = (
    ('empty-cases', 'if not table:', 'if False:', 'test_empty_cases_refused',
     '#1107 empty case table has its own admission guard'),
    ('zero-oracle', 'if not tuple(oracle):', 'if False:', 'test_empty_oracle_refused',
     '#1107 zero-row oracle refuses before guard'),
    ('selection', 'if not choices or len(set(choices)) != len(choices) or set(choices) - set(names):',
     'if False:', 'test_selection_validation', '#1107 invalid selection refuses before guard'),
    ('duplicate-cases', 'if len(set(names)) != len(names):', 'if False:',
     'test_selection_validation', '#1107 duplicate cases have their own admission guard'),
    ('sample-mode', "mode = 'full' if len(chosen) == total else 'sample'", "mode = 'full'",
     'test_sample_metadata', '#1107 sample reports exact scope'),
    ('sample-total', 'len(table)\n            mode', 'len(chosen)\n            mode',
     'test_sample_metadata', '#1107 sample reports exact scope'),
    ('sample-selection', "'selected': selected", "'selected': []",
     'test_sample_metadata', '#1107 sample reports exact scope'),
    ('marker-binding', "if not re.fullmatch(r'#[0-9]+ \\S[^\\r\\n]*', case.assertion):",
     'if False:', 'test_multiline_and_unbound_markers_refused',
     '#1107 assertion must be an issue-specific single line'),
    ('marker-newline', "_line(getattr(case, field), field)", 'pass',
     'test_multiline_and_unbound_markers_refused', '#1107 assertion must be an issue-specific single line'),
    ('environment', 'env = environment(os.environ if environ is None else environ)',
     'env = dict(os.environ if environ is None else environ)', 'test_environment_required',
     '#1107 environment locations are explicit absolute paths'),
    ('environment-roots', "if name != 'CARGO_TARGET_DIR' and not Path(value).is_dir():",
     'if False:', 'test_environment_required', '#1107 environment roots must exist'),
    ('target-override', "env.setdefault('CARGO_PROFILE_DEV_DEBUG', 'line-tables-only')",
     "env['CARGO_TARGET_DIR'] = '/tmp/wrong-target'", 'test_environment_preserved',
     '#1107 caller target directory is preserved'),
    ('nonempty-admission', 'if any(path.iterdir()):', 'if False:',
     'test_nonempty_output_untouched', '#1107 nonempty output refuses before guard'),
    ('claim-exclusive', "(path / '.claim').open('x',", "(path / '.claim').open('w',",
     'test_claim_race_is_exclusive', '#1107 claim race has a single winner'),
    ('sigint-default', 'signal.signal(signal.SIGINT, signal.default_int_handler)',
     'signal.getsignal(signal.SIGINT)', 'test_sigint_default_and_restore', '#1107 runner uses sigint-default'),
    ('sigint-restore', 'signal.signal(signal.SIGINT, previous)', 'pass',
     'test_sigint_default_and_restore', '#1107 prior signal handler is restored'),
    ('exit-pre-entry', 'code = 0 if success else (2 if entered else 1)',
     'code = 0 if success else 2', 'test_pre_entry_errors_exit_one',
     '#1107 pre-entry failures exit one by entered flag'),
    ('exit-post-entry', 'code = 0 if success else (2 if entered else 1)',
     'code = 0 if success else 1', 'test_restore_failure_exits_two',
     '#1107 restore failures exit two by entered flag'),
    ('fatal-invalidation', "row.update(status='INVALID', reason=row['reason'] + '; ' + reason, hits=[])",
     'pass', 'test_fatal_revokes_completed_and_pending',
     '#1107 fatal exit revokes every completed and pending row'),
    ('fatal-hit-revocation', "reason=row['reason'] + '; ' + reason, hits=[])",
     "reason=row['reason'] + '; ' + reason, hits=row['hits'])",
     'test_fatal_revokes_completed_and_pending', '#1107 invalidation clears prior hits'),
    ('suppressed-cancellation', "failure = f'{type(error).__name__}: {error}'\n                    raise",
     "failure = ''\n                    raise", 'test_guard_suppression_cannot_hide_cancellation',
     '#1107 suppressed cancellation has no summary'),
    ('cleanup-before-publication', "                if owner:", "                if False:",
     'test_sigint_restore_failure_does_not_publish', '#1107 cleanup failure cannot publish completed rows'),
    ('publication-sigint', '            if not failure:',
     '            signal.signal(signal.SIGINT, signal.SIG_IGN)\n            if not failure:',
     'test_sigint_during_publication', '#1107 publication still obeys sigint-default'),
    ('early-publication', '            with guard():',
     '            publish_summary(out, _document(rows, mode, names, total))\n            with guard():',
     'test_publication_after_restore', '#1107 healthy run exits zero'),
    ('non-atomic-publication', "temporary = output / 'summary.json.tmp'", "temporary = output / 'summary.json'",
     'test_publication_after_restore', '#1107 healthy run exits zero'),
    ('publication-rollback', 'if identity == (stat.st_dev, stat.st_ino):', 'if False:',
     'test_publication_failure_is_closed', '#1107 failed publication removes owned temp'),
    ('publication-foreign-unlink', 'if identity == (stat.st_dev, stat.st_ino):', 'if True:',
     'test_publication_does_not_delete_foreign_inode', '#1107 publication rollback only removes its own inode'),
    ('abnormal-exit', "    if result.returncode not in (0, 101):\n        return 'INVALID', 'abnormal exit (signal, launch or non-test failure)', []",
     '    result.returncode = 101 if result.returncode != 0 else 0',
     'test_abnormal_exit_invalid', '#1107 signal and abnormal exits never kill'),
    ('exact-test', "if running != ['1'] or len(named) != 1 or named[0][0] != case.test:",
     'if False:', 'test_named_test_required', '#1107 kill requires exactly one named failure'),
    ('panic-ownership', 'or headers[0][1][1] != case.test:', 'or False:',
     'test_exclusive_panic_marker_required', '#1107 marker belongs to the named panic'),
    ('assertion-marker', 'if payload != case.assertion and not re.fullmatch(', 'if False and not re.fullmatch(',
     'test_exclusive_panic_marker_required', '#1107 marker belongs to the first assertion payload line'),
    ('watchdog', "if any(line.lstrip().startswith(HANG_PREFIXES) for line in body):",
     'if False:', 'test_timeout_watchdog_invalid', '#1107 timeout watchdog panic never kills'),
    ('continue-all', "row.update(status='INVALID', reason=f'{type(error).__name__}: {error}',\n                                       hits=[])",
     "row.update(status='INVALID', reason=str(error), hits=[])\n                            break",
     'test_exceptions_continue_all_cases', '#1107 ordinary case errors do not abort remaining cases'),
    ('late-publication-rollback', "                if owner:", "                if False:",
     'test_cancellation_after_publisher_returns', '#1107 outer finalizer revokes published summary'),
    ('watchdog-data-confusion', "if any(line.lstrip().startswith(HANG_PREFIXES) for line in body):",
     "if any('timeout' in line or 'watchdog' in line for line in body):",
     'test_assertion_data_can_contain_timeout_words', '#1107 assertion data is not a watchdog diagnostic'),
    ('panic-payload-tail', "        if re.match(r'^(?:stack backtrace:", "        if False and re.match(r'^(?:stack backtrace:",
     'test_backtrace_is_not_panic_payload', '#1107 backtrace and report text are not panic payload'),
    ('counts', "sum(row['status'] == status for row in rows)", "0",
     'test_counts_match_rows', '#1107 counts exactly reflect every verdict'),
    ('canonical-schema', 'dict(mutation=case.mutation, test=case.test, assertion=case.assertion,',
     'dict(name=case.mutation, test=case.test, assertion=case.assertion,',
     'test_canonical_schema_and_equivalence', '#1107 summary rows use the canonical comparator schema'),
)


class BoundResult(unittest.TextTestResult):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self.assertions = []

    def addFailure(self, test, error):
        super().addFailure(test, error)
        self.assertions.append((test.id(), error))


def run_bound(method, module, output, label):
    prior = tests.C
    tests.C = module
    stream = io.StringIO()
    try:
        with redirect_stdout(stream), redirect_stderr(stream):
            result = unittest.TextTestRunner(stream=stream, verbosity=2, resultclass=BoundResult).run(
                unittest.TestSuite([tests.ContractTests(method)]))
    finally:
        tests.C = prior
    with (output / (label + '.log')).open('x') as log:
        log.write(stream.getvalue())
    return result


def controls(output, selected=None):
    output.mkdir(parents=True, exist_ok=False)
    source = Path(tests.C.__file__).read_text()
    original = tests.C
    chosen = [item for item in VARIANTS if selected is None or item[0] in selected]
    if not chosen or (selected is not None and set(selected) - {item[0] for item in chosen}):
        raise ValueError('empty/unknown variant selection')
    rows = []
    for name, before, after, method, marker in chosen:
        identity = 'test_contract.ContractTests.' + method
        row = dict(mutation=name, test=identity, assertion=marker, status='INVALID', reason='not run', hits=[])
        module_name = '_negkit_contract_mutant'
        try:
            if source.count(before) != 1:
                raise ValueError(f'stale anchor: expected 1, got {source.count(before)}: {before!r}')
            code = compile(source.replace(before, after, 1), str(Path(original.__file__)), 'exec')
            module = ModuleType(module_name)
            module.__file__ = original.__file__
            sys.modules[module_name] = module
            exec(code, module.__dict__)
            positive = run_bound(method, original, output, name + '-positive')
            negative = run_bound(method, module, output, name + '-negative')
            restored = run_bound(method, original, output, name + '-restored')
            healthy = positive.wasSuccessful() and restored.wasSuccessful()
            bound = (negative.testsRun == 1 and not negative.errors and len(negative.assertions) == 1
                     and negative.assertions[0][0] == identity
                     and issubclass(negative.assertions[0][1][0], AssertionError)
                     and str(negative.assertions[0][1][1]).endswith(' : ' + marker))
            if healthy and bound:
                row.update(status='KILLED', reason='compiled; exact bound assertion failed; restored passed', hits=[identity])
            elif healthy and negative.wasSuccessful():
                row.update(status='SURVIVED', reason='bound test still passed')
            else:
                row['reason'] = 'positive/restored failure or unrelated mutant failure; see raw logs'
        except Exception as error:
            row['reason'] = f'{type(error).__name__}: {error}'
        finally:
            sys.modules.pop(module_name, None)
        rows.append(row)
        print(f'{name}: {row["status"]}: {row["reason"]}', flush=True)
    document = {'mode': 'full' if len(chosen) == len(VARIANTS) else 'sample',
                'selected': [row['mutation'] for row in rows], 'total': len(VARIANTS), 'controls': rows}
    original.publish_summary(output, document)
    print('summary: ' + ', '.join(f'{status}={sum(row["status"] == status for row in rows)}'
                                 for status in ('KILLED', 'SURVIVED', 'INVALID')))
    return int(any(row['status'] != 'KILLED' for row in rows))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('output', nargs='?', type=Path)
    parser.add_argument('variants', nargs='*')
    args = parser.parse_args()
    if args.output is not None:
        return controls(args.output, args.variants or None)
    with tempfile.TemporaryDirectory(prefix='negkit-contract-controls-') as directory:
        return controls(Path(directory) / 'evidence')


if __name__ == '__main__':
    raise SystemExit(main())
