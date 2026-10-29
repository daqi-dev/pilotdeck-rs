#!/usr/bin/env python3
"""#991 mutations for the one entry guard; every mutant is compiled first.

The historical failure mode was N copied bootstraps with one stale copy.  This
suite therefore mutates `_guard.py` (the only implementation) plus one entry's
preamble/dispatch (the machine check that keeps entries from re-copying it),
runs the bound test_safety case, restores the bytes and then runs the same
positive case again.  Every selected mutant is recorded as KILLED / SURVIVED /
INVALID and the script exits non-zero unless every record is KILLED.

`mutating_sources` refuses a non-pristine source and holds the ruler's parity
lease; this file itself must be committed before it can run (HEAD baseline).
Use: python3 -I -B .../negative_controls.py --output <new directory>.
"""
import argparse
import io
import json
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import unittest

sys.dont_write_bytecode = True
sys.pycache_prefix = tempfile.mkdtemp(prefix='pdrs-991-negative-pyc-')
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

ROOT = Path(__file__).resolve().parents[5]
GUARD = ROOT / 'scripts/gateway-parity/_guard.py'
COMMON = ROOT / 'scripts/gateway-parity/common.py'
BUILD = ROOT / 'scripts/gateway-parity/build_proxy.py'
COMPARE = ROOT / 'scripts/gateway-parity/compare.py'
SAFETY = ROOT / 'scripts/gateway-parity/test_safety.py'
CLEANUP = ROOT / 'src/pilotdeck-gateway/tests/fixtures/session-domain-cleanup/check_cleanup.py'

