//! Summary-only projection. Audio, references and assistant media stay intact.
use super::super::{content, is_block, is_role, replace_content, text_block};
use pilotdeck_protocol::JsValue;

pub fn strip_multimedia_from_messages(messages: &[JsValue]) -> Vec<JsValue> {
    messages
        .iter()
        .map(|message| {
            if !is_role(message, "user") {
                return message.clone();
            }
            replace_content(
                message,
                content(message)
                    .iter()
                    .map(|block| {
                        if is_block(block, "tool_result") {
                            replace_content(block, content(block).iter().map(strip).collect())
                        } else {
                            strip(block)
                        }
                    })
                    .collect(),
            )
        })
        .collect()
}

fn strip(block: &JsValue) -> JsValue {
    if is_block(block, "image") {
        text_block("[image]".into())
    } else if is_block(block, "pdf") {
        text_block("[document]".into())
    } else {
        block.clone()
    }
}
