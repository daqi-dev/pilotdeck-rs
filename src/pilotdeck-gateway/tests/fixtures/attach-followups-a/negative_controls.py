#!/usr/bin/env python3
"""#1458/#1459/#1462/#1464/#1468/#1469/#1618/#1629: every attach-followups-a mutation.

Usage: negative_controls.py --check
       negative_controls.py [--compile-only] NEW_OUTPUT_DIR [mutation ...]
Every mutation must compile, fail at its bound nonce receipt, and pass again
after restoration. Source bytes must equal HEAD; no alternate baseline exists.
The shared-filter rows (filter-info-inverted, warning-gated-on-registration
and unsupported-file-message-dropped) and the two #1458/#1469 rows together
pin the one composer filter behind both diagnostics. The two #1618 item-5
rows pin the lexical pre-filter's dual spelling roots. The eight #1468 rows
pin the handle binding itself: the dropped containment check (the
multi-step choreography reaches the read only through it), the fail-open
error arm, the (dev, inode) depth check it complements, the pre-open identity
itself, the no-follow sampling of that identity (a link planted before the
approved stat must not become the approved object), and the two legitimate
homes — the upload-store root and the lease root — each with its own
containment and leaf-link terms in the shared `canonical_home` guard. The two
#1629 rows pin the last lease_guard softening: the path's own canonicalize
failure (any errno, not just NotFound) must stay a not-found diagnostic.
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
# #1887: the shared "home" guard now lives beside the store it serves, so the
# two rows that mutate it directly point here.
STORE = ROOT / 'src/pilotdeck-gateway/src/upload/mod.rs'
sys.path.insert(0, str(FIXTURES))
from negative_process import (mutating_sources, run_owned, wait_for_slot,
                              tree_lock, write_source_atomic)
from negative_suite import ReceiptRun, invalidate_completed, judge, passed

mutations = [
    ('pdf-size-estimate-dropped', SERVICES,
     '                    diagnostics.push(pdf_size_estimate(metadata.len(), estimated_pages).into());',
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
     '    let lease_canon = crate::upload::canonical_home(&lease_lexical, project_dir, &project_canon);',
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
    # #1629: the last path-inspection failure that could still reject the
    # turn — the path's own canonicalize failing with a non-`NotFound` errno
    # under a valid root (ELOOP/ENOTDIR/EACCES/…) — softened like every other
    # branch: the TS resolver's stat try/catch has no errno filter, so any
    # stat failure becomes a not-found diagnostic. The row restores the
    # pre-#1629 hard failure; the ELOOP and ENOTDIR fixtures each bind it so
    # both spellings of "any failure" are proven live (an ELOOP-only row
    # would leave the ENOTDIR test unproven, and vice versa).
    ('lease-path-hard-failure-restored', SERVICES,
     '            Err(error) => Ok(LeaseGuard::Unresolved(error)),',
     '''            Err(error) => Err(failure(format!(
                "Attachment lease path cannot be resolved: {error}."
            ))),''',
     'native_process::services_tests::symlink_loop_lease_path_softens_to_not_found_diagnostic',
     '#1629 a symlink-loop lease path must not fail the turn'),
    ('lease-path-notdir-hard-failure-restored', SERVICES,
     '            Err(error) => Ok(LeaseGuard::Unresolved(error)),',
     '''            Err(error) => Err(failure(format!(
                "Attachment lease path cannot be resolved: {error}."
            ))),''',
     'native_process::services_tests::not_directory_lease_path_softens_to_not_found_diagnostic',
     '#1629 an ENOTDIR lease path must not fail the turn'),
    ('filter-info-inverted', SERVICES,
     '        DiagnosticSeverity::Info => !registered,',
     '        DiagnosticSeverity::Info => registered,',
     'native_process::services_tests::registered_pdf_has_no_size_estimate_diagnostic',
     '#1458 a registered pdf must not carry the pdf_size_estimate diagnostic'),
    # #1469: the warning half of the one composer filter had no row of its
    # own — `filter-info-inverted` above pins the Info arm, but nothing proved
    # `Warning => true` (warnings enter the diagnostics block regardless of
    # registration). The push row covers the one step both severities share;
    # its bound test is deliberately an unregistered upload, so the warning
    # filter passes it either way and only the dropped push — not a
    # registration gate — can turn that row red.
    ('warning-gated-on-registration', SERVICES,
     '        DiagnosticSeverity::Warning => true,',
     '        DiagnosticSeverity::Warning => !registered,',
     'native_process::services_tests::registered_unsupported_extension_keeps_its_warning_diagnostic',
     '#1469 a registered attachment with an unsupported extension must still carry its warning diagnostic'),
    ('unsupported-file-message-dropped', SERVICES,
     '                    diagnostics.push(unsupported_file_message(&entry.path, &ext).into());',
     '                    let _ = unsupported_file_message(&entry.path, &ext);',
     'native_process::services_tests::unregistered_unsupported_extension_still_carries_the_warning',
     '#1469 the unsupported-file message must enter the diagnostics block'),
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
    # containment term accepts a store root that resolves outside the project;
    # dropping the no-symlink term accepts a store root that is itself a link
    # into the project. Each has its own assertion, in its own test.
    #
    # #1887 review boundary P1: after the per-component walk landed, the old
    # kill test (`upload_store_root_outside_the_project_is_not_a_home`) no
    # longer reaches the containment term — its outside-ancestor case is now
    # refused by the walk first, so this mutation SURVIVED (remote r1, mbp2,
    # `20-neg-attach.log`). The kill test is now the window-injected one: a
    # `.tmp` swapped for an outside link *after* the walk passes, which only
    # containment can refuse.
    ('upload-store-containment-dropped', STORE,
     '        Ok(resolved) if resolved.starts_with(project_canon) => Some(resolved),',
     '        Ok(resolved) => Some(resolved),',
     'native_process::services_tests::store_root_swapped_outside_after_the_walk_is_not_a_home',
     '#1887 a store root swapped outside after the walk must not become a home'),
    ('upload-store-link-root-accepted', STORE,
     '            Ok(metadata) if metadata.file_type().is_symlink() => return None,',
     '            Ok(metadata) if false => return None,',
     'native_process::services_tests::upload_store_root_outside_the_project_is_not_a_home',
     '#1468 a store root that is a link into the project must not become a home'),
    # #1887: the ancestor term. `canonical_home` now walks every component
    # between the project root and the home, so a `.tmp` that is a link to
    # another subtree of the *same* project is refused by the walk, not only by
    # containment (which it passes). This row is the pre-#1887 shape — the walk
    # skips the ancestor terms and only the leaf is still sampled — so the
    # composer accepts `src/...` as a lease home again and the paired
    # composer-side test goes red. Its read-side sibling lives in
    # `tests/fixtures/upload-gateway/negative_controls.py`
    # (`read-at-upload-root-prefix-dropped`); the two assertion strings name
    # their side, so neither row can be satisfied by the other side's test.
    ('lease-ancestor-link-accepted', STORE,
     '''        walked.push(component);
        // Leaf and ancestor are the same answer: neither may be a link.
        match fs::symlink_metadata(&walked) {
            Ok(metadata) if metadata.file_type().is_symlink() => return None,
            Ok(_) => {}
            Err(_) => return None,
        }''',
     '''        walked.push(component);
        // Leaf and ancestor are the same answer: neither may be a link.
        if walked.as_path() == root {
            match fs::symlink_metadata(&walked) {
                Ok(metadata) if metadata.file_type().is_symlink() => return None,
                Ok(_) => {}
                Err(_) => return None,
            }
        }''',
     'native_process::services_tests::lease_root_under_a_linked_tmp_ancestor_is_not_a_home',
     '#1887 a .tmp ancestor that is a link into the project must not become a home'),
    # Review GLM sibling#1: the lease root is the other legitimate home. This
    # row restores the pre-r3 shape — a bare `canonicalize` + containment, with
    # no leaf-link term — so a `.tmp/chat-upload-leases` that is a link into
    # the project becomes a lease home again and the lease-root test goes red.
    # The store rows above cannot cover it: they mutate the shared guard, which
    # this row proves is *reached* by the lease root too.
    # #1446: the dual registration of TS `collectRegisteredAttachmentReadFiles`
    # (composer :182-183) — the lexical spelling and the canonicalized realpath
    # both enter allowedReadFiles, and the note line prints the lexical one
    # (`safeAllowedAttachmentPath`, composer :167-171). Each term has its own
    # row and its own test; the two-spelling rows cannot be satisfied by each
    # other's test because each fixture asserts the exact array.
    ('dual-registration-realpath-dropped', SERVICES,
     '        for spelling in [&attachment.lexical, &attachment.resolved] {',
     '        for spelling in [&attachment.lexical] {',
     'native_process::services_tests::dual_registration_carries_both_spellings',
     '#1446 the dual registration must carry the link spelling and the physical spelling'),
    ('dual-registration-lexical-dropped', SERVICES,
     '        for spelling in [&attachment.lexical, &attachment.resolved] {',
     '        for spelling in [&attachment.resolved] {',
     'native_process::services_tests::dual_registration_dedups_the_set_across_spellings',
     '#1446 the Set must keep one entry per spelling'),
    # #2495 re-anchored: the note pass now reads `entries` (the TS composer's
    # membership domain) and normalizes the per-entry lexical spelling, so the
    # row can no longer swap `attachment.lexical`/`attachment.resolved`. The
    # same kill — the note line printing the physical spelling instead of the
    # registered link spelling — is rebuilt as a canonicalize override on the
    # per-entry lexical, the post-#2495 shape of that bug.
    ('dual-registration-note-uses-realpath', SERVICES,
     '        let normalized = lexical.to_string_lossy().into_owned();',
     '''        let mut normalized = lexical.to_string_lossy().into_owned();
        if let Ok(canonical) = fs::canonicalize(&lexical) {
            normalized = canonical.to_string_lossy().into_owned();
        }''',
     'native_process::services_tests::dual_registration_carries_both_spellings',
     '#1446 the note line must carry the link spelling'),
    # #1448 fix-r2 收编三行（PR #1418 的负控产物不在本机，按
    # be9cc9481 的改动语义在 #1448 重建；重建过程记录在 RESULT 异常小节）：
    # 扁平图片块被改回嵌套形、content 图片分支被反转（删除）、有界读退化为
    # 无界读——各自有独立杀测试，断言均为静态串（回执按格式化全文匹配）。
    ('flat-image-block-nested-again', SERVICES,
     '''    block.insert("source".into(), "base64".into());
    block.insert("data".into(), JsValue::from(data));
    block.insert("mimeType".into(), JsValue::from(mime));''',
     '''    block.insert("source".into(), object([
        ("type", "base64".into()),
        ("media_type", JsValue::from(mime)),
        ("data", JsValue::from(data)),
    ]));''',
     'native_process::services_tests::normal_lease_image_is_inlined_and_registered',
     '#1321 flat image block must carry source/data/mimeType at the top level'),
    ('image-content-branch-inverted', SERVICES,
     '''        if entry.image_type
            && let Some(content) = &entry.content
            && !entry.mime.is_empty()
        {''',
     '''        if !entry.image_type
            && let Some(content) = &entry.content
            && !entry.mime.is_empty()
        {''',
     'native_process::services_tests::content_image_branch_emits_the_flat_block',
     '#1448 the content image branch must keep message and image blocks'),
    ('read-bound-dropped', SERVICES,
     '''                let bytes = match read_bounded(&mut file, limit) {
                    Ok(bytes) => bytes,''',
     '''                let bytes = match {
                    let mut all = Vec::new();
                    std::io::Read::read_to_end(&mut file, &mut all).map(|_| all)
                } {
                    Ok(bytes) => bytes,''',
     'native_process::services_tests::grown_between_stat_and_read_hits_the_full_read_backstop',
     '#1448 a grown read must hit the full-read backstop and skip'),
    # Review minimax boundary#1 / GLM sibling#1 (fix-r1): the row below was
    # dropped from this list when the #1446 rows were added, leaving
    # `lease_root_link_is_not_a_home` (#1468) with no kill row of its own —
    # the `lease-root-hard-failure-restored` row above shares the call site
    # but its own bound test is the #1459 ELOOP one, so a dead #1468 fixture
    # would go unnoticed. The anchor is this row's own (the call site plus the
    # comment line that follows it), so it never collides with the
    # `lease-root-hard-failure-restored` anchor on the same source line.
    ('lease-root-link-accepted', SERVICES,
     '''    let lease_canon = crate::upload::canonical_home(&lease_lexical, project_dir, &project_canon);
    // #1468: the second legitimate home of a lease object.''',
     '''    let lease_canon = match fs::canonicalize(&lease_lexical) {
        Ok(resolved) if resolved.starts_with(&project_canon) => Some(resolved),
        _ => None,
    };
    // #1468: the second legitimate home of a lease object.''',
     'native_process::services_tests::lease_root_link_is_not_a_home',
     '#1468 a lease root that is a link into the project must not become a home'),
    # #1618 item 5 (P1): the lexical pre-filter's dual spelling roots. The
    # first row reverts `lease_lexical` to the canonicalized root (the
    # pre-#2423 shape), so a symlinked project's alias-spelled lease paths
    # take the not-found branch; the second drops the canonical entry, so a
    # physically-spelled lease path does. Each row has its own
    # static-assertion bound test — the pre-existing
    # `symlinked_project_spelling_keeps_lease_attachments` kills the same
    # behaviors, but its assertions are formatted (`got: …`) and the #946
    # receipt protocol can only bind an exact static string.
    ('lease-logical-root-canonicalized', SERVICES,
     '    let lease_lexical = project_dir.join(".tmp").join("chat-upload-leases");',
     '    let lease_lexical = project_canon.join(".tmp").join("chat-upload-leases");',
     'native_process::services_tests::lease_logical_root_keeps_alias_spelled_attachments_inline',
     '#1618 the logical spelling of the lease root must keep alias-spelled lease attachments inline'),
    ('lease-canonical-spelling-dropped', SERVICES,
     '    let lease_lexical_roots = [lease_lexical.clone(), lease_lexical_canon];',
     '    let lease_lexical_roots = [lease_lexical.clone()];',
     'native_process::services_tests::lease_canonical_root_keeps_physically_spelled_attachments_inline',
     '#1618 the canonical spelling of the lease root must keep physically-spelled lease attachments inline'),
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
    # #2795 fix-r4（评审 sibling P1 同族跟改，fix-r3 mcp-http-encoding 先例）：
    # 锚点预检也要持树锁读——兄弟变异运行器在自己的 write 与 restore 之间可能
    # 把变异体留在盘上，无锁读会把核对错在它不拥有的源上；锁忙 / 读失败是
    # 拒跑（2），不是宿主会读成 rc 1 的裸 traceback。本窗口只包只读预检面，
    # 与后续 mutating_sources 是顺序关系，锁先释放再重取，无同进程嵌套。
    try:
        with tree_lock(ROOT, timeout=60):
            stale = []
            for name, path, old, new, test, assertion in mutations:
                text = path.read_text()
                body = test_body(test)
                if (text.count(old) != 1 or old == new or
                        not body or f'"{assertion}"' not in body):
                    stale.append(name)
    except (TimeoutError, OSError, UnicodeDecodeError) as error:
        print(f'tree lock or anchor read failed: '
              f'{type(error).__name__}: {error}', file=sys.stderr)
        raise SystemExit(2)
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
    guarded = [SERVICES, TESTS, STORE, Path(__file__).resolve(),
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
                    # #2495 fix-r1 (concurrency review P2 same-family co-fix):
                    # one atomic rename, never a truncating in-place write — a
                    # SIGKILL mid-window must not leave a half-written source
                    # (#1903, write_source_atomic).
                    write_source_atomic(
                        ROOT, path, originals[path].decode().replace(old, new, 1).encode())
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
                    write_source_atomic(ROOT, path, originals[path])
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
