#!/usr/bin/env python3
"""#1253 source mutations; execute on the committed delivery, never a shadow HEAD."""
import json
import os
from pathlib import Path
import sys

root = Path(__file__).resolve().parents[6]
sys.path.insert(0, str(Path(__file__).resolve().parents[2]))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import Case, run_cases, publish_summary

mutations = [
    ('completion-index-skipped', 'src/pilotdeck-gateway/src/runtime/session/factory.rs',
     'lock(&self.projection.completed_turns).insert(turn.clone());',
     'let _ = turn; // omit durable completion index',
     'delta_matches_ts_matrix', '#1253 delta equals TS at every event'),
    ('invalid-completion-index-skipped', 'src/pilotdeck-gateway/src/runtime/session/factory.rs',
     'lock(&self.projection.completed_turns).insert(turn.clone());',
     'if self.projection.valid.load(Ordering::SeqCst) { lock(&self.projection.completed_turns).insert(turn.clone()); }',
     'completion_during_invalid_projection_survives_recovery', '#1253 completion index survives invalid projection'),
    ('ordinary-full-replay', 'src/pilotdeck-gateway/src/runtime/session/factory.rs',
     'self.projection.valid.load(Ordering::SeqCst) && !requires_replay(entry);',
     'self.projection.valid.load(Ordering::SeqCst) && !requires_replay(entry) && false;',
     'ordinary_append_never_replays_history', '#1253 ordinary append must not replay historical entries'),
    ('input-messages-dropped', 'src/pilotdeck-gateway/src/runtime/session/factory.rs',
     'state.messages.extend(messages);', 'state.messages.extend(messages.into_iter().take(0));',
     'delta_matches_ts_matrix', '#1253 delta equals TS at every event'),
    ('model-selection-dropped', 'src/pilotdeck-gateway/src/runtime/session/factory.rs',
     'state\n                .metadata\n                .insert("modelSelection".into(), selection.clone());',
     'state\n                .metadata\n                .insert("wrongSelection".into(), selection.clone());',
     'delta_matches_ts_matrix', '#1253 delta equals TS at every event'),
    ('incomplete-messages-visible', 'src/pilotdeck-gateway/src/runtime/session/factory.rs',
     '.any(|name| is(kind, name))\n        && done',
     '.any(|name| is(kind, name))\n        && !done',
     'delta_matches_ts_matrix', '#1253 delta equals TS at every event'),
    ('compact-replacement-visible', 'src/pilotdeck-gateway/src/runtime/session/factory.rs',
     '            state.messages.push(clone_message(message)?);',
     '            state.messages.push(clone_message(message)?);\n        } else {\n            state.messages.push(clone_message(message)?);',
     'delta_matches_ts_matrix', '#1253 delta equals TS at every event'),
    ('context-completion-inverted', 'src/pilotdeck-gateway/src/runtime/session/factory.rs',
     '} else if is(kind, "context_snapshot") && done {',
     '} else if is(kind, "context_snapshot") && !done {',
     'delta_matches_ts_matrix', '#1253 delta equals TS at every event'),
    ('metadata-delta-dropped', 'src/pilotdeck-gateway/src/runtime/session/factory.rs',
     'merge_metadata(&mut state.metadata, &spread(metadata));',
     'merge_metadata(&mut state.metadata, &JsObject::new());',
     'delta_matches_ts_matrix', '#1253 delta equals TS at every event'),
    ('completion-replay-removed', 'src/pilotdeck-gateway/src/runtime/session/factory.rs',
     'is(kind, "turn_result")\n        || (is(kind, "control_boundary")',
     'false\n        || (is(kind, "control_boundary")',
     'delta_matches_ts_matrix', '#1253 delta equals TS at every event'),
    ('compact-replay-removed', 'src/pilotdeck-gateway/src/runtime/session/factory.rs',
     '&& is(field(boundary, "subtype"), "compact_boundary"))',
     '&& is(field(boundary, "subtype"), "compact_boundary") && false)',
     'delta_matches_ts_matrix', '#1253 delta equals TS at every event'),
    ('invalid-state-erased', 'src/pilotdeck-gateway/src/runtime/session/factory.rs',
     'self.projection.valid.load(Ordering::SeqCst) && !requires_replay(entry);',
     '!requires_replay(entry);',
     'invalid_delta_keeps_last_valid_and_compaction_recovers', '#1253 later delta cannot erase historical error'),
    ('restore-completed-index-empty', 'src/pilotdeck-gateway/src/runtime/session/projection_cache.rs',
     '*lock(&self.projection.completed_turns) = completed_turns(&entries);',
     '*lock(&self.projection.completed_turns) = HashSet::new();',
     'restored_completion_index_matches_ts', '#1253 restore rebuilds completed turns'),
    ('reload-single-attempt', 'src/pilotdeck-gateway/src/runtime/session/factory.rs',
     'for _ in 0..8 {', 'for _ in 0..1 {',
     'reload_retry_budget_and_concurrent_writer', '#1253 stable reload succeeds'),
    ('restore-completed-suffix-omitted', 'src/pilotdeck-gateway/src/runtime/session/projection_cache.rs',
     '*lock(&self.projection.completed_turns) = completed_turns(&entries);',
     '*lock(&self.projection.completed_turns) = completed_turns(&entries[..entries.len().saturating_sub(1)]);',
     'restore_live_suffix_rebuilds_completion_index', '#1253 completed index includes merged live suffix'),
    ('reload-accept-torn-state', 'src/pilotdeck-gateway/src/runtime/session/factory.rs',
     'if before.sequence == after.sequence && before.last_entry_id == after.last_entry_id {',
     'if before.sequence <= after.sequence || before.last_entry_id == after.last_entry_id {',
     'reload_retry_budget_and_concurrent_writer', '#1253 reload writer version agrees'),
    ('oracle-regeneration-drift', 'src/pilotdeck-gateway/src/runtime/session/projection_delta_tests.rs',
     'crate::test_baseline::regenerates_identically("session-factory/append-delta");',
     'crate::test_baseline::regenerates_identically_with("session-factory/append-delta", true);',
     'append_delta_regenerates_identically', '#280 session-factory/append-delta/oracle.json differs'),
    ('tail-work-left-behind', 'src/pilotdeck-gateway/tests/fixtures/turn-runner-tail/generate.mts',
     'fs.rmSync(runDir,{recursive:true,force:true});', 'void runDir; // keep owned scratch',
     'tail_generator_cleans_only_owned_work', '#1253 tail generator removes only its owned work root'),
    ('tail-foreign-work-deleted', 'src/pilotdeck-gateway/tests/fixtures/turn-runner-tail/generate.mts',
     'fs.rmSync(runDir,{recursive:true,force:true});',
     "for(const name of fs.readdirSync(ROOT))if(name.startsWith('run-'))fs.rmSync(path.join(ROOT,name),{recursive:true,force:true});",
     'tail_generator_cleans_only_owned_work', '#1253 tail generator removes only its owned work root'),
    ('tail-collector-outside-owned-root', 'src/pilotdeck-gateway/tests/fixtures/turn-runner-tail/generate.mts',
     "const workspace=path.join(runDir,'collector',c.name);", "const workspace=path.join(ROOT,'collector',c.name);",
     'tail_generator_cleans_only_owned_work', '#1253 foreign collector contains only its original file'),
]


