//! Feishu consumes the shared full live controller, compared with actual fixed TS API traffic.
use super::*;
use crate::channel::weixin::live_reply::LiveReplyOptions;
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
fn gold() -> Value {
    serde_json::from_str(include_str!("live-oracle.json")).unwrap()
}
struct Events(std::collections::VecDeque<JsValue>);
impl crate::channel::protocol::TurnEventStream for Events {
    fn next(
        &mut self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Option<JsValue>, ChannelError>> + Send + '_>,
    > {
        Box::pin(async move { Ok(self.0.pop_front()) })
    }
}
struct Gateway;
impl TurnGateway for Gateway {
    fn submit_with_options(
        &self,
        input: TurnRequest,
        options: ChannelTurnOptions,
    ) -> BoxFuture<Result<Box<dyn crate::channel::protocol::TurnEventStream>, ChannelError>> {
        assert_eq!(options.allow_plan_mode_tools, Some(false));
        assert_eq!(options.timeout_ms, Some(0.0));
        self.submit(input)
    }
    fn submit(
        &self,
        _input: TurnRequest,
    ) -> BoxFuture<Result<Box<dyn crate::channel::protocol::TurnEventStream>, ChannelError>> {
        let events = [
            r#"{"type":"turn_started","runId":"run-fixture"}"#,
            r#"{"type":"assistant_text_delta","text":"first"}"#,
            r#"{"type":"assistant_text_delta","text":" second"}"#,
            r#"{"type":"tool_call_started","name":"read_file"}"#,
            r#"{"type":"tool_call_finished","ok":true}"#,
            r#"{"type":"assistant_text_delta","text":"tail"}"#,
        ]
        .into_iter()
        .map(|s| pilotdeck_protocol::parse_json(s).unwrap())
        .collect();
        Box::pin(async move {
            Ok(Box::new(Events(events)) as Box<dyn crate::channel::protocol::TurnEventStream>)
        })
    }
}
fn normalize(mut row: Value) -> Value {
    if let Some(body) = row.get_mut("body")
        && let Some(content) = body.get("content").and_then(Value::as_str)
    {
        body["content"] = serde_json::from_str(content).unwrap();
    }
    row
}
#[tokio::test]
async fn live_post_create_edit_segment_and_fallback_match_actual_ts() {
    let gold = gold();
    let wire = Arc::new(Mutex::new(Vec::<Value>::new()));
    let current = Arc::new(Mutex::new(String::new()));
    let fail = Arc::new(AtomicBool::new(false));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let (record, case, failure) = (wire.clone(), current.clone(), fail.clone());
    let server = tokio::spawn(async move {
        let mut tasks = tokio::task::JoinSet::new();
        loop {
            tokio::select! {accepted=listener.accept()=>{let(mut socket,_)=accepted.unwrap();let(record,case,failure)=(record.clone(),case.clone(),failure.clone());tasks.spawn(async move{
            let mut headers=vec![];while !headers.ends_with(b"\r\n\r\n"){let mut byte=[0];if socket.read_exact(&mut byte).await.is_err(){return;}headers.push(byte[0]);}
            let headers=String::from_utf8(headers).unwrap();let mut words=headers.lines().next().unwrap().split_whitespace();let method=words.next().unwrap().to_owned();let path=words.next().unwrap().to_owned();
            let len=headers.lines().find_map(|s|s.to_lowercase().strip_prefix("content-length:").map(|s|s.trim().parse::<usize>().unwrap())).unwrap_or(0);let mut bytes=vec![0;len];socket.read_exact(&mut bytes).await.unwrap();let body:Value=serde_json::from_slice(&bytes).unwrap();
            let mut row=json!({"case":case.lock().unwrap().clone(),"path":path,"method":method,"body":body});
            if let Some(auth)=headers.lines().find_map(|s|s.strip_prefix("authorization: ").or_else(||s.strip_prefix("Authorization: "))){row["authorization"]=auth.into();}
            let index={let mut rows=record.lock().unwrap();rows.push(row.clone());rows.len()};
            let value=if path.contains("tenant_access_token"){json!({"code":0,"tenant_access_token":"mock-token","expire":7200})}else if method=="PUT"&&body["msg_type"]=="post"&&failure.swap(false,Ordering::SeqCst){json!({"code":400,"msg":"mock post edit refused"})}else{json!({"code":0,"data":{"message_id":format!("sent-message-{index}")}})};
            let body=value.to_string();let reply=format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len());let _=socket.write_all(reply.as_bytes()).await;
            });},Some(_)=tasks.join_next(),if !tasks.is_empty()=>{}}
        }
    });
    for case in gold["cases"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        *current.lock().unwrap() = name.into();
        fail.store(name == "post-edit-fallback", Ordering::SeqCst);
        let start = wire.lock().unwrap().len();
        let channel = FeishuChannel::new(
            FeishuOptions {
                app_id: "cli_0123456789abcdef".into(),
                app_secret: "mock-secret".into(),
                connection_mode: "webhook".into(),
                base_url: origin.clone(),
                ..Default::default()
            },
            Arc::new(super::super::im_interaction::NoInteraction),
        )
        .with_live_reply_options(LiveReplyOptions {
            initial_throttle_ms: 0,
            initial_buffer_threshold: 1,
            throttle_ms: 0,
            buffer_threshold: 1,
            activity_delay_ms: 100000,
            turn_timeout_ms: 0,
            ..Default::default()
        });
        let handle = channel
            .start(ChannelStartDeps {
                turn_gateway: Some(Arc::new(Gateway)),
                ..Default::default()
            })
            .await
            .unwrap();
        channel
            .runtime
            .handle_webhook(
                &json!({"chatId":"room","text":"live input","eventId":name}).to_string(),
            )
            .await;
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let count = wire.lock().unwrap().len() - start;
                let active = channel.runtime.active_chats.lock().unwrap().len();
                if count >= case["wire"].as_array().unwrap().len() && active == 0 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        let actual = wire.lock().unwrap()[start..]
            .iter()
            .cloned()
            .map(normalize)
            .collect::<Vec<_>>();
        let expected = case["wire"]
            .as_array()
            .unwrap()
            .iter()
            .cloned()
            .map(normalize)
            .collect::<Vec<_>>();
        assert_eq!(actual, expected, "actual TS live traffic {name}");
        handle.stop("fixture").await.unwrap();
    }
    server.abort();
    let _ = server.await;
}
