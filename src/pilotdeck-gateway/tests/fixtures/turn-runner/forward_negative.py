#!/usr/bin/env python3
"""Bounded runtime controls; commit first, caller owns CARGO_TARGET_DIR."""
import json, os, sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_suite import Case, run_cases
from _negative_common import mutate
root=Path(__file__).resolve().parents[5]
base=root/'src/pilotdeck-gateway/src/runtime/turn'
# Whole-file occurrences for the static preflight; mutate() still requires
# exactly one occurrence inside the named Rust function.
ANCHOR_COUNTS = {'status-nullish': 3, 'host-callback-cancel': 2, 'pump-cleanup': 2}
# id, source, before, after, function scope, full test name, assertion anchor
test_prefix = 'runtime::turn::live::stream_tests::forwarding::'
cases = [
    ('cleanup-skip-lease', base / 'forward_tests.rs',
     '            || runtime.lease_count() != 0', '            || false',
     None, test_prefix + 'forward_status_drop', '#741h cleanup observer waits for lease'),
    ('cleanup-late-deadline', base / 'forward_tests.rs',
     'timeout(Duration::from_secs(10), async {', 'timeout(Duration::from_secs(20), async {',
     None, test_prefix + 'forward_status_drop', '#741h cleanup observer reaches 10-second deadline'),
    ('wire-origin', base / 'live.rs',
     '(e, true)', '(e, false)',
     'forward', test_prefix + 'forward_status_explicit', '#741h forward event order/payload'),
    ('host-origin', base / 'live.rs',
     '(JsValue::Object(e), false)', '(JsValue::Object(e), true)',
     'forward', test_prefix + 'forward_host_status', '#741h forward event order/payload'),
    ('loop-failed-dispatch', base / 'live.rs',
     'kind.equals_str("turn_failed")', 'kind.equals_str("never_failed")',
     'record_event', test_prefix + 'forward_failed_twice', '#741h forward event order/payload'),
    ('failure-raw-fields', base / 'live.rs',
     '*events.last_mut().expect("loop failure") = event;', 'let _ = event;',
     'settle_recording', test_prefix + 'forward_failed_twice', '#741h forward event order/payload'),
    ('artifact-filter', base / 'live.rs',
     'event.get("type") == Some(&"file_artifacts".into())', 'event.get("type") == Some(&"never_artifacts".into())',
     None, test_prefix + 'forward_artifacts', '#741h forward event order/payload'),
    ('status-callback', base / 'live.rs',
     'wire && kind.equals_str("agent_status")', 'false && wire && kind.equals_str("agent_status")',
     'record_event', test_prefix + 'forward_status_explicit', '#741h forward event order/payload'),
    ('status-write', base / 'runner.rs',
     'r.writer().record_agent_status_message(s, t, &status)?', '{ let _ = (r,s,t,status); () }',
     'loop_status', test_prefix + 'forward_status_explicit', '#741h forward durable callback/complete'),
    ('status-kind', base / 'runner.rs',
     'kind == "error".into()', 'kind == "status".into()',
     'loop_status', test_prefix + 'forward_status_explicit', '#741h forward event order/payload'),
    ('status-exclusion', base / 'runner.rs',
     'name != "turn_failed".into()', 'true',
     'loop_status', test_prefix + 'forward_status_turn_failed', '#741h forward event order/payload'),
    ('status-visible', base / 'runner.rs',
     'name != "turn_failed".into()', 'name != "turn_failed".into() && detail.and_then(|d| d.get("visible")) != Some(&JsValue::Bool(false))',
     'loop_status', test_prefix + 'forward_status_hidden', '#741h forward event order/payload'),
    ('status-severity', base / 'runner.rs',
     'Some(&"error".into())', 'Some(&"wrong".into())',
     'loop_status', test_prefix + 'forward_status_severity', '#741h forward event order/payload'),
    ('implicit-max_budget_reached', base / 'runner.rs',
     '"max_budget_reached"', '"disabled_max_budget_reached"',
     'loop_status', test_prefix + 'forward_status_implicit_max_budget_reached', '#741h forward event order/payload'),
    ('implicit-task_budget_reached', base / 'runner.rs',
     '"task_budget_reached"', '"disabled_task_budget_reached"',
     'loop_status', test_prefix + 'forward_status_implicit_task_budget_reached', '#741h forward event order/payload'),
    ('implicit-model_request_failed', base / 'runner.rs',
     '"model_request_failed"', '"disabled_model_request_failed"',
     'loop_status', test_prefix + 'forward_status_implicit_model_request_failed', '#741h forward event order/payload'),
    ('implicit-tool_call_recovery_exhausted', base / 'runner.rs',
     '"tool_call_recovery_exhausted"', '"disabled_tool_call_recovery_exhausted"',
     'loop_status', test_prefix + 'forward_status_implicit_tool_call_recovery_exhausted', '#741h forward event order/payload'),
    ('implicit-max_turns_reached', base / 'runner.rs',
     '"max_turns_reached"', '"disabled_max_turns_reached"',
     'loop_status', test_prefix + 'forward_status_implicit_max_turns_reached', '#741h forward event order/payload'),
    ('implicit-max_output_recovery_exhausted', base / 'runner.rs',
     '"max_output_recovery_exhausted"', '"disabled_max_output_recovery_exhausted"',
     'loop_status', test_prefix + 'forward_status_implicit_max_output_recovery_exhausted', '#741h forward event order/payload'),
    ('implicit-empty_response', base / 'runner.rs',
     '"empty_response"', '"disabled_empty_response"',
     'loop_status', test_prefix + 'forward_status_implicit_empty_response', '#741h forward event order/payload'),
    ('implicit-content_filter', base / 'runner.rs',
     '"content_filter"', '"disabled_content_filter"',
     'loop_status', test_prefix + 'forward_status_implicit_content_filter', '#741h forward event order/payload'),
    ('implicit-unknown_finish_reason', base / 'runner.rs',
     '"unknown_finish_reason"', '"disabled_unknown_finish_reason"',
     'loop_status', test_prefix + 'forward_status_implicit_unknown_finish_reason', '#741h forward event order/payload'),
    ('status-nullish', base / 'runner.rs',
     'JsValue::Null | JsValue::Undefined', 'JsValue::Undefined',
     'loop_status', test_prefix + 'forward_status_nullish', '#741h forward event order/payload'),
    ('status-text', base / 'runner.rs',
     'present(event.get("text"))', 'None::<JsValue>',
     'loop_status', test_prefix + 'forward_status_text', '#741h forward durable callback/complete'),
    ('status-detail-text', base / 'runner.rs',
     'matches!(v, JsValue::String(_))', '{ let _ = v; false }',
     'loop_status', test_prefix + 'forward_status_severity', '#741h forward durable callback/complete'),
    ('catch-unconditional', base / 'runner.rs',
     'let status = self.status(&error).await;', 'let status = if self.failure_status {self.event("skipped",[])} else {self.status(&error).await};',
     'fail_inner', test_prefix + 'forward_catch_complete_once', '#741h forward event order/payload'),
    ('receipt-required', base / 'live.rs',
     'if record_status', 'if false && record_status',
     'forward', test_prefix + 'forward_status_receipt_failure', ['#741h status awaits durable receipt', '#741h status callback cannot release successor']),
    ('malformed-failure', base / 'runner.rs',
     'matches!(error, JsValue::Null | JsValue::Undefined)', 'false',
     'loop_failed', test_prefix + 'forward_malformed_null_first', '#741h forward event order/payload'),
    ('host-callback-cancel', base / 'live.rs',
     'cancel.cancel(None);', 'let _ = &cancel;',
     'next', test_prefix + 'forward_malformed_host', '#741h LiveTurn must make progress'),
    ('pump-cleanup', base / 'live.rs',
     'let _ = execution.await;', 'let _ = &mut execution;',
     'with_pump', test_prefix + 'forward_pump_error_cleanup', '#741h pump error must await execution cleanup'),
    ('kind-falsy', base / 'runner.rs',
     'present(event.get("kind"))', 'present(event.get("kind")).filter(super::submit::truthy)',
     'loop_status', test_prefix + 'forward_status_kind_false', '#741h forward event order/payload'),
    ('text-falsy', base / 'runner.rs',
     'present(event.get("text"))', 'present(event.get("text")).filter(super::submit::truthy)',
     'loop_status', test_prefix + 'forward_status_text_false', '#741h forward durable callback/complete'),
    # Omitting null from the temporary status is equivalent: the writer drops
    # both null and missing detail. Wrap the input instead, so a nonempty object
    # incorrectly reaches the transcript; keep the original test/assertion.
    ('detail-null', base / 'runner.rs',
     'status.insert("detail".into(), detail.clone());',
     'status.insert("detail".into(), if *detail == JsValue::Null { object([("value", detail.clone())]) } else { detail.clone() });',
     'loop_status', test_prefix + 'forward_status_detail_null', '#741h forward durable callback/complete'),
    ('status-pending-retained', base / 'live.rs',
     'if self.pending_event.is_some() {', 'if self.pending_event.take().is_some() {',
     None, test_prefix + 'forward_status_cancel', '#741h status predecessor still pending'),
    # #996-pr2 fix-r5: the gates-release-pending-slot switch made this let a
    # multi-line if/else, which the scoped mutate() single-line rule rejects.
    # Anchor the else branch alone; replacing it with a typed None keeps the
    # same mutant (the io worker holds no owner Arc).
    ('status-worker-slot', base / 'runner.rs',
     'self.owner.clone()', 'Option::<Arc<dyn Send + Sync>>::None',
     'io', test_prefix + 'forward_status_drop', '#741h status dropped worker holds slot'),
    ('status-worker-lease', base / 'runner.rs',
     'let result = write(&recorder, &session, &turn);\n            drop(owner);\n            drop(lease);', 'drop(lease);\n            let result = write(&recorder, &session, &turn);\n            drop(owner);',
     None, test_prefix + 'forward_status_drop', '#741h status dropped worker holds lease'),
]

