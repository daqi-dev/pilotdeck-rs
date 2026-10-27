use super::super::runner::tail::{SessionTail, TailConfig};
use super::*;
use std::{fs, path::PathBuf};
struct Handler;
impl HostHandler for Handler {
    fn module_call(&self, _: JsValue) -> HostFuture<JsObject> {
        Box::pin(async { Ok(JsObject::new()) })
    }
    fn event(&self, _: JsValue) -> HostFuture<()> {
        Box::pin(async { Ok(()) })
    }
}
async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(5), future)
        .await
        .expect("#742 live tail bounded")
}
fn options(turn: &str) -> JsValue {
    crate::metadata::object([
        ("sessionId", "session".into()),
        ("turnId", turn.into()),
        ("runId", "run".into()),
        ("operationId", "op".into()),
        ("requestId", "request".into()),
    ])
}
fn input() -> JsValue {
    crate::metadata::object([("type", "text".into()), ("text", "request".into())])
}
fn disk(path: &std::path::Path) -> String {
    fs::read_to_string(path).expect("#742 live tail disk")
}
async fn event(turn: &mut dyn LiveTurn) -> JsValue {
    bounded(turn.next())
        .await
        .expect("#742 live tail next")
        .expect("#742 live tail event")
}
fn kind(event: &JsValue, name: &str) {
    assert_eq!(
        event.get("type"),
        Some(&name.into()),
        "#742 live tail event kind"
    );
}
fn setup() -> (
    LiveSessionAdapter,
    Arc<SessionTail>,
    Arc<tokio::sync::Semaphore>,
    Cancellation,
    PathBuf,
) {
    let recorder = test_recorder();
    let root = recorder
        .writer()
        .path()
        .parent()
        .expect("#742 live tail root")
        .to_path_buf();
    let cwd = root.join("workspace");
    fs::create_dir_all(&cwd).expect("#742 live tail directory");
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let entered = Cancellation::default();
    let tail = SessionTail::new(
        recorder.clone(),
        "session".into(),
        TailConfig {
            cwd: Some(cwd.clone()),
            suggestion_generator: Some(Arc::new({
                let gate = gate.clone();
                let entered = entered.clone();
                move |input| {
                    assert_eq!(
                        input.user_prompt,
                        JsString::from("request"),
                        "#742 live tail original prompt"
                    );
                    assert_eq!(
                        input.assistant_response,
                        JsString::from("done"),
                        "#742 live tail assistant response"
                    );
                    let gate = gate.clone();
                    let entered = entered.clone();
                    Box::pin(async move {
                        entered.cancel(None);
                        let permit = gate.acquire().await.expect("#742 live tail release");
                        permit.forget();
                        Ok(Some("follow up".into()))
                    })
                }
            })),
            ..TailConfig::default()
        },
    );
    let session = LiveSessionAdapter::new(recorder, Vec::new, move |_| {
        let cwd = cwd.clone();
        Box::pin(async move {
            let mut command = tokio::process::Command::new("python3");
            command
                .arg(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/tests/fixtures/live-session/peer.py"
                ))
                .arg("live-tail")
                .arg(cwd);
            let client = Box::pin(async move {
                HostClient::spawn(
                    &mut command,
                    Some(&"fixture".into()),
                    crate::host::Limits {
                        execution_timeout: Some(Duration::from_secs(5)),
                        ..Default::default()
                    },
                )
                .await
            }) as HostFuture<HostClient>;
            Ok((
                client,
                Arc::new(Handler) as Arc<dyn HostHandler>,
                test_lease(),
            ))
        })
    })
    .with_tail(tail.clone());
    (session, tail, gate, entered, root)
}
#[tokio::test]
async fn live_tail_progress_and_drop() {
    bounded(async{
        let (session,tail,gate,entered,root)=setup();let path=root.join("session.jsonl");
        let mut turn=session.submit(input(),options("turn")).expect("#742 live tail submit");
        for name in ["turn_started","input_accepted","user_prompt_submitted","tool_result","file_artifacts"]{kind(&event(turn.as_mut()).await,name);}
        assert!(!disk(&path).contains("\"type\":\"turn_result\""),"#742 live tail artifact boundary");
        {
            let next=turn.next();tokio::pin!(next);
            tokio::select!{biased;out=&mut next=>panic!("#742 live tail suggestion must hold terminal {:?}",out.is_ok()),_=entered.cancelled()=>{}}
            assert!(disk(&path).contains("\"type\":\"turn_result\""),"#742 live tail durable result");
            assert!(!disk(&path).contains("isSnapshot"),"#742 live tail snapshot before suggestion");
            gate.add_permits(1);
            kind(&bounded(next).await.expect("#742 live tail suggestion result").expect("#742 live tail suggestion event"),"prompt_suggestion");
        }
        assert!(!disk(&path).contains("isSnapshot"),"#742 live tail suggestion boundary");
        kind(&event(turn.as_mut()).await,"turn_completed");assert!(disk(&path).contains("isSnapshot"),"#742 live tail snapshot at terminal");
        assert!(bounded(turn.next()).await.expect("#742 live tail closed").is_none(),"#742 live tail single terminal");drop(turn);
        // A second write differs, so this turn also yields an artifact before
        // completion. Drop the stream at that yield, then admit a successor.
        fs::write(root.join("workspace/report.txt"),"old").expect("#742 live tail change baseline");
        let mut turn=session.submit(input(),options("turn-2")).expect("#742 live tail second submit");
        for name in ["turn_started","input_accepted","user_prompt_submitted","tool_result","file_artifacts"]{kind(&event(turn.as_mut()).await,name);}
        drop(turn);
        let turn=session.submit(input(),options("turn-3")).expect("#742 live tail dropped progress releases slot");drop(turn);
        bounded(session.dispose()).await.expect("#742 live tail dispose");drop(tail);fs::remove_dir_all(root).expect("#742 live tail cleanup");
    }).await;
}

fn test_lease() -> Arc<RuntimeLease> {
    use crate::runtime::project::{ProjectRuntimeRegistry, ResourceBundle};
    let registry = ProjectRuntimeRegistry::new(
        std::env::temp_dir(),
        Arc::new(|_: &std::path::Path, _: &mut ResourceBundle| Ok(())),
    );
    Arc::new(registry.acquire(None).expect("#742 live tail lease"))
}
