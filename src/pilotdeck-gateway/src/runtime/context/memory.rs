//! Failure-tolerant memory attachments (`MemoryAttachmentBuilder.ts:25-89`).
//! The provider is injected; this layer owns only request cancellation, timeout
//! and attachment rendering, never the provider's retrieval/truncation policy.
use crate::{host::Cancellation, metadata::js_trim};
use pilotdeck_protocol::JsValue;
use std::{future::Future, pin::Pin, time::Duration};

pub struct MemoryRetrieveInput {
    pub query: String,
    pub session_id: String,
    pub project_root: String,
    pub recent_messages: Vec<JsValue>,
    pub signal: Cancellation,
}
#[derive(Clone, Debug)]
pub struct MemoryDiagnostic {
    pub code: String,
    pub severity: String,
    pub message: String,
}
#[derive(Default)]
pub struct MemoryRetrieveResult {
    pub system_context: Option<String>,
    pub diagnostics: Vec<MemoryDiagnostic>,
}
pub type MemoryFuture =
    Pin<Box<dyn Future<Output = Result<MemoryRetrieveResult, String>> + Send + 'static>>;
pub trait MemoryResolver: Send + Sync {
    fn retrieve(&self, input: MemoryRetrieveInput) -> MemoryFuture;
}

pub(super) async fn build(
    resolver: &dyn MemoryResolver,
    mut input: MemoryRetrieveInput,
    parent: Option<&Cancellation>,
    timeout_ms: f64,
) -> (Option<String>, Vec<MemoryDiagnostic>) {
    if parent.is_some_and(Cancellation::is_cancelled) {
        return (None, Vec::new());
    }
    let controller = Cancellation::default();
    input.signal = controller.clone();
    let retrieve = resolver.retrieve(input);
    let cancelled = async {
        match parent {
            Some(parent) => parent.cancelled().await,
            None => std::future::pending().await,
        }
    };
    let timeout = async {
        if timeout_ms > 0.0 {
            let delay = crate::runtime::network::node_timer_delay_ms(timeout_ms);
            tokio::time::sleep(Duration::from_millis(delay as u64)).await;
        } else {
            std::future::pending().await
        }
    };
    let result = tokio::select! {
        biased;
        result = retrieve => result,
        () = cancelled => {
            controller.cancel(None);
            return (None, Vec::new());
        }
        () = timeout => {
            controller.cancel(None);
            Err(format!("MemoryResolver.retrieve timed out after {}ms.", crate::js_value::display(&JsValue::Number(timeout_ms)).unwrap_or_default()))
        }
    };
    // A provider can abort the parent and resolve in the same poll. TS's
    // forwarding listener has already aborted the child in that case.
    if parent.is_some_and(Cancellation::is_cancelled) {
        controller.cancel(None);
    }
    match result {
        Ok(result) => {
            let context = result
                .system_context
                .as_deref()
                .map(js_trim)
                .filter(|s| !s.is_empty())
                .map(|s| format!("<memory-context>\n{s}\n</memory-context>"));
            (context, result.diagnostics)
        }
        Err(error) => {
            if parent.is_some_and(Cancellation::is_cancelled) {
                return (None, Vec::new());
            }
            let message = if controller.is_cancelled() {
                error
            } else {
                format!("MemoryResolver.retrieve failed: {error}")
            };
            (
                None,
                vec![MemoryDiagnostic {
                    code: "memory_provider_error".into(),
                    severity: "warning".into(),
                    message,
                }],
            )
        }
    }
}

pub(super) fn recent_user_text(messages: &[JsValue]) -> String {
    for message in messages.iter().rev() {
        if !message
            .get("role")
            .and_then(JsValue::as_string)
            .is_some_and(|s| s.equals_str("user"))
        {
            continue;
        }
        if let Some(JsValue::Array(content)) = message.get("content") {
            for block in content {
                if block
                    .get("type")
                    .and_then(JsValue::as_string)
                    .is_some_and(|s| s.equals_str("text"))
                    && let Some(text) = block.get("text").and_then(JsValue::as_string)
                {
                    let text = String::from_utf16_lossy(text.units());
                    if !js_trim(&text).is_empty() {
                        return text;
                    }
                }
            }
        }
    }
    String::new()
}
