#!/usr/bin/env python3
"""#1458/#1459/#1462/#1464/#1469: every attach-followups-a mutation.

Usage: negative_controls.py --check
       negative_controls.py [--compile-only] NEW_OUTPUT_DIR [mutation ...]
Every mutation must compile, fail at its bound nonce receipt, and pass again
after restoration. Source bytes must equal HEAD; no alternate baseline exists.
The shared-filter row (filter-info-inverted) and the two #1458/#1469 rows
together pin the one composer filter behind both diagnostics.
"""
import argparse
import hashlib
import json
import os
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[5]
FIXTURES = Path(__file__).resolve().parents[1]
SERVICES = ROOT / 'src/pilotdeck-gateway/src/native_process/services.rs'
TESTS = ROOT / 'src/pilotdeck-gateway/src/native_process/services_tests.rs'
sys.path.insert(0, str(FIXTURES))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import ReceiptRun, invalidate_completed, judge, passed

mutations = [
    ('pdf-size-estimate-dropped', SERVICES,
     '                    diagnostics.push(pdf_size_estimate(metadata.len(), estimated_pages));',
     '                    let _ = pdf_size_estimate(metadata.len(), estimated_pages);',
     'native_process::services_tests::unregistered_pdf_reports_size_estimate_diagnostic',
     '#1458 an unregistered pdf must carry the pdf_size_estimate diagnostic'),
    ('binary-container-severity-swapped', SERVICES,
     '''                    let severity = if BINARY_CONTAINER_EXTENSIONS.contains(&ext.as_str()) {
                        DiagnosticSeverity::Info
                    } else {
                        DiagnosticSeverity::Warning
                    };''',
     '''                    let severity = if BINARY_CONTAINER_EXTENSIONS.contains(&ext.as_str()) {
                        DiagnosticSeverity::Warning
                    } else {
                        DiagnosticSeverity::Info
                    };''',
     'native_process::services_tests::binary_container_info_diagnostic_gating',
     '#1469 a registered binary-container attachment must drop its info diagnostic'),
    ('read-failure-message-reverted', SERVICES,
     '''                        let rejection = crate::node_fs_message(&error, "read", None)
                            .unwrap_or_else(|| {
                                format!("Attachment {} could not be read: {error}", entry.path)
                            });''',
     '''                        let rejection =
                            format!("Attachment {} could not be read: {error}", entry.path);''',
     'native_process::services_tests::read_failure_fails_the_turn',
     '#1462 a directory read failure must carry the Node EISDIR message'),
    ('lease-root-hard-failure-restored', SERVICES,
     '''    let lease_canon = match fs::canonicalize(&lease_lexical) {
        Ok(resolved) if resolved.starts_with(&project_canon) => Some(resolved),
        _ => None,
    };''',
     '''    let lease_canon = match fs::canonicalize(&lease_lexical) {
        Ok(resolved) if resolved.starts_with(&project_canon) => Some(resolved),
        Err(error) => {
            return Err(failure(format!(
                "Attachment lease root cannot be resolved: {error}."
            )));
        }
        _ => None,
    };''',
     'native_process::services_tests::unresolvable_lease_root_softens_to_not_found',
     '#1459 an ELOOP lease root must not fail the turn'),
    ('lease-parent-dotdot-hard-failure-restored', SERVICES,
     '''        return Ok(LeaseGuard::Unresolved(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "attachment path is outside the permitted lease directory",
        )));''',
     '''        return Err(failure(
            "Attachment path is outside the permitted lease directory.",
        ));''',
     'native_process::services_tests::lease_parent_dotdot_softens_to_not_found',
     "#1464 a '..' lease path must not fail the turn"),
    ('lease-symlink-escape-hard-failure-restored', SERVICES,
     '''            Ok(_) => Ok(LeaseGuard::Unresolved(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "attachment resolved outside the permitted lease directory",
            ))),''',
     '''            Ok(_) => Err(failure(
                "Attachment resolved outside the permitted lease directory.",
            )),''',
     'native_process::services_tests::lease_symlink_escape_softens_to_not_found',
     '#1464 a lease symlink escape must not fail the turn'),
    ('filter-info-inverted', SERVICES,
     '        DiagnosticSeverity::Info => !registered,',
     '        DiagnosticSeverity::Info => registered,',
     'native_process::services_tests::registered_pdf_has_no_size_estimate_diagnostic',
     '#1458 a registered pdf must not carry the pdf_size_estimate diagnostic'),
]


def test_body(test, texts=None):
    if texts is None:
        texts = {TESTS: TESTS.read_text()}
    # Top-level test functions: a column-zero closing brace ends this
    # function, so a sibling's assertion cannot satisfy it.
    pattern = r'^(?:async )?fn ' + re.escape(test.rsplit('::', 1)[-1]) + r'\([^\n]*\).*?^}'
    bodies = re.findall(pattern, texts[TESTS], re.M | re.S)
    return bodies[0] if len(bodies) == 1 else ''


