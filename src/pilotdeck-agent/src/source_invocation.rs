//! Lossless source-valid invocation owner. The model reducer owns typed open
//! assembler state; the host owns preparation/iterator handles and provider IO.
use crate::driver::{InvocationDisposition, LiveModelEvent};
use crate::ports::{
    CallControl, HostCall, HostPort, HostReply, ModelHints, PortError, PortResult,
    PreparedInvocation, StreamEvent,
};
use crate::source_model::{
    OriginalSourceModelReducer, SourceAssemblerState, SourceAssemblyError, SourceField,
    SourceModelReducer,
};
use pilotdeck_protocol::model::BoundarySnapshot;
use pilotdeck_protocol::{JsString, JsValue};
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct SourceExecutedRequest {
    pub provider: SourceField,
    pub model: SourceField,
    pub fingerprint: SourceField,
}
#[derive(Debug, Clone)]
pub struct SourceInvocationReport {
    pub assembled: Result<BoundarySnapshot, SourceAssemblyError>,
    pub state: SourceAssemblerState,
    pub executed: Option<SourceExecutedRequest>,
    pub disposition: InvocationDisposition,
    /// Only an acknowledged plain failure of the model iterator Host call.
    /// Consumer/event delivery failures and uncertain transport remain distinct.
    pub plain_host_stream_throw: bool,
    /// The host iterator ended without throwing: the final batch was consumed
    /// (`done:true`) or a source error event closed it. Original AgentLoop.ts
    /// reaches the :916 post-stream abort check only on this path; a thrown or
    /// cancelled iterator takes the :859 catch. A cancellation observed after
    /// the last batch does not turn a completed iterator into a thrown one.
    pub stream_completed: bool,
}
fn interrupted(error: PortError) -> InvocationDisposition {
    match error {
        PortError::Cancelled { cause } => InvocationDisposition::Cancelled(cause),
        error => InvocationDisposition::PortFailure(Box::new(error)),
    }
}
fn field(value: &JsValue, name: &str) -> SourceField {
    value.get(name).map_or(SourceField::Missing, |value| {
        SourceField::Present(BoundarySnapshot::capture(value))
    })
}
fn assembly_error(error: SourceAssemblyError, boundary: &BoundarySnapshot) -> PortError {
    let message = match error {
        SourceAssemblyError::TypeError { message }
        | SourceAssemblyError::NativeGraphRequired { message } => message,
    };
    PortError::SourceException {
        message,
        boundary: boundary.clone(),
    }
}
fn boundary(event: &StreamEvent) -> &BoundarySnapshot {
    match event {
        StreamEvent::Known(value) => &value.boundary,
        StreamEvent::BoundaryOnly(value) => value,
        StreamEvent::CoercedDelta { boundary, .. }
        | StreamEvent::DeferredSourceException { boundary, .. }
        | StreamEvent::ErrorBoundaryOnly { boundary, .. } => boundary,
    }
}

