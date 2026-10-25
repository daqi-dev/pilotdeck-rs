//! Real-provider probe for the gateway model runtime.
//!
//! This is the only executable on origin/main that can reach a provider
//! without the missing #589 host/WS assembly: it calls the gateway's public
//! `complete_model` / `stream_model` HTTP runtime directly, with the same
//! Anthropic protocol adapter and TLS transport the product uses.
//!
//! Usage: real-provider-probe <text|stream-cancel|tool> <base-url> <model>
//! The key is read from PDRS_PROVIDER_KEY and only placed in the in-memory
//! provider config. Nothing here prints it; the request body cannot contain it
//! (it is sent as the `x-api-key` header).

use pilotdeck_gateway::runtime::model::http::{
    complete_model, stream_model, HyperModelTransport, ModelStreamError, StreamOptions, StreamOutput,
};
use pilotdeck_gateway::runtime::model::router::AbortFlag;
use pilotdeck_protocol::{parse_json, stringify_json, JsObject, JsValue};
use std::time::{Duration, Instant};

static UNDEFINED: JsValue = JsValue::Undefined;

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
fn provider_config(base_url: &str, model: &str, key: &str) -> JsValue {
    let model_entry = obj(vec![
        ("id", string(model)),
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
            parse_json(r#"{"maxRetries":0,"streamMaxRetries":0}"#).expect("static retry json"),
        ),
        (
            "models",
            obj(vec![(model, model_entry)]),
        ),
    ]);
    obj(vec![(
        "providers",
        obj(vec![("llmcenter", provider)]),
    )])
}

