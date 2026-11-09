use super::*;
use crate::channel::protocol::{
    ChannelStatusPort, ChannelStatusSnapshot, ChannelTurnOptions, ChannelUpdatePort,
    ChannelUpdateResult, TurnEventStream, TurnGateway, TurnRequest,
};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;
struct Gateway;
struct Events(bool);
impl TurnEventStream for Events {
    fn next(
        &mut self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Option<JsValue>, ChannelError>> + Send + '_>,
    > {
        Box::pin(async move {
            if self.0 {
                return Ok(None);
            }
            self.0 = true;
            Ok(Some(
                pilotdeck_protocol::parse_json(
                    r#"{"type":"assistant_text_delta","text":"answer"}"#,
                )
                .unwrap(),
            ))
        })
    }
}
impl TurnGateway for Gateway {
    fn submit(&self, _: TurnRequest) -> BoxFuture<Result<Box<dyn TurnEventStream>, ChannelError>> {
        Box::pin(async { Err(ChannelError::error("missing options")) })
    }
    fn submit_with_options(
        &self,
        request: TurnRequest,
        options: ChannelTurnOptions,
    ) -> BoxFuture<Result<Box<dyn TurnEventStream>, ChannelError>> {
        assert_eq!(
            request.session_key,
            JsString::from("wecom:dm=alice:general")
        );
        assert_eq!(request.message, JsString::from("hello"));
        assert_eq!(options.allow_plan_mode_tools, Some(false));
        Box::pin(async { Ok(Box::new(Events(false)) as Box<dyn TurnEventStream>) })
    }
}
struct Host;
impl ChannelStatusPort for Host {
    fn read(&self) -> BoxFuture<Result<ChannelStatusSnapshot, ChannelError>> {
        Box::pin(async { Err(ChannelError::error("unused")) })
    }
}
impl ChannelUpdatePort for Host {
    fn run(&self, _: bool) -> BoxFuture<Result<ChannelUpdateResult, ChannelError>> {
        Box::pin(async { Err(ChannelError::error("unused")) })
    }
}
#[tokio::test]
async fn websocket_callback_reaches_shared_turn_and_replies() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut peer = tokio_tungstenite::accept_async(socket).await.unwrap();
        let subscribe = peer.next().await.unwrap().unwrap().into_text().unwrap();
        let value = pilotdeck_protocol::parse_json(subscribe.as_str()).unwrap();
        assert_eq!(
            value.get("cmd").and_then(JsValue::as_string),
            Some(&JsString::from("aibot_subscribe"))
        );
        let id = String::from_utf16_lossy(
            value
                .get("headers")
                .unwrap()
                .get("req_id")
                .unwrap()
                .as_string()
                .unwrap()
                .units(),
        );
        peer.send(Message::Text(
            format!(r#"{{"headers":{{"req_id":"{id}"}},"body":{{"errcode":0}}}}"#).into(),
        ))
        .await
        .unwrap();
        peer.send(Message::Text(r#"{"cmd":"aibot_msg_callback","headers":{"req_id":"r-1"},"body":{"msgid":"m-1","from":{"userid":"alice"},"chatid":"alice","chattype":"single","msgtype":"text","text":{"content":"hello"}}}"#.into())).await.unwrap();
        let reply = tokio::time::timeout(std::time::Duration::from_secs(3), peer.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .into_text()
            .unwrap();
        let value = pilotdeck_protocol::parse_json(reply.as_str()).unwrap();
        assert_eq!(
            value.get("cmd").and_then(JsValue::as_string),
            Some(&JsString::from("aibot_respond_msg"))
        );
        assert_eq!(
            value
                .get("body")
                .and_then(|v| v.get("markdown"))
                .and_then(|v| v.get("content"))
                .and_then(JsValue::as_string),
            Some(&JsString::from("answer"))
        );
        peer.send(Message::Text(
            r#"{"headers":{"req_id":"r-1"},"body":{"errcode":0}}"#.into(),
        ))
        .await
        .unwrap();
    });
    let options=pilotdeck_protocol::parse_json(&format!(r#"{{"botKey":"bot","extra":{{"botSecret":"secret","dmPolicy":"open","websocketUrl":"ws://{address}"}}}}"#)).unwrap();
    let host = Arc::new(Host);
    let channel = WeComChannel::new(
        &options,
        Arc::new(super::super::im_interaction::NoInteraction),
        None,
        None,
        host.clone(),
        host,
        None,
        None,
    );
    let handle = channel
        .start(ChannelStartDeps {
            turn_gateway: Some(Arc::new(Gateway)),
            ..Default::default()
        })
        .await
        .unwrap();
    server.await.unwrap();
    handle.stop("test").await.unwrap();
}

#[tokio::test]
async fn missing_callback_id_sends_proactive_dm() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut peer = tokio_tungstenite::accept_async(socket).await.unwrap();
        let subscribe = peer.next().await.unwrap().unwrap().into_text().unwrap();
        let value = pilotdeck_protocol::parse_json(subscribe.as_str()).unwrap();
        assert_eq!(
            value.get("cmd").and_then(JsValue::as_string),
            Some(&JsString::from("aibot_subscribe"))
        );
        let id = String::from_utf16_lossy(
            value
                .get("headers")
                .unwrap()
                .get("req_id")
                .unwrap()
                .as_string()
                .unwrap()
                .units(),
        );
        peer.send(Message::Text(
            format!(r#"{{"headers":{{"req_id":"{id}"}},"body":{{"errcode":0}}}}"#).into(),
        ))
        .await
        .unwrap();
        peer.send(Message::Text(r#"{"cmd":"aibot_msg_callback","body":{"msgid":"m-1","from":{"userid":"alice"},"chatid":"alice","chattype":"single","msgtype":"text","text":{"content":"hello"}}}"#.into())).await.unwrap();
        let reply = tokio::time::timeout(std::time::Duration::from_secs(3), peer.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .into_text()
            .unwrap();
        let value = pilotdeck_protocol::parse_json(reply.as_str()).unwrap();
        assert_eq!(
            value.get("cmd").and_then(JsValue::as_string),
            Some(&JsString::from("aibot_send_msg"))
        );
        assert_eq!(
            value
                .get("body")
                .and_then(|v| v.get("markdown"))
                .and_then(|v| v.get("content"))
                .and_then(JsValue::as_string),
            Some(&JsString::from("answer"))
        );
        let reply_id = String::from_utf16_lossy(
            value
                .get("headers")
                .unwrap()
                .get("req_id")
                .unwrap()
                .as_string()
                .unwrap()
                .units(),
        );
        peer.send(Message::Text(
            format!(r#"{{"headers":{{"req_id":"{reply_id}"}},"body":{{"errcode":0}}}}"#).into(),
        ))
        .await
        .unwrap();
    });
    let options=pilotdeck_protocol::parse_json(&format!(r#"{{"botKey":"bot","extra":{{"botSecret":"secret","dmPolicy":"open","websocketUrl":"ws://{address}"}}}}"#)).unwrap();
    let host = Arc::new(Host);
    let channel = WeComChannel::new(
        &options,
        Arc::new(super::super::im_interaction::NoInteraction),
        None,
        None,
        host.clone(),
        host,
        None,
        None,
    );
    let handle = channel
        .start(ChannelStartDeps {
            turn_gateway: Some(Arc::new(Gateway)),
            ..Default::default()
        })
        .await
        .unwrap();
    server.await.unwrap();
    handle.stop("test").await.unwrap();
}

#[tokio::test]
async fn reconnect_reauthenticates_and_delivers_a_turn() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        for attempt in 0..2 {
            let (socket, _) = listener.accept().await.unwrap();
            let mut peer = tokio_tungstenite::accept_async(socket).await.unwrap();
            let subscribe = peer.next().await.unwrap().unwrap().into_text().unwrap();
            let value = pilotdeck_protocol::parse_json(subscribe.as_str()).unwrap();
            let id = String::from_utf16_lossy(
                value
                    .get("headers")
                    .unwrap()
                    .get("req_id")
                    .unwrap()
                    .as_string()
                    .unwrap()
                    .units(),
            );
            peer.send(Message::Text(
                format!(r#"{{"headers":{{"req_id":"{id}"}},"body":{{"errcode":0}}}}"#).into(),
            ))
            .await
            .unwrap();
            if attempt == 0 {
                peer.close(None).await.unwrap();
                continue;
            }
            peer.send(Message::Text(r#"{"cmd":"aibot_msg_callback","headers":{"req_id":"r-2"},"body":{"msgid":"m-2","from":{"userid":"alice"},"chatid":"alice","chattype":"single","msgtype":"text","text":{"content":"hello"}}}"#.into())).await.unwrap();
            let reply = tokio::time::timeout(std::time::Duration::from_secs(4), peer.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap()
                .into_text()
                .unwrap();
            let value = pilotdeck_protocol::parse_json(reply.as_str()).unwrap();
            assert_eq!(
                value
                    .get("body")
                    .and_then(|v| v.get("markdown"))
                    .and_then(|v| v.get("content"))
                    .and_then(JsValue::as_string),
                Some(&JsString::from("answer"))
            );
            peer.send(Message::Text(
                r#"{"headers":{"req_id":"r-2"},"body":{"errcode":0}}"#.into(),
            ))
            .await
            .unwrap();
        }
    });
    let options=pilotdeck_protocol::parse_json(&format!(r#"{{"botKey":"bot","extra":{{"botSecret":"secret","dmPolicy":"open","websocketUrl":"ws://{address}","textBatchDelayMs":0}}}}"#)).unwrap();
    let host = Arc::new(Host);
    let channel = WeComChannel::new(
        &options,
        Arc::new(super::super::im_interaction::NoInteraction),
        None,
        None,
        host.clone(),
        host,
        None,
        None,
    );
    let handle = channel
        .start(ChannelStartDeps {
            turn_gateway: Some(Arc::new(Gateway)),
            ..Default::default()
        })
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(8), server)
        .await
        .unwrap()
        .unwrap();
    handle.stop("test").await.unwrap();
}

struct BatchGateway;
impl TurnGateway for BatchGateway {
    fn submit(&self, _: TurnRequest) -> BoxFuture<Result<Box<dyn TurnEventStream>, ChannelError>> {
        Box::pin(async { Err(ChannelError::error("missing options")) })
    }
    fn submit_with_options(
        &self,
        request: TurnRequest,
        _: ChannelTurnOptions,
    ) -> BoxFuture<Result<Box<dyn TurnEventStream>, ChannelError>> {
        assert_eq!(request.message, JsString::from("hello\nworld"));
        Box::pin(async { Ok(Box::new(Events(false)) as Box<dyn TurnEventStream>) })
    }
}
#[tokio::test]
async fn consecutive_text_callbacks_make_one_turn_and_reply_to_latest() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut peer = tokio_tungstenite::accept_async(socket).await.unwrap();
        let subscribe = peer.next().await.unwrap().unwrap().into_text().unwrap();
        let value = pilotdeck_protocol::parse_json(subscribe.as_str()).unwrap();
        let id = String::from_utf16_lossy(
            value
                .get("headers")
                .unwrap()
                .get("req_id")
                .unwrap()
                .as_string()
                .unwrap()
                .units(),
        );
        peer.send(Message::Text(
            format!(r#"{{"headers":{{"req_id":"{id}"}},"body":{{"errcode":0}}}}"#).into(),
        ))
        .await
        .unwrap();
        for (index, message) in ["hello", "world"].iter().enumerate() {
            peer.send(Message::Text(format!(r#"{{"cmd":"aibot_msg_callback","headers":{{"req_id":"r-{index}"}},"body":{{"msgid":"m-{index}","from":{{"userid":"alice"}},"chatid":"alice","chattype":"single","msgtype":"text","text":{{"content":"{message}"}}}}}}"#).into())).await.unwrap();
        }
        let reply = tokio::time::timeout(std::time::Duration::from_secs(3), peer.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .into_text()
            .unwrap();
        let value = pilotdeck_protocol::parse_json(reply.as_str()).unwrap();
        assert_eq!(
            value
                .get("headers")
                .unwrap()
                .get("req_id")
                .and_then(JsValue::as_string),
            Some(&JsString::from("r-1"))
        );
        peer.send(Message::Text(
            r#"{"headers":{"req_id":"r-1"},"body":{"errcode":0}}"#.into(),
        ))
        .await
        .unwrap();
    });
    let options=pilotdeck_protocol::parse_json(&format!(r#"{{"botKey":"bot","extra":{{"botSecret":"secret","dmPolicy":"open","websocketUrl":"ws://{address}","textBatchDelayMs":30}}}}"#)).unwrap();
    let host = Arc::new(Host);
    let channel = WeComChannel::new(
        &options,
        Arc::new(super::super::im_interaction::NoInteraction),
        None,
        None,
        host.clone(),
        host,
        None,
        None,
    );
    let handle = channel
        .start(ChannelStartDeps {
            turn_gateway: Some(Arc::new(BatchGateway)),
            ..Default::default()
        })
        .await
        .unwrap();
    server.await.unwrap();
    handle.stop("test").await.unwrap();
}

struct MediaGateway;
impl TurnGateway for MediaGateway {
    fn submit(&self, _: TurnRequest) -> BoxFuture<Result<Box<dyn TurnEventStream>, ChannelError>> {
        Box::pin(async { Err(ChannelError::error("missing options")) })
    }
    fn submit_with_options(
        &self,
        request: TurnRequest,
        options: ChannelTurnOptions,
    ) -> BoxFuture<Result<Box<dyn TurnEventStream>, ChannelError>> {
        assert_eq!(request.message, JsString::from("用户发送了企业微信附件。"));
        assert_eq!(options.attachments.len(), 1);
        let attachment = &options.attachments[0];
        assert_eq!(
            attachment.get("type").and_then(JsValue::as_string),
            Some(&JsString::from("file"))
        );
        let path = String::from_utf16_lossy(
            attachment
                .get("path")
                .and_then(JsValue::as_string)
                .unwrap()
                .units(),
        );
        Box::pin(async move {
            assert_eq!(tokio::fs::read(&path).await.unwrap(), b"hello");
            tokio::fs::remove_file(path).await.unwrap();
            Ok(Box::new(Events(false)) as Box<dyn TurnEventStream>)
        })
    }
}
#[tokio::test]
async fn inbound_attachment_is_cached_and_reaches_the_shared_turn() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut peer = tokio_tungstenite::accept_async(socket).await.unwrap();
        let subscribe = peer.next().await.unwrap().unwrap().into_text().unwrap();
        let value = pilotdeck_protocol::parse_json(subscribe.as_str()).unwrap();
        let id = String::from_utf16_lossy(
            value
                .get("headers")
                .unwrap()
                .get("req_id")
                .unwrap()
                .as_string()
                .unwrap()
                .units(),
        );
        peer.send(Message::Text(
            format!(r#"{{"headers":{{"req_id":"{id}"}},"body":{{"errcode":0}}}}"#).into(),
        ))
        .await
        .unwrap();
        peer.send(Message::Text(r#"{"cmd":"aibot_msg_callback","headers":{"req_id":"r-media"},"body":{"msgid":"m-media","from":{"userid":"alice"},"chatid":"alice","chattype":"single","msgtype":"file","file":{"base64":"aGVsbG8=","filename":"note.txt"}}}"#.into())).await.unwrap();
        let reply = tokio::time::timeout(std::time::Duration::from_secs(4), peer.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .into_text()
            .unwrap();
        let value = pilotdeck_protocol::parse_json(reply.as_str()).unwrap();
        assert_eq!(
            value
                .get("body")
                .and_then(|v| v.get("markdown"))
                .and_then(|v| v.get("content"))
                .and_then(JsValue::as_string),
            Some(&JsString::from("answer"))
        );
        peer.send(Message::Text(
            r#"{"headers":{"req_id":"r-media"},"body":{"errcode":0}}"#.into(),
        ))
        .await
        .unwrap();
    });
    let options=pilotdeck_protocol::parse_json(&format!(r#"{{"botKey":"bot","extra":{{"botSecret":"secret","dmPolicy":"open","websocketUrl":"ws://{address}"}}}}"#)).unwrap();
    let host = Arc::new(Host);
    let channel = WeComChannel::new(
        &options,
        Arc::new(super::super::im_interaction::NoInteraction),
        None,
        None,
        host.clone(),
        host,
        None,
        None,
    );
    let handle = channel
        .start(ChannelStartDeps {
            turn_gateway: Some(Arc::new(MediaGateway)),
            ..Default::default()
        })
        .await
        .unwrap();
    server.await.unwrap();
    handle.stop("test").await.unwrap();
}

struct ImageGateway;
struct ImageEvents(usize);
impl TurnEventStream for ImageEvents {
    fn next(
        &mut self,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Option<JsValue>, ChannelError>> + Send + '_>,
    > {
        Box::pin(async move {
            let source = match self.0 {
                0 => Some(
                    r#"{"type":"tool_call_finished","ok":true,"toolCallId":"t1","images":[{"mimeType":"image/png","data":"aGVsbG8="}]}"#,
                ),
                1 => Some(r#"{"type":"assistant_text_delta","text":"answer"}"#),
                _ => None,
            };
            self.0 += 1;
            Ok(source.map(|s| pilotdeck_protocol::parse_json(s).unwrap()))
        })
    }
}
impl TurnGateway for ImageGateway {
    fn submit(&self, _: TurnRequest) -> BoxFuture<Result<Box<dyn TurnEventStream>, ChannelError>> {
        Box::pin(async { Err(ChannelError::error("missing options")) })
    }
    fn submit_with_options(
        &self,
        _: TurnRequest,
        _: ChannelTurnOptions,
    ) -> BoxFuture<Result<Box<dyn TurnEventStream>, ChannelError>> {
        Box::pin(async { Ok(Box::new(ImageEvents(0)) as Box<dyn TurnEventStream>) })
    }
}
#[tokio::test]
async fn tool_image_uploads_over_same_websocket_before_final_text() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut peer = tokio_tungstenite::accept_async(socket).await.unwrap();
        let subscribe = peer.next().await.unwrap().unwrap().into_text().unwrap();
        let value = pilotdeck_protocol::parse_json(subscribe.as_str()).unwrap();
        let id = String::from_utf16_lossy(
            value
                .get("headers")
                .unwrap()
                .get("req_id")
                .unwrap()
                .as_string()
                .unwrap()
                .units(),
        );
        peer.send(Message::Text(
            format!(r#"{{"headers":{{"req_id":"{id}"}},"body":{{"errcode":0}}}}"#).into(),
        ))
        .await
        .unwrap();
        peer.send(Message::Text(r#"{"cmd":"aibot_msg_callback","headers":{"req_id":"r-image"},"body":{"msgid":"m-image","from":{"userid":"alice"},"chatid":"alice","chattype":"single","msgtype":"text","text":{"content":"hello"}}}"#.into())).await.unwrap();
        for (expected, payload) in [
            (
                "aibot_upload_media_init",
                r#"{"errcode":0,"upload_id":"u1"}"#,
            ),
            ("aibot_upload_media_chunk", r#"{"errcode":0}"#),
            (
                "aibot_upload_media_finish",
                r#"{"errcode":0,"media_id":"media1"}"#,
            ),
            ("aibot_respond_msg", r#"{"errcode":0}"#),
            ("aibot_respond_msg", r#"{"errcode":0}"#),
        ] {
            let frame = tokio::time::timeout(std::time::Duration::from_secs(4), peer.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap()
                .into_text()
                .unwrap();
            let value = pilotdeck_protocol::parse_json(frame.as_str()).unwrap();
            assert_eq!(
                value.get("cmd").and_then(JsValue::as_string),
                Some(&JsString::from(expected))
            );
            let id = String::from_utf16_lossy(
                value
                    .get("headers")
                    .unwrap()
                    .get("req_id")
                    .unwrap()
                    .as_string()
                    .unwrap()
                    .units(),
            );
            if expected == "aibot_upload_media_chunk" {
                assert_eq!(
                    value
                        .get("body")
                        .and_then(|v| v.get("base64_data"))
                        .and_then(JsValue::as_string),
                    Some(&JsString::from("aGVsbG8="))
                );
            }
            peer.send(Message::Text(
                format!(r#"{{"headers":{{"req_id":"{id}"}},"body":{payload}}}"#).into(),
            ))
            .await
            .unwrap();
        }
    });
    let options=pilotdeck_protocol::parse_json(&format!(r#"{{"botKey":"bot","extra":{{"botSecret":"secret","dmPolicy":"open","websocketUrl":"ws://{address}","textBatchDelayMs":0}}}}"#)).unwrap();
    let host = Arc::new(Host);
    let channel = WeComChannel::new(
        &options,
        Arc::new(super::super::im_interaction::NoInteraction),
        None,
        None,
        host.clone(),
        host,
        None,
        None,
    );
    let handle = channel
        .start(ChannelStartDeps {
            turn_gateway: Some(Arc::new(ImageGateway)),
            ..Default::default()
        })
        .await
        .unwrap();
    server.await.unwrap();
    handle.stop("test").await.unwrap();
}
