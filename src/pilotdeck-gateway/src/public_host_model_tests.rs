use super::*;
use crate::runtime::{
    model::{
        fallback::RouterStreamFuture,
        http::HyperModelTransport,
        provider_registry::{ModelInvocationProvider, NativeModelInvocationProvider},
    },
    project::{ProjectRuntimeRegistry, ResourceBundle, RuntimeResult},
};
use pilotdeck_protocol::{parse_json, stringify_json};
use std::{
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

fn oracle() -> JsValue {
    crate::test_baseline::oracle(include_str!(
        "../tests/fixtures/public-host-model/oracle.json"
    ))
}
fn json(text: &str) -> JsValue {
    parse_json(text).unwrap()
}
fn wire_response(response: HostResponse) -> JsValue {
    let HostBody::Value(body) = response.body else {
        panic!("value response")
    };
    object([
        ("status", JsValue::Number(f64::from(response.status))),
        ("body", body),
    ])
}
#[tokio::test]
async fn all_fixed_ts_public_prepare_cases_have_exact_native_consumers() {
    let oracle = oracle();
    let config = field(&oracle, "config");
    let providers = Arc::new(ModelInvocationProviderRegistry::new(
        tokio::runtime::Handle::current(),
    ));
    providers
        .register(Arc::new(NativeModelInvocationProvider::new(
            field(field(config, "providers"), "p").clone(),
            Arc::new(HyperModelTransport::new()),
        )))
        .unwrap();
    let model = ModelResources {
        providers: providers.clone(),
        config: config.clone(),
    };
    let cases = array(field(&oracle, "cases")).unwrap();
    assert_eq!(cases.len(), 34);
    for row in cases {
        let response = match prepare(field(row, "input"), field(row, "catalog"), &model) {
            Ok(prepared) => value_response(200, prepared.body),
            Err(error) => request_error_response(error),
        };
        assert_eq!(
            wire_response(response),
            *field(row, "response"),
            "case {}",
            stringify_json(field(row, "case")).unwrap().unwrap()
        );
    }
    providers.dispose().await.unwrap();
}
#[test]
fn public_budget_safe_integer_edges_do_not_coerce_or_round() {
    for n in [
        0.0,
        -0.0,
        -1.0,
        0.5,
        f64::NAN,
        f64::INFINITY,
        9_007_199_254_740_992.0,
    ] {
        assert!(!safe_positive(n));
    }
    assert!(safe_positive(9_007_199_254_740_991.0));
    assert!(positive(&JsValue::Undefined, "x").unwrap().is_none());
    assert!(positive(&JsValue::from("2"), "x").is_err());
}
type Observation = Arc<std::sync::Mutex<Option<(JsValue, Option<f64>, AbortFlag)>>>;
struct StreamingProvider {
    id: JsString,
    entered: Arc<tokio::sync::Notify>,
    dropped: Arc<AtomicBool>,
    observed: Observation,
}
struct Dropped(Arc<AtomicBool>);
impl Drop for Dropped {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}
impl ModelInvocationProvider for StreamingProvider {
    fn provider_id(&self) -> &JsString {
        &self.id
    }
}
impl RouterModelInvocation for StreamingProvider {
    fn get_capabilities(&self, _: &JsString, _: &JsString) -> Result<JsValue, ModuleError> {
        Ok(json("{\"maxOutputTokens\":256}"))
    }
    fn get_multimodal(&self, _: &JsString, _: &JsString) -> Result<JsValue, ModuleError> {
        Ok(JsValue::Undefined)
    }
    fn stream<'a>(
        &'a self,
        request: &'a JsValue,
        options: &'a StreamOptions,
        sink: &'a mut (dyn FnMut(StreamOutput) + Send),
    ) -> RouterStreamFuture<'a> {
        Box::pin(async move {
            let _dropped = Dropped(self.dropped.clone());
            *self.observed.lock().unwrap() = Some((
                request.clone(),
                options.stream_timeout_ms,
                options.abort.clone().unwrap(),
            ));
            self.entered.notify_one();
            sink(StreamOutput::Event(json(
                "{\"type\":\"text_delta\",\"text\":\"actual event\"}",
            )));
            options.yield_boundary().await;
            pending().await
        })
    }
}
#[tokio::test]
async fn response_drop_cancels_provider_and_releases_the_retired_generation() {
    let registry = Arc::new(ProjectRuntimeRegistry::new(
        std::env::temp_dir(),
        Arc::new(|_: &Path, _: &mut ResourceBundle| -> RuntimeResult { Ok(()) }),
    ));
    let generation = Arc::new(registry.acquire(None).unwrap());
    let runtime = generation.runtime().clone();
    let providers = Arc::new(ModelInvocationProviderRegistry::new(
        tokio::runtime::Handle::current(),
    ));
    let dropped = Arc::new(AtomicBool::new(false));
    let entered = Arc::new(tokio::sync::Notify::new());
    let observed = Arc::new(std::sync::Mutex::new(None));
    providers
        .register(Arc::new(StreamingProvider {
            id: "p".into(),
            entered: entered.clone(),
            dropped: dropped.clone(),
            observed: observed.clone(),
        }))
        .unwrap();
    let body = json("{\"selection\":{\"selectedModelId\":\"m\",\"providerId\":\"p\"}}");
    let response = stream_response(
        Prepared {
            body,
            request: json("{\"provider\":\"p\",\"model\":\"m\"}"),
            timeout_ms: Some(17.0),
        },
        providers.clone(),
        generation,
        None,
    );
    let HostBody::Stream(mut receiver) = response.body else {
        panic!("stream")
    };
    let event = tokio::time::timeout(Duration::from_secs(2), receiver.recv())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let HostEvent::Value(event) = event else {
        panic!("canonical event")
    };
    assert_eq!(
        event,
        json("{\"type\":\"text_delta\",\"text\":\"actual event\"}")
    );
    let (request, timeout, abort) = observed.lock().unwrap().clone().unwrap();
    assert_eq!(field(&request, "stream"), &JsValue::Bool(true));
    assert_eq!(timeout, Some(17.0));
    let disposing = tokio::spawn(async move { registry.dispose().await });
    tokio::time::timeout(Duration::from_secs(2), async {
        while runtime.state() != crate::runtime::project::RuntimeState::Retired {
            tokio::task::yield_now().await
        }
    })
    .await
    .unwrap();
    assert_eq!(runtime.lease_count(), 1);
    assert!(!disposing.is_finished());
    drop(receiver);
    tokio::time::timeout(Duration::from_secs(2), disposing)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(abort.is_aborted());
    assert!(dropped.load(Ordering::SeqCst));
    providers.dispose().await.unwrap();
}

