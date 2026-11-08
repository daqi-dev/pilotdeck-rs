//! Observer-only projection and report body; the existing fork/owner does all
//! execution, cancellation, lifecycle and generation ownership.
use crate::runtime::{
    background::{StartBackgroundSubagentInput, SubagentFuture, SubagentKind},
    memory::js_trim_units,
    tools::ToolCancellation,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{future::Future, pin::Pin, sync::Arc};

const MAX_EVENTS: usize = 64;
const MAX_TEXT: usize = 12_000;
const TRUNCATED: &str = "\n\n[observer digest truncated]";

// Keep only the digest prefix, but retain its exact logical length and final
// non-whitespace position. This also handles a trailing empty report exactly:
// crossing the raw cap with a final newline does not imply trimmed overflow.
#[derive(Clone, Default)]
struct Text {
    head: Vec<u16>,
    len: usize,
    last: usize,
}
impl Text {
    fn units(&mut self, value: &[u16]) {
        if let Some(index) = value.iter().rposition(|unit| !js_trim_units(std::slice::from_ref(unit)).is_empty()) {
            self.last = self.len.saturating_add(index).saturating_add(1);
        }
        self.len = self.len.saturating_add(value.len());
        self.head.extend_from_slice(&value[..value.len().min(MAX_TEXT.saturating_sub(self.head.len()))]);
    }
    fn string(&mut self, value: &str) { self.units(&value.encode_utf16().collect::<Vec<_>>()); }
    fn append(&mut self, value: &Text) {
        if value.last != 0 { self.last = self.len.saturating_add(value.last); }
        self.len = self.len.saturating_add(value.len);
        self.head.extend_from_slice(&value.head[..value.head.len().min(MAX_TEXT.saturating_sub(self.head.len()))]);
    }
    fn coerced(&mut self, value: Option<&JsValue>) -> Result<(), JsString> {
        match value {
            None => self.string("undefined"),
            Some(JsValue::String(value)) => self.units(value.units()),
            Some(value) => self.units(&crate::js_value::to_string_units_checked(value).map_err(|error| JsString::from(match error { crate::js_value::ToStringError::NotPrimitive => "Cannot convert object to primitive value", crate::js_value::ToStringError::StackOverflow => "Maximum call stack size exceeded" }))?),
        }
        Ok(())
    }
    fn finish(mut self) -> JsString {
        if self.last <= MAX_TEXT {
            self.head.truncate(self.last);
        } else {
            self.head.truncate(MAX_TEXT - 72);
            self.head.extend(TRUNCATED.encode_utf16());
        }
        JsString::from_utf16(self.head)
    }
}
fn truncate(value: &JsString, max: usize) -> JsString {
    let units = js_trim_units(value.units());
    if units.len() <= max { return JsString::from_utf16(units.to_vec()); }
    let mut kept = units[..max.saturating_sub(72)].to_vec();
    kept.extend(TRUNCATED.encode_utf16());JsString::from_utf16(kept)
}
fn required_text(value: Option<&JsValue>) -> Result<&JsString, JsString> {
    match value {
        Some(JsValue::String(value)) => Ok(value),
        None | Some(JsValue::Undefined) => Err("Cannot read properties of undefined (reading 'trim')".into()),
        Some(JsValue::Null) => Err("Cannot read properties of null (reading 'trim')".into()),
        _ => Err("value.trim is not a function".into()),
    }
}
fn is(value: Option<&JsValue>, expected: &str) -> bool {
    value.and_then(JsValue::as_string).is_some_and(|value| value.equals_str(expected))
}
fn line(event: &JsObject) -> Result<Option<Text>, JsString> {
    let mut out = Text::default();
    if is(event.get("type"), "model_request_started") {
        out.string("model request: ");out.coerced(event.get("provider"))?;out.string("/");out.coerced(event.get("model"))?;
    } else if is(event.get("type"), "tool_calls_detected") {
        let calls = match event.get("calls") {
            Some(JsValue::Array(calls)) => calls,
            None | Some(JsValue::Undefined) => return Err("Cannot read properties of undefined (reading 'map')".into()),
            Some(JsValue::Null) => return Err("Cannot read properties of null (reading 'map')".into()),
            _ => return Err("event.calls.map is not a function".into()),
        };
        let mut names = Text::default();
        for (index, call) in calls.iter().enumerate() {
            if matches!(call, JsValue::Null) { return Err("Cannot read properties of null (reading 'name')".into()); }
            if matches!(call, JsValue::Undefined) { return Err("Cannot read properties of undefined (reading 'name')".into()); }
            if index != 0 { names.string(", "); }
            if let Some(value) = call.get("name").filter(|v| !matches!(v, JsValue::Null | JsValue::Undefined)) { names.coerced(Some(value))?; }
        }
        out.string("tool calls: ");if names.len == 0 { out.string("none"); } else { out.append(&names); }
    } else if is(event.get("type"), "tool_result") {
        let result = match event.get("result") {
            None | Some(JsValue::Undefined) => return Err("Cannot read properties of undefined (reading 'toolName')".into()),
            Some(JsValue::Null) => return Err("Cannot read properties of null (reading 'toolName')".into()),
            Some(value) => value,
        };
        out.string("tool result: ");out.coerced(result.get("toolName"))?;out.string(" (");out.string(if is(result.get("type"), "success") {"success"} else {"error"});out.string(")");
    } else if is(event.get("type"), "assistant_message") {
        out.string("assistant response emitted");
    } else if is(event.get("type"), "warning") {
        out.string("warning: ");out.units(truncate(required_text(event.get("code"))?,120).units());
    } else if is(event.get("type"), "agent_status") {
        out.string("agent status: ");out.units(truncate(required_text(event.get("event"))?,120).units());
    } else { return Ok(None); }
    Ok(Some(out))
}

/// First 64 raw events, before filtering. Only bounded digest prefixes are
/// retained; assistant/tool bodies never become extra observer-owned payloads.
/// Formatting errors are deferred until preparation, after the observed fork.
#[derive(Clone, Default)]
pub(super) struct ObserverActivity {
    seen: usize,
    lines: Vec<Result<Text, JsString>>,
}
impl ObserverActivity {
    pub(super) fn record(&mut self, event: &JsObject) {
        if self.seen == MAX_EVENTS { return; }
        self.seen += 1;
        match line(event) { Ok(Some(line)) => self.lines.push(Ok(line)), Ok(None) => (), Err(error) => self.lines.push(Err(error)) }
    }
    pub(super) fn take(&mut self) -> Self { std::mem::take(self) }
}
#[derive(Clone)]
pub(super) struct ObserverOutcome {
    pub success: bool,
    pub markdown: Option<JsString>,
    pub error: Option<JsString>,
}
pub(super) struct ObserverInput {
    pub session_id: JsString,
    pub parent_turn_id: JsString,
    pub observed_subagent_id: JsString,
    pub observed_subagent_type: JsString,
    pub observer_subagent_id: JsString,
    pub definition: JsObject,
    pub directive: JsString,
    pub activity: ObserverActivity,
    pub observer_message: Option<JsString>,
    pub timeout_ms: Option<f64>,
    pub outcome: ObserverOutcome,
}
#[derive(Clone)]
pub(super) struct ObserverForkRequest {
    pub definition_id: JsString,
    pub definition: JsObject,
    pub directive: JsString,
    pub subagent_id: JsString,
    pub timeout_ms: Option<f64>,
    pub suppress_auto_observer: bool,
    /// Host-only identity: the real child scope must consume this to disable
    /// all MCP owner preparation, not merely remove model-visible tool names.
    pub native_observer: bool,
}
pub(super) type ObserverFuture = Pin<Box<dyn Future<Output = Result<JsString, JsString>> + Send>>;
pub(super) type ObserverFork = Box<dyn FnOnce(ObserverForkRequest, ToolCancellation) -> ObserverFuture + Send>;
pub(super) type ObserverEmitter = Arc<dyn Fn(JsObject) -> Result<(), JsString> + Send + Sync>;
pub(super) type ObserverClock = Arc<dyn Fn() -> f64 + Send + Sync>;

fn definition(mut value: JsObject) -> JsObject {
    value.insert("allowedTools".into(),JsValue::Array(vec![]));value.insert("disallowedTools".into(),JsValue::Array(vec![]));
    value.insert("isReadOnly".into(),true.into());value.insert("permissionMode".into(),"plan".into());
    for field in ["mcpServers","skills"] { value.insert(field.into(),JsValue::Undefined); }
    value.insert("memory".into(),"disabled".into());
    for field in ["initialPrompt","background","observer","observerMessage"] { value.insert(field.into(),JsValue::Undefined); }
    value
}
fn digest(input: &ObserverInput) -> Result<JsString, JsString> {
    let mut text = Text::default();
    text.string("You are an observer. Do not execute the observed task or attempt to change its outcome.\nReview this read-only activity digest and produce a concise report of risks, failures, or notable findings.\n\nObserved agent: ");
    text.units(input.observed_subagent_type.units());text.string("\nObserved directive: ");text.units(truncate(&input.directive,2000).units());text.string("\n\nActivity:\n");
    if input.activity.lines.is_empty() { text.string("- No observable model or tool activity was recorded."); }
    for (index, line) in input.activity.lines.iter().enumerate() {
        let line=line.as_ref().map_err(Clone::clone)?;if index!=0 {text.string("\n");}text.string("- ");text.append(line);
    }
    text.string("\n\nOutcome: ");text.string(if input.outcome.success {"completed"} else {"failed"});
    if let Some(value)=&input.outcome.markdown && !value.is_empty() {text.string("\nObserved final report:\n");text.units(truncate(value,4000).units());}
    if let Some(value)=&input.outcome.error && !value.is_empty() {text.string("\nObserved error: ");text.units(truncate(value,1000).units());}
    if let Some(value)=&input.observer_message {let trimmed=js_trim_units(value.units());if !trimmed.is_empty(){text.string("\n\n");text.units(trimmed);}}
    Ok(text.finish())
}
fn report(input: &ObserverInput, observer_type: &JsString, result: Result<JsString,JsString>, duration: f64) -> JsObject {
    let mut event = JsObject::new();
    for (key,value) in [("type","observer_report".into()),("sessionId",JsValue::String(input.session_id.clone())),("turnId",JsValue::String(input.parent_turn_id.clone())),("observedSubagentId",JsValue::String(input.observed_subagent_id.clone())),("observedSubagentType",JsValue::String(input.observed_subagent_type.clone())),("observerSubagentId",JsValue::String(input.observer_subagent_id.clone())),("observerSubagentType",JsValue::String(observer_type.clone()))] {event.insert(key.into(),value);}
    match result {Ok(value)=>{event.insert("success".into(),true.into());event.insert("report".into(),JsValue::String(value));},Err(error)=>{event.insert("success".into(),false.into());event.insert("error".into(),JsValue::String(error));}}
    event.insert("durationMs".into(),JsValue::Number(duration));event
}
/// The caller publishes this through its existing NativeForkOwner.start. The
/// injected fork must use the same real factory/lease/lifecycle and force plan,
/// no tools, no MCP preparation, disabled memory and no recursive observer.
pub(super) fn prepare_task(input: ObserverInput, fork: ObserverFork, emit: Option<ObserverEmitter>, now: ObserverClock) -> Result<StartBackgroundSubagentInput, JsString> {
    let observer_type = input.definition.get("id").and_then(JsValue::as_string).cloned().filter(|id|!id.is_empty()).ok_or_else(||JsString::from("Observer definition must have a nonempty id."))?;
    let request = ObserverForkRequest { definition_id:observer_type.clone(), definition:definition(input.definition.clone()), directive:digest(&input)?, subagent_id:input.observer_subagent_id.clone(), timeout_ms:input.timeout_ms, suppress_auto_observer:true, native_observer:true };
    Ok(StartBackgroundSubagentInput {
        kind:Some(SubagentKind::Observer),session_id:input.session_id.clone(),parent_turn_id:input.parent_turn_id.clone(),subagent_id:input.observer_subagent_id.clone(),subagent_type:observer_type.clone(),observed_subagent_id:Some(input.observed_subagent_id.clone()),
        run:Box::new(move |signal| -> SubagentFuture {Box::pin(async move {
            let started=now();
            let result=match fork(request,signal).await {
                Ok(markdown)=>match &emit {Some(emit)=>emit(report(&input,&observer_type,Ok(markdown),now()-started)),None=>Ok(())},
                Err(error)=>Err(error),
            };
            if let Err(error)=result {
                if let Some(emit)=&emit {emit(report(&input,&observer_type,Err(error.clone()),now()-started))?;}
                return Err(error);
            }
            Ok(())
        })}),
    })
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex,atomic::{AtomicUsize,Ordering}};
    fn value(parent:&JsValue,name:&str)->JsValue {parent.get(name).cloned().unwrap_or(JsValue::Undefined)}
    fn string(parent:&JsValue,name:&str)->Option<JsString>{parent.get(name).and_then(JsValue::as_string).cloned()}
    fn yes(parent:&JsValue,name:&str)->bool{parent.get(name)==Some(&JsValue::Bool(true))}
    fn array(value:&JsValue)->&[JsValue]{match value{JsValue::Array(v)=>v,_=>panic!("array expected")}}
    fn object(entries:impl IntoIterator<Item=(&'static str,JsValue)>)->JsObject {let mut out=JsObject::new();for(key,v)in entries{out.insert(key.into(),v);}out}
    fn encoded(value:&JsValue)->String {pilotdeck_protocol::stringify_json(value).unwrap().unwrap()}
    fn input(spec:&JsValue,id:JsString)->ObserverInput {
        let mut activity=ObserverActivity::default();
        if let Some(JsValue::Array(events))=spec.get("activity"){for event in events {activity.record(event.as_object().unwrap());}}
        let activity=activity.take();let outcome=value(spec,"outcome");
        ObserverInput {session_id:"parent-session".into(),parent_turn_id:"parent-turn".into(),observed_subagent_id:"observed-child".into(),observed_subagent_type:"research".into(),observer_subagent_id:id,definition:value(spec,"observerDefinition").as_object().unwrap().clone(),directive:string(spec,"directive").unwrap_or_else(||"Observed directive".into()),activity,observer_message:string(spec,"observerMessage"),timeout_ms:Some(1234.0),outcome:ObserverOutcome{success:yes(&outcome,"success"),markdown:string(&outcome,"markdown"),error:string(&outcome,"error")}}
    }
    #[tokio::test]
    async fn real_ts_twenty_seven_observer_digests_and_twenty_five_run_cases() {
        let oracle=pilotdeck_protocol::parse_json(include_str!("../../tests/fixtures/native-process/observer-oracle.json")).unwrap();let rows=value(&oracle,"rows");assert_eq!(array(&rows).len(),27);let mut replayed=0;let mut launcher_only=0;
        for row in array(&rows) {
            let id=string(row,"id").unwrap();let spec=value(row,"input");let observer_id=if yes(&spec,"noUuid") {"observer-random-fixture"}else{"observer-fixture"};let input=input(&spec,observer_id.into());
            assert_eq!(input.activity.seen as f64,row.get("collectedCount").unwrap().as_number().unwrap(),"{id:?}");assert_eq!(&digest(&input).unwrap(),row.get("digest").unwrap().as_string().unwrap(),"{id:?}");
            if yes(&spec,"missingLauncher")||spec.get("launchFailure").is_some(){launcher_only+=1;continue;}
            let calls=Arc::new(Mutex::new(Vec::new()));let recorded=calls.clone();let fork_error=string(&spec,"forkError").or_else(||string(&spec,"forkNonError"));
            let fork:ObserverFork=Box::new(move|request,signal|Box::pin(async move {
                assert!(request.native_observer&&request.suppress_auto_observer);assert!(!signal.is_aborted());
                let own=JsValue::Array(request.definition.own_entries().into_iter().map(|(key,v)|JsValue::Object(object([("key",JsValue::String(key.clone())),("isUndefined",JsValue::Bool(v.is_undefined()))]))).collect());
                recorded.lock().unwrap().push(JsValue::Object(object([("definitionId",JsValue::String(request.definition_id)),("directive",JsValue::String(request.directive)),("subagentId",JsValue::String(request.subagent_id)),("timeoutMs",request.timeout_ms.map_or(JsValue::Undefined,JsValue::Number)),("suppressAutoObserver",JsValue::Bool(request.suppress_auto_observer)),("definitionOverride",JsValue::Object(request.definition)),("abortSignalPresent",JsValue::Bool(true)),("definitionOwn",own)])));
                match fork_error {Some(error)=>Err(error),None=>Ok("Observer report: fixture".into())}
            }));
            let events=Arc::new(Mutex::new(Vec::new()));let emitted=events.clone();let emit_count=Arc::new(AtomicUsize::new(0));let attempts=emit_count.clone();let failure=string(&spec,"emitFailure");
            let emitter:Option<ObserverEmitter>=if yes(&spec,"noEmitter"){None}else{Some(Arc::new(move|event|{emitted.lock().unwrap().push(JsValue::Object(event));let attempt=attempts.fetch_add(1,Ordering::SeqCst);if failure.as_ref().is_some_and(|v|v.equals_str("all")||(v.equals_str("first")&&attempt==0)){Err("emitter failure".into())}else{Ok(())}}))};
            let clock_count=Arc::new(AtomicUsize::new(0));let ticks=clock_count.clone();let negative=yes(&spec,"negativeClock");let now:ObserverClock=Arc::new(move||{let n=ticks.fetch_add(1,Ordering::SeqCst)as f64;if negative{1000.0-100.0*n}else{1000.0+10.0*n}});
            let task=prepare_task(input,fork,emitter,now).unwrap();assert_eq!(task.kind,Some(SubagentKind::Observer));assert_eq!(task.observed_subagent_id,Some("observed-child".into()));assert_eq!(task.session_id,"parent-session".into());assert_eq!(task.parent_turn_id,"parent-turn".into());
            let expected_launch=value(row,"launches");let launch=array(&expected_launch).first().unwrap();assert_eq!(task.subagent_id,launch.get("observerSubagentId").unwrap().as_string().unwrap().clone());assert_eq!(task.subagent_type,launch.get("observerSubagentType").unwrap().as_string().unwrap().clone());
            let result=(task.run)(ToolCancellation::new()).await;let expected=value(row,"output");assert_eq!(result.is_ok(),yes(&expected,"ok"),"{id:?}");if let Err(error)=result {assert_eq!(&error,value(&expected,"error").get("message").unwrap().as_string().unwrap(),"{id:?}");}
            // The oracle records JSON projections; undefined own slots are
            // separately carried in definitionOwn instead of equating graphs.
            assert_eq!(encoded(&JsValue::Array(calls.lock().unwrap().clone())),encoded(&value(row,"forkCalls")),"{id:?}: fork JSON projection");assert_eq!(encoded(&JsValue::Array(events.lock().unwrap().clone())),encoded(&value(row,"events")),"{id:?}: report JSON projection");assert_eq!(clock_count.load(Ordering::SeqCst)as f64,row.get("clockCalls").unwrap().as_number().unwrap(),"{id:?}");replayed+=1;
        }
        assert_eq!((replayed,launcher_only),(25,2));
    }
    #[test]
    fn activity_retains_only_bounded_prefixes_and_never_tool_or_assistant_bodies() {
        let mut activity=ObserverActivity::default();let event=object([("type","model_request_started".into()),("provider",JsValue::String("p".repeat(1_000_000).into())),("model","fixture".into())]);for _ in 0..65{activity.record(&event);}assert_eq!(activity.seen,64);assert_eq!(activity.lines.len(),64);assert!(activity.lines.iter().map(|line|line.as_ref().unwrap().head.len()).sum::<usize>()<=64*MAX_TEXT);
        let mut ignore=ObserverActivity::default();ignore.record(&object([("type","text_delta".into()),("text",JsValue::String("secret body".repeat(1000).into()))]));assert!(ignore.lines.is_empty());assert_eq!(ignore.seen,1);
    }
    #[test]
    fn malformed_activity_errors_are_deferred_after_the_observed_fork() {
        let fixture=pilotdeck_protocol::parse_json(include_str!("../../tests/fixtures/native-process/observer-oracle.json")).unwrap();let rows=value(&fixture,"rows");let spec=value(&array(&rows)[0],"input");let mut input=input(&spec,"observer-malformed".into());input.activity.record(&object([("type","tool_calls_detected".into()),("calls",JsValue::Null)]));assert_eq!(input.activity.seen,1);assert_eq!(digest(&input).unwrap_err(),"Cannot read properties of null (reading 'map')".into());
    }
    #[tokio::test]
    async fn existing_background_runtime_owns_observer_cancel_and_report_settlement() {
        use crate::runtime::background::{BackgroundSubagentRuntime,SubagentListFilter,TaskStatus};
        let fixture=pilotdeck_protocol::parse_json(include_str!("../../tests/fixtures/native-process/observer-oracle.json")).unwrap();let rows=value(&fixture,"rows");let spec=value(&array(&rows)[0],"input");let input=input(&spec,"observer-cancel".into());let (entered,wait)=tokio::sync::oneshot::channel();let fork:ObserverFork=Box::new(move|_,signal|Box::pin(async move{let _=entered.send(());signal.cancelled().await;Err("observer cancelled".into())}));
        let events=Arc::new(Mutex::new(Vec::new()));let output=events.clone();let emit:ObserverEmitter=Arc::new(move|event|{output.lock().unwrap().push(event);Ok(())});let task=prepare_task(input,fork,Some(emit),Arc::new(||1000.0)).unwrap();let runtime=BackgroundSubagentRuntime::default();let admitted=runtime.start(task);tokio::time::timeout(std::time::Duration::from_secs(3),wait).await.unwrap().unwrap();assert!(tokio::time::timeout(std::time::Duration::from_secs(3),runtime.stop_and_wait(&admitted.task_id)).await.unwrap());
        let tasks=runtime.list(&SubagentListFilter{session_id:Some("parent-session".into()),kind:Some(SubagentKind::Observer),status:None});assert_eq!(tasks.len(),1);assert_eq!(tasks[0].status,TaskStatus::Cancelled);assert_eq!(tasks[0].observed_subagent_id,Some("observed-child".into()));let events=events.lock().unwrap();assert_eq!(events.len(),1);assert_eq!(events[0].get("success"),Some(&JsValue::Bool(false)));assert!(is(events[0].get("error"),"observer cancelled"));
    }
}
