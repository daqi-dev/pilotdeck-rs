#!/usr/bin/env python3
"""#1011 projection cache controls; run only after the sources are committed."""
import json
import os
from pathlib import Path
import sys

root = Path(__file__).resolve().parents[6]
sys.path.insert(0, str(Path(__file__).resolve().parents[2]))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import Case, bind_module_file, declares_test, run_cases, publish_summary

mutations = [
    ('cache-oracle-regeneration',
     'src/pilotdeck-gateway/src/runtime/session/factory_tests.rs',
     'crate::test_baseline::regenerates_identically("session-factory/projection-cache");',
     'crate::test_baseline::regenerates_identically_with("session-factory/projection-cache", true);',
     'runtime::session::factory::tests::projection_cache_regenerates_identically',
     '#280 session-factory/projection-cache/oracle.json differs'),
    ('generator-leftovers',
     'src/pilotdeck-gateway/tests/fixtures/session-factory/projection-cache/generate.mts',
     'fs.rmSync(work, {recursive: true, force: true});',
     'void work; // #1011 retain generated work directory',
     'runtime::session::factory::projection_cache_tests::generator_cleans_owned_work',
     '#1011 generator left files behind in OUT'),
    # #1890: the hard-deadline exit skips the lexical finally, so its own
    # cleanup is the only thing that removes the work root on that path.
    ('generator-deadline-cleanup-skipped',
     'src/pilotdeck-gateway/tests/fixtures/session-factory/projection-cache/generate.mts',
     'fs.rmSync(ownedWork, {recursive: true, force: true, maxRetries: 2, retryDelay: 10});',
     'void ownedWork; // #1890 deadline expiry keeps its work root',
     'runtime::session::factory::projection_cache_tests::generator_deadline_cleans_owned_work',
     '#1011 deadline exit leaves no work-* behind'),
    # #1890: a removal that throws must be named on stderr before the deadline
    # exit; swallowing it leaves the caller unable to tell an owned work root
    # was left behind.
    ('generator-deadline-cleanup-silent',
     'src/pilotdeck-gateway/tests/fixtures/session-factory/projection-cache/generate.mts',
     'report.push(`#1011 generator deadline cleanup failed for ${ownedWork}: ${(error as Error).message}`);',
     'void error; // #1890 swallow the cleanup failure',
     'runtime::session::factory::projection_cache_tests::generator_deadline_reports_cleanup_failure',
     '#1011 generator deadline names the cleanup failure'),
    ('stale-suffix-skipped',
     'src/pilotdeck-gateway/src/runtime/session/projection_cache.rs',
     'if sequence(entry) > watermark {',
     'if sequence(entry) < -2.0 {',
     'runtime::session::factory::projection_cache_tests::stale_load',
     '#1011 projection equals TS durable state'),
    ('invalid-envelope-accepted',
     'src/pilotdeck-gateway/src/runtime/session/projection_cache.rs',
     'valid_envelope(value, &self.key.session_id, &entries)',
     '(valid_envelope(value, &self.key.session_id, &entries) || true)',
     'runtime::session::factory::projection_cache_tests::invalid_future',
     '#1011 invalid envelope is rejected before hydrate'),
    ('concurrent-append-lost',
     'src/pilotdeck-gateway/src/runtime/session/factory.rs',
     'self.inner.append(key, entry)?;',
     'if field(entry, "turnId").as_string().is_none_or(|id| !id.equals_str("concurrent-2")) { self.inner.append(key, entry)?; }',
     'runtime::session::factory::projection_cache_tests::concurrent_append_keeps_both_commits',
     '#1011 durable log entries equal TS including parent chain'),
    ('turn-result-not-scheduled',
     'src/pilotdeck-gateway/src/runtime/session/factory.rs',
     'self.cache.schedule();',
     'if false { self.cache.schedule(); }',
     'runtime::session::factory::projection_cache_tests::turn_result_schedules_and_binding_dispose_waits',
     '#1011 checkpoint watermark equals TS'),
    # fix-r2 (#1011): SessionProjection::snapshot's disposed guard is the TS
    # assertActive port and the projection-level recreate-disposed rejection
    # point; this direct storage-level pin does not pass through the adapter
    # guard in front of it. Run only after this script itself is committed.
    ('disposed-guard-removed',
     'src/pilotdeck-gateway/src/runtime/session/factory.rs',
     'if self.disposed.load(Ordering::SeqCst) {',
     'if false && self.disposed.load(Ordering::SeqCst) {',
     'runtime::session::factory::projection_cache_tests::missing_recreate',
     '#1011 fenced predecessor disposed before replacement load'),
    ('capture-single-attempt',
     'src/pilotdeck-gateway/src/runtime/session/projection_cache.rs',
     'for _ in 0..3 {',
     'for _ in 0..1 {',
     'runtime::session::factory::projection_cache_tests::capture_retries_three_times',
     '#1011 capture retry budget is exactly three'),
    # fix-r1 (opus P2): removing the read-side size bound would allocate and
    # parse the whole oversized cache; the missing-file fallback must win.
    ('oversized-checkpoint-accepted',
     'src/pilotdeck-gateway/src/runtime/session/projection_cache.rs',
     'if len > crate::storage::TRANSCRIPT_MAX_BYTES {',
     'if false {',
     'runtime::session::factory::projection_cache_tests::oversized_checkpoint_is_missing',
     '#1011 oversized checkpoint is treated as missing'),
    # fix-r1 (DeepSeek P2-2): replay alone produces equal projections; only
    # the hydrated-row counter observes that a valid cache was consumed.
    ('hydrate-branch-removed',
     'src/pilotdeck-gateway/src/runtime/session/projection_cache.rs',
     'if let Some(checkpoint) = checkpoint.filter(|_| ordered) {',
     'if let Some(checkpoint) = checkpoint.filter(|_| false) {',
     'runtime::session::factory::projection_cache_tests::fresh_load',
     '#1011 valid cache is hydrated'),
    # fix-r1 (DeepSeek P2-1): rejecting the whole envelope because one row's
    # codec is unknown must not dehydrate the two supported rows.
    ('envelope-over-rejected',
     'src/pilotdeck-gateway/src/runtime/session/projection_cache.rs',
     '.is_some_and(|n| (1.0..=9_007_199_254_740_991.0).contains(&n) && n.fract() == 0.0)',
     '.is_some_and(|n| (1.0..=2.0).contains(&n) && n.fract() == 0.0)',
     'runtime::session::factory::projection_cache_tests::invalid_codec',
     '#1011 invalid codec row falls back alone'),

    ('guard-lib-wiring-omitted',
     'src/pilotdeck-gateway/tests/fixtures/session-factory/projection-cache/negative_controls.py',
     "    guard_inputs += [root / 'src/pilotdeck-gateway/src/lib.rs']\n",
     "    guard_inputs += []\n",
     'runtime::session::factory::projection_cache_tests::guard_includes_library_wiring',
     '#1011 guard includes library wiring'),
    ('generator-owner-wrong',
     'src/pilotdeck-gateway/tests/fixtures/session-factory/projection-cache/generate.mts',
     "'session-factory/projection-cache\\n', {flag: 'wx'}",
     "'#1011\\n', {flag: 'wx'}",
     'runtime::session::factory::projection_cache_tests::generator_cleans_owned_work',
     '#1011 generator owner identifies fixture'),
    # #1281: the work cleanup must also run when the oracle.json wx write
    # fails after the whole matrix. Moving the rmSync from the finally into the
    # tail of the try body models "cleanup only on the success exit" and keeps
    # the file parseable (an emptied finally clause, not a bare try — a bare
    # `try { }` is a SyntaxError, which would make the cell fail for the wrong
    # reason and the mutation INVALID rather than KILLED). The collision cell of
    # generator_cleans_owned_work must catch the leftover work-* directory.
    ('generator-cleanup-success-only',
     'src/pilotdeck-gateway/tests/fixtures/session-factory/projection-cache/generate.mts',
     '} finally {\n  fs.rmSync(work, {recursive: true, force: true});\n}',
     '  fs.rmSync(work, {recursive: true, force: true});\n} finally {\n}',
     'runtime::session::factory::projection_cache_tests::generator_cleans_owned_work',
     '#1011 generator left files behind in OUT'),

]


