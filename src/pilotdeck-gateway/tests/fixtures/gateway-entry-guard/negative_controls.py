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
import importlib.util
import json
from pathlib import Path
import sys
import tempfile

sys.dont_write_bytecode = True
sys.pycache_prefix = tempfile.mkdtemp(prefix='pdrs-991-negative-pyc-')
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned

ROOT = Path(__file__).resolve().parents[5]
GUARD = ROOT / 'scripts/gateway-parity/_guard.py'
COMMON = ROOT / 'scripts/gateway-parity/common.py'
BUILD = ROOT / 'scripts/gateway-parity/build_proxy.py'
COMPARE = ROOT / 'scripts/gateway-parity/compare.py'
SAFETY = ROOT / 'scripts/gateway-parity/test_safety.py'

# (name, source, before, after): the static checker and the runner share this
# table; every before must occur exactly once or the anchor is stale.
MUTATIONS = [
    ('guard-preflight-removed', GUARD,
     '        verify_cli_imports()\n', '        pass  # mutant skips preflight\n'),
    ('guard-preflight-exit-one', GUARD,
     '        raise SystemExit(2)', '        raise SystemExit(1)'),
    ('guard-preflight-except-narrowed', GUARD,
     "    except (Exception, KeyboardInterrupt) as error:\n"
     "        print(json.dumps({'status': 'BLOCKED', 'error': str(error)}, ensure_ascii=False))",
     "    except RuntimeError as error:\n"
     "        print(json.dumps({'status': 'BLOCKED', 'error': str(error)}, ensure_ascii=False))"),
    ('guard-main-exit-zero', GUARD,
     '        raise SystemExit(code)\n    raise SystemExit(code)',
     '        raise SystemExit(code)\n    raise SystemExit(0)'),
    ('guard-main-except-narrowed', GUARD,
     '    except (Exception, KeyboardInterrupt) as error:\n        (status, code), rule = _failure(error)',
     '    except Exception as error:\n        (status, code), rule = _failure(error)'),
    ('guard-cancel-is-fail', GUARD,
     '(PermissionError, TimeoutError, KeyboardInterrupt)',
     '(PermissionError, TimeoutError)'),
    ('guard-evidence-is-blocked', GUARD,
     "    blocked = (rule == 'APPARATUS_IMPORTS'\n"
     "               or isinstance(error, (PermissionError, TimeoutError, KeyboardInterrupt)))",
     '    blocked = True'),
    ('guard-error-rule-lost', GUARD,
     "    rule = getattr(error, 'rule', None) or 'EVIDENCE'",
     "    rule = getattr(error, 'rule', None) or 'APPARATUS_IMPORTS'"),
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
     "if __name__ == '__main__':\n    run_entry(main)",
     "if __name__ == '__main__':\n    try:\n        run_entry(main)\n"
     "    except Exception:\n        raise SystemExit(1)"),
    ('entry-private-verifier-restored', COMPARE,
     'def main():',
     'def _verify_cli_imports():\n    return None\n\n\ndef main():'),
    ('entry-import-before-isolated', COMPARE,
     'import sys\n\nif not sys.flags.isolated:',
     'import sys\nimport argparse\n\nif not sys.flags.isolated:'),
]

CHECKS = {
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
        'ImportIsolation.test_ruler_work_signals_are_blocked_after_lease_release',
        '#166 fix-r10 work signals must return BLOCKED/2/EVIDENCE after lease release'),
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
        '#991 __main__ must be exactly run_entry(main)'),
    'entry-private-verifier-restored': (
        'ApparatusInputs.test_cli_main_is_only_the_one_guard_call',
        '#991 compare.py must alias, not redefine, _verify_cli_imports'),
    'entry-import-before-isolated': (
        'ApparatusInputs.test_a2_entries_share_one_verbatim_path_bootstrap',
        '#991 -I check must precede every other import: compare.py'),
}


def publish(path, value):
    """Atomic publish for a reader that treats existence as complete (#166 r8)."""
    tmp = path.with_suffix(path.suffix + '.tmp')
    tmp.write_text(json.dumps(value, ensure_ascii=False, indent=2) + '\n')
    tmp.replace(path)


def passed(result, test):
    log = result.stdout + result.stderr
    return (result.returncode == 0 and f'{test} (' in log and '... ok' in log
            and 'Ran 1 test' in log and 'OK' in log and 'skipped' not in log)


def classify(result, test, message):
    log = result.stdout + result.stderr
    method = test.split('.')[-1]
    if passed(result, test):
        return 'SURVIVED', 'bound test passed the mutant'
    if (result.returncode != 0 and f'FAIL: {method}' in log and message in log
            and 'ERROR:' not in log):
        return 'KILLED', message
    return 'INVALID', 'missing bound assertion (anchor, compile or wrong failure)'


def run_test(out, label, test, timeout=300):
    command = [sys.executable, '-I', '-B', str(SAFETY), test, '-v']
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
    args = parser.parse_args()
    if args.check_anchors:
        return check_anchors()
    if args.output is None:
        parser.error('--output is required unless --check-anchors is used')
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
