#[path = "common/baseline.rs"]
mod baseline;
use pilotdeck_gateway::host::modules::{ModuleRouter, budget, context::*, model, tool_context::*};
use pilotdeck_gateway::host::{HostError, HostFuture, HostHandler, ModuleError};
use pilotdeck_gateway::runtime::network::Cancellation;
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use std::{
    path::Path,
    sync::{Arc, Mutex},
};
const ORACLE: &str = include_str!("fixtures/host-context/oracle.json");
static UNDEFINED: JsValue = JsValue::Undefined;
fn field<'a>(v: &'a JsValue, k: &str) -> &'a JsValue {
    v.get(k).unwrap_or(&UNDEFINED)
}
fn text(v: &JsValue) -> String {
    v.as_string().unwrap().to_utf8().unwrap()
}
fn array(v: &JsValue) -> &[JsValue] {
    if let JsValue::Array(a) = v {
        a
    } else {
        panic!("array {v:?}")
    }
}
fn obj<const N: usize>(fields: [(&str, JsValue); N]) -> JsObject {
    let mut o = JsObject::new();
    for (k, v) in fields {
        o.insert(k.into(), v);
    }
    o
}
fn val<const N: usize>(fields: [(&str, JsValue); N]) -> JsValue {
    JsValue::Object(obj(fields))
}
fn decode(v: &JsValue, root: &Path) -> JsValue {
    match v {
        JsValue::String(s) => s.to_utf8().map_or_else(
            |_| v.clone(),
            |s| JsValue::String(s.replace("<fixture>", root.to_str().unwrap()).into()),
        ),
        JsValue::Array(a) => JsValue::Array(a.iter().map(|v| decode(v, root)).collect()),
        JsValue::Object(o) => {
            if o.contains_key("$undefined") {
                return JsValue::Undefined;
            }
            if let Some(n) = o.get("$number") {
                return JsValue::Number(match text(n).as_str() {
                    "NaN" => f64::NAN,
                    "Infinity" => f64::INFINITY,
                    "-Infinity" => f64::NEG_INFINITY,
                    other => panic!("unknown $number sentinel {other}"),
                });
            }
            let mut out = JsObject::new();
            for (k, v) in o.entries() {
                out.insert(k.clone(), decode(v, root));
            }
            JsValue::Object(out)
        }
        _ => v.clone(),
    }
}
fn normalized(v: &JsValue, root: &Path) -> String {
    // JSON normalization still omits undefined fields, but key order is part of
    // the contract: the dispatcher records and returns objects as the runtime
    // built them. Compare the exact serialized bytes instead of sorting keys.
    stringify_json(v)
        .unwrap()
        .unwrap()
        .replace(root.to_str().unwrap(), "<fixture>")
}
fn request() -> JsValue {
    parse_json(r#"{"provider":"p","model":"m","messages":[],"systemPrompt":"base prompt","tools":[{"name":"base"}],"metadata":{"kept":true}}"#).unwrap()
}
fn messages() -> JsValue {
    parse_json(r#"[{"role":"user","content":[{"type":"text","text":"hello"}]}]"#).unwrap()
}
fn call(payload: JsValue, module: &str) -> JsValue {
    val([
        ("kind", "request".into()),
        ("method", "module_call".into()),
        ("module", module.into()),
        ("messageId", "call".into()),
        ("requestId", "request".into()),
        ("runId", "run".into()),
        ("operationId", "operation".into()),
        ("payload", payload),
    ])
}
#[derive(Clone)]
struct Fake {
    spec: JsValue,
    calls: Arc<Mutex<Vec<JsValue>>>,
    signal: Option<Cancellation>,
    abort: Option<Arc<dyn std::any::Any + Send + Sync>>,
}
impl Fake {
    fn record(&self, v: JsValue) {
        self.calls.lock().unwrap().push(v);
    }
    fn fail(&self, at: &str) -> Result<(), HostError> {
        if field(&self.spec, "errorAt")
            .as_string()
            .is_some_and(|s| s.equals_str(at))
        {
            let error = field(&self.spec, "error");
            Err(ModuleError::from_js_fields(
                field(error, "message").as_string().unwrap().clone(),
                error,
            )
            .into())
        } else {
            Ok(())
        }
    }
    fn invoke(&self, op: Operation, input: ContextInput) -> HostFuture<JsValue> {
        let fake = self.clone();
        Box::pin(async move {
            // Mirror the generator's recordInput: trusted fields are removed
            // from the recorded object and appended at the end. JsObject::insert
            // keeps an existing key's position, so a plain clone would preserve
            // the wire order of a forged abortSignal instead.
            let forged_abort = input.fields.get("abortSignal").cloned();
            let mut fields = JsObject::new();
            for (key, value) in input.fields.entries() {
                if key.equals_str("abortSignal") || key.equals_str("budgetEvaluator") {
                    continue;
                }
                fields.insert(key.clone(), value.clone());
            }
            let abort = if let Some(abort) = &input.abort_signal {
                assert!(Arc::ptr_eq(abort, fake.abort.as_ref().unwrap()));
                Some(if fake.signal.as_ref().unwrap().is_cancelled() {
                    "aborted".into()
                } else {
                    "active".into()
                })
            } else {
                forged_abort
            };
            if let Some(abort) = abort.filter(|v| !v.is_undefined()) {
                fields.insert("abortSignal".into(), abort);
            }
            if input.budget_evaluator.is_some() {
                fields.insert("budgetEvaluator".into(), "function".into());
            }
            fake.record(val([
                ("operation", op.name().into()),
                ("input", JsValue::Object(fields)),
            ]));
            fake.fail(op.name())?;
            if op == Operation::PrepareForModel
                && input.fields.get("previewOnly").and_then(JsValue::as_bool) == Some(true)
            {
                if let Some(preview) = fake.spec.get("previewResult") {
                    return Ok(preview.clone());
                }
                let mut result = obj([
                    ("messages", input.fields.get("messages").unwrap().clone()),
                    ("systemPrompt", "preview prompt".into()),
                    ("tools", JsValue::Array(vec![])),
                    (
                        "cacheBreakpoints",
                        JsValue::Array(vec![JsValue::Number(0.0)]),
                    ),
                ]);
                if let Some(preview) = field(&fake.spec, "preview").as_object() {
                    for (k, v) in preview.entries() {
                        result.insert(k.clone(), v.clone());
                    }
                }
                return Ok(JsValue::Object(result));
            }
            if op == Operation::TryAutoCompact
                && let Some(evaluator) = input.budget_evaluator
            {
                let snapshot = evaluator
                    .evaluate(fake.spec.get("candidate").cloned().unwrap_or_else(messages))
                    .await?;
                return Ok(val([("snapshot", snapshot)]));
            }
            Ok(fake.spec.get("result").cloned().unwrap_or_else(|| {
                val([("operation", op.name().into()), ("extra", "kept".into())])
            }))
        })
    }
}
impl ContextRuntime for Fake {
    fn supports(&self, op: Operation) -> bool {
        let i = match op {
            Operation::PrepareForModel => 0,
            Operation::ApplyToolResults => 1,
            Operation::RecoverFromModelError => 2,
            Operation::CaptureTurn => 3,
            Operation::TryAutoCompact => 4,
        };
        self.spec
            .get("capabilities")
            .is_none_or(|c| array(c)[i].as_bool().unwrap())
    }
    fn prepare_for_model(&self, i: ContextInput) -> HostFuture<JsValue> {
        self.invoke(Operation::PrepareForModel, i)
    }
    fn apply_tool_results(&self, i: ContextInput) -> HostFuture<JsValue> {
        self.invoke(Operation::ApplyToolResults, i)
    }
    fn recover_from_model_error(&self, i: ContextInput) -> HostFuture<JsValue> {
        self.invoke(Operation::RecoverFromModelError, i)
    }
    fn capture_turn(&self, i: ContextInput) -> HostFuture<JsValue> {
        self.invoke(Operation::CaptureTurn, i)
    }
    fn try_auto_compact(&self, i: ContextInput) -> HostFuture<JsValue> {
        self.invoke(Operation::TryAutoCompact, i)
    }
}
impl budget::RequestBudgetEvaluator for Fake {
    fn evaluate_request_budget(
        &self,
        request: JsObject,
        options: budget::EvaluationOptions,
    ) -> HostFuture<JsValue> {
        let fake = self.clone();
        Box::pin(async move {
            let mut fields = obj([(
                "maxContextTokens",
                JsValue::Number(options.max_context_tokens),
            )]);
            if let Some(n) = options.reserved_output_tokens {
                fields.insert("reservedOutputTokens".into(), JsValue::Number(n));
            }
            if let Some(c) = options.calibration {
                fields.insert("calibration".into(), JsValue::Object(c));
            }
            assert!(options.use_provider_count.is_none());
            let signal = if let Some(s) = options.signal {
                assert!(s.same_channel(fake.signal.as_ref().unwrap()));
                if s.is_cancelled() {
                    "aborted"
                } else {
                    "active"
                }
            } else {
                "absent"
            };
            fake.record(val([
                ("operation", "budget".into()),
                ("request", JsValue::Object(request)),
                ("options", JsValue::Object(fields)),
                ("signal", signal.into()),
            ]));
            fake.fail("budget")?;
            Ok(fake.spec.get("snapshot").cloned().unwrap_or_else(||parse_json(r#"{"tokens":25,"maxContextTokens":100,"warningRatio":0.7,"blockingRatio":0.9,"ratio":0.25,"state":"ok","extra":"kept"}"#).unwrap()))
        })
    }
}
impl model::ModelExecution for Fake {
    fn prepare(&self, input: model::PrepareInput) -> model::ModelFuture<model::PreparedInvocation> {
        self.record(val([
            ("operation", "model.prepare".into()),
            ("request", input.request.clone()),
        ]));
        let route = self
            .spec
            .get("preparedRoute")
            .cloned()
            .unwrap_or_else(|| val([("provider", "p".into()), ("model", "m".into())]));
        Box::pin(async move {
            Ok(model::PreparedInvocation::new(
                input.request,
                field(&route, "provider").clone(),
                field(&route, "model").clone(),
            ))
        })
    }
    fn stream(&self, _: model::StreamInput) -> model::ModelFuture<Box<dyn model::ModelStream>> {
        panic!("unexpected stream")
    }
}
impl model::RequestMaterializer for Fake {
    fn materialize(
        &self,
        p: model::PreparedInvocation,
        mut candidate: JsObject,
    ) -> model::ModelFuture<JsValue> {
        self.record(val([
            ("operation", "materialize".into()),
            (
                "prepared",
                val([
                    ("request", p.request),
                    ("provider", p.provider),
                    ("model", p.model),
                ]),
            ),
            ("candidate", JsValue::Object(candidate.clone())),
        ]));
        let result = self
            .fail("materialize")
            .map_err(|e| *e.module_error.unwrap());
        let output = self.spec.get("materialized").cloned();
        Box::pin(async move {
            result?;
            Ok(output.unwrap_or_else(|| {
                candidate.insert(
                    "metadata".into(),
                    val([("materialized", JsValue::Bool(true))]),
                );
                candidate.insert("systemPrompt".into(), "materialized prompt".into());
                JsValue::Object(candidate)
            }))
        })
    }
}
struct Environment;
impl TurnEnvironment for Environment {
    fn build(
        &self,
        _: Option<&JsValue>,
        _: &JsString,
        _: &JsString,
        _: &JsString,
    ) -> Result<JsObject, HostError> {
        panic!("identity must not build environment")
    }
}
fn outcome(result: Result<JsObject, HostError>) -> JsValue {
    match result {
        Ok(payload) => val([
            ("ok", JsValue::Bool(true)),
            ("payload", JsValue::Object(payload)),
        ]),
        Err(e) => {
            let e = e.module_error.expect("module error details");
            let mut details = obj([("message", JsValue::String(e.message))]);
            if let Some(c) = e.canonical {
                details.insert("canonical".into(), JsValue::Object(c));
            }
            if let Some(b) = e.retryable {
                details.insert("retryable".into(), JsValue::Bool(b));
            }
            if let Some(n) = e.retry_after_ms {
                details.insert("retryAfterMs".into(), JsValue::Number(n));
            }
            val([
                ("ok", JsValue::Bool(false)),
                (
                    "code",
                    JsValue::String(e.code.unwrap_or_else(|| "HOST_MODULE_FAILED".into())),
                ),
                ("error", JsValue::Object(details)),
            ])
        }
    }
}
#[tokio::test]
async fn real_ts_context_dispatcher_replay() {
    let oracle = baseline::oracle(ORACLE);
    let cases = array(field(&oracle, "cases"));
    assert_eq!(cases.len(), 372);
    let root = std::env::temp_dir().join(format!("pdrs-host-context-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("media.txt"), b"aGVsbG8=\n").unwrap();
    // Preserve this unique run directory as evidence; no recursive deletion.
    for case in cases {
        let name = text(field(case, "name"));
        let spec = decode(case, &root);
        let calls = Arc::new(Mutex::new(vec![]));
        let signal = case.get("signal").map(|_| {
            let signal = Cancellation::new();
            if field(case, "signal")
                .as_string()
                .is_some_and(|s| s.equals_str("aborted"))
            {
                signal.abort();
            }
            signal
        });
        let abort = signal
            .as_ref()
            .map(|_| Arc::new(()) as Arc<dyn std::any::Any + Send + Sync>);
        let fake = Arc::new(Fake {
            spec: spec.clone(),
            calls: calls.clone(),
            signal: signal.clone(),
            abort: abort.clone(),
        });
        let mut config = obj([
            ("cwd", "/pdrs-fixture/project".into()),
            ("provider", "p".into()),
            ("model", "m".into()),
            ("permissionMode", "default".into()),
        ]);
        if let Some(patch) = field(&spec, "config").as_object() {
            for (k, v) in patch.entries() {
                config.insert(k.clone(), v.clone());
            }
        }
        let identity = ContextRequestIdentity::new(
            TurnBinding {
                config: config.clone(),
                input: obj([("sessionId", "session".into()), ("turnId", "turn".into())]),
                checkpoint: HostToolCheckpoint::default(),
                abort_signal: abort,
                environment: Arc::new(Environment),
            },
            ToolContextPorts::default(),
        );
        let context = Handler::new(fake.clone(), identity, config, signal.clone());
        let methods = JsValue::Array(context.methods().into_iter().map(JsValue::from).collect());
        assert_eq!(
            normalized(&methods, &root),
            normalized(field(case, "methods"), &root),
            "methods {name}"
        );
        let mut model =
            model::Handler::with_ports(fake.clone(), None, model::TurnContext::default());
        if field(case, "materializer").as_bool() == Some(true) {
            model.materializer = Some(fake.clone());
        }
        let budget_spec = field(case, "budget");
        let ports = if budget_spec.as_bool() == Some(false) {
            None
        } else {
            Some(budget::Ports {
                evaluate_request_budget: (!budget_spec
                    .as_string()
                    .is_some_and(|value| value.equals_str("no-port")))
                .then(|| fake.clone() as Arc<dyn budget::RequestBudgetEvaluator>),
                ..Default::default()
            })
        };
        let router = ModuleRouter {
            context,
            model,
            budget: budget::Handler { ports, signal },
            ..Default::default()
        };
        if field(case, "prepared").as_bool() == Some(true) {
            router
                .module_call(call(
                    val([
                        ("operation", "prepare".into()),
                        ("preparationId", "known".into()),
                        ("request", request()),
                    ]),
                    "model",
                ))
                .await
                .unwrap();
        }
        if field(case, "clearPrepared")
            .as_string()
            .is_some_and(|s| s.equals_str("close_stream"))
        {
            router
                .module_call(call(
                    val([
                        ("operation", "close_stream".into()),
                        ("preparationId", "known".into()),
                    ]),
                    "model",
                ))
                .await
                .unwrap();
        }
        if field(case, "clearPrepared")
            .as_string()
            .is_some_and(|s| s.equals_str("dispose"))
        {
            router.dispose().await.unwrap();
        }
        let module = field(case, "module")
            .as_string()
            .map(|s| s.to_utf8().unwrap())
            .unwrap_or_else(|| "context".into());
        let response = outcome(
            router
                .module_call(call(field(&spec, "payload").clone(), &module))
                .await,
        );
        let expected = field(case, "response");
        let fields = if field(expected, "ok").as_bool() == Some(true) {
            obj([
                ("ok", JsValue::Bool(true)),
                ("payload", field(expected, "payload").clone()),
            ])
        } else {
            obj([
                ("ok", JsValue::Bool(false)),
                ("code", field(expected, "code").clone()),
                ("error", field(expected, "error").clone()),
            ])
        };
        assert_eq!(
            normalized(&response, &root),
            normalized(&JsValue::Object(fields), &root),
            "response {name}"
        );
        assert_eq!(
            normalized(&JsValue::Array(calls.lock().unwrap().clone()), &root),
            normalized(field(case, "calls"), &root),
            "calls {name}"
        );
        router.dispose().await.unwrap();
    }
}