# (name, source, before, after): the static checker and the runner share this
# table; every before must occur exactly once or the anchor is stale.
MUTATIONS = [
    ('guard-aggregation-registration-removed', CLEANUP,
     "    ('scripts/gateway-parity/_guard.py', '_failure'): 'guard-failure',\n", ''),
    ('guard-round-aggregation-bypassed', GUARD,
     "return {'status': 'BLOCKED', 'error': str(error)}, common.aggregate_status('FAIL')[1]",
     "return {'status': 'BLOCKED', 'error': str(error)}, common.BATCH_EXIT_CODES['FAIL']"),
    ('guard-session-aggregation-bypassed', GUARD,
     "return {'status': 'BLOCKED', 'error': str(error)}, common.aggregate_status('BLOCKED')[1]",
     "return {'status': 'BLOCKED', 'error': str(error)}, common.BATCH_EXIT_CODES['BLOCKED']"),
    ('guard-ruler-aggregation-bypassed', GUARD,
     "        status, code = common.aggregate_status('BLOCKED' if blocked else 'FAIL')",
     "        status = 'BLOCKED' if blocked else 'FAIL'\n        code = common.BATCH_EXIT_CODES[status]"),
    ('guard-controls-aggregation-bypassed', GUARD,
     "'detail': str(error)}}, common.aggregate_status('BLOCKED')[1]",
     "'detail': str(error)}}, common.BATCH_EXIT_CODES['BLOCKED']"),
    ('guard-preflight-removed', GUARD,
     '        verify_cli_imports()\n', '        pass  # mutant skips preflight\n'),
    ('guard-preflight-exit-one', GUARD,
     '        raise SystemExit(2)',
     '        raise SystemExit(1)'),
    ('guard-preflight-except-narrowed', GUARD,
     "    except (Exception, KeyboardInterrupt) as error:\n"
     "        print(json.dumps({'status': 'BLOCKED', 'error': str(error)}, ensure_ascii=False))",
     "    except RuntimeError as error:\n"
     "        print(json.dumps({'status': 'BLOCKED', 'error': str(error)}, ensure_ascii=False))"),
    ('guard-main-exit-zero', GUARD,
     '        raise SystemExit(code)\n    raise SystemExit(code)',
     '        raise SystemExit(code)\n    raise SystemExit(0)'),
    ('guard-main-except-narrowed', GUARD,
     '    except (Exception, KeyboardInterrupt) as error:\n        failure = _failure(error, policy, common)',
     '    except Exception as error:\n        failure = _failure(error, policy, common)'),
    ('guard-cancel-is-fail', GUARD,
     '(PermissionError, TimeoutError, KeyboardInterrupt)',
     '(PermissionError, TimeoutError)'),
    ('guard-evidence-is-blocked', GUARD,
     '        blocked = isinstance(error, (PermissionError, TimeoutError, KeyboardInterrupt))',
     '        blocked = True'),
    ('guard-error-rule-lost', GUARD,
     "        return {'status': status, 'failure': {'rule': 'EVIDENCE', 'detail': str(error)}}, code",
     "        return {'status': status, 'failure': {'rule': 'APPARATUS_IMPORTS', 'detail': str(error)}}, code"),
    ('guard-load-cache-open', GUARD,
     "if not isinstance(getattr(existing, '_apparatus_source', None), bytes):",
     "if False and not isinstance(getattr(existing, '_apparatus_source', None), bytes):"),
    ('guard-load-provenance-late', GUARD,
     '    module._apparatus_source = source',
     '    module._apparatus_source = path.read_bytes()'),
    ('guard-load-pop-removed', GUARD,
     '        sys.modules.pop(name, None)\n        raise',
     '        pass  # mutant retains partial module\n        raise'),
    ('guard-verifier-skips', GUARD,
     '    trusted.verify_loaded_apparatus(head)\n    return head',
     '    return head  # mutant skips the HEAD verifier'),
    ('guard-static-head-required', GUARD,
     '    if not _has_git_checkout(root):',
     '    if False and not _has_git_checkout(root):'),
    ('guard-checkout-ancestors', GUARD,
     '    for directory in (root, *root.parents):', '    for directory in (root,):'),
    ('guard-checkout-symlink', GUARD,
     "            (directory / '.git').lstat()", "            (directory / '.git').stat()"),
    ('guard-checkout-read-errors', GUARD,
     '        except FileNotFoundError:\n            continue',
     '        except OSError:\n            continue'),
    ('guard-checkout-env-ignored', GUARD,
     "    if 'GIT_DIR' in os.environ:", "    if False and 'GIT_DIR' in os.environ:"),
    ('entry-main-wrap-restored', BUILD,
     "if __name__ == '__main__':\n    run_entry(main, 'traceback')",
     "if __name__ == '__main__':\n    try:\n        run_entry(main, 'traceback')\n"
     "    except Exception:\n        raise SystemExit(1)"),
    ('entry-private-verifier-restored', COMPARE,
     'def main():',
     'def _verify_cli_imports():\n    return None\n\n\ndef main():'),
    ('entry-import-before-isolated', COMPARE,
     '# >>> apparatus bootstrap (verbatim in every A2 entry; test_safety checks it)',
     'import sys\nimport argparse\n'
     '# >>> apparatus bootstrap (verbatim in every A2 entry; test_safety checks it)'),
    ('policy-build_proxy-unified', ROOT / 'scripts/gateway-parity/build_proxy.py',
     "    run_entry(main, 'traceback')",
     "    run_entry(main, 'ruler')"),
    ('policy-check_frozen-unified', ROOT / 'scripts/gateway-parity/check_frozen.py',
     "    run_entry(main, 'traceback')",
     "    run_entry(main, 'ruler')"),
    ('policy-check_ruler-unified', ROOT / 'scripts/gateway-parity/check_ruler.py',
     "    run_entry(main, 'ruler')",
     "    run_entry(main, 'session')"),
    ('policy-compare-unified', ROOT / 'scripts/gateway-parity/compare.py',
     "    run_entry(main, 'traceback')",
     "    run_entry(main, 'ruler')"),
    ('policy-observer-unified', ROOT / 'scripts/gateway-parity/observer.py',
     "    run_entry(main, 'traceback')",
     "    run_entry(main, 'ruler')"),
    ('policy-run_round-unified', ROOT / 'scripts/gateway-parity/run_round.py',
     "    run_entry(main, 'round')",
     "    run_entry(main, 'ruler')"),
    ('policy-session_domain-unified', ROOT / 'scripts/gateway-parity/session_domain.py',
     "    run_entry(main, 'session')",
     "    run_entry(main, 'ruler')"),
    ('policy-controls-run_all-unified', ROOT / 'scripts/gateway-parity/controls/run_all.py',
     "    run_entry(main, 'controls')",
     "    run_entry(main, 'ruler')"),
    ('policy-controls-session_domain-unified', ROOT / 'scripts/gateway-parity/controls/session_domain.py',
     "    run_entry(main, 'traceback')",
     "    run_entry(main, 'ruler')"),
    ('guard-round-catches-interrupt', GUARD,
     '        if isinstance(error, KeyboardInterrupt):',
     '        if False and isinstance(error, KeyboardInterrupt):'),
    ('guard-controls-matches-rule-only', GUARD,
     '        if isinstance(error, common.ApparatusVerificationError):',
     "        if getattr(error, 'rule', None) == 'APPARATUS_IMPORTS':"),
    ('guard-ruler-subprocess-timeout-blocked', GUARD,
     '(PermissionError, TimeoutError, KeyboardInterrupt)',
     '(PermissionError, TimeoutError, subprocess.TimeoutExpired, KeyboardInterrupt)'),
    ('guard-round-exit-literal', GUARD,
     "return {'status': 'BLOCKED', 'error': str(error)}, common.aggregate_status('FAIL')[1]",
     "return {'status': 'BLOCKED', 'error': str(error)}, 1"),
    ('guard-preflight-untrusted-exit-map', GUARD,
     '        raise SystemExit(2)',
     "        raise SystemExit(common.BATCH_EXIT_CODES['BLOCKED'])"),
    ('entry-main-alternate-dispatch', BUILD,
     "    run_entry(main, 'traceback')",
     "    run_entry(main, 'traceback')\nelse:\n    pass"),
    ('entry-new-a2-with-private-dispatch', COMMON,
     "'controls/run_all.py', 'controls/prerequisites.py', 'driver.mts', 'mock.mjs',",
     "'controls/run_all.py', 'controls/prerequisites.py', 'driver.mts', 'mock.mjs', 'check_schema.py',"),
    ('observer-preflight-removed', ROOT / 'scripts/gateway-parity/observer.py',
     "    run_entry(main, 'traceback')",
     '    raise SystemExit(main())'),
    ('observer-preflight-exit-one', ROOT / 'scripts/gateway-parity/observer.py',
     "    run_entry(main, 'traceback')",
     "    try:\n        run_entry(main, 'traceback')\n    except SystemExit as error:\n        raise SystemExit(1 if error.code == 2 else error.code)"),
    ('observer-normal-exit-zero', ROOT / 'scripts/gateway-parity/observer.py',
     "    run_entry(main, 'traceback')",
     "    run_entry(lambda: (main(), 0)[1], 'traceback')"),
    ('entry-suite-parity-lock-bypassed', ROOT / 'src/pilotdeck-gateway/tests/fixtures/negative_process.py',
     'parity = any(relative.startswith("scripts/gateway-parity/")',
     'parity = False and any(relative.startswith("scripts/gateway-parity/")'),
    ('entry-suite-common-not-guarded', Path(__file__).resolve(),
     '    paths = [COMMON, *dict.fromkeys(row[1] for row in selected)]\n    try:',
     '    paths = list(dict.fromkeys(row[1] for row in selected))\n    try:'),
    ('guard-session-exit-literal', GUARD,
     "return {'status': 'BLOCKED', 'error': str(error)}, common.aggregate_status('BLOCKED')[1]",
     "return {'status': 'BLOCKED', 'error': str(error)}, 2"),
    ('guard-ruler-exit-literal', GUARD,
     '}, code',
     "}, 2 if status == 'BLOCKED' else 1"),
    ('guard-controls-exit-literal', GUARD,
     "'detail': str(error)}}, common.aggregate_status('BLOCKED')[1]",
     "'detail': str(error)}}, 2"),
    ('guard-unknown-policy-ignored', GUARD,
     "    raise ValueError(f'#991 unknown entry failure policy: {policy}')",
     '    return None'),
]

