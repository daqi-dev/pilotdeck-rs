//! Compaction summary owns the real model stream and forwards turn cancellation.
use pilotdeck_gateway::{
    host::{
        Cancellation, ModuleError,
        modules::model::{
            ModelExecution, ModelFuture, ModelStream, PrepareInput, PreparedInvocation, StreamInput,
        },
    },
    runtime::context::compaction::engine::{
        CompactionEngine, CompactionEngineOptions, CompactionInput, ProtectedTools,
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
