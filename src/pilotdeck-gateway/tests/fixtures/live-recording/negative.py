#!/usr/bin/env python3
"""Mutate actual live owners; every RED is bound to its test and panic anchor."""
import json, os, sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot
root = Path(__file__).resolve().parents[5]
out = Path(sys.argv[1]).resolve(); out.mkdir(parents=True, exist_ok=False)
env = os.environ.copy()
if not env.get('CARGO_TARGET_DIR'): raise SystemExit('CARGO_TARGET_DIR must be supplied by the caller')
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
 ('pending-payload', live, 'self.pending_event = Some((event, terminal));', 'self.pending_event = Some((if terminal { crate::metadata::object([("type", "turn_completed".into())]) } else { event }, terminal));', 'live_recording_complete_cancel', 'original payload survives cancelled next'),
 ('terminal-releases-runner', live, '        self.recording.take();', '        // mutant retains recording', 'live_recording_complete_cancel', 'EOF stream releases slot'),
 ('accepted-marker', live, 'self.accepted = true;', 'self.accepted = false;', 'live_recording_complete_cancel', 'recording gate must precede delivery'),
 ('complete-dispatch', live, '_ if terminal && self.accepted =>', '_ if terminal && !self.accepted =>', 'live_recording_complete_cancel', 'recording gate must precede delivery'),
 ('accept-dispatch', live, '"input_accepted",\n                                RecordingAction::Accept(self.input.clone()),', '"input_accepted", RecordingAction::Start,', 'live_recording_accept_cancel', 'recording gate must precede delivery'),
 ('start-dispatch', live, '("turn_started", RecordingAction::Start)', '("turn_started", RecordingAction::Accept(self.input.clone()))', 'live_recording_start_cancel', 'recording gate must precede delivery'),
]
# Preflight payloads now come from the committed runner; completion preserves
# the original sidecar payload. Mutate the delivery path each phase really uses.
for phase, kind, field in [('start','turn_started','sessionId'),('accept','input_accepted','messages'),('complete','turn_completed','messages')]:
 old = 'self.pending_event = Some((event, terminal));' if phase == 'complete' else 'fn deliver_recorded(&mut self, event: JsValue) -> Result<Option<JsValue>, RuntimeError> {'
 injection = f'let event = if event.get("type") == Some(&"{kind}".into()) {{ let mut fields = event.as_object().unwrap().clone(); fields.insert("{field}".into(), JsValue::Null); JsValue::Object(fields) }} else {{ event }};'
 new = injection + old if phase == 'complete' else old + injection
 for action in ['cancel', 'resubmit']:
  controls.append((f'payload-fields-{phase}-{action}', live, old, new, f'live_recording_{phase}_{action}', 'original payload survives cancelled next'))
if sys.argv[2:]:
 selected = set(sys.argv[2:])
 assert selected <= {c[0] for c in controls}
 controls = [c for c in controls if c[0] in selected]
def run(name, test='live_recording_'):
 slots=wait_for_slot(root/'scripts/cargo-slots.sh', timeout=900)
 (out/f'{name}.slots.log').write_text(slots.stdout+slots.stderr)
 r=run_owned(['cargo','test','--offline','--manifest-path','src/Cargo.toml','-p','pilotdeck-gateway','--lib',test,'--','--test-threads=1'],cwd=root,env=env,timeout=900)
 (out/f'{name}.log').write_text(r.stdout+r.stderr)
 return r
summary=[]
with mutating_sources(root, [live,runner,driver,interaction], baseline=env.get('PDRS_NEGATIVE_BASELINE','HEAD')) as originals:
 assert run('positive').returncode==0
 assert run('positive-shared','recording_shared_lease').returncode==0
 for name,path,old,new,test,anchor in controls:
  source=originals[path].decode(); assert source.count(old)==1,(name,source.count(old))
  path.write_text(source.replace(old,new,1))
  r=run(name,test); output=r.stdout+r.stderr
  assert r.returncode!=0 and 'test result: FAILED' in output and 'panicked at' in output,(name,output[-2000:])
  assert f'::{test} ... FAILED' in output and anchor in output,(name,'wrong failure',output[-2000:])
  path.write_bytes(originals[path])
  summary.append({'mutation':name,'test':test,'assertion':anchor,'runtimeRed':True})
  print(name,'runtime RED',flush=True)
 assert run('restored').returncode==0
 assert run('restored-shared','recording_shared_lease').returncode==0
(out/'summary.json').write_text(json.dumps({'controls':summary,'restoredPass':True},indent=2)+'\n')
