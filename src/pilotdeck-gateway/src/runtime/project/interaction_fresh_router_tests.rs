//! Caller contract: fresh non-interaction handlers on each actual factory call.
//! This tests host assembly, not a promise that bind sanitizes arbitrary ports.
use super::*;
use crate::host::modules::{budget, context, tool_context};

struct Probe {
    identity: ExecutionIdentity,
    stamp: String,
    signal: Cancellation,
    observed: Arc<Mutex<Vec<String>>>,
}
fn object<const N: usize>(fields: [(&str, JsValue); N]) -> JsObject {
    let mut value = JsObject::new();
    for (key, field) in fields {
        value.insert(key.into(), field);
    }
    value
}
impl Probe {
    fn fields(&self, signal: Option<&Cancellation>) -> JsObject {
        object([
            ("stamp", self.stamp.as_str().into()),
            ("cancelled", JsValue::Bool(signal.unwrap().is_cancelled())),
        ])
    }
}
impl model::ModelExecution for Probe {
    fn prepare(&self, input: model::PrepareInput) -> model::ModelFuture<model::PreparedInvocation> {
        let mut request = input.request.as_object().unwrap().clone();
        request.insert("context".into(), JsValue::Object(input.context));
        request.insert("stamp".into(), self.stamp.as_str().into());
        Box::pin(async move {
            Ok(model::PreparedInvocation::new(
                JsValue::Object(request),
                "provider".into(),
                "model".into(),
            ))
        })
    }
    fn stream(&self, input: model::StreamInput) -> model::ModelFuture<Box<dyn model::ModelStream>> {
        let mut fields = self.fields(input.abort.as_ref());
        fields.insert("context".into(), JsValue::Object(input.context));
        Box::pin(async move {
            Ok(Box::new(ProbeStream(Some(JsValue::Object(fields)))) as Box<dyn model::ModelStream>)
        })
    }
}
struct ProbeStream(Option<JsValue>);
impl model::ModelStream for ProbeStream {
    fn next(&mut self) -> model::StreamFuture<'_, Option<JsValue>> {
        Box::pin(async { Ok(self.0.take()) })
    }
}
impl budget::RequestBudgetEvaluator for Probe {
    fn evaluate_request_budget(
        &self,
        _: JsObject,
        options: budget::EvaluationOptions,
    ) -> HostFuture<JsValue> {
        let value = self.fields(options.signal.as_ref());
        Box::pin(async move { Ok(JsValue::Object(value)) })
    }
}
impl context::ContextRuntime for Probe {
    fn supports(&self, _: context::Operation) -> bool {
        false
    }
    fn prepare_for_model(&self, input: context::ContextInput) -> HostFuture<JsValue> {
        let mut value = self.fields(input.abort_signal.as_ref().unwrap().downcast_ref());
        value.insert("identity".into(), JsValue::Object(input.fields));
        Box::pin(async move { Ok(JsValue::Object(value)) })
    }
}
impl capability::ToolCatalog for Probe {
    fn list(&self) -> HostFuture<Vec<Arc<dyn capability::ToolDefinition>>> {
        Box::pin(async { Ok(vec![]) })
    }
}
impl capability::ToolExecution for Probe {
    fn execute_all(
        &self,
        _: Vec<capability::ToolCall>,
        context: capability::ToolInvocationContext,
    ) -> HostFuture<Vec<JsObject>> {
        let value = object([
            ("stamp", self.stamp.as_str().into()),
            (
                "context",
                context
                    .runtime
                    .downcast_ref::<String>()
                    .unwrap()
                    .as_str()
                    .into(),
            ),
        ]);
        Box::pin(async move { Ok(vec![value]) })
    }
}
impl capability::ToolContext for Probe {
    fn prepare(&self, _: JsValue) -> HostFuture<capability::ToolInvocationContext> {
        let stamp = self.stamp.clone();
        Box::pin(async move {
            Ok(capability::ToolInvocationContext {
                runtime: Arc::new(stamp),
                execution: Arc::new(()),
            })
        })
    }
}
impl capability::CapabilityResultObserver for Probe {
    fn on_capability_results(&self, _: Vec<JsObject>) -> HostFuture<()> {
        self.observed.lock().unwrap().push(self.stamp.clone());
        Box::pin(async { Ok(()) })
    }
}
impl capability::PlanTodoHandler for Probe {
    fn call(&self, _: JsValue) -> HostFuture<JsObject> {
        let value = object([("stamp", self.stamp.as_str().into())]);
        Box::pin(async move { Ok(value) })
    }
}
struct NoEnvironment;
impl tool_context::TurnEnvironment for NoEnvironment {
    fn build(
        &self,
        _: Option<&JsValue>,
        _: &JsString,
        _: &JsString,
        _: &JsString,
    ) -> Result<JsObject, HostError> {
        panic!("context identity must not request a tool environment")
    }
}
fn model_handler(probe: &Arc<Probe>) -> model::Handler {
    model::Handler::with_ports(
        probe.clone(),
        None,
        model::TurnContext {
            session_id: probe.identity.session_id.clone(),
            turn_id: probe.identity.turn_id.clone(),
            abort: Some(probe.signal.clone()),
            ..Default::default()
        },
    )
}
fn budget_handler(probe: &Arc<Probe>) -> budget::Handler {
    budget::Handler {
        ports: Some(budget::Ports {
            evaluate_request_budget: Some(probe.clone()),
            ..Default::default()
        }),
        signal: Some(probe.signal.clone()),
    }
}
fn context_handler(probe: &Arc<Probe>) -> context::Handler {
    context::Handler::new(
        probe.clone(),
        tool_context::ContextRequestIdentity::new(
            tool_context::TurnBinding {
                input: object([
                    (
                        "sessionId",
                        JsValue::String(probe.identity.session_id.clone()),
                    ),
                    ("turnId", JsValue::String(probe.identity.turn_id.clone())),
                ]),
                config: JsObject::new(),
                checkpoint: tool_context::HostToolCheckpoint::new(None, &[]).unwrap(),
                abort_signal: Some(Arc::new(probe.signal.clone())),
                environment: Arc::new(NoEnvironment),
            },
            Default::default(),
        ),
        JsObject::new(),
        Some(probe.signal.clone()),
    )
}
fn fresh_router(
    current: &Arc<Probe>,
    _previous: Option<&(Arc<Probe>, model::Handler)>,
) -> ModuleRouter {
    // Each line has a paired mutation in the fix-r2 evidence. Reusing a new
    // outer router with any of these old bindings violates the same contract.
    let model = model_handler(current);
    let budget = budget_handler(current);
    let context = context_handler(current);
    let execution = current.clone();
    let tool_context = current.clone();
    let observer = current.clone();
    let plan = current.clone();
    ModuleRouter {
        model,
        budget,
        context,
        capability: capability::Handler::new(execution, tool_context, observer)
            .with_plan_todo(plan),
        ..Default::default()
    }
}

