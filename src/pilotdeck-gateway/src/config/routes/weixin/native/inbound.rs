//! Inbound message parsing (`WeixinChannel.ts:560-628`, `weixin-ilink` `MessageItemType`).
use super::super::item_types::{
    MESSAGE_ITEM_FILE, MESSAGE_ITEM_IMAGE, MESSAGE_ITEM_TEXT, MESSAGE_ITEM_VIDEO,
    MESSAGE_ITEM_VOICE,
};
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
    message_id: &str,
    kind: &str,
    media_owner: &Value,
) -> Option<String> {
    let name = if kind == "image" {
        format!("image-{message_id}.jpg")
    } else {
        media_owner
            .get("file_name")
            .and_then(Value::as_str)
            .unwrap_or("file")
            .to_owned()
    };
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
    if let Some(mime) = super::super::media::guess_mime_type_from_url(
        &url,
        if kind == "image" { Some("image/jpeg") } else { None },
    ) {
        attachment["mimeType"] = mime.into();
    }
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

fn json_nonempty_id(value: &Value) -> Option<String> {
    match value {
        Value::String(text) if !text.trim().is_empty() => Some(text.to_owned()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

fn resolve_inbound_attachment_id(message: &Value) -> String {
    message
        .get("message_id")
        .or_else(|| message.get("client_id"))
        .and_then(json_nonempty_id)
        .unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_millis().to_string())
                .unwrap_or_else(|_| "0".into())
        })
}

pub(crate) fn weixin_inbound_text_and_attachments(
    message: &Value,
) -> Option<(String, Vec<Value>)> {
    if message.get("message_type").and_then(Value::as_i64)? != 1 {
        return None;
    }
    let items = message.get("item_list")?.as_array()?;
    let message_id = resolve_inbound_attachment_id(message);
    let mut parts: Vec<String> = Vec::new();
    let mut attachments = Vec::new();
    let mut diagnostics = Vec::new();
    for item in items {
        match item.get("type").and_then(Value::as_i64) {
            Some(t) if t == MESSAGE_ITEM_TEXT => {
                if let Some(text) = item
                    .get("text_item")
                    .and_then(|value| value.get("text"))
                    .and_then(Value::as_str)
                {
                    parts.push(text.to_owned());
                }
            }
            Some(t) if t == MESSAGE_ITEM_IMAGE => {
                if let Some(image) = item.get("image_item") {
                    if let Some(line) =
                        push_weixin_attachment(&mut attachments, &message_id, "image", image)
                    {
                        diagnostics.push(line);
                    }
                } else {
                    diagnostics.push(WEIXIN_IMAGE_MISSING_URL_DIAGNOSTIC.into());
                }
            }
            Some(t) if t == MESSAGE_ITEM_VOICE => {
                if let Some(voice) = item
                    .get("voice_item")
                    .and_then(|v| v.get("text"))
                    .and_then(Value::as_str)
                    .filter(|text| !text.is_empty())
                {
                    parts.push(format!("[微信语音转文字]\n{voice}"));
                }
            }
            Some(t) if t == MESSAGE_ITEM_FILE => {
                if let Some(file) = item.get("file_item") {
                    if let Some(line) =
                        push_weixin_attachment(&mut attachments, &message_id, "file", file)
                    {
                        diagnostics.push(line);
                    }
                } else {
                    diagnostics.push(
                        "微信文件 (unknown) 缺少下载 URL，已跳过。调试信息：file_item 中未找到 url/cdn_url/media.full_url。"
                            .into(),
                    );
                }
            }
            Some(t) if t == MESSAGE_ITEM_VIDEO => {
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
                {"type": 2, "image_item": {"url": "https://cdn.example/photo.png"}},
                {"type": 4, "file_item": {"file_name": "doc.pdf", "url": "https://cdn.example/doc.pdf"}}
            ]
        }))
        .unwrap();
        assert_eq!(text, "see file");
        assert_eq!(attachments.len(), 2);
        assert_eq!(attachments[0]["type"], "image");
        let image_name = attachments[0]["name"].as_str().unwrap();
        assert!(image_name.starts_with("image-") && image_name.ends_with(".jpg"));
        assert_ne!(image_name, "image-(unknown).jpg");
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
        assert!(!chunks[0].contains('\u{FFFD}'));
        assert!(!chunks[1].contains('\u{FFFD}'));
    }

    #[test]
    fn inbound_numeric_message_id_stringifies() {
        let (_, attachments) = weixin_inbound_text_and_attachments(&json!({
            "message_type": 1,
            "message_id": 4242,
            "item_list": [{
                "type": 2,
                "image_item": {"url": "https://cdn.example/x.png"}
            }]
        }))
        .unwrap();
        assert_eq!(attachments[0]["name"], "image-4242.jpg");
    }

    #[test]
    fn inbound_image_name_matches_ts_message_id_pattern() {
        let (_, attachments) = weixin_inbound_text_and_attachments(&json!({
            "message_type": 1,
            "message_id": "mid-42",
            "item_list": [{
                "type": 2,
                "image_item": {"file_name": "photo.png", "url": "https://cdn.example/x.png"}
            }]
        }))
        .unwrap();
        assert_eq!(attachments[0]["name"], "image-mid-42.jpg");
    }
}
