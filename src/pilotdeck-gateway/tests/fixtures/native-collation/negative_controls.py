#!/usr/bin/env python3
"""#132 HEAD-only product mutations; compile each, classify all, verify restore.

Run after the coordinator commits the patch. This script deliberately refuses
uncommitted/new sources. The caller supplies CARGO_TARGET_DIR; no alternate
repository or build directory is created. Use --check-anchors before commit.
"""
import io
import os
from pathlib import Path
import sys

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import (mutating_sources, run_owned, wait_for_slot,
                               tree_lock, write_source_atomic)
from negative_selection import registered, refuse
from negative_suite import Case, classify, invalidate_completed, passed, publish_summary

root = Path(__file__).resolve().parents[5]
FIXTURE = Path(__file__).resolve().parent
PROJECT = root / 'src/pilotdeck-gateway/src/project_files.rs'
CATALOG = root / 'src/pilotdeck-gateway/src/model_catalog.rs'
SESSION = root / 'src/pilotdeck-gateway/src/session_model.rs'
COMPARISON = root / 'src/vendor/icu_collator/src/comparison.rs'
BOUNDARY = root / 'src/pilotdeck-gateway/tests/unsafe_boundary.rs'
MANIFEST = root / 'src/vendor/icu_collator/Cargo.toml'
ARCHIVE = root / 'src/pilotdeck-gateway/src/native_archive.rs'
TRANSCRIPT = root / 'src/pilotdeck-gateway/src/storage/transcript.rs'
WORKSPACE = root / 'src/pilotdeck-gateway/src/storage/workspace_snapshot.rs'
STORAGE_TESTS = root / 'src/pilotdeck-gateway/src/storage/tests.rs'
DISPATCH = root / 'src/pilotdeck-gateway/src/dispatch.rs'
ORACLE = root / 'src/pilotdeck-gateway/tests/fixtures/native-collation/oracle.json'
TRANSCRIPT_ORACLE = root / 'src/pilotdeck-gateway/tests/fixtures/transcript-collation/oracle.json'
VENDOR_SCOPE = 'icu_collator_vendor_scope_matches_approved_lib_only_boundary'
PREFIX = 'pairing::native_collation_tests::'
MATRIX = PREFIX + 'native_matrix_matches_node'
PAIRS = PREFIX + 'native_comparator_matches_node_all_pairs'
mutations = [
    ('icu-vendor-registration-lost', BOUNDARY,
     '    "vendor/icu_collator",', '    "vendor/unregistered-icu",',
     VENDOR_SCOPE, '#132 ICU vendor must be registered'),
    ('icu-test-target-restored', MANIFEST,
     '[dependencies.databake]',
     '[[test]]\nname = "restored_test"\npath = "src/lib.rs"\n\n[dependencies.databake]',
     VENDOR_SCOPE, '#132 ICU exposes only one lib target'),
    ('icu-bench-target-restored', MANIFEST,
     '[dependencies.databake]',
     '[[bench]]\nname = "restored_bench"\npath = "src/lib.rs"\nharness = false\n\n[dependencies.databake]',
     VENDOR_SCOPE, '#132 ICU exposes only one lib target'),
    ('icu-lib-only-guard-relaxed', BOUNDARY,
     '            kinds,\n            ["lib"],',
     '            kinds,\n            kinds,',
     VENDOR_SCOPE, '#132 ICU non-lib target must be rejected'),
    ('code-unit-order', PROJECT,
     'collator.compare_utf16_preserving_surrogates(left.units(), right.units())',
     'left.units().cmp(right.units())', PAIRS, '#132 comparator raw weights'),
    ('replacement-fold', PROJECT,
     'collator.compare_utf16_preserving_surrogates(left.units(), right.units())',
     'collator.compare_utf16(left.units(), right.units())', MATRIX, '#132 matrix wire bytes'),
    ('pua-collision', COMPARISON,
     'CollationElement::new_implicit_from_code_point(u32::from(*surrogate))',
     'CollationElement::new_implicit_from_code_point(0xE000 + u32::from(*surrogate) - 0xD800)',
     PAIRS, '#132 comparator raw weights'),
    ('catalog-host-remains', CATALOG,
     '    items.sort_by(|a, b| node_compare_utf16(&collator, &a.0, &b.0));',
     '    if items.len() > 1 && items.iter().any(|(id, _)| char::decode_utf16(id.units().iter().copied()).any(|unit| unit.is_err())) {\n'
     '        return Err(config::Error { code: "JS_HOST_REQUIRED".into(), message: "".into(), diagnostics: vec![], name: "Error", requires_host: true });\n'
     '    }\n    items.sort_by(|a, b| node_compare_utf16(&collator, &a.0, &b.0));',
     MATRIX, '#132 matrix must terminate natively'),
    ('session-host-remains', SESSION,
     '    if entries.len() > 1 {\n        let collator',
     '    if entries.iter().any(|e| char::decode_utf16(field(e, "createdAt").as_string().unwrap().units().iter().copied()).any(|unit| unit.is_err())) {\n'
     '        let mut host = error("JS_HOST_REQUIRED", ""); host.requires_host = true; return Err(host);\n'
     '    }\n    if entries.len() > 1 {\n        let collator',
     MATRIX, '#132 matrix must terminate natively'),
    ('valid-pair-split', COMPARISON,
     'Ok(c) => length += c.len_utf16(),',
     'Ok(c) => { if c.len_utf16() == 2 { break; } length += 1; }',
     PAIRS, '#132 comparator raw weights'),
    ('french-secondary-forward', PROJECT,
     'collator.with_backward_second_level(true)',
     'collator.with_backward_second_level(false)',
     PAIRS, '#132 comparator raw weights'),
    ('identical-raw-units', COMPARISON,
     'return self.preserving_identical(left).cmp(&self.preserving_identical(right));',
     'return left.cmp(right);',
     PREFIX + 'preserving_identical_strength_keeps_normalization_and_ignorables',
     '#132 identical canonical equivalence'),
    ('identical-tie-dropped', COMPARISON,
     'if result == Ordering::Equal && self.options.strength() == Strength::Identical {',
     'if false && result == Ordering::Equal && self.options.strength() == Strength::Identical {',
     PREFIX + 'preserving_identical_strength_keeps_normalization_and_ignorables',
     '#132 identical retains primary ignorables'),
    # The default spelling keeps fr-CA's repair under a Latin script tag; this
    # mutation drops that widening, so `fr-Latn-CA` falls back to generic `fr`.
    # The 14-locale oracle compares the *explicit* spelling and is blind to
    # this, so the control binds the dedicated default-spelling test.
    ('french-script-fallback-lost', PROJECT,
     'script.is_none_or(|script| script.to_string() == "Latn")',
     'script.is_none_or(|_| false)',
     'project_files::tests::fr_ca_script_tag_follows_the_default_spelling_not_the_explicit_one',
     '#132 fr_Latn_CA default keeps fr-CA\'s backward secondary'),
    # The opposite direction: widening the Latin test to every script would
    # repair `fr-Cyrl-CA`, which Node's default collator does not (measured
    # row `fr_Cyrl_CA.UTF-8` in `pilotdeck-cli/tests/fixtures/skills-fr-ca-probe.json`).
    ('french-script-widened', PROJECT,
     'script.is_none_or(|script| script.to_string() == "Latn")',
     'script.is_none_or(|_| true)',
     'project_files::tests::fr_ca_script_tag_follows_the_default_spelling_not_the_explicit_one',
     '#132 a non-Latin script does not get fr-CA\'s backward secondary'),
    # Paired mutation for the explicit spelling: dropping its script condition
    # makes `new Intl.Collator("fr-Latn-CA")` take the fr-CA repair, which the
    # oracle records as generic `fr` (sign -1 for `coté` vs `côte`).
    ('french-explicit-script-repaired', PROJECT,
     '|| !process_default && locale.id.script.is_none()',
     '|| !process_default',
     PAIRS, '#132 comparator raw weights'),
    # Paired mutations for the three boundaries this task retires plus the
    # ownership handoff: each one reinstates the pre-#132 behaviour and must
    # turn its bound test red.
    ('archive-host-remains', ARCHIVE,
     '    if entries.len() > 1 {\n        let locale = node_locale(',
     '    let mut counts = std::collections::HashMap::new();\n'
     '    for entry in &entries {\n'
     '        let sequence = field(entry, "sequence").as_number().unwrap();\n'
     '        *counts.entry(if sequence == 0.0 { 0 } else { sequence.to_bits() }).or_insert(0usize) += 1;\n'
     '    }\n'
     '    if entries.iter().any(|entry| {\n'
     '        let sequence = field(entry, "sequence").as_number().unwrap();\n'
     '        counts[&if sequence == 0.0 { 0 } else { sequence.to_bits() }] > 1\n'
     '            && char::decode_utf16(field(entry, "createdAt").as_string().unwrap().units().iter().copied())\n'
     '                .any(|unit| unit.is_err())\n'
     '    }) {\n'
     '        return Ok(None);\n'
     '    }\n'
     '    if entries.len() > 1 {\n        let locale = node_locale(',
     'native_archive::tests::archive_lone_surrogate_ties_answer_natively_with_ts_bytes',
     'unexpected forward "ties-lone-native_archive_manifest"'),
    ('transcript-host-remains', TRANSCRIPT,
     '    let collator = if ties.is_empty() {\n        None\n    } else {\n        Some(',
     '    let collator = if ties.is_empty() {\n        None\n    } else {\n'
     '        for entry in &read.entries {\n'
     '            let n = field(entry, "sequence").as_number().unwrap();\n'
     '            if ties.contains(&if n == 0.0 { 0 } else { n.to_bits() })\n'
     '                && char::decode_utf16(field(entry, "createdAt").as_string().unwrap().units().iter().copied())\n'
     '                    .any(|unit| unit.is_err())\n'
     '            {\n'
     '                return Err(invalid("Transcript timestamp collation requires the Node host for unpaired surrogates."));\n'
     '            }\n'
     '        }\n'
     '        Some(',
     'storage::tests::equal_sequence_created_at_keeps_isolated_utf16_weights',
     "#132 isolated surrogates keep Node's implicit weights"),
    ('workspace-host-remains', WORKSPACE,
     '        descriptors.sort_by(|left, right| {',
     '        if descriptors.iter().any(|descriptor| {\n'
     '            descriptor.created_at.as_ref().is_some_and(|value| {\n'
     '                char::decode_utf16(value.units().iter().copied()).any(|unit| unit.is_err())\n'
     '            })\n'
     '        }) {\n'
     '            return Err(WorkspaceSnapshotError::Invalid(\n'
     '                "Snapshot createdAt collation requires the Node host for unpaired surrogates.".into(),\n'
     '            ));\n'
     '        }\n'
     '        descriptors.sort_by(|left, right| {',
     'runtime::snapshot::tests::dispatch_answers_the_collation_case_locally',
     # The reinstated refusal is a *local* error object (`Invalid` renders
     # through `project_error` with `requires_host: false`). Since #2194
     # dispatch carries it as `Error`, never a flattened response, so the
     # test's variant guard rejects it with this same marker (the payload
     # check below stays the success-path assertion). Bind that message,
     # not the forward guard above it.
     '#132 collation case answers with a list, not an error'),
    ('archive-local-method-lost', DISPATCH,
     '    "native_archive_manifest",\n', '',
     'pairing::tests::archive_lone_surrogate_ties_never_reach_upstream',
     # The forwarded request never produces a frame, so the test dies in the
     # read helper's `expect("read timeout")`; bind that message, not the
     # unrelated worker deadline.
     'read timeout'),
    # #132 P2-5: the matrix pairing test had no reverse mutation. Dropping one
    # locally answered business method makes every locale row forward its first
    # catalog request upstream, so the parent test must red with the failed
    # locale child named in the panic.
    ('catalog-local-method-lost', DISPATCH,
     '    "model_catalog_list",\n    "session_model_get",',
     '    "session_model_get",',
     PREFIX + 'matrix_pairing_has_zero_business_upstream',
     # The wrapper spawns the worker in a child process, so its panic names the
     # child: `...matrix_pairing_worker/en_US.UTF-8: expected one passed test`.
     'matrix_pairing_worker/en_US.UTF-8'),
    ('oracle-commit-corrupt', ORACLE,
     '"sourceCommit":"cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85"',
     '"sourceCommit":"0000000000000000000000000000000000000000"',
     PAIRS, '#1109 oracle baseline commit mismatch'),
    # The transcript gold has the same provenance contract; corrupting it must
    # trip the git-free commit check before any entry comparison runs.
    ('transcript-oracle-commit-corrupt', TRANSCRIPT_ORACLE,
     '"sourceCommit":"cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85"',
     '"sourceCommit":"0000000000000000000000000000000000000000"',
     'storage::tests::equal_sequence_created_at_keeps_isolated_utf16_weights',
     '#1109 oracle baseline commit mismatch'),
]
# fix-r3 (minimax boundary#3, DS r4b sibling family): the run denominator must
# be a literal, not `len(mutations)` — a dropped row would silently shrink
# the run (CLI twin's EXPECTED_NAMES form).
EXPECTED_NAMES = (
    'icu-vendor-registration-lost',
    'icu-test-target-restored',
    'icu-bench-target-restored',
    'icu-lib-only-guard-relaxed',
    'code-unit-order',
    'replacement-fold',
    'pua-collision',
    'catalog-host-remains',
    'session-host-remains',
    'valid-pair-split',
    'french-secondary-forward',
    'identical-raw-units',
    'identical-tie-dropped',
    'french-script-fallback-lost',
    'french-script-widened',
    'french-explicit-script-repaired',
    'archive-host-remains',
    'transcript-host-remains',
    'workspace-host-remains',
    'archive-local-method-lost',
    'catalog-local-method-lost',
    'oracle-commit-corrupt',
    'transcript-oracle-commit-corrupt',
)


