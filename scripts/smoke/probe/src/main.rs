//! Real-provider probe for the gateway model runtime.
//!
//! This is the only executable on origin/main that can reach a provider
//! without the missing #589 host/WS assembly: it calls the gateway's public
//! `complete_model` / `stream_model` HTTP runtime directly, with the same
//! Anthropic protocol adapter and TLS transport the product uses.
//!
//! Usage: real-provider-probe <text|stream-cancel|tool> <base-url> <model>
//!                             <max-output-tokens> <timeout-seconds>
//! The key is read from PDRS_PROVIDER_KEY and only placed in the in-memory
//! provider config. Nothing here prints it; the request body cannot contain it
//! (it is sent as the `x-api-key` header).
//!
//! The two numeric arguments are the smoke budget, owned by
//! `scripts/smoke/real_provider.py` and echoed back in `PROBE_RESULT` so the
//! report states what was actually sent rather than a second hard-coded copy.

use pilotdeck_gateway::runtime::model::http::{
    HyperModelTransport, ModelStreamError, StreamOptions, StreamOutput, complete_model,
    stream_model,
};
use pilotdeck_gateway::runtime::model::router::AbortFlag;
use pilotdeck_protocol::{JsObject, JsValue, parse_json, stringify_json};
use std::time::{Duration, Instant};

static UNDEFINED: JsValue = JsValue::Undefined;

/// Per-run limits, parsed from argv. The Python harness is the owner of the
/// values; the probe only enforces/echoes them.
struct Limits {
    max_output_tokens: f64,
    timeout_seconds: u64,
    timeout: Duration,
}

fn field<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    value.get(key).unwrap_or(&UNDEFINED)
}

fn text(value: &JsValue) -> String {
    value
        .as_string()
        .and_then(|s| s.to_utf8().ok())
        .unwrap_or_default()
}

fn obj(entries: Vec<(&str, JsValue)>) -> JsValue {
    let mut out = JsObject::new();
    for (key, value) in entries {
        out.insert(key.into(), value);
    }
    JsValue::Object(out)
}

fn string(value: &str) -> JsValue {
    JsValue::String(value.into())
}

fn number(value: f64) -> JsValue {
    JsValue::Number(value)
}

fn json_text(value: &JsValue) -> String {
    stringify_json(value).ok().flatten().unwrap_or_default()
}

fn truncate(value: &str, max: usize) -> String {
    let mut out: String = value.chars().take(max).collect();
    if value.chars().count() > max {
        out.push_str("...");
    }
    out
}