fn request(model: &str, prompt: &str, tools: bool) -> JsValue {
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
        ("maxOutputTokens", number(128.0)),
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
        entries.push((
            "code",
            canonical.get("code").unwrap_or(&UNDEFINED).clone(),
        ));
        entries.push((
            "retryable",
            canonical.get("retryable").unwrap_or(&UNDEFINED).clone(),
        ));
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

async fn run_text(config: &JsValue, model: &str) -> JsValue {
    let started = Instant::now();
    let prompt = "This is a synthetic connectivity smoke test with no personal data. \
                  Reply with exactly: SMOKE-OK";
    let request = request(model, prompt, false);
    let transport = HyperModelTransport::new();
    let outcome = tokio::time::timeout(
        Duration::from_secs(45),
        complete_model(&transport, &request, config, &StreamOptions::default()),
    )
    .await;
    match outcome {
        Err(_) => obj(vec![
            ("scenario", string("text")),
            ("status", string("FAIL")),
            ("kind", string("timeout")),
            ("error", string("complete_model exceeded 45s")),
            ("elapsedMs", number(elapsed_ms(started))),
        ]),
        Ok(Err(error)) => obj(vec![
            ("scenario", string("text")),
            ("status", string("FAIL")),
            ("kind", string("provider_error")),
            ("error", error_value(&error)),
            ("elapsedMs", number(elapsed_ms(started))),
        ]),
        Ok(Ok(response)) => {
            let body = content_text(&response);
            let status = if body.trim().is_empty() { "FAIL" } else { "PASS" };
            obj(vec![
                ("scenario", string("text")),
                ("status", string(status)),
                ("kind", string("complete")),
                ("provider", string("anthropic")),
                ("model", string(model)),
                ("text", string(&truncate(&body, 200))),
                ("finishReason", field(&response, "finishReason").clone()),
                ("usage", field(&response, "usage").clone()),
                ("elapsedMs", number(elapsed_ms(started))),
            ])
        }
    }
}

async fn run_stream_cancel(config: &JsValue, model: &str) -> JsValue {
    let started = Instant::now();
    let prompt = "Write a 300 word synthetic description of a calm grey cube. \
                  Do not mention people, places, or real products.";
    let request = request(model, prompt, false);
    let transport = HyperModelTransport::new();
    let abort = AbortFlag::new();
    let options = StreamOptions {
        abort: Some(abort.clone()),
        ..Default::default()
    };
    let mut events = Vec::new();
    let mut first_delta_chars = 0usize;
    let mut first_delta_kind = String::new();
    let mut aborted_after_delta = false;
    let outcome = tokio::time::timeout(
        Duration::from_secs(45),
        stream_model(
            &transport,
            &request,
            config,
            &options,
            &mut |output| {
                let StreamOutput::Event(event) = output else {
                    return;
                };
                let kind = text(field(&event, "type"));
                // Reasoning models may emit thinking_delta before any
                // text_delta; both are consumer-visible stream content, so
                // either one is a valid mid-stream cancellation point.
                if (kind == "text_delta" || kind == "thinking_delta")
                    && !aborted_after_delta
                {
                    first_delta_chars = text(field(&event, "text")).chars().count();
                    first_delta_kind = kind;
                    abort.abort();
                    aborted_after_delta = true;
                }
                if events.len() < 200 {
                    events.push(summarize_event(&event));
                }
            },
        ),
    )
    .await;
    let mut result = vec![
        ("scenario", string("stream-cancel")),
        ("kind", string("stream")),
        ("provider", string("anthropic")),
        ("model", string(model)),
        ("firstDeltaChars", number(first_delta_chars as f64)),
        ("firstDeltaKind", string(&first_delta_kind)),
        ("abortRequested", JsValue::Bool(aborted_after_delta)),
        ("events", JsValue::Array(events)),
        ("elapsedMs", number(elapsed_ms(started))),
    ];
    match outcome {
        Err(_) => {
            result.push(("status", string("FAIL")));
            result.push(("error", string("stream_model exceeded 45s after abort")));
        }
        Ok(Err(error)) => {
            let name = text(&JsValue::String(error.name()));
            let message = text(&JsValue::String(error.message()));
            let clean = name == "AbortError"
                || message.to_lowercase().contains("abort")
                || message.to_lowercase().contains("cancel");
            result.push(("status", string(if aborted_after_delta && clean {
                "PASS"
            } else {
                "FAIL"
            })));
            result.push(("error", error_value(&error)));
        }
        Ok(Ok(())) => {
            // A cancel can also surface as a terminal error event; both are
            // acceptable only if we actually observed a delta first.
            result.push(("status", string(if aborted_after_delta {
                "PASS"
            } else {
                "FAIL"
            })));
        }
    }
    obj(result)
}

async fn run_tool(config: &JsValue, model: &str) -> JsValue {
    let started = Instant::now();
    let prompt = "Call the record_smoke_value tool exactly once with value \"alpha-42\". \
                  Do not write prose.";
    let request = request(model, prompt, true);
    let transport = HyperModelTransport::new();
    let mut events = Vec::new();
    let mut calls = Vec::new();
    let outcome = tokio::time::timeout(
        Duration::from_secs(45),
        stream_model(
            &transport,
            &request,
            config,
            &StreamOptions::default(),
            &mut |output| {
                let StreamOutput::Event(event) = output else {
                    return;
                };
                if text(field(&event, "type")) == "tool_call_end" {
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
        ("events", JsValue::Array(events)),
        ("elapsedMs", number(elapsed_ms(started))),
    ];
    match outcome {
        Err(_) => {
            result.push(("status", string("FAIL")));
            result.push(("error", string("stream_model exceeded 45s")));
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
    obj(vec![
        ("status", string("FAIL")),
        ("error", string(message)),
    ])
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() != 3 {
        eprintln!("usage: real-provider-probe <text|stream-cancel|tool> <base-url> <model>");
        std::process::exit(2);
    }
    let scenario = args[0].as_str();
    let base_url = args[1].as_str();
    let model = args[2].as_str();
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
            "text" => run_text(&config, model).await,
            "stream-cancel" => run_stream_cancel(&config, model).await,
            "tool" => run_tool(&config, model).await,
            other => fail(&format!("unknown scenario {other}")),
        }
    });
    // Always label the scenario, even for argument/config failures.
    if let JsValue::Object(record) = &mut result {
        record.insert("scenario".into(), string(scenario));
    }
    println!("PROBE_RESULT {}", json_text(&result));
}
