#!/usr/bin/env python3
"""Live input/Recorder controls; each runtime mutant names its test and assertion."""
import os, sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot
from _negative_common import mutate, check_mutations
from negative_suite import Case, run_cases
root = Path(__file__).resolve().parents[5]
live = root / 'src/pilotdeck-gateway/src/runtime/turn/live.rs'
recording = live.with_name('runner_recording.rs')
factory = live.with_name('host_factory.rs')
oracle = 'live_input_matches_ts_original_history_synthetic_and_metadata'
mutations = [
    ('host-factory-recorder', factory, 'into_live_session', 'factory.create(&identity, recorder).await', 'factory.create(&identity, Arc::new(Recorder::from_writer(recorder.writer().clone()))).await', 'live_session_shares_recorder_and_reads_current_history', '#741k ports factory must receive the session Recorder'),
    ('new-recorder', live, None, 'host_factory(recorder)', 'host_factory(Arc::new(Recorder::from_writer(recorder.writer().clone())))', 'live_input_new_factory_shares_recorder', '#741k factory must receive the session Recorder'),
    ('interaction-recorder', live, 'with_interaction', 'factory(identity.clone(), recorder.clone())', 'factory(identity.clone(), Arc::new(Recorder::from_writer(recorder.writer().clone())))', 'live_input_interaction_factory_shares_recorder', '#741k factory must receive the session Recorder'),
    ('history', live, 'submit', '(self.history)()', 'vec![]', 'live_input_reads_current_history_on_every_submit', '#741k fresh history per submit'),
    ('synthetic', live, None, 'Some(JsValue::Array(messages)) => messages.clone(),', 'Some(JsValue::Array(_)) => vec![],', oracle, '#741k TS loop input'),
    ('nullish', live, None, 'Some(JsValue::Null | JsValue::Undefined)', 'Some(JsValue::Undefined)', 'live_input_nullish_synthetic_is_empty', '#741k nullish synthetic must parse'),
    ('synthetic-shape', live, None, '_ => return Err(RuntimeError::new("Expected syntheticMessages array.")),', '_ => vec![],', 'execute_request_rejects_flattened_and_invalid_input', '#741k reject synthetic shape'),
    ('synthetic-duplicate', live, None, '!key.equals_str("syntheticMessages")', 'true', oracle, '#741k synthetic must not be applied twice'),
    ('expanded-messages', live, None, 'payload.insert("messages".into(), JsValue::Array(input.loop_messages()));', 'if !payload.contains_key("messages") { payload.insert("messages".into(), JsValue::Array(input.loop_messages())); }', 'execute_request_preserves_host_options_and_original_input', '#741k raw input wins'),
    ('abort-messages', live, 'aborted_result', 'self.input.loop_messages()', 'self.input.accepted_messages()', 'live_input_reads_current_history_on_every_submit', '#741k abort preserves history and synthetic'),
    ('accepted-text', recording, 'accepted_messages', 'JsValue::String(text.clone())', 'JsValue::String(String::from_utf16_lossy(text.units()).into())', oracle, '#741k TS loop input'),
    ('accepted-blocks', recording, 'accepted_messages', 'content.clone()', 'vec![]', oracle, '#741k TS loop input'),
    ('metadata', recording, 'from_agent_input', '(!metadata.is_empty()).then_some(metadata)', 'None', oracle, '#741k TS metadata'),
]
if sys.argv[1:] == ['--check']:
    raise SystemExit(check_mutations((n, p, s, [(o, v)]) for n, p, s, o, v, _, _ in mutations))
env = os.environ.copy()
if not env.get('CARGO_TARGET_DIR'):
    raise SystemExit('CARGO_TARGET_DIR must be supplied by the caller')
names = set(sys.argv[2:])
if names - {m[0] for m in mutations}:
    raise SystemExit('unknown mutation')
chosen = [m for m in mutations if not names or m[0] in names]
out = Path(sys.argv[1]).resolve()
out.mkdir(parents=True, exist_ok=False)
stream = 'runtime::turn::live::stream_tests::'
host = 'runtime::turn::host_factory::tests::'
cases = [Case(n, p, o, v, (host if n == 'host-factory-recorder' else stream) + t, a, s)
         for n, p, s, o, v, t, a in chosen]
raise SystemExit(run_cases(root, cases, out, env, mutate,
                           baseline=env.get('PDRS_NEGATIVE_BASELINE', 'HEAD')))
