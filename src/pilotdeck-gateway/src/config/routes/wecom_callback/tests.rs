use super::super::home_assistant::tests::Gateway;
use super::*;
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::Notify,
};
const KEY: &str = "YWJjZGVmZ2hpamtsbW5vcHFyc3R1dnd4eXowMTIzNDU";
struct Server {
    base: String,
    requests: Arc<StdMutex<Vec<(String, Value)>>>,
    changed: Arc<Notify>,
    invalid: Arc<AtomicBool>,
    task: JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Server {
    async fn wait(&self, predicate: impl Fn(&[(String, Value)]) -> bool) {
        tokio::time::timeout(Duration::from_secs(4), async {
            loop {
                let notified = self.changed.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if predicate(&self.requests.lock().unwrap()) {
                    return;
                }
                notified.await;
            }
        })
        .await
        .unwrap();
    }
}
async fn server() -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/cgi-bin", listener.local_addr().unwrap());
    let requests = Arc::new(StdMutex::new(vec![]));
    let recorded = requests.clone();
    let changed = Arc::new(Notify::new());
    let notify = changed.clone();
    let invalid = Arc::new(AtomicBool::new(false));
    let rejected = invalid.clone();
    let task = tokio::spawn(async move {
        let mut connections = JoinSet::new();
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let recorded = recorded.clone();
            let notify = notify.clone();
            let rejected = rejected.clone();
            connections.spawn(async move{
        let mut bytes=vec![];let mut buf=[0;4096];let(header,headers,path)=loop{let count=stream.read(&mut buf).await.unwrap();if count==0{return;}bytes.extend_from_slice(&buf[..count]);if let Some(end)=bytes.windows(4).position(|part|part==b"\r\n\r\n"){let headers=String::from_utf8_lossy(&bytes[..end]).into_owned();let path=headers.split_whitespace().nth(1).unwrap().to_owned();break(end+4,headers,path);}};
        let (kind,body,reply)=if path.starts_with("/cgi-bin/gettoken?"){assert_eq!(path,"/cgi-bin/gettoken?corpid=corp&corpsecret=secret");("token",Value::Null,json!({"access_token":"access token","expires_in":7200}))}else{
            assert_eq!(path,"/cgi-bin/message/send?access_token=access%20token");let length=headers.lines().find_map(|line|line.to_ascii_lowercase().strip_prefix("content-length:").and_then(|value|value.trim().parse::<usize>().ok())).unwrap();while bytes.len()<header+length{let count=stream.read(&mut buf).await.unwrap();if count==0{return;}bytes.extend_from_slice(&buf[..count]);}let body:Value=serde_json::from_slice(&bytes[header..header+length]).unwrap();("send",body,json!({"errcode":if rejected.load(Ordering::SeqCst){40014}else{0}}))};recorded.lock().unwrap().push((kind.into(),body));notify.notify_waiters();let reply=reply.to_string();let _=stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",reply.len()).as_bytes()).await;
    });
            while connections.try_join_next().is_some() {}
        }
    });
    Server {
        base,
        requests,
        changed,
        invalid,
        task,
    }
}
fn options(server: &Server) -> WeComCallbackOptions {
    WeComCallbackOptions {
        corp_id: " corp ".into(),
        agent_id: "42".into(),
        secret: "secret".into(),
        token: "callback".into(),
        encoding_aes_key: KEY.into(),
        host: "127.0.0.1".into(),
        port: 0,
        api_base: server.base.clone(),
    }
}
async fn request(
    handle: &WeComCallbackHandle,
    method: &str,
    path: &str,
    body: &str,
) -> (u16, String) {
    let mut stream = tokio::net::TcpStream::connect(handle.address)
        .await
        .unwrap();
    stream.write_all(format!("{method} {path} HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
    let mut bytes = vec![];
    stream.read_to_end(&mut bytes).await.unwrap();
    let reply = String::from_utf8(bytes).unwrap();
    (
        reply.split_whitespace().nth(1).unwrap().parse().unwrap(),
        reply.split_once("\r\n\r\n").unwrap().1.into(),
    )
}
fn callback(xml: &str) -> (String, String) {
    let encrypted = crypto::encrypt(KEY, xml, "corp");
    let signature = crypto::signature("callback", "123", "nonce", &encrypted);
    (
        format!("/any/path?msg_signature={signature}&timestamp=123&nonce=nonce"),
        format!("<xml><Encrypt><![CDATA[{encrypted}]]></Encrypt></xml>"),
    )
}
fn xml(text: &str) -> String {
    format!(
        "<xml><MsgType><![CDATA[text]]></MsgType><FromUserName><![CDATA[user]]></FromUserName><Content><![CDATA[{text}]]></Content></xml>"
    )
}
#[tokio::test]
async fn real_get_echo_and_encrypted_post_stream_reply_token_cache_and_cron() {
    let server = server().await;
    let gateway = Arc::new(Gateway::default());
    let channel = WeComCallbackChannel::new(options(&server), gateway.clone());
    let handle = channel.start().await.unwrap();
    let encrypted = crypto::encrypt(KEY, "echo text", "corp");
    let signature = crypto::signature("callback", "123", "nonce", &encrypted);
    let path = format!(
        "/?msg_signature={signature}&timestamp=123&nonce=nonce&echostr={}",
        http::encode(&encrypted)
    );
    assert_eq!(
        request(&handle, "GET", &path, "").await,
        (200, "echo text".into())
    );
    let (path, body) = callback(&xml("你好"));
    assert_eq!(
        request(&handle, "POST", &path, &body).await,
        (200, "success".into())
    );
    server
        .wait(|requests| {
            requests
                .iter()
                .any(|(kind, body)| kind == "send" && body["text"]["content"] == "reply 你好")
        })
        .await;
    assert_eq!(
        gateway.turns.lock().unwrap()[0],
        json!({"sessionKey":"wecom_callback:chat=user:general","channelKey":"wecom_callback","message":"你好"})
    );
    assert!(
        channel
            .deliver(&json!({"sessionKey":"wecom_callback:chat=user:general","text":"cron"}))
            .await
            .unwrap()
    );
    let requests = server.requests.lock().unwrap();
    assert_eq!(
        requests.iter().filter(|(kind, _)| kind == "token").count(),
        1
    );
    assert_eq!(
        requests.iter().find(|(kind, _)| kind == "send").unwrap().1["agentid"],
        42.0
    );
    drop(requests);
    handle.close().await;
}
#[tokio::test]
async fn malformed_signature_decryption_and_message_type_filters_keep_gateway_idle() {
    let server = server().await;
    let gateway = Arc::new(Gateway::default());
    let channel = WeComCallbackChannel::new(options(&server), gateway.clone());
    let handle = channel.start().await.unwrap();
    assert_eq!(request(&handle, "GET", "/", "").await.0, 400);
    assert_eq!(request(&handle, "POST", "/", "<xml/>").await.0, 400);
    assert_eq!(request(&handle, "PUT", "/", "").await.0, 405);
    let (path, body) = callback(&xml("signed"));
    assert_eq!(
        request(
            &handle,
            "POST",
            &path.replace("msg_signature=", "msg_signature=bad"),
            &body
        )
        .await
        .0,
        403
    );
    let encrypted = crypto::encrypt(KEY, &xml("wrong corp"), "other");
    let signature = crypto::signature("callback", "123", "nonce", &encrypted);
    assert_eq!(
        request(
            &handle,
            "POST",
            &format!("/?msg_signature={signature}&timestamp=123&nonce=nonce"),
            &format!("<Encrypt>{encrypted}</Encrypt>")
        )
        .await
        .0,
        500
    );
    channel.incoming_xml("<xml><MsgType>image</MsgType><FromUserName>user</FromUserName><Content>ignore</Content></xml>").await.unwrap();
    assert!(gateway.turns.lock().unwrap().is_empty());
    handle.close().await;
}
#[tokio::test]
async fn encrypted_permission_and_elicitation_answers_resume_live_gateway() {
    for (input, answer, method) in [
        ("permission", "2", "permission_decide"),
        ("question", "2", "elicitation_respond"),
    ] {
        let server = server().await;
        let gateway = Arc::new(Gateway::default());
        let channel = WeComCallbackChannel::new(options(&server), gateway.clone());
        let handle = channel.start().await.unwrap();
        let (path, body) = callback(&xml(input));
        request(&handle, "POST", &path, &body).await;
        gateway.ready.notified().await;
        let (path, body) = callback(&xml(answer));
        request(&handle, "POST", &path, &body).await;
        server
            .wait(|requests| {
                requests.iter().any(|(kind, body)| {
                    kind == "send" && body["text"]["content"] == format!("reply {input}")
                })
            })
            .await;
        assert_eq!(gateway.requests.lock().unwrap()[0].0, method);
        handle.close().await;
    }
}
#[tokio::test]
async fn invalid_access_token_invalidates_cache_outbound_truncates_and_stop_cancels() {
    let server = server().await;
    let gateway = Arc::new(Gateway::default());
    let channel = WeComCallbackChannel::new(options(&server), gateway.clone());
    let handle = channel.start().await.unwrap();
    assert!(channel.send_reply("user", &"x".repeat(2049)).await);
    server.invalid.store(true, Ordering::SeqCst);
    assert!(!channel.send_reply("user", "invalid").await);
    assert!(channel.access_token.lock().unwrap().token.is_none());
    server.invalid.store(false, Ordering::SeqCst);
    assert!(channel.send_reply("user", "renewed").await);
    assert_eq!(
        server
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(kind, _)| kind == "token")
            .count(),
        2
    );
    assert_eq!(
        server
            .requests
            .lock()
            .unwrap()
            .iter()
            .find(|(kind, _)| kind == "send")
            .unwrap()
            .1["text"]["content"]
            .as_str()
            .unwrap()
            .len(),
        2048
    );
    channel.incoming("user", "/new").await.unwrap();
    let (path, body) = callback(&xml("hold"));
    request(&handle, "POST", &path, &body).await;
    gateway.ready.notified().await;
    channel.incoming("user", "busy").await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), handle.close())
        .await
        .unwrap();
    assert!(gateway.cancelled.load(Ordering::SeqCst));
    assert!(
        !channel
            .deliver(&json!({"sessionKey":"wecom_callback:chat=user:general","text":"stopped"}))
            .await
            .unwrap()
    );
}
#[test]
fn aes_matches_node_crypto_known_ciphertext_and_xml_regex_contract() {
    assert_eq!(
        crypto::decrypt(KEY, "n+2yCHTHPE9sik87zYQO29j8aSAQgZ72tBgZLHp622c=", "corp").unwrap(),
        "hello"
    );
    assert_eq!(
        crypto::encrypt(KEY, "hello", "corp"),
        "n+2yCHTHPE9sik87zYQO29j8aSAQgZ72tBgZLHp622c="
    );
    assert!(crypto::decrypt("bad", "cipher", "corp").is_err());
    assert_eq!(
        xml_tag("<CONTENT><![CDATA[ a < b ]]></CONTENT>", "Content"),
        Some("a < b".into())
    );
    assert_eq!(
        xml_tag("<Content>a &amp; b</Content>", "Content"),
        Some("a &amp; b".into())
    );
}
#[tokio::test]
async fn missing_configuration_fails_before_listener() {
    assert!(
        WeComCallbackChannel::new(
            WeComCallbackOptions::default(),
            Arc::new(Gateway::default())
        )
        .start()
        .await
        .is_err()
    );
}
