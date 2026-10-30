//! #1082 real registry leases, bundle ownership and service continuations.
use super::same_root_tests::{Root, dependencies, js, options};
use crate::{
    host::Cancellation,
    runtime::{
        context::memory::{MemoryCaptureTurnInput, MemoryRetrieveInput},
        memory::{
            MemoryModelConfig, MemoryModelProvider, ProjectMemoryBundle,
            ProjectMemoryBundleOptions,
            edgeclaw::{
                factory::{CreatedMemoryProvider, create_with_dependencies},
                service::FlushOptions,
            },
        },
        model::http::{ModelHttpTransport, TransportFuture},
        project::{DisposeFuture, ProjectRuntimeRegistry, ResourceBundle, RuntimeResources},
    },
};
use http_body_util::BodyExt;
use pilotdeck_protocol::JsValue;
use serde_json::{Value, json};
use std::{
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::Semaphore;

struct Gate {
    entered: Semaphore,
    release: Semaphore,
}
impl Gate {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            entered: Semaphore::new(0),
            release: Semaphore::new(0),
        })
    }
    async fn enter(&self) {
        self.entered.add_permits(1);
        bounded(self.release.acquire())
            .await
            .expect("#1082 INVALID gate closed")
            .forget();
    }
    async fn reached(&self) {
        bounded(self.entered.acquire())
            .await
            .expect("#1082 INVALID entry gate closed")
            .forget();
    }
    fn open(&self) {
        self.release.add_permits(1);
    }
}
async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(10), future)
        .await
        .expect("#1082 INVALID lifecycle gate timeout")
}
struct Transport {
    gate: Arc<Gate>,
    calls: AtomicUsize,
}
impl ModelHttpTransport for Transport {
    fn send(&self, _: hyper::Request<http_body_util::Full<bytes::Bytes>>) -> TransportFuture {
        let first = self.calls.fetch_add(1, Ordering::SeqCst) == 0;
        let gate = self.gate.clone();
        TransportFuture::io(async move {
            if first {
                gate.enter().await;
            }
            let content = json!({"route":"none","should_store":false,"labels":[]}).to_string();
            let body = bytes::Bytes::from(
                json!({"choices":[{"message":{"content":content}}]}).to_string(),
            );
            Ok(hyper::Response::new(
                http_body_util::Full::new(body)
                    .map_err(|never| match never {})
                    .boxed(),
            ))
        })
    }
}
struct Generation {
    created: CreatedMemoryProvider,
    bundle: ProjectMemoryBundle,
    closes: AtomicUsize,
    close_gate: Option<Arc<Gate>>,
}
struct Owner(Arc<Generation>);
impl RuntimeResources for Owner {
    fn dispose(&self) -> DisposeFuture<'_> {
        Box::pin(async move {
            if let Some(gate) = &self.0.close_gate {
                gate.enter().await;
            }
            self.0.bundle.dispose().await?;
            self.0.closes.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }
}
async fn capture(g: &Generation, session: &str) {
    let message_value = js(&json!([
        {"role":"user","content":[{"type":"text","text":"Project Aurora must retain the blue deployment rule."}]},
        {"role":"assistant","content":[{"type":"text","text":"I will retain the blue deployment rule."}]}
    ]));
    let JsValue::Array(messages) = &message_value else {
        panic!("#1082 INVALID messages");
    };
    g.created
        .provider
        .capture_turn(MemoryCaptureTurnInput {
            session_id: session.into(),
            project_root: g
                .created
                .service
                .workspace_dir
                .to_string_lossy()
                .into_owned(),
            messages: messages.clone(),
            errored: JsValue::Bool(false),
        })
        .await
        .expect("#1082 lifecycle capture succeeds");
}
fn rows(g: &Generation, session: &str) -> usize {
    g.created
        .service
        .repository
        .storage
        .list_all_l0()
        .expect("#1082 lifecycle rows")
        .iter()
        .filter(|row| row.session_key == session)
        .count()
}
fn encode(value: &JsValue) -> Value {
    let text = pilotdeck_protocol::stringify_json(value)
        .expect("#1082 lifecycle encode")
        .expect("#1082 lifecycle JSON defined");
    serde_json::from_str(&text).expect("#1082 lifecycle JSON")
}
async fn matrix(name: &str) {
    let root = Root::new();
    let llm = Gate::new();
    let close = Gate::new();
    let generations = Arc::new(Mutex::new(Vec::<Arc<Generation>>::new()));
    let factory = {
        let root = root.0.clone();
        let generations = generations.clone();
        let llm = llm.clone();
        let close = close.clone();
        Arc::new(move |workspace: &Path, resources: &mut ResourceBundle| {
            let mut saved = generations
                .lock()
                .expect("#1082 INVALID generation records");
            let mut input = options(&root);
            input.project_root = workspace.to_path_buf();
            let mut deps = dependencies(&root);
            input.model_config = Some(MemoryModelConfig {
                providers: std::collections::BTreeMap::from([(
                    "fixture".into(),
                    MemoryModelProvider {
                        protocol: Some("openai".into()),
                        url: Some("http://fixture.invalid/v1".into()),
                        api_key: Some("fixture-key".into()),
                    },
                )]),
            });
            deps.transport = Some(Arc::new(Transport {
                gate: llm.clone(),
                calls: AtomicUsize::new(0),
            }));
            let created = create_with_dependencies(&input, deps)
                .expect("#1082 lifecycle real factory")
                .expect("#1082 lifecycle enabled factory");
            let native = crate::runtime::memory::NativeProjectMemoryProvider {
                provider: created.provider.clone(),
                service: created.service.clone(),
            };
            let mut bundle_options = ProjectMemoryBundleOptions::new(workspace);
            bundle_options.create_provider = Some(Arc::new(move |_| {
                Ok(Some(crate::runtime::memory::NativeProjectMemoryProvider {
                    provider: native.provider.clone(),
                    service: native.service.clone(),
                }))
            }));
            let bundle = ProjectMemoryBundle::new(bundle_options);
            bundle.stage().expect("#1082 lifecycle bundle stage");
            let g = Arc::new(Generation {
                created,
                bundle,
                closes: AtomicUsize::new(0),
                close_gate: saved.is_empty().then(|| close.clone()),
            });
            resources.add(Owner(g.clone()));
            saved.push(g);
            Ok(())
        })
    };
    let registry = ProjectRuntimeRegistry::new(root.0.join("workspace"), factory);
    let lease = registry
        .acquire(None)
        .expect("#1082 INVALID acquire real lease");
    let runtime = lease.runtime().clone();
    let mut lease = Some(lease);
    let old = generations.lock().expect("#1082 INVALID old generation")[0].clone();
    let flush = name.contains("flush");
    let disposing = name.starts_with("disposing") || name.starts_with("cancelled-release");
    let signal = Cancellation::default();
    if flush {
        capture(&old, "old-session").await;
    }
    let settled = Arc::new(AtomicBool::new(false));
    let (completed, mut completion) = tokio::sync::watch::channel(false);
    let pending = {
        let old = old.clone();
        let signal = signal.clone();
        let settled = settled.clone();
        tokio::spawn(async move {
            let result = if flush {
                let stats = old
                    .created
                    .service
                    .flush(FlushOptions::default())
                    .await
                    .expect("#1082 lifecycle old flush resumes");
                encode(&stats.to_value())
            } else {
                let result = old
                    .created
                    .provider
                    .retrieve(MemoryRetrieveInput {
                        query: "remember the blue deployment rule".into(),
                        session_id: "old-session".into(),
                        project_root: old
                            .created
                            .service
                            .workspace_dir
                            .to_string_lossy()
                            .into_owned(),
                        recent_messages: vec![],
                        signal,
                    })
                    .await;
                let result = encode(&result);
                json!({"diagnostics":result["diagnostics"],"route":result["metadata"]["debug"]["route"],"systemContext":result["systemContext"]})
            };
            settled.store(true, Ordering::SeqCst);
            let _ = completed.send(true);
            result
        })
    };
    llm.reached().await;
    registry.invalidate(None);
    let mut observed = json!({"leasedBefore":runtime.lease_count()});
    let mut releasing = None;
    if disposing {
        let mut held = lease.take().expect("#1082 INVALID last lease");
        releasing = Some(tokio::spawn(async move { held.release().await }));
        close.reached().await;
    }
    if name == "cancelled-retrieve-write" {
        signal.cancel(Some("fixture cancelled waiter".into()));
    }
    if name.starts_with("cancelled") {
        if name.starts_with("cancelled-release") {
            let waiter = releasing.take().expect("#1082 INVALID release waiter");
            waiter.abort();
            assert!(
                bounded(waiter)
                    .await
                    .expect_err("#1082 cancelled release waiter")
                    .is_cancelled(),
                "#1082 release waiter cancelled"
            );
        } else {
            // Like TS Promise.race, abandon an observer of the actual pending
            // operation, while retaining a completion observer for cleanup.
            let (entered, ready) = tokio::sync::oneshot::channel();
            let waiter = tokio::spawn(async move {
                let _ = entered.send(());
                completion.changed().await
            });
            bounded(ready)
                .await
                .expect("#1082 INVALID retrieval observer entry");
            waiter.abort();
            assert!(
                bounded(waiter)
                    .await
                    .expect_err("#1082 cancelled retrieve observer")
                    .is_cancelled(),
                "#1082 retrieve waiter cancelled"
            );
        }
        observed["waiterCancelled"] = json!(true);
    }
    observed["leasesAtSuccessor"] = json!(runtime.lease_count());
    observed["oldCloseBefore"] = json!(old.closes.load(Ordering::SeqCst));
    observed["predecessorPending"] = json!(!settled.load(Ordering::SeqCst));
    old.created
        .service
        .repository
        .storage
        .set_pipeline_state("workspaceDir", &JsValue::from("#1082-before-successor"))
        .expect("#1082 lifecycle workspace sentinel");
    registry
        .resolve(None)
        .expect("#1082 lifecycle successor stage");
    let next = generations.lock().expect("#1082 INVALID next generation")[1].clone();
    observed["sameDatabase"] = json!(old.created.service.db_path == next.created.service.db_path);
    observed["workspaceWritten"] = json!(
        next.created
            .service
            .repository
            .storage
            .get_pipeline_state("workspaceDir")
            .expect("#1082 lifecycle workspace row")
            == Some(JsValue::from(
                next.created
                    .service
                    .workspace_dir
                    .to_string_lossy()
                    .as_ref()
            ))
    );
    if !name.ends_with("-stage") {
        capture(&next, "new-session").await;
    }
    observed["newRows"] = json!(rows(&next, "new-session"));
    observed["predecessorPendingAfterSuccessor"] = json!(!settled.load(Ordering::SeqCst));
    llm.open();
    observed["operation"] = bounded(pending)
        .await
        .expect("#1082 lifecycle predecessor completion");
    observed["oldIndexed"] = json!(
        next.created
            .service
            .repository
            .storage
            .list_all_l0()
            .expect("#1082 lifecycle indexed rows")
            .iter()
            .filter(|r| r.session_key == "old-session")
            .map(|r| r.indexed)
            .collect::<Vec<_>>()
    );
    observed["oldRows"] = json!(rows(&next, "old-session"));
    if let Some(mut held) = lease.take() {
        releasing = Some(tokio::spawn(async move { held.release().await }));
        close.reached().await;
    }
    close.open();
    if let Some(releasing) = releasing {
        bounded(releasing)
            .await
            .expect("#1082 lifecycle release task")
            .expect("#1082 lifecycle release");
    }
    bounded(runtime.wait_disposed())
        .await
        .expect("#1082 lifecycle cleanup survives cancelled waiter");
    next.created
        .service
        .repository
        .storage
        .set_pipeline_state("after-old-close", &JsValue::from("alive"))
        .expect("#1082 lifecycle successor alive");
    observed["afterOldClose"] = json!(
        next.created
            .service
            .repository
            .storage
            .get_pipeline_state("after-old-close")
            .expect("#1082 lifecycle successor read")
            .and_then(|v| v
                .as_string()
                .map(|s| s.to_utf8().expect("#1082 lifecycle UTF8")))
    );
    observed["oldClosed"] = json!(
        old.created
            .service
            .repository
            .storage
            .get_pipeline_state("workspaceDir")
            .is_err()
    );
    observed["oldCloses"] = json!(old.closes.load(Ordering::SeqCst));
    bounded(registry.dispose())
        .await
        .expect("#1082 lifecycle registry cleanup");
    observed["newCloses"] = json!(next.closes.load(Ordering::SeqCst));
    let oracle: Value = serde_json::from_str(super::same_root_tests::SAME_ROOT_ORACLE)
        .expect("#1082 lifecycle oracle");
    assert_eq!(
        observed, oracle["lifecycle"][name],
        "#1082 lifecycle matrix {name}"
    );
}
macro_rules! case {
    ($test:ident,$name:literal) => {
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn $test() {
            matrix($name).await;
        }
    };
}
case!(staged_flush_stage, "staged-flush-stage");
case!(staged_flush_write, "staged-flush-write");
case!(staged_retrieve_stage, "staged-retrieve-stage");
case!(staged_retrieve_write, "staged-retrieve-write");
case!(disposing_flush_stage, "disposing-flush-stage");
case!(disposing_flush_write, "disposing-flush-write");
case!(disposing_retrieve_stage, "disposing-retrieve-stage");
case!(disposing_retrieve_write, "disposing-retrieve-write");
case!(never_flush_write, "never-flush-write");
case!(never_retrieve_write, "never-retrieve-write");
case!(cancelled_retrieve_write, "cancelled-retrieve-write");
case!(
    cancelled_release_flush_write,
    "cancelled-release-flush-write"
);
