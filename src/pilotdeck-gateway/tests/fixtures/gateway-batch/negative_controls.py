#!/usr/bin/env python3
"""#166/#701/#209 source controls; committed HEAD only, no cargo is launched.

Run with python3 -I .../negative_controls.py --output <new directory>.
Compile every Python mutant before its bound unittest; run all selected cases,
then aggregate KILLED / SURVIVED / INVALID. A stale anchor is INVALID, never a
survivor. No worktree snapshot or alternate baseline may bypass HEAD.
"""
import argparse
import importlib.util
import json
from pathlib import Path
import sys
import tempfile

sys.dont_write_bytecode = True
sys.pycache_prefix = tempfile.mkdtemp(prefix='pdrs-166-negative-pyc-')
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_process import committed_status_contract

ROOT = Path(__file__).resolve().parents[5]
COMMON = ROOT / 'scripts/gateway-parity/common.py'
SESSION = ROOT / 'scripts/gateway-parity/session_domain.py'
SAFETY = ROOT / 'scripts/gateway-parity/test_safety.py'
ROUND = ROOT / 'scripts/gateway-parity/run_round.py'
NEGATIVE = Path(__file__).resolve()
SESSION_NEGATIVE = NEGATIVE.parent.parent / 'session-domain-cleanup/negative_controls.py'
DRIVER_TEST = ROOT / 'src/pilotdeck-gateway/tests/test_metadata_io_driver.py'
PROCESS = NEGATIVE.parent.parent / 'negative_process.py'
PROCESS_TEST = NEGATIVE.parent.parent / 'test_negative_process.py'
MUTATIONS = [
    ('controls-late-verification-removed', ROOT / 'scripts/gateway-parity/controls/prerequisites.py',
     '        _verify_cli_imports()\n    return module', '        pass  # mutant accepts late import\n    return module'),
    ('controls-saved-tree-verified-as-current', ROOT / 'scripts/gateway-parity/controls/prerequisites.py',
     '    if Path(path).resolve().is_relative_to(_apparatus):', '    if True:'),
    ('cli-verifier-from-mutant-common', ROUND,
     '    trusted.verify_loaded_apparatus(head)', '    common.verify_loaded_apparatus(head)'),
    ('round-cli-preflight-removed', ROUND,
     '        _verify_cli_imports()\n        code = main()', '        code = main()'),
    ('ruler-cli-preflight-removed', ROOT / 'scripts/gateway-parity/check_ruler.py',
     '        _verify_cli_imports()\n        code = main()', '        code = main()'),
    ('session-cli-preflight-removed', SESSION,
     "if __name__ == '__main__':\n    _verify_cli_imports()\n    raise SystemExit(main())",
     "if __name__ == '__main__':\n    raise SystemExit(main())"),
    ('a2-controls-cli-preflight-removed', ROOT / 'scripts/gateway-parity/controls/run_all.py',
     "if __name__ == '__main__':\n    _verify_cli_imports()\n    raise SystemExit(main())",
     "if __name__ == '__main__':\n    raise SystemExit(main())"),
    ('e1-controls-cli-preflight-removed', ROOT / 'scripts/gateway-parity/controls/session_domain.py',
     "if __name__ == '__main__':\n    _verify_cli_imports()\n    raise SystemExit(main())",
     "if __name__ == '__main__':\n    raise SystemExit(main())"),
    ('main-code-verification-bypassed', COMMON,
     'matches = _compiled_fingerprint(code) == _compiled_fingerprint(expected_code)',
     'matches = True'),
    ('import-verification-before-lock', COMMON,
     '    with trusted.parity_lock(timeout, takeovers, lock):\n        verify_loaded_apparatus(head)\n        yield',
     '    verify_loaded_apparatus(head)\n    with trusted.parity_lock(timeout, takeovers, lock):\n        yield'),
    ('failed-import-cache-retained', SAFETY,
     '        sys.modules.pop(name, None)\n        raise',
     '        pass  # mutant retains partial module\n        raise'),
    ('session-lock-busy-wrong-exit', SESSION,
     "'error': str(error)}, ensure_ascii=False))\n        return 2",
     "'error': str(error)}, ensure_ascii=False))\n        return 1"),
    ('session-shutdown-after-lease', SESSION,
     '    install_shutdown()\n    try:\n        with parity_lock(timeout=30, takeovers=takeovers):',
     '    try:\n        with parity_lock(timeout=30, takeovers=takeovers):'),
    ('import-verification-removed', COMMON,
     '        verify_loaded_apparatus(head)\n        yield',
     '        yield  # mutant accepts pre-lock imports'),
    ('import-verification-reads-disk', COMMON,
     'isinstance(source, bytes) and source == expected',
     'isinstance(source, bytes) and Path(path).read_bytes() == expected'),
    ('import-provenance-after-exec', SAFETY,
     '    module._apparatus_source = source', '    module._apparatus_source = path.read_bytes()'),
    ('import-unknown-cache-open', SAFETY,
     "if not isinstance(getattr(existing, '_apparatus_source', None), bytes):",
     "if False and not isinstance(getattr(existing, '_apparatus_source', None), bytes):"),
    ('verified-cleanup-detached', COMMON,
     '    trusted.LIVE_GROUPS = LIVE_GROUPS', '    trusted.LIVE_GROUPS = set()'),
    ('verified-lease-bypassed', COMMON,
     '    with trusted.parity_lock(timeout, takeovers, lock):',
     "    with __import__('contextlib').nullcontext():"),
    ('round-import-verification-bypassed', ROUND,
     'parity_lock = common.verified_parity_lock', 'parity_lock = common.parity_lock'),
    ('ruler-import-verification-bypassed', ROOT / 'scripts/gateway-parity/check_ruler.py',
     'parity_lock = common.verified_parity_lock', 'parity_lock = common.parity_lock'),
    ('session-import-verification-bypassed', SESSION,
     'parity_lock = common.verified_parity_lock', 'parity_lock = common.parity_lock'),
    ('a2-controls-import-verification-bypassed', ROOT / 'scripts/gateway-parity/controls/run_all.py',
     '    common.verify_loaded_apparatus()', '    pass  # mutant accepts polluted controls'),
    ('e1-controls-import-verification-bypassed', ROOT / 'scripts/gateway-parity/controls/session_domain.py',
     '    common.verify_loaded_apparatus()', '    pass  # mutant accepts polluted controls'),
    ('optional-pass-exit-nonzero', COMMON,
     "BATCH_EXIT_CODES = {'PASS': 0,", "BATCH_EXIT_CODES = {'PASS': 1,"),
    ('parity-window-unlocked', PROCESS, 'if parity else nullcontext())', 'if False else nullcontext())'),
    ('parity-unverified-released', PROCESS, 'if not restored[0]:', 'if False and not restored[0]:'),
    ('guard-contract-open', PROCESS,
     'if set(namespace) != names or len(functions) != 1:',
     'if False and (set(namespace) != names or len(functions) != 1):'),
    ('guard-exit-fail', PROCESS,
     "return SimpleNamespace(aggregate_status=namespace['aggregate_status'])",
     "return SimpleNamespace(aggregate_status=lambda *_: ('BLOCKED', 1))"),
    ('compile-invalid-not-reached', SESSION_NEGATIVE,
     "                    compile(changed, str(source), 'exec')",
     '                    pass  # mutant skips compiler'),
    ('gateway-common-unguarded', NEGATIVE,
     "[COMMON, " "*(row[1] for row in selected)]", "[row[1] for row in selected]"),
    ('session-common-unguarded', SESSION_NEGATIVE,
     "{COMMON_SOURCE, *(Path(row[1]) for row in MUTATIONS)}", "{Path(row[1]) for row in MUTATIONS}"),
    ('gateway-common-loaded-early', NEGATIVE,
     "COMMON = ROOT / 'scripts/gateway-parity/common.py'\n",
     "COMMON = ROOT / 'scripts/gateway-parity/common.py'\n"
     "_early = importlib.util.spec_from_file_location('early_common', COMMON)\n"
     "_early.loader.exec_module(importlib.util.module_from_spec(_early))\n"),
    ('session-common-loaded-early', SESSION_NEGATIVE,
     "COMMON_SOURCE = ROOT / 'scripts/gateway-parity/common.py'\n",
     "COMMON_SOURCE = ROOT / 'scripts/gateway-parity/common.py'\n"
     "_early = importlib.util.spec_from_file_location('early_common', COMMON_SOURCE)\n"
     "_early.loader.exec_module(importlib.util.module_from_spec(_early))\n"),
    ('a3-bootstrap-unscanned', SAFETY,
     "for name in common.A2_SCRIPTS + common.A3_SCRIPTS:\n            if name == 'common.py'",
     "for name in common.A2_SCRIPTS:\n            if name == 'common.py'"),
    ('a3-imports-unscanned', SAFETY,
     "for name in common.A2_SCRIPTS + common.A3_SCRIPTS:\n            if not name.endswith('.py') or name == 'common.py'",
     "for name in common.A2_SCRIPTS:\n            if not name.endswith('.py') or name == 'common.py'"),
    ('child-mask-callback-missing', ROUND,
     "preexec_fn=common.unblock_child_signals, **kwargs)", "preexec_fn=None, **kwargs)"),
    ('child-signals-blocked', COMMON,
     "    signal.pthread_sigmask(signal.SIG_UNBLOCK, DEFERRED_SIGNALS)",
     "    signal.pthread_sigmask(signal.SIG_BLOCK, DEFERRED_SIGNALS)"),
    ('spawn-log-leaked', ROUND,
     "        except BaseException:\n            log.close()\n            raise",
     "        except BaseException:\n            raise"),
    ('registration-after-interrupt', ROUND,
     "        common.LIVE_GROUPS.add(proc.pid)\n        entry.update(pid=proc.pid, processGroup=proc.pid)",
     "        entry.update(pid=proc.pid, processGroup=proc.pid)"),
    ('cleanup-list-after-interrupt', ROUND,
     "        processes.append((proc, log, entry))", "    processes.append((proc, log, entry))"),
    ('cleanup-batch-unmasked', ROUND,
     "    with common.deferred_signals():\n        for proc, log, entry in reversed(processes):",
     "    if True:\n        for proc, log, entry in reversed(processes):"),
    ('cleanup-error-stops-batch', ROUND,
     "                remaining.append(proc.pid)\n            finally:",
     "                remaining.append(proc.pid)\n                break\n            finally:"),
    ('cleanup-errors-forgotten', ROUND,
     "            common.LIVE_GROUPS.update(proc.pid for proc, _, _ in processes)",
     "            pass  # mutant: cleanup error does not restore registration"),
    ('real-cleanup-rejected', ROUND,
     "stopped = not remaining and not errors and proc.returncode is not None", "stopped = False"),
    ('mutation-survived-as-pass', COMMON, "'SURVIVED': 'FAIL'", "'SURVIVED': 'PASS'"),
    ('not-run-green', COMMON, "'NOT_RUN': 3,", "'NOT_RUN': 0,"),
    ('deadline-unmapped', SESSION,
     "            _, code = common.aggregate_status(report['status'])",
     "            code = 0"),
    ('a3-module-missing', SAFETY,
     "Path(name).stem for name in common.A2_SCRIPTS + common.A3_SCRIPTS if name.endswith('.py')",
     "Path(name).stem for name in common.A2_SCRIPTS if name.endswith('.py')"),
    ('unverified-lock-release', COMMON, "                if LIVE_GROUPS:\n",
     "                if False and LIVE_GROUPS:\n"),
    ('pending-lock-stolen', COMMON,
     "if reason and not damaged and not (previous and previous.get('cleanupPending')):", "if reason and not damaged:"),
    ('damaged-owner-stolen', COMMON,
     "if reason and not damaged and not (previous and previous.get('cleanupPending')):",
     "if reason and not (previous and previous.get('cleanupPending')):"),
    ('signals-not-deferred', COMMON,
     "previous = signal.pthread_sigmask(signal.SIG_BLOCK, DEFERRED_SIGNALS)",
     "previous = signal.pthread_sigmask(signal.SIG_UNBLOCK, DEFERRED_SIGNALS)"),
    ('failed-cleanup-forgotten', ROUND,
     "        if stopped:\n            common.LIVE_GROUPS.discard(proc.pid)",
     "        if True:\n            common.LIVE_GROUPS.discard(proc.pid)"),
    ('reap-without-identity', ROUND,
     "os.WEXITED | os.WNOHANG | os.WNOWAIT", "os.WEXITED | os.WNOHANG"),
    ('extra-registration-ignored', ROUND,
     "remaining = sorted(set(remaining) | common.LIVE_GROUPS)", "remaining = sorted(set(remaining))"),
    ('zombie-eperm-poisons-cleanup', ROUND,
     "(zombie_denials if leader_exited else errors).append(str(error))", "errors.append(str(error))"),
    ('release-entry-unmasked', COMMON,
     "        with deferred_signals(), guard(lock):", "        with guard(lock), deferred_signals():"),
    ('acquisition-marker-missing', COMMON,
     "            'cleanupPending': True}", "            'cleanupPending': False}"),
    ('mutation-invalid-as-pass', COMMON,
     "'INVALID': 'BLOCKED'", "'INVALID': 'PASS'"),
]
CHECKS = {
    'controls-late-verification-removed': ('ImportIsolation.test_controls_late_import_is_verified_before_return', '#166 controls late import must reject mutant before returning module'),
    'controls-saved-tree-verified-as-current': ('ImportIsolation.test_controls_saved_tree_import_keeps_existing_semantics', '#166 saved code-root import must retain its existing semantics'),
    'cli-verifier-from-mutant-common': ('ImportIsolation.test_cli_preflight_uses_head_verifier_for_mutated_common', '#166 CLI preflight must use HEAD verifier even when disk helper is mutant'),
    'round-cli-preflight-removed': ('ImportIsolation.test_cli_dispatch_verifies_before_main', '#166 CLI dispatch must verify HEAD imports before main'),
    'ruler-cli-preflight-removed': ('ImportIsolation.test_cli_dispatch_verifies_before_main', '#166 CLI dispatch must verify HEAD imports before main'),
    'session-cli-preflight-removed': ('ImportIsolation.test_cli_dispatch_verifies_before_main', '#166 CLI dispatch must verify HEAD imports before main'),
    'a2-controls-cli-preflight-removed': ('ImportIsolation.test_cli_dispatch_verifies_before_main', '#166 CLI dispatch must verify HEAD imports before main'),
    'e1-controls-cli-preflight-removed': ('ImportIsolation.test_cli_dispatch_verifies_before_main', '#166 CLI dispatch must verify HEAD imports before main'),
    'main-code-verification-bypassed': ('ImportIsolation.test_main_code_is_verified_after_entry_disk_restore', '#166 CLI entry code must be verified after disk restoration'),
    'import-verification-before-lock': ('ImportIsolation.test_imported_mutant_is_rejected_after_disk_restore', '#166 pre-lock mutant must be rejected after disk restoration'),
    'failed-import-cache-retained': ('ImportIsolation.test_failed_import_does_not_poison_cache', '#166 failed import must not retain a partial module'),
    'session-lock-busy-wrong-exit': ('ImportIsolation.test_session_busy_or_cancelled_lock_is_blocked', '#166 session lock refusal must remain BLOCKED/2'),
    'session-shutdown-after-lease': ('ImportIsolation.test_entry_installs_shutdown_before_lock', '#166 shutdown handler must precede lease acquisition'),
    'import-verification-removed': ('ImportIsolation.test_imported_mutant_is_rejected_after_disk_restore', '#166 pre-lock mutant must be rejected after disk restoration'),
    'import-verification-reads-disk': ('ImportIsolation.test_imported_mutant_is_rejected_after_disk_restore', '#166 pre-lock mutant must be rejected after disk restoration'),
    'import-provenance-after-exec': ('ImportIsolation.test_loader_records_executed_bytes_not_later_disk', '#166 provenance must be exactly the compiled bytes'),
    'import-unknown-cache-open': ('ImportIsolation.test_unknown_cache_is_refused', '#166 cache without executed bytes must fail closed'),
    'verified-cleanup-detached': ('ImportIsolation.test_pending_cleanup_keeps_verified_lease', '#166 verified lock must preserve running cleanup registry'),
    'verified-lease-bypassed': ('ImportIsolation.test_clean_controls_do_not_reacquire_parent_lease', '#166 clean round must hold real parity lease'),
    'optional-pass-exit-nonzero': ('ApparatusInputs.test_deadline_not_run_has_dedicated_exit_code', '#166 empty optional aggregation stays PASS/0'),
    'round-import-verification-bypassed': ('ImportIsolation.test_real_and_control_entries_enforce_import_verification', '#166 real entry must use HEAD-backed verified parity lock'),
    'ruler-import-verification-bypassed': ('ImportIsolation.test_real_and_control_entries_enforce_import_verification', '#166 real entry must use HEAD-backed verified parity lock'),
    'session-import-verification-bypassed': ('ImportIsolation.test_real_and_control_entries_enforce_import_verification', '#166 real entry must use HEAD-backed verified parity lock'),
    'a2-controls-import-verification-bypassed': ('ImportIsolation.test_real_and_control_entries_enforce_import_verification', '#166 every real/control entry must reject imported mutant before work'),
    'e1-controls-import-verification-bypassed': ('ImportIsolation.test_real_and_control_entries_enforce_import_verification', '#166 every real/control entry must reject imported mutant before work'),
    'parity-window-unlocked': ('ParityMutationLock.test_lock_held_through_restore_and_head_verification', '#166 second parity acquirer must be refused during mutation'),
    'parity-unverified-released': ('ParityMutationLock.test_failed_restore_retains_cleanup_pending_lease', '#166 failed restoration must retain parity lock'),
    'guard-contract-open': ('AggregationAudit.test_committed_contract_requires_complete_head', '#166 incomplete committed contract must be refused'),
    'guard-exit-fail': ('AggregationAudit.test_residual_mapping_is_rejected_before_import', '#166 source guard must exit BLOCKED'),
    'compile-invalid-not-reached': ('AggregationAudit.test_session_suite_continues_after_invalid_mutants', '#166 uncompilable mutant must reach compiler'),
    'gateway-common-unguarded': ('AggregationAudit.test_residual_mapping_is_rejected_before_import', '#166 common.py must be included in guarded sources'),
    'session-common-unguarded': ('AggregationAudit.test_residual_mapping_is_rejected_before_import', '#166 common.py must be included in guarded sources'),
    'gateway-common-loaded-early': ('AggregationAudit.test_residual_mapping_is_rejected_before_import', '#166 residual mapping must be guarded before import'),
    'session-common-loaded-early': ('AggregationAudit.test_residual_mapping_is_rejected_before_import', '#166 residual mapping must be guarded before import'),
    'a3-bootstrap-unscanned': ('ApparatusInputs.test_a3_entries_are_scanned_by_both_guards', '#701 A3 scanner must inspect check_schema.py'),
    'a3-imports-unscanned': ('ApparatusInputs.test_a3_entries_are_scanned_by_both_guards', '#701 A3 scanner must inspect check_schema.py'),
    'child-mask-callback-missing': ('Safety.test_start_registers_before_pending_signal_and_unblocks_child', '#209 child mask restored'),
    'child-signals-blocked': ('Safety.test_start_registers_before_pending_signal_and_unblocks_child', '#209 child signals must be unblocked'),
    'spawn-log-leaked': ('Safety.test_spawn_failure_closes_log_without_registering', '#209 failed spawn closes log'),
    'registration-after-interrupt': ('Safety.test_start_registers_before_pending_signal_and_unblocks_child', '#209 register before signal delivery'),
    'cleanup-list-after-interrupt': ('Safety.test_start_registers_before_pending_signal_and_unblocks_child', '#209 cleanup list published before signal'),
    'cleanup-batch-unmasked': ('Safety.test_cleanup_continues_after_oserror_and_defers_whole_batch', '#209 cleanup must reach every group before interrupt'),
    'cleanup-error-stops-batch': ('Safety.test_cleanup_continues_after_oserror_and_defers_whole_batch', '#209 cleanup must reach every group before interrupt'),
    'cleanup-errors-forgotten': ('Safety.test_cleanup_log_error_restores_all_registrations', '#209 cleanup error restores registrations'),
    'real-cleanup-rejected': ('Safety.test_real_held_leaders_cleanup_after_exit_and_term', '#209 real cleanup must prove absence'),
    'mutation-survived-as-pass': ('ApparatusInputs.test_deadline_not_run_has_dedicated_exit_code', '#166 source control verdict mapping'),
    'not-run-green': ('ApparatusInputs.test_deadline_not_run_has_dedicated_exit_code', '#166 NOT_RUN must exit 3'),
    'deadline-unmapped': ('ApparatusInputs.test_deadline_not_run_has_dedicated_exit_code', '#166 NOT_RUN must exit 3'),
    'a3-module-missing': ('ApparatusInputs.test_apparatus_import_pattern_covers_import_lists', '#701 A3 module must be guarded'),
    'unverified-lock-release': ('Safety.test_cleanup_pending_retains_lock_and_cannot_be_taken_over', '#209 unverified cleanup must retain lock'),
    'pending-lock-stolen': ('Safety.test_cleanup_pending_retains_lock_and_cannot_be_taken_over', '#209 cleanup-pending owner was stolen'),
    'damaged-owner-stolen': ('Safety.test_damaged_published_owner_is_not_stale_unpublished_lock', '#209 damaged published owner was stolen'),
    'signals-not-deferred': ('Safety.test_start_registers_before_pending_signal_and_unblocks_child', '#209 spawn-registration signal window'),
    'failed-cleanup-forgotten': ('Safety.test_stop_owned_proves_absence_and_keeps_all_failure_shapes', '#209 failed cleanup remains registered'),
    'reap-without-identity': ('Safety.test_owned_wait_holds_identity_and_stop_never_signals_reaped_child', '#209 wait must reserve leader identity'),
    'extra-registration-ignored': ('Safety.test_missing_or_extra_registration_fails_closed', '#209 extra registration is reported'),
    'zombie-eperm-poisons-cleanup': ('Safety.test_zombie_denial_requires_final_esrch', '#209 zombie-only EPERM may clear only after final ESRCH'),
    'release-entry-unmasked': ('Safety.test_release_guard_interrupt_publishes_pending_before_delivery', '#209 release entry interrupt must publish pending groups'),
    'acquisition-marker-missing': ('Safety.test_cleanup_record_write_failure_keeps_preexisting_marker', '#209 lock must be marked before spawning'),
    'mutation-invalid-as-pass': ('ApparatusInputs.test_deadline_not_run_has_dedicated_exit_code', '#166 source control verdict mapping'),
}


