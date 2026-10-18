//! Turn drain callbacks and atomic compaction persistence (TS dispatcher:269-306).
use super::{tool_context::SteerAuthorizations, wire};
use crate::{
    host::{HostError, HostFuture, JsObject, JsValue, ModuleError},
    runtime::steer::TurnSteers,
    storage::recorder::Recorder,
};
use pilotdeck_protocol::JsString;
use std::sync::Arc;

pub type DrainSteer = Arc<dyn Fn() -> HostFuture<Vec<JsObject>> + Send + Sync>;
pub type DrainOrCloseSteer = Arc<dyn Fn() -> HostFuture<JsObject> + Send + Sync>;
/// TS `AgentLoopInput.onCompactPersisted`: the dispatcher has already validated
/// `payload.boundary` as a record and `payload.messages` as an array.
pub type PersistCompaction = Arc<dyn Fn(JsObject, Vec<JsValue>) -> HostFuture<()> + Send + Sync>;
/// TS `loopAbortController.abort(reason)` in `TurnRunner`'s persistence catch.
/// The host wires this to the turn's `Cancellation`; the adapter still answers
/// `{persisted:true}` because the abort travels beside the response.
pub type AbortCompaction = Arc<dyn Fn(&str) + Send + Sync>;

#[derive(Clone, Default)]
pub struct Ports {
    pub drain_steer: Option<DrainSteer>,
    pub drain_or_close_steer: Option<DrainOrCloseSteer>,
    pub persist_compaction: Option<PersistCompaction>,
}
impl Ports {
    /// Both callbacks share the runner's tracking state and host turn binding.
    /// Remote payload fields cannot select a different mailbox or turn.
    pub fn for_turn(turn: TurnSteers) -> Self {
        let drain = turn.clone();
        Self {
            drain_steer: Some(Arc::new(move || {
                let messages = drain.drain().iter().map(|m| m.to_object()).collect();
                Box::pin(async move { Ok(messages) })
            })),
            drain_or_close_steer: Some(Arc::new(move || {
                let drained = turn.drain_or_close();
                let mut result =
                    response_messages(drained.messages.iter().map(|m| m.to_object()).collect());
                result.insert("closed".into(), JsValue::Bool(drained.closed));
                Box::pin(async move { Ok(result) })
            })),
            persist_compaction: None,
        }
    }
    pub fn with_persist_compaction(mut self, port: PersistCompaction) -> Self {
        self.persist_compaction = Some(port);
        self
    }
}
#[derive(Clone, Default)]
pub struct Handler {
    pub ports: Ports,
    pub authorizations: Option<SteerAuthorizations>,
}
impl Handler {
    pub fn new(ports: Ports, authorizations: SteerAuthorizations) -> Self {
        Self {
            ports,
            authorizations: Some(authorizations),
        }
    }
    /// Empty capabilities omit turn from A6.9's manifest.
    pub fn methods(&self) -> Option<Vec<&'static str>> {
        let mut methods = Vec::new();
        if self.ports.drain_steer.is_some() {
            methods.push("drain_steer");
        }
        if self.ports.drain_or_close_steer.is_some() {
            methods.push("drain_or_close_steer");
        }
        if self.ports.persist_compaction.is_some() {
            methods.push("persist_compaction");
        }
        (!methods.is_empty()).then_some(methods)
    }
    pub fn call(&self, call: JsValue) -> HostFuture<JsObject> {
        let handler = self.clone();
        Box::pin(async move {
            let payload = call.get("payload").and_then(JsValue::as_object);
            let operation = wire::string_field(payload, "operation")?;
            if operation.equals_str("drain_steer") {
                if let Some(drain) = handler.ports.drain_steer {
                    let messages = drain().await?;
                    for message in &messages {
                        remember(message, handler.authorizations.as_ref())?;
                    }
                    return Ok(response_messages(messages));
                }
            } else if operation.equals_str("drain_or_close_steer")
                && let Some(drain) = handler.ports.drain_or_close_steer
            {
                let result = drain().await?;
                let Some(JsValue::Array(messages)) = result.get("messages") else {
                    return Err(ModuleError::new("messages is not iterable").into());
                };
                for message in messages {
                    let message = message
                        .as_object()
                        .ok_or_else(|| ModuleError::new("Invalid host steer message."))?;
                    remember(message, handler.authorizations.as_ref())?;
                }
                return Ok(result);
            } else if operation.equals_str("persist_compaction")
                && let Some(persist) = handler.ports.persist_compaction
            {
                // dispatcher:285-293 `asRecord` + Array.isArray guard. A record
                // is an object but not an array; both fields are required.
                let boundary = payload
                    .and_then(|payload| payload.get("boundary"))
                    .filter(|value| matches!(value, JsValue::Object(_)))
                    .and_then(JsValue::as_object)
                    .cloned();
                let messages = payload
                    .and_then(|payload| payload.get("messages"))
                    .and_then(|value| match value {
                        JsValue::Array(items) => Some(items.clone()),
                        _ => None,
                    });
                let (Some(boundary), Some(messages)) = (boundary, messages) else {
                    return Err(ModuleError::new(
                        "Compaction persistence requires boundary and messages.",
                    )
                    .into());
                };
                persist(boundary, messages).await?;
                let mut result = JsObject::new();
                result.insert("persisted".into(), JsValue::Bool(true));
                return Ok(result);
            }
            Err(wire::named_error(
                "Host turn capability does not support ",
                operation,
                ".",
            ))
        })
    }
    pub fn apply_steer_authorization(&self, item_id: &JsString) {
        if let Some(authorizations) = &self.authorizations {
            authorizations.apply(item_id);
        }
    }
    /// Sidecar's steer_applied ordering: claim -> durable transcript -> grant ->
    /// append the original drained message and ack. A failed write stops here.
    pub fn persist_and_apply_steer(
        &self,
        turn: TurnSteers,
        item_id: JsString,
        message: JsValue,
        record: impl FnOnce(JsValue) -> HostFuture<()> + Send + 'static,
    ) -> HostFuture<()> {
        let handler = self.clone();
        Box::pin(async move {
            turn.on_durable_message(message, record).await?;
            handler.apply_steer_authorization(&item_id);
            turn.on_steer_applied(&item_id);
            Ok(())
        })
    }
    pub fn dispose(&self) -> HostFuture<()> {
        // The session owns mailbox close/finish; dispatcher disposal does not.
        Box::pin(async { Ok(()) })
    }
}
/// TS `TurnRunner`'s `onCompactPersisted` durable branch (TurnRunner.ts:338-374):
/// a compact boundary is written as one atomic replacement record, then the
/// deferred `compaction_completed` is committed. A failed write is best-effort:
/// the bracket closes as `compaction_failed`, the loop is aborted with
/// `compaction_persistence_failed`, and the callback still returns normally so
/// the dispatcher answers `{persisted:true}` exactly like TS.
pub fn persist_compaction_to_recorder(
    recorder: Arc<Recorder>,
    session_id: JsString,
    turn_id: JsString,
    abort: AbortCompaction,
) -> PersistCompaction {
    Arc::new(move |boundary, messages| {
        let recorder = recorder.clone();
        let session_id = session_id.clone();
        let turn_id = turn_id.clone();
        let abort = abort.clone();
        Box::pin(async move {
            let compact = boundary
                .get("kind")
                .and_then(JsValue::as_string)
                .is_some_and(|kind| kind.equals_str("compact"))
                && boundary
                    .get("subtype")
                    .and_then(JsValue::as_string)
                    .is_some_and(|subtype| subtype.equals_str("compact_boundary"));
            let result = if compact {
                recorder
                    .record_compaction_replacement(
                        &session_id,
                        &turn_id,
                        &JsValue::Object(boundary),
                        &messages,
                    )
                    .and_then(|()| recorder.commit_deferred_compaction(&session_id, &turn_id))
            } else {
                Err(crate::storage::recorder::RecorderError::State(
                    "Transcript writer does not support atomic compaction replacement.".into(),
                ))
            };
            if let Err(error) = result {
                let message = error.to_string();
                // `failDeferredCompaction(...).catch(() => {})` in TS.
                let _ = recorder.fail_deferred_compaction(&session_id, &turn_id, &message);
                abort("compaction_persistence_failed");
            }
            Ok(())
        })
    })
}
fn response_messages(messages: Vec<JsObject>) -> JsObject {
    let mut result = JsObject::new();
    result.insert(
        "messages".into(),
        JsValue::Array(messages.into_iter().map(JsValue::Object).collect()),
    );
    result
}
fn remember(
    message: &JsObject,
    authorizations: Option<&SteerAuthorizations>,
) -> Result<(), HostError> {
    let Some(authorizations) = authorizations else {
        return Ok(());
    };
    let paths = match message.get("allowedReadFiles") {
        None | Some(JsValue::Null | JsValue::Undefined) => Vec::new(),
        Some(JsValue::Array(paths)) => paths
            .iter()
            .filter_map(|p| p.as_string().cloned())
            .collect(),
        _ => {
            return Err(
                ModuleError::new("message.allowedReadFiles?.filter is not a function").into(),
            );
        }
    };
    if let Some(item_id) = message.get("itemId").and_then(JsValue::as_string) {
        authorizations.remember(item_id.clone(), paths);
    }
    Ok(())
}