CHECKS = {
    'guard-aggregation-registration-removed': (
        'ApparatusInputs.test_guard_failure_is_registered_aggregation_point',
        '#991 guard failure must remain a registered shared aggregation point'),
    'guard-round-aggregation-bypassed': (
        'ImportIsolation.test_guard_exit_codes_use_shared_mapping',
        '#991 guard failure must call the single shared aggregator: round'),
    'guard-session-aggregation-bypassed': (
        'ImportIsolation.test_guard_exit_codes_use_shared_mapping',
        '#991 guard failure must call the single shared aggregator: session'),
    'guard-ruler-aggregation-bypassed': (
        'ImportIsolation.test_guard_exit_codes_use_shared_mapping',
        '#991 guard failure must call the single shared aggregator: ruler'),
    'guard-controls-aggregation-bypassed': (
        'ImportIsolation.test_guard_exit_codes_use_shared_mapping',
        '#991 guard failure must call the single shared aggregator: controls'),
    'guard-preflight-removed': (
        'ImportIsolation.test_tools_cli_preflight_errors_are_blocked',
        '#166 every CLI preflight failure must produce BLOCKED JSON/2'),
    'guard-preflight-exit-one': (
        'ImportIsolation.test_tools_cli_preflight_errors_are_blocked',
        '#166 every CLI preflight failure must produce BLOCKED JSON/2'),
    'guard-preflight-except-narrowed': (
        'ImportIsolation.test_tools_cli_preflight_errors_are_blocked',
        '#166 every CLI preflight failure must produce BLOCKED JSON/2'),
    'guard-main-exit-zero': (
        'ImportIsolation.test_tools_cli_dispatch_preserves_exit_codes',
        '#166 every CLI wrapper must preserve main exit code'),
    'guard-main-except-narrowed': (
        'ImportIsolation.test_ruler_work_signals_are_blocked_after_lease_release',
        '#166 fix-r10 work signals must return BLOCKED/2/EVIDENCE after lease release'),
    'guard-cancel-is-fail': (
        'ImportIsolation.test_ruler_late_validation_errors_are_distinct_from_evidence_failures',
        '#166 ruler late validation errors are BLOCKED/2/APPARATUS_IMPORTS; evidence stays exit 1'),
    'guard-evidence-is-blocked': (
        'ImportIsolation.test_ruler_late_validation_errors_are_distinct_from_evidence_failures',
        '#166 ruler late validation errors are BLOCKED/2/APPARATUS_IMPORTS; evidence stays exit 1'),
    'guard-error-rule-lost': (
        'ImportIsolation.test_ruler_late_validation_errors_are_distinct_from_evidence_failures',
        '#166 ruler late validation errors are BLOCKED/2/APPARATUS_IMPORTS; evidence stays exit 1'),
    'guard-load-cache-open': (
        'ImportIsolation.test_unknown_cache_is_refused',
        '#166 cache without executed bytes must fail closed'),
    'guard-load-provenance-late': (
        'ImportIsolation.test_loader_records_executed_bytes_not_later_disk',
        '#166 provenance must be exactly the compiled bytes'),
    'guard-load-pop-removed': (
        'ImportIsolation.test_failed_import_does_not_poison_cache',
        '#166 failed import must not retain a partial module'),
    'guard-verifier-skips': (
        'ImportIsolation.test_cli_preflight_uses_head_verifier_for_mutated_common',
        '#166 CLI preflight must use HEAD verifier even when disk helper is mutant'),
    'guard-static-head-required': (
        'CheckoutProvenance.test_static_exports_do_not_read_head',
        '#166 static export must not read HEAD'),
    'guard-checkout-ancestors': (
        'CheckoutProvenance.test_checkout_metadata_never_skips_head',
        '#166 checkout metadata must require HEAD'),
    'guard-checkout-symlink': (
        'CheckoutProvenance.test_checkout_metadata_never_skips_head',
        '#166 checkout metadata must require HEAD'),
    'guard-checkout-read-errors': (
        'CheckoutProvenance.test_unreadable_checkout_metadata_fails_closed',
        '#166 unreadable checkout metadata must fail closed'),
    'guard-checkout-env-ignored': (
        'CheckoutProvenance.test_checkout_metadata_never_skips_head',
        '#166 checkout metadata must require HEAD'),
    'entry-main-wrap-restored': (
        'ApparatusInputs.test_cli_main_is_only_the_one_guard_call',
        '#991 __main__ must be exactly run_entry(main, policy)'),
    'entry-private-verifier-restored': (
        'ApparatusInputs.test_cli_main_is_only_the_one_guard_call',
        '#991 compare.py must alias, not redefine, _verify_cli_imports'),
    'entry-import-before-isolated': (
        'ApparatusInputs.test_a2_entries_share_one_verbatim_path_bootstrap',
        '#991 -I check must precede every other import: compare.py'),
    'policy-build_proxy-unified': (
        'ImportIsolation.test_cli_main_failures_preserve_pre_refactor_contract',
        '#991 every entry must preserve its measured main exception contract'),
    'policy-check_frozen-unified': (
        'ImportIsolation.test_cli_main_failures_preserve_pre_refactor_contract',
        '#991 every entry must preserve its measured main exception contract'),
    'policy-check_ruler-unified': (
        'ImportIsolation.test_cli_main_failures_preserve_pre_refactor_contract',
        '#991 every entry must preserve its measured main exception contract'),
    'policy-compare-unified': (
        'ImportIsolation.test_cli_main_failures_preserve_pre_refactor_contract',
        '#991 every entry must preserve its measured main exception contract'),
    'policy-observer-unified': (
        'ImportIsolation.test_cli_main_failures_preserve_pre_refactor_contract',
        '#991 every entry must preserve its measured main exception contract'),
    'policy-run_round-unified': (
        'ImportIsolation.test_cli_main_failures_preserve_pre_refactor_contract',
        '#991 every entry must preserve its measured main exception contract'),
    'policy-session_domain-unified': (
        'ImportIsolation.test_cli_main_failures_preserve_pre_refactor_contract',
        '#991 every entry must preserve its measured main exception contract'),
    'policy-controls-run_all-unified': (
        'ImportIsolation.test_cli_main_failures_preserve_pre_refactor_contract',
        '#991 every entry must preserve its measured main exception contract'),
    'policy-controls-session_domain-unified': (
        'ImportIsolation.test_cli_main_failures_preserve_pre_refactor_contract',
        '#991 every entry must preserve its measured main exception contract'),
    'guard-round-catches-interrupt': (
        'ImportIsolation.test_cli_main_failures_preserve_pre_refactor_contract',
        '#991 every entry must preserve its measured main exception contract'),
    'guard-controls-matches-rule-only': (
        'ImportIsolation.test_cli_main_failures_preserve_pre_refactor_contract',
        '#991 every entry must preserve its measured main exception contract'),
    'guard-ruler-subprocess-timeout-blocked': (
        'ImportIsolation.test_cli_main_failures_preserve_pre_refactor_contract',
        '#991 every entry must preserve its measured main exception contract'),
    'guard-round-exit-literal': (
        'ImportIsolation.test_guard_exit_codes_use_shared_mapping',
        '#991 guard must use shared exit codes including historical BLOCKED/1'),
    'guard-preflight-untrusted-exit-map': (
        'ImportIsolation.test_guard_exit_codes_use_shared_mapping',
        '#991 untrusted shared mapping must never turn preflight refusal into success'),
    'entry-main-alternate-dispatch': (
        'ApparatusInputs.test_cli_main_is_only_the_one_guard_call',
        '#991 __main__ must have no alternate dispatch'),
    'entry-new-a2-with-private-dispatch': (
        'ApparatusInputs.test_cli_main_is_only_the_one_guard_call',
        '#991 __main__ must be exactly run_entry(main, policy)'),
    'observer-preflight-removed': (
        'ImportIsolation.test_cli_dispatch_verifies_before_main',
        '#166 CLI dispatch must verify HEAD imports before main'),
    'observer-preflight-exit-one': (
        'ImportIsolation.test_tools_cli_preflight_errors_are_blocked',
        '#166 every CLI preflight failure must produce BLOCKED JSON/2'),
    'observer-normal-exit-zero': (
        'ImportIsolation.test_tools_cli_dispatch_preserves_exit_codes',
        '#166 every CLI wrapper must preserve main exit code'),
    'entry-suite-parity-lock-bypassed': (
        'AggregationAudit.test_negative_suites_require_parity_lock_and_report_busy_as_invalid',
        '#166 both negative suites must request parity lock'),
    'entry-suite-common-not-guarded': (
        'AggregationAudit.test_residual_mapping_is_rejected_before_import',
        '#166 common.py must be included in guarded sources'),
    'guard-session-exit-literal': (
        'ImportIsolation.test_guard_exit_codes_use_shared_mapping',
        '#991 guard must use shared exit codes including historical BLOCKED/1'),
    'guard-ruler-exit-literal': (
        'ImportIsolation.test_guard_exit_codes_use_shared_mapping',
        '#991 guard must use shared exit codes including historical BLOCKED/1'),
    'guard-controls-exit-literal': (
        'ImportIsolation.test_guard_exit_codes_use_shared_mapping',
        '#991 guard must use shared exit codes including historical BLOCKED/1'),
    'guard-unknown-policy-ignored': (
        'ImportIsolation.test_guard_exit_codes_use_shared_mapping',
        '#991 guard must use shared exit codes including historical BLOCKED/1'),
}