#[test]
#[ignore = "requires fixed PDRS_BASELINE checkout and Node toolchain"]
fn fixed_ts_public_model_oracle_regenerates_identically() {
    crate::test_baseline::regenerates_identically("public-host-model");
}
#[tokio::test]
async fn already_cancelled_prepare_returns_499_before_reading_runtime() {
    let registry = ProjectRuntimeRegistry::new(
        std::env::temp_dir(),
        Arc::new(|_: &Path, _: &mut ResourceBundle| -> RuntimeResult { Ok(()) }),
    );
    let generation = Arc::new(registry.acquire(None).unwrap());
    let signal = AbortFlag::new();
    signal.abort();
    let response = call(
        &JsValue::Null,
        &JsValue::Null,
        generation,
        false,
        Some(signal),
    )
    .await
    .unwrap();
    assert_eq!(
        wire_response(response),
        json("{\"status\":499,\"body\":{\"code\":\"PUBLIC_HOST_CANCELLED\"}}")
    );
    registry.dispose().await.unwrap();
}
#[tokio::test]
async fn parent_cancellation_closes_provider_with_a_failed_stream_body() {
    let registry = ProjectRuntimeRegistry::new(
        std::env::temp_dir(),
        Arc::new(|_: &Path, _: &mut ResourceBundle| -> RuntimeResult { Ok(()) }),
    );
    let generation = Arc::new(registry.acquire(None).unwrap());
    let providers = Arc::new(ModelInvocationProviderRegistry::new(
        tokio::runtime::Handle::current(),
    ));
    let dropped = Arc::new(AtomicBool::new(false));
    providers
        .register(Arc::new(StreamingProvider {
            id: "p".into(),
            entered: Arc::new(tokio::sync::Notify::new()),
            dropped: dropped.clone(),
            observed: Arc::new(std::sync::Mutex::new(None)),
        }))
        .unwrap();
    let signal = AbortFlag::new();
    let response = stream_response(
        Prepared {
            body: json("{\"selection\":{\"selectedModelId\":\"m\",\"providerId\":\"p\"}}"),
            request: json("{\"provider\":\"p\",\"model\":\"m\"}"),
            timeout_ms: None,
        },
        providers.clone(),
        generation,
        Some(signal.clone()),
    );
    let HostBody::Stream(mut receiver) = response.body else {
        panic!("stream")
    };
    assert!(
        tokio::time::timeout(Duration::from_secs(2), receiver.recv())
            .await
            .unwrap()
            .unwrap()
            .is_ok()
    );
    signal.abort();
    let error = tokio::time::timeout(Duration::from_secs(2), receiver.recv())
        .await
        .unwrap()
        .unwrap()
        .err()
        .unwrap();
    assert_eq!(error, "This operation was aborted");
    assert!(dropped.load(Ordering::SeqCst));
    assert!(
        tokio::time::timeout(Duration::from_secs(2), receiver.recv())
            .await
            .unwrap()
            .is_none()
    );
    registry.dispose().await.unwrap();
    providers.dispose().await.unwrap();
}
