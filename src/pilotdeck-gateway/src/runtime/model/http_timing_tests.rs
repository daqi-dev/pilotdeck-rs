//! One row per real TS observation; see fixtures/provider-timing/probe.mts.
use super::tests::{poll_pending, stream_config};
use super::*;
use crate::host::modules::budget::{EvaluationOptions, RequestBudgetEvaluator};
use crate::runtime::model::token_accounting::TokenAccountingRuntime;
use crate::runtime::model::{CountFuture, CountHttpRequest, ProviderCountTransport};
use serde_json::{Value, json};
use std::{
    future::Future,
    pin::Pin,
    sync::atomic::{AtomicBool, Ordering},
    task::Poll,
};

fn text(value: &JsValue) -> Option<String> {
    value
        .as_string()
        .map(|s| String::from_utf16_lossy(s.units()))
}

struct MatrixBody {
    timer: Option<Pin<Box<tokio::time::Sleep>>>,
    event: String,
    data: Option<Bytes>,
}
impl hyper::body::Body for MatrixBody {
    type Data = Bytes;
    type Error = BodyFailure;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<Result<hyper::body::Frame<Bytes>, BodyFailure>>> {
        if self.event == "idle" {
            return Poll::Pending;
        }
        if let Some(timer) = &mut self.timer {
            std::task::ready!(timer.as_mut().poll(cx));
        }
        if self.event == "rejected" {
            return Poll::Ready(Some(Err(BodyFailure)));
        }
        Poll::Ready(
            self.data
                .take()
                .map(|data| Ok(hyper::body::Frame::data(data))),
        )
    }
}

