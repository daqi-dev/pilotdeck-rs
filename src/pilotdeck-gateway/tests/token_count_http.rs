use pilotdeck_gateway::host::HostFuture;
use pilotdeck_gateway::host::modules::budget::{EvaluationOptions, RequestBudgetEvaluator};
use pilotdeck_gateway::runtime::model::token_accounting::TokenAccountingRuntime;
use pilotdeck_gateway::runtime::model::{
    CountHttpRequest, HyperCountTransport, ProviderCountTransport,
};
use pilotdeck_protocol::JsValue;
use std::{
    convert::Infallible,
    sync::{Arc, Mutex},
};
#[path = "common/baseline.rs"]
mod baseline;
#[path = "common/token_count_http.rs"]
mod fixture;

struct Guarded(Arc<Mutex<fixture::Exchange>>);
impl ProviderCountTransport for Guarded {
    fn post(&self, request: CountHttpRequest) -> HostFuture<JsValue> {
        fixture::guard(&request, &self.0);
        HyperCountTransport::default().post(request)
    }
}
struct Server(tokio::task::JoinHandle<()>);
impl Drop for Server {
    fn drop(&mut self) {
        self.0.abort();
    }
}
async fn listen(exchange: Arc<Mutex<fixture::Exchange>>, index: usize) -> Server {
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("loopback HTTP fixture needs listen permission");
    let origin = format!("http://{}", listener.local_addr().unwrap());
    exchange.lock().unwrap().origins[index] = origin.clone();
    Server(tokio::spawn(async move {
        let mut connections = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                connection = listener.accept() => {
                    let (socket, _) = connection.unwrap();
                    let exchange = exchange.clone();
                    let origin = origin.clone();
                    connections.spawn(async move {
                        let service = hyper::service::service_fn(move |request| {
                            let exchange = exchange.clone(); let origin = origin.clone();
                            async move { Ok::<_, Infallible>(fixture::respond(request, &origin, &exchange).await) }
                        });
                        let _ = hyper::server::conn::http1::Builder::new().serve_connection(hyper_util::rt::TokioIo::new(socket), service).await;
                    });
                }
                Some(_) = connections.join_next(), if !connections.is_empty() => {}
            }
        }
    }))
}
#[tokio::test]
async fn real_ts_fetch_redirects_and_json_errors_over_loopback_http() {
    let exchange = Arc::new(Mutex::new(fixture::Exchange::default()));
    let _first = listen(exchange.clone(), 0).await;
    let _second = listen(exchange.clone(), 1).await;
    fixture::replay(exchange.clone(), Arc::new(Guarded(exchange))).await;
}

#[test]
#[ignore = "requires fixed TS checkout, Node 22.22.0/tsx, and loopback listen permission"]
fn regenerate_http_twice_identically() {
    let baseline = std::env::var_os("PDRS_BASELINE").expect("set PDRS_BASELINE");
    let directory =
        std::env::temp_dir().join(format!("pdrs-token-count-http-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    let mut generated = Vec::new();
    for _ in 0..2 {
        let output = std::process::Command::new("node")
            .arg("--import")
            .arg(std::path::Path::new(&baseline).join("node_modules/tsx/dist/loader.mjs"))
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/token-accounting/generate-http.mts"
            ))
            .arg(&directory)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8(output.stdout).unwrap();
        let metadata: serde_json::Value =
            serde_json::from_str(stdout.lines().last().unwrap()).unwrap();
        assert_eq!(metadata["transport"], "loopback HTTP");
        assert_eq!(metadata["cases"], 149);
        assert_eq!(metadata["requests"], 324);
        generated.push(
            std::fs::read(
                std::path::Path::new(metadata["output"].as_str().unwrap()).join("http-oracle.json"),
            )
            .unwrap(),
        );
    }
    assert_eq!(generated[0], generated[1]);
    assert_eq!(generated[0], fixture::ORACLE.as_bytes());
}