def run(out, label, test):
    script = (DRIVER_TEST if test.startswith('AggregationAudit.') else
              PROCESS_TEST if test.startswith('ParityMutationLock.') else SAFETY)
    result = run_owned([sys.executable, '-I', '-B', str(script), test, '-v'], cwd=ROOT, timeout=120)
    (out / (label + '.log')).write_text(result.stdout + result.stderr)
    return result

def record(out, results, name, status, reason):
    results.append({'mutation': name, 'status': status, 'reason': reason})
    print(f'{name}: {status}: {reason}', flush=True)
    (out / 'summary.json').write_text(json.dumps(results, indent=2) + '\n')


def load_pristine_common():
    spec = importlib.util.spec_from_file_location('gateway_batch_common', COMMON)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def require_single_anchor(original, old, name):
    if original.count(old) != 1:
        raise ValueError(f'#{name} anchor expired: count={original.count(old)}')


def require_positive(result):
    if result.returncode != 0 or 'Ran 1 test' not in result.stderr:
        raise RuntimeError('bound positive test did not pass')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', required=True, type=Path)
    parser.add_argument('--only', action='append')
    args = parser.parse_args()
    known = {row[0] for row in MUTATIONS}
    if args.only and (set(args.only) - known or len(args.only) != len(set(args.only))):
        common = committed_status_contract(ROOT)
        return common.aggregate_status('BLOCKED')[1]
    selected = [row for row in MUTATIONS if not args.only or row[0] in args.only]
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    results = []

    try:
        with mutating_sources(ROOT, list(dict.fromkeys([COMMON, *(row[1] for row in selected)]))) as originals:
            # Import only after every source, including the verdict table, is pristine.
            common = load_pristine_common()
            MUTATION_STATUS = common.MUTATION_STATUS
            for name, source, old, new in selected:
                status, reason = 'INVALID', 'not executed'
                test, message = CHECKS[name]
                try:
                    original = originals[source].decode()
                    require_single_anchor(original, old, name)
                    mutant = original.replace(old, new, 1)
                    compile(mutant, str(source), 'exec')
                    (out / (name + '-compile.log')).write_text('Python compile: PASS\n')
                    positive = run(out, name + '-positive', test)
                    require_positive(positive)
                    source.write_text(mutant)
                    negative = run(out, name + '-negative', test)
                    text = negative.stdout + negative.stderr
                    if negative.returncode == 0:
                        status, reason = 'SURVIVED', 'bound test passed the mutant'
                    elif ('FAIL: ' + test.split('.')[-1]) in text and message in text and 'ERROR:' not in text:
                        status, reason = 'KILLED', message
                    else:
                        status, reason = 'INVALID', 'failed outside bound assertion: ' + message
                except Exception as error:
                    status, reason = 'INVALID', f'{type(error).__name__}: {error}'
                finally:
                    source.write_bytes(originals[source])
                try:
                    restored = run(out, name + '-restored', test)
                    if restored.returncode != 0:
                        status, reason = 'INVALID', reason + '; restored positive failed'
                except Exception as error:
                    status, reason = 'INVALID', reason + f'; restoration test: {error}'
                record(out, results, name, status, reason)
    except Exception as error:
        for row in results:
            row.update(status='INVALID', reason=f'source guard: {error}')
        for name, *_ in selected[len(results):]:
            record(out, results, name, 'INVALID', f'source guard: {error}')
        (out / 'summary.json').write_text(json.dumps(results, indent=2) + '\n')
        common = committed_status_contract(ROOT)
        return common.aggregate_status('BLOCKED')[1]
    counts = {state: sum(row['status'] == state for row in results)
              for state in ('KILLED', 'SURVIVED', 'INVALID')}
    print(json.dumps(counts), flush=True)
    return common.aggregate_status(*(MUTATION_STATUS.get(row['status'], 'FAIL') for row in results))[1]


if __name__ == '__main__':
    raise SystemExit(main())
