//! Feishu transport for the existing full shared IM live-reply controller.
use super::{Runtime, client::SendResult, render};
use crate::channel::weixin::live_reply::{
    LiveReplyController, LiveReplyOptions, LiveReplyTransport, SendOutcome,
};
use pilotdeck_protocol::{JsObject, JsValue};
use std::sync::{Arc, Mutex};
#[derive(Clone)]
pub(super) struct Handle {
    message_id: String,
    live_post: bool,
}
pub(super) type Controller = LiveReplyController<Arc<Mutex<Handle>>>;
pub(super) fn controller(
    runtime: Arc<Runtime>,
    chat: String,
    generation: u64,
    mut options: LiveReplyOptions,
) -> Controller {
    let send_runtime = runtime.clone();
    let send_chat = chat.clone();
    let send_cursor = options.cursor.clone();
    let edit_runtime = runtime.clone();
    let edit_chat = chat.clone();
    let edit_cursor = options.cursor.clone();
    let logger = runtime.logger.fn_handle();
    options.on_transport_error = Some(Arc::new(move |error, phase| {
        logger(
            "warn",
            &format!("feishu: live reply {phase} failed: {error}"),
        )
    }));
    let transport = LiveReplyTransport {
        send: Arc::new(move |text| {
            let runtime = send_runtime.clone();
            let chat = send_chat.clone();
            let cursor = send_cursor.clone();
            Box::pin(async move {
                if !runtime.is_current(&chat, generation) {
                    return Ok(SendOutcome::Failed);
                }
                let Some(api) = runtime.api() else {
                    return Ok(SendOutcome::Failed);
                };
                let final_ = !text.ends_with(&cursor);
                let result = api
                    .send_post(
                        &chat,
                        &text,
                        final_,
                        Some(render::activity_kind_from_text(&text)),
                    )
                    .await;
                if let SendResult::Ok(Some(id)) = result {
                    return Ok(SendOutcome::Handle(Arc::new(Mutex::new(Handle {
                        message_id: id,
                        live_post: true,
                    }))));
                }
                let mut content = JsObject::new();
                content.insert("text".into(), JsValue::from(text.as_str()));
                match api.send_raw(&chat, "text", JsValue::Object(content)).await {
                    SendResult::Ok(Some(id)) => {
                        Ok(SendOutcome::Handle(Arc::new(Mutex::new(Handle {
                            message_id: id,
                            live_post: false,
                        }))))
                    }
                    SendResult::Ok(None) => Ok(SendOutcome::NoHandle),
                    SendResult::Failed => Ok(SendOutcome::Failed),
                }
            })
        }),
        edit: Some(Arc::new(move |handle: Arc<Mutex<Handle>>, text| {
            let runtime = edit_runtime.clone();
            let chat = edit_chat.clone();
            let cursor = edit_cursor.clone();
            Box::pin(async move {
                if !runtime.is_current(&chat, generation) {
                    return Ok(false);
                }
                let Some(api) = runtime.api() else {
                    return Ok(false);
                };
                let h = handle.lock().expect("feishu live handle").clone();
                if h.live_post {
                    let content = render::render_live_post(
                        &text,
                        !text.ends_with(&cursor),
                        Some(render::activity_kind_from_text(&text)),
                    );
                    if api
                        .edit_message(&h.message_id, "post", content, "update post")
                        .await
                    {
                        return Ok(true);
                    }
                    handle.lock().expect("feishu live handle").live_post = false;
                }
                let mut content = JsObject::new();
                content.insert("text".into(), JsValue::from(text.as_str()));
                Ok(api
                    .edit_message(
                        &h.message_id,
                        "text",
                        JsValue::Object(content),
                        "update message",
                    )
                    .await)
            })
        })),
        delete: None,
        clear: None,
        pulse_activity: None,
        stop_activity: None,
        max_message_length: Some(render::MAX_TEXT_MESSAGE_LENGTH),
        format_text: None,
    };
    LiveReplyController::new(transport, options)
}