def publish(path, value):
    """Atomic publish for a reader that treats existence as complete (#166 r8)."""
    tmp = path.with_suffix(path.suffix + '.tmp')
    tmp.write_text(json.dumps(value, ensure_ascii=False, indent=2) + '\n')
    tmp.replace(path)


def single_test(log):
    return len(re.findall(r'^Ran 1 test in [0-9.]+s$', log, re.MULTILINE)) == 1


def descriptions(test):
    owner, method = test.rsplit('.', 1)
    # Accept both observed unittest layouts, retaining the exact module,
    # class and method identity in either layout (#1060).
    return (f'{method} (__main__.{test})', f'{method} (__main__.{owner})')


def passed(result, test):
    log = result.stdout + result.stderr
    return (result.returncode == 0
            and any(description + ' ... ok' in log.splitlines()
                    for description in descriptions(test))
            and single_test(log) and log.rstrip().endswith('\nOK')
            and 'skipped' not in log)


def classify(result, test, message):
    log = result.stdout + result.stderr
    if passed(result, test):
        return 'SURVIVED', 'bound test passed the mutant'
    if (result.returncode != 1 or not single_test(log)
            or not re.search(r'^FAILED \(failures=[1-9][0-9]*\)\s*\Z', log, re.MULTILINE)
            or 'ERROR:' in log):
        return 'INVALID', 'missing normal single-test assertion failure'
    # subTest descriptions follow either layout. Only this test's own
    # AssertionError may supply the marker.
    names = '|'.join(re.escape(name) for name in descriptions(test))
    header = re.compile(
        rf'FAIL: (?:{names})(?: \(.*\))?')
    separator = '\n' + '-' * 70 + '\n'
    for block in log.split('\n' + '=' * 70 + '\n')[1:]:
        title, found, body = block.partition(separator)
        if not found or not header.fullmatch(title):
            continue
        body = body.split(separator, 1)[0]
        assertion = re.search(r'^AssertionError: (.*)', body, re.MULTILINE | re.DOTALL)
        if assertion and message in assertion.group(1):
            return 'KILLED', message
    return 'INVALID', 'missing bound assertion (anchor, compile or wrong failure)'


