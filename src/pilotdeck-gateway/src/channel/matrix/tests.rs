use super::*;
use std::{collections::VecDeque, future::Future, pin::Pin};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
struct Stream(VecDeque<JsValue>);
impl TurnEventStream for Stream {
    fn next(
        &mut self,
    ) -> Pin<Box<dyn Future<Output = Result<Option<JsValue>, ChannelError>> + Send + '_>> {
        Box::pin(async move { Ok(self.0.pop_front()) })
    }
}
#[derive(Default)]
struct Gateway(Mutex<Vec<TurnRequest>>);
impl TurnGateway for Gateway {
    fn submit(
        &self,
        input: TurnRequest,
    ) -> BoxFuture<Result<Box<dyn TurnEventStream>, ChannelError>> {
        self.0.lock().unwrap().push(input);
        Box::pin(async {
            Ok(Box::new(Stream(VecDeque::from([parse_json(
                r#"{"type":"assistant_text_delta","text":"matrix reply"}"#,
            )
            .unwrap()]))) as Box<dyn TurnEventStream>)
        })
    }
}
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("pdrs-matrix-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct Mock {
    root: String,
    calls: Arc<Mutex<Vec<(String, String, JsValue)>>>,
    task: JoinHandle<()>,
}
impl Drop for Mock {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Mock {
    async fn new(sync: JsValue) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let root = format!("http://{}", listener.local_addr().unwrap());
        let calls = Arc::new(Mutex::new(Vec::new()));
        let record = calls.clone();
        let first = Arc::new(Mutex::new(Some(sync)));
        let task = tokio::spawn(async move {
            let mut tasks = JoinSet::new();
            loop {
                tokio::select! {
                    accepted=listener.accept()=> {let(mut socket,_)=accepted.unwrap();let calls=record.clone();let first=first.clone();tasks.spawn(async move {
                        let mut bytes=Vec::new();while !bytes.ends_with(b"\r\n\r\n") {let mut byte=[0];if socket.read_exact(&mut byte).await.is_err(){return;}bytes.push(byte[0]);assert!(bytes.len()<16384);}
                        let headers=String::from_utf8(bytes).unwrap();assert!(headers.to_ascii_lowercase().contains("authorization: bearer fixture"));let mut parts=headers.lines().next().unwrap().split_whitespace();let method=parts.next().unwrap().to_owned();let path=parts.next().unwrap().to_owned();
                        let len:usize=headers.lines().find_map(|l|l.to_ascii_lowercase().strip_prefix("content-length:").map(|s|s.trim().parse().unwrap())).unwrap_or(0);let mut body=vec![0;len];socket.read_exact(&mut body).await.unwrap();let body=parse_json(&String::from_utf8(body).unwrap()).unwrap();calls.lock().unwrap().push((method,path.clone(),body));
                        let body=if path.contains("/account/whoami") {parse_json(r#"{"user_id":"@bot:fixture"}"#).unwrap()}else if path.contains("/sync?") {let value=first.lock().unwrap().take();tokio::time::sleep(Duration::from_millis(10)).await;value.unwrap_or_else(||parse_json(r#"{"next_batch":"batch2","rooms":{}}"#).unwrap())}else {object([])};
                        let body=stringify_json(&body).unwrap().unwrap();let response=format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",body.len(),body);let _=socket.write_all(response.as_bytes()).await;
                    });},
                    Some(_)=tasks.join_next(),if !tasks.is_empty()=>{},
                }
            }
        });
        Self { root, calls, task }
    }
    async fn until(&self, predicate: impl Fn(&[(String, String, JsValue)]) -> bool) {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if predicate(&self.calls.lock().unwrap()) {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("Matrix wire observed");
    }
}
#[test]
fn mapper_is_room_scoped() {
    let mut mapper = MatrixSessionMapper::default();
    assert_eq!(
        mapper
            .resolve(&"!room:server".into(), &" text ".into())
            .session_key,
        JsString::from("matrix:chat=!room:server:general")
    );
    assert_eq!(encode_path(&"!room:server".into()), "%21room%3Aserver");
}
#[tokio::test]
async fn sync_invite_filter_reply_and_token_survive_restart() {
    let scratch = Scratch::new();
    let storage = scratch.0.join("matrix.json");
    std::fs::write(&storage, r#"{"syncToken":"old","otherSdkState":7}"#).unwrap();
    let sync=parse_json(r#"{"next_batch":"batch1","rooms":{"invite":{"!invite:fixture":{}},"join":{"!room:fixture":{"timeline":{"events":[{"type":"m.room.message","sender":"@bot:fixture","content":{"body":"self"}},{"type":"m.room.message","sender":"@human:fixture","content":{"body":"notice","msgtype":"m.notice"}},{"type":"m.room.message","sender":"@human:fixture","content":{"body":"edit","m.relates_to":{"rel_type":"m.replace"}}},{"type":"m.room.message","sender":"@human:fixture","content":{"body":" hello ","msgtype":"m.text"}}]}}}}}"#).unwrap();
    let mock = Mock::new(sync).await;
    let gateway = Arc::new(Gateway::default());
    let channel = MatrixChannel::new(MatrixOptions {
        access_token: Some("fixture".into()),
        homeserver: Some(mock.root.clone()),
        storage_path: Some(storage.clone()),
        ..Default::default()
    });
    let handle = channel
        .start(ChannelStartDeps {
            turn_gateway: Some(gateway.clone()),
            ..Default::default()
        })
        .await
        .unwrap();
    mock.until(|calls| {
        calls.iter().any(|(_, path, body)| {
            path.contains("/send/m.room.message/")
                && body.get("body") == Some(&JsValue::from("matrix reply"))
        })
    })
    .await;
    handle.stop("fixture").await.unwrap();
    assert!(channel.inner.active.lock().unwrap().is_empty());
    assert!(!channel.inner.running.load(Ordering::Acquire));
    {
        let requests = gateway.0.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].channel_key, ChannelKey::new("matrix"));
        assert_eq!(
            requests[0].session_key,
            JsString::from("matrix:chat=!room:fixture:general")
        );
        assert_eq!(requests[0].message, JsString::from("hello"));
        let golden = parse_json(include_str!("fixtures/oracle-diagnostic.json")).unwrap();
        assert_eq!(
            golden.get("baseline"),
            Some(&JsValue::from("cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85"))
        );
        let Some(JsValue::Array(expected)) = golden.get("requests") else {
            panic!("fixed TS Matrix requests")
        };
        assert_eq!(expected.len(), 1);
        assert_eq!(
            expected[0].get("sessionKey"),
            Some(&JsValue::String(requests[0].session_key.clone()))
        );
        assert_eq!(
            expected[0].get("channelKey"),
            Some(&JsValue::String(requests[0].channel_key.as_js().clone()))
        );
        assert_eq!(
            expected[0].get("message"),
            Some(&JsValue::String(requests[0].message.clone()))
        );
    }
    {
        let calls = mock.calls.lock().unwrap();
        assert!(calls.iter().any(
            |(method, path, _)| method == "POST" && path.ends_with("/join/%21invite%3Afixture")
        ));
        assert!(calls.iter().any(|(_, path, _)| path.contains("since=old")));
    }
    let state = parse_json(&std::fs::read_to_string(&storage).unwrap()).unwrap();
    assert_eq!(state.get("otherSdkState"), Some(&JsValue::Number(7.0)));
    let token = state
        .get("syncToken")
        .and_then(JsValue::as_string)
        .unwrap()
        .clone();
    let restarted = channel.start(ChannelStartDeps::default()).await.unwrap();
    mock.until(|calls| {
        calls.iter().any(|(_, path, _)| {
            path.contains(&format!(
                "since={}",
                String::from_utf16_lossy(token.units())
            ))
        })
    })
    .await;
    handle.stop("old handle retry").await.unwrap();
    channel
        .accept_room_event(
            "!room:fixture".into(),
            parse_json(r#"{"sender":"@human:fixture","content":{"body":"/new"}}"#).unwrap(),
        )
        .await
        .unwrap();
    restarted.stop("fixture").await.unwrap();
}
#[tokio::test]
async fn native_storage_refuses_special_file_without_blocking() {
    let scratch = Scratch::new();
    let path = scratch.0.join("fifo");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&path)
            .status()
            .unwrap()
            .success()
    );
    tokio::time::timeout(Duration::from_secs(1), async {
        let path = path.clone();
        assert!(
            tokio::task::spawn_blocking(move || read_storage(&path))
                .await
                .unwrap()
                .is_err()
        );
    })
    .await
    .unwrap();
}
