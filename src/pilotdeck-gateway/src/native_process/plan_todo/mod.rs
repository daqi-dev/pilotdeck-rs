//! Session-owned durable plan/todo service, bound to the active turn.
mod files;
mod projection;
use self::projection::{Snapshot, merge, replace};
use crate::{
    host::{
        HostError, HostFuture, ModuleError,
        modules::{
            capability::{PlanTodoHandler, RuntimeContext},
            tool_context::SessionService,
        },
    },
    runtime::session::factory::SessionProjection,
    storage::recorder::Recorder,
};
pub(crate) use files::NativePlanFiles;
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::sync::Arc;

#[derive(Clone)]
pub(crate) struct PlanTodo {
    pub session: JsString,
    pub turn: JsString,
    pub projection: Arc<SessionProjection>,
    pub recorder: Arc<Recorder>,
    pub serial: Arc<tokio::sync::Mutex<()>>,
}
fn object<const N: usize>(pairs: [(&str, JsValue); N]) -> JsObject {
    let mut o = JsObject::new();
    for (k, v) in pairs {
        o.insert(k.into(), v);
    }
    o
}
fn error(message: impl Into<String>) -> HostError {
    ModuleError::new(message.into()).into()
}
fn get_string<'a>(value: &'a JsValue, name: &str) -> Result<&'a JsString, HostError> {
    value
        .get(name)
        .and_then(JsValue::as_string)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| error(format!("Missing {name}.")))
}
fn snapshot(projection: &SessionProjection) -> Result<Snapshot, HostError> {
    projection
        .fold_domain(Snapshot::default(), |mut state, event| {
            state.apply(event)?;
            Ok(state)
        })
        .map_err(|e| error(e.to_string()))
}
fn array(value: &JsValue) -> Result<&[JsValue], HostError> {
    match value {
        JsValue::Array(items) => Ok(items),
        _ => Err(error("Plan/todo items must be an array.")),
    }
}
fn validate_updates(values: &[JsValue], items: bool) -> Result<(), HostError> {
    for v in values {
        if v.as_object().is_none() {
            return Err(error("Plan/todo entry must be an object."));
        }
        if items
            && v.get("content")
                .and_then(JsValue::as_string)
                .is_none_or(|v| v.is_empty())
        {
            return Err(error("Plan/todo item is invalid."));
        }
        if let Some(status) = v.get("status") {
            if !status.as_string().is_some_and(|s| {
                ["pending", "in_progress", "completed", "cancelled"]
                    .iter()
                    .any(|x| s.equals_str(x))
            }) {
                return Err(error(if items {
                    "Plan/todo item is invalid."
                } else {
                    "Plan/todo update status is invalid."
                }));
            }
        } else if items {
            return Err(error("Plan/todo item is invalid."));
        }
    }
    Ok(())
}
impl PlanTodo {
    pub(crate) fn execute(&self, payload: JsValue) -> HostFuture<JsObject> {
        let this = self.clone();
        Box::pin(async move {
            if get_string(&payload, "sessionId")? != &this.session
                || get_string(&payload, "turnId")? != &this.turn
            {
                return Err(error(
                    "Plan/todo capability identity does not match the active host turn.",
                ));
            }
            let method = get_string(&payload, "method")?.clone();
            static SLOTS: std::sync::OnceLock<Arc<tokio::sync::Semaphore>> =
                std::sync::OnceLock::new();
            let permit = SLOTS
                .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(4)))
                .clone()
                .acquire_owned()
                .await
                .map_err(|_| error("Plan/todo executor unavailable."))?;
            let serial = this.serial.clone().lock_owned().await;
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                let _serial = serial;
                let state = snapshot(&this.projection)?;
                let mut draft = JsObject::new();
                if method.equals_str("read") {
                    return Ok(object([("snapshot", state.value())]));
                }
                if method.equals_str("mark_plan_approved") {
                    let plan = get_string(&payload, "plan")?;
                    projection::validate_plan(plan).map_err(|e| error(e.to_string()))?;
                    let units = crate::runtime::memory::js_trim_units(plan.units());
                    draft.insert("type".into(), "plan_todo_plan_changed".into());
                    draft.insert(
                        "plan".into(),
                        if units.is_empty() {
                            JsValue::Null
                        } else {
                            JsValue::String(JsString::from_utf16(units.to_vec()))
                        },
                    );
                } else if method.equals_str("record_todo_write") || method.equals_str("write_todos")
                {
                    let undefined = JsValue::Undefined;
                    let items = array(payload.get("todos").unwrap_or(&undefined))?;
                    let legacy = method.equals_str("record_todo_write");
                    validate_updates(items, legacy)?;
                    let merging = !legacy && payload.get("merge") == Some(&JsValue::Bool(true));
                    let todos = if merging {
                        merge(state.todos(), items)
                    } else {
                        replace(items)
                    };
                    draft.insert("type".into(), "plan_todo_written".into());
                    draft.insert(
                        "mode".into(),
                        if legacy {
                            "markdown".into()
                        } else {
                            "structured".into()
                        },
                    );
                    draft.insert("merge".into(), JsValue::Bool(merging));
                    if legacy {
                        draft.insert(
                            "markdown".into(),
                            JsValue::String(get_string(&payload, "markdown")?.clone()),
                        );
                    } else if let Some(value) =
                        payload.get("markdown").filter(|v| v.as_string().is_some())
                    {
                        draft.insert("markdown".into(), value.clone());
                    }
                    if let Some(reason) = payload.get("reason").and_then(JsValue::as_string) {
                        let value = crate::runtime::memory::js_trim_units(reason.units());
                        if !value.is_empty() {
                            draft.insert(
                                "reason".into(),
                                JsValue::String(JsString::from_utf16(value.to_vec())),
                            );
                        }
                    }
                    draft.insert(
                        "todos".into(),
                        JsValue::Array(todos.iter().map(projection::Todo::value).collect()),
                    );
                } else if method.equals_str("mark_tool_progress") {
                    let name = get_string(&payload, "toolName")?;
                    if !state.should_record_progress(name) {
                        return Ok(object([("snapshot", state.value())]));
                    }
                    draft.insert("type".into(), "plan_todo_progressed".into());
                    draft.insert("toolName".into(), JsValue::String(name.clone()));
                } else {
                    return Err(error(format!(
                        "Unsupported sidecar plan/todo method: {}",
                        String::from_utf16_lossy(method.units())
                    )));
                }
                let mut prospective = state;
                prospective
                    .apply(&JsValue::Object(draft.clone()))
                    .map_err(|e| error(e.to_string()))?;
                this.recorder
                    .writer()
                    .record_session_event_checked(&this.session, &this.turn, draft)
                    .map_err(|e| error(e.to_string()))?;
                Ok(object([("snapshot", snapshot(&this.projection)?.value())]))
            })
            .await
            .map_err(|e| error(e.to_string()))?
        })
    }
}
impl PlanTodoHandler for PlanTodo {
    fn call(&self, call: JsValue) -> HostFuture<JsObject> {
        self.execute(call.get("payload").cloned().unwrap_or(JsValue::Undefined))
    }
    fn blocking_message(&self, tool: JsString, read_only: bool) -> HostFuture<Option<JsString>> {
        if read_only || tool.equals_str("todo_write") {
            return Box::pin(async { Ok(None) });
        }
        let mut request = object([
            ("sessionId", JsValue::String(self.session.clone())),
            ("turnId", JsValue::String(self.turn.clone())),
            ("method", "read".into()),
        ]);
        let future = self.execute(JsValue::Object(std::mem::take(&mut request)));
        Box::pin(async move {
            let result = future.await?;
            let blocked = result
                .get("snapshot")
                .and_then(|v| v.get("requiresInitialization"))
                == Some(&JsValue::Bool(true));
            Ok(blocked.then(||JsString::from("An approved plan is active, but the todo list has not been initialized yet. Call `todo_write` first with a markdown checklist based on the approved plan, then retry this tool.")))
        })
    }
}
impl SessionService for PlanTodo {
    fn for_session(&self, session: &JsString) -> Option<RuntimeContext> {
        (session == &self.session).then(|| {
            Arc::new(crate::host::modules::capability::PlanTodoContext(Arc::new(
                self.clone(),
            ))) as RuntimeContext
        })
    }
}