def check_roster():
    try:
        registered(mutations, EXPECTED_NAMES)
    except ValueError as error:
        refuse(str(error))


def row_error(name, count, old, new, test, marker):
    """One validator for BOTH anchor paths (fix-r3, DS r1 sibling#1 /
    minimax concurrency#1 / boundary#2 / anthropic simplify#1: the run loop
    used to check only count and old == new, so an empty marker survived
    into classify(), where `'' in evidence` matches ANY panic -- a kill for
    the wrong reason)."""
    if count != 1 or old == new:
        return f'{name}: stale/invalid anchor: expected=1 actual={count}'
    if not test or not marker:
        return f'{name}: empty test or marker binding'
    return None


def check_anchors(rows=None, stdout=None, stderr=None):
    """Exit non-zero on any expired anchor; an empty manifest never passes.

    fix-r4 (DS fix-r3 concurrency#1 / boundary#1): the table is passed as
    `rows`, never rewritten in place -- `anchor_selfcheck.py` statically
    rejects in-place mutation-table updates (clear/slice/element writes),
    which would turn the repo anchor gate red as a false FAIL."""
    rows = mutations if rows is None else rows
    stdout = sys.stdout if stdout is None else stdout
    stderr = sys.stderr if stderr is None else stderr
    errors = []
    # #2795 fix-r4（评审 sibling P1 同族跟改；MM boundary#2 指认 CI 已接线）：
    # 锚点核对真实读 8 个产品源（dispatch.rs 是 ≥6 个 runner 的共享变异目标），
    # 要持树锁读——兄弟变异运行器在自己的 write 与 restore 之间可能把变异体
    # 留在盘上，无锁读会把核对错在它不拥有的源上；锁忙 / 读失败是拒跑（2）。
    # 本窗口只包只读核对面，与后续 mutating_sources 是顺序关系，锁先释放
    # 再重取，无同进程嵌套。
    try:
        with tree_lock(root, timeout=60):
            # #2170 P3: the count is genuinely deduplicated per (path, old) anchor —
            # the old "N unique anchors" wording counted table rows.
            seen, duplicates = set(), []
            for name, path, old, new, test, marker in rows:
                key = (str(path), old)
                if key in seen:
                    duplicates.append(name)
                seen.add(key)
                error = row_error(name, path.read_text(encoding='utf-8').count(old),
                                  old, new, test, marker)
                if error:
                    errors.append(error)
    except (TimeoutError, OSError, UnicodeDecodeError) as error:
        print(f'tree lock or anchor read failed: '
              f'{type(error).__name__}: {error}', file=stderr)
        return 2
    for error in errors:
        print(error, file=stdout)
    if not rows:
        # #1364 P0-a form: an empty manifest must never print "0 anchors
        # checked" and return 0.
        print('empty mutation manifest; refusing to pass', file=stderr)
        return 2
    print(f'{len(seen)} unique anchors checked ({len(rows)} rows)', file=stdout)
    if duplicates:
        print('rows sharing an anchor: ' + ', '.join(duplicates), file=stdout)
    return int(bool(errors))


