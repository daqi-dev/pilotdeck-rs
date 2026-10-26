#!/usr/bin/env python3
"""Mutate actual live owners; every RED is bound to its test and panic anchor."""
import json, os, sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_suite import Case, run_cases
root = Path(__file__).resolve().parents[5]
live = root / 'src/pilotdeck-gateway/src/runtime/turn/live.rs'
runner = live.with_name('runner.rs'); driver = live.with_name('runner_recording.rs'); interaction = live.with_name('interaction.rs')
controls = [
 ('disposed-admission', live, 'if state.disposed {', 'if false {', 'live_recording_start_dispose', 'disposed session stays closed'),
 ('active-admission', live, 'if state.active.is_some() {', 'if false {', 'live_recording_start_resubmit', 'pending write blocks resubmit'),
 ('start-error-propagates', runner, 'self.io(|r, s, t| r.start_turn(s, t)).await?;', 'let _ = self.io(|r, s, t| r.start_turn(s, t)).await;', 'live_recording_start_failure', 'called `Result::unwrap_err()` on an `Ok` value'),
 ('shared-lease-exclusive-only', interaction, 'if let Ok(mut lease) = Arc::try_unwrap(owned.lease)', 'if Arc::strong_count(&owned.lease) > 1 { return Err(ModuleError::new("shared recorder lease incorrectly blocks handler cleanup").into()); }\n                if let Ok(mut lease) = Arc::try_unwrap(owned.lease)', 'recording_shared_lease_outlives_handler_cleanup', 'shared recorder lease incorrectly blocks handler cleanup'),
 ('accept-failure-not-success', runner, '("type", "error".into()),', '("type", "success".into()),', 'live_recording_accept_failure', 'assertion `left == right` failed'),
 ('complete-failure-not-success', runner, '("type", "error".into()),', '("type", "success".into()),', 'live_recording_complete_failure', 'assertion `left == right` failed'),
 ('real-slot', live, 'Recording::new(runner, owner)', 'Recording::new(runner, { drop(owner); Arc::new(()) })', 'live_recording_start_drop', 'worker owns active slot'),
 ('worker-slot', runner, 'let owner = self.owner.clone();', 'let owner: Option<Arc<dyn Send + Sync>> = None;', 'live_recording_accept_drop', 'worker owns active slot'),
 ('worker-lease', runner, 'let result = write(&recorder, &session, &turn);\n            drop(owner);\n            drop(lease);', 'drop(lease);\n            let result = write(&recorder, &session, &turn);\n            drop(owner);', 'live_recording_complete_dispose', 'worker retains lease'),
 ('waiter-takes-transition', driver, 'self\n            .pending\n            .as_mut()', 'self\n            .pending\n            .take()', 'live_recording_accept_cancel', 'pending predecessor'),
 ('pending-payload', live, 'self.pending_event = Some((event, terminal, receipt));', 'self.pending_event = Some((if terminal { crate::metadata::object([("type", "turn_completed".into())]) } else { event }, terminal, receipt));', 'live_recording_complete_cancel', 'original payload survives cancelled next'),
 ('terminal-releases-runner', live, '        self.recording.take();', '        // mutant retains recording', 'live_recording_complete_cancel', 'EOF stream releases slot'),
 ('accepted-marker', live, 'self.accepted = true;', 'self.accepted = false;', 'live_recording_complete_cancel', 'recording gate must precede delivery'),
 ('complete-dispatch', live, '_ if terminal && self.accepted =>', '_ if terminal && !self.accepted =>', 'live_recording_complete_cancel', 'recording gate must precede delivery'),
 ('accept-dispatch', live, '"input_accepted",\n                                RecordingAction::Accept(self.input.clone()),', '"input_accepted", RecordingAction::Start,', 'live_recording_accept_cancel', 'recording gate must precede delivery'),
 ('start-dispatch', live, '("turn_started", RecordingAction::Start)', '("turn_started", RecordingAction::Accept(self.input.clone()))', 'live_recording_start_cancel', 'recording gate must precede delivery'),
]
# Preflight payloads now come from the committed runner; completion preserves
# the original sidecar payload. Mutate the delivery path each phase really uses.
for phase, kind, field in [('start','turn_started','sessionId'),('accept','input_accepted','messages'),('complete','turn_completed','messages')]:
 old = 'self.pending_event = Some((event, terminal, receipt));' if phase == 'complete' else 'fn deliver_recorded(&mut self, event: JsValue) -> Result<Option<JsValue>, RuntimeError> {'
 injection = f'let event = if event.get("type") == Some(&"{kind}".into()) {{ let mut fields = event.as_object().unwrap().clone(); fields.insert("{field}".into(), JsValue::Null); JsValue::Object(fields) }} else {{ event }};'
 new = injection + old if phase == 'complete' else old + injection
 for action in ['cancel', 'resubmit']:
  controls.append((f'payload-fields-{phase}-{action}', live, old, new, f'live_recording_{phase}_{action}', 'original payload survives cancelled next'))
def mutate(source, scope, replacements):
 for old,new in replacements:
  assert source.count(old)==1,(old,source.count(old))
  source=source.replace(old,new,1)
 return source

def full_test(test):
 prefix = 'runtime::project::factory::tests::interaction_host::' if test == 'recording_shared_lease_outlives_handler_cleanup' else 'runtime::turn::live::stream_tests::'
 return prefix + test

def audit():
 for name,path,old,new,test,anchor in controls:
  mutate(path.read_text(),None,[(old,new)])
 return [{'mutation':n,'test':full_test(t),'assertion':a} for n,_,_,_,t,a in controls]

if sys.argv[1:]==['--audit']:
 print(json.dumps(audit(),indent=2));raise SystemExit()
env=os.environ.copy()
if not env.get('CARGO_TARGET_DIR'): raise SystemExit('CARGO_TARGET_DIR must be supplied by the caller')
env.update(CARGO_PROFILE_DEV_DEBUG='line-tables-only',CARGO_PROFILE_TEST_DEBUG='line-tables-only',CARGO_TERM_QUIET='false')
out=Path(sys.argv[1]).resolve();out.mkdir(parents=True,exist_ok=False)
if sys.argv[2:]:
 selected=set(sys.argv[2:])
 unknown=selected-{c[0] for c in controls}
 if unknown: raise SystemExit(f'unknown controls: {sorted(unknown)}')
 controls=[c for c in controls if c[0] in selected]
raise SystemExit(run_cases(root,
 [Case(n,p,old,new,full_test(test),anchor) for n,p,old,new,test,anchor in controls],
 out,env,mutate,baseline=env.get('PDRS_NEGATIVE_BASELINE','HEAD')))
