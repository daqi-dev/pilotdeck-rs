use bytes::Bytes;
use hyper::Response;
use pilotdeck_protocol::{JsObject, JsString, JsValue};

use super::{
    AbortFlag, Checkpoint, DumpFailure, ErrorInput, JsError, ModelBody, ModelHttpTransport,
    ModelStreamError, StreamGuard, StreamOptions, StreamOutput, aborted, build_http_request,
    continuation_request, delay, dump_request_to_file, duration, error_event, field,
    idle_timeout_ms, is_retryable_stream_error, js_round, normalize_model_error, object_entries,
    protocol, provider_exchange, provider_id, providers, read_frame, request_started_event,
    response_text, retry_decide, retryable_message, sse, sse_error, sse_limit, throw_if_aborted,
    wait_abort,
};

/// `toProviderError(provider, <fs error>)`: the canonical error the TS Google
/// catch yields for a failed dump `writeFileSync` — no SDK send, no retry, one
/// non-retryable `error` event (`streamModel.ts:395-401` → `:461-495`).
pub(super) fn dump_failure_error(provider: &JsValue, failure: &DumpFailure) -> ModelStreamError {
    // The fs Error's own fields, as Node's `JSON.stringify` projects them.
    let mut raw = JsObject::new();
    if let Some(errno) = failure.errno {
        raw.insert("errno".into(), JsValue::Number(f64::from(errno)));
    }
    if let Some(code) = failure.code {
        raw.insert("code".into(), code.into());
    }
    raw.insert("syscall".into(), failure.syscall.into());
    if let Some(path) = &failure.path {
        raw.insert(
            "path".into(),
            JsValue::String(JsString::from(path.display().to_string().as_str())),
        );
    }
    let mut canonical = normalize_model_error(
        &provider_id(provider),
        &protocol(provider),
        ErrorInput::Error(&JsError {
            message: JsString::from(failure.message().as_str()),
            code: failure.code.map(JsString::from),
            network_code: None,
            raw: JsValue::Object(raw),
        }),
        None,
    );
    // A dump write failure never carries a retryable code or status in TS
    // (`is_retryable(None, "ENOENT")` is false); pin the same for the
    // unmapped-errno and candidate-exhaustion fallbacks, whose
    // message-derived codes could otherwise look retryable.
    canonical.insert("retryable".into(), JsValue::Bool(false));
    ModelStreamError::Provider(canonical)
}

// ---------------------------------------------------------------------------
// Google protocol (`streamGoogleProviderRequest` in streamModel.ts)

/// `sendGoogleCompleteRequest`/`generateContentStream` transport for the SDK:
/// one URL, SDK headers, SDK `extraBody` deep merge. Transport failures are
/// plain `TypeError: fetch failed` (the SDK uses bare fetch, not networkFetch).
pub(super) async fn send_google_request(
    transport: &dyn ModelHttpTransport,
    provider: &JsValue,
    model: &JsValue,
    body: &JsValue,
    options: &StreamOptions,
) -> Result<Response<(ModelBody, bool)>, (ModelStreamError, bool)> {
    let timeout_ms = idle_timeout_ms(provider, options);
    let final_body = providers::google::final_body(body, provider);
    let headers = providers::google::headers(provider).map_err(|error| (error, false))?;
    let encoded = crate::js_value::json_stringify(&final_body)
        .map_err(|_| {
            (
                ModelStreamError::thrown("RangeError", crate::js_value::StackOverflow::MESSAGE),
                false,
            )
        })?
        .unwrap_or_default();
    // PILOTDECK_DUMP_REQUEST: debug aid that writes the provider request body
    // to a file under the OS temp directory before the SDK send
    // (TS `streamModel.ts:395-401`). Google carries its model in the URL, so
    // the log line's model comes from the canonical request, not the body.
    if std::env::var("PILOTDECK_DUMP_REQUEST").as_deref() == Ok("1") {
        // A failed write surfaces like TS: the throw lands in the Google
        // attempt catch → `toProviderError` → one non-retryable error event,
        // and the SDK send never happens (probe: 0 server hits with a
        // dead TMPDIR, `streamModel.ts:395-401` → `:461-495`).
        if let Err(failure) = dump_request_to_file(&final_body, model) {
            return Err((dump_failure_error(provider, &failure), false));
        }
    }
    let endpoint = providers::google::endpoint(provider, model).map_err(|error| (error, false))?;
    let (url, header_map) = build_http_request(&endpoint, &headers)
        .map_err(|error| (ModelStreamError::thrown("TypeError", error.message), false))?;
    let deadline = tokio::time::Instant::now() + duration(timeout_ms);
    crate::fetch_redirect::follow(
        url,
        hyper::Method::POST,
        header_map,
        Bytes::from(encoded),
        || (ModelStreamError::thrown("TypeError", "fetch failed"), false),
        |request| async move {
            provider_exchange(
                transport,
                request,
                options,
                Some(deadline),
                |failure| ModelStreamError::thrown("TypeError", failure.message),
                || {
                    ModelStreamError::thrown(
                        "StreamIdleTimeoutError",
                        format!(
                            "Stream idle timeout: no data received for {}ms",
                            crate::js_value::number_string(timeout_ms)
                        ),
                    )
                },
            )
            .await
        },
    )
    .await
}