def mutate(content, scope, replacements):
    for old, new in replacements:
        if content.count(old) != 1 or old == new:
            raise RuntimeError('#1011 expired or ineffective mutation anchor')
        content = content.replace(old, new, 1)
    return content


def run(out, env):
    # #1281: PDRS_NEGATIVE_BASELINE lets an uncommitted sandbox candidate run
    # its controls against the exact working tree, mirroring the
    # turn-runner/spill-isolation runners; committed hosts keep HEAD.
    baseline = env.get('PDRS_NEGATIVE_BASELINE') or 'HEAD'
    compiled = []
    for name, filename, old, new, _, _ in mutations:
        target = root / filename
        try:
            with mutating_sources(root, [target], baseline=baseline) as originals:
                target.write_text(mutate(originals[target].decode(), None, [(old, new)]))
                slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=900)
                (out / f'{name}-compile-slots.log').write_text(slot.stdout + slot.stderr)
                result = run_owned(['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
                                    '-p', 'pilotdeck-gateway', '--lib', '--no-run'],
                                   cwd=root, env=env, timeout=900)
                (out / f'{name}-compile.log').write_text(result.stdout + result.stderr)
                compiled.append({'mutation': name, 'compiled': result.returncode == 0})
        except Exception as error:
            compiled.append({'mutation': name, 'compiled': False, 'reason': str(error)})
    (out / 'compilation.json').write_text(json.dumps(compiled, indent=2) + '\n')
    selected = [Case(name, root / filename, old, new, test, assertion)
                for name, filename, old, new, test, assertion in mutations]
    result = run_cases(root, selected, out, env, mutate, timeout=900, script=__file__,
                       baseline=env.get('PDRS_NEGATIVE_BASELINE') or 'HEAD')
    return result or int(any(not r['compiled'] for r in compiled))