/// Config mirrors `model.providers.<id>` after config/snapshot.rs:644-672.
///
/// `thinking.state = "disabled"` is deliberate: for the Anthropic protocol the
/// adapter turns it into `"thinking": {"type": "disabled"}`. The smoke prompt
/// asks for a two-word answer inside a 128-token budget; leaving provider-side
/// reasoning on is the known `20261018-deepseek-thinking` trap where thinking
/// consumes the whole budget and the text body arrives empty.
fn provider_config(base_url: &str, model: &str, key: &str) -> JsValue {
    let model_entry = obj(vec![
        ("id", string(model)),
        ("thinking", obj(vec![("state", string("disabled"))])),
        (
            "capabilities",
            obj(vec![
                ("supportsStreaming", JsValue::Bool(true)),
                ("supportsToolUse", JsValue::Bool(true)),
                ("supportsSystemPrompt", JsValue::Bool(true)),
                ("maxOutputTokens", number(256.0)),
            ]),
        ),
        (
            "multimodal",
            obj(vec![("input", JsValue::Array(vec![string("text")]))]),
        ),
    ]);
    let provider = obj(vec![
        ("id", string("llmcenter")),
        ("protocol", string("anthropic")),
        ("url", string(base_url)),
        ("apiKey", string(key)),
        (
            "retry",
            // The product reads requestMaxRetries/streamMaxRetries
            // (config/snapshot.rs:723-731; http.rs retry_number). `maxRetries`
            // is not a key the runtime understands.
            parse_json(r#"{"requestMaxRetries":0,"streamMaxRetries":0}"#)
                .expect("static retry json"),
        ),
        ("models", obj(vec![(model, model_entry)])),
    ]);
    obj(vec![("providers", obj(vec![("llmcenter", provider)]))])
}

fn request(model: &str, prompt: &str, tools: bool, max_output_tokens: f64) -> JsValue {
    let message = obj(vec![
        ("role", string("user")),
        (
            "content",
            JsValue::Array(vec![obj(vec![
                ("type", string("text")),
                ("text", string(prompt)),
            ])]),
        ),
    ]);
    let mut entries = vec![
        ("provider", string("llmcenter")),
        ("model", string(model)),
        ("messages", JsValue::Array(vec![message])),
        ("maxOutputTokens", number(max_output_tokens)),
    ];
    if tools {
        entries.push((
            "tools",
            JsValue::Array(vec![obj(vec![
                ("name", string("record_smoke_value")),
                (
                    "description",
                    string("Record one synthetic smoke-test value. Call it when asked."),
                ),
                (
                    "inputSchema",
                    parse_json(
                        r#"{"type":"object","properties":{"value":{"type":"string","description":"the value to record"}},"required":["value"],"additionalProperties":false}"#,
                    )
                    .expect("static tool schema"),
                ),
            ])]),
        ));
        entries.push((
            "toolChoice",
            obj(vec![
                ("type", string("tool")),
                ("name", string("record_smoke_value")),
            ]),
        ));
    }
    obj(entries)
}

fn error_value(error: &ModelStreamError) -> JsValue {
    let mut entries = vec![
        ("name", string(&text(&JsValue::String(error.name())))),
        ("message", string(&text(&JsValue::String(error.message())))),
    ];
    if let ModelStreamError::Request { code, .. } = error {
        entries.push(("code", string(&text(&JsValue::String(code.clone())))));
    }
    if let ModelStreamError::Provider(canonical) = error {
        entries.push(("code", canonical.get("code").unwrap_or(&UNDEFINED).clone()));
        entries.push((
            "retryable",
            canonical.get("retryable").unwrap_or(&UNDEFINED).clone(),
        ));
        // `errors.rs` always stores `status` in the canonical object (Number or
        // Undefined). Keep a numeric status so the Python consumer can classify
        // a non-canonical 5xx code (for example `api_error`) as an environment
        // failure instead of `product-or-protocol`.
        if let Some(status) = canonical.get("status") {
            if status.as_number().is_some() {
                entries.push(("status", status.clone()));
            }
        }
    }
    obj(entries)
}

/// Only selected fields: never the raw provider payload.
fn summarize_event(event: &JsValue) -> JsValue {
    let kind = text(field(event, "type"));
    match kind.as_str() {
        "text_delta" => {
            let delta = text(field(event, "text"));
            obj(vec![
                ("type", string("text_delta")),
                ("text", string(&truncate(&delta, 120))),
                ("chars", number(delta.chars().count() as f64)),
            ])
        }
        "tool_call_start" => obj(vec![
            ("type", string("tool_call_start")),
            ("id", field(event, "id").clone()),
            ("name", field(event, "name").clone()),
        ]),
        "tool_call_delta" => {
            let delta = text(field(event, "delta"));
            obj(vec![
                ("type", string("tool_call_delta")),
                ("id", field(event, "id").clone()),
                ("chars", number(delta.chars().count() as f64)),
            ])
        }
        "tool_call_end" => obj(vec![
            ("type", string("tool_call_end")),
            ("toolCall", field(event, "toolCall").clone()),
            ("wasRepaired", field(event, "wasRepaired").clone()),
        ]),
        "message_end" => obj(vec![
            ("type", string("message_end")),
            ("finishReason", field(event, "finishReason").clone()),
        ]),
        "usage" => obj(vec![
            ("type", string("usage")),
            ("usage", field(event, "usage").clone()),
        ]),
        "error" => obj(vec![
            ("type", string("error")),
            ("error", field(event, "error").clone()),
        ]),
        other => obj(vec![("type", string(other))]),
    }
}

fn content_text(response: &JsValue) -> String {
    let mut out = String::new();
    if let JsValue::Array(items) = field(response, "content") {
        for item in items {
            if text(field(item, "type")) == "text" {
                out.push_str(&text(field(item, "text")));
            }
        }
    }
    out
}

fn elapsed_ms(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1000.0
}

fn push_defined(entries: &mut Vec<(&'static str, JsValue)>, key: &'static str, value: JsValue) {
    if !value.is_undefined() {
        entries.push((key, value));
    }
}

async fn run_text(config: &JsValue, model: &str, limits: &Limits) -> JsValue {
    let started = Instant::now();
    let prompt = "This is a synthetic connectivity smoke test with no personal data. \
                  Reply with exactly: SMOKE-OK";
    let request = request(model, prompt, false, limits.max_output_tokens);
    let transport = HyperModelTransport::new();
    let outcome = tokio::time::timeout(
        limits.timeout,
        complete_model(&transport, &request, config, &StreamOptions::default()),
    )
    .await;
    match outcome {
        Err(_) => obj(vec![
            ("scenario", string("text")),
            ("status", string("FAIL")),
            ("kind", string("timeout")),
            (
                "error",
                string(&format!(
                    "complete_model exceeded {}s",
                    limits.timeout_seconds
                )),
            ),
            ("elapsedMs", number(elapsed_ms(started))),
            ("requests", number(1.0)),
        ]),
        Ok(Err(error)) => obj(vec![
            ("scenario", string("text")),
            ("status", string("FAIL")),
            ("kind", string("provider_error")),
            ("error", error_value(&error)),
            ("elapsedMs", number(elapsed_ms(started))),
            ("requests", number(1.0)),
        ]),
        Ok(Ok(response)) => {
            let body = content_text(&response);
            let finish = field(&response, "finishReason").clone();
            let status = if body.trim().is_empty() {
                "FAIL"
            } else {
                "PASS"
            };
            let mut entries = vec![
                ("scenario", string("text")),
                ("status", string(status)),
                ("kind", string("complete")),
                ("provider", string("anthropic")),
                ("model", string(model)),
                ("text", string(&truncate(&body, 200))),
                ("finishReason", finish.clone()),
                ("elapsedMs", number(elapsed_ms(started))),
                ("requests", number(1.0)),
            ];
            push_defined(&mut entries, "usage", field(&response, "usage").clone());
            if status == "FAIL" {
                // Keep the reason machine-readable: classify_failure() uses
                // finishReason=length + empty text as a budget/proxy signal.
                entries.push((
                    "error",
                    obj(vec![
                        ("name", string("EmptyResponse")),
                        (
                            "message",
                            string(&format!(
                                "empty text response (finishReason={})",
                                json_text(&finish)
                            )),
                        ),
                    ]),
                ));
            }
            obj(entries)
        }
    }
}

#[derive(Debug, Default, Clone, Copy)]
struct AbortAudit {
    /// A `message_end` arrived after the abort request.
    message_end: bool,
    /// A consumer-visible content event arrived after the abort request.
    content: bool,
    /// A terminal `type=error` event whose body names abort/cancel.
    abort_error: bool,
}

impl AbortAudit {
    fn observe(&mut self, event: &JsValue) {
        match text(field(event, "type")).as_str() {
            "message_end" => self.message_end = true,
            "text_delta" | "thinking_delta" | "tool_call_start" | "tool_call_delta"
            | "tool_call_end" => self.content = true,
            "error" => {
                let body = json_text(field(event, "error")).to_lowercase();
                if body.contains("abort") || body.contains("cancel") {
                    self.abort_error = true;
                }
            }
            _ => {}
        }
    }
}

/// Counters derived from every event seen by the stream callback.
///
/// The `events` vector is capped at 200 entries for the report, but request
/// counts, the last `usage`, and the post-abort audit are updated before the
/// cap is applied. Otherwise a stream that sends its `usage` event after the
/// 200th event would report "0 output tokens", and a cancelled stream whose
/// post-abort content arrives after the cap would look clean. Both counters
/// are therefore independent of the report truncation.
#[derive(Debug, Default)]
struct StreamTally {
    requests: f64,
    usage: Option<JsValue>,
    aborted: bool,
    first_delta_chars: usize,
    first_delta_kind: String,
    post_abort: AbortAudit,
}

impl StreamTally {
    /// Feed one event; returns true when this event triggered the abort.
    fn observe(&mut self, event: &JsValue, abort_on_delta: bool) -> bool {
        let kind = text(field(event, "type"));
        match kind.as_str() {
            "request_started" => self.requests += 1.0,
            "usage" => self.usage = Some(field(event, "usage").clone()),
            _ => {}
        }
        if !self.aborted && abort_on_delta && (kind == "text_delta" || kind == "thinking_delta") {
            self.first_delta_chars = text(field(event, "text")).chars().count();
            self.first_delta_kind = kind;
            self.aborted = true;
            return true;
        }
        if self.aborted {
            self.post_abort.observe(event);
        }
        false
    }

    fn usage(&self) -> JsValue {
        self.usage.clone().unwrap_or(JsValue::Undefined)
    }
}

/// Audit the events that arrived *after* the delta that triggered the abort.
/// Test helper: production derives the audit in the stream callback so it is
/// not truncated by the 200-event report cap.
#[cfg(test)]
fn audit_post_abort(events: &[JsValue]) -> AbortAudit {
    let mut audit = AbortAudit::default();
    for event in events {
        audit.observe(event);
    }
    audit
}

/// PASS iff the stream really stopped early. An `Ok(())` return means the
/// consumer loop finished normally; that is only acceptable when a terminal
/// abort/cancel error event was observed and no content or `message_end`
/// followed the abort. Without those assertions a runtime that turns
/// `AbortFlag` into a no-op would still PASS this negative control.
fn cancel_ok(aborted: bool, returned_ok: bool, audit: &AbortAudit) -> bool {
    if !aborted || audit.message_end || audit.content {
        return false;
    }
    if returned_ok { audit.abort_error } else { true }
}

async fn run_stream_cancel(config: &JsValue, model: &str, limits: &Limits) -> JsValue {
    let started = Instant::now();
    let prompt = "Write a 300 word synthetic description of a calm grey cube. \
                  Do not mention people, places, or real products.";
    let request = request(model, prompt, false, limits.max_output_tokens);
    let transport = HyperModelTransport::new();
    let abort = AbortFlag::new();
    let options = StreamOptions {
        abort: Some(abort.clone()),
        ..Default::default()
    };
    let mut events = Vec::new();
    let mut tally = StreamTally::default();
    // Index of the event whose callback requested the abort. Everything after
    // it is the post-abort window the report displays; the verdict uses
    // `tally.post_abort`, which is not capped with `events`.
    let mut abort_index: Option<usize> = None;
    let outcome = tokio::time::timeout(
        limits.timeout,
        stream_model(&transport, &request, config, &options, &mut |output| {
            let StreamOutput::Event(event) = output else {
                return;
            };
            let event_index = events.len();
            if events.len() < 200 {
                events.push(summarize_event(&event));
            }
            // Reasoning models may emit thinking_delta before any text_delta;
            // both are consumer-visible stream content, so either one is a
            // valid mid-stream cancellation point. `observe` updates request
            // counts, usage, and the post-abort audit before the 200-event
            // cap can hide them.
            if tally.observe(&event, true) {
                abort_index = Some(event_index);
                abort.abort();
            }
        }),
    )
    .await;
    let aborted = tally.aborted;
    let post: &[JsValue] = match abort_index {
        Some(index) if index + 1 < events.len() => &events[index + 1..],
        _ => &[],
    };
    let audit = tally.post_abort;
    // Snapshot the derived values before `events` moves into the result.
    let requests = tally.requests;
    let usage = tally.usage();
    let mut result = vec![
        ("scenario", string("stream-cancel")),
        ("kind", string("stream")),
        ("provider", string("anthropic")),
        ("model", string(model)),
        ("firstDeltaChars", number(tally.first_delta_chars as f64)),
        ("firstDeltaKind", string(&tally.first_delta_kind)),
        ("abortRequested", JsValue::Bool(aborted)),
        (
            "abortEventIndex",
            number(abort_index.map_or(-1.0, |i| i as f64)),
        ),
        ("postAbortContent", JsValue::Bool(audit.content)),
        ("postAbortMessageEnd", JsValue::Bool(audit.message_end)),
        ("postAbortTerminalError", JsValue::Bool(audit.abort_error)),
        ("postAbortEvents", JsValue::Array(post.to_vec())),
        ("events", JsValue::Array(events)),
        ("elapsedMs", number(elapsed_ms(started))),
        ("requests", number(requests)),
    ];
    push_defined(&mut result, "usage", usage);
    match outcome {
        Err(_) => {
            result.push(("status", string("FAIL")));
            result.push((
                "error",
                string(&format!(
                    "stream_model exceeded {}s after abort",
                    limits.timeout_seconds
                )),
            ));
        }
        Ok(Err(error)) => {
            let name = text(&JsValue::String(error.name()));
            let message = text(&JsValue::String(error.message()));
            let clean = name == "AbortError"
                || message.to_lowercase().contains("abort")
                || message.to_lowercase().contains("cancel");
            let pass = cancel_ok(aborted, false, &audit) && clean;
            result.push(("status", string(if pass { "PASS" } else { "FAIL" })));
            // Keep the terminal error in the raw record even on PASS: it is
            // evidence that the stream ended with AbortError rather than a
            // normal `Ok(())` after ignoring the flag.
            result.push(("abortError", error_value(&error)));
            if !pass {
                if clean && !aborted {
                    result.push((
                        "error",
                        string("stream ended with an abort error but no delta aborted it"),
                    ));
                } else if clean && audit.message_end {
                    result.push(("error", string("abort error arrived after message_end")));
                } else {
                    result.push(("error", error_value(&error)));
                }
            }
        }
        Ok(Ok(())) => {
            let pass = cancel_ok(aborted, true, &audit);
            result.push(("status", string(if pass { "PASS" } else { "FAIL" })));
            if !pass {
                result.push((
                    "error",
                    string(&format!(
                        "abort did not terminate the stream (aborted={aborted}, \
                         contentAfterAbort={}, messageEndAfterAbort={}, \
                         terminalAbortError={}, elapsedMs={:.1})",
                        audit.content,
                        audit.message_end,
                        audit.abort_error,
                        elapsed_ms(started)
                    )),
                ));
            }
        }
    }
    obj(result)
}

async fn run_tool(config: &JsValue, model: &str, limits: &Limits) -> JsValue {
    let started = Instant::now();
    let prompt = "Call the record_smoke_value tool exactly once with value \"alpha-42\". \
                  Do not write prose.";
    let request = request(model, prompt, true, limits.max_output_tokens);
    let transport = HyperModelTransport::new();
    let mut events = Vec::new();
    let mut calls = Vec::new();
    let mut tally = StreamTally::default();
    let outcome = tokio::time::timeout(
        limits.timeout,
        stream_model(
            &transport,
            &request,
            config,
            &StreamOptions::default(),
            &mut |output| {
                let StreamOutput::Event(event) = output else {
                    return;
                };
                let kind = text(field(&event, "type"));
                // No abort in this scenario: keep counting requests and the
                // last usage independently of the 200-event report cap.
                tally.observe(&event, false);
                if kind == "tool_call_end" {
                    calls.push(field(&event, "toolCall").clone());
                }
                if events.len() < 200 {
                    events.push(summarize_event(&event));
                }
            },
        ),
    )
    .await;
    let expected = calls.iter().any(|call| {
        text(field(call, "name")) == "record_smoke_value"
            && text(field(field(call, "input"), "value")) == "alpha-42"
    });
    let mut result = vec![
        ("scenario", string("tool")),
        ("kind", string("stream")),
        ("provider", string("anthropic")),
        ("model", string(model)),
        ("toolCalls", JsValue::Array(calls.clone())),
        ("events", JsValue::Array(events.clone())),
        ("elapsedMs", number(elapsed_ms(started))),
        ("requests", number(tally.requests)),
    ];
    push_defined(&mut result, "usage", tally.usage());
    match outcome {
        Err(_) => {
            result.push(("status", string("FAIL")));
            result.push((
                "error",
                string(&format!(
                    "stream_model exceeded {}s",
                    limits.timeout_seconds
                )),
            ));
        }
        Ok(Err(error)) => {
            result.push(("status", string("FAIL")));
            result.push(("error", error_value(&error)));
        }
        Ok(Ok(())) => {
            result.push(("status", string(if expected { "PASS" } else { "FAIL" })));
            if !expected {
                result.push((
                    "error",
                    string("no tool_call_end with name=record_smoke_value value=alpha-42"),
                ));
            }
        }
    }
    obj(result)
}

fn fail(message: &str) -> JsValue {
    obj(vec![("status", string("FAIL")), ("error", string(message))])
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() != 5 {
        eprintln!(
            "usage: real-provider-probe <text|stream-cancel|tool> <base-url> <model> \
             <max-output-tokens> <timeout-seconds>"
        );
        std::process::exit(2);
    }
    let scenario = args[0].as_str();
    let base_url = args[1].as_str();
    let model = args[2].as_str();
    let max_output_tokens = match args[3].parse::<f64>() {
        Ok(value) if value.is_finite() && value > 0.0 => value,
        _ => {
            eprintln!("max-output-tokens must be a positive number");
            std::process::exit(2);
        }
    };
    let timeout_seconds = match args[4].parse::<u64>() {
        Ok(value) if value > 0 => value,
        _ => {
            eprintln!("timeout-seconds must be a positive integer");
            std::process::exit(2);
        }
    };
    let limits = Limits {
        max_output_tokens,
        timeout_seconds,
        timeout: Duration::from_secs(timeout_seconds),
    };
    let key = match std::env::var("PDRS_PROVIDER_KEY") {
        Ok(key) if !key.trim().is_empty() => key,
        _ => {
            let result = fail("PDRS_PROVIDER_KEY is not set in the child environment");
            println!("PROBE_RESULT {}", json_text(&result));
            std::process::exit(0);
        }
    };
    let config = provider_config(base_url, model, &key);
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            let result = fail(&format!("tokio runtime: {error}"));
            println!("PROBE_RESULT {}", json_text(&result));
            return;
        }
    };
    let mut result = runtime.block_on(async {
        match scenario {
            "text" => run_text(&config, model, &limits).await,
            "stream-cancel" => run_stream_cancel(&config, model, &limits).await,
            "tool" => run_tool(&config, model, &limits).await,
            other => fail(&format!("unknown scenario {other}")),
        }
    });
    // Always label the scenario and echo the harness-owned limits, even for
    // argument/config failures.
    if let JsValue::Object(record) = &mut result {
        record.insert("scenario".into(), string(scenario));
        record.insert("maxOutputTokens".into(), number(max_output_tokens));
        record.insert("timeoutSeconds".into(), number(timeout_seconds as f64));
    }
    println!("PROBE_RESULT {}", json_text(&result));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(raw: &str) -> JsValue {
        parse_json(raw).expect("test event json")
    }

    fn audit(raw_events: &[&str]) -> AbortAudit {
        let events: Vec<JsValue> = raw_events.iter().map(|raw| event(raw)).collect();
        audit_post_abort(&events)
    }

    #[test]
    fn abort_error_without_message_end_passes() {
        let audit =
            audit(&[r#"{"type":"error","error":{"name":"AbortError","message":"aborted"}}"#]);
        assert!(audit.abort_error);
        assert!(!audit.message_end);
        assert!(!audit.content);
        assert!(cancel_ok(true, false, &audit));
    }

    #[test]
    fn ok_return_needs_a_terminal_abort_error() {
        let empty = audit(&[]);
        assert!(!cancel_ok(true, true, &empty));
        let terminal =
            audit(&[r#"{"type":"error","error":{"name":"AbortError","message":"cancelled"}}"#]);
        assert!(cancel_ok(true, true, &terminal));
    }

    #[test]
    fn message_end_after_abort_always_fails() {
        let audit = audit(&[
            r#"{"type":"error","error":{"name":"AbortError","message":"aborted"}}"#,
            r#"{"type":"message_end","finishReason":"stop"}"#,
        ]);
        assert!(audit.message_end);
        assert!(!cancel_ok(true, false, &audit));
        assert!(!cancel_ok(true, true, &audit));
    }

    #[test]
    fn content_after_abort_rejects_an_ok_return() {
        let audit = audit(&[
            r#"{"type":"error","error":{"name":"AbortError","message":"aborted"}}"#,
            r#"{"type":"text_delta","text":"still going"}"#,
        ]);
        assert!(audit.content);
        assert!(!cancel_ok(true, true, &audit));
    }

    #[test]
    fn content_after_abort_rejects_an_abort_error() {
        let audit = audit(&[r#"{"type":"text_delta","text":"still going"}"#]);
        assert!(audit.content);
        assert!(!cancel_ok(true, false, &audit));
    }

    #[test]
    fn no_abort_request_never_passes() {
        assert!(!cancel_ok(false, false, &AbortAudit::default()));
        assert!(!cancel_ok(false, true, &AbortAudit::default()));
    }

    #[test]
    fn provider_config_disables_thinking_and_uses_product_retry_keys() {
        let config = provider_config("https://example.invalid", "m", "k");
        let provider = field(field(&config, "providers"), "llmcenter");
        let retry = field(provider, "retry");
        assert!(field(retry, "requestMaxRetries").as_number() == Some(0.0));
        assert!(field(retry, "streamMaxRetries").as_number() == Some(0.0));
        assert!(field(retry, "maxRetries").is_undefined());
        let model = field(field(provider, "models"), "m");
        assert_eq!(text(field(field(model, "thinking"), "state")), "disabled");
    }

    #[test]
    fn request_uses_the_harness_budget() {
        let request = request("m", "p", false, 128.0);
        assert!(field(&request, "maxOutputTokens").as_number() == Some(128.0));
    }

    #[test]
    fn request_count_and_usage_survive_the_event_cap() {
        // The report stores at most 200 events, but these counters must keep
        // updating on every event: a provider that sends usage after the cap
        // would otherwise look like 0 output tokens.
        let mut tally = StreamTally::default();
        for _ in 0..250 {
            tally.observe(&event(r#"{"type":"request_started"}"#), false);
        }
        for _ in 0..250 {
            tally.observe(&event(r#"{"type":"text_delta","text":"x"}"#), false);
        }
        tally.observe(
            &event(r#"{"type":"usage","usage":{"outputTokens":7,"totalTokens":9}}"#),
            false,
        );
        assert_eq!(tally.requests, 250.0);
        let usage = tally.usage();
        assert_eq!(field(&usage, "outputTokens").as_number(), Some(7.0));
        assert_eq!(field(&usage, "totalTokens").as_number(), Some(9.0));
    }

    #[test]
    fn post_abort_audit_survives_the_event_cap() {
        // The cancelled stream's audit is derived in the callback, not from
        // the capped `events` vector, so late content cannot be truncated
        // away.
        let mut tally = StreamTally::default();
        let trigger = event(r#"{"type":"text_delta","text":"first"}"#);
        assert!(tally.observe(&trigger, true));
        for _ in 0..250 {
            tally.observe(&event(r#"{"type":"text_delta","text":"still"}"#), true);
        }
        assert!(tally.aborted);
        assert!(tally.post_abort.content);
        assert!(!cancel_ok(tally.aborted, false, &tally.post_abort));
    }

    #[test]
    fn provider_error_keeps_the_canonical_status() {
        // `errors.rs` builds `{code,status,message,retryable,...}`. The Python
        // side treats status >= 500 as environment, but it can only do that if
        // the probe copies the field. A non-canonical upstream `type`
        // (`api_error`) is not in the needle list; `status` is the fallback.
        let mut canonical = JsObject::new();
        canonical.insert("code".into(), string("api_error"));
        canonical.insert("message".into(), string("upstream 503"));
        canonical.insert("status".into(), number(503.0));
        canonical.insert("retryable".into(), JsValue::Bool(true));
        let value = error_value(&ModelStreamError::Provider(canonical));
        assert_eq!(field(&value, "status").as_number(), Some(503.0));
        assert_eq!(text(field(&value, "code")), "api_error");
        assert!(field(&value, "retryable").as_bool() == Some(true));
        // The probe prints this object through stringify_json; the consumer
        // can only see the field if it survives serialization.
        let rendered = json_text(&value);
        assert!(rendered.contains("\"status\":503"), "{rendered}");
    }

    #[test]
    fn provider_error_without_status_does_not_invent_one() {
        let mut canonical = JsObject::new();
        canonical.insert("code".into(), string("provider_error"));
        canonical.insert("message".into(), string("no status"));
        let value = error_value(&ModelStreamError::Provider(canonical));
        assert!(field(&value, "status").is_undefined());
    }

    #[test]
    fn tool_mode_does_not_treat_the_first_delta_as_an_abort() {
        let mut tally = StreamTally::default();
        assert!(!tally.observe(&event(r#"{"type":"text_delta","text":"x"}"#), false));
        assert!(!tally.aborted);
        assert!(!tally.post_abort.content);
    }
}
