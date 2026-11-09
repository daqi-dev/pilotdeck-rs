use super::*;
use crate::runtime::hooks::{HostedHookServer, HostedHookServerOptions};
use http_body_util::{BodyExt, Full};
use hyper_util::{
    client::legacy::{Client, connect::HttpConnector},
    rt::TokioExecutor,
};
use pilotdeck_protocol::{parse_json, stringify_json};
use std::sync::Mutex;

struct Callback<F>(F);
impl<F> HostedHookCallback for Callback<F>
where
    F: Fn(JsObject, Option<JsString>, Cancellation, JsString) -> HostedHookFuture + Send + Sync,
{
    fn call(
        &self,
        input: JsObject,
        tool: Option<JsString>,
        signal: Cancellation,
        id: JsString,
    ) -> HostedHookFuture {
        (self.0)(input, tool, signal, id)
    }
}
fn callback<F>(f: F) -> Arc<dyn HostedHookCallback>
where
    F: Fn(JsObject, Option<JsString>, Cancellation, JsString) -> HostedHookFuture
        + Send
        + Sync
        + 'static,
{
    Arc::new(Callback(f))
}
fn output(source: &str) -> Arc<dyn HostedHookCallback> {
    let value = parse_json(source).unwrap();
    callback(move |_, _, _, _| {
        let value = value.clone();
        Box::pin(async move { Ok(value) })
    })
}
fn adapter(
    callbacks: Vec<Arc<dyn HostedHookCallback>>,
    timeout: Option<f64>,
) -> HostedHookCallbacks {
    HostedHookCallbacks::new(vec![(
        "PreToolUse".into(),
        vec![HostedHookCallbackMatcher {
            matcher: Some("Bash".into()),
            timeout_seconds: timeout,
            callbacks,
        }],
    )])
    .unwrap()
}
async fn run(handler: &HostedHookCallbacks) -> Result<JsValue, HostedHookError> {
    handler
        .call(
            "PreToolUse".into(),
            0,
            JsObject::new(),
            Cancellation::new(),
            "async-id".into(),
        )
        .await
}