def audit():
    for name,path,old,new,scope,test,anchor in cases:
        mutate(path.read_text(),scope,[(old,new)])
        assert test.rsplit('::',1)[1] in (base/'forward_tests.rs').read_text(),test
    return [{'mutation':n,'test':t,'assertion':a} for n,_,_,_,_,t,a in cases]
if sys.argv[1:] == ['--audit']:
    print(json.dumps(audit(),indent=2));raise SystemExit()
env=os.environ.copy()
if not env.get('CARGO_TARGET_DIR'): raise SystemExit('CARGO_TARGET_DIR must be supplied by caller')
env.update(CARGO_PROFILE_DEV_DEBUG='line-tables-only',CARGO_PROFILE_TEST_DEBUG='line-tables-only',CARGO_TERM_QUIET='false')
out=Path(sys.argv[1]);out.mkdir(parents=True,exist_ok=False)
selected=cases[:2] if sys.argv[2:]==['--sample'] else cases
if sys.argv[2:] and sys.argv[2:] != ['--sample']:
    names=set(sys.argv[2:])
    unknown=names-{row[0] for row in cases}
    if unknown: raise SystemExit(f'unknown controls: {sorted(unknown)}')
    selected=[row for row in cases if row[0] in names]
# Package cache contention is not test execution. Match the shared suite
# build allowance; runtime progress/cleanup assertions retain their own bounds.
raise SystemExit(run_cases(root,
    [Case(n,p,old,new,test,anchor,scope) for n,p,old,new,scope,test,anchor in selected],
    out,env,mutate,timeout=900,baseline=env.get('PDRS_NEGATIVE_BASELINE','HEAD')))
