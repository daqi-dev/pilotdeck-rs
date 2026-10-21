//! Request budget evaluation against the fixed TS TokenAccountingRuntime.
use super::count_body::{count_request, field, object};
use super::{HyperCountTransport, ProviderCountTransport};
use crate::host::modules::budget::{EvaluationOptions, RequestBudgetEvaluator, accounting_request};
use crate::host::{HostError, HostFuture, ModuleError};
use crate::model_cost;
use crate::runtime::network::{
    Cancellation, Exchange, exchange_with_prearmed_parent, node_timer_duration,
};
use pilotdeck_protocol::{
    JsObject, JsValue,
    model::{ContentBlock, ContentBody, Message},
    stringify_json,
};
use sha2::{Digest, Sha256};
use std::{collections::VecDeque, sync::Arc, time::Duration};
use tokio::sync::Mutex;

#[derive(Clone, Debug)]
pub struct TokenBudgetManager {
    pub multimedia_tokens: f64,
    pub per_message_overhead: f64,
    pub warning_ratio: f64,
    pub blocking_ratio: f64,
}
impl Default for TokenBudgetManager {
    fn default() -> Self {
        Self {
            multimedia_tokens: 2000.0,
            per_message_overhead: 4.0,
            warning_ratio: 0.8,
            blocking_ratio: 0.9,
        }
    }
}
impl TokenBudgetManager {
    pub fn estimate_text_tokens(
        &self,
        text: &pilotdeck_protocol::JsString,
    ) -> Result<f64, HostError> {
        pilotdeck_tokenizer::count_tokens(text)
            .map(|n| n as f64)
            .map_err(|e| ModuleError::new(format!("{e:?}")).into())
    }
    pub fn estimate_block_tokens(&self, block: &ContentBlock) -> Result<f64, HostError> {
        match block.body {
            ContentBody::Image(_)
            | ContentBody::Pdf(_)
            | ContentBody::Audio(_)
            | ContentBody::MediaReference(_) => Ok(self.multimedia_tokens),
            _ => model_cost::estimate_block_tokens(block)
                .map(|n| n as f64)
                .map_err(|e| ModuleError::new(format!("{e:?}")).into()),
        }
    }
    pub fn estimate_messages_tokens(&self, messages: &[Message]) -> Result<f64, HostError> {
        let mut total = 0.0;
        for message in messages {
            let mut count = self.per_message_overhead;
            for block in &message.content {
                count += self.estimate_block_tokens(block)?;
            }
            total += count;
        }
        Ok(total)
    }
    pub fn evaluate(
        &self,
        messages: &[Message],
        context: f64,
        reserved: Option<f64>,
    ) -> Result<JsValue, HostError> {
        let mut metadata = JsObject::new();
        if let Some(reserved) = reserved {
            metadata.insert("reservedOutputTokens".into(), JsValue::Number(reserved));
        }
        Ok(self.snapshot_from_tokens(self.estimate_messages_tokens(messages)?, context, &metadata))
    }

    pub fn estimate_request_breakdown(&self, request: &JsObject) -> Result<JsValue, HostError> {
        let (request, extra) = accounting_request(request)?;
        let counts = model_cost::estimate_request_breakdown(&request)
            .map_err(|e| ModuleError::new(format!("{e:?}")))?;
        let messages = if self.multimedia_tokens == 2000.0 && self.per_message_overhead == 4.0 {
            counts.messages as f64
        } else {
            self.estimate_messages_tokens(&request.messages)?
        };
        let tools = (counts.tools + extra) as f64;
        Ok(object([
            ("source", "local_estimate".into()),
            (
                "total",
                JsValue::Number(
                    (counts.system + counts.mcp + counts.memory) as f64 + messages + tools,
                ),
            ),
            ("system", JsValue::Number(counts.system as f64)),
            ("tools", JsValue::Number(tools)),
            ("messages", JsValue::Number(messages)),
            ("mcp", JsValue::Number(counts.mcp as f64)),
            ("memory", JsValue::Number(counts.memory as f64)),
        ]))
    }

