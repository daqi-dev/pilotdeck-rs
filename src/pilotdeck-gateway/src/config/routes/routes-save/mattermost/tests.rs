use super::super::home_assistant::tests::{Gateway, Server, server};
use super::*;
fn options(server: &Server) -> MattermostOptions {
    MattermostOptions {
        server_url: server.base.clone(),
        token: "test token".into(),
        team_id: Some("team".into()),
    }
}
fn post(channel: &str, root: Option<&str>, text: &str) -> Value {
    json!({"event":"posted","data":{"post":json!({"user_id":"human","channel_id":channel,"root_id":root.unwrap_or(""),"message":text,"props":{}}).to_string()}})
}
async fn idle(channel: &MattermostChannel, chat: &str) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if !channel.chat(chat).lock().await.is_active() {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn real_ws_rest_auth_thread_session_and_post_filtering() {
    let server = server(true).await;
    let gateway = Arc::new(Gateway::default());
    let channel = MattermostChannel::new(options(&server), gateway.clone());
    let handle = channel.start().await.unwrap();
    for mut ignored in [
        post("channel", None, "self"),
        post("channel", None, "webhook"),
        post("channel", None, "bot"),
    ] {
        let mut inner: Value =
            serde_json::from_str(ignored["data"]["post"].as_str().unwrap()).unwrap();
        match inner["message"].as_str().unwrap() {
            "self" => inner["user_id"] = "bot".into(),
            "webhook" => inner["props"]["from_webhook"] = "true".into(),
            _ => inner["props"]["from_bot"] = "true".into(),
        }
        ignored["data"]["post"] = inner.to_string().into();
        server.incoming.send(ignored).unwrap();
    }
    server.incoming.send(json!({"event":"hello"})).unwrap();
    server
        .incoming
        .send(json!({"event":"posted","data":{"post":"{invalid"}}))
        .unwrap();
    server
        .incoming
        .send(post("channel", Some("thread"), "  你好\r\n世界  "))
        .unwrap();
    server
        .wait(|requests| {
            requests
                .iter()
                .any(|(kind, body)| kind == "post" && body["message"] == "reply 你好\n世界")
        })
        .await;
    idle(&channel, "channel:thread").await;
    assert_eq!(
        gateway.turns.lock().unwrap().as_slice(),
        &[
            json!({"sessionKey":"mattermost:chat=channel:thread:general","channelKey":"mattermost","message":"你好\n世界"})
        ]
    );
    assert_eq!(
        server.replies()[0],
        json!({"channel_id":"channel","root_id":"thread","message":"reply 你好\n世界"})
    );
    server.incoming.send(post("channel", None, "root")).unwrap();
    server
        .wait(|requests| {
            requests
                .iter()
                .any(|(kind, body)| kind == "post" && body["message"] == "reply root")
        })
        .await;
    assert_eq!(
        gateway.turns.lock().unwrap()[1]["sessionKey"],
        "mattermost:chat=channel:general"
    );
    assert!(server.replies()[1].get("root_id").is_none());
    handle.close().await;
}
#[tokio::test]
async fn real_ws_permission_and_question_answers_keep_thread_context() {
    for (input, answer, method) in [
        ("permission", "2", "permission_decide"),
        ("question", "0", "elicitation_respond"),
    ] {
        let server = server(true).await;
        let gateway = Arc::new(Gateway::default());
        let channel = MattermostChannel::new(options(&server), gateway.clone());
        let handle = channel.start().await.unwrap();
        server
            .incoming
            .send(post("channel", Some("thread"), input))
            .unwrap();
        gateway.ready.notified().await;
        server
            .incoming
            .send(post("channel", Some("thread"), answer))
            .unwrap();
        server
            .wait(|requests| {
                requests.iter().any(|(kind, body)| {
                    kind == "post" && body["message"] == format!("reply {input}")
                })
            })
            .await;
        let requests = gateway.requests.lock().unwrap();
        assert_eq!(requests[0].0, method);
        assert_eq!(
            requests[0].1["sessionKey"],
            "mattermost:chat=channel:thread:general"
        );
        if method == "permission_decide" {
            assert_eq!(requests[0].1["remember"], true);
        } else {
            assert_eq!(
                requests[0].1["answer"],
                json!({"type":"cancelled","reason":"user cancelled"})
            );
        }
        drop(requests);
        assert!(
            server
                .replies()
                .iter()
                .all(|reply| reply["root_id"] == "thread")
        );
        handle.close().await;
    }
}
#[tokio::test]
async fn new_session_busy_drop_stop_cancels_stream_and_chunked_rest_posts() {
    let server = server(true).await;
    let gateway = Arc::new(Gateway::default());
    let channel = MattermostChannel::new(options(&server), gateway.clone());
    let handle = channel.start().await.unwrap();
    channel.incoming("channel:thread", "/new").await.unwrap();
    server
        .incoming
        .send(post("channel", Some("thread"), "hold"))
        .unwrap();
    gateway.ready.notified().await;
    channel
        .incoming("channel:thread", "/new ignored")
        .await
        .unwrap();
    assert_eq!(gateway.turns.lock().unwrap().len(), 1);
    assert!(
        gateway.turns.lock().unwrap()[0]["sessionKey"]
            .as_str()
            .unwrap()
            .starts_with("mattermost:chat=channel:thread:s_")
    );
    assert!(
        channel
            .send_reply("channel:other", &"x".repeat(16384))
            .await
    );
    let chunks = server
        .replies()
        .into_iter()
        .filter(|value| value["root_id"] == "other")
        .collect::<Vec<_>>();
    assert_eq!(chunks.len(), 2);
    assert_eq!(chunks[0]["message"].as_str().unwrap().len(), 16383);
    assert_eq!(chunks[1]["message"], "x");
    tokio::time::timeout(Duration::from_secs(3), handle.close())
        .await
        .unwrap();
    assert!(gateway.cancelled.load(Ordering::SeqCst));
    assert!(!channel.send_reply("channel", "after stop").await);
}
#[test]
fn ws_url_origin_and_chunk_boundaries_match_ts() {
    assert_eq!(
        websocket_url("https://example.test/base/", "a +/字").unwrap(),
        (
            "wss://example.test/base/api/v4/websocket?token=a%20%2B%2F%E5%AD%97".into(),
            "https://example.test".into()
        )
    );
    assert_eq!(chunk_text("abc\ndef ghi", 6), vec!["abc", "def", " ghi"]);
    assert_eq!(chunk_text("aaa\nbbbbb", 7), vec!["aaa\nbbb", "bb"]);
}

#[tokio::test]
async fn websocket_close_reconnects_and_authenticates_again_with_same_token() {
    let server = server(true).await;
    let gateway = Arc::new(Gateway::default());
    let channel = MattermostChannel::new(options(&server), gateway.clone());
    let handle = channel.start().await.unwrap();
    server.incoming.send(json!({"type":"__close"})).unwrap();
    tokio::time::timeout(Duration::from_secs(7), async {
        loop {
            if server
                .requests
                .lock()
                .unwrap()
                .iter()
                .filter(|(kind, _)| kind == "ws")
                .count()
                == 2
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    server
        .incoming
        .send(post("channel", None, "after reconnect"))
        .unwrap();
    server
        .wait(|requests| {
            requests
                .iter()
                .any(|(kind, body)| kind == "post" && body["message"] == "reply after reconnect")
        })
        .await;
    handle.close().await;
}
#[tokio::test]
async fn missing_token_or_server_fails_before_http_request() {
    let channel =
        MattermostChannel::new(MattermostOptions::default(), Arc::new(Gateway::default()));
    assert!(channel.start().await.is_err());
    assert!(!channel.running.load(Ordering::SeqCst));
}
