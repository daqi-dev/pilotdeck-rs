#!/usr/bin/env python3
"""#1265 HEAD-only mutations; every row compiles before runtime classification.

Usage: negative_controls.py --check
       negative_controls.py [--compile-only] NEW_OUTPUT_DIR [mutation ...]
CARGO_TARGET_DIR must be supplied. No repository copies or alternate git index.
"""
from pathlib import Path
import argparse
import json
import os
import sys
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import Case, classify, passed, publish_summary

root = Path(__file__).resolve().parents[5]
source = root / 'src/pilotdeck-regexp-proto/src/lib.rs'
tests = root / 'src/pilotdeck-regexp-proto/src/tests.rs'
fixture = root / 'src/pilotdeck-gateway/tests/fixtures/rquickjs-regexp/oracle.json'
generator = root / 'src/pilotdeck-gateway/tests/fixtures/rquickjs-regexp/generate.mjs'
mutations = [
    ('disable-interrupt', source, 'handler.interrupt()', 'false && handler.interrupt()',
     'tests::budget_tristate_and_reuse', '#1265 native budget exhausted'),
    ('exhaustion-is-success', source, 'return Err(Error::BudgetExhausted);', 'return Ok(());',
     'tests::budget_tristate_and_reuse', '#1265 native budget exhausted'),
    ('reset-every-match', source, 'let limits = self.engine.limits;',
     'self.budget.polls.store(0, Ordering::SeqCst);\n        let limits = self.engine.limits;',
     'tests::shared_budget_and_new_request', '#1265 shared second call exhausts'),
    ('ignore-cancellation', source, 'if self.cancellation.0.load(Ordering::SeqCst) {',
     'if false && self.cancellation.0.load(Ordering::SeqCst) {',
     'tests::cancellation_and_stale_token', '#1265 cancelled before call'),
    ('share-old-cancellation', source, 'cancellation: Cancellation::default(),',
     'cancellation: { static TOKEN: std::sync::OnceLock<Cancellation> = std::sync::OnceLock::new(); TOKEN.get_or_init(Cancellation::default).clone() },',
     'tests::cancellation_and_stale_token', '#1265 stale token isolation'),
    ('lossy-u16', source, 'args.push_args(chunk.iter().copied())?;',
     'args.push_args(chunk.iter().copied().map(|u| if (0xd800..=0xdfff).contains(&u) { 0xfffd } else { u }))?;',
     'tests::arbitrary_u16_roundtrip', '#1265 all u16 preserved'),
    ('ignore-last-index', source, '.set("lastIndex", last_index)', '.set("lastIndex", last_index.min(0))',
     'tests::node_semantics', '#1265 semantics'),
    ('drop-capture-unit', source, 'for index in 0..len {', 'for index in 0..len.saturating_sub(1) {',
     'tests::node_semantics', '#1265 semantics'),
    ('ignore-pattern-limit', source, 'if pattern.len() > limits.pattern_units {',
     'if false && pattern.len() > limits.pattern_units {',
     'tests::input_and_output_limits', '#1265 pattern limit'),
    ('ignore-input-limit', source, 'if input.len() > limits.input_units {',
     'if false && input.len() > limits.input_units {',
     'tests::input_and_output_limits', '#1265 input limit'),
    ('ignore-output-limit', source, 'remaining.checked_sub(len).ok_or(Error::Limit("output"))?',
     'remaining.saturating_sub(len)',
     'tests::input_and_output_limits', '#1265 output multiplicity bounded'),
    ('ignore-flags-limit', source, 'if flags.len() > 8 {', 'if false && flags.len() > 8 {',
     'tests::flags_limit', '#1265 flags length limit'),
    ('disable-memory-limit', source, 'runtime.set_memory_limit(limits.memory_bytes);',
     'runtime.set_memory_limit(0);',
     'tests::compilation_resources', '#1265 configured native memory limit'),
    ('disable-stack-limit', source, 'runtime.set_max_stack_size(limits.stack_bytes);',
     'runtime.set_max_stack_size(16 * 1024 * 1024);',
     'tests::configured_stack_limit', '#1265 configured stack rejects before exhausting host'),
    ('accept-unlimited-memory', source, 'limits.memory_bytes < 1024 * 1024',
     'limits.memory_bytes == usize::MAX',
     'tests::invalid_limits_and_zero_budget', '#1265 reject unlimited configuration'),
    ('enable-eval', source, 'Context::custom::<(intrinsic::RegExpCompiler, intrinsic::RegExp)>',
     'Context::custom::<intrinsic::All>',
     'tests::closed_context', ['#1265 unavailable global', '#1265 eval disabled']),
    ('corrupt-node-provenance', fixture, '"node": "v22.22.0"', '"node": "v22.22.1"',
     'tests::node_semantics', '#1265 Node provenance'),
    ('generator-version-drift', generator, "process.version, 'v22.22.0'", "process.version, 'v22.22.1'",
     'tests::fixture_regenerates_identically', '#1265 regeneration identical'),
]