def mutate(content, scope, replacements):
    for old, new in replacements:
        if content.count(old) != 1 or old == new:
            raise RuntimeError('#1253 expired or ineffective mutation anchor')
        content = content.replace(old, new, 1)
    return content


def input_paths():
    gateway = root / 'src/pilotdeck-gateway'
    inputs = [root / 'src/Cargo.toml', root / 'src/Cargo.lock', gateway / 'Cargo.toml']
    inputs += list((gateway / 'src').rglob('*.rs'))
    inputs += list((root / 'src/pilotdeck-protocol/src').rglob('*.rs'))
    inputs += list((gateway / 'tests/common').rglob('*.rs'))
    inputs += [p for p in (gateway / 'tests/fixtures/_shared').rglob('*') if p.is_file() and p.suffix != '.pyc']
    inputs += list((gateway / 'tests/fixtures').glob('negative_*.py'))
    inputs += [gateway / 'tests/fixtures/pending_reanchor.py', gateway / 'tests/fixtures/pending-reanchor.txt',
               gateway / 'tests/baseline_source_guard.mjs', root / 'scripts/cargo-slots.sh']
    inputs += [p for p in Path(__file__).parent.iterdir() if p.is_file()]
    inputs += [gateway / 'tests/fixtures/turn-runner-tail/generate.mts', gateway / 'tests/fixtures/turn-runner-tail/oracle.json']
    return inputs


def run(out, env):
    compilation = []
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
                compilation.append({'mutation': name, 'compiled': result.returncode == 0})
        except Exception as error:
            compilation.append({'mutation': name, 'compiled': False, 'reason': str(error)})
    prefix = 'runtime::session::factory::projection_delta_tests::'
    selected = [Case(name, root / filename, old, new, prefix + test, assertion)
                for name, filename, old, new, test, assertion in mutations]
    run_cases(root, selected, out, env, mutate, timeout=900, script=__file__)
    results = json.loads((out / 'summary.json').read_text())['controls']
    for control, compile_result in zip(results, compilation):
        control['compiled'] = compile_result['compiled']
        if not compile_result['compiled']:
            control.update(status='INVALID', reason='mutant compilation failed or refused: ' + compile_result.get('reason', 'see compile log'))
    return results, compilation


def main():
    if sys.argv[1:] == ['--inputs']:
        print(json.dumps([str(p.relative_to(root)) for p in input_paths()]))
        return 0
    for _, filename, old, new, _, _ in mutations:
        mutate((root / filename).read_text(), None, [(old, new)])
    if sys.argv[1:] == ['--audit']:
        print(f'PASS: #1253 {len(mutations)} unique effective mutation anchors (static only)')
        return 0
    if len(sys.argv) != 2:
        raise SystemExit('usage: negative_controls.py OUT | --audit | --inputs')
    if not os.environ.get('CARGO_TARGET_DIR') or not os.environ.get('PDRS_BASELINE'):
        raise SystemExit('#1253 caller must set CARGO_TARGET_DIR and PDRS_BASELINE')
    out = Path(sys.argv[1]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    env = dict(os.environ, CARGO_PROFILE_DEV_DEBUG='line-tables-only', CARGO_PROFILE_TEST_DEBUG='line-tables-only')
    guarded = out / 'guarded-run'
    guarded.mkdir()
    try:
        with mutating_sources(root, input_paths()):
            results, compilation = run(guarded, env)
    except Exception as error:
        results = [{'mutation': name, 'test': 'runtime::session::factory::projection_delta_tests::' + test,
                     'assertion': assertion, 'compiled': False, 'status': 'INVALID',
                     'reason': f'#1253 input/restore guard refused: {error}'}
                    for name, _, _, _, test, assertion in mutations]
        compilation = [{'mutation': name, 'compiled': False, 'reason': str(error)}
                       for name, *_ in mutations]
    publish_summary(out, results)
    (out / 'compilation.json').write_text(json.dumps(compilation, indent=2) + '\n')
    return int(any(row['status'] != 'KILLED' for row in results))


if __name__ == '__main__':
    raise SystemExit(main())
