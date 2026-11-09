//! Inbound message parsing (`WeixinChannel.ts:560-628`, `weixin-ilink` `MessageItemType`).
use serde_json::{Value, json};

const WEIXIN_IMAGE_MISSING_URL_DIAGNOSTIC: &str =
    "微信图片附件缺少下载 URL，已跳过。调试信息：image_item 中未找到 url/cdn_url/media.full_url。";
const WEIXIN_VIDEO_SKIPPED_DIAGNOSTIC: &str = "微信视频附件暂未接入内容理解，已跳过。";

pub(crate) fn weixin_inbound_message_type(message: &Value) -> bool {
    message.get("message_type").and_then(Value::as_i64) == Some(1)
}

fn weixin_file_missing_url_diagnostic(media_owner: &Value) -> String {
    let name = media_owner
        .get("file_name")
        .and_then(Value::as_str)
        .unwrap_or("(unknown)");
    format!(
        "微信文件 {name} 缺少下载 URL，已跳过。调试信息：file_item 中未找到 url/cdn_url/media.full_url。"
    )
}

fn push_weixin_attachment(
    attachments: &mut Vec<Value>,
    kind: &str,
    media_owner: &Value,
) -> Option<String> {
    let name = media_owner
        .get("file_name")
        .and_then(Value::as_str)
        .unwrap_or(if kind == "image" { "image" } else { "file" });
    let mut attachment = json!({
        "type": kind,
        "name": name,
    });
    let Some(url) = super::super::media::weixin_media_download_url(media_owner) else {
        return Some(if kind == "image" {
            WEIXIN_IMAGE_MISSING_URL_DIAGNOSTIC.into()
        } else {
            weixin_file_missing_url_diagnostic(media_owner)
        });
    };
    attachment["url"] = url.into();
    let mut metadata = json!({ "weixinMediaOwner": media_owner.clone() });
    if let Some(file_id) = media_owner
        .get("file_id")
        .or_else(|| media_owner.get("fileId"))
        .and_then(Value::as_str)
    {
        metadata["fileId"] = file_id.into();
    }
    attachment["metadata"] = metadata;
    attachments.push(attachment);
    None
}

pub(crate) fn weixin_inbound_text_and_attachments(
    message: &Value,
) -> Option<(String, Vec<Value>)> {
    if message.get("message_type").and_then(Value::as_i64)? != 1 {
        return None;
    }
    let items = message.get("item_list")?.as_array()?;
    let mut parts: Vec<String> = Vec::new();
    let mut attachments = Vec::new();
    let mut diagnostics = Vec::new();
    for item in items {
        match item.get("type").and_then(Value::as_i64) {
            Some(1) => {
                if let Some(text) = item
                    .get("text_item")
                    .and_then(|value| value.get("text"))
                    .and_then(Value::as_str)
                {
                    parts.push(text.to_owned());
                }
            }
            Some(2) => {
                if let Some(image) = item.get("image_item") {
                    if let Some(line) = push_weixin_attachment(&mut attachments, "image", image) {
                        diagnostics.push(line);
                    }
                }
            }
            // `weixin-ilink@0.1.0` `dist/types.js:13-19`: VOICE=3, FILE=4, VIDEO=5
            Some(3) => {
                if let Some(voice) = item
                    .get("voice_item")
                    .and_then(|v| v.get("text"))
                    .and_then(Value::as_str)
                    .filter(|text| !text.is_empty())
                {
                    parts.push(format!("[微信语音转文字]\n{voice}"));
                }
            }
            Some(4) => {
                if let Some(file) = item.get("file_item") {
                    if let Some(line) = push_weixin_attachment(&mut attachments, "file", file) {
                        diagnostics.push(line);
                    }
                }
            }
            Some(5) => {
                diagnostics.push(WEIXIN_VIDEO_SKIPPED_DIAGNOSTIC.into());
            }
            _ => {}
        }
    }
    if !diagnostics.is_empty() {
        parts.push(super::super::attachments::format_attachment_diagnostics_block(
            &diagnostics,
        ));
    }
    let text = crate::metadata::js_trim(&parts.join("\n")).to_owned();
    if text.is_empty() && attachments.is_empty() {
        return None;
    }
    Some((text, attachments))
}

