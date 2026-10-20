//! `durableContextRuntime.tryAutoCompact` using pr2's recorder lifecycle.
use super::*;
use crate::storage::recorder::{Recorder, RecorderError};

pub fn compact_with_recorder(
    runtime: Arc<dyn ContextRuntime>,
    recorder: Arc<Recorder>,
    input: ContextInput,
) -> HostFuture<JsValue> {
    // The host aborts module callbacks on terminal-drain timeout
    // (`execution.rs` `callbacks.abort_all`). TS has no equivalent: its
    // promise chain keeps running after the host stops waiting, so the
    // `catch` in `durableContextRuntime.ts:110-115` always records the
    // terminal fact. Run the bracket on a detached task; dropping this
    // caller's JoinHandle detaches it instead of dropping the bracket.
    Box::pin(async move {
        let task = tokio::spawn(compact_task(runtime, recorder, input));
        match task.await {
            Ok(result) => result,
            Err(error) => {
                Err(ModuleError::new(format!("Host compaction task failed: {error}")).into())
            }
        }
    })
}

async fn compact_task(
    runtime: Arc<dyn ContextRuntime>,
    recorder: Arc<Recorder>,
    mut input: ContextInput,
) -> Result<JsValue, HostError> {
    let session = input
        .fields
        .get("sessionId")
        .and_then(JsValue::as_string)
        .cloned()
        .unwrap_or_default();
    let turn = input
        .fields
        .get("turnId")
        .and_then(JsValue::as_string)
        .cloned()
        .unwrap_or_default();
    let operation = recorder
        .next_compaction_operation_id()
        .map_err(recorder_error)?;
    let trigger = input
        .fields
        .get("trigger")
        .filter(|v| !matches!(v, JsValue::Null | JsValue::Undefined))
        .cloned()
        .unwrap_or_else(|| {
            if input
                .fields
                .get("allowFallbackOnFailure")
                .is_some_and(crate::runtime::context::compaction::js_truthy)
            {
                JsValue::String("reactive".into())
            } else {
                JsValue::String("auto".into())
            }
        });
    let count = length(input.fields.get("messages"))?;
    let mut started = object([
        ("operationId", JsValue::String(operation.clone())),
        ("trigger", trigger.clone()),
        ("messageCount", count.clone()),
    ]);
    if let Some(max) = input
        .fields
        .get("maxContextTokens")
        .filter(|v| !v.is_undefined())
    {
        started.insert("maxContextTokens".into(), max.clone());
    }
    recorder
        .record_compaction_started(&session, &turn, &JsValue::Object(started))
        .map_err(recorder_error)?;
    input.fields.insert("trigger".into(), trigger.clone());
    let outcome: Result<JsValue, HostError> = async {
        let result = runtime.try_auto_compact(input).await?;
        let kind = result.get("type").cloned().unwrap_or(JsValue::Undefined);
        let compacted = is(&kind, "compacted");
        let details = result
            .get("result")
            .filter(|v| crate::runtime::context::compaction::js_truthy(v));
        let mut completed = object([
            ("operationId", JsValue::String(operation.clone())),
            ("status", kind),
        ]);
        if compacted {
            completed.insert(
                "tier".into(),
                result.get("tier").cloned().unwrap_or(JsValue::Undefined),
            );
            if let Some(id) = details
                .and_then(|r| r.get("compactionId"))
                .filter(|v| crate::runtime::context::compaction::js_truthy(v))
            {
                completed.insert("compactionId".into(), id.clone());
            }
            let count = length(result.get("messages"))?;
            completed.insert("messageCount".into(), count);
            if let Some(error) = result
                .get("error")
                .filter(|v| crate::runtime::context::compaction::js_truthy(v))
            {
                completed.insert("error".into(), error.clone());
            }
        } else {
            completed.insert("messageCount".into(), count);
        }
        if compacted && let Some(details) = details {
            if is(&trigger, "manual")
                && (details.get("error").is_some_and(|v| !v.is_undefined())
                    || details
                        .get("summaryMessage")
                        .is_none_or(JsValue::is_undefined))
            {
                return Err(ModuleError::new(
                    "Manual compaction provider returned no useful summary replacement.",
                )
                .into());
            }
            recorder
                .defer_compaction_completion(&session, &turn, &JsValue::Object(completed))
                .map_err(recorder_error)?;
        } else if compacted && is(&trigger, "manual") {
            return Err(ModuleError::new(
                "Manual compaction provider returned no durable replacement result.",
            )
            .into());
        } else {
            recorder
                .record_compaction_completed(&session, &turn, &JsValue::Object(completed))
                .map_err(recorder_error)?;
        }
        Ok(result)
    }
    .await;
    if let Err(error) = &outcome {
        let message = error
            .module_error
            .as_ref()
            .map_or_else(|| error.message.clone().into(), |e| e.message.clone());
        recorder
            .record_compaction_failed(
                &session,
                &turn,
                &JsValue::Object(object([
                    ("operationId", JsValue::String(operation)),
                    ("error", JsValue::String(message)),
                ])),
            )
            .map_err(recorder_error)?;
    }
    outcome
}
fn recorder_error(error: RecorderError) -> HostError {
    ModuleError::new(error.to_string()).into()
}
fn length(value: Option<&JsValue>) -> Result<JsValue, HostError> {
    match value {
        None | Some(JsValue::Undefined) => {
            Err(ModuleError::new("Cannot read properties of undefined (reading 'length')").into())
        }
        Some(JsValue::Null) => {
            Err(ModuleError::new("Cannot read properties of null (reading 'length')").into())
        }
        Some(JsValue::Array(values)) => Ok(JsValue::Number(values.len() as f64)),
        Some(JsValue::String(value)) => Ok(JsValue::Number(value.len_utf16() as f64)),
        Some(value) => Ok(value.get("length").cloned().unwrap_or(JsValue::Undefined)),
    }
}