def input_paths():
    fixture = Path(__file__).resolve().parent
    tests = root / 'src/pilotdeck-gateway/tests'
    # Fixed file/directory inventory, not a home-grown Rust source parser.
    # Include the oracle, generator and judge as well as the mutated test.
    guard_inputs = [fixture / name for name in ['generate.mts', 'oracle.json', 'README.md', 'negative_controls.py']]
    guard_inputs += list((root / 'src/pilotdeck-gateway/src/runtime/session').glob('*.rs'))
    guard_inputs += [p for p in (tests / 'common').rglob('*.rs')]
    guard_inputs += [p for p in (tests / 'fixtures/_shared').rglob('*') if p.is_file() and p.suffix != '.pyc']
    guard_inputs += [tests / 'baseline_source_guard.mjs', root / 'scripts/cargo-slots.sh']
    guard_inputs += [root / 'src/pilotdeck-gateway/src/lib.rs']
    guard_inputs += [p for p in (tests / 'fixtures').glob('negative_*.py')]
    guard_inputs += [tests / 'fixtures/pending_reanchor.py', tests / 'fixtures/pending-reanchor.txt']
    return guard_inputs


def main():
    if sys.argv[1:] == ['--inputs']:
        print(json.dumps([str(p.relative_to(root)) for p in input_paths()]))
        return 0
    for _, filename, old, new, _, _ in mutations:
        mutate((root / filename).read_text(), None, [(old, new)])
    if sys.argv[1:] == ['--audit']:
        # #2303 fix-r4 (DS concurrency#1 / MM sibling#1, zero denominator): an
        # empty table is never "every anchor bound" -- mutations emptied,
        # renamed away or skipped to zero rows must refuse (2, the
        # never-started exit) instead of printing a PASS over zero anchors.
        if not mutations:
            sys.stderr.write('INVALID: audit produced zero anchors (mutations '
                             'emptied, renamed away or skipped?); "every '
                             'anchor bound" cannot pass on an empty '
                             'denominator\n')
            sys.stderr.flush()
            return 2
        # #1210: bound test names may come from the matrix_tests! macro table,
        # so a plain `fn <name>` scan would not find them. Accept either the
        # direct function or the `name=>"case"` table entry; anything else
        # fails the audit instead of silently binding a nonexistent test.
        # #1281 review: the full module path picks the one file that declares
        # that module (factory.rs pins the #[path] names); a concatenation of
        # both files would accept a test that lives in the other module.
        # #1890: the rule lives in negative_suite.bind_module_file so the six
        # audit copies cannot drift (this one used to require `=>"`).
        modules = (('runtime::session::factory::tests::', 'factory_tests.rs'),
                   ('runtime::session::factory::projection_cache_tests::', 'projection_cache_tests.rs'))
        sources = {name: (root / 'src/pilotdeck-gateway/src/runtime/session' / name).read_text()
                   for _, name in modules}
        # #2303 (条 4 同类跟改): every row is audited before the refusal, and
        # a scanner refusal (ValueError) becomes one named record per row
        # instead of a raw traceback on the first failure (the session-
        # factory / session-drop-release audit shape).
        invalid = []
        for _name, _filename, _old, _new, test, _assertion in mutations:
            try:
                file, short = bind_module_file(test, modules)
                if not declares_test(sources[file], short):
                    raise ValueError(f'bound test not found for {short}: {test}')
            except Exception as error:
                invalid.append(f'{test}: {type(error).__name__}: {error}')
        if invalid:
            raise SystemExit('#1210 binding audit INVALID rows: ' + '; '.join(invalid))
        print('PASS: #1011 projection cache mutation anchors (static only)')
        return 0
    if len(sys.argv) != 2:
        raise SystemExit('usage: negative_controls.py OUT | --audit')
    if not os.environ.get('CARGO_TARGET_DIR') or not os.environ.get('PDRS_BASELINE'):
        raise SystemExit('#1011 set CARGO_TARGET_DIR and PDRS_BASELINE; no fallback target')
    out = Path(sys.argv[1]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    env = dict(os.environ, CARGO_PROFILE_DEV_DEBUG='line-tables-only', CARGO_PROFILE_TEST_DEBUG='line-tables-only')
    run_out = out / 'guarded-run'
    run_out.mkdir()
    try:
        with mutating_sources(root, input_paths(),
                              baseline=os.environ.get('PDRS_NEGATIVE_BASELINE') or 'HEAD'):
            result = run(run_out, env)
        # Only the top-level receipt is final; publish it after all inputs have
        # been restored and verified. A killed process leaves no final receipt.
        publish_summary(out, json.loads((run_out / 'summary.json').read_text())['controls'])
        (out / 'compilation.json').write_text((run_out / 'compilation.json').read_text())
        return result
    except BaseException as error:
        # An outer source/restore failure must revoke any inner KILLED receipt.
        publish_summary(out, [{'mutation': name, 'test': test, 'assertion': assertion,
                               'status': 'INVALID', 'reason': f'#1011 input guard: {error}'}
                              for name, _, _, _, test, assertion in mutations])
        (out / 'compilation.json').write_text(json.dumps([
            {'mutation': name, 'compiled': False, 'reason': f'#1011 input guard: {error}'}
            for name, *_ in mutations], indent=2) + '\n')
        return 2


if __name__ == '__main__':
    raise SystemExit(main())
