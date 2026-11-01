#!/usr/bin/env python3
"""#1011 projection cache controls; run only after the sources are committed."""
import json
import os
from pathlib import Path
import sys

root = Path(__file__).resolve().parents[6]
sys.path.insert(0, str(Path(__file__).resolve().parents[2]))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import Case, run_cases, publish_summary

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
     'if let Some(checkpoint) = checkpoint {',
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

]


def mutate(content, scope, replacements):
    for old, new in replacements:
        if content.count(old) != 1 or old == new:
            raise RuntimeError('#1011 expired or ineffective mutation anchor')
        content = content.replace(old, new, 1)
    return content


def run(out, env):
    compiled = []
    for name, filename, old, new, _, _ in mutations:
        target = root / filename
        try:
            with mutating_sources(root, [target]) as originals:
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
    result = run_cases(root, selected, out, env, mutate, timeout=900, script=__file__)
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
        # #1210: bound test names may come from the matrix_tests! macro table,
        # so a plain `fn <name>` scan would not find them. Accept either the
        # direct function or the `name=>"case"` table entry; anything else
        # fails the audit instead of silently binding a nonexistent test.
        # Guards bound at the factory::tests level (regenerates_identically)
        # live in factory_tests.rs, so both files are scanned.
        test_source = '\n'.join(
            (root / 'src/pilotdeck-gateway/src/runtime/session' / name).read_text()
            for name in ('projection_cache_tests.rs', 'factory_tests.rs'))
        compact = ''.join(test_source.split())
        for _name, _filename, _old, _new, test, _assertion in mutations:
            short = test.rsplit('::', 1)[-1]
            if 'fn' + short + '(' not in compact and short + '=>"' not in test_source:
                raise SystemExit(f'#1210 bound test not found for {short}: {test}')
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
        with mutating_sources(root, input_paths()):
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