/// `throwErrorIfNotOK` + `toProviderError` for the SDK. A JSON content type is
/// parsed like `Response.json()`; any other content type is synthesized into
/// `{error:{message,code,status}}` before `JSON.stringify`.
pub(super) async fn google_response_error(
    response: Response<(ModelBody, bool)>,
    options: &StreamOptions,
    provider: &JsValue,
) -> Result<ModelStreamError, ModelStreamError> {
    let status = response.status().as_u16();
    // Hyper retains non-canonical and explicitly empty statusText in this
    // extension. An absent extension means the canonical reason phrase.
    let status_text = response
        .extensions()
        .get::<hyper::ext::ReasonPhrase>()
        .map(|reason| String::from_utf8_lossy(reason.as_bytes()).into_owned())
        .unwrap_or_else(|| {
            response
                .status()
                .canonical_reason()
                .unwrap_or_default()
                .to_string()
        });
    // The SDK uses case-sensitive `includes("application/json")` on the value.
    let content_type = response
        .headers()
        .get("content-type")
        .map(|value| String::from_utf8_lossy(value.as_bytes()).into_owned())
        .unwrap_or_default();
    let (mut body, before_abort) = response.into_body();
    let text = response_text(&mut body, options, before_abort).await?;
    let error_body = if content_type.contains("application/json") {
        providers::google::parse_error_body(&text)?
    } else {
        providers::google::synthesized_error_body(&text, status, &status_text)
    };
    let message = providers::google::api_error_message(&error_body)?;
    let js_error = JsError {
        message,
        code: None,
        network_code: None,
        raw: JsValue::Object(providers::google::api_error_raw(status)),
    };
    let canonical = normalize_model_error(
        &provider_id(provider),
        &"google".into(),
        ErrorInput::Error(&js_error),
        Some(f64::from(status)),
    );
    Ok(ModelStreamError::Provider(canonical))
}