    /// Metadata matches snapshotFromTokens; absent properties remain absent.
    pub fn snapshot_from_tokens(&self, tokens: f64, context: f64, metadata: &JsObject) -> JsValue {
        let reserved = js_max(
            metadata
                .get("reservedOutputTokens")
                .and_then(JsValue::as_number)
                .unwrap_or(0.0)
                .floor(),
            0.0,
        );
        let total = js_max(context.floor(), 1.0);
        let budget = if context.is_finite() && context > 0.0 {
            js_max(
                context.floor() - if reserved.is_finite() { reserved } else { 0.0 },
                1.0,
            )
        } else {
            context
        };
        let ratio = if budget > 0.0 { tokens / budget } else { 0.0 };
        let state = if ratio >= self.blocking_ratio {
            "blocking"
        } else if ratio >= self.warning_ratio {
            "warning"
        } else {
            "ok"
        };
        let mut result = JsObject::new();
        result.insert("tokens".into(), JsValue::Number(tokens));
        for key in ["localEstimateTokens", "displayTokens"] {
            if let Some(value) = metadata.get(key).filter(|v| !v.is_undefined())
                && (key != "displayTokens" || value.as_number() != Some(tokens))
            {
                result.insert(key.into(), value.clone());
            }
        }
        let usage = metadata
            .get("usageTokens")
            .is_some_and(|v| !v.is_undefined());
        result.insert(
            "estimateSource".into(),
            if usage { "usage" } else { "estimator" }.into(),
        );
        for key in [
            "usageTokens",
            "calibrationActualInputTokens",
            "calibrationEstimatedInputTokens",
            "breakdown",
        ] {
            if let Some(value) = metadata.get(key).filter(|v| !v.is_undefined()) {
                result.insert(key.into(), value.clone());
            }
        }
        for (key, number) in [
            ("totalContextTokens", total),
            ("maxContextTokens", budget),
            ("effectiveContextTokens", budget),
            ("maxOutputTokens", reserved),
            ("warningRatio", self.warning_ratio),
            ("blockingRatio", self.blocking_ratio),
        ] {
            result.insert(key.into(), JsValue::Number(number));
        }
        result.insert("state".into(), state.into());
        result.insert("ratio".into(), JsValue::Number(ratio));
        for key in ["source", "exact"] {
            if let Some(value) = metadata.get(key) {
                result.insert(key.into(), value.clone());
            }
        }
        result.insert("reservedOutputTokens".into(), JsValue::Number(reserved));
        if let Some(value) = metadata.get("estimatorError") {
            result.insert("estimatorError".into(), value.clone());
        }
        JsValue::Object(result)
    }
}

type CountCache = Arc<Mutex<VecDeque<([u8; 32], f64)>>>;

#[derive(Clone)]
pub struct TokenAccountingRuntime {
    config: Arc<JsObject>,
    manager: TokenBudgetManager,
    transport: Arc<dyn ProviderCountTransport>,
    timeout: Duration,
    cache_size: usize,
    cache: CountCache,
}
impl Default for TokenAccountingRuntime {
    fn default() -> Self {
        Self::new(
            object([("providers", object([]))])
                .as_object()
                .expect("config object")
                .clone(),
        )
    }
}
impl TokenAccountingRuntime {
    /// Receives an already resolved ModelConfig (including providers/models).
    pub fn new(config: JsObject) -> Self {
        Self {
            config: Arc::new(config),
            manager: TokenBudgetManager::default(),
            transport: Arc::new(HyperCountTransport::default()),
            timeout: Duration::from_millis(1500),
            cache_size: 256,
            cache: Arc::default(),
        }
    }
    pub fn with_transport(mut self, transport: Arc<dyn ProviderCountTransport>) -> Self {
        self.transport = transport;
        self
    }
    pub fn with_manager(mut self, manager: TokenBudgetManager) -> Self {
        self.manager = manager;
        self
    }
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
    pub fn with_cache_size(mut self, size: usize) -> Self {
        self.cache_size = size;
        self
    }

