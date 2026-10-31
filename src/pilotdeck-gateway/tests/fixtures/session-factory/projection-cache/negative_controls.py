#!/usr/bin/env python3
"""#1011 regeneration guard control; run only after the sources are committed."""
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


def main():
    for _, filename, old, new, _, _ in mutations:
        mutate((root / filename).read_text(), None, [(old, new)])
    if sys.argv[1:] == ['--audit']:
        print('PASS: #1011 regeneration mutation anchor (static only)')
        return 0
    if len(sys.argv) != 2:
        raise SystemExit('usage: negative_controls.py OUT | --audit')
    if not os.environ.get('CARGO_TARGET_DIR') or not os.environ.get('PDRS_BASELINE'):
        raise SystemExit('#1011 set CARGO_TARGET_DIR and PDRS_BASELINE; no fallback target')
    out = Path(sys.argv[1]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    env = dict(os.environ, CARGO_PROFILE_DEV_DEBUG='line-tables-only', CARGO_PROFILE_TEST_DEBUG='line-tables-only')
    fixture = Path(__file__).resolve().parent
    tests = root / 'src/pilotdeck-gateway/tests'
    # Fixed file/directory inventory, not a home-grown Rust source parser.
    # Include the oracle, generator and judge as well as the mutated test.
    guard_inputs = [fixture / name for name in ['generate.mts', 'oracle.json', 'README.md', 'negative_controls.py']]
    guard_inputs += [p for p in (tests / 'common').rglob('*.rs')]
    guard_inputs += [p for p in (tests / 'fixtures/_shared').rglob('*') if p.is_file() and p.suffix != '.pyc']
    guard_inputs += [tests / 'baseline_source_guard.mjs', root / 'scripts/cargo-slots.sh']
    guard_inputs += [p for p in (tests / 'fixtures').glob('negative_*.py')]
    guard_inputs += [tests / 'fixtures/pending_reanchor.py', tests / 'fixtures/pending-reanchor.txt']
    run_out = out / 'guarded-run'
    run_out.mkdir()
    try:
        with mutating_sources(root, guard_inputs):
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
