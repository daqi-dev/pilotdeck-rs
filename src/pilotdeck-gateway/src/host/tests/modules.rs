use super::*;
use crate::host::{
    execution::module_response,
    modules::{ModuleRouter, wire},
};

fn oracle() -> JsValue {
    let value = parse_json(include_str!(
        "../../../tests/fixtures/host-modules/oracle.json"
    ))
    .unwrap();
    assert_eq!(array(&value, "routing").len(), 863);
    assert_eq!(array(&value, "errors").len(), 11);
    assert_eq!(array(&value, "fields").len(), 90);
    value
}
fn array<'a>(value: &'a JsValue, key: &str) -> &'a [JsValue] {
    let JsValue::Array(items) = value.get(key).unwrap() else {
        panic!("{key}")
    };
    items
}
fn nonfinite(value: &JsValue) -> Option<f64> {
    match value
        .get("nonfinite")?
        .as_string()?
        .to_utf8()
        .unwrap()
        .as_str()
    {
        "NaN" => Some(f64::NAN),
        "Infinity" => Some(f64::INFINITY),
        "-Infinity" => Some(f64::NEG_INFINITY),
        other => panic!("unexpected special number {other}"),
    }
}
fn module_error(case: &JsValue) -> HostError {
    let mut fields = case.get("fields").unwrap().as_object().unwrap().clone();
    if let Some(value) = nonfinite(case) {
        fields.insert("retryAfterMs".into(), JsValue::Number(value));
    }
    ModuleError::from_js_fields(
        case.get("message").unwrap().as_string().unwrap().clone(),
        &JsValue::Object(fields),
    )
    .into()
}
#[tokio::test]
async fn real_ts_module_routing_and_error_projection() {
    let oracle = oracle();
    let router = ModuleRouter::default();
    for case in array(&oracle, "routing") {
        let call = case.get("call").unwrap();
        let response = module_response(call, router.module_call(call.clone()).await);
        assert_eq!(&response, case.get("response").unwrap(), "call={call:?}");
    }
    for case in array(&oracle, "errors") {
        assert_eq!(
            &module_response(case.get("call").unwrap(), Err(module_error(case))),
            case.get("response").unwrap()
        );
    }
    // Generic transport/callback errors keep their old fallback shape.
    let call = array(&oracle, "errors")[0].get("call").unwrap();
    let projected = module_response(call, Err(HostError::new(HostErrorKind::Callback, "plain")));
    assert!(text_is(projected.get("code"), "HOST_MODULE_FAILED"));
    assert_eq!(
        projected.get("error").unwrap(),
        &parse_json(r#"{"message":"plain"}"#).unwrap()
    );
    router.dispose().await.unwrap();
}
#[test]
fn real_ts_payload_readers() {
    let oracle = oracle();
    for case in array(&oracle, "fields") {
        let input = case.get("input").unwrap();
        let value = nonfinite(input)
            .map(JsValue::Number)
            .unwrap_or_else(|| input.get("value").cloned().unwrap_or(JsValue::Undefined));
        let mut object = JsObject::new();
        if input.get("absent").is_none() {
            object.insert("value".into(), value);
        }
        let record = if input.get("missing").is_some() {
            None
        } else {
            Some(&object)
        };
        let value = record.and_then(|object| object.get("value"));
        let helper = case
            .get("helper")
            .unwrap()
            .as_string()
            .unwrap()
            .to_utf8()
            .unwrap();
        let result = match helper.as_str() {
            "stringField" => {
                wire::string_field(record, "value").map(|v| Some(JsValue::String(v.clone())))
            }
            "optionalStringField" => wire::optional_string_field(record, "value")
                .map(|v| v.map(|v| JsValue::String(v.clone()))),
            "booleanField" => wire::boolean_field(record, "value").map(|v| Some(JsValue::Bool(v))),
            "positiveFinite" => {
                wire::positive_finite(value, "value").map(|v| Some(JsValue::Number(v)))
            }
            "optionalNonNegativeFinite" => {
                wire::optional_non_negative_finite(value, "value").map(|v| v.map(JsValue::Number))
            }
            other => panic!("unknown helper {other}"),
        };
        let mut actual = JsObject::new();
        match result {
            Ok(Some(value)) => {
                actual.insert("value".into(), value);
            }
            Ok(None) => {
                actual.insert("missing".into(), JsValue::Bool(true));
            }
            Err(error) => {
                actual.insert("error".into(), error.message.as_str().into());
            }
        }
        assert_eq!(
            &JsValue::Object(actual),
            case.get("expected").unwrap(),
            "{case:?}"
        );
    }
    assert_eq!(wire::optional_non_negative_finite(None, "x").unwrap(), None);
    assert!(wire::positive_finite(None, "x").is_err());
}
struct ErrorHandler;
impl HostHandler for ErrorHandler {
    fn module_call(&self, call: JsValue) -> HostFuture<JsObject> {
        Box::pin(async move {
            let oracle = oracle();
            let case = array(&oracle, "errors")
                .iter()
                .find(|case| case.get("call") == Some(&call))
                .expect("known error fixture");
            Err(module_error(case))
        })
    }
}
#[tokio::test]
async fn stdio_routes_multiple_modules_and_projects_error_details() {
    for (mode, handler) in [
        (
            "module-routing",
            Arc::new(ModuleRouter::default()) as Arc<dyn HostHandler>,
        ),
        (
            "module-errors",
            Arc::new(ErrorHandler) as Arc<dyn HostHandler>,
        ),
    ] {
        let (mut command, receipt) = peer(mode);
        let client = HostClient::spawn(&mut command, None, protocol_limits())
            .await
            .unwrap();
        assert!(matches!(
            client.execute(request(), handler).await.unwrap(),
            ExecuteResult::Terminal(_)
        ));
        receipt.assert_pass(mode);
    }
}
#[tokio::test]
async fn dispose_once_after_callbacks_on_every_exit_path() {
    for mode in [
        "positive",
        "rejected",
        "identity",
        "disconnect",
        "callback-disconnect",
        "terminal-pending-callback",
        "cleanup-hang",
    ] {
        let (mut command, receipt) = peer(mode);
        let mut limits = protocol_limits();
        limits.close_timeout = Duration::from_millis(200);
        let client = HostClient::spawn(&mut command, None, limits).await.unwrap();
        let handler = Handler::new(mode);
        let result = client.execute(request(), handler.clone()).await;
        if matches!(
            mode,
            "positive" | "rejected" | "terminal-pending-callback" | "cleanup-hang"
        ) {
            result.unwrap();
            receipt.assert_pass(mode);
        } else {
            assert!(result.is_err(), "{mode}");
        }
        assert_eq!(handler.disposed.load(Ordering::SeqCst), 1, "{mode}");
        assert_eq!(handler.calls_at_dispose.load(Ordering::SeqCst), 0, "{mode}");
    }
    let (mut command, receipt) = peer("cancel-pre-execute");
    let client = HostClient::spawn(&mut command, None, protocol_limits())
        .await
        .unwrap();
    let handler = Handler::new("cancel-pre-execute");
    let cancel = Cancellation::default();
    cancel.cancel(None);
    client
        .execute_cancellable(request(), handler.clone(), cancel)
        .await
        .unwrap();
    receipt.assert_pass("cancel-pre-execute");
    assert_eq!(handler.disposed.load(Ordering::SeqCst), 1);
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropped_execute_joins_callbacks_before_dispose() {
    let mut command = command("callback-timeout");
    let client = HostClient::spawn(&mut command, None, protocol_limits())
        .await
        .unwrap();
    let handler = Handler::new("callback-timeout");
    let task = tokio::spawn(client.execute(request(), handler.clone()));
    timeout(Duration::from_secs(5), async {
        while handler.active_calls.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    timeout(Duration::from_secs(5), async {
        while handler.disposed.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(handler.disposed.load(Ordering::SeqCst), 1);
    assert_eq!(handler.calls_at_dispose.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn execute_dropped_outside_runtime_still_joins_callbacks_and_disposes_once() {
    let mut command = command("callback-timeout");
    let client = HostClient::spawn(&mut command, None, protocol_limits())
        .await
        .unwrap();
    let handler = Handler::new("callback-timeout");
    let mut execution = Box::pin(client.execute(request(), handler.clone()));
    timeout(Duration::from_secs(5), async {
        tokio::select! {
            result = &mut execution => panic!("execution finished before drop: {result:?}"),
            () = async {
                while handler.active_calls.load(Ordering::SeqCst) == 0 {
                    tokio::task::yield_now().await;
                }
            } => {}
        }
    })
    .await
    .unwrap();
    std::thread::spawn(move || {
        assert!(tokio::runtime::Handle::try_current().is_err());
        drop(execution);
    })
    .join()
    .unwrap();
    timeout(Duration::from_secs(5), async {
        while handler.disposed.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(handler.disposed.load(Ordering::SeqCst), 1);
    assert_eq!(handler.calls_at_dispose.load(Ordering::SeqCst), 0);
    assert_eq!(handler.active_calls.load(Ordering::SeqCst), 0);
}

struct DisposeProbe {
    started: Arc<AtomicUsize>,
    finished: Arc<AtomicUsize>,
    release: Arc<Notify>,
    mode: &'static str,
}
impl DisposeProbe {
    fn new(mode: &'static str) -> Arc<Self> {
        Arc::new(Self {
            started: Arc::new(AtomicUsize::new(0)),
            finished: Arc::new(AtomicUsize::new(0)),
            release: Arc::new(Notify::new()),
            mode,
        })
    }
}
struct DisposeFinished(Arc<AtomicUsize>);
impl Drop for DisposeFinished {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}
impl HostHandler for DisposeProbe {
    fn module_call(&self, _call: JsValue) -> HostFuture<JsObject> {
        Box::pin(async { Ok(JsObject::new()) })
    }
    fn dispose(&self) -> HostFuture<()> {
        let (started, finished, release, mode) = (
            self.started.clone(),
            self.finished.clone(),
            self.release.clone(),
            self.mode,
        );
        Box::pin(async move {
            started.fetch_add(1, Ordering::SeqCst);
            let _finished = DisposeFinished(finished);
            match mode {
                "reject" => Err(HostError::new(HostErrorKind::Callback, "cleanup rejection")),
                "panic" => panic!("controlled cleanup panic"),
                _ => {
                    release.notified().await;
                    Ok(())
                }
            }
        })
    }
}
#[tokio::test]
async fn disposer_timeout_rejection_and_panic_keep_the_terminal() {
    for mode in ["pending", "reject", "panic"] {
        let (mut command, receipt) = peer("positive");
        let mut limits = protocol_limits();
        limits.close_timeout = Duration::from_millis(100);
        let client = HostClient::spawn(&mut command, None, limits).await.unwrap();
        let handler = DisposeProbe::new(mode);
        let result = timeout(
            Duration::from_secs(5),
            client.execute(request(), handler.clone()),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(matches!(result, ExecuteResult::Terminal(_)));
        receipt.assert_pass("positive");
        assert_eq!(handler.started.load(Ordering::SeqCst), 1, "{mode}");
        assert_eq!(handler.finished.load(Ordering::SeqCst), 1, "{mode}");
    }
}
#[tokio::test]
async fn dropping_execute_during_dispose_does_not_cancel_or_repeat_cleanup() {
    let (mut command, receipt) = peer("positive");
    let client = HostClient::spawn(&mut command, None, protocol_limits())
        .await
        .unwrap();
    let handler = DisposeProbe::new("wait");
    let task = tokio::spawn(client.execute(request(), handler.clone()));
    timeout(Duration::from_secs(5), async {
        while handler.started.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert_eq!(handler.finished.load(Ordering::SeqCst), 0);
    handler.release.notify_one();
    timeout(Duration::from_secs(5), async {
        while handler.finished.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(handler.started.load(Ordering::SeqCst), 1);
    assert_eq!(handler.finished.load(Ordering::SeqCst), 1);
    receipt.assert_pass("positive");
}
