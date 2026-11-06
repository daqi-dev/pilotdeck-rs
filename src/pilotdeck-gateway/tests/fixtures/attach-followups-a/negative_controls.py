#!/usr/bin/env python3
"""#1458/#1459/#1462/#1464/#1468/#1469: every attach-followups-a mutation.

Usage: negative_controls.py --check
       negative_controls.py [--compile-only] NEW_OUTPUT_DIR [mutation ...]
Every mutation must compile, fail at its bound nonce receipt, and pass again
after restoration. Source bytes must equal HEAD; no alternate baseline exists.
The shared-filter row (filter-info-inverted) and the two #1458/#1469 rows
together pin the one composer filter behind both diagnostics. The six #1468
rows pin the handle binding itself: the dropped containment check (the
multi-step choreography reaches the read only through it), the fail-open
error arm, the (dev, inode) depth check it complements, the pre-open identity
itself, the no-follow sampling of that identity (a link planted before the
approved stat must not become the approved object), and the upload-store
root containment guard that makes the store a second legitimate home.
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
     '''                    let severity = if is_binary_container(&ext) {
                        DiagnosticSeverity::Info
                    } else {
                        DiagnosticSeverity::Warning
                    };''',
     '''                    let severity = if is_binary_container(&ext) {
                        DiagnosticSeverity::Warning
                    } else {
                        DiagnosticSeverity::Info
                    };''',
     'native_process::services_tests::binary_container_info_diagnostic_gating',
     '#1469 a registered binary-container attachment must drop its info diagnostic'),
    ('read-failure-message-reverted', SERVICES,
     '''                        return Err(failure(crate::node_fs_message_or_unknown(
                            &error, "read", None,
                        )));''',
     '''                        return Err(failure(
                            format!("Attachment {} could not be read: {error}", entry.path)
                        ));''',
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
    ('handle-path-check-dropped', SERVICES,
     '''        Ok(handle) => {
            let named_inside = handle.starts_with(lease_root)
                || upload_store.is_some_and(|store| handle.starts_with(store));
            // The name says the object has a legitimate home; the identity
            // says it is the object the pre-open validation approved. A
            // rename after the open satisfies the first and not the second.
            let same_object = file
                .metadata()
                .map(|opened| attachment_identity_matches(approved, &opened))
                .unwrap_or(false);
            named_inside && same_object
        }''',
     '        Ok(_) => true,',
     'native_process::services_tests::multi_step_swap_across_all_windows_is_not_found',
     '#1468 a handle outside the lease root must never be read'),
    # The pre-open identity is the one term inside the binding that a rename
    # after the open defeats; dropping only it must let the renamed-in
    # outside handle through, so the named test goes red.
    ('approved-identity-dropped', SERVICES,
     '''            let same_object = file
                .metadata()
                .map(|opened| attachment_identity_matches(approved, &opened))
                .unwrap_or(false);''',
     '            let same_object = true;',
     'native_process::services_tests::rename_after_open_is_not_found',
     '#1468 a renamed-in outside handle must be diagnosed not-found'),
    ('handle-path-fail-open-passes', SERVICES,
     '''        // #1468: an unavailable handle path is never approved.
        Err(_) => false,''',
     '''        // #1468: an unavailable handle path is never approved.
        Err(_) => true,''',
     'native_process::services_tests::handle_path_unavailable_fails_closed',
     '#1468 an unavailable handle path must fail closed'),
    ('identity-depth-removed', SERVICES,
     '        if !attachment_identity_matches(&path_stat, &metadata) {',
     '        if false {',
     'native_process::services_tests::leaf_swapped_after_revalidation_fails_identity',
     '#1468 the (dev, inode) depth check must stay'),
    # The pre-open identity is only meaningful if it is sampled without
    # following a link: restoring `fs::metadata` lets a link planted in the
    # `[canonicalize, approved stat]` window become the approved object, and
    # the after-open rename then lands the outside inode inside the lease
    # root with every remaining check passing.
    ('approved-stat-follows-link', SERVICES,
     '        let approved_stat = match fs::symlink_metadata(&resolved) {',
     '        let approved_stat = match fs::metadata(&resolved) {',
     'native_process::services_tests::leaf_swapped_before_approved_stat_is_not_found',
     '#1468 a link planted before the approved stat must never inline outside bytes'),
    # The upload-store guard is two independent terms. Dropping the
    # containment term accepts a store root under an outside ancestor (the
    # leaf is a real directory, so only this term sees it); dropping the
    # no-symlink term accepts a store root that is itself a link into the
    # project. Each has its own assertion in the same unit test.
    ('upload-store-containment-dropped', SERVICES,
     '        Ok(resolved) if resolved.starts_with(project_canon) => Some(resolved),',
     '        Ok(resolved) => Some(resolved),',
     'native_process::services_tests::upload_store_root_outside_the_project_is_not_a_home',
     '#1468 a store root under an outside ancestor must not become a home'),
    ('upload-store-link-root-accepted', SERVICES,
     '        Ok(metadata) if metadata.file_type().is_symlink() => return None,',
     '        Ok(metadata) if false => return None,',
     'native_process::services_tests::upload_store_root_outside_the_project_is_not_a_home',
     '#1468 a store root that is a link into the project must not become a home'),
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
