//! Compaction summary owns the real model stream and forwards turn cancellation.
use pilotdeck_gateway::{
    host::{
        Cancellation, ModuleError,
        modules::model::{
            ModelExecution, ModelFuture, ModelStream, PrepareInput, PreparedInvocation, StreamInput,
        },
    },
    runtime::context::compaction::engine::{
        CompactionEngine, CompactionEngineOptions, CompactionInput, CompactionResult,
        ProtectedTools,
    },
};
use pilotdeck_protocol::JsValue;
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;

struct Delayed {
    seen: Arc<
        Mutex<Option<oneshot::Sender<Option<pilotdeck_gateway::runtime::network::Cancellation>>>>,
    >,
}
impl ModelExecution for Delayed {
    fn prepare(&self, input: PrepareInput) -> ModelFuture<PreparedInvocation> {
        Box::pin(async move {
            Ok(PreparedInvocation::new(
                input.request,
                "fixture".into(),
                "mock".into(),
            ))
        })
    }
    fn stream(&self, input: StreamInput) -> ModelFuture<Box<dyn ModelStream>> {
        if let Some(sender) = self.seen.lock().unwrap().take() {
            let _ = sender.send(input.abort);
        }
        Box::pin(async move {
            std::future::pending::<Result<Box<dyn ModelStream>, ModuleError>>().await
        })
    }
}
fn messages() -> Vec<JsValue> {
    match pilotdeck_protocol::parse_json(r#"[{"role":"user","content":[{"type":"text","text":"old one"}]},{"role":"assistant","content":[{"type":"text","text":"old answer"}]},{"role":"user","content":[{"type":"text","text":"new question"}]},{"role":"assistant","content":[{"type":"text","text":"new answer"}]}]"#).unwrap() { JsValue::Array(ref messages) => messages.clone(), _ => unreachable!() }
}
fn assert_failure_contract(result: &CompactionResult, id: &str) {
    let oracle = pilotdeck_protocol::parse_json(include_str!(
        "fixtures/compaction-failure-contract/oracle.json"
    ))
    .unwrap();
    let JsValue::Array(cases) = oracle.get("cases").unwrap() else {
        panic!("actual TS cases")
    };
    let case = cases
        .iter()
        .find(|case| case.get("id") == Some(&id.into()))
        .unwrap();
    assert_eq!(
        Some(JsValue::Bool(result.summary_generated)),
        case.get("summaryGenerated").cloned()
    );
    assert!(result.summary_message.is_none());
    assert_eq!(
        result.error.clone().map(JsValue::String),
        case.get("error").cloned()
    );
    assert_eq!(
        Some(JsValue::Array(result.diagnostics.clone())),
        case.get("diagnostics").cloned()
    );
    assert_eq!(Some(&result.boundary_marker), case.get("boundaryMarker"));
    assert_eq!(
        Some(JsValue::Array(result.messages_to_keep.clone())),
        case.get("messagesToKeep").cloned()
    );
}

#[tokio::test]
async fn summary_model_receives_forwarded_cancellation_and_cancellation_settles() {
    let (tx, rx) = oneshot::channel();
    let model = Arc::new(Delayed {
        seen: Arc::new(Mutex::new(Some(tx))),
    });
    let engine = CompactionEngine::new(CompactionEngineOptions::new(
        model,
        "fixture".into(),
        "mock".into(),
    ));
    let signal = Cancellation::default();
    let mut input = CompactionInput::new("manual", messages());
    input.signal = Some(signal.clone());
    input.target_post_tokens = Some(1.0);
    input.protected_tool_names = ProtectedTools::Disabled;
    let task = tokio::spawn(async move { engine.run(input).await });
    let provider_signal = tokio::time::timeout(std::time::Duration::from_secs(3), rx)
        .await
        .unwrap()
        .unwrap()
        .expect("provider receives cancellation");
    signal.cancel(None);
    tokio::time::timeout(
        std::time::Duration::from_secs(3),
        provider_signal.cancelled(),
    )
    .await
    .expect("provider sees caller cancellation");
    let _ = tokio::time::timeout(std::time::Duration::from_secs(3), task)
        .await
        .expect("engine settles")
        .unwrap();
}

struct FailingStream {
    seen: Arc<Mutex<Option<pilotdeck_gateway::runtime::network::Cancellation>>>,
}
impl ModelExecution for FailingStream {
    fn prepare(&self, input: PrepareInput) -> ModelFuture<PreparedInvocation> {
        Box::pin(async move {
            Ok(PreparedInvocation::new(
                input.request,
                "fixture".into(),
                "mock".into(),
            ))
        })
    }
    fn stream(&self, input: StreamInput) -> ModelFuture<Box<dyn ModelStream>> {
        *self.seen.lock().unwrap() = input.abort;
        Box::pin(async { Err(ModuleError::new("mock stream refused")) })
    }
}
#[tokio::test]
async fn summary_failure_aborts_owned_provider_child_without_aborting_turn() {
    let seen = Arc::new(Mutex::new(None));
    let engine = CompactionEngine::new(CompactionEngineOptions::new(
        Arc::new(FailingStream { seen: seen.clone() }),
        "fixture".into(),
        "mock".into(),
    ));
    let parent = pilotdeck_gateway::runtime::network::Cancellation::new();
    let mut input = CompactionInput::new("manual", messages());
    input.transport_signal = Some(parent.clone());
    input.target_post_tokens = Some(1.0);
    input.protected_tool_names = ProtectedTools::Disabled;
    let result = engine
        .run(input)
        .await
        .expect("TS resolves a structured summary failure");
    assert_failure_contract(&result, "stream-failure");
    assert!(
        !parent.is_cancelled(),
        "summary error must not abort its owning turn"
    );
    assert!(
        seen.lock()
            .unwrap()
            .as_ref()
            .expect("provider got owned child")
            .is_cancelled()
    );
}
#[tokio::test]
async fn native_parent_cancellation_reaches_owned_provider_child() {
    let (tx, rx) = oneshot::channel();
    let model = Arc::new(Delayed {
        seen: Arc::new(Mutex::new(Some(tx))),
    });
    let engine = CompactionEngine::new(CompactionEngineOptions::new(
        model,
        "fixture".into(),
        "mock".into(),
    ));
    let parent = pilotdeck_gateway::runtime::network::Cancellation::new();
    let mut input = CompactionInput::new("manual", messages());
    input.transport_signal = Some(parent.clone());
    input.target_post_tokens = Some(1.0);
    input.protected_tool_names = ProtectedTools::Disabled;
    let task = tokio::spawn(async move { engine.run(input).await });
    let child = tokio::time::timeout(std::time::Duration::from_secs(3), rx)
        .await
        .unwrap()
        .unwrap()
        .expect("provider got child");
    assert!(!child.is_cancelled());
    parent.abort();
    tokio::time::timeout(std::time::Duration::from_secs(3), child.cancelled())
        .await
        .expect("child cancellation");
    let result = tokio::time::timeout(std::time::Duration::from_secs(3), task)
        .await
        .expect("engine settled")
        .unwrap()
        .expect("TS records summary cancellation in its result");
    assert_failure_contract(&result, "parent-cancellation");
}

struct BrokenIterator;
impl ModelStream for BrokenIterator {
    fn next(
        &mut self,
    ) -> pilotdeck_gateway::host::modules::model::StreamFuture<'_, Option<JsValue>> {
        Box::pin(async { Err(ModuleError::new("mock next failed")) })
    }
}
struct IteratorFailure {
    seen: Arc<Mutex<Option<pilotdeck_gateway::runtime::network::Cancellation>>>,
}
impl ModelExecution for IteratorFailure {
    fn prepare(&self, input: PrepareInput) -> ModelFuture<PreparedInvocation> {
        Box::pin(async move {
            Ok(PreparedInvocation::new(
                input.request,
                "fixture".into(),
                "mock".into(),
            ))
        })
    }
    fn stream(&self, input: StreamInput) -> ModelFuture<Box<dyn ModelStream>> {
        *self.seen.lock().unwrap() = input.abort;
        Box::pin(async { Ok(Box::new(BrokenIterator) as Box<dyn ModelStream>) })
    }
}
#[tokio::test]
async fn summary_iterator_failure_aborts_child_and_keeps_parent_alive() {
    let seen = Arc::new(Mutex::new(None));
    let engine = CompactionEngine::new(CompactionEngineOptions::new(
        Arc::new(IteratorFailure { seen: seen.clone() }),
        "fixture".into(),
        "mock".into(),
    ));
    let parent = pilotdeck_gateway::runtime::network::Cancellation::new();
    let mut input = CompactionInput::new("manual", messages());
    input.transport_signal = Some(parent.clone());
    input.target_post_tokens = Some(1.0);
    input.protected_tool_names = ProtectedTools::Disabled;
    let result = engine
        .run(input)
        .await
        .expect("TS resolves a structured summary failure");
    assert_failure_contract(&result, "iterator-failure");
    assert!(!parent.is_cancelled());
    assert!(seen.lock().unwrap().as_ref().unwrap().is_cancelled());
}
