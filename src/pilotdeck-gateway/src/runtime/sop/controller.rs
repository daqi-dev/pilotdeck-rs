//! Native orchestration of the portable StaffDeck owner. The host executor owns
//! node admission, sidechain persistence and real per-node stream validation.
use super::{
    client::SopClient,
    common::*,
    definitions,
    discovery::{Discovery, encode},
};
use crate::{
    host::{ExecuteRequest, ExecuteResult, HostError, HostFuture},
    module_host::approvals::{disk::DiskSopStore, native_http::JsonTransport},
    runtime::turn::loop_control::{
        HostLoopController, SopFuture, SopLoopInput, SopNodeContext, SopNodeExecutor,
        SopNodeOutcome, SopNodePlan, SopRunBinding,
    },
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, admission::ExecutionIdentity};
use std::{
    collections::HashSet,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};
pub(crate) type ModelValidator = Arc<dyn Fn(JsObject) -> HostFuture<()> + Send + Sync>;
pub(crate) struct StaffDeckController {
    profile: JsValue,
    bundle: JsValue,
    store: Arc<DiskSopStore>,
    client: Arc<SopClient>,
    discovery: Option<Discovery>,
}
impl StaffDeckController {
    pub(crate) async fn load(
        profile: JsValue,
        http: Arc<dyn JsonTransport>,
    ) -> Result<Arc<Self>, HostError> {
        let path = field(&profile, "definitionsPath")
            .as_string()
            .ok_or_else(|| {
                error(
                    "SOP_DEFINITIONS_INVALID",
                    "SOP definitionsPath is unavailable.",
                )
            })?
            .to_utf8()
            .map_err(|_| {
                error(
                    "SOP_DEFINITIONS_INVALID",
                    "SOP definitionsPath cannot be represented by this host.",
                )
            })?;
        let bundle = definitions::load(PathBuf::from(path.clone())).await?;
        let selected = field(&profile, "defaultSopId").as_string().ok_or_else(|| {
            error(
                "SOP_DEFINITIONS_INVALID",
                "SOP defaultSopId is unavailable.",
            )
        })?;
        if !definitions::contains(&bundle, selected) {
            return Err(error(
                "SOP_DEFINITIONS_INVALID",
                format!(
                    "StaffDeck SOP defaultSopId '{}' is not present in {path}.",
                    String::from_utf16_lossy(selected.units())
                )
                .as_str(),
            ));
        }
        let state = field(&profile, "stateRoot")
            .as_string()
            .ok_or_else(|| error("SOP_DEFINITIONS_INVALID", "SOP stateRoot is unavailable."))?
            .to_utf8()
            .map_err(|_| {
                error(
                    "SOP_DEFINITIONS_INVALID",
                    "SOP stateRoot cannot be represented by this host.",
                )
            })?;
        let client = Arc::new(SopClient::new(
            display(field(&profile, "endpoint")),
            field(&profile, "manifestPath")
                .as_string()
                .map(|s| String::from_utf16_lossy(s.units())),
            profile
                .get("implementationId")
                .filter(|v| !v.is_undefined())
                .map(|_| {
                    crate::metadata::object([
                        (
                            "implementationId",
                            field(&profile, "implementationId").clone(),
                        ),
                        ("contract", field(&profile, "contract").clone()),
                        ("transport", field(&profile, "transport").clone()),
                    ])
                }),
            field(&profile, "timeoutMs").as_number(),
            http.clone(),
        ));
        let discovery = Discovery::from_profile(&profile, http);
        Ok(Arc::new(Self {
            profile,
            bundle,
            store: Arc::new(DiskSopStore::new(PathBuf::from(state).join("sessions"))),
            client,
            discovery,
        }))
    }
    pub(crate) fn require_tools(
        &self,
        tools: &[Arc<dyn crate::host::modules::capability::ToolDefinition>],
    ) -> Result<(), HostError> {
        let selected = field(&self.profile, "defaultSopId")
            .as_string()
            .expect("validated default SOP");
        let mut required = vec![];
        if let JsValue::Array(rows) = field(&self.bundle, "sops")
            && let Some(row) = rows
                .iter()
                .find(|r| definitions::id(r).as_ref() == Some(selected))
                && let JsValue::Array(nodes) = field(field(row, "content"), "nodes") {
                    for node in nodes {
                        for action in strings(field(node, "allowed_actions")) {
                            if let Some(name) =
                                String::from_utf16_lossy(action.units()).strip_prefix("call_tool:")
                            {
                                let name = crate::metadata::js_trim(name);
                                if !name.is_empty() && !required.iter().any(|v: &String| v == name)
                                {
                                    required.push(name.to_owned());
                                }
                            }
                        }
                    }
                }
        let missing = required
            .into_iter()
            .filter(|name| {
                !tools.iter().any(|t| {
                    t.fields()
                        .get("name")
                        .and_then(JsValue::as_string)
                        .is_some_and(|v| v.equals_str(name))
                })
            })
            .collect::<Vec<_>>();
        if missing.is_empty() {
            Ok(())
        } else {
            Err(error(
                "SOP_REQUIRED_TOOL_UNAVAILABLE",
                format!(
                    "StaffDeck SOP '{}' requires unavailable PilotDeck tools: {}.",
                    String::from_utf16_lossy(selected.units()),
                    missing.join(", ")
                )
                .as_str(),
            ))
        }
    }
    async fn select(
        &self,
        session: &JsString,
        messages: &[JsValue],
        signal: &crate::host::Cancellation,
    ) -> Result<Option<JsString>, HostError> {
        let state = self.store.load(session).await.map_err(store_error)?;
        let persisted = state
            .as_ref()
            .and_then(|s| {
                field(s, "state")
                    .get("selected_skill_id")
                    .or_else(|| field(s, "state").get("active_skill_id"))
            })
            .filter(|v| crate::runtime::turn::submit::truthy(v));
        let selected = if let Some(id) = persisted {
            Some(id.as_string().cloned().ok_or_else(|| {
                error(
                    "SOP_STATE_INVALID",
                    "Persisted SOP selection must be a string.",
                )
            })?)
        } else if let Some(discovery) = &self.discovery {
            discovery
                .route(
                    latest_user(messages).as_str().into(),
                    session.clone(),
                    signal,
                )
                .await?
        } else {
            field(&self.profile, "defaultSopId").as_string().cloned()
        };
        if let Some(id) = &selected
            && !definitions::contains(&self.bundle, id) {
                return Err(error("SOP_DISCOVERY_SELECTION_UNAVAILABLE",format!("StaffDeck SOP discovery selected '{}', but it is not present in the PilotDeck bundle.",String::from_utf16_lossy(id.units())).as_str()));
            }
        Ok(selected)
    }
}
pub(super) struct RunState {
    pub store: Arc<DiskSopStore>,
    pub client: Arc<SopClient>,
    pub bundle: JsValue,
    pub selected: JsString,
    pub session: JsString,
    pub identity: ExecutionIdentity,
    pub deadline: Option<JsString>,
    pub signal: crate::host::Cancellation,
    pub user: String,
    pub submission: Mutex<Option<JsValue>>,
    pub saw_tool_error: AtomicBool,
    pub collect_checked: AtomicBool,
    profile: JsValue,
    model_validate: ModelValidator,
}
impl RunState {
    async fn prepare(&self, step_id: &JsValue) -> Result<(JsValue, Option<JsObject>), HostError> {
        let persisted = self
            .store
            .load_or_create(&self.session, self.bundle.clone(), self.selected.clone())
            .await
            .map_err(store_error)?;
        if matches!(field(field(&persisted,"state"),"status").as_string(),Some(s)if ["completed","handoff","blocked","waiting_external_task"].iter().any(|v|s.equals_str(v)))
        {
            return Ok((JsValue::Undefined, None));
        }
        let session = String::from_utf16_lossy(self.session.units());
        let turn = String::from_utf16_lossy(self.identity.turn_id.units());
        let id = if step_id.is_undefined() {
            "0".into()
        } else {
            display(step_id)
        };
        let request = format!("sop.prepare:{session}:{turn}:{id}");
        let context = crate::metadata::object([
            ("runId", format!("sop:{session}:{turn}").as_str().into()),
            (
                "operationId",
                format!("sop.prepare:{session}:{turn}").as_str().into(),
            ),
            ("requestId", request.as_str().into()),
            ("sessionId", JsValue::String(self.session.clone())),
            ("turnId", JsValue::String(self.identity.turn_id.clone())),
            ("idempotencyKey", request.as_str().into()),
            ("expectedRevision", field(&persisted, "revision").clone()),
        ]);
        let signal = crate::runtime::network::Cancellation::new();
        let prepared = tokio::select! {biased;_=self.signal.cancelled()=>{signal.abort();return Err(cancelled());},v=self.client.prepare(field(&persisted,"bundle").clone(),field(&persisted,"state").clone(),Some(&context),&signal)=>v.map_err(client_error)?};
        let mut step = without(
            field(&prepared, "step"),
            &["optionalKnowledge", "optionalKnowledgeUnavailable"],
        );
        if field(field(&step, "node"), "type") == &JsValue::from("knowledge_query") {
            let mut tools = strings(field(&step, "requiredToolNames"));
            if !tools.iter().any(|s| s.equals_str("knowledge_query")) {
                tools.push("knowledge_query".into());
            }
            set(
                &mut step,
                "requiredToolNames",
                JsValue::Array(tools.into_iter().map(JsValue::String).collect()),
            );
        }
        let sop = field(&step, "skillId").as_string().expect("validated step");
        let node = field(&step, "nodeId").as_string().expect("validated step");
        let configured = definitions::node(field(&persisted, "bundle"), sop, node);
        let mode = step
            .get("contextMode")
            .or_else(|| configured.as_ref().and_then(|n| n.get("contextMode")))
            .or_else(|| self.profile.get("contextMode"))
            .and_then(JsValue::as_string)
            .filter(|s| s.equals_str("inherit") || s.equals_str("new_session"))
            .cloned()
            .unwrap_or_else(|| "new_session".into());
        let model = configured
            .as_ref()
            .and_then(|n| n.get("model"))
            .and_then(definitions::parse_model);
        if let Some(model) = &model {
            (self.model_validate)(model.clone()).await.map_err(|e| {
                let code = e
                    .module_error
                    .as_ref()
                    .and_then(|m| m.code.as_ref())
                    .map(|s| String::from_utf16_lossy(s.units()))
                    .unwrap_or_else(|| "SOP_NODE_MODEL_UNAVAILABLE".into());
                error(
                    &code,
                    format!(
                        "{code}: SOP node '{}' model '{}/{}': {}",
                        String::from_utf16_lossy(node.units()),
                        display(model.get("provider").unwrap()),
                        display(model.get("model").unwrap()),
                        e.message
                    )
                    .as_str(),
                )
            })?;
        }
        set(&mut step, "contextMode", JsValue::String(mode));
        self.store
            .replace(
                &self.session,
                field(&persisted, "bundle").clone(),
                field(&prepared, "state").clone(),
                field(&persisted, "revision").as_number(),
            )
            .await
            .map_err(store_error)?;
        Ok((step, model))
    }
}
#[derive(Clone)]
pub(super) struct StepBinding {
    pub run: Arc<RunState>,
    current: Arc<Mutex<JsValue>>,
    preflight: Arc<AtomicBool>,
    correction: bool,
}
impl StepBinding {
    pub fn step(&self) -> JsValue {
        self.current
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
    pub async fn instruction(&self, step_id: &JsValue) -> Result<JsString, HostError> {
        if !self.preflight.swap(false, Ordering::SeqCst) {
            let (next, _) = self.run.prepare(step_id).await?;
            *self.current.lock().unwrap_or_else(|e| e.into_inner()) = next;
        }
        let step = self.step();
        let owner = self
            .run
            .store
            .model_context(&self.run.session)
            .await
            .map_err(store_error)?;
        let mut instruction = render(&step, owner.as_ref());
        if self.correction {
            instruction.push_str("\nSOP_STEP_RESULT_REQUIRED: Your previous response did not submit the current SOP node result. Text cannot create an approval or missing-field wait. Follow the current node contract and call submit_step_result with your actual result before ending. Do not claim a persisted wait or completion without a successful tool receipt.");
        }
        Ok(instruction.as_str().into())
    }
}
impl HostLoopController for StaffDeckController {
    fn validate_tools<'a>(
        &'a self,
        catalog: Arc<dyn crate::host::modules::capability::ToolCatalog>,
    ) -> SopFuture<'a, Result<(), HostError>> {
        Box::pin(async move { self.require_tools(&catalog.list().await?) })
    }

    fn run<'a>(
        &'a self,
        input: SopLoopInput,
        native: &'a dyn SopNodeExecutor,
    ) -> SopFuture<'a, Result<ExecuteResult, HostError>> {
        Box::pin(async move {
            let result:Result<ExecuteResult,HostError>=async {
  let identity=input.request.identity.clone();let session=&identity.session_id;let mut visible=input.visible_messages.clone();
  if let Some(delivery)=self.store.reply_delivery(session).await.map_err(store_error)?{let state=self.store.load(session).await.map_err(store_error)?;let finish=!state.as_ref().is_some_and(|s|active(field(s,"state")));let result=field(&delivery,"result").clone();let message=reply(&result,&if finish{identity.turn_id.clone()}else{format!("{}:recovered-step",String::from_utf16_lossy(identity.turn_id.units())).as_str().into()});if field(&delivery,"phase")==&JsValue::from("pending")||field(&delivery,"turnId")!=&JsValue::String(identity.turn_id.clone()){native.append_parent_reply(message.clone()).await?;self.store.mark_reply_durable(session,field(&delivery,"turnId").as_string().expect("validated delivery").clone(),identity.turn_id.clone()).await.map_err(store_error)?;native.emit_parent(assistant(&identity,message.clone())).await?;}append_once(&mut visible,message.clone());self.store.clear_reply_delivery(session,identity.turn_id.clone()).await.map_err(store_error)?;if finish{return Ok(aggregate(recovery(&identity,result,message),visible));}}
  let selected=self.select(session,&visible,&input.signal).await?;
  let Some(selected)=selected else{let outcome=native.run(SopNodePlan{continuation:false,context:SopNodeContext::Inherit,request:input.request,instruction:"".into(),model_override:None,step:JsValue::Undefined,binding:None}).await?;native.finish_node(Some(&outcome)).await?;return Ok(aggregate(outcome.result,outcome.messages));};
  let run=Arc::new(RunState{store:self.store.clone(),client:self.client.clone(),bundle:self.bundle.clone(),selected,session:session.clone(),identity:identity.clone(),deadline:input.request.operation_deadline.clone(),signal:input.signal.clone(),user:latest_user(&visible),submission:Mutex::new(None),saw_tool_error:AtomicBool::new(false),collect_checked:AtomicBool::new(false),profile:self.profile.clone(),model_validate:native.model_validator()?});
  let mut corrections=HashSet::new();let mut total_turns=0.0;let mut total_spent=0.0;let mut usage=JsObject::new();let mut denials=vec![];let mut correcting=false;let mut continued=input.request.clone();let mut correction_result:Option<JsValue>=None;
  let mut completed=loop {
   if input.signal.is_cancelled(){return Err(cancelled());}
   let(step,model)=run.prepare(&JsValue::Number(0.0)).await?;
   let binding=StepBinding{run:run.clone(),current:Arc::new(Mutex::new(step.clone())),preflight:Arc::new(AtomicBool::new(true)),correction:correcting};
   let sop=field(&step,"skillId").as_string().cloned().unwrap_or_else(||run.selected.clone());let node=field(&step,"nodeId").as_string().cloned().unwrap_or_else(||"".into());
   let isolated=field(&step,"contextMode")==&JsValue::from("new_session");let context=if isolated{SopNodeContext::Isolated{sop:sop.clone(),node:node.clone()}}else{SopNodeContext::Inherit};
   let mut request=continued.clone();request.identity.operation_id=format!("{}:sop-node:{}:{}{}",String::from_utf16_lossy(identity.operation_id.units()),encode(&sop)?,encode(&node)?,if correcting{":correction"}else{""}).as_str().into();request.payload.insert("messages".into(),JsValue::Array(visible.clone()));request.payload.insert("stopOnStructuredOutput".into(),JsValue::Bool(true));let mut agent=request.payload.get("agent").and_then(JsValue::as_object).cloned().unwrap_or_default();agent.insert("stopOnStructuredOutput".into(),JsValue::Bool(true));request.payload.insert("agent".into(),JsValue::Object(agent));
   let instruction=if step.is_undefined(){String::new()}else{render(&step,self.store.model_context(session).await.map_err(store_error)?.as_ref())};
   let mut outcome=native.run(SopNodePlan{continuation:correcting,context,request,instruction:instruction.as_str().into(),model_override:model,step:step.clone(),binding:(!step.is_undefined()).then(||Arc::new(binding)as Arc<dyn SopRunBinding>)}).await?;
   let result=&outcome.result;total_turns+=field(result,"turns").as_number().unwrap_or(0.0);total_spent+=field(field(result,"budget"),"turnSpentUsd").as_number().unwrap_or(0.0);if let Some(o)=field(result,"usage").as_object(){for(k,v)in o.own_entries(){if let Some(n)=v.as_number(){let old=usage.get_key(k).and_then(JsValue::as_number).unwrap_or(0.0);usage.insert(k.clone(),JsValue::Number(old+n));}}}if let JsValue::Array(items)=field(result,"permissionDenials"){denials.extend(items.clone());}if !isolated{visible=outcome.messages.clone();}
   let state=self.store.load(session).await.map_err(store_error)?;let active=state.as_ref().is_some_and(|s|active(field(s,"state")));let stop=field(result,"stopReason")==&JsValue::from("completed");
   let max=continued.payload.get("maxTurns").and_then(JsValue::as_number).or_else(||continued.payload.get("agent").and_then(|a|a.get("maxTurns")).and_then(JsValue::as_number));
   if active&&stop&&max.is_some_and(|max|field(result,"turns").as_number().unwrap_or(0.0)>=max){let mut result=result.clone();finish(native,&mut outcome,&mut correction_result).await?;set(&mut result,"type","max_turns".into());set(&mut result,"stopReason","max_turns".into());break result;}
   if active&&crate::runtime::turn::submit::truthy(field(result,"structuredOutput")){finish(native,&mut outcome,&mut correction_result).await?;correcting=false;continue_request(&mut continued,&outcome.result,outcome.messages);continue;}
   let has_submission=run.submission.lock().unwrap_or_else(|e|e.into_inner()).is_some();
   if active&&stop&&!has_submission&&!run.saw_tool_error.load(Ordering::SeqCst){let key=(sop,node);if !corrections.insert(key){return Err(error("SOP_STEP_RESULT_REQUIRED","SOP_STEP_RESULT_REQUIRED: model ended without submitting the active node result"));}correcting=true;correction_result=Some(correction_result.take().map_or_else(||outcome.result.clone(),|previous|merge_result(previous,outcome.result.clone())));continue_request(&mut continued,&outcome.result,outcome.messages);continue;}
   finish(native,&mut outcome,&mut correction_result).await?;break outcome.result;
  };
  set(&mut completed,"turns",JsValue::Number(total_turns));set(&mut completed,"usage",JsValue::Object(usage));set(&mut completed,"permissionDenials",JsValue::Array(denials));if let Some(JsValue::Object(budget))=completed.get("budget"){let mut budget=budget.clone();budget.insert("turnSpentUsd".into(),JsValue::Number(total_spent));set(&mut completed,"budget",JsValue::Object(budget));}
  let submission=run.submission.lock().unwrap_or_else(|e|e.into_inner()).clone();if let Some(result)=submission{let message=reply(&result,&identity.turn_id);native.append_parent_reply(message.clone()).await?;self.store.mark_reply_durable(session,identity.turn_id.clone(),identity.turn_id.clone()).await.map_err(store_error)?;native.emit_parent(assistant(&identity,message.clone())).await?;append_once(&mut visible,message.clone());set(&mut completed,"finalMessage",message);set(&mut completed,"structuredOutput",crate::metadata::object([("sop",result)]));self.store.clear_reply_delivery(session,identity.turn_id.clone()).await.map_err(store_error)?;}
  set(&mut completed,"sessionId",JsValue::String(identity.session_id.clone()));set(&mut completed,"turnId",JsValue::String(identity.turn_id.clone()));Ok(aggregate(completed,visible))
 }.await;
            let cleanup = native.finish_node(None).await;
            match (result, cleanup) {
                (Ok(value), Ok(())) => Ok(value),
                (Err(error), _) => Err(error),
                (Ok(_), Err(error)) => Err(error),
            }
        })
    }
}
async fn finish(
    native: &dyn SopNodeExecutor,
    outcome: &mut SopNodeOutcome,
    previous: &mut Option<JsValue>,
) -> Result<(), HostError> {
    if let Some(previous) = previous.take() {
        outcome.result = merge_result(previous, outcome.result.clone());
    }
    native.finish_node(Some(outcome)).await
}
fn merge_result(previous: JsValue, mut current: JsValue) -> JsValue {
    {
        let field_name = "turns";
        let n = field(&previous, field_name).as_number().unwrap_or(0.0)
            + field(&current, field_name).as_number().unwrap_or(0.0);
        set(&mut current, field_name, JsValue::Number(n));
    }
    let mut usage = object(field(&current, "usage"));
    if let Some(o) = field(&previous, "usage").as_object() {
        for (k, v) in o.own_entries() {
            if let Some(n) = v.as_number() {
                let total = n + usage.get_key(k).and_then(JsValue::as_number).unwrap_or(0.0);
                usage.insert(k.clone(), JsValue::Number(total));
            }
        }
    }
    set(&mut current, "usage", JsValue::Object(usage));
    let mut denials = match field(&previous, "permissionDenials") {
        JsValue::Array(a) => a.clone(),
        _ => vec![],
    };
    if let JsValue::Array(a) = field(&current, "permissionDenials") {
        denials.extend(a.clone());
    }
    set(&mut current, "permissionDenials", JsValue::Array(denials));
    set(
        &mut current,
        "startedAt",
        field(&previous, "startedAt").clone(),
    );
    if let Some(b) = field(&current, "budget").as_object() {
        let mut b = b.clone();
        b.insert(
            "turnSpentUsd".into(),
            JsValue::Number(
                field(field(&previous, "budget"), "turnSpentUsd")
                    .as_number()
                    .unwrap_or(0.0)
                    + field(field(&current, "budget"), "turnSpentUsd")
                        .as_number()
                        .unwrap_or(0.0),
            ),
        );
        set(&mut current, "budget", JsValue::Object(b));
    }
    current
}
fn continue_request(request: &mut ExecuteRequest, result: &JsValue, messages: Vec<JsValue>) {
    request
        .payload
        .insert("messages".into(), JsValue::Array(messages));
    let spent = field(field(result, "budget"), "turnSpentUsd")
        .as_number()
        .unwrap_or(0.0);
    let turns = field(result, "turns").as_number().unwrap_or(0.0);
    for outer in [false, true] {
        let mut fields = if outer {
            request.payload.clone()
        } else {
            request
                .payload
                .get("agent")
                .and_then(JsValue::as_object)
                .cloned()
                .unwrap_or_default()
        };
        for (k, used) in [("maxTurns", turns), ("maxBudgetUsd", spent)] {
            if let Some(value) = fields.get(k).and_then(JsValue::as_number) {
                fields.insert(k.into(), JsValue::Number((value - used).max(0.0)));
            }
        }
        let prior = fields
            .get("initialTaskBudgetSpentUsd")
            .and_then(JsValue::as_number)
            .unwrap_or(0.0);
        fields.insert(
            "initialTaskBudgetSpentUsd".into(),
            JsValue::Number(prior + spent),
        );
        if outer {
            request.payload = fields;
        } else {
            request
                .payload
                .insert("agent".into(), JsValue::Object(fields));
        }
    }
}
fn aggregate(result: JsValue, messages: Vec<JsValue>) -> ExecuteResult {
    ExecuteResult::Terminal(crate::metadata::object([(
        "payload",
        crate::metadata::object([("result", result), ("messages", JsValue::Array(messages))]),
    )]))
}
fn reply(result: &JsValue, turn: &JsString) -> JsValue {
    crate::metadata::object([
        ("role", "assistant".into()),
        (
            "content",
            JsValue::Array(vec![crate::metadata::object([
                ("type", "text".into()),
                ("text", field(result, "replyFragment").clone()),
            ])]),
        ),
        (
            "metadata",
            crate::metadata::object([
                ("purpose", "staffdeck_sop_reply".into()),
                (
                    "transientId",
                    format!(
                        "staffdeck-sop-reply:{}",
                        String::from_utf16_lossy(turn.units())
                    )
                    .as_str()
                    .into(),
                ),
            ]),
        ),
    ])
}
fn append_once(messages: &mut Vec<JsValue>, message: JsValue) {
    if !messages.iter().any(|m| {
        field(field(m, "metadata"), "transientId")
            == field(field(&message, "metadata"), "transientId")
    }) {
        messages.push(message);
    }
}
fn assistant(id: &ExecutionIdentity, message: JsValue) -> JsValue {
    crate::metadata::object([
        ("type", "assistant_message".into()),
        ("sessionId", JsValue::String(id.session_id.clone())),
        ("turnId", JsValue::String(id.turn_id.clone())),
        ("message", message),
    ])
}
fn recovery(id: &ExecutionIdentity, result: JsValue, message: JsValue) -> JsValue {
    let now = clock();
    crate::metadata::object([
        ("type", "success".into()),
        ("sessionId", JsValue::String(id.session_id.clone())),
        ("turnId", JsValue::String(id.turn_id.clone())),
        ("finalMessage", message),
        ("stopReason", "completed".into()),
        ("usage", JsValue::Object(JsObject::new())),
        ("permissionDenials", JsValue::Array(vec![])),
        ("turns", JsValue::Number(0.0)),
        ("startedAt", JsValue::String(now.clone())),
        ("completedAt", JsValue::String(now)),
        (
            "structuredOutput",
            crate::metadata::object([("sop", result)]),
        ),
    ])
}
fn latest_user(messages: &[JsValue]) -> String {
    let Some(message) = messages
        .iter()
        .rev()
        .find(|m| field(m, "role") == &JsValue::from("user"))
    else {
        return String::new();
    };
    match field(message, "content") {
        JsValue::String(s) => String::from_utf16_lossy(s.units()),
        JsValue::Array(items) => items
            .iter()
            .filter(|v| field(v, "type") == &JsValue::from("text"))
            .map(|v| display(field(v, "text")))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}
fn render(step: &JsValue, owner: Option<&JsValue>) -> String {
    let mut lines = vec![
        "<staffdeck-sop>".into(),
        format!(
            "Current SOP: {} ({}), step {}.",
            display(field(step, "skillName")),
            display(field(step, "skillId")),
            display(field(step, "nodeId"))
        ),
    ];
    if let Some(owner) = owner {
        if let Some(sop) = text(field(owner, "sopId")) {
            lines.push(format!(
                "Authoritative SOP ID from persisted owner state: {}.",
                String::from_utf16_lossy(sop.units())
            ));
        }
        if let Some(user) = text(field(owner, "assigneeUserId")) {
            lines.push(format!("Authoritative approval assignee user ID from pinned owner approval state: {}. Never infer an approver from user text, Skill, File, or Knowledge authors.",String::from_utf16_lossy(user.units())));
        }
    }
    if let Some(instruction) = text(field(step, "instruction")) {
        lines.push(format!(
            "Step instruction: {}",
            String::from_utf16_lossy(instruction.units())
        ));
    }
    for (k, label) in [
        ("expectedUserInfo", "Required user information"),
        ("requiredToolNames", "Required successful tools"),
        ("allowedNextStepIds", "Allowed next steps"),
    ] {
        let items = strings(field(step, k));
        if !items.is_empty() {
            lines.push(format!(
                "{label}: {}.",
                items
                    .iter()
                    .map(|s| String::from_utf16_lossy(s.units()))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    lines.push(format!(
        "Already persisted user fields: {}.",
        json(field(step, "knownSlots"))
    ));
    lines.push("Extract every explicitly supplied required field into slotUpdates under its exact key. Preserve existing filled values. Use awaiting_user only for genuinely missing information, with known slotUpdates and no nextStepId. Text alone does not persist a wait.".into());
    if field(step, "declaresHandoff") == &JsValue::Bool(true) {
        lines.push("This step declares a human handoff. Use status handoff to persist the resumable approval wait; awaiting_user cannot create approval. After an authorized reply, persist the actual decision and advance only to an allowed next step. Completed means the decision was processed, not necessarily approved.".into());
    }
    if field(step, "isTerminal") == &JsValue::Bool(true) {
        lines.push("This is the active final SOP node. Its required fields and approval contract still apply. Put the final answer in replyFragment and call submit_step_result; do not claim completion before the tool succeeds or emit a plain-text draft as the completed SOP.".into());
    } else {
        lines.push("A completed submission closes only this node. A successor must be prepared and submitted separately. Select nextStepId only from the declared transitions.".into());
    }
    if let JsValue::Array(transitions) = field(step, "transitions") {
        for t in transitions {
            lines.push(format!(
                "Transition {}: condition={}, priority={}, target={}",
                display(field(t, "nextStepId")),
                display(field(t, "condition")),
                display(field(t, "priority")),
                json(field(t, "targetStep"))
            ));
        }
    }
    lines.push("When this step has a result, call submit_step_result exactly once, after all required tools have returned. A question or approval wait is also a persisted step result. Do not claim a tool succeeded before its result is in the conversation.".into());
    lines.push("</staffdeck-sop>".into());
    lines.join("\n")
}