def self_check():
    """Negative control for this gate (#1364 P0-a): the empty-manifest and
    empty-marker fail-closed branches must actually fire.

    fix-r4 (DS fix-r3 concurrency#2 / sibling#6): check_roster() runs first,
    so the CI `--self-check` entry also pins EXPECTED_NAMES against the
    shipped table (drift -> rc=2, never silently green)."""
    if not mutations:
        print('self-check FAILED: the shipped mutation manifest is empty', file=sys.stderr)
        return 1
    check_roster()
    out, err = io.StringIO(), io.StringIO()
    status = check_anchors(rows=[], stdout=out, stderr=err)
    if status == 0:
        print('self-check FAILED: an empty manifest passed the anchor gate', file=sys.stderr)
        return 1
    if 'empty mutation manifest' not in err.getvalue():
        print('self-check FAILED: empty manifest did not report the explicit error',
              file=sys.stderr)
        return 1
    # The empty-marker arm is bound to ITS message (#2728 lesson: a criterion
    # that only matches the row name can be satisfied by a stale-anchor
    # error, passing for the wrong reason). A stale first-row anchor makes
    # this arm refuse to pass, so the branch can only be verified on a table
    # whose first anchor is live.
    blank_row = (*mutations[0][:5], '')
    out2 = io.StringIO()
    status2 = check_anchors(rows=[blank_row] + mutations[1:], stdout=out2)
    if status2 == 0 or 'empty test or marker binding' not in out2.getvalue():
        print('self-check FAILED: an empty marker binding passed the anchor gate',
              file=sys.stderr)
        return 1
    print(f'self-check: empty manifest fails closed (rc={status}); '
          f'empty marker binding fails closed (rc={status2}); '
          f'shipped manifest still has {len(mutations)} anchors: ok')
    return 0


