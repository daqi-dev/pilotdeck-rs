//! Context dispatch and request-level compaction budgeting (TS dispatcher:308-376).
//! Runtime implementations must use cooperative futures, like the budget ports.
mod durable;
mod request;
use super::{budget, model, tool_context::ContextRequestIdentity, wire};
use crate::host::{HostError, HostFuture, JsObject, JsValue, ModuleError};
use crate::runtime::network::Cancellation;
pub use durable::compact_with_recorder;
use std::sync::Arc;

pub const COMPACTION_BUDGET_CONTRACT_ERROR_CODE: &str = "COMPACTION_BUDGET_CONTRACT_INVALID";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    PrepareForModel,
    ApplyToolResults,
    RecoverFromModelError,
    CaptureTurn,
    TryAutoCompact,
}
impl Operation {
    pub fn name(self) -> &'static str {
        match self {
            Self::PrepareForModel => "prepare_for_model",
            Self::ApplyToolResults => "apply_tool_results",
            Self::RecoverFromModelError => "recover_from_model_error",
            Self::CaptureTurn => "capture_turn",
            Self::TryAutoCompact => "try_auto_compact",
        }
    }
}
const OPERATIONS: [Operation; 5] = [
    Operation::PrepareForModel,
    Operation::ApplyToolResults,
    Operation::RecoverFromModelError,
    Operation::CaptureTurn,
    Operation::TryAutoCompact,
];

/// Non-serializable host capabilities are separate from the shallow wire fields.
/// The dispatcher deliberately does not validate the runtime's input schemas.
pub struct ContextInput {
    pub fields: JsObject,
    pub abort_signal: Option<super::capability::RuntimeContext>,
    pub budget_evaluator: Option<Arc<dyn CompactionBudgetEvaluator>>,
}
pub trait CompactionBudgetEvaluator: Send + Sync {
    fn evaluate(&self, messages: JsValue) -> HostFuture<JsValue>;
}
pub trait ContextRuntime: Send + Sync {
    /// Prepare is required; the remaining methods are independently optional.
    fn supports(&self, operation: Operation) -> bool;
    fn prepare_for_model(&self, input: ContextInput) -> HostFuture<JsValue>;
    fn apply_tool_results(&self, _input: ContextInput) -> HostFuture<JsValue> {
        unsupported(Operation::ApplyToolResults)
    }
    fn recover_from_model_error(&self, _input: ContextInput) -> HostFuture<JsValue> {
        unsupported(Operation::RecoverFromModelError)
    }
    fn capture_turn(&self, _input: ContextInput) -> HostFuture<JsValue> {
        unsupported(Operation::CaptureTurn)
    }
    fn try_auto_compact(&self, _input: ContextInput) -> HostFuture<JsValue> {
        unsupported(Operation::TryAutoCompact)
    }
}
fn unsupported(operation: Operation) -> HostFuture<JsValue> {
    Box::pin(async move {
        Err(ModuleError::new(format!(
            "Host context capability does not support {}.",
            operation.name()
        ))
        .into())
    })
}