/// Runs actual eager or pull host operations. Full raw events, including known
/// tags outside the normalized model, are reduced after their live publication.
/// This is an invocation, not a complete turn/recovery or durable completion.
pub fn run_source_model_invocation(
    host: &mut dyn HostPort,
    control: &CallControl,
    cleanup: &CallControl,
    prepared: &PreparedInvocation,
    hints: &ModelHints,
    response_id: JsString,
    emit: &mut dyn FnMut(LiveModelEvent) -> PortResult<()>,
) -> SourceInvocationReport {
    let reducer = OriginalSourceModelReducer;
    let mut state = reducer.create(&JsValue::String(response_id));
    let mut executed = None;
    let mut done = false;
    let mut error_event = false;
    let mut disposition = InvocationDisposition::Assembled;
    let mut plain_host_stream_throw = false;
    let pull = host.advertised("model", "stream_next");
    loop {
        if let Err(error) = control.check() {
            disposition = interrupted(error);
            break;
        }
        let call = if pull {
            HostCall::ModelPull {
                handle: prepared.handle.clone(),
                request: Box::new(prepared.request.clone()),
                hints: hints.clone(),
            }
        } else {
            HostCall::ModelEagerStream {
                handle: prepared.handle.clone(),
                request: Some(Box::new(prepared.request.clone())),
                hints: hints.clone(),
            }
        };
        let (events, reply_done) = match host.call(control, call) {
            Ok(HostReply::ModelChunk(chunk)) if pull => (chunk.events, chunk.done),
            Ok(HostReply::ModelEagerEvents(events)) if !pull => (events, true),
            Ok(_) => {
                disposition = interrupted(PortError::InvalidResponse {
                    message: "Model stream returned a reply for another operation.".into(),
                });
                break;
            }
            Err(error) => {
                plain_host_stream_throw = matches!(&error, PortError::HostFailure { .. });
                disposition = interrupted(error);
                break;
            }
        };
        // The original iterator yields the complete already returned host batch.
        // Cancellation prevents the next host call; it does not truncate these
        // live events or their assembler state after acceptance.
        for event in &events {
            let boundary = boundary(event);
            let value = boundary.value();
            // The original accesses event.type before computing or yielding the
            // live event. Nullish input therefore throws before publication.
            if matches!(value, JsValue::Null | JsValue::Undefined) {
                let name = if value.is_undefined() {
                    "undefined"
                } else {
                    "null"
                };
                disposition = interrupted(PortError::SourceException {
                    message: format!("Cannot read properties of {name} (reading 'type')").into(),
                    boundary: boundary.clone(),
                });
                break;
            }
            let kind = value.get("type").and_then(JsValue::as_string);
            if kind.is_some_and(|kind| kind.equals_str("request_started")) {
                executed = Some(SourceExecutedRequest {
                    provider: field(value, "provider"),
                    model: field(value, "model"),
                    fingerprint: field(value, "requestFingerprint"),
                });
            }
            let block_kind = kind.and_then(|kind| {
                if kind.equals_str("text_delta") {
                    Some(JsString::from("text"))
                } else if kind.equals_str("thinking_delta") {
                    Some(JsString::from("thinking"))
                } else {
                    None
                }
            });
            let block_id = match block_kind
                .map(|kind| reducer.block_id(&state, &kind))
                .transpose()
            {
                Ok(id) => id.flatten(),
                Err(error) => {
                    disposition = interrupted(assembly_error(error, boundary));
                    break;
                }
            };
            if let Err(error) = emit(LiveModelEvent {
                boundary: boundary.clone(),
                block_id,
            }) {
                disposition = interrupted(error);
                break;
            }
            // The lossless reducer performs its own ordered accesses/coercion.
            // A normalized adapter's deferred/coerced view cannot replace it.
            if let Err(error) = reducer.apply(&mut state, boundary) {
                disposition = interrupted(assembly_error(error, boundary));
                break;
            }
            if kind.is_some_and(|kind| kind.equals_str("error")) {
                error_event = true;
                if let SourceField::Present(error) = &state.error {
                    disposition = InvocationDisposition::BoundaryError {
                        error: error.clone(),
                    };
                }
                break;
            }
        }
        // done:true is not consumed on source error/consumer exception inside a batch.
        if !error_event && matches!(disposition, InvocationDisposition::Assembled) {
            done = reply_done;
        }
        if done || error_event || !matches!(disposition, InvocationDisposition::Assembled) {
            break;
        }
    }
    if pull && !done && host.advertised("model", "close_stream") {
        let cleanup = CallControl {
            cancellation: cleanup.cancellation.clone(),
            deadline: Some(cleanup.deadline.map_or_else(
                || Instant::now() + Duration::from_secs(5),
                |deadline| deadline.min(Instant::now() + Duration::from_secs(5)),
            )),
        };
        let close = host.call(
            &cleanup,
            HostCall::ModelClose {
                handle: prepared.handle.clone(),
            },
        );
        if let Err(error @ PortError::AcceptedUnknown { .. }) = close
            && !matches!(&disposition, InvocationDisposition::PortFailure(original)
                if matches!(original.as_ref(), PortError::AcceptedUnknown { .. }))
        {
            // A known close failure remains advisory, but uncertain iterator
            // advancement/closure cannot become successful cancellation.
            disposition = interrupted(error);
        }
    }
    let stream_completed = done || error_event;
    if matches!(disposition, InvocationDisposition::Assembled)
        && let Err(error) = control.check()
    {
        disposition = interrupted(error);
    }
    let assembled = reducer.assemble(&mut state);
    SourceInvocationReport {
        assembled,
        state,
        executed,
        disposition,
        plain_host_stream_throw,
        stream_completed,
    }
}