/// One `streamGoogleProviderRequest` attempt: send, then consume the SDK SSE
/// stream. Returns as soon as a `message_end` event is emitted, like the TS
/// loop's `stream.return(...)`.
async fn google_attempt(
    transport: &dyn ModelHttpTransport,
    provider: &JsValue,
    model: &JsValue,
    body: &JsValue,
    options: &StreamOptions,
    sink: &mut (dyn FnMut(StreamOutput) + Send),
    checkpoint: &mut Checkpoint,
) -> (Result<(), ModelStreamError>, bool) {
    // Error normalization is also a queued continuation: a later parent timer
    // must not replace an already-read SDK error with AbortError.
    let mut body_before_abort = false;
    let sdk_deadline = tokio::time::Instant::now() + duration(idle_timeout_ms(provider, options));
    let outcome = async {
        let response = send_google_request(transport, provider, model, body, options)
            .await
            .map_err(|(error, before_abort)| {
                body_before_abort = before_abort;
                error
            })?;
        body_before_abort = response.body().1;
        let status = response.status().as_u16();
        if matches!(status, 204 | 205) {
            // fetch exposes a null body for null-body statuses; the SDK's stream
            // reader throws before yielding any chunk.
            return Err(ModelStreamError::thrown("Error", "Response body is empty"));
        }
        if !(200..300).contains(&status) {
            use crate::runtime::network::{Exchange, exchange_prearmed_continuing};
            let idle_ms = idle_timeout_ms(provider, options);
            let provider = provider.clone();
            let read_options = options.clone();
            let read = crate::runtime::network::ExchangeFuture::io(async move {
                google_response_error(response, &read_options, &provider).await
            });
            // Preserve the SDK error-body promise's catch continuation.
            return match exchange_prearmed_continuing(
                read,
                wait_abort(&options.abort),
                || options.abort.as_ref().and_then(AbortFlag::abort_event_at),
                Some(sdk_deadline),
                body_before_abort,
            )
            .await
            {
                Exchange::Response(result, before_abort) => {
                    body_before_abort = before_abort;
                    Err(result?)
                }
                Exchange::Aborted => Err(ModelStreamError::abort()),
                Exchange::Timeout { before_abort } => {
                    body_before_abort = before_abort;
                    Err(ModelStreamError::thrown(
                        "StreamIdleTimeoutError",
                        format!(
                            "Stream idle timeout: no data received for {}ms",
                            crate::js_value::number_string(idle_ms)
                        ),
                    ))
                }
            };
        }
        let (mut body, _) = response.into_body();
        let mut state = providers::google::StreamState::default();
        let idle_ms = idle_timeout_ms(provider, options);
        let mut guard = StreamGuard::new(provider, (options.clock)());
        let mut buffer = sse::SseBuffer::with_limit(sse_limit());
        loop {
            if !body_before_abort {
                throw_if_aborted(options)?;
            }
            let chunk = read_frame(&mut body, idle_ms, options, &mut body_before_abort).await?;
            if !body_before_abort {
                throw_if_aborted(options)?;
            }
            let parsed = match chunk {
                Some(ref bytes) => buffer.push(bytes),
                None => buffer.finish(),
            };
            for item in parsed {
                let item = item.map_err(sse_error)?;
                guard.check_duration((options.clock)())?;
                let raw = match item {
                    sse::SseEvent::Done => continue,
                    sse::SseEvent::Data(raw) => raw,
                };
                for event in state.normalize(&raw)? {
                    let tag = field(&event, "type");
                    if tag.as_string().is_some_and(|tag| tag.equals_str("error")) {
                        return Err(ModelStreamError::Provider(object_entries(field(
                            &event, "error",
                        ))));
                    }
                    let message_end = tag
                        .as_string()
                        .is_some_and(|tag| tag.equals_str("message_end"));
                    guard.observe(&event, (options.clock)())?;
                    checkpoint.on_event(&event)?;
                    sink(StreamOutput::Event(event));
                    options.yield_boundary().await;
                    if message_end {
                        return Ok(());
                    }
                }
            }
            if chunk.is_none() {
                break;
            }
        }
        guard.check_duration((options.clock)())?;
        if !state.ended() {
            return Err(ModelStreamError::thrown(
                "IncompleteStreamError",
                "Network stream ended before provider completion sentinel.",
            ));
        }
        Ok(())
    }
    .await;
    (outcome, body_before_abort)
}

/// `throwIfGoogleAbort`: preserve abort-like errors, while an observed parent
/// cancellation takes precedence. A non-abort error continues into retry handling.
pub(super) fn throw_if_google_abort(
    error: ModelStreamError,
    options: &StreamOptions,
    before_abort: bool,
) -> Result<ModelStreamError, ModelStreamError> {
    if !before_abort && aborted(options) {
        return Err(ModelStreamError::abort());
    }
    if error.is_abort() {
        return Err(error);
    }
    Ok(error)
}