#[tokio::test]
async fn factory_assembles_fresh_handlers_for_next_turn() {
    assert_fresh_handlers(false).await;
}

#[tokio::test]
async fn factory_assembles_fresh_handlers_for_new_generation_with_same_identity() {
    assert_fresh_handlers(true).await;
}

async fn assert_fresh_handlers(reload: bool) {
    let scratch = Scratch::new();
    let registry = Arc::new(interaction_registry(&scratch));
    let saved = Arc::new(Mutex::new(Vec::<(Arc<Probe>, model::Handler)>::new()));
    let observed = Arc::new(Mutex::new(Vec::new()));
    let session = LiveSessionAdapter::with_interaction(test_recorder(), Vec::new, {
        let registry = registry.clone();
        let saved = saved.clone();
        let observed = observed.clone();
        move |identity, _| {
            let lease = registry.acquire(None).unwrap();
            let mut saved = saved.lock().unwrap();
            let current = Arc::new(Probe {
                identity,
                stamp: format!("{:p}/{}", Arc::as_ptr(lease.runtime()), saved.len()),
                signal: Cancellation::new(),
                observed: observed.clone(),
            });
            let router = fresh_router(&current, saved.last());
            saved.push((current.clone(), router.model.clone()));
            let mut input = options();
            input.signal = current.signal.clone();
            input.switches.plan_todo = true;
            Box::pin(async move {
                Ok(InteractionHost {
                    client: client("live-fresh-router").await?,
                    lease,
                    router,
                    input,
                })
            })
        }
    });
    let initial = registry.resolve(None).unwrap();
    for round in 0..2 {
        if reload && round == 1 {
            bounded(registry.reload()).await.unwrap();
            assert!(!Arc::ptr_eq(&initial, &registry.resolve(None).unwrap()));
        } else {
            assert!(Arc::ptr_eq(&initial, &registry.resolve(None).unwrap()));
        }
        // Reload deliberately repeats the exact identity: identity equality
        // alone cannot prove that a handler belongs to the leased generation.
        let turn_id = if reload || round == 0 {
            "first"
        } else {
            "next"
        };
        let wire = JsValue::Object(object([
            ("sessionId", "session".into()),
            ("turnId", turn_id.into()),
            ("runId", "run".into()),
        ]));
        let mut stream = session
            .submit(
                parse_json(r#"{"type":"blocks","content":[]}"#).unwrap(),
                wire,
            )
            .unwrap();
        let mut report = None;
        while let Some(event) = bounded(stream.next()).await.unwrap() {
            if let Some(value) = event.get("observed") {
                report = Some(value.clone());
            }
        }
        let report = report.expect("peer must exercise every non-interaction module");
        let (probe, model) = saved.lock().unwrap()[round].clone();
        assert_eq!(
            saved.lock().unwrap().len(),
            round + 1,
            "factory must run once per turn"
        );
        assert_eq!(
            report.get("oldPreparationRejected"),
            Some(&JsValue::Bool(true)),
            "old model preparation leaked: {report:?}"
        );
        assert_eq!(
            report.get("oldPreparationError"),
            Some(&"Unknown sidecar model preparation: old-only".into()),
            "an unrelated error cannot prove model state isolation"
        );
        for name in ["model", "stream", "budget", "context", "capability", "plan"] {
            let value = report.get(name).unwrap();
            assert_eq!(
                value.get("stamp"),
                Some(&probe.stamp.as_str().into()),
                "stale {name}: {report:?}"
            );
        }
        for name in ["stream", "budget", "context"] {
            assert_eq!(
                report.get(name).unwrap().get("cancelled"),
                Some(&JsValue::Bool(false)),
                "stale {name} cancellation: {report:?}"
            );
        }
        for (name, field) in [
            ("model", "context"),
            ("stream", "context"),
            ("context", "identity"),
        ] {
            let value = report.get(name).unwrap().get(field).unwrap();
            assert_eq!(value.get("sessionId"), Some(&"session".into()));
            assert_eq!(
                value.get("turnId"),
                Some(&turn_id.into()),
                "stale {name} identity"
            );
        }
        assert_eq!(
            report.get("capability").unwrap().get("context"),
            Some(&probe.stamp.as_str().into()),
            "stale capability context"
        );
        assert_eq!(
            observed.lock().unwrap().last(),
            Some(&probe.stamp),
            "stale capability observer"
        );
        assert_eq!(observed.lock().unwrap().len(), round + 1);
        // Wait for real host cleanup, then model a late old callback. A clone
        // of this handler now carries real prepared state into the next turn.
        bounded(async {
            while registry.resolve(None).unwrap().lease_count() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await;
        let prepared = model.call(parse_json(r#"{"payload":{"operation":"prepare","preparationId":"old-only","request":{"provider":"provider","model":"model","messages":[]}}}"#).unwrap()).await.unwrap();
        assert_eq!(
            prepared
                .get("prepared")
                .unwrap()
                .get("request")
                .unwrap()
                .get("stamp"),
            Some(&probe.stamp.as_str().into())
        );
        probe.signal.cancel();
    }
    bounded(session.dispose()).await.unwrap();
    bounded(registry.dispose()).await.unwrap();
}