pub(crate) fn weixin_inbound_text(message: &Value) -> Option<String> {
    let mut parts = Vec::new();
    for item in message.get("item_list")?.as_array()? {
        if item.get("type").and_then(Value::as_i64) != Some(1) {
            continue;
        }
        if let Some(text) = item
            .get("text_item")
            .and_then(|value| value.get("text"))
            .and_then(Value::as_str)
        {
            parts.push(text);
        }
    }
    (!parts.is_empty()).then(|| parts.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::weixin_text_chunks;
    use serde_json::json;

    #[test]
    fn reads_item_list_text_item_text_like_ts() {
        let message = json!({
            "message_type": 1,
            "from_user_id": "mock-user",
            "context_token": "ctx",
            "item_list": [{ "type": 1, "text_item": { "text": "/update check" } }]
        });
        assert_eq!(
            weixin_inbound_text(&message).as_deref(),
            Some("/update check")
        );
    }

    #[test]
    fn only_message_type_one_is_processed() {
        assert!(!weixin_inbound_message_type(&json!({ "message_type": 2 })));
        assert!(weixin_inbound_message_type(&json!({ "message_type": 1 })));
    }

    #[test]
    fn collects_image_and_file_items_as_attachments() {
        let (text, attachments) = weixin_inbound_text_and_attachments(&json!({
            "message_type": 1,
            "item_list": [
                {"type": 1, "text_item": {"text": "see file"}},
                {"type": 2, "image_item": {"file_name": "photo.png", "url": "https://cdn.example/photo.png"}},
                {"type": 4, "file_item": {"file_name": "doc.pdf", "url": "https://cdn.example/doc.pdf"}}
            ]
        }))
        .unwrap();
        assert_eq!(text, "see file");
        assert_eq!(attachments.len(), 2);
        assert_eq!(attachments[0]["type"], "image");
        assert_eq!(attachments[1]["type"], "file");
    }

    #[test]
    fn cos_url_only_file_item_emits_diagnostics_like_ts() {
        let (text, attachments) = weixin_inbound_text_and_attachments(&json!({
            "message_type": 1,
            "item_list": [
                {"type": 4, "file_item": {
                    "file_name": "doc.pdf",
                    "cos_url": "https://cdn.example/doc.pdf",
                    "file_id": "fid-1"
                }}
            ]
        }))
        .unwrap();
        assert!(attachments.is_empty());
        assert!(text.contains("[Attachment diagnostics]"));
        assert!(text.contains("微信文件 doc.pdf 缺少下载 URL，已跳过"));
    }

    #[test]
    fn prefers_cdn_url_and_keeps_media_owner_for_decrypt() {
        let (_, attachments) = weixin_inbound_text_and_attachments(&json!({
            "message_type": 1,
            "item_list": [{
                "type": 2,
                "image_item": {
                    "file_name": "photo.png",
                    "cdn_url": "https://cdn.example/cdn.png",
                    "url": "https://cdn.example/url.png",
                    "aeskey": "0123456789abcdef0123456789abcdef"
                }
            }]
        }))
        .unwrap();
        assert_eq!(attachments[0]["url"], "https://cdn.example/cdn.png");
    }

    #[test]
    fn voice_type_three_requires_nonempty_text_like_ts() {
        let empty = weixin_inbound_text_and_attachments(&json!({
            "message_type": 1,
            "item_list": [{"type": 3, "voice_item": {"text": ""}}]
        }));
        assert!(empty.is_none());
        let (text, attachments) = weixin_inbound_text_and_attachments(&json!({
            "message_type": 1,
            "item_list": [{"type": 3, "voice_item": {"text": "你好"}}]
        }))
        .unwrap();
        assert!(attachments.is_empty());
        assert_eq!(text, "[微信语音转文字]\n你好");
    }

    #[test]
    fn filename_fallback_not_used_for_missing_url_diagnostic() {
        let (text, _) = weixin_inbound_text_and_attachments(&json!({
            "message_type": 1,
            "item_list": [{"type": 4, "file_item": {"filename": "hidden.pdf", "cos_url": "https://x"}}]
        }))
        .unwrap();
        assert!(text.contains("微信文件 (unknown) 缺少下载 URL"));
        assert!(!text.contains("hidden.pdf"));
    }

    #[test]
    fn chunks_by_utf16_code_units_not_bytes() {
        let emoji = "😀";
        let body = "x".repeat(2000) + emoji;
        let chunks = weixin_text_chunks(&body);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].encode_utf16().count(), 2000);
        assert_eq!(chunks[1], emoji);
    }
}