/// `streamGoogleProviderRequest`: request_started per attempt, SDK error
/// canonicalization, continuation/network retries and error events.
pub(super) async fn stream_google_model(
    transport: &dyn ModelHttpTransport,
    request: &JsValue,
    config: &JsValue,
    options: &StreamOptions,
    sink: &mut (dyn FnMut(StreamOutput) + Send),
    provider: &JsValue,
) -> Result<(), ModelStreamError> {
    let mut current = request.clone();
    let (max_retries, _) = retry_decide(provider, 0.0, None, 0.0);
    let mut checkpoint = Checkpoint::default();
    let mut attempt = 0.0;
    let mut continuation_before_abort = false;
    while attempt <= max_retries {
        if !std::mem::take(&mut continuation_before_abort) {
            throw_if_aborted(options)?;
        }
        sink(StreamOutput::Event(request_started_event(
            provider, &current,
        )?));
        options.yield_boundary().await;

        let (_, model) = providers::validate_model_request(&current, config)?;
        let model_id = field(&current, "model").clone();
        let (outcome, before_abort) = match providers::google::build_request(&current, &model) {
            Ok(body) => {
                google_attempt(
                    transport,
                    provider,
                    &model_id,
                    &body,
                    options,
                    sink,
                    &mut checkpoint,
                )
                .await
            }
            Err(error) => (Err(error), false),
        };
        let Err(error) = outcome else {
            return Ok(());
        };
        let error = throw_if_google_abort(error, options, before_abort)?;
        let canonical = match &error {
            ModelStreamError::Provider(canonical) => canonical.clone(),
            other => normalize_model_error(
                &provider_id(provider),
                &"google".into(),
                ErrorInput::Error(&other.as_js_error()),
                None,
            ),
        };
        let canonical_message = canonical
            .get("message")
            .and_then(JsValue::as_string)
            .map(|message| String::from_utf16_lossy(message.units()).to_lowercase())
            .unwrap_or_default();
        let retryable = canonical.get("retryable") == Some(&JsValue::Bool(true))
            || is_retryable_stream_error(&error)
            || retryable_message(&canonical_message);
        let current_model = field(&current, "model").clone();
        let progress =
            |sink: &mut (dyn FnMut(StreamOutput) + Send), reason: &str, delay_ms: f64| {
                let mut value = JsObject::new();
                value.insert("reason".into(), reason.into());
                value.insert("attempt".into(), JsValue::Number(attempt + 1.0));
                value.insert("maxAttempts".into(), JsValue::Number(max_retries));
                value.insert("delayMs".into(), JsValue::Number(js_round(delay_ms)));
                value.insert("provider".into(), field(provider, "id").clone());
                value.insert("model".into(), current_model.clone());
                sink(StreamOutput::RetryProgress(JsValue::Object(value)));
            };
        if attempt < max_retries && retryable && checkpoint.can_continue_text() {
            current = continuation_request(&current, &checkpoint.partial_text);
            let (_, delay_ms) = retry_decide(provider, attempt, None, (options.random)());
            progress(sink, "continuation", delay_ms);
            continuation_before_abort = delay(delay_ms, options).await?;
            attempt += 1.0;
            continue;
        }
        let empty = checkpoint
            .interruption()
            .get("phase")
            .is_some_and(|phase| phase.as_string().is_some_and(|p| p.equals_str("empty")));
        if attempt < max_retries && retryable && empty {
            let (_, delay_ms) = retry_decide(provider, attempt, None, (options.random)());
            progress(sink, "network_error", delay_ms);
            continuation_before_abort = delay(delay_ms, options).await?;
            attempt += 1.0;
            continue;
        }
        let mut canonical = canonical;
        if retryable {
            canonical.insert("streamInterruption".into(), checkpoint.interruption());
        }
        sink(StreamOutput::Event(error_event(JsValue::Object(canonical))));
        options.yield_boundary().await;
        return Ok(());
    }
    Ok(())
}
