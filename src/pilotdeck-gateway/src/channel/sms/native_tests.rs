use super::*;
use crate::channel::protocol::ChannelAdapter;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
fn gold() -> serde_json::Value {
    serde_json::from_str(include_str!("native-oracle.json")).unwrap()
}
#[tokio::test]
async fn native_rest_matches_fixed_ts_sdk_inputs_and_official_http_contract() {
    let gold = gold();
    let case = gold["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["kind"] == "configured")
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (tx, rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut raw = vec![];
        while !raw.ends_with(b"\r\n\r\n") {
            let mut b = [0];
            socket.read_exact(&mut b).await.unwrap();
            raw.push(b[0]);
        }
        let head = String::from_utf8(raw).unwrap();
        let size = head
            .lines()
            .find_map(|l| {
                l.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .map(|s| s.trim().parse::<usize>().unwrap())
            })
            .unwrap();
        let mut body = vec![0; size];
        socket.read_exact(&mut body).await.unwrap();
        let _ = tx.send((head, String::from_utf8(body).unwrap()));
        let body = r#"{"sid":"SMmock"}"#;
        socket.write_all(format!("HTTP/1.1 201 Created\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
    });
    let factory = sender::native_twilio_factory_with_base(base);
    let sdk = &case["sdk"][0];
    let client = factory(
        &sdk["sid"].as_str().unwrap().into(),
        &sdk["token"].as_str().unwrap().into(),
    )
    .unwrap();
    let msg = &case["sent"][0];
    client
        .send(SmsOutgoingMessage {
            body: msg["body"].as_str().unwrap().into(),
            from: msg["from"].as_str().unwrap().into(),
            to: msg["to"].as_str().unwrap().into(),
        })
        .await
        .unwrap();
    let (head, body) = rx.await.unwrap();
    assert!(head.starts_with(
        "POST /2010-04-01/Accounts/AC00000000000000000000000000000000/Messages.json HTTP/1.1"
    ));
    use base64::Engine;
    let expected = base64::engine::general_purpose::STANDARD
        .encode("AC00000000000000000000000000000000:mock-token");
    let authorization = head
        .lines()
        .find_map(|line| {
            line.split_once(':')
                .filter(|(name, _)| name.eq_ignore_ascii_case("authorization"))
                .map(|(_, value)| value.trim())
        })
        .unwrap();
    assert_eq!(authorization, format!("Basic {expected}"));
    let form: std::collections::BTreeMap<_, _> = url::form_urlencoded::parse(body.as_bytes())
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    assert_eq!(form.len(), 3);
    assert_eq!(form["Body"], msg["body"]);
    assert_eq!(form["From"], msg["from"]);
    assert_eq!(form["To"], msg["to"]);
    server.await.unwrap();
}
#[tokio::test]
async fn native_rest_propagates_remote_error_and_rejects_empty_credentials() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let factory = sender::native_twilio_factory_with_base(format!(
        "http://{}",
        listener.local_addr().unwrap()
    ));
    assert!(factory(&"".into(), &"token".into()).is_err());
    assert!(factory(&"ACfixture".into(), &"".into()).is_err());
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut b = [0; 8192];
        let _ = socket.read(&mut b).await.unwrap();
        let body = r#"{"code":21610,"message":"mock recipient unsubscribed"}"#;
        socket.write_all(format!("HTTP/1.1 400 Bad Request\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
    });
    let error = factory(&"ACfixture".into(), &"token".into())
        .unwrap()
        .send(SmsOutgoingMessage {
            body: "x".into(),
            from: "+1".into(),
            to: "+2".into(),
        })
        .await
        .unwrap_err();
    assert_eq!(error.message(), "mock recipient unsubscribed");
    server.await.unwrap();
}
#[tokio::test]
async fn explicit_optional_sdk_absence_preserves_fixed_ts_noop_branch() {
    let logs = Arc::new(Mutex::new(Vec::new()));
    let captured = logs.clone();
    let channel = SmsChannel::new(SmsChannelOptions {
        extra: None,
        mapper: None,
        gateway: None,
        env: None,
        twilio_factory: None,
        record_unhandled: None,
        request_timeout: None,
    });
    let handle = channel
        .start(crate::channel::protocol::ChannelStartDeps {
            log: Some(Arc::new(move |level, message| {
                captured
                    .lock()
                    .unwrap()
                    .push((level.to_owned(), message.to_owned()))
            })),
            ..Default::default()
        })
        .await
        .unwrap();
    handle.stop("fixture").await.unwrap();
    let expected = gold();
    let absent = expected["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["kind"] == "sdk-absent")
        .unwrap();
    assert_eq!(
        logs.lock().unwrap()[0].1,
        absent["logs"][0][1].as_str().unwrap()
    );
}
