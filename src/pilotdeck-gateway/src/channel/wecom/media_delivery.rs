//! Outbound media uses the same correlated channel connection as text.
use super::{
    delivery,
    media::{self, MediaRequestPort, PreparedMedia},
    transport::ActiveTransport,
    wire,
};
use crate::channel::protocol::{BoxFuture, ChannelError};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{sync::Arc, time::Duration};

pub(crate) struct RequestPort(pub Arc<ActiveTransport>);
impl MediaRequestPort for RequestPort {
    fn request(&self, cmd: JsString, body: JsValue) -> BoxFuture<Result<JsValue, ChannelError>> {
        let transport = self.0.clone();
        Box::pin(async move {
            let name = String::from_utf16_lossy(cmd.units());
            let request_id: JsString = format!("{}-{}", name, uuid::Uuid::new_v4().simple()).into();
            let mut headers = JsObject::new();
            headers.insert("req_id".into(), JsValue::String(request_id.clone()));
            let mut frame = JsObject::new();
            frame.insert("cmd".into(), JsValue::String(cmd));
            frame.insert("headers".into(), JsValue::Object(headers));
            frame.insert("body".into(), body);
            transport
                .request(request_id, JsValue::Object(frame), Duration::from_secs(15))
                .await
                .map_err(ChannelError::error)
        })
    }
}
pub(crate) async fn send_prepared(
    transport: Arc<ActiveTransport>,
    reply_id: &delivery::ReplyTarget,
    prepared: &PreparedMedia,
) -> bool {
    if let Some(reason) = &prepared.reject_reason {
        delivery::reply(&transport, reply_id.clone(), reason.as_str().into()).await;
        return false;
    }
    let Ok(upload) = media::upload_media(&RequestPort(transport.clone()), prepared).await else {
        return false;
    };
    let Some(media_id) = upload
        .get("media_id")
        .or_else(|| upload.get("mediaId"))
        .and_then(JsValue::as_string)
        .cloned()
    else {
        return false;
    };
    if media_id.units().is_empty() {
        return false;
    }
    let mut media = JsObject::new();
    media.insert("media_id".into(), JsValue::String(media_id));
    let mut body = JsObject::new();
    body.insert(
        "msgtype".into(),
        JsValue::String(prepared.final_type.as_str().into()),
    );
    body.insert(prepared.final_type.as_str().into(), JsValue::Object(media));
    let (request_id, cmd) = match reply_id {
        delivery::ReplyTarget::Callback(id) => (id.clone(), "aibot_respond_msg"),
        delivery::ReplyTarget::Direct(chat) => {
            body.insert("chatid".into(), JsValue::String(chat.clone()));
            (
                format!("aibot_send_msg-{}", uuid::Uuid::new_v4().simple()).into(),
                "aibot_send_msg",
            )
        }
        delivery::ReplyTarget::Unavailable => return false,
    };
    let mut headers = JsObject::new();
    headers.insert("req_id".into(), JsValue::String(request_id.clone()));
    let mut frame = JsObject::new();
    frame.insert("cmd".into(), cmd.into());
    frame.insert("headers".into(), JsValue::Object(headers));
    frame.insert("body".into(), JsValue::Object(body));
    let Ok(response) = transport
        .request(request_id, JsValue::Object(frame), Duration::from_secs(15))
        .await
    else {
        return false;
    };
    if !matches!(wire::response_error(&response), Ok(None)) {
        return false;
    }
    if let Some(note) = &prepared.downgrade_note {
        delivery::reply(&transport, reply_id.clone(), note.as_str().into()).await;
    }
    true
}
