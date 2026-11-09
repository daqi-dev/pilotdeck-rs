use super::{common::*, controller::StaffDeckController};
use crate::{
    host::{
        self, HostError, HostFuture,
        modules::{
            capability::{
                ToolCall, ToolCatalog, ToolDefinition, ToolExecution, ToolInvocationContext,
            },
            context,
        },
    },
    module_host::approvals::native_http::{JsonFuture, JsonRequest, JsonResponse, JsonTransport},
    runtime::turn::loop_control::*,
};
use pilotdeck_protocol::{JsObject, JsValue, admission::ExecutionIdentity};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
struct Owner {
    fixtures: JsValue,
    calls: Mutex<Vec<String>>,
}
impl JsonTransport for Owner {
    fn request(&self, input: JsonRequest) -> JsonFuture<'_> {
        let body = if input.method == "GET" {
            field(&self.fixtures, "manifest").clone()
        } else {
            let request = pilotdeck_protocol::parse_json(input.body.as_deref().unwrap()).unwrap();
            let operation = input.url.path().rsplit('/').next().unwrap();
            let mut calls = self.calls.lock().unwrap();
            if operation == "prepare" {
                let suffix = if calls.is_empty() { 0 } else { 7 };
                assert_eq!(
                    field(&request, "requestId"),
                    &JsValue::from(format!("sop.prepare:session:turn:{suffix}").as_str()),
                    "owner request follows actual model step, not repeated preflight id"
                );
            }
            calls.push(operation.into());
            drop(calls);
            let payload = match operation {
                "prepare" => crate::metadata::object([
                    ("state", field(&self.fixtures, "state").clone()),
                    ("step", field(&self.fixtures, "step").clone()),
                ]),
                "submit" => crate::metadata::object([
                    (
                        "state",
                        crate::metadata::object([
                            ("status", "completed".into()),
                            ("active_skill_id", "fixture".into()),
                            ("active_step_id", "node".into()),
                            ("slots_json", JsValue::Object(JsObject::new())),
                        ]),
                    ),
                    ("result", field(&self.fixtures, "result").clone()),
                ]),
                _ => panic!("unexpected owner operation"),
            };
            crate::metadata::object([
                ("protocolVersion", "2.0".into()),
                ("requestId", field(&request, "requestId").clone()),
                ("ok", JsValue::Bool(true)),
                ("outcome", "completed".into()),
                ("payload", payload),
            ])
        };
        Box::pin(async move {
            Ok(JsonResponse {
                status: 200,
                body,
                json_error: None,
            })
        })
    }
}
struct EmptyTools;
impl ToolCatalog for EmptyTools {
    fn list(&self) -> HostFuture<Vec<Arc<dyn ToolDefinition>>> {
        Box::pin(async { Ok(vec![]) })
    }
}
impl ToolExecution for EmptyTools {
    fn execute_all(&self, _: Vec<ToolCall>, _: ToolInvocationContext) -> HostFuture<Vec<JsObject>> {
        Box::pin(async {
            Err(error(
                "UNEXPECTED_DELEGATE",
                "reserved control must not execute through ordinary tools",
            ))
        })
    }
}
struct Context;
impl context::ContextRuntime for Context {
    fn supports(&self, _: context::Operation) -> bool {
        true
    }
    fn prepare_for_model(&self, input: context::ContextInput) -> HostFuture<JsValue> {
        Box::pin(async move { Ok(JsValue::Object(input.fields)) })
    }
}
struct Executor {
    run: AtomicUsize,
    finished: AtomicUsize,
    cleanup: AtomicUsize,
    replies: Mutex<Vec<JsValue>>,
    events: Mutex<Vec<JsValue>>,
}
impl SopNodeExecutor for Executor {
    fn model_validator(&self) -> Result<SopModelValidator, HostError> {
        Ok(Arc::new(|_| Box::pin(async { Ok(()) })))
    }
    fn run<'a>(&'a self, plan: SopNodePlan) -> SopFuture<'a, Result<SopNodeOutcome, HostError>> {
        Box::pin(async move {
            self.run.fetch_add(1, Ordering::SeqCst);
            assert!(!plan.continuation);
            assert!(matches!(plan.context, SopNodeContext::Inherit));
            assert_eq!(
                plan.request
                    .payload
                    .get("agent")
                    .and_then(|a| a.get("stopOnStructuredOutput")),
                Some(&JsValue::Bool(true))
            );
            let binding = plan.binding.expect("actual SOP wrappers");
            let prepared = binding
                .context(Arc::new(Context))
                .prepare_for_model(context::ContextInput {
                    fields: JsObject::new(),
                    abort_signal: None,
                    budget_evaluator: None,
                })
                .await?;
            assert!(
                display(field(&prepared, "appendSystemPrompt"))
                    .contains("Follow fixture instruction.")
            );
            let mut fields = JsObject::new();
            fields.insert("stepId".into(), JsValue::Number(7.0));
            binding
                .context(Arc::new(Context))
                .prepare_for_model(context::ContextInput {
                    fields,
                    abort_signal: None,
                    budget_evaluator: None,
                })
                .await?;
            let tools = binding.tools(Arc::new(EmptyTools));
            let catalog = tools.list().await?;
            assert_eq!(catalog.len(), 1);
            assert_eq!(
                catalog[0].fields().get("name"),
                Some(&JsValue::from("submit_step_result"))
            );
            assert_eq!(
                catalog[0].is_read_only(&JsValue::Null)?,
                JsValue::Bool(true)
            );
            let call = ToolCall {
                id: "sop-control-1".into(),
                name: "submit_step_result".into(),
                input: crate::metadata::object([
                    ("status", "completed".into()),
                    ("replyFragment", "Fixture complete.".into()),
                    ("slotUpdates", JsValue::Object(JsObject::new())),
                ]),
            };
            let results = tools
                .execute_all(
                    vec![call],
                    ToolInvocationContext {
                        runtime: Arc::new(()),
                        execution: Arc::new(()),
                    },
                )
                .await?;
            assert_eq!(results.len(), 1);
            assert_eq!(results[0].get("type"), Some(&JsValue::from("success")));
            assert_eq!(
                results[0]
                    .get("metadata")
                    .and_then(|m| m.get("structuredOutput")),
                Some(&JsValue::Bool(true))
            );
            Ok(SopNodeOutcome {
                result: crate::metadata::object([
                    ("type", "success".into()),
                    (
                        "sessionId",
                        JsValue::String(plan.request.identity.session_id),
                    ),
                    ("turnId", JsValue::String(plan.request.identity.turn_id)),
                    ("stopReason", "completed".into()),
                    ("turns", JsValue::Number(1.0)),
                    (
                        "usage",
                        crate::metadata::object([("inputTokens", JsValue::Number(2.0))]),
                    ),
                    ("permissionDenials", JsValue::Array(vec![])),
                    (
                        "structuredOutput",
                        crate::metadata::object([("submitted", JsValue::Bool(true))]),
                    ),
                ]),
                messages: vec![],
            })
        })
    }
    fn finish_node<'a>(
        &'a self,
        outcome: Option<&'a SopNodeOutcome>,
    ) -> SopFuture<'a, Result<(), HostError>> {
        Box::pin(async move {
            if outcome.is_some() {
                self.finished.fetch_add(1, Ordering::SeqCst);
            } else {
                self.cleanup.fetch_add(1, Ordering::SeqCst);
            }
            Ok(())
        })
    }
    fn append_parent_reply<'a>(&'a self, message: JsValue) -> SopFuture<'a, Result<(), HostError>> {
        Box::pin(async move {
            self.replies.lock().unwrap().push(message);
            Ok(())
        })
    }
    fn emit_parent<'a>(&'a self, event: JsValue) -> SopFuture<'a, Result<(), HostError>> {
        Box::pin(async move {
            self.events.lock().unwrap().push(event);
            Ok(())
        })
    }
}
#[tokio::test]
async fn actual_sop_control_commits_owner_result_before_parent_delivery_and_clears_receipt() {
    let root = std::env::temp_dir().join(format!("pdrs-sop-controller-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).unwrap();
    let definitions = root.join("definitions.json");
    std::fs::write(&definitions,r#"{"sops":[{"id":"fixture","version":"v1","content":{"nodes":[{"node_id":"node","contextMode":"inherit"}]}}]}"#).unwrap();
    let fixed = pilotdeck_protocol::parse_json(include_str!(
        "../../../tests/fixtures/native-sop/client-oracle.json"
    ))
    .unwrap();
    let http = Arc::new(Owner {
        fixtures: field(&fixed, "fixtures").clone(),
        calls: Mutex::new(vec![]),
    });
    let profile = crate::metadata::object([
        ("endpoint", "http://127.0.0.1:1".into()),
        (
            "definitionsPath",
            definitions.to_string_lossy().as_ref().into(),
        ),
        ("defaultSopId", "fixture".into()),
        ("contextMode", "inherit".into()),
        (
            "stateRoot",
            root.join("state").to_string_lossy().as_ref().into(),
        ),
    ]);
    let controller = StaffDeckController::load(profile, http.clone())
        .await
        .unwrap();
    let executor = Executor {
        run: AtomicUsize::new(0),
        finished: AtomicUsize::new(0),
        cleanup: AtomicUsize::new(0),
        replies: Mutex::new(vec![]),
        events: Mutex::new(vec![]),
    };
    let input = SopLoopInput {
        request: host::ExecuteRequest {
            identity: ExecutionIdentity {
                run_id: "run".into(),
                operation_id: "operation".into(),
                request_id: "request".into(),
                session_id: "session".into(),
                turn_id: "turn".into(),
            },
            payload: JsObject::new(),
            idempotency_key: None,
            operation_deadline: None,
        },
        signal: host::Cancellation::default(),
        visible_messages: vec![],
    };
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        controller.run(input, &executor),
    )
    .await
    .unwrap()
    .unwrap();
    let host::ExecuteResult::Terminal(result) = result else {
        panic!("real controller result")
    };
    let payload = field(&result, "payload");
    assert_eq!(
        field(field(field(payload, "result"), "structuredOutput"), "sop"),
        field(&http.fixtures, "result")
    );
    assert_eq!(executor.run.load(Ordering::SeqCst), 1);
    assert_eq!(executor.finished.load(Ordering::SeqCst), 1);
    assert_eq!(executor.cleanup.load(Ordering::SeqCst), 1);
    assert_eq!(executor.replies.lock().unwrap().len(), 1);
    assert_eq!(executor.events.lock().unwrap().len(), 1);
    assert_eq!(
        *http.calls.lock().unwrap(),
        vec!["prepare", "prepare", "submit"]
    );
    let state = std::fs::read(root.join("state/sessions/c2Vzc2lvbg.json")).unwrap();
    let state = pilotdeck_protocol::parse_json(std::str::from_utf8(&state).unwrap()).unwrap();
    assert_eq!(
        field(field(&state, "state"), "status"),
        &JsValue::from("completed")
    );
    assert!(state.get("replyDelivery").is_none());
    std::fs::remove_dir_all(root).unwrap();
}