fn sse(protocol: &str) -> &'static str {
    match protocol {
        "anthropic" => {
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"FIRST\"}}\n\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"}}\n\n"
        }
        "openai" => {
            "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"FIRST\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n"
        }
        "openai-responses" => {
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"r\",\"status\":\"completed\",\"output\":[]}}\n\n"
        }
        "google" => {
            "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"FIRST\"}]},\"finishReason\":\"STOP\"}]}\n\n"
        }
        _ => unreachable!(),
    }
}
struct MatrixTransport {
    protocol: String,
    phase: String,
    event: String,
    synchronous: bool,
    signal: AbortFlag,
    started: Arc<AtomicBool>,
}
impl ModelHttpTransport for MatrixTransport {
    fn send(&self, _: Request<Full<Bytes>>) -> TransportFuture {
        self.started.store(true, Ordering::SeqCst);
        if self.phase == "headers" && self.event == "idle" {
            return TransportFuture::io(std::future::pending());
        }
        let result = if self.phase == "headers" && self.event == "rejected" {
            Err(TransportFailure {
                message: "fixture transport failure".into(),
                cause_code: None,
            })
        } else {
            let error_body = self.phase == "response_text";
            let body = MatrixBody {
                timer: (self.phase != "headers")
                    .then(|| Box::pin(tokio::time::sleep(Duration::from_millis(20)))),
                event: if self.phase == "headers" {
                    "fulfilled".into()
                } else {
                    self.event.clone()
                },
                data: Some(Bytes::from(if error_body {
                    r#"{"error":{"message":"fixture bad request","code":400,"status":"INVALID_ARGUMENT"}}"#
                } else {
                    sse(&self.protocol)
                })),
            };
            Ok(Response::builder()
                .status(if error_body { 400 } else { 200 })
                .header(
                    "content-type",
                    if error_body {
                        "application/json"
                    } else {
                        "text/event-stream"
                    },
                )
                .body(body.boxed())
                .unwrap())
        };
        if self.synchronous {
            self.signal.abort();
            TransportFuture::ready(result)
        } else if self.phase == "headers" {
            TransportFuture::after(Duration::from_millis(20), result)
        } else {
            TransportFuture::ready(result)
        }
    }
}
impl ProviderCountTransport for MatrixTransport {
    fn post(&self, _: CountHttpRequest) -> CountFuture {
        self.started.store(true, Ordering::SeqCst);
        match self.event.as_str() {
            "fulfilled" => CountFuture::after(
                Duration::from_millis(20),
                Ok(parse_json(r#"{"input_tokens":73}"#).unwrap()),
            ),
            "rejected" => CountFuture::after(
                Duration::from_millis(20),
                Err(ModuleError::new("fixture transport failure").into()),
            ),
            "idle" => CountFuture::io(std::future::pending()),
            _ => unreachable!(),
        }
    }
}
async fn poll_once<F: Future>(mut future: Pin<&mut F>) -> Option<F::Output> {
    std::future::poll_fn(|cx| {
        Poll::Ready(match future.as_mut().poll(cx) {
            Poll::Ready(v) => Some(v),
            Poll::Pending => None,
        })
    })
    .await
}

/// Drive callbacks in actual Node order. A timer's continuation is drained
/// before a later callback, including equal deadlines registered later. Parent
/// first rows deliver the original event time with both events overdue; these
/// also exercise dispatch's late-poll ordering rather than only the executor.
async fn finish<F: Future>(
    mut future: Pin<&mut F>,
    signal: &AbortFlag,
    event: &str,
    order: &str,
) -> F::Output {
    if order == "synchronous" {
        return poll_once(future).await.expect("sync continuation");
    }
    let operation_ms = if event == "idle" { 40 } else { 20 };
    let parent_ms = match order {
        "continuation-first" => operation_ms + 5,
        "parent-first" => operation_ms - 5,
        _ => operation_ms,
    };
    let start = tokio::time::Instant::now();
    if order == "continuation-first" || order == "tie-parent-last" {
        tokio::time::advance(Duration::from_millis(operation_ms)).await;
        if let Some(value) = poll_once(future.as_mut()).await {
            return value;
        }
        tokio::time::advance(Duration::from_millis(parent_ms - operation_ms)).await;
        signal.cancel_at(start + Duration::from_millis(parent_ms));
    } else {
        tokio::time::advance(Duration::from_millis(55)).await;
        signal.cancel_at(start + Duration::from_millis(parent_ms));
    }
    // The body reader may have to finish in the absence of a TS parent
    // listener. Its own body timeout remains independent of cancellation.
    if let Some(value) = poll_once(future.as_mut()).await {
        return value;
    }
    tokio::time::advance(Duration::from_millis(40)).await;
    poll_once(future)
        .await
        .expect("matrix must settle without unbounded clock advance")
}

#[tokio::test(start_paused = true)]
async fn provider_ts_timing_matrix() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/provider-timing/oracle.json"
    ))
    .unwrap();
    let rows = fixture["cases"].as_array().unwrap();
    assert_eq!(
        rows.len(),
        196,
        "4 protocols × 4 phases × 3 events × 4 orders + 4 synchronous rejections"
    );
    let mut mismatches = Vec::new();
    for row in rows {
        let protocol = row["protocol"].as_str().unwrap();
        let phase = row["phase"].as_str().unwrap();
        let event = row["event"].as_str().unwrap();
        let order = row["order"].as_str().unwrap();
        let (request, mut config) = stream_config();
        let mut provider = field(field(&config, "providers"), "p").clone();
        put(&mut provider, "protocol", protocol.into());
        put(
            &mut provider,
            "url",
            if protocol == "openai-responses" {
                "https://api.openai.com/v1"
            } else {
                "http://example.test"
            }
            .into(),
        );
        put(&mut provider, "retry", parse_json(r#"{"streamMaxRetries":1,"maxRetries":1,"baseDelayMs":100,"maxDelayMs":100,"jitter":0}"#).unwrap());
        let mut providers = JsObject::new();
        providers.insert("p".into(), provider);
        put(&mut config, "providers", JsValue::Object(providers));
        let signal = AbortFlag::new();
        let started = Arc::new(AtomicBool::new(false));
        let transport = MatrixTransport {
            protocol: protocol.into(),
            phase: phase.into(),
            event: event.into(),
            synchronous: order == "synchronous",
            signal: signal.clone(),
            started: started.clone(),
        };
        let observed = if phase == "count" {
            let runtime = TokenAccountingRuntime::new(config.as_object().unwrap().clone())
                .with_transport(Arc::new(transport))
                .with_cache_size(0)
                .with_timeout(Duration::from_millis(40));
            let mut pending = Box::pin(runtime.evaluate_request_budget(
                request.as_object().unwrap().clone(),
                EvaluationOptions {
                    max_context_tokens: 1000.0,
                    signal: Some(signal.clone()),
                    reserved_output_tokens: None,
                    use_provider_count: None,
                    calibration: None,
                },
            ));
            let early = loop {
                if let Some(result) = poll_once(pending.as_mut()).await {
                    break Some(result);
                }
                if started.load(Ordering::SeqCst) {
                    break None;
                }
                tokio::task::yield_now().await;
            };
            let result = match early {
                Some(result) => result,
                None => finish(pending.as_mut(), &signal, event, order).await,
            }
            .unwrap();
            let metadata = &result;
            json!({"name":null,"message":text(field(metadata,"estimatorError")),"retries":0,"source":text(field(metadata,"source")),"tokens":field(&result,"tokens").as_number().map(|n| n as u64)})
        } else {
            let options = StreamOptions {
                abort: Some(signal.clone()),
                stream_timeout_ms: Some(40.0),
                body_timeout_ms: if phase == "response_text"
                    && protocol != "google"
                    && event == "idle"
                {
                    40.0
                } else {
                    600_000.0
                },
                ..StreamOptions::default()
            };
            let mut events = Vec::new();
            let mut sink = |e| events.push(e);
            let mut pending = Box::pin(stream_model(
                &transport, &request, &config, &options, &mut sink,
            ));
            if order != "synchronous" {
                poll_pending(pending.as_mut()).await;
            }
            let result = finish(pending.as_mut(), &signal, event, order).await;
            drop(pending);
            let retries = events
                .iter()
                .filter(|e| matches!(e, StreamOutput::RetryProgress(_)))
                .count();
            let error_event = events.iter().find_map(|e| match e {
                StreamOutput::Event(e) if field(e, "type") == &JsValue::from("error") => {
                    Some(field(e, "error"))
                }
                _ => None,
            });
            match result {
                Err(e) => {
                    json!({"name":String::from_utf16_lossy(e.name().units()),"message":e.message_text(),"retries":retries,"delivery":"throw"})
                }
                Ok(()) => match error_event {
                    Some(e) => {
                        json!({"name":text(field(field(e,"raw"),"name")).unwrap_or_else(||"ModelProviderError".into()),"message":text(field(e,"message")),"retries":retries,"delivery":"event"})
                    }
                    None => json!({"name":null,"message":null,"retries":retries,"delivery":"ok"}),
                },
            }
        };
        println!("MATRIX {}", json!({"id":row["id"],"observable":observed}));
        if observed != row["observable"] {
            mismatches.push(format!(
                "{}: TS={} Rust={observed}",
                row["id"], row["observable"]
            ));
        }
    }
    assert!(
        mismatches.is_empty(),
        "{} mismatched cells:\n{}",
        mismatches.len(),
        mismatches.join("\n")
    );
}
