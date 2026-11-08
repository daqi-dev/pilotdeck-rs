//! The reserved control capability delegates ordinary tools to the existing
//! native permission/hook scheduler, then records only its actual successes.
use super::{common::*, controller::StepBinding};
use crate::{
    host::{
        HostError, HostFuture,
        modules::capability::{
            ToolCall, ToolCatalog, ToolDefinition, ToolExecution, ToolInvocationContext,
        },
    },
    runtime::project::RuntimeLease,
};
use pilotdeck_protocol::{JsObject, JsValue};
use std::sync::Arc;
pub(super) const CONTROL: &str = "submit_step_result";
pub(super) struct Tools {
    pub base: Arc<dyn ToolExecution>,
    pub binding: StepBinding,
}
struct Control {
    fields: JsObject,
}
impl ToolDefinition for Control {
    fn fields(&self) -> &JsObject {
        &self.fields
    }
    fn is_read_only(&self, _: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(true))
    }
    fn is_concurrency_safe(&self, _: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(false))
    }
}
fn control() -> Arc<dyn ToolDefinition> {
    let fields=pilotdeck_protocol::parse_json(r#"{"name":"submit_step_result","title":"Submit SOP step result","description":"Submit the current StaffDeck SOP step result after all required PilotDeck tools have returned successfully.","kind":"structured_output","inputSchema":{"type":"object","properties":{"status":{"type":"string","enum":["completed","awaiting_user","handoff","failed","blocked","waiting_external_task"]},"replyFragment":{"type":"string"},"slotUpdates":{"type":"object","additionalProperties":true,"description":"Persist every known user field using the Required user information keys. Required even for awaiting_user: missing other fields must not discard fields already supplied. Use {} only when no new fields are known."},"taskSummary":{"type":"string"},"structuredResult":{},"nextStepId":{"type":"string"}},"required":["status","replyFragment","slotUpdates"],"additionalProperties":false}}"#).expect("host control schema").as_object().expect("host control schema object").clone();
    Arc::new(Control { fields })
}
impl ToolCatalog for Tools {
    fn list(&self) -> HostFuture<Vec<Arc<dyn ToolDefinition>>> {
        let base = self.base.clone();
        let binding = self.binding.clone();
        Box::pin(async move {
            let mut tools = base.list().await?;
            if tools.iter().any(|t| {
                t.fields()
                    .get("name")
                    .and_then(JsValue::as_string)
                    .is_some_and(|n| n.equals_str(CONTROL))
            }) {
                return Err(error(
                    "SOP_RESERVED_TOOL",
                    "submit_step_result is reserved by the StaffDeck SOP runtime.",
                ));
            }
            let step = binding.step();
            if strings(field(&step, "requiredToolNames")).is_empty() {
                tools.clear();
            }
            tools.push(control());
            Ok(tools)
        })
    }
}
impl ToolExecution for Tools {
    fn bind_execution_generation(&self, g: Arc<RuntimeLease>) -> Result<(), HostError> {
        self.base.bind_execution_generation(g)
    }
    fn applies_mode_effects(&self) -> bool {
        self.base.applies_mode_effects()
    }
    fn bind_turn_permission(
        &self,
        p: crate::runtime::project::interaction_permission::TurnPermission,
    ) -> Result<(), HostError> {
        self.base.bind_turn_permission(p)
    }
    fn bind_turn_hooks(
        &self,
        p: crate::runtime::project::interaction_hooks::TurnHooks,
    ) -> Result<(), HostError> {
        self.base.bind_turn_hooks(p)
    }
    fn dispose(&self) -> HostFuture<()> {
        self.base.dispose()
    }
    fn wait_closed(&self) -> HostFuture<()> {
        self.base.wait_closed()
    }
    fn drain_events(&self) -> Result<Vec<JsObject>, HostError> {
        self.base.drain_events()
    }
    fn refresh(&self) -> HostFuture<()> {
        self.base.refresh()
    }
    fn execute_all(
        &self,
        calls: Vec<ToolCall>,
        context: ToolInvocationContext,
    ) -> HostFuture<Vec<JsObject>> {
        let base = self.base.clone();
        let binding = self.binding.clone();
        Box::pin(async move {
            let ordinary = calls
                .iter()
                .filter(|c| !c.name.equals_str(CONTROL))
                .cloned()
                .collect::<Vec<_>>();
            let has_ordinary = !ordinary.is_empty();
            let step = binding.step();
            let mut results = vec![];
            if !ordinary.is_empty() {
                if strings(field(&step, "requiredToolNames")).is_empty() {
                    for c in ordinary {
                        results.push(control_error(
                            &c,
                            "SOP_AUTHORITY_TOOL_UNAVAILABLE",
                            "tool_execution_failed",
                            None,
                        ));
                    }
                } else {
                    results = base.execute_all(ordinary, context).await?;
                }
            }
            let names = results
                .iter()
                .filter(|r| r.get("type") == Some(&JsValue::from("success")))
                .filter_map(|r| r.get("toolName").and_then(JsValue::as_string).cloned())
                .collect::<Vec<_>>();
            if !names.is_empty() {
                binding
                    .run
                    .store
                    .record_successful_tools(
                        &binding.run.session,
                        binding.run.bundle.clone(),
                        binding.run.selected.clone(),
                        names,
                    )
                    .await
                    .map_err(store_error)?;
            }
            if results.iter().any(|r| {
                r.get("type") == Some(&JsValue::from("error"))
                    && r.get("error").and_then(|v| v.get("code"))
                        != Some(&JsValue::from("invalid_tool_input"))
            }) {
                binding
                    .run
                    .saw_tool_error
                    .store(true, std::sync::atomic::Ordering::SeqCst);
            }
            for call in calls.iter().filter(|c| c.name.equals_str(CONTROL)) {
                let result = if has_ordinary {
                    control_error(
                        call,
                        "Call submit_step_result only after the preceding tool results are available to you.",
                        "tool_execution_failed",
                        None,
                    )
                } else {
                    submit(&binding, call).await
                };
                results.push(result);
            }
            if results.iter().any(|r| {
                r.get("type") == Some(&JsValue::from("error"))
                    && r.get("error").and_then(|v| v.get("code"))
                        != Some(&JsValue::from("invalid_tool_input"))
            }) {
                binding
                    .run
                    .saw_tool_error
                    .store(true, std::sync::atomic::Ordering::SeqCst);
            }
            Ok(calls
                .iter()
                .map(|call| {
                    results
                        .iter()
                        .rev()
                        .find(|r| r.get("toolCallId") == Some(&JsValue::String(call.id.clone())))
                        .cloned()
                        .unwrap_or_else(|| {
                            control_error(
                                call,
                                "Tool execution produced no result.",
                                "tool_execution_failed",
                                None,
                            )
                        })
                })
                .collect())
        })
    }
}
fn result_base(call: &ToolCall, kind: &str) -> JsObject {
    let mut out = JsObject::new();
    out.insert("type".into(), kind.into());
    out.insert("toolCallId".into(), JsValue::String(call.id.clone()));
    out.insert("toolName".into(), JsValue::String(call.name.clone()));
    let now = clock();
    out.insert("startedAt".into(), JsValue::String(now.clone()));
    out.insert("completedAt".into(), JsValue::String(now));
    out
}
fn control_error(call: &ToolCall, message: &str, code: &str, details: Option<JsValue>) -> JsObject {
    let mut out = result_base(call, "error");
    let mut fields = JsObject::new();
    fields.insert("code".into(), code.into());
    fields.insert("message".into(), message.into());
    if let Some(details) = details {
        fields.insert("details".into(), details);
    }
    out.insert("error".into(), JsValue::Object(fields));
    out.insert(
        "content".into(),
        JsValue::Array(vec![crate::metadata::object([
            ("type", "text".into()),
            ("text", message.into()),
        ])]),
    );
    out
}
fn success(call: &ToolCall, result: JsValue, prior: &JsValue, state: &JsValue) -> JsObject {
    let mut out = result_base(call, "success");
    let mut projection = JsObject::new();
    projection.insert(
        "submittedStepId".into(),
        prior
            .get("active_step_id")
            .cloned()
            .unwrap_or(JsValue::Null),
    );
    projection.insert("status".into(), field(&result, "status").clone());
    projection.insert("resultStatus".into(), field(&result, "status").clone());
    projection.insert(
        "activeStepId".into(),
        state
            .get("active_step_id")
            .cloned()
            .unwrap_or(JsValue::Null),
    );
    projection.insert(
        "sopStatus".into(),
        state.get("status").cloned().unwrap_or(JsValue::Null),
    );
    projection.insert(
        "nextStepId".into(),
        result.get("nextStepId").cloned().unwrap_or(JsValue::Null),
    );
    if active(state) {
        projection.insert("nextStepRequiresOwnSubmission".into(), JsValue::Bool(true));
    }
    out.insert(
        "content".into(),
        JsValue::Array(vec![crate::metadata::object([
            ("type", "json".into()),
            ("value", JsValue::Object(projection)),
        ])]),
    );
    out.insert("data".into(), result);
    out.insert(
        "metadata".into(),
        crate::metadata::object([("structuredOutput", JsValue::Bool(true))]),
    );
    out
}
pub(crate) fn parse_proposal(input: &JsValue) -> Option<JsValue> {
    let status = field(input, "status").as_string()?;
    if ![
        "completed",
        "awaiting_user",
        "handoff",
        "failed",
        "blocked",
        "waiting_external_task",
    ]
    .iter()
    .any(|s| status.equals_str(s))
    {
        return None;
    }
    let reply = text(field(input, "replyFragment"))?;
    if input.as_object().is_none()
        || input
            .get("slotUpdates")
            .filter(|v| !v.is_undefined())
            .is_some_and(|v| v.as_object().is_none())
    {
        return None;
    }
    for k in ["nextStepId", "taskSummary"] {
        if !matches!(
            field(input, k),
            JsValue::Undefined | JsValue::Null | JsValue::String(_)
        ) {
            return None;
        }
    }
    let mut out = JsObject::new();
    out.insert("status".into(), JsValue::String(status.clone()));
    out.insert("replyFragment".into(), JsValue::String(reply));
    if let Some(slots) = field(input, "slotUpdates").as_object() {
        out.insert("slotUpdates".into(), JsValue::Object(slots.clone()));
    }
    for k in ["taskSummary", "nextStepId"] {
        if let Some(v) = text(field(input, k)) {
            out.insert(k.into(), JsValue::String(v));
        }
    }
    if input.as_object()?.contains_key("structuredResult") {
        out.insert(
            "structuredResult".into(),
            field(input, "structuredResult").clone(),
        );
    }
    Some(JsValue::Object(out))
}
fn filled(v: &JsValue) -> bool {
    match v {
        JsValue::Null | JsValue::Undefined => false,
        JsValue::String(_) => text(v).is_some(),
        JsValue::Array(a) => !a.is_empty(),
        JsValue::Object(o) => !o.is_empty(),
        _ => true,
    }
}
async fn submit(binding: &StepBinding, call: &ToolCall) -> JsObject {
    let Some(mut proposal) = parse_proposal(&call.input) else {
        return control_error(
            call,
            "submit_step_result requires a valid status, non-empty replyFragment, and correctly typed known fields.",
            "invalid_tool_input",
            None,
        );
    };
    let run = &binding.run;
    let attempted:Result<JsObject,HostError>=async {
 let current=run.store.load_or_create(&run.session,run.bundle.clone(),run.selected.clone()).await.map_err(store_error)?;let state=field(&current,"state");let step=binding.step();
 if field(&step,"nodeId")==field(state,"active_step_id"){
  let fields=strings(field(&step,"expectedUserInfo"));let slots=field(state,"slots_json");let updates=field(&proposal,"slotUpdates");
  for field_key in &fields{if updates.as_object().is_some_and(|o|o.get_key(field_key).is_some())&&filled(key(slots,field_key))&&!filled(key(updates,field_key)){return Ok(control_error(call,&format!("REQUIRED_SLOT_ERASURE: {} is already filled; an empty update cannot erase it.",String::from_utf16_lossy(field_key.units())),"invalid_tool_input",None));}}
  let status=field(&proposal,"status");let handoff=field(&step,"declaresHandoff")==&JsValue::Bool(true);
  if status==&JsValue::from("handoff")&&!handoff{return Ok(control_error(call,"HANDOFF_NOT_DECLARED: complete the current evidence step and advance to the declared approval node before creating handoff.","invalid_tool_input",None));}
  if status==&JsValue::from("awaiting_user")&&handoff{return Ok(control_error(call,"HANDOFF_REQUIRED: this node declares a resumable human approval handoff. Submit handoff to wait for the responsible person's reply; awaiting_user cannot create that approval wait.","invalid_tool_input",None));}
  let allowed=strings(field(&step,"allowedNextStepIds"));
  if let Some(next)=field(&proposal,"nextStepId").as_string(){if !allowed.contains(next){let list=if allowed.is_empty(){"none (this node is terminal or must remain active)".into()}else{allowed.iter().map(|s|String::from_utf16_lossy(s.units())).collect::<Vec<_>>().join(", ")};let message=format!("SOP_INVALID_TRANSITION: nextStepId '{}' is not declared from '{}'. Allowed nextStepId values: {list}. Do not invent a transition; preserve the current state and submit the declared result or ask for the missing information.",String::from_utf16_lossy(next.units()),display(field(&step,"nodeId")));return Ok(control_error(call,&message,"invalid_tool_input",Some(crate::metadata::object([("activeStepId",field(&step,"nodeId").clone()),("requestedNextStepId",JsValue::String(next.clone())),("allowedNextStepIds",field(&step,"allowedNextStepIds").clone())]))));}}
  if status==&JsValue::from("awaiting_user"){
   if proposal.get("nextStepId").is_some(){return Ok(control_error(call,"AWAITING_USER_CANNOT_ADVANCE: remove nextStepId and retain the current step.","invalid_tool_input",None));}
   let mut merged=object(slots);if let Some(o)=updates.as_object(){for(k,v)in o.own_entries(){merged.insert(k.clone(),v.clone());}}
   let missing=fields.iter().filter(|k|!filled(merged.get_key(*k).unwrap_or(&JsValue::Undefined))).cloned().collect::<Vec<_>>();let kind=field(field(&step,"node"),"type");
   if !missing.is_empty()&&kind==&JsValue::from("collect_info")&&(field(state,"status")==&JsValue::from("awaiting_user")||updates.as_object().is_none_or(|o|o.is_empty()))&&!run.user.trim().is_empty()&&!run.collect_checked.swap(true,std::sync::atomic::Ordering::SeqCst){let message=format!("COLLECT_FIELDS_CHECK: Before waiting, re-read this turn's user message: {}. Persisted slots: {}. Your proposal still leaves these fields missing: {}. Extract every explicitly supplied value into slotUpdates using these exact keys: {}. If a field is genuinely absent, resubmit awaiting_user for only that field; do not invent a value or repeat fields already supplied.",json(&JsValue::from(run.user.as_str())),json(slots),missing.iter().map(|s|String::from_utf16_lossy(s.units())).collect::<Vec<_>>().join(", "),fields.iter().map(|s|String::from_utf16_lossy(s.units())).collect::<Vec<_>>().join(", "));return Ok(control_error(call,&message,"invalid_tool_input",None));}
   let actions=strings(field(&step,"allowedActions"));let can_ask=actions.iter().any(|s|s.equals_str("ask_user")||s.equals_str("ask_missing"));
   if missing.is_empty()&&(!fields.is_empty()||!can_ask){return Ok(control_error(call,"STEP_MUST_ADVANCE: required fields are complete. Submit completed to an allowed next step; approval belongs to the declared handoff node.","invalid_tool_input",None));}
   if kind==&JsValue::from("collect_info")&&!missing.is_empty(){set(&mut proposal,"replyFragment",format!("请补充以下信息，以继续当前步骤：\n{}",missing.iter().map(|s|format!("- {}",String::from_utf16_lossy(s.units()))).collect::<Vec<_>>().join("\n")).as_str().into());}
  }
  if field(&proposal,"status")==&JsValue::from("completed")&&field(&step,"isTerminal")==&JsValue::Bool(true)&&fields.is_empty()&&!strings(field(&step,"allowedActions")).iter().any(|s|s.equals_str("ask_user")||s.equals_str("ask_missing"))&&claims_wait(&display(field(&proposal,"replyFragment"))){return Ok(control_error(call,"FINAL_REPLY_CONTRADICTS_COMPLETION: this terminal step has no missing user fields, but replyFragment says progress is blocked or asks the user for missing information. Rewrite the final answer from the current step instruction and known facts, then submit completed without a nextStepId. Do not reuse the rejected waiting reply.","invalid_tool_input",None));}
 }
 let successful=strings(field(state,"successful_tool_names")).into_iter().filter(|s|!s.is_empty()).collect::<Vec<_>>();
 if field(&proposal,"status")==&JsValue::from("completed")&&field(field(&step,"node"),"type")==&JsValue::from("knowledge_query")&&!successful.iter().any(|s|s.equals_str("knowledge_query")){return Ok(control_error(call,"REQUIRED_CAPABILITY_NOT_INVOKED: the current knowledge node requires a successful knowledge_query receipt before completion.","invalid_tool_input",None));}
 let context=crate::metadata::object([("runId",JsValue::String(run.identity.run_id.clone())),("operationId",JsValue::String(run.identity.operation_id.clone())),("requestId",format!("sop.submit:{}",String::from_utf16_lossy(call.id.units())).as_str().into()),("sessionId",JsValue::String(run.session.clone())),("turnId",JsValue::String(run.identity.turn_id.clone())),("idempotencyKey",JsValue::String(call.id.clone())),("deadlineAt",run.deadline.clone().map(JsValue::String).unwrap_or(JsValue::Undefined)),("expectedRevision",field(&current,"revision").clone())]);
 let signal=crate::runtime::network::Cancellation::new();
 let submitted=tokio::select!{biased;_=run.signal.cancelled()=>{signal.abort();return Err(cancelled());},v=run.client.submit(field(&current,"bundle").clone(),state.clone(),proposal,successful,Some(&context),&signal)=>v.map_err(client_error)?};
 let next_state=field(&submitted,"state");let result=field(&submitted,"result").clone();
 run.store.commit_submission(&run.session,field(&current,"bundle").clone(),next_state.clone(),field(&current,"revision").as_number().unwrap_or(0.0),run.identity.turn_id.clone(),result.clone()).await.map_err(store_error)?;
 if active(next_state){run.store.clear_reply_delivery(&run.session,run.identity.turn_id.clone()).await.map_err(store_error)?;}else{*run.submission.lock().unwrap_or_else(|e|e.into_inner())=Some(result.clone());}
 Ok(success(call,result,state,next_state))
 }.await;
    match attempted {
        Ok(result) => result,
        Err(e) => {
            let code = e
                .module_error
                .as_ref()
                .and_then(|m| m.code.as_ref())
                .map(|s| String::from_utf16_lossy(s.units()))
                .unwrap_or_else(|| "SOP_RUNTIME_REJECTED".into());
            let details = crate::metadata::object([(
                "sopRuntime",
                crate::metadata::object([
                    ("code", code.as_str().into()),
                    ("message", e.message.as_str().into()),
                    ("retryability", "unsafe".into()),
                    (
                        "details",
                        e.module_error
                            .as_ref()
                            .and_then(|m| m.canonical.clone())
                            .map(JsValue::Object)
                            .unwrap_or_else(|| JsValue::Object(JsObject::new())),
                    ),
                ]),
            )]);
            control_error(
                call,
                &format!("[{code}] {}", e.message),
                if code == "SOP_RUNTIME_CANCELLED" {
                    "tool_aborted"
                } else {
                    "tool_execution_failed"
                },
                Some(details),
            )
        }
    }
}
fn claims_wait(reply: &str) -> bool {
    static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?i)(?:^|[。！？.!?\n])\s*(?:(?:由于)?(?:我|目前|当前|本步骤|此步骤|SOP).{0,80}(?:卡在|无法(?:继续|推进|完成|生成)|缺少.{0,30}(?:信息|资料|上下文))|请(?:您|你)?(?:提供|补充)|(?:I|we|this step|the SOP)\s+(?:(?:am|are|is)\s+)?(?:stuck|blocked|cannot|can't|lack|need more information)|(?:please|could you)\s+(?:provide|supply))").expect("SOP wait wording regex")
    });
    RE.is_match(reply)
}