class LogParsingTests(unittest.TestCase):
    """Exercise real unittest formatting without touching apparatus sources."""

    TEST = 'Probe.test_bound'
    MESSAGE = '#991 parser probe assertion'
    # Independent descriptions recorded on local CPython 3.14.3 / 3.9.6;
    # never derive these expectations from descriptions().
    DESCRIPTIONS = ('test_bound (__main__.Probe.test_bound)',
                    'test_bound (__main__.Probe)')

    def formatted_results(self, result):
        for description in self.DESCRIPTIONS:
            log = result.stderr
            for observed in self.DESCRIPTIONS:
                log = log.replace(observed, description)
            yield subprocess.CompletedProcess([], result.returncode, result.stdout, log)

    def probe(self, *, fail=False, subtests=False):
        def bound(case):
            if subtests:
                for value in range(2):
                    with case.subTest(value=value):
                        case.assertEqual('a\nb\nc', 'a\nd\nc', self.MESSAGE)
            elif fail:
                case.fail(self.MESSAGE)
        probe = type('Probe', (unittest.TestCase,),
                     {'__module__': '__main__', 'test_bound': bound})
        stream = io.StringIO()
        outcome = unittest.TextTestRunner(stream=stream, verbosity=2).run(probe('test_bound'))
        return subprocess.CompletedProcess([], int(not outcome.wasSuccessful()), '', stream.getvalue())

    def test_accepts_real_verbose_pass(self):
        result = self.probe()
        self.assertTrue(passed(result, self.TEST), '#991 real verbose pass must match')
        self.assertEqual(classify(result, self.TEST, self.MESSAGE)[0], 'SURVIVED',
                         '#991 bound pass must survive')

    def test_accepts_both_verbose_pass_formats(self):
        for result in self.formatted_results(self.probe()):
            with self.subTest(description=result.stderr.splitlines()[0]):
                self.assertTrue(passed(result, self.TEST),
                                '#1060 both bound unittest pass descriptions must pass')
                self.assertEqual(classify(result, self.TEST, self.MESSAGE)[0], 'SURVIVED',
                                 '#1060 both bound unittest pass descriptions must survive')

    def test_rejects_other_test_success(self):
        for result in self.formatted_results(self.probe()):
            description = result.stderr.splitlines()[0].split(' ...', 1)[0]
            for old, new in [('test_bound', 'test_other'), ('test_bound (', 'test_other ('),
                             ('__main__.Probe', '__main__.Other'),
                             ('__main__', 'other_module'),
                             (description, description[:-1] + '_extra)')]:
                with self.subTest(replacement=new, description=result.stderr.splitlines()[0]):
                    wrong = subprocess.CompletedProcess([], 0, '', result.stderr.replace(old, new))
                    self.assertFalse(passed(wrong, self.TEST),
                                     '#991 another method or qualified name must not pass')

    def test_requires_single_unskipped_success(self):
        result = self.probe()
        for old, new in [('Ran 1 test', 'Ran 11 tests'), ('Ran 1 test', 'No tests'),
                         ('\nOK\n', '\nNOT OK\n'), ('... ok', "... skipped 'reason'")]:
            with self.subTest(replacement=new):
                wrong = subprocess.CompletedProcess([], 0, '', result.stderr.replace(old, new))
                self.assertFalse(passed(wrong, self.TEST), '#991 pass requires one unskipped OK')
        skipped = subprocess.CompletedProcess([], 0, 'skipped\n', result.stderr)
        self.assertFalse(passed(skipped, self.TEST), '#991 skip diagnostics must not pass')
        result.returncode = 1
        self.assertFalse(passed(result, self.TEST), '#991 nonzero exit must not pass')

    def test_accepts_bound_assertions_and_subtests(self):
        for subtests in (False, True):
            with self.subTest(subtests=subtests):
                self.assertEqual(classify(self.probe(fail=True, subtests=subtests),
                                          self.TEST, self.MESSAGE)[0], 'KILLED',
                                 '#991 bound AssertionError including multiline subTest must kill')

    def test_accepts_both_assertion_formats(self):
        for subtests in (False, True):
            for result in self.formatted_results(self.probe(fail=True, subtests=subtests)):
                with self.subTest(subtests=subtests, description=result.stderr.splitlines()[0]):
                    self.assertEqual(classify(result, self.TEST, self.MESSAGE)[0], 'KILLED',
                                     '#1060 both bound unittest assertion descriptions must kill')

    def test_rejects_wrong_failure_identity_or_message(self):
        for result in self.formatted_results(self.probe(fail=True)):
            description = result.stderr.splitlines()[0].split(' ...', 1)[0]
            for old, new in [('FAIL: test_bound (', 'FAIL: test_other ('),
                             ('__main__.Probe', '__main__.Other'),
                             ('__main__', 'other_module'),
                             (description, description[:-1] + '_extra)'),
                             ('AssertionError: ' + self.MESSAGE, 'AssertionError: unrelated')]:
                with self.subTest(replacement=new, description=result.stderr.splitlines()[0]):
                    # A marker outside the bound assertion cannot rescue it.
                    wrong = subprocess.CompletedProcess([], 1, self.MESSAGE + '\n',
                                                         result.stderr.replace(old, new))
                    self.assertEqual(classify(wrong, self.TEST, self.MESSAGE)[0], 'INVALID',
                                     '#991 failure must bind both names and its own assertion message')
            source_echo = result.stderr.replace('AssertionError: ' + self.MESSAGE,
                                               '    self.fail(' + repr(self.MESSAGE) + ')\n'
                                               'AssertionError: unrelated')
            wrong = subprocess.CompletedProcess([], 1, '', source_echo)
            self.assertEqual(classify(wrong, self.TEST, self.MESSAGE)[0], 'INVALID',
                             '#991 traceback source echo is not an assertion message')

    def test_rejects_abnormal_failure(self):
        result = self.probe(fail=True)
        for old, new in [('Ran 1 test', 'Ran 11 tests'), ('FAILED (failures=1)', ''),
                         ('FAIL: ', 'ERROR: '), ('AssertionError:', 'RuntimeError:')]:
            with self.subTest(replacement=new):
                wrong = subprocess.CompletedProcess([], 1, '', result.stderr.replace(old, new))
                self.assertEqual(classify(wrong, self.TEST, self.MESSAGE)[0], 'INVALID',
                                 '#991 wrong count, missing summary or unrelated error is invalid')
        result.returncode = -2
        self.assertEqual(classify(result, self.TEST, self.MESSAGE)[0], 'INVALID',
                         '#991 signal exit must not kill')