def main():
    args = sys.argv[1:]
    if args == ['--self-check']:
        return self_check()
    if args == ['--check-anchors']:
        check_roster()
        return check_anchors()
    compile_only = '--compile-only' in args
    args = [a for a in args if a != '--compile-only']
    # fix-r4 (DS fix-r3 sibling#2 / boundary#2): startup rejections go
    # through refuse() (rc=2, "never started") like the CLI twin — a string
    # SystemExit is rc=1 and reads as "ran and did not come out green".
    for a in args:
        if a.startswith('-'):
            refuse(f'#132 unknown option or mode combination: {a}')
    if not args:
        refuse('usage: --check-anchors | --self-check | '
               '[--compile-only] NEW_OUTPUT_DIR [mutation ...]')
    check_roster()
    selected = [row for row in mutations if not args[1:] or row[0] in args[1:]]
    if not selected or (args[1:] and len(selected) != len(args[1:])):
        refuse('#132 unknown or duplicate mutation')
    env = dict(os.environ)
    if not env.get('CARGO_TARGET_DIR'):
        refuse('#132 caller must supply CARGO_TARGET_DIR')
    out = Path(args[0]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
               '-p', 'pilotdeck-gateway']
    results = []

    def run(label, arguments, test):
        slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=900)
        with (out / (label + '.slots.log')).open('x') as stream:
            stream.write(slot.stdout + slot.stderr)
        # Lib tests carry a module path; integration test names do not.
        target = ['--lib'] if '::' in test else ['--test', 'unsafe_boundary']
        result = run_owned(command + target + arguments, cwd=root, env=env, timeout=900)
        with (out / (label + '.log')).open('x') as stream:
            stream.write(result.stdout + result.stderr)
        return result

    guarded = [PROJECT, CATALOG, SESSION, COMPARISON, BOUNDARY, MANIFEST, ARCHIVE, TRANSCRIPT,
               WORKSPACE, STORAGE_TESTS, DISPATCH, FIXTURE / 'regression.rs',
               FIXTURE / 'oracle.json', FIXTURE / 'generate.mts', Path(__file__).resolve(),
               TRANSCRIPT_ORACLE,
               root / 'src/vendor/icu_collator/src/elements.rs',
               *(FIXTURE.parent / name for name in (
                   'negative_process.py', 'negative_suite.py', 'negative_panic.py', 'pending_reanchor.py'))]
    try:
        with mutating_sources(root, guarded) as originals:
            for name, path, old, new, test, marker in selected:
                row = dict(mutation=name, test=test, assertion=marker,
                           status='INVALID', reason='not run', compiled=False)
                arguments = [test, '--', '--exact']
                try:
                    original = originals[path].decode()
                    # fix-r3: shared validator with check_anchors -- an empty
                    # marker must refuse HERE too, not only in --check-anchors.
                    error = row_error(name, original.count(old), old, new, test, marker)
                    if error:
                        raise ValueError(error)
                    if not compile_only and not passed(run(name + '-positive', arguments, test), test):
                        raise RuntimeError('positive control did not pass')
                    # #1903: a truncating write here would leave a half-file
                    # on SIGKILL; the rename publish never does.
                    write_source_atomic(root, path,
                                        original.replace(old, new, 1).encode())
                    if run(name + '-compile', ['--no-run'], test).returncode != 0:
                        raise RuntimeError('mutant did not compile; never a kill')
                    row['compiled'] = True
                    if compile_only:
                        row.update(status='COMPILED', reason='mutant compiles; runtime check still required')
                    else:
                        status, reason = classify(run(name + '-negative', arguments, test),
                                                  Case(name, path, old, new, test, marker))
                        row.update(status=status, reason=reason)
                except Exception as error:
                    row.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
                finally:
                    write_source_atomic(root, path, originals[path])
                try:
                    restored = run(name + '-restored', ['--no-run'] if compile_only else arguments, test)
                    if restored.returncode or (not compile_only and not passed(restored, test)):
                        raise RuntimeError('restored positive did not pass')
                except Exception as error:
                    row.update(status='INVALID', reason=row['reason'] + f'; restore: {error}')
                results.append(row)
                print(f'{name}: {row["status"]}: {row["reason"]}', flush=True)
    except BaseException as error:
        reason = f'source guard: {type(error).__name__}: {error}'
        invalidate_completed(results, reason)
        for name, _path, _old, _new, test, marker in selected[len(results):]:
            results.append(dict(mutation=name, test=test, assertion=marker,
                                status='INVALID', reason=reason, compiled=False))
            print(f'{name}: INVALID: {reason}', flush=True)
    publish_summary(out, results)
    expected = 'COMPILED' if compile_only else 'KILLED'
    return int(any(row['status'] != expected for row in results))


if __name__ == '__main__':
    raise SystemExit(main())
