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

ROOT = Path(__file__).resolve().parents[5]
COMMON = ROOT / 'scripts/gateway-parity/common.py'
SESSION = ROOT / 'scripts/gateway-parity/session_domain.py'
SAFETY = ROOT / 'scripts/gateway-parity/test_safety.py'
ROUND = ROOT / 'scripts/gateway-parity/run_round.py'
AUDIT = ROOT / 'src/pilotdeck-gateway/tests/fixtures/session-domain-cleanup/check_cleanup.py'
NEGATIVE = Path(__file__).resolve()
SESSION_NEGATIVE = NEGATIVE.parent.parent / 'session-domain-cleanup/negative_controls.py'
DRIVER_TEST = ROOT / 'src/pilotdeck-gateway/tests/test_metadata_io_driver.py'
MUTATIONS = [
    ('nested-scope-erased', AUDIT, "name = '.'.join((*scope, child.name))", "name = child.name"),
    ('duplicate-entry-accepted', AUDIT,
     "if len(functions) != 1 and (markers or key in AGGREGATION_POINTS):",
     "if False and (markers or key in AGGREGATION_POINTS):"),
    ('definition-time-exit-unchecked', AUDIT,
     "children = ast.iter_child_nodes(node)",
     "children = (child for child in ast.iter_child_nodes(node) if not isinstance(child, (ast.ClassDef, ast.Lambda)))"),
    ('conditional-system-exit-unchecked', AUDIT,
     "for value in ast.walk(node.exc)",
     "for value in ([node.exc.func] if isinstance(node.exc, ast.Call) else [node.exc])"),
    ('system-exit-unchecked', AUDIT,
     "in ('SystemExit', 'builtins.SystemExit')", "in ('NeverExit',)"),
    ('sys-exit-unchecked', AUDIT,
     "in ('sys.exit', 'os._exit')", "in ('os._exit',)"),
    ('os-exit-unchecked', AUDIT,
     "in ('sys.exit', 'os._exit')", "in ('sys.exit',)"),
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
    ('empty-aggregation-green', COMMON,
     "if not known or any(child not in BATCH_STATUS_ORDER for child in known):",
     "if any(child not in BATCH_STATUS_ORDER for child in known):"),
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
    'nested-scope-erased': ('AggregationAudit.test_nested_names_cannot_replace_registered_entry', '#166 nested aggregation must retain scope identity'),
    'duplicate-entry-accepted': ('AggregationAudit.test_duplicate_entries_fail_closed', '#166 duplicate definitions cannot certify entry'),
    'definition-time-exit-unchecked': ('AggregationAudit.test_definition_time_exits_cannot_hide_in_nested_scopes', '#166 definition-time exit must certify its own code'),
    'conditional-system-exit-unchecked': ('AggregationAudit.test_explicit_exits_cannot_use_unreachable_returns', '#166 explicit exit must certify its own code'),
    'system-exit-unchecked': ('AggregationAudit.test_explicit_exits_cannot_use_unreachable_returns', '#166 explicit exit must certify its own code'),
    'sys-exit-unchecked': ('AggregationAudit.test_explicit_exits_cannot_use_unreachable_returns', '#166 explicit exit must certify its own code'),
    'os-exit-unchecked': ('AggregationAudit.test_explicit_exits_cannot_use_unreachable_returns', '#166 explicit exit must certify its own code'),
    'gateway-common-unguarded': ('AggregationAudit.test_residual_mapping_is_rejected_before_import', '#166 residual mapping must be guarded before import'),
    'session-common-unguarded': ('AggregationAudit.test_residual_mapping_is_rejected_before_import', '#166 residual mapping must be guarded before import'),
    'gateway-common-loaded-early': ('AggregationAudit.test_residual_mapping_is_rejected_before_import', '#166 residual mapping must be guarded before import'),
    'session-common-loaded-early': ('AggregationAudit.test_residual_mapping_is_rejected_before_import', '#166 residual mapping must be guarded before import'),
    'empty-aggregation-green': ('ApparatusInputs.test_empty_aggregation_fails_closed', '#166 empty aggregation must fail'),
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


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', required=True, type=Path)
    parser.add_argument('--only', action='append')
    args = parser.parse_args()
    known = {row[0] for row in MUTATIONS}
    if args.only and (set(args.only) - known or len(args.only) != len(set(args.only))):
        parser.error('unknown or duplicate --only')
    selected = [row for row in MUTATIONS if not args.only or row[0] in args.only]
    out = args.output.resolve()
    out.mkdir(parents=True, exist_ok=False)
    results = []

    def run(label, test):
        script = DRIVER_TEST if test.startswith('AggregationAudit.') else SAFETY
        result = run_owned([sys.executable, '-I', '-B', str(script), test, '-v'], cwd=ROOT, timeout=60)
        (out / (label + '.log')).write_text(result.stdout + result.stderr)
        return result

    def record(name, status, reason):
        results.append({'mutation': name, 'status': status, 'reason': reason})
        print(f'{name}: {status}: {reason}', flush=True)
        (out / 'summary.json').write_text(json.dumps(results, indent=2) + '\n')

    try:
        with mutating_sources(ROOT, list(dict.fromkeys([COMMON, *(row[1] for row in selected)]))) as originals:
            # Import only after every source, including the verdict table, is pristine.
            _spec = importlib.util.spec_from_file_location('gateway_batch_common', COMMON)
            common = importlib.util.module_from_spec(_spec)
            _spec.loader.exec_module(common)
            MUTATION_STATUS = common.MUTATION_STATUS
            for name, source, old, new in selected:
                status, reason = 'INVALID', 'not executed'
                test, message = CHECKS[name]
                try:
                    original = originals[source].decode()
                    if original.count(old) != 1:
                        raise ValueError(f'#{name} anchor expired: count={original.count(old)}')
                    mutant = original.replace(old, new, 1)
                    compile(mutant, str(source), 'exec')
                    (out / (name + '-compile.log')).write_text('Python compile: PASS\n')
                    positive = run(name + '-positive', test)
                    if positive.returncode != 0 or 'Ran 1 test' not in positive.stderr:
                        raise RuntimeError('bound positive test did not pass')
                    source.write_text(mutant)
                    negative = run(name + '-negative', test)
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
                    restored = run(name + '-restored', test)
                    if restored.returncode != 0:
                        status, reason = 'INVALID', reason + '; restored positive failed'
                except Exception as error:
                    status, reason = 'INVALID', reason + f'; restoration test: {error}'
                record(name, status, reason)
    except Exception as error:
        for row in results:
            row.update(status='INVALID', reason=f'source guard: {error}')
        for name, *_ in selected[len(results):]:
            record(name, 'INVALID', f'source guard: {error}')
        (out / 'summary.json').write_text(json.dumps(results, indent=2) + '\n')
        raise  # A failed guard must never use an unverified verdict module.
    counts = {state: sum(row['status'] == state for row in results)
              for state in ('KILLED', 'SURVIVED', 'INVALID')}
    print(json.dumps(counts), flush=True)
    return common.aggregate_status(*(MUTATION_STATUS.get(row['status'], 'FAIL') for row in results))[1]


if __name__ == '__main__':
    raise SystemExit(main())
