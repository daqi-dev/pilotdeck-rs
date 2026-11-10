//! Native signal reaches context memory even when cancellation and completion share one poll.
use pilotdeck_gateway::{
    host::modules::context::{ContextInput, ContextRuntime},
    runtime::{
        context::{
            memory::{
                MemoryCaptureFuture, MemoryCaptureTurnInput, MemoryFuture, MemoryResolver,
                MemoryRetrieveInput, MemoryRetrieveResult,
            },
            DefaultContextRuntime, Options,
        },
        network::Cancellation,
    },
};
use pilotdeck_protocol::{parse_json, stringify_json, JsObject, JsValue};
use std::sync::{Arc, Mutex};

struct CancellingMemory {
    parent: Cancellation,
    child: Arc<Mutex<Option<pilotdeck_gateway::host::Cancellation>>>,
}
impl MemoryResolver for CancellingMemory {
    fn capture_turn(&self, _: MemoryCaptureTurnInput) -> MemoryCaptureFuture {
        Box::pin(async { Ok(()) })
    }
    fn retrieve(&self, input: MemoryRetrieveInput) -> MemoryFuture {
        *self.child.lock().unwrap() = Some(input.signal.clone());
        self.parent.abort();
        Box::pin(async {
            Ok(MemoryRetrieveResult {
                system_context: Some("RESOLVED_MEMORY".into()),
                diagnostics: vec![],
            })
        })
    }
}
#[tokio::test]
async fn native_network_cancel_during_memory_poll_keeps_resolved_result_and_aborts_child() {
    let parent = Cancellation::new();
    let child = Arc::new(Mutex::new(None));
    let runtime = DefaultContextRuntime::new(Options {
        memory_resolver: Some(Arc::new(CancellingMemory {
            parent: parent.clone(),
            child: child.clone(),
        })),
        ..Options::default()
    });
    let fields: JsObject = parse_json(r#"{"cwd":"/fixture","provider":"fixture","model":"mock","permissionMode":"default","additionalWorkingDirectories":[],"tools":[],"messages":[],"protocol":"anthropic","supportsPromptCache":true}"#).unwrap().as_object().unwrap().clone();
    let result: JsValue = runtime
        .prepare_for_model(ContextInput {
            fields,
            abort_signal: Some(Arc::new(parent.clone())),
            budget_evaluator: None,
        })
        .await
        .unwrap();
    assert!(parent.is_cancelled());
    assert!(child
        .lock()
        .unwrap()
        .as_ref()
        .expect("memory was entered")
        .is_cancelled());
    assert!(stringify_json(&result)
        .unwrap()
        .unwrap()
        .contains("RESOLVED_MEMORY"));
}
