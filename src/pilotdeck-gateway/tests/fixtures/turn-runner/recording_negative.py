#!/usr/bin/env python3
"""Recording input/ownership runtime controls; use the shared source guard."""
import json
from functools import partial
import os
from pathlib import Path
import sys
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot
from _negative_common import mutate, run

root = Path(__file__).resolve().parents[5]
out = Path(sys.argv[1]).resolve()
out.mkdir(parents=True, exist_ok=False)
source = root / 'src/pilotdeck-gateway/src/runtime/turn/runner_recording.rs'
runner = source.with_name('runner.rs')
env = os.environ.copy()
if not env.get('CARGO_TARGET_DIR'):
    raise SystemExit('CARGO_TARGET_DIR must be supplied by the caller')
command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
           '-p', 'pilotdeck-gateway', '--lib', 'runtime::turn::runner::tests']
mutations = [
    ('pending-owned', source, '.as_mut()', '.take()'),
    ('pending-cleared', source, 'self.pending = None;', ''),
    ('runner-restored', source, 'self.runner = Some(runner);', 'drop(runner);'),
    ('events-returned', source, 'Ok(events)', 'Ok(vec![])'),
    ('pending-guard', source, 'if self.pending.is_some()', 'if false'),
    ('closed-guard', source, 'self.runner.as_ref().is_none_or(TurnRunner::is_complete)', 'self.runner.is_none()'),
    ('poisoned-guard', source, 'self.runner.as_ref().is_none_or(TurnRunner::is_complete)', 'self.runner.as_ref().is_some_and(TurnRunner::is_complete)'),
    ('idle-settle-error', source, '.ok_or_else(|| RecorderError::State("No recording transition is pending.".into()))?', '.expect("mutant requires pending")'),
    ('owner-attached', source, 'runner.owner = Some(owner);', 'drop(owner);'),
    ('worker-owner', runner, 'let owner = self.owner.clone();', 'let owner: Option<Arc<dyn Send + Sync>> = None;'),
    ('text-value', source, '.cloned()', '.map(|_| "probe".into())'),
    ('text-shape', source, '.ok_or_else(invalid)?', '.unwrap_or_else(|| "mutant".into())'),
    ('input-kind', source, 'kind.equals_str("text")', '!kind.equals_str("blocks")'),
    ('blocks-value', source, 'AgentInput::Blocks(blocks.clone())', 'AgentInput::Blocks(vec![])'),
    ('blocks-shape', source, '_ => return Err(invalid()),', '_ => AgentInput::Blocks(vec![]),'),
    ('truthy-fields', source, '.filter(|v| super::super::submit::truthy(v))', '.filter(|_| true)'),
    ('allow-false-null', source, '.filter(|v| !matches!(v, JsValue::Undefined))', '.filter(|v| super::super::submit::truthy(v))'),
    ('allow-undefined', source, '.filter(|v| !matches!(v, JsValue::Undefined))', '.filter(|_| true)'),
    ('empty-metadata', source, '(!metadata.is_empty()).then_some(metadata)', 'Some(metadata)'),
    ('model-own-only', source, 'JsValue::Object(own_properties(value))', 'value.clone()'),
    ('model-string-units', source, 'vec![*unit]', 'vec![0xfffd]'),
    ('model-array-index', source, 'out.insert(index.to_string().into(), value.clone());', 'out.insert("wrong".into(), value.clone());'),
]
for field in ['modelSelection', 'permissionMode', 'runMode', 'basePermissionMode']:
    mutations.append((f'metadata-{field}', source, f'"{field}",', '"missingField",'))
for name, old, new in [
    ('start', 'runner.start().await', 'runner.fail(JsValue::Null).await'),
    ('accept', 'runner.accept(input).await', 'runner.start().await'),
    ('loop-failed', 'runner.loop_failed(error).await', 'runner.fail(error).await'),
    ('complete', 'runner.complete(result).await', 'runner.loop_failed(result).await'),
    ('fail', 'runner.fail(error).await', 'runner.loop_failed(error).await'),
]:
    mutations.append((f'action-{name}', source, old, new))
if sys.argv[2:]:
    selected = set(sys.argv[2:])
    assert selected <= {m[0] for m in mutations}
    mutations = [m for m in mutations if m[0] in selected]
run = partial(run, root=root, out=out, command=command, env=env)
with mutating_sources(root, [source, runner], baseline=os.environ.get('PDRS_NEGATIVE_BASELINE', 'HEAD')) as originals:
    assert run('positive').returncode == 0
    for name, path, old, new in mutations:
        original = originals[path].decode()
        path.write_text(mutate(original, None, [(old, new)], expected_count=2 if name == 'blocks-shape' else 1))
        result = run(name)
        assert result.returncode != 0 and 'test result: FAILED' in result.stdout, name
        assert 'panicked at' in result.stdout + result.stderr, name
        path.write_bytes(originals[path])
        print(f'{name}: runtime RED; source restored', flush=True)
    assert run('restored').returncode == 0
(out / 'summary.json').write_text(json.dumps({'mutations':len(mutations),'runtimeFailures':len(mutations),'restoredPass':True},indent=2)+'\n')
