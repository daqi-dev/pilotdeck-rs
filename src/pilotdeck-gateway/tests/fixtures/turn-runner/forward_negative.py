#!/usr/bin/env python3
"""Bounded runtime controls; commit first, caller owns CARGO_TARGET_DIR."""
import json, os, sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot
from _negative_common import mutate
root=Path(__file__).resolve().parents[5]
base=root/'src/pilotdeck-gateway/src/runtime/turn'
# id, source, function scope, unique mutation, test, assertion anchor
cases=[]
def add(name,file,scope,old,new,test,anchor='forward event order/payload'):
    cases.append((name,base/file,scope,[(old,new)],'runtime::turn::live::stream_tests::forwarding::'+test,anchor))
add('wire-origin','live.rs','forward','(e, true)','(e, false)','forward_status_explicit')
add('host-origin','live.rs','forward','(JsValue::Object(e), false)','(JsValue::Object(e), true)','forward_host_status')
add('loop-failed-dispatch','live.rs','record_event','kind.equals_str("turn_failed")','kind.equals_str("never_failed")','forward_failed_twice')
add('failure-raw-fields','live.rs','settle_recording','*events.last_mut().expect("loop failure") = event;','let _ = event;','forward_failed_twice')
add('artifact-filter','live.rs',None,'event.get("type") == Some(&"file_artifacts".into())','event.get("type") == Some(&"never_artifacts".into())','forward_artifacts')
add('status-callback','live.rs','record_event','wire && kind.equals_str("agent_status")','false && wire && kind.equals_str("agent_status")','forward_status_explicit')
add('status-write','runner.rs','loop_status','r.writer().record_agent_status_message(s, t, &status)?','{ let _ = (r,s,t,status); () }','forward_status_explicit','forward durable callback/complete')
add('status-kind','runner.rs','loop_status','kind == "error".into()','kind == "status".into()','forward_status_explicit')
add('status-exclusion','runner.rs','loop_status','name != "turn_failed".into()','true','forward_status_turn_failed')
add('status-visible','runner.rs','loop_status','name != "turn_failed".into()','name != "turn_failed".into() && detail.and_then(|d| d.get("visible")) != Some(&JsValue::Bool(false))','forward_status_hidden')
add('status-severity','runner.rs','loop_status','Some(&"error".into())','Some(&"wrong".into())','forward_status_severity')
for label in ['max_budget_reached','task_budget_reached','model_request_failed','tool_call_recovery_exhausted','max_turns_reached','max_output_recovery_exhausted','empty_response','content_filter','unknown_finish_reason']:
    add('implicit-'+label,'runner.rs','loop_status','"'+label+'"','"disabled_'+label+'"','forward_status_implicit_'+label)
add('status-nullish','runner.rs','loop_status','JsValue::Null | JsValue::Undefined','JsValue::Undefined','forward_status_nullish')
add('status-text','runner.rs','loop_status','present(event.get("text"))','None::<JsValue>','forward_status_text','forward durable callback/complete')
add('status-detail-text','runner.rs','loop_status','matches!(v, JsValue::String(_))','{ let _ = v; false }','forward_status_severity','forward durable callback/complete')
add('catch-unconditional','runner.rs','fail_inner','let status = self.status(&error).await;','let status = if self.failure_status {self.event("skipped",[])} else {self.status(&error).await};','forward_catch_complete_once')
add('receipt-required','live.rs','forward','if record_status','if false && record_status','forward_status_receipt_failure',['status awaits durable receipt','status callback cannot release successor'])
add('malformed-failure','runner.rs','loop_failed','matches!(error, JsValue::Null | JsValue::Undefined)','false','forward_malformed_null_first')
add('host-callback-cancel','live.rs','next','cancel.cancel(None);','let _ = &cancel;','forward_malformed_host','LiveTurn must make progress')
add('pump-cleanup','live.rs','with_pump','let _ = execution.await;','let _ = &mut execution;','forward_pump_error_cleanup','pump error must await execution cleanup')
add('kind-falsy','runner.rs','loop_status','present(event.get("kind"))','present(event.get("kind")).filter(super::submit::truthy)','forward_status_kind_false')
add('text-falsy','runner.rs','loop_status','present(event.get("text"))','present(event.get("text")).filter(super::submit::truthy)','forward_status_text_false','forward durable callback/complete')
add('detail-null','runner.rs','loop_status','(detail, &mut status)','(detail.filter(|v| **v != JsValue::Null), &mut status)','forward_status_detail_null','forward durable callback/complete')
add('status-pending-retained','live.rs',None,'if self.pending_event.is_some() {','if self.pending_event.take().is_some() {','forward_status_cancel','status predecessor still pending')

add('status-worker-slot','runner.rs','io','let owner = self.owner.clone();','let owner: Option<Arc<dyn Send + Sync>> = None;','forward_status_drop','status dropped worker holds slot')
cases.append(('status-worker-lease',base/'runner.rs',None,[('let result = write(&recorder, &session, &turn);\n            drop(owner);\n            drop(lease);','drop(lease);\n            let result = write(&recorder, &session, &turn);\n            drop(owner);')],'runtime::turn::live::stream_tests::forwarding::forward_status_drop','status dropped worker holds lease'))

def audit():
    for name,path,scope,replacements,test,anchor in cases:
        mutate(path.read_text(),scope,replacements)
        assert test.rsplit('::',1)[1] in (base/'forward_tests.rs').read_text(),test
    return [{'mutation':n,'test':t,'assertion':a} for n,_,_,_,t,a in cases]
if sys.argv[1:] == ['--audit']:
    print(json.dumps(audit(),indent=2));raise SystemExit()
env=os.environ.copy()
if not env.get('CARGO_TARGET_DIR'): raise SystemExit('CARGO_TARGET_DIR must be supplied by caller')
env.update(CARGO_PROFILE_DEV_DEBUG='line-tables-only',CARGO_PROFILE_TEST_DEBUG='line-tables-only',CARGO_TERM_QUIET='false')
out=Path(sys.argv[1]);out.mkdir(parents=True,exist_ok=False)
selected=cases[:2] if sys.argv[2:]==['--sample'] else cases
audit()
command=['cargo','test','--offline','--manifest-path','src/Cargo.toml','-p','pilotdeck-gateway','--lib']
def run(label,test):
    slot=wait_for_slot(root/'scripts/cargo-slots.sh',timeout=900)
    (out/(label+'.slots.log')).write_text(slot.stdout+slot.stderr)
    result=run_owned(command+[test,'--','--exact','--nocapture'],cwd=root,env=env,timeout=180)
    (out/(label+'.log')).write_text(result.stdout+result.stderr)
    return result
with mutating_sources(root,list({row[1] for row in selected})+[base/'forward_tests.rs']) as originals:
    for name,path,scope,replacements,test,anchor in selected:
        assert run(name+'-positive',test).returncode==0,name
        path.write_text(mutate(originals[path].decode(),scope,replacements))
        result=run(name+'-negative',test);log=result.stdout+result.stderr
        assert result.returncode!=0 and 'test result: FAILED' in log and test+' ... FAILED' in log and 'panicked at' in log and any(a in log for a in (anchor if isinstance(anchor,list) else [anchor])),(name,'must fail at bound runtime assertion')
        path.write_bytes(originals[path])
        assert run(name+'-restored',test).returncode==0,name
(out/'summary.json').write_text(json.dumps({'passed':[row[0] for row in selected]},indent=2)+'\n')
