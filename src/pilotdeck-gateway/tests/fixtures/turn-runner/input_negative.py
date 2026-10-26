#!/usr/bin/env python3
"""Live input/Recorder controls; each runtime mutant names its test and assertion."""
import json, os, sys
from pathlib import Path
from functools import partial
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot
from _negative_common import mutate, run
root = Path(__file__).resolve().parents[5]
env = os.environ.copy()
if not env.get('CARGO_TARGET_DIR'):
    raise SystemExit('CARGO_TARGET_DIR must be supplied by the caller')
out = Path(sys.argv[1]).resolve()
out.mkdir(parents=True, exist_ok=False)
live = root / 'src/pilotdeck-gateway/src/runtime/turn/live.rs'
recording = live.with_name('runner_recording.rs')
factory = live.with_name('host_factory.rs')
oracle = 'live_input_matches_ts_original_history_synthetic_and_metadata'
mutations = [
    ('host-factory-recorder', factory, 'into_live_session', 'factory.create(&identity, recorder).await', 'factory.create(&identity, Arc::new(Recorder::from_writer(recorder.writer().clone()))).await', 'live_session_shares_recorder_and_reads_current_history', 'ports factory must receive the session Recorder'),
    ('new-recorder', live, None, 'host_factory(recorder)', 'host_factory(Arc::new(Recorder::from_writer(recorder.writer().clone())))', 'live_input_new_factory_shares_recorder', 'factory must receive the session Recorder'),
    ('interaction-recorder', live, 'with_interaction', 'factory(identity.clone(), recorder)', 'factory(identity.clone(), Arc::new(Recorder::from_writer(recorder.writer().clone())))', 'live_input_interaction_factory_shares_recorder', 'factory must receive the session Recorder'),
    ('history', live, 'submit', '(self.history)()', 'vec![]', 'live_input_reads_current_history_on_every_submit', 'fresh history per submit'),
    ('synthetic', live, None, 'Some(JsValue::Array(messages)) => messages.clone(),', 'Some(JsValue::Array(_)) => vec![],', oracle, 'TS loop input'),
    ('nullish', live, None, 'Some(JsValue::Null | JsValue::Undefined)', 'Some(JsValue::Undefined)', 'live_input_nullish_synthetic_is_empty', 'Expected syntheticMessages array'),
    ('synthetic-shape', live, None, '_ => return Err(RuntimeError::new("Expected syntheticMessages array.")),', '_ => vec![],', 'execute_request_rejects_flattened_and_invalid_input', 'reject synthetic shape'),
    ('synthetic-duplicate', live, None, '!key.equals_str("syntheticMessages")', 'true', oracle, 'synthetic must not be applied twice'),
    ('expanded-messages', live, None, 'payload.insert("messages".into(), JsValue::Array(input.loop_messages()));', 'if !payload.contains_key("messages") { payload.insert("messages".into(), JsValue::Array(input.loop_messages())); }', 'execute_request_preserves_host_options_and_original_input', 'raw input wins'),
    ('abort-messages', live, 'aborted_result', 'self.input.loop_messages()', 'self.input.accepted_messages()', 'live_input_reads_current_history_on_every_submit', 'abort preserves history and synthetic'),
    ('accepted-text', recording, 'accepted_messages', 'JsValue::String(text.clone())', 'JsValue::String(String::from_utf16_lossy(text.units()).into())', oracle, 'TS loop input'),
    ('accepted-blocks', recording, 'accepted_messages', 'content.clone()', 'vec![]', oracle, 'TS loop input'),
    ('metadata', recording, 'from_agent_input', '(!metadata.is_empty()).then_some(metadata)', 'None', oracle, 'TS metadata'),
]
command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml', '-p', 'pilotdeck-gateway', '--lib', '--', 'runtime::turn::live::stream_tests::', 'runtime::turn::host_factory::tests::live_session_shares_recorder_and_reads_current_history']
run = partial(run, root=root, out=out, command=command, env=env)
with mutating_sources(root, [live, recording, factory], baseline=env.get('PDRS_NEGATIVE_BASELINE', 'HEAD')) as originals:
    assert run('positive').returncode == 0
    for name, path, scope, old, new, test, anchor in mutations:
        path.write_text(mutate(originals[path].decode(), scope, [(old, new)]))
        result = run(name)
        assert result.returncode != 0 and 'test result: FAILED' in result.stdout, name
        assert f'::{test} ... FAILED' in result.stdout and anchor in result.stdout, (name, test, anchor)
        assert 'panicked at' in result.stdout + result.stderr, name
        path.write_bytes(originals[path])
        print(f'{name}: runtime RED ({test}: {anchor}); restored', flush=True)
    assert run('restored').returncode == 0
(out / 'summary.json').write_text(json.dumps({'mutations': [{'name': m[0], 'test': m[-2], 'assertion': m[-1]} for m in mutations], 'runtimeFailures': len(mutations), 'restoredPass': True}, indent=2)+'\n')