// TS hook-server.ts:169-207 runs each callback in array order; :263-278
// merges fields while preserving an earlier block and both system messages.
#[tokio::test]
async fn ordered_callbacks_merge_fields_and_preserve_blocking_effects() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let callback_outputs = [
        r#"{"continue":false,"decision":"block","systemMessage":"first","hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","additionalContext":"one"},"first":1}"#,
        r#"{"continue":true,"decision":"approve","systemMessage":"second","hookSpecificOutput":{"additionalContext":"two","updatedInput":{"x":1}},"last":2}"#,
    ];
    let callbacks = callback_outputs
        .iter()
        .enumerate()
        .map(|(i, source)| {
            let seen = seen.clone();
            let value = parse_json(source).unwrap();
            callback(move |input, tool, signal, id| {
                assert_eq!(input.get("session_id"), Some(&"session".into()));
                assert_eq!(tool, Some("tool-use".into()));
                assert_eq!(id, JsString::from("shared-id"));
                assert!(!signal.is_aborted());
                seen.lock().unwrap().push(i);
                let value = value.clone();
                Box::pin(async move { Ok(value) })
            })
        })
        .collect();
    let handler = adapter(callbacks, None);
    let mut input = JsObject::new();
    input.insert("session_id".into(), "session".into());
    input.insert("tool_use_id".into(), "tool-use".into());
    input.insert("tool_call_id".into(), "fallback".into());
    let result = handler
        .call(
            "PreToolUse".into(),
            0,
            input,
            Cancellation::new(),
            "shared-id".into(),
        )
        .await
        .unwrap();
    assert_eq!(*seen.lock().unwrap(), vec![0, 1]);
    assert_eq!(result,parse_json(r#"{"continue":false,"decision":"block","systemMessage":"first\nsecond","hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","additionalContext":"two","updatedInput":{"x":1}},"first":1,"last":2}"#).unwrap());
}

// TS :186-206 forbids async+sync, sync+async, and two async outputs. Falsy
// callback results are skipped, while a real empty object is a sync result.
#[tokio::test]
async fn async_outputs_reject_mixed_effects_and_validate_their_timeout() {
    for sources in [
        vec![r#"{"continue":false}"#, r#"{"async":true}"#],
        vec![r#"{"async":true}"#, r#"{}"#],
        vec![r#"{"async":true}"#, r#"{"async":true}"#],
    ] {
        let error = run(&adapter(sources.into_iter().map(output).collect(), None))
            .await
            .unwrap_err();
        assert_eq!(error, mixed_output());
    }
    for timeout in ["0", "-1", "null", "\"10\""] {
        let handler = adapter(
            vec![output(&format!(
                r#"{{"async":true,"asyncTimeout":{timeout}}}"#
            ))],
            None,
        );
        assert_eq!(
            run(&handler).await.unwrap_err().message,
            "asyncTimeout must be a positive finite number of seconds."
        );
    }
    let handler = adapter(
        vec![
            output("null"),
            output("false"),
            output("{}"),
            output(r#"{"async":true,"asyncTimeout":2}"#),
        ],
        None,
    );
    assert_eq!(
        run(&handler).await.unwrap(),
        parse_json(r#"{"async":true,"asyncTimeout":2}"#).unwrap()
    );
}

// TS :309-353 starts a separate timer for every callback. The aggregate
// matcher can take longer than that timeout and still succeed.
#[tokio::test]
async fn callback_timeouts_apply_individually_and_stop_after_failure() {
    let mut callbacks = Vec::new();
    for _ in 0..3 {
        callbacks.push(callback(|_, _, _, _| {
            Box::pin(async {
                tokio::time::sleep(Duration::from_millis(55)).await;
                Ok(JsValue::Object(JsObject::new()))
            })
        }));
    }
    let handler = adapter(callbacks, Some(0.1));
    let started = tokio::time::Instant::now();
    run(&handler).await.unwrap();
    assert!(started.elapsed() >= Duration::from_millis(150));
    let calls = Arc::new(Mutex::new(Vec::new()));
    let signal_seen = Arc::new(Mutex::new(None));
    let signal_slot = signal_seen.clone();
    let calls_first = calls.clone();
    let first = callback(move |_, _, signal, _| {
        calls_first.lock().unwrap().push(1);
        *signal_slot.lock().unwrap() = Some(signal);
        Box::pin(std::future::pending())
    });
    let calls_second = calls.clone();
    let second = callback(move |_, _, _, _| {
        calls_second.lock().unwrap().push(2);
        Box::pin(async { Ok(JsValue::Undefined) })
    });
    let error = run(&adapter(vec![first, second], Some(0.01)))
        .await
        .unwrap_err();
    assert_eq!(error.code, "timeout");
    assert_eq!(error.message, "SDK hook callback timed out after 0.01s.");
    assert_eq!(*calls.lock().unwrap(), vec![1]);
    assert!(signal_seen.lock().unwrap().as_ref().unwrap().is_aborted());
}

#[tokio::test]
async fn timed_callback_observes_parent_abort_and_invalid_timeout_never_calls_it() {
    let started = Arc::new(tokio::sync::Notify::new());
    let started_callback = started.clone();
    let signal_seen = Arc::new(Mutex::new(None));
    let slot = signal_seen.clone();
    let cb = callback(move |_, _, signal, _| {
        *slot.lock().unwrap() = Some(signal);
        started_callback.notify_one();
        Box::pin(std::future::pending())
    });
    let handler = adapter(vec![cb.clone()], Some(1.0));
    let parent = Cancellation::new();
    let task = tokio::spawn(handler.call(
        "PreToolUse".into(),
        0,
        JsObject::new(),
        parent.clone(),
        "id".into(),
    ));
    started.notified().await;
    parent.abort();
    assert_eq!(task.await.unwrap().unwrap_err(), aborted());
    assert!(signal_seen.lock().unwrap().as_ref().unwrap().is_aborted());
    *signal_seen.lock().unwrap() = None;
    for timeout in [0.0, -1.0, f64::INFINITY, f64::NAN] {
        assert_eq!(
            run(&adapter(vec![cb.clone()], Some(timeout)))
                .await
                .unwrap_err()
                .code,
            "validation_error"
        );
        assert!(signal_seen.lock().unwrap().is_none());
    }
}

// Exercise the product constructor through a real HTTP request: serializable
// event/matcher selection uses the same array owner, and native output still
// validates the merged event and converts the SDK `defer` spelling.
#[tokio::test]
async fn callback_server_runs_the_ordered_adapter_through_http() {
    let cb = callback(|input, tool, _, _| {
        assert_eq!(input.get("session_id"), Some(&"s".into()));
        assert_eq!(tool, Some("fallback".into()));
        Box::pin(async {
            Ok(parse_json(r#"{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"defer"}}"#).unwrap())
        })
    });
    let server = HostedHookServer::from_callbacks(vec![(
        "PreToolUse".into(),
        vec![HostedHookCallbackMatcher {
            matcher: Some("Bash".into()),
            timeout_seconds: Some(1.0),
            callbacks: vec![
                output(r#"{"systemMessage":"one"}"#),
                cb,
                output(r#"{"systemMessage":"two"}"#),
            ],
        }],
    )])
    .unwrap();
    let endpoint = server
        .start(HostedHookServerOptions::default())
        .await
        .unwrap();
    let client: Client<HttpConnector, Full<bytes::Bytes>> =
        Client::builder(TokioExecutor::new()).build_http();
    let request=hyper::Request::builder().method("POST").uri(format!("{}?event=PreToolUse&matcher=0",endpoint.url)).header(hyper::header::AUTHORIZATION,&endpoint.authorization).body(Full::new(bytes::Bytes::from_static(br#"{"hookEventName":"PreToolUse","sessionId":"s","transcriptPath":"","cwd":"/isolated","toolCallId":"fallback"}"#))).unwrap();
    let response = client.request(request).await.unwrap();
    assert_eq!(response.status(), hyper::StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let actual = parse_json(std::str::from_utf8(&bytes).unwrap()).unwrap();
    assert_eq!(actual,parse_json(r#"{"systemMessage":"one\ntwo","hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"passthrough"}}"#).unwrap(),"{}",stringify_json(&actual).unwrap().unwrap());
    server.close().await;
}