#[derive(Default)]
pub struct Handler {
    recorder: Option<Arc<crate::storage::recorder::Recorder>>,
    runtime: Option<Arc<dyn ContextRuntime>>,
    identity: Option<ContextRequestIdentity>,
    config: JsObject,
    signal: Option<Cancellation>,
}
impl Handler {
    pub fn new(
        runtime: Arc<dyn ContextRuntime>,
        identity: ContextRequestIdentity,
        config: JsObject,
        signal: Option<Cancellation>,
    ) -> Self {
        Self {
            recorder: None,
            runtime: Some(runtime),
            identity: Some(identity),
            config,
            signal,
        }
    }
    /// Session-owned durable compaction bracket. Completion is deferred until
    /// the existing turn persistence callback commits the replacement.
    pub fn with_recorder(mut self, recorder: Arc<crate::storage::recorder::Recorder>) -> Self {
        self.recorder = Some(recorder);
        self
    }
    pub fn methods(&self) -> Vec<&'static str> {
        self.runtime.as_ref().map_or_else(Vec::new, |runtime| {
            OPERATIONS
                .into_iter()
                .filter(|op| *op == Operation::PrepareForModel || runtime.supports(*op))
                .map(Operation::name)
                .collect()
        })
    }
    pub fn call(
        &self,
        call: JsValue,
        budget: &budget::Handler,
        model: &model::Handler,
    ) -> HostFuture<JsObject> {
        let payload = call.get("payload").and_then(JsValue::as_object);
        let operation = wire::string_field(payload, "operation").cloned();
        let source = payload
            .and_then(|p| p.get("input"))
            .and_then(JsValue::as_object);
        let identity = self
            .identity
            .as_ref()
            .map(|identity| identity.context_identity(source));
        let mut input = ContextInput {
            fields: identity
                .as_ref()
                .map(|i| i.fields.clone())
                .unwrap_or_default(),
            abort_signal: identity.and_then(|i| i.abort_signal),
            budget_evaluator: None,
        };
        let config = self.config.clone();
        let signal = self.signal.clone();
        let runtime = self.runtime.clone();
        let recorder = self.recorder.clone();
        let budget = budget
            .ports
            .as_ref()
            .and_then(|p| p.evaluate_request_budget.clone());
        let model = model.clone();
        Box::pin(async move {
            let name = operation?;
            if name.equals_str("try_auto_compact") {
                if missing(input.fields.get("budgetStage"))
                    && let Some(stage) = input
                        .fields
                        .get("budgetProjection")
                        .and_then(JsValue::as_object)
                        .and_then(|p| p.get("stage"))
                        .filter(|s| ["pre_route", "routed", "recovery"].iter().any(|v| is(s, v)))
                {
                    input.fields.insert("budgetStage".into(), stage.clone());
                }
                if missing(input.fields.get("maxContextTokens"))
                    && let Some(value) =
                        config.get("maxContextTokens").filter(|v| !v.is_undefined())
                {
                    input
                        .fields
                        .insert("maxContextTokens".into(), value.clone());
                }
                if !missing(input.fields.get("budgetRequest")) {
                    let request =
                        contract(budget::canonical_request(input.fields.get("budgetRequest")))?;
                    if let Some(port) = budget {
                        let max_context_tokens = contract(wire::positive_finite(
                            input.fields.get("maxContextTokens"),
                            "maxContextTokens",
                        ))?;
                        let reserved_output_tokens = contract(wire::optional_non_negative_finite(
                            input.fields.get("reservedOutputTokens"),
                            "reservedOutputTokens",
                        ))?;
                        let preparation = input
                            .fields
                            .get("budgetPreparation")
                            .and_then(JsValue::as_object)
                            .filter(|p| {
                                matches!(p.get("tools"), Some(JsValue::Array(_)))
                                    && matches!(p.get("provider"), Some(JsValue::String(_)))
                                    && matches!(p.get("model"), Some(JsValue::String(_)))
                            })
                            .cloned()
                            .ok_or_else(|| {
                                contract_error(
                                    "Sidecar compaction budget preparation is missing or invalid.",
                                )
                            })?;
                        let id = contract(wire::optional_string_field(
                            Some(&input.fields),
                            "budgetPreparationId",
                        ))?;
                        let prepared = if let Some(id) = id {
                            Some(model.preparation(id).ok_or_else(|| {
                                contract_host(wire::named_error(
                                    "Unknown sidecar compaction budget preparation: ",
                                    id,
                                    "",
                                ))
                            })?)
                        } else {
                            None
                        };
                        if prepared.as_ref().is_some_and(|p| {
                            request.get("provider") != Some(&JsValue::String(p.provider.clone()))
                                || request.get("model") != Some(&JsValue::String(p.model.clone()))
                        }) {
                            return Err(contract_error(
                                "Sidecar compaction budget preparation does not match the request route.",
                            ));
                        }
                        let stage = get(&input.fields, "budgetStage");
                        if model.materializer.is_some()
                            && !is(&stage, "pre_route")
                            && prepared.is_none()
                        {
                            return Err(contract_error(
                                "Sidecar compaction budget is missing its prepared request reference.",
                            ));
                        }
                        let calibration =
                            calibration(input.fields.get("budgetCalibration"), &request)?;
                        input.budget_evaluator = Some(Arc::new(Evaluator {
                            request,
                            preparation,
                            prepared,
                            stage,
                            runtime: runtime.clone(),
                            materializer: model.materializer.clone(),
                            permission_mode: get(&config, "permissionMode"),
                            abort_signal: input.abort_signal.clone(),
                            options: budget::EvaluationOptions {
                                max_context_tokens,
                                reserved_output_tokens,
                                use_provider_count: None,
                                calibration,
                                signal,
                            },
                            port,
                        }));
                    }
                    remove(&mut input.fields, "budgetRequest");
                }
            }
            let operation = OPERATIONS.into_iter().find(|op| name.equals_str(op.name()));
            let Some((runtime, operation)) = runtime
                .zip(operation)
                .filter(|(r, op)| *op == Operation::PrepareForModel || r.supports(*op))
            else {
                return Err(wire::named_error(
                    "Host context capability does not support ",
                    &name,
                    ".",
                ));
            };
            let result = match operation {
                Operation::PrepareForModel => runtime.prepare_for_model(input).await?,
                Operation::ApplyToolResults => runtime.apply_tool_results(input).await?,
                Operation::RecoverFromModelError => runtime.recover_from_model_error(input).await?,
                Operation::CaptureTurn => {
                    runtime.capture_turn(input).await?;
                    JsValue::Null
                }
                Operation::TryAutoCompact => match recorder {
                    Some(recorder) => compact_with_recorder(runtime, recorder, input).await?,
                    None => runtime.try_auto_compact(input).await?,
                },
            };
            Ok(object([("result", result)]))
        })
    }
    pub fn dispose(&self) -> HostFuture<()> {
        Box::pin(async { Ok(()) })
    }
}
#[derive(Clone)]
struct Evaluator {
    request: JsObject,
    preparation: JsObject,
    prepared: Option<model::PreparedInvocation>,
    stage: JsValue,
    runtime: Option<Arc<dyn ContextRuntime>>,
    materializer: Option<Arc<dyn model::RequestMaterializer>>,
    permission_mode: JsValue,
    abort_signal: Option<super::capability::RuntimeContext>,
    options: budget::EvaluationOptions,
    port: Arc<dyn budget::RequestBudgetEvaluator>,
}
impl CompactionBudgetEvaluator for Evaluator {
    fn evaluate(&self, messages: JsValue) -> HostFuture<JsValue> {
        let evaluator = self.clone();
        Box::pin(async move {
            let messages = canonical_messages(&messages)?;
            let runtime = evaluator.runtime.ok_or_else(|| ModuleError::new("Host context capability is unavailable for request-level compaction budgeting."))?;
            let mut fields = evaluator.preparation;
            fields.insert(
                "messages".into(),
                JsValue::Array(request::normalize(messages)?),
            );
            fields.insert("previewOnly".into(), JsValue::Bool(true));
            if evaluator.abort_signal.is_some() {
                remove(&mut fields, "abortSignal");
            }
            let prepared = runtime
                .prepare_for_model(ContextInput {
                    fields,
                    abort_signal: evaluator.abort_signal,
                    budget_evaluator: None,
                })
                .await?;
            let candidate = request::finalize(
                &evaluator.request,
                prepared,
                is(&evaluator.permission_mode, "plan"),
            )
            .await?;
            let request = if let Some((prepared, materializer)) =
                evaluator.prepared.zip(evaluator.materializer)
            {
                let result = materializer.materialize(prepared, candidate).await?;
                budget::canonical_request(Some(&result))?
            } else if is(&evaluator.stage, "routed") || is(&evaluator.stage, "recovery") {
                let mut request = evaluator.request;
                for key in ["messages", "cacheBreakpoints", "cachePlan"] {
                    request.insert(key.into(), get(&candidate, key));
                }
                budget::canonical_request(Some(&JsValue::Object(request)))?
            } else {
                candidate
            };
            let snapshot = evaluator
                .port
                .evaluate_request_budget(request, evaluator.options)
                .await?;
            validate_snapshot(&snapshot)?;
            Ok(snapshot)
        })
    }
}
fn canonical_messages(value: &JsValue) -> Result<&[JsValue], HostError> {
    if let JsValue::Array(messages) = value
        && messages.iter().all(|message| {
            message.as_object().is_some_and(|m| {
                (is(&get(m, "role"), "user") || is(&get(m, "role"), "assistant"))
                    && matches!(m.get("content"), Some(JsValue::Array(_)))
            })
        })
    {
        return Ok(messages);
    }
    Err(contract_error(
        "Compaction budget messages must be canonical messages.",
    ))
}
fn calibration(value: Option<&JsValue>, request: &JsObject) -> Result<Option<JsObject>, HostError> {
    if missing(value) {
        return Ok(None);
    }
    let calibration = value
        .and_then(JsValue::as_object)
        .filter(|c| {
            c.get("provider") == request.get("provider") && c.get("model") == request.get("model")
        })
        .ok_or_else(|| {
            contract_error(
                "Sidecar compaction budget calibration does not match the request route.",
            )
        })?;
    let actual = contract(wire::positive_finite(
        calibration.get("actualInputTokens"),
        "calibration.actualInputTokens",
    ))?;
    let estimated = contract(wire::positive_finite(
        calibration.get("estimatedInputTokens"),
        "calibration.estimatedInputTokens",
    ))?;
    Ok(Some(object([
        ("provider", get(request, "provider")),
        ("model", get(request, "model")),
        ("actualInputTokens", JsValue::Number(actual)),
        ("estimatedInputTokens", JsValue::Number(estimated)),
    ])))
}
fn validate_snapshot(value: &JsValue) -> Result<(), HostError> {
    let snapshot = value.as_object().ok_or_else(|| {
        contract_error("Host compaction budget capability returned an invalid snapshot.")
    })?;
    for field in [
        "tokens",
        "maxContextTokens",
        "warningRatio",
        "blockingRatio",
        "ratio",
    ] {
        contract(wire::optional_non_negative_finite(
            snapshot.get(field),
            field,
        ))?;
        if missing(snapshot.get(field)) {
            return Err(contract_error(&format!(
                "Host compaction budget snapshot is missing {field}."
            )));
        }
    }
    if !["ok", "warning", "blocking"]
        .iter()
        .any(|state| is(&get(snapshot, "state"), state))
    {
        return Err(contract_error(
            "Host compaction budget snapshot has an invalid state.",
        ));
    }
    Ok(())
}
fn contract<T>(result: Result<T, HostError>) -> Result<T, HostError> {
    result.map_err(contract_host)
}
fn contract_host(mut error: HostError) -> HostError {
    let details = error
        .module_error
        .get_or_insert_with(|| Box::new(ModuleError::new(error.message.clone())));
    details.code = Some(COMPACTION_BUDGET_CONTRACT_ERROR_CODE.into());
    error
}
fn contract_error(message: &str) -> HostError {
    let mut error = ModuleError::new(message);
    error.code = Some(COMPACTION_BUDGET_CONTRACT_ERROR_CODE.into());
    error.into()
}
fn missing(value: Option<&JsValue>) -> bool {
    value.is_none_or(JsValue::is_undefined)
}
fn is(value: &JsValue, expected: &str) -> bool {
    value.as_string().is_some_and(|s| s.equals_str(expected))
}
fn get(value: &JsObject, key: &str) -> JsValue {
    value.get(key).cloned().unwrap_or(JsValue::Undefined)
}
fn object<const N: usize>(fields: [(&str, JsValue); N]) -> JsObject {
    let mut value = JsObject::new();
    for (key, item) in fields {
        value.insert(key.into(), item);
    }
    value
}

fn remove(value: &mut JsObject, key: &str) {
    let mut result = JsObject::new();
    for (k, v) in value.entries() {
        if !k.equals_str(key) {
            result.insert(k.clone(), v.clone());
        }
    }
    *value = result;
}
