#!/usr/bin/env python3
# anchor-selfcheck: unsupported three-field name/scope/replacements rows, including ordered multiple replacements; not the recording adapter single-replacement consumer
"""Runtime mutations of the durable runner; always restore the owned tree."""
import json
from functools import partial
import os
from pathlib import Path
import sys
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot
from _negative_common import mutate, run, check_mutations

root = Path(__file__).resolve().parents[5]
source = root / 'src/pilotdeck-gateway/src/runtime/turn/runner.rs'
env = os.environ.copy()
command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
           '-p', 'pilotdeck-gateway', '--lib', 'runtime::turn::runner::tests']
mutations = [
    ('start-durable', 'start', [('self.io(|r, s, t| r.start_turn(s, t)).await?;', '')]),
    ('accepted-before-write', 'accept', [('.record_accepted_input(s, t, written, input.metadata)?', '.record_accepted_input(s, t, written, input.metadata).unwrap_or_default()')]),
    ('accepted-boundary', 'accept', [('JsValue::Array(accepted)', 'JsValue::Array(self.messages.clone())')]),
    ('synthetic-durable', 'accept', [('.chain(input.synthetic)', '.chain(std::iter::empty())')]),
    ('history-loop-only', 'accept', [('self.messages.extend(durable);', 'self.messages = durable;')]),
    ('status-required', 'loop_failed', [('if !self.failure_status {', 'if false {')]),
    ('status-dedup', 'loop_failed', [('if !self.failure_status {', 'if true {')]),
    ('complete-durable', 'complete', [('self.io(move |r, s, t| r.complete_turn(s, t, written))', 'async { let _ = written; Ok::<(), RecorderError>(()) }')]),
    ('catch-complete', 'fail_inner', [('self.io(move |r, s, t| r.complete_turn(s, t, written))', 'async { let _ = written; Ok::<(), RecorderError>(()) }')]),
    ('terminal-mark', 'complete', [('self.phase = Phase::Completed;', 'self.phase = Phase::Accepted;')]),
    ('catch-terminal-mark', 'fail_inner', [('self.phase = Phase::Completed;', 'self.phase = Phase::Accepted;')]),
    ('worker-lease', 'io', [('drop(lease);', ''), ('let result = write(&recorder, &session, &turn);', 'drop(lease); let result = write(&recorder, &session, &turn);')]),
    ('aborted-reason', 'fail_inner', [('error.get("code") == Some(&"agent_aborted".into())', 'false')]),
    ('nullish-hint', 'status', [('.filter(|v| !matches!(v, JsValue::Null | JsValue::Undefined))', '')]),
    ('visible-error-details', 'accept', [('"SessionEventValidationError".into()', '"Error".into()')]),
    ('phase-guard', 'require', [('if self.phase == phase {', 'if true {')]),
    ('catch-phase-guard', 'fail_inner', [('!matches!(self.phase, Phase::Started | Phase::Accepted)', 'false')]),
    ('identity-turn-guard', 'complete', [('|| result.get("turnId") != Some(&JsValue::String(self.turn.clone()))', '|| false')]),
    ('identity-guard', 'complete', [('if result.get("sessionId")', 'if false && result.get("sessionId")')]),
    ('invalid-result-early-return', 'complete', [('let message = match &result {', 'if true { return Err(RecorderError::State("mutant early return".into())); } let message = match &result {')]),
    ('invalid-result-error-shape', 'complete', [('("message", JsValue::String(message)),', '("message", { let _ = message; "wrong error".into() }),')]),
    ('status-undefined-detail', 'status', [('].into_iter().filter(|(_, value)| !matches!(value, JsValue::Undefined)))', '].into_iter())')]),
    ('status-null-text', 'status', [('.filter(|(_, field)| !matches!(field, JsValue::Undefined))', '.filter(|(_, field)| !matches!(field, JsValue::Undefined | JsValue::Null))')]),
    ('status-undefined-text', 'status', [('.filter(|(_, field)| !matches!(field, JsValue::Undefined))', '.filter(|_| true)')]),
    ('catch-status-must-repeat', 'fail_inner', [('let status = self.status(&error).await;', 'let status = if self.failure_status { self.event("skipped_status", []) } else { self.status(&error).await };')]),
    ('undefined-result-message', 'complete', [("Cannot read properties of undefined (reading 'turnId')", 'wrong undefined result')]),
    ('nonstring-turn-message', 'complete', [('Some(turn)', 'Some(turn @ JsValue::String(_))')]),
    ('undefined-turn-dedup', 'complete', [('!matches!(turn, JsValue::Undefined)', 'true')]),
    ('turn-surrogate-lossy', 'complete', [('.chain(units)', '.chain(String::from_utf16_lossy(&units).encode_utf16())')]),
    ('turn-coercion-error', 'complete', [('Err(error) => error.message().into(),', 'Err(_) => "wrong coercion error".into(),')]),
    ('best-effort-status', 'status', [('.await;', '.await.expect("mutant status failure");')]),
]

if sys.argv[1:] == ['--check']:
    raise SystemExit(check_mutations((n, source, s, r) for n, s, r in mutations))
if not env.get('CARGO_TARGET_DIR'):
    raise SystemExit('CARGO_TARGET_DIR must be supplied by the caller')
out = Path(sys.argv[1]).resolve()
out.mkdir(parents=True, exist_ok=False)
if sys.argv[2:]:
    selected = set(sys.argv[2:])
    assert selected <= {row[0] for row in mutations}, 'unknown mutation'
    mutations = [row for row in mutations if row[0] in selected]
run = partial(run, root=root, out=out, command=command, env=env)
with mutating_sources(root, [source], baseline=os.environ.get('PDRS_NEGATIVE_BASELINE', 'HEAD')) as originals:
    original = originals[source].decode()
    assert run('positive').returncode == 0
    for name, scope, replacements in mutations:
        source.write_text(mutate(original, scope, replacements))
        result = run(name)
        assert result.returncode != 0 and 'test result: FAILED' in result.stdout, name
        assert 'panicked at' in result.stdout + result.stderr, name
        source.write_bytes(originals[source])
        print(f'{name}: runtime RED; source restored', flush=True)
    assert run('restored').returncode == 0
(out / 'summary.json').write_text(json.dumps({'mutations':len(mutations),'runtimeFailures':len(mutations),'restoredPass':True},indent=2)+'\n')
