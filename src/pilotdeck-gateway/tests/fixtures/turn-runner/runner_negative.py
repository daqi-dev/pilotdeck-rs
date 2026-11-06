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
from negative_selection import claim_dir, refuse, registered, select, verdict_exit
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
    # #996-pr2 fix-r5: the gates-skip-complete/-failure-complete switches split
    # the old one-line self.io closure; anchor the durable write call itself.
    # Dropping its arguments and returning Ok skips the same durable write.
    ('complete-durable', 'complete', [('r.complete_turn(s, t, written)', '{ let _ = (r, s, t, written); Ok::<(), RecorderError>(()) }')]),
    ('catch-complete', 'fail_inner', [('r.complete_turn(s, t, written)', '{ let _ = (r, s, t, written); Ok::<(), RecorderError>(()) }')]),
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

# #1666: the denominator is this literal roster, never `len(mutations)`. A
# dropped or renamed row is a startup refusal instead of a shorter green run,
# and `select()` refuses an unknown name instead of silently matching nothing.
EXPECTED_NAMES = (
    'start-durable',
    'accepted-before-write',
    'accepted-boundary',
    'synthetic-durable',
    'history-loop-only',
    'status-required',
    'status-dedup',
    'complete-durable',
    'catch-complete',
    'terminal-mark',
    'catch-terminal-mark',
    'worker-lease',
    'aborted-reason',
    'nullish-hint',
    'visible-error-details',
    'phase-guard',
    'catch-phase-guard',
    'identity-turn-guard',
    'identity-guard',
    'invalid-result-early-return',
    'invalid-result-error-shape',
    'status-undefined-detail',
    'status-null-text',
    'status-undefined-text',
    'catch-status-must-repeat',
    'undefined-result-message',
    'nonstring-turn-message',
    'undefined-turn-dedup',
    'turn-surrogate-lossy',
    'turn-coercion-error',
    'best-effort-status',
)

if sys.argv[1:] == ['--check']:
    raise SystemExit(check_mutations((n, source, s, r) for n, s, r in mutations))
if not env.get('CARGO_TARGET_DIR'):
    refuse('CARGO_TARGET_DIR must be supplied by the caller')
# Every startup guard runs before the evidence directory exists, so a
# `refuse()` ("never started", exit 2) really leaves nothing behind.
try:
    registered(mutations, EXPECTED_NAMES)
    run_rows = select(mutations, sys.argv[2:], always=())
except ValueError as error:
    refuse(str(error))
out = claim_dir(sys.argv[1])
run = partial(run, root=root, out=out, command=command, env=env)
verdicts = []
with mutating_sources(root, [source], baseline=os.environ.get('PDRS_NEGATIVE_BASELINE', 'HEAD')) as originals:
    original = originals[source].decode()
    if run('positive').returncode != 0:
        raise RuntimeError('unmutated baseline did not pass')
    for name, scope, replacements in run_rows:
        source.write_text(mutate(original, scope, replacements))
        result = run(name)
        if result.returncode == 0 or 'test result: FAILED' not in result.stdout:
            raise RuntimeError(f'{name}: bound runner test did not fail')
        if 'panicked at' not in result.stdout + result.stderr:
            raise RuntimeError(f'{name}: failure is not a test assertion panic')
        source.write_bytes(originals[source])
        verdicts.append({'verdict': 'KILLED'})
        print(f'{name}: runtime RED; source restored', flush=True)
    if run('restored').returncode != 0:
        raise RuntimeError('restored baseline did not pass')
(out / 'summary.json').write_text(json.dumps({'mutations':len(run_rows),'runtimeFailures':len(run_rows),'restoredPass':True},indent=2)+'\n')
# Zero rows can never read as success: a run that proves nothing exits 1.
sys.exit(verdict_exit(verdicts))

