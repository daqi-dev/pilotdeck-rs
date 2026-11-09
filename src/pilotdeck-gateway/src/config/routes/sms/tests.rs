use super::super::home_assistant::tests::Gateway;
use super::*;
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::Notify,
};
struct Twilio {
    base: String,
    sent: Arc<StdMutex<Vec<HashMap<String, String>>>>,
    changed: Arc<Notify>,
    task: JoinHandle<()>,
}
impl Drop for Twilio {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Twilio {
    async fn wait(&self, predicate: impl Fn(&[HashMap<String, String>]) -> bool) {
        tokio::time::timeout(Duration::from_secs(4), async {
            loop {
                let notified = self.changed.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if predicate(&self.sent.lock().unwrap()) {
                    return;
                }
                notified.await;
            }
        })
        .await
        .unwrap();
    }
}
async fn twilio() -> Twilio {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let sent = Arc::new(StdMutex::new(vec![]));
    let recorded = sent.clone();
    let changed = Arc::new(Notify::new());
    let notify = changed.clone();
    let task = tokio::spawn(async move {
        let mut connections = JoinSet::new();
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let recorded = recorded.clone();
            let notify = notify.clone();
            connections.spawn(async move {
                let mut bytes = vec![];
                let mut buf = [0; 4096];
                let (header, headers) = loop {
                    let count = stream.read(&mut buf).await.unwrap();
                    if count == 0 {
                        return;
                    }
                    bytes.extend_from_slice(&buf[..count]);
                    if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                        break (end + 4, String::from_utf8_lossy(&bytes[..end]).into_owned());
                    }
                };
                assert!(headers.starts_with("POST /Accounts/ACtest/Messages.json HTTP/1.1"));
                assert!(
                    headers.to_ascii_lowercase().contains(
                        "authorization: basic QUN0ZXN0OnRlc3QgdG9rZW4="
                            .to_ascii_lowercase()
                            .as_str()
                    )
                );
                assert!(
                    headers
                        .to_ascii_lowercase()
                        .contains("content-type: application/x-www-form-urlencoded")
                );
                let length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|value| value.trim().parse::<usize>().ok())
                    })
                    .unwrap();
                while bytes.len() < header + length {
                    let count = stream.read(&mut buf).await.unwrap();
                    if count == 0 {
                        return;
                    }
                    bytes.extend_from_slice(&buf[..count]);
                }
                let params = url::form_urlencoded::parse(&bytes[header..header + length])
                    .map(|(k, v)| (k.into_owned(), v.into_owned()))
                    .collect::<HashMap<_, _>>();
                recorded.lock().unwrap().push(params);
                notify.notify_waiters();
                let _ = stream
                    .write_all(
                        b"HTTP/1.1 201 Created\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                    )
                    .await;
            });
            while connections.try_join_next().is_some() {}
        }
    });
    Twilio {
        base,
        sent,
        changed,
        task,
    }
}
fn options(server: &Twilio) -> SmsOptions {
    SmsOptions {
        account_sid: "ACtest".into(),
        auth_token: "test token".into(),
        phone_number: "+100".into(),
        webhook_port: 0,
        api_base: server.base.clone(),
        public_url: "https://public.test/root/".into(),
        ..Default::default()
    }
}
async fn send(
    handle: &SmsHandle,
    path: &str,
    content_type: &str,
    body: &str,
    sig: Option<&str>,
) -> (u16, String) {
    let mut stream = tokio::net::TcpStream::connect(handle.address)
        .await
        .unwrap();
    let signature = sig
        .map(|sig| format!("X-Twilio-Signature: {sig}\r\n"))
        .unwrap_or_default();
    stream.write_all(format!("POST {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n{signature}\r\n{body}",body.len()).as_bytes()).await.unwrap();
    let mut reply = Vec::new();
    stream.read_to_end(&mut reply).await.unwrap();
    let reply = String::from_utf8(reply).unwrap();
    let status = reply.split_whitespace().nth(1).unwrap().parse().unwrap();
    (status, reply.split_once("\r\n\r\n").unwrap().1.into())
}
fn form(from: &str, text: &str) -> String {
    url::form_urlencoded::Serializer::new(String::new())
        .append_pair("Body", text)
        .append_pair("From", from)
        .finish()
}
#[tokio::test]
async fn form_webhook_ack_then_stream_reply_uses_twilio_basic_auth_and_cron() {
    let twilio = twilio().await;
    let gateway = Arc::new(Gateway::default());
    let channel = SmsChannel::new(options(&twilio), gateway.clone());
    let handle = channel.start().await.unwrap();
    assert_eq!(send(&handle, "/other", "text/plain", "", None).await.0, 404);
    assert_eq!(
        send(
            &handle,
            "/sms/incoming?x=1",
            "application/x-www-form-urlencoded",
            &form("+200", "你好"),
            None
        )
        .await,
        (200, TWIML_OK.into())
    );
    twilio
        .wait(|sent| {
            sent.iter()
                .any(|sms| sms.get("Body").is_some_and(|body| body == "reply 你好"))
        })
        .await;
    assert_eq!(
        gateway.turns.lock().unwrap()[0],
        json!({"sessionKey":"sms:chat=+200:general","channelKey":"sms","message":"你好"})
    );
    {
        let sent = twilio.sent.lock().unwrap();
        assert_eq!(sent[0]["From"], "+100");
        assert_eq!(sent[0]["To"], "+200");
    }
    assert!(
        channel
            .deliver(&json!({"sessionKey":"sms:chat=+200:general","text":"cron"}))
            .await
            .unwrap()
    );
    assert!(!channel.deliver(&json!({"originChannelKey":"signal","sessionKey":"sms:chat=+200:general","text":"wrong"})).await.unwrap());
    handle.close().await;
    assert!(
        !channel
            .deliver(&json!({"sessionKey":"sms:chat=+200:general","text":"stopped"}))
            .await
            .unwrap()
    );
}
#[tokio::test]
async fn json_and_form_signature_validation_match_sorted_twilio_contract() {
    let twilio = twilio().await;
    let gateway = Arc::new(Gateway::default());
    let channel = SmsChannel::new(options(&twilio), gateway.clone());
    let handle = channel.start().await.unwrap();
    let params = HashMap::from([
        ("From".into(), "+200".into()),
        ("Body".into(), "signed".into()),
    ]);
    let signature = http::signature(
        "https://public.test/root/sms/incoming",
        &params,
        "test token",
    );
    assert_eq!(
        send(
            &handle,
            "/sms/incoming",
            "application/json",
            r#"{"From":"+200","Body":"signed"}"#,
            Some("bad")
        )
        .await
        .0,
        401
    );
    assert_eq!(
        send(
            &handle,
            "/sms/incoming",
            "application/json",
            "{broken",
            None
        )
        .await
        .0,
        400
    );
    assert_eq!(
        send(
            &handle,
            "/sms/incoming",
            "application/json",
            r#"{"From":"+200","Body":"signed"}"#,
            Some(&signature)
        )
        .await
        .0,
        200
    );
    twilio
        .wait(|sent| {
            sent.iter()
                .any(|sms| sms.get("Body").is_some_and(|body| body == "reply signed"))
        })
        .await;
    assert_eq!(gateway.turns.lock().unwrap().len(), 1);
    handle.close().await;
}
#[tokio::test]
async fn permission_and_elicitation_reply_resume_same_stream() {
    for (input, answer, method) in [
        ("permission", "1", "permission_decide"),
        ("question", "2", "elicitation_respond"),
    ] {
        let twilio = twilio().await;
        let gateway = Arc::new(Gateway::default());
        let channel = SmsChannel::new(options(&twilio), gateway.clone());
        let handle = channel.start().await.unwrap();
        assert_eq!(
            send(
                &handle,
                "/sms/incoming",
                "application/x-www-form-urlencoded",
                &form("+200", input),
                None
            )
            .await
            .0,
            200
        );
        gateway.ready.notified().await;
        assert_eq!(
            send(
                &handle,
                "/sms/incoming",
                "application/x-www-form-urlencoded",
                &form("+200", answer),
                None
            )
            .await
            .0,
            200
        );
        twilio
            .wait(|sent| {
                sent.iter().any(|sms| {
                    sms.get("Body")
                        .is_some_and(|body| body == &format!("reply {input}"))
                })
            })
            .await;
        let requests = gateway.requests.lock().unwrap();
        assert_eq!(requests[0].0, method);
        if method == "permission_decide" {
            assert_eq!(requests[0].1["remember"], false);
        } else {
            assert_eq!(
                requests[0].1["answer"],
                json!({"type":"answered","answers":{"Pick":"B"}})
            );
        }
        drop(requests);
        handle.close().await;
    }
}
#[tokio::test]
async fn new_session_busy_drop_and_stop_abort_background_turn() {
    let twilio = twilio().await;
    let gateway = Arc::new(Gateway::default());
    let channel = SmsChannel::new(options(&twilio), gateway.clone());
    let handle = channel.start().await.unwrap();
    channel.incoming("+200", "/new").await.unwrap();
    send(
        &handle,
        "/sms/incoming",
        "application/x-www-form-urlencoded",
        &form("+200", "hold"),
        None,
    )
    .await;
    gateway.ready.notified().await;
    channel.incoming("+200", "/new ignored").await.unwrap();
    assert_eq!(gateway.turns.lock().unwrap().len(), 1);
    assert!(
        gateway.turns.lock().unwrap()[0]["sessionKey"]
            .as_str()
            .unwrap()
            .starts_with("sms:chat=+200:s_")
    );
    tokio::time::timeout(Duration::from_secs(3), handle.close())
        .await
        .unwrap();
    assert!(gateway.cancelled.load(Ordering::SeqCst));
}
#[tokio::test]
async fn body_limit_and_empty_config_reject_without_turn() {
    let gateway = Arc::new(Gateway::default());
    assert!(
        SmsChannel::new(SmsOptions::default(), gateway.clone())
            .start()
            .await
            .is_err()
    );
    let twilio = twilio().await;
    let channel = SmsChannel::new(options(&twilio), gateway.clone());
    let handle = channel.start().await.unwrap();
    assert_eq!(
        send(
            &handle,
            "/sms/incoming",
            "text/plain",
            &"x".repeat(1_048_577),
            None
        )
        .await
        .0,
        400
    );
    assert!(gateway.turns.lock().unwrap().is_empty());
    handle.close().await;
}
#[test]
fn signature_matches_independent_hmac_sha1_fixture() {
    let params = HashMap::from([
        ("CallSid".into(), "CA1234567890ABCDE".into()),
        ("Caller".into(), "+14158675310".into()),
        ("Digits".into(), "1234".into()),
        ("From".into(), "+14158675310".into()),
        ("To".into(), "+18005551212".into()),
    ]);
    assert_eq!(
        http::signature(
            "https://mycompany.com/myapp.php?foo=1&bar=2",
            &params,
            "12345"
        ),
        "GvWf1cFY/Q7PnoempGyD5oXAezc="
    );
}