def run_test(out, label, test, timeout=300):
    script = (ROOT / 'src/pilotdeck-gateway/tests/test_metadata_io_driver.py'
              if test.startswith('AggregationAudit.') else SAFETY)
    command = [sys.executable, '-I', '-B', str(script), test, '-v']
    result = run_owned(command, cwd=ROOT, timeout=timeout)
    (out / f'{label}.log').write_text(result.stdout + result.stderr)
    return result


def require_anchor(original, before, name):
    count = original.count(before)
    if count != 1:
        raise ValueError(f'#{name} anchor expired: count={count}')


def check_anchors():
    stale = []
    for name, source, before, _after in MUTATIONS:
        count = source.read_text().count(before)
        if count != 1:
            stale.append((name, source.relative_to(ROOT).as_posix(), count))
    for name, relative, count in stale:
        print(f'STALE ANCHOR {name}: {count} matches in {relative}')
    if stale:
        print('STALE ANCHORS: refusing to run; anchors are out of sync')
        return 2
    print(f'{len(MUTATIONS)} anchors: ok')
    return 0


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, help='new evidence directory')
    parser.add_argument('--only', action='append')
    parser.add_argument('--check-anchors', action='store_true')
    parser.add_argument('--self-test', action='store_true', help='test unittest log binding only')
    args = parser.parse_args()
    if args.self_test:
        suite = unittest.defaultTestLoader.loadTestsFromTestCase(LogParsingTests)
        return int(not unittest.TextTestRunner(verbosity=2).run(suite).wasSuccessful())
    if args.check_anchors:
        return check_anchors()
    if args.output is None:
        parser.error('--output is required unless --check-anchors or --self-test is used')
    known = [row[0] for row in MUTATIONS]
    if args.only and (set(args.only) - set(known) or len(args.only) != len(set(args.only))):
        print('unknown or duplicate --only mutation', file=sys.stderr)
        return 2
    selected = [row for row in MUTATIONS if not args.only or row[0] in args.only]
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    results = []
    # The shared parity lease loader requires common.py in the guarded set,
    # even though this suite never mutates it.
    paths = [COMMON, *dict.fromkeys(row[1] for row in selected)]
    try:
        with mutating_sources(ROOT, paths) as originals:
            for name, source, before, after in selected:
                status, reason = 'INVALID', 'not executed'
                test, message = CHECKS[name]
                try:
                    original = originals[source].decode()
                    require_anchor(original, before, name)
                    mutant = original.replace(before, after, 1)
                    compile(mutant, str(source), 'exec')
                    (out / f'{name}-compile.log').write_text('Python compile: PASS\n')
                    positive = run_test(out, name + '-positive', test)
                    if not passed(positive, test):
                        raise RuntimeError('positive control did not pass the bound test')
                    source.write_text(mutant)
                    negative = run_test(out, name + '-negative', test)
                    status, reason = classify(negative, test, message)
                except Exception as error:
                    status, reason = 'INVALID', f'{type(error).__name__}: {error}'
                finally:
                    source.write_bytes(originals[source])
                try:
                    restored = run_test(out, name + '-restored', test)
                    if not passed(restored, test):
                        status, reason = 'INVALID', reason + '; restored positive failed'
                except Exception as error:
                    status, reason = 'INVALID', reason + f'; restored: {type(error).__name__}: {error}'
                results.append({'mutation': name, 'test': test, 'status': status, 'reason': reason})
                counts = {state: sum(row['status'] == state for row in results)
                          for state in ('KILLED', 'SURVIVED', 'INVALID')}
                publish(out / 'summary.json', {'controls': results, 'counts': counts})
                print(f'{name}: {status}: {reason}', flush=True)
    except Exception as error:
        # A baseline/lock/restoration failure makes further mutation unsafe, but
        # still account for every selected case and never publish all-green.
        reason = f'source guard: {type(error).__name__}: {error}'
        for row in results:
            row.update(status='INVALID', reason=row['reason'] + '; ' + reason)
        for name, *_ in selected[len(results):]:
            results.append({'mutation': name, 'test': CHECKS[name][0],
                            'status': 'INVALID', 'reason': reason})
        publish(out / 'summary.json', {
            'controls': results,
            'counts': {'KILLED': 0, 'SURVIVED': 0, 'INVALID': len(results)}})
    counts = {state: sum(row['status'] == state for row in results)
              for state in ('KILLED', 'SURVIVED', 'INVALID')}
    print('summary: ' + ', '.join(f'{state}={count}' for state, count in counts.items()),
          flush=True)
    return int(not results or any(row['status'] != 'KILLED' for row in results))


if __name__ == '__main__':
    raise SystemExit(main())