def check(row, original):
    name, _, old, new, _, _ = row
    if original.count(old) != 1 or old == new:
        raise ValueError(f'#1265 {name}: stale or non-unique anchor; refusing mutation')

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--check', action='store_true')
    parser.add_argument('--compile-only', action='store_true')
    parser.add_argument('output', nargs='?')
    parser.add_argument('selected', nargs='*')
    args = parser.parse_args()
    if args.check:
        errors = []
        for row in mutations:
            try:
                check(row, row[1].read_text())
            except Exception as error:
                errors.append(str(error))
        print('\n'.join(errors) if errors else f'#1265 {len(mutations)} unique anchors checked')
        return int(bool(errors))
    if not args.output or not os.environ.get('CARGO_TARGET_DIR'):
        parser.error('NEW_OUTPUT_DIR and caller-supplied CARGO_TARGET_DIR are required')
    selected = [row for row in mutations if not args.selected or row[0] in args.selected]
    if not selected or (args.selected and len(selected) != len(args.selected)):
        parser.error('unknown or duplicate mutation')
    out = Path(args.output).resolve()
    out.mkdir(parents=True, exist_ok=False)
    env = dict(os.environ)
    command = ['cargo', 'test', '--locked', '--offline', '--manifest-path',
               str(root / 'src/pilotdeck-regexp-proto/Cargo.toml'), '--lib']
    paths = [source, tests, fixture, generator, Path(__file__).resolve()]
    results = []

    def run(label, test, compile_only=False):
        slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=600)
        (out / f'{label}.slots.log').write_text(slot.stdout + slot.stderr)
        argv = command + (['--no-run'] if compile_only else [test, '--', '--exact', '--include-ignored'])
        (out / f'{label}.command.json').write_text(json.dumps({'command':argv,'CARGO_TARGET_DIR':env['CARGO_TARGET_DIR']},indent=2)+'\n')
        result = run_owned(argv, cwd=root, env=env, timeout=900)
        (out / f'{label}.log').write_text(result.stdout + result.stderr)
        return result

    for row in selected:
        name, path, old, new, test, assertion = row
        case = Case(name, path, old, new, test, assertion)
        item = dict(mutation=name, test=test, status='INVALID', reason='not run', compiled=False, restoredPass=False)
        try:
            with mutating_sources(root, paths) as originals:
                original = originals[path].decode()
                check(row, original)
                if not passed(run(name+'-positive',test),test):
                    raise RuntimeError('positive bound test failed')
                try:
                    path.write_text(original.replace(old,new,1))
                    if run(name+'-compile',test,True).returncode:
                        raise RuntimeError('mutant did not compile; never a kill')
                    item['compiled'] = True
                    if args.compile_only:
                        item.update(status='COMPILED',reason='cargo test --no-run passed')
                    else:
                        status,reason=classify(run(name+'-negative',test),case)
                        item.update(status=status,reason=reason)
                finally:
                    # Restore inside the shared guard, which also verifies every
                    # original byte on exit. No private snapshot/restore protocol.
                    path.write_bytes(originals[path])
                item['restoredPass']=passed(run(name+'-restored',test),test)
                if not item['restoredPass']:
                    raise RuntimeError('restored bound test failed')
        except Exception as error:
            item.update(status='INVALID',reason=f'{type(error).__name__}: {error}')
        results.append(item)
        print(f'{name}: {item["status"]}: {item["reason"]}',flush=True)
    publish_summary(out,results)
    required='COMPILED' if args.compile_only else 'KILLED'
    return int(any(item['status']!=required for item in results))

if __name__ == '__main__':
    raise SystemExit(main())
