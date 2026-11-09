//! Real host context signal type must reach the summary capability.
use pilotdeck_gateway::{
    host::{HostFuture, modules::context::ContextInput},
    runtime::{
        context::compaction::{
            engine::{
                CompactionInput, CompactionResult,
                port::{CompactionPort, CompactionSummaryPort},
            },
            orchestrator::CompactionOrchestrator,
        },
        model::token_accounting::TokenBudgetManager,
        network::Cancellation,
    },
};
use pilotdeck_protocol::parse_json;
use std::sync::{Arc, Mutex};
use tokio::sync::oneshot;

struct Probe(Mutex<Option<oneshot::Sender<Option<Cancellation>>>>);
impl CompactionSummaryPort for Probe {
    fn run(&self, input: CompactionInput) -> HostFuture<CompactionResult> {
        if let Some(tx) = self.0.lock().unwrap().take() {
            let _ = tx.send(input.transport_signal);
        }
        Box::pin(std::future::pending())
    }
}
#[tokio::test]
async fn native_network_abort_is_forwarded_through_the_orchestrator() {
    let (tx, rx) = oneshot::channel();
    let port = CompactionPort {
        budget: Some(Arc::new(TokenBudgetManager::default())),
        summary: Some(Arc::new(Probe(Mutex::new(Some(tx))))),
        ..Default::default()
    };
    let fields = parse_json(r#"{"trigger":"manual","manualForce":true,"sessionId":"s","turnId":"t","messages":[{"role":"user","content":[{"type":"text","text":"one"}]},{"role":"assistant","content":[{"type":"text","text":"two"}]}]}"#).unwrap();
    let signal = Cancellation::new();
    let input = ContextInput {
        fields: fields.as_object().unwrap().clone(),
        abort_signal: Some(Arc::new(signal.clone())),
        budget_evaluator: None,
    };
    let task =
        tokio::spawn(async move { CompactionOrchestrator::new(port, 12000.0).run(input).await });
    let forwarded = tokio::time::timeout(std::time::Duration::from_secs(3), rx)
        .await
        .unwrap()
        .unwrap()
        .expect("native network signal missing");
    signal.abort();
    tokio::time::timeout(std::time::Duration::from_secs(3), forwarded.cancelled())
        .await
        .expect("same native signal not cancelled");
    task.abort();
    let _ = task.await;
}