def check_binding_scope():
    fixture = 'fn target() {\n}\nfn sibling() {\n assert!(false, "only sibling");\n}\n'
    if 'only sibling' in test_body('target', {TESTS: fixture}):
        raise RuntimeError('#1458 binding checker accepted sibling assertion')
    if 'only sibling' not in test_body('sibling', {TESTS: fixture}):
        raise RuntimeError('#1458 binding checker lost its own assertion')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--check', action='store_true')
    parser.add_argument('--compile-only', action='store_true')
    parser.add_argument('output', nargs='?')
    parser.add_argument('selected', nargs='*')
    args = parser.parse_args()
    check_binding_scope()
    stale = []
    for name, path, old, new, test, assertion in mutations:
        text = path.read_text()
        body = test_body(test)
        if (text.count(old) != 1 or old == new or
                not body or f'"{assertion}"' not in body):
            stale.append(name)
    if stale:
        raise SystemExit('#1458 stale anchor/test/assertion bindings: ' + ', '.join(stale))
    if args.check:
        print(f'#1458 {len(mutations)} unique anchors and assertion bindings: OK')
        return 0
    selected = [row for row in mutations if not args.selected or row[0] in args.selected]
    if not selected or (args.selected and len(selected) != len(args.selected)):
        raise SystemExit('#1458 unknown or repeated mutation')
    if not args.output or not os.environ.get('CARGO_TARGET_DIR'):
        raise SystemExit('#1458 output and caller CARGO_TARGET_DIR required')
    output = Path(args.output).resolve()
    output.mkdir(parents=True, exist_ok=False)
    env = dict(os.environ)
    for key in [ReceiptRun.ENV, ReceiptRun.PARENT_ENV, 'PDRS_MUTANT', 'LIVE_E2E_MUTANT']:
        env.pop(key, None)
    guarded = [SERVICES, TESTS, Path(__file__).resolve(),
               FIXTURES / 'negative_process.py',
               FIXTURES / 'negative_suite.py',
               ROOT / 'src/pilotdeck-gateway/src/lib.rs']
    results = []
    guarded_sha256 = {}

    def run(label, run_env, test, *, compile_only=False):
        slot = wait_for_slot(ROOT / 'scripts/cargo-slots.sh', timeout=600)
        (output / f'{label}.slots.log').write_text(slot.stdout + slot.stderr)
        command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
                   '-p', 'pilotdeck-gateway', '--lib']
        command += ['--no-run'] if compile_only else [test, '--', '--exact', '--include-ignored']
        (output / f'{label}.command.json').write_text(json.dumps(command) + '\n')
        result = run_owned(command, cwd=ROOT, env=run_env, timeout=900)
        (output / f'{label}.log').write_text(result.stdout + result.stderr)
        return result

    try:
        with mutating_sources(ROOT, guarded) as originals:
            guarded_sha256 = {str(path.relative_to(ROOT)): hashlib.sha256(originals[path]).hexdigest() for path in guarded}
            for name, path, old, new, test, assertion in selected:
                item = dict(name=name, test=test, assertion=assertion,
                            status='INVALID', reason='not run', compiled=False)
                try:
                    if not args.compile_only and not passed(run(name + '-positive', env, test), test):
                        raise RuntimeError('positive bound test failed')
                    path.write_text(originals[path].decode().replace(old, new, 1))
                    if run(name + '-compile', env, test, compile_only=True).returncode:
                        raise RuntimeError('mutant did not compile; not a runtime kill')
                    item['compiled'] = True
                    if args.compile_only:
                        item.update(status='COMPILED', reason='mutated source compiled')
                    else:
                        receipt = ReceiptRun(output, name, [(test, assertion)])
                        result = run(name + '-negative', receipt.environment(env), test)
                        killed, reason, hits = judge(result, '', [(test, assertion)], receipt)
                        item.update(status='KILLED' if killed else ('SURVIVED' if passed(result, test) else 'INVALID'),
                                    reason=reason, hits=hits)
                except Exception as error:
                    item.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
                finally:
                    path.write_bytes(originals[path])
                try:
                    if not args.compile_only:
                        if not passed(run(name + '-restored', env, test), test):
                            raise RuntimeError('restored bound test failed')
                        item['restoredPass'] = True
                except Exception as error:
                    item.update(status='INVALID', reason=item['reason'] + f'; restored: {error}')
                results.append(item)
                print(f'{name}: {item["status"]}: {item["reason"]}', flush=True)
    except Exception as error:
        reason = f'source guard: {type(error).__name__}: {error}'
        invalidate_completed(results, reason)
        for name, _, _, _, test, assertion in selected[len(results):]:
            results.append(dict(name=name, test=test, assertion=assertion,
                                status='INVALID', reason=reason, compiled=False))
    counts = {state: sum(row['status'] == state for row in results)
              for state in ['KILLED', 'SURVIVED', 'INVALID', 'COMPILED']}
    temporary = output / 'summary.json.tmp'
    temporary.write_text(json.dumps(
        {'controls': results, 'counts': counts, 'guardedSha256': guarded_sha256}, indent=2) + '\n')
    temporary.replace(output / 'summary.json')
    print(f'summary: KILLED={counts["KILLED"]}, SURVIVED={counts["SURVIVED"]}, '
          f'INVALID={counts["INVALID"]}', flush=True)
    print(json.dumps({'controls': results, 'counts': counts}), flush=True)
    success = 'COMPILED' if args.compile_only else 'KILLED'
    return int(any(row['status'] != success for row in results))


if __name__ == '__main__':
    raise SystemExit(main())