    async fn evaluate(
        &self,
        request: JsObject,
        options: EvaluationOptions,
    ) -> Result<JsValue, HostError> {
        let manager = self.manager.clone();
        let raw = request.clone();
        // Tokenizer initialization/BPE and request serialization are CPU work,
        // kept off the async handler thread even on a current-thread runtime.
        let needs_key = options.use_provider_count != Some(false) && self.cache_size > 0;
        let (breakdown, key) = super::cpu::run(move || {
            Ok::<_, HostError>((
                manager.estimate_request_breakdown(&raw)?,
                if needs_key { cache_key(&raw)? } else { [0; 32] },
            ))
        })
        .await?;
        let local = field(&breakdown, "total").as_number().unwrap_or(0.0);
        let mut tokens = local;
        let mut source = "local";
        let mut error = None;
        if options.use_provider_count != Some(false) {
            let cached = if self.cache_size == 0 {
                None
            } else {
                let mut cache = self.cache.lock().await;
                cache
                    .iter()
                    .position(|(k, _)| k == &key)
                    .and_then(|index| cache.remove(index))
                    .map(|entry| {
                        cache.push_back(entry);
                        entry.1
                    })
            };
            let counted = if let Some(cached) = cached {
                Ok(Some(cached))
            } else {
                self.provider_count(request.clone(), options.signal).await
            };
            match counted {
                Ok(Some(count)) => {
                    tokens = count;
                    source = "provider";
                    if self.cache_size > 0 {
                        let mut cache = self.cache.lock().await;
                        cache.retain(|(k, _)| k != &key);
                        cache.push_back((key, count));
                        while cache.len() > self.cache_size {
                            cache.pop_front();
                        }
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    error = Some(
                        e.module_error
                            .map(|e| e.message)
                            .unwrap_or_else(|| e.message.into()),
                    )
                }
            }
        }
        let mut metadata = JsObject::new();
        if source != "provider"
            && let Some(calibration) = options.calibration.as_ref()
            && ["provider", "model"]
                .iter()
                .all(|key| calibration.get(key) == request.get(key))
            && let (Some(actual), Some(estimated)) = (
                positive(calibration.get("actualInputTokens")),
                positive(calibration.get("estimatedInputTokens")),
            )
        {
            let maximum = js_max((local * 0.5).floor(), 1.0);
            tokens = js_max(
                (local + js_min(maximum, js_max(-maximum, actual - estimated)) + 0.5).floor(),
                1.0,
            );
            source = "calibrated";
            metadata.insert(
                "calibrationActualInputTokens".into(),
                JsValue::Number(actual),
            );
            metadata.insert(
                "calibrationEstimatedInputTokens".into(),
                JsValue::Number(estimated),
            );
        }
        metadata.insert("localEstimateTokens".into(), JsValue::Number(local));
        metadata.insert("source".into(), source.into());
        metadata.insert("exact".into(), JsValue::Bool(source == "provider"));
        metadata.insert("breakdown".into(), breakdown);
        if source != "local" {
            metadata.insert("usageTokens".into(), JsValue::Number(tokens));
        }
        if let Some(error) = error {
            metadata.insert("estimatorError".into(), JsValue::String(error));
        }
        if let Some(reserved) = options.reserved_output_tokens {
            metadata.insert("reservedOutputTokens".into(), JsValue::Number(reserved));
        }
        Ok(self
            .manager
            .snapshot_from_tokens(tokens, options.max_context_tokens, &metadata))
    }

    async fn provider_count(
        &self,
        request: JsObject,
        signal: Option<Cancellation>,
    ) -> Result<Option<f64>, HostError> {
        let config = self.config.clone();
        let request = super::cpu::run(move || count_request(&config, &request)).await?;
        let Some(request) = request else {
            return Ok(None);
        };
        // TS arms the outer timer before calling fetch. Fulfilled promises
        // keep their result through synchronous abort; undispatched response
        // timers yield to it, even when their deadlines have already passed.
        // A caller's prearmed parent timer wins an exact response-deadline tie.
        let deadline =
            tokio::time::Instant::now() + node_timer_duration(self.timeout.as_secs_f64() * 1000.0);
        let raw = match exchange_with_prearmed_parent(
            self.transport.post(request),
            async {
                match &signal {
                    Some(signal) => signal.cancelled().await,
                    None => std::future::pending().await,
                }
            },
            || signal.as_ref().and_then(Cancellation::abort_event_at),
            Some(deadline),
        )
        .await
        {
            Exchange::Response(result, _) => result?,
            Exchange::Aborted | Exchange::Timeout => {
                return Err(ModuleError::new("This operation was aborted").into());
            }
        };
        for key in ["input_tokens", "inputTokens", "tokens"] {
            if let Some(value) = raw
                .as_object()
                .and_then(|o| o.get(key))
                .and_then(JsValue::as_number)
                .filter(|n| n.is_finite())
            {
                return Ok(Some(value));
            }
        }
        Err(ModuleError::new("Provider token count response did not include input_tokens.").into())
    }
}
impl RequestBudgetEvaluator for TokenAccountingRuntime {
    fn evaluate_request_budget(
        &self,
        request: JsObject,
        options: EvaluationOptions,
    ) -> HostFuture<JsValue> {
        let runtime = self.clone();
        Box::pin(async move { runtime.evaluate(request, options).await })
    }
}
fn positive(value: Option<&JsValue>) -> Option<f64> {
    value
        .and_then(JsValue::as_number)
        .filter(|v| v.is_finite() && *v > 0.0)
}
fn cache_key(request: &JsObject) -> Result<[u8; 32], HostError> {
    let mut projected = JsObject::new();
    for key in [
        "provider",
        "model",
        "messages",
        "systemPrompt",
        "tools",
        "toolChoice",
        "thinking",
        "outputSchema",
        "cacheBreakpoints",
        "cachePlan",
    ] {
        if let Some(value) = request.get(key) {
            projected.insert(key.into(), value.clone());
        }
    }
    let serialized = stringify_json(&super::json_walk::transform_with_limit(
        &JsValue::Object(projected),
        true,
        super::json_walk::CACHE_KEY_MAX_DEPTH,
        |o| o,
    )?)
    .map_err(|e| ModuleError::new(e.to_string()))?
    .unwrap_or_default();
    Ok(Sha256::digest(serialized.as_bytes()).into())
}

fn js_max(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else {
        a.max(b)
    }
}
fn js_min(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        f64::NAN
    } else {
        a.min(b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::model::{CountFuture, CountHttpRequest};
    use std::{
        future::Future,
        sync::atomic::{AtomicBool, Ordering},
        task::Poll,
    };

    fn configured_count(
        transport: impl ProviderCountTransport + 'static,
    ) -> TokenAccountingRuntime {
        TokenAccountingRuntime::new(
            pilotdeck_protocol::parse_json(
                r#"{"providers":{"p":{"id":"p","protocol":"anthropic","url":"http://example.test","apiKey":"fixture","headers":{},"models":{"m":{"id":"m"}}}}}"#,
            )
            .unwrap()
            .as_object()
            .unwrap()
            .clone(),
        )
        .with_transport(Arc::new(transport))
        .with_timeout(Duration::from_millis(100))
    }

    fn count_input() -> JsObject {
        pilotdeck_protocol::parse_json(r#"{"provider":"p","model":"m","messages":[]}"#)
            .unwrap()
            .as_object()
            .unwrap()
            .clone()
    }

    struct FulfilledCount {
        signal: Cancellation,
        fail: bool,
    }
    impl ProviderCountTransport for FulfilledCount {
        fn post(&self, _: CountHttpRequest) -> CountFuture {
            let result = if self.fail {
                Err(ModuleError::new("fixture transport failure").into())
            } else {
                Ok(object([("input_tokens", JsValue::Number(73.0))]))
            };
            // A settled fetch Promise cannot be rejected by a subsequent
            // controller.abort() in the same JS stack (real TS Node probe).
            self.signal.abort();
            CountFuture::ready(result)
        }
    }

    #[tokio::test(start_paused = true)]
    async fn count_fulfilled_promise_keeps_result_through_synchronous_abort() {
        for fail in [false, true] {
            let signal = Cancellation::new();
            let runtime = configured_count(FulfilledCount {
                signal: signal.clone(),
                fail,
            });
            let result = runtime
                .provider_count(count_input(), Some(signal.clone()))
                .await;
            assert!(signal.is_cancelled());
            if fail {
                assert_eq!(
                    result.unwrap_err().module_error.unwrap().message,
                    "fixture transport failure".into()
                );
            } else {
                assert_eq!(result.unwrap(), Some(73.0));
            }
        }
    }

    struct TimedCount {
        started: Arc<AtomicBool>,
        response_ms: u64,
        fail: bool,
    }
    impl ProviderCountTransport for TimedCount {
        fn post(&self, _: CountHttpRequest) -> CountFuture {
            self.started.store(true, Ordering::SeqCst);
            CountFuture::after(
                Duration::from_millis(self.response_ms),
                if self.fail {
                    Err(ModuleError::new("fixture transport failure").into())
                } else {
                    Ok(object([("input_tokens", JsValue::Number(73.0))]))
                },
            )
        }
    }

    #[tokio::test(start_paused = true)]
    async fn count_overdue_response_orders_synchronous_abort_parent_and_timeout() {
        for (response_ms, parent_ms, synchronous, success) in [
            (50, 60, true, false),
            (50, 60, false, true),
            (50, 50, false, false),
            (50, 40, false, false),
            (110, 120, false, false),
            (100, 120, false, false),
        ] {
            // Real TS keeps an earlier rejected fetch value too: the count
            // catch does not recheck a later parent signal (unlike providers).
            for fail in [true, false] {
                let started = Arc::new(AtomicBool::new(false));
                let runtime = configured_count(TimedCount {
                    started: started.clone(),
                    response_ms,
                    fail,
                });
                let signal = Cancellation::new();
                let mut pending =
                    Box::pin(runtime.provider_count(count_input(), Some(signal.clone())));
                // Let the CPU request-builder finish without spawning the exchange:
                // it must remain unpolled between advance() and abort delivery.
                while !started.load(Ordering::SeqCst) {
                    std::future::poll_fn(|cx| {
                        assert!(pending.as_mut().poll(cx).is_pending());
                        Poll::Ready(())
                    })
                    .await;
                    tokio::task::yield_now().await;
                }
                let at = tokio::time::Instant::now();
                tokio::time::advance(Duration::from_millis(130)).await;
                if synchronous {
                    signal.abort();
                } else {
                    signal.cancel_at(at + Duration::from_millis(parent_ms));
                }
                let result = std::future::poll_fn(|cx| match pending.as_mut().poll(cx) {
                    Poll::Ready(result) => Poll::Ready(result),
                    Poll::Pending => panic!("must settle without advancing the paused clock"),
                })
                .await;
                if success && fail {
                    assert_eq!(
                        result.unwrap_err().module_error.unwrap().message,
                        "fixture transport failure".into()
                    );
                } else if success {
                    assert_eq!(result.unwrap(), Some(73.0));
                } else {
                    assert_eq!(
                        result.unwrap_err().module_error.unwrap().message,
                        "This operation was aborted".into()
                    );
                }
            }
        }
    }

    struct UnreachableTransport;
    impl super::super::ProviderCountTransport for UnreachableTransport {
        fn post(&self, _: super::super::CountHttpRequest) -> super::super::CountFuture {
            panic!("malformed provider body must fail before transport")
        }
    }

    #[tokio::test]
    async fn real_ts_responses_tool_result_content_errors() {
        let fixture = crate::test_baseline::oracle(include_str!(
            "../../../tests/fixtures/token-accounting/oracle.json"
        ));
        let JsValue::Array(cases) = fixture.get("providerErrors").unwrap() else {
            panic!("providerErrors")
        };
        assert_eq!(cases.len(), 3);
        for (case, suffix) in cases.iter().zip(["null", "missing", "number"]) {
            assert_eq!(
                case.get("name")
                    .unwrap()
                    .as_string()
                    .unwrap()
                    .to_utf8()
                    .unwrap(),
                format!("openai-responses-tool-result-{suffix}")
            );
            assert_eq!(case.get("calls").and_then(JsValue::as_number), Some(0.0));
            let runtime = TokenAccountingRuntime::new(
                case.get("config").unwrap().as_object().unwrap().clone(),
            )
            .with_transport(Arc::new(UnreachableTransport));
            // Exercise provider_count directly: the public evaluator's local
            // estimator would throw earlier and hide a flatten_result defect.
            let error = runtime
                .provider_count(
                    case.get("request").unwrap().as_object().unwrap().clone(),
                    None,
                )
                .await
                .unwrap_err();
            assert_eq!(
                JsValue::String(error.module_error.unwrap().message),
                case.get("result").unwrap().get("message").unwrap().clone(),
                "tool result content {suffix}"
            );
        }
    }
}
