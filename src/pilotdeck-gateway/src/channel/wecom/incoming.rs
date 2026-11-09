//! Text and media-reference extraction from a WeCom callback body.
use pilotdeck_protocol::{JsString, JsValue};

pub(crate) struct Parts {
    pub text: JsString,
    pub reply_text: Option<JsString>,
    pub media: Vec<(&'static str, JsValue)>,
}
fn value_text(value: Option<&JsValue>) -> Result<JsString, &'static str> {
    let empty = JsValue::from("");
    let v = value
        .filter(|v| !matches!(v, JsValue::Null | JsValue::Undefined))
        .unwrap_or(&empty);
    let units = crate::js_value::to_string_units_checked(v).map_err(|_| "invalid WeCom field")?;
    Ok(JsString::from_utf16(
        crate::runtime::memory::js_trim_units(&units).to_vec(),
    ))
}
fn kind(value: Option<&JsValue>) -> Result<String, &'static str> {
    Ok(String::from_utf16_lossy(value_text(value)?.units()).to_lowercase())
}
fn field<'a>(value: &'a JsValue, name: &str) -> Option<&'a JsValue> {
    value.get(name)
}
fn media(out: &mut Vec<(&'static str, JsValue)>, name: &'static str, value: Option<&JsValue>) {
    if let Some(value) = value.filter(|v| v.as_object().is_some()) {
        out.push((name, value.clone()));
    }
}
pub(crate) fn extract(body: &JsValue) -> Result<Parts, &'static str> {
    let msgtype = kind(field(body, "msgtype"))?;
    let mut texts = Vec::new();
    let mut refs = Vec::new();
    if msgtype == "mixed" {
        if let Some(JsValue::Array(items)) = field(body, "mixed").and_then(|v| v.get("msg_item")) {
            for item in items {
                if item.as_object().is_none() {
                    continue;
                }
                let item_kind = kind(item.get("msgtype"))?;
                if item_kind == "text" {
                    let text = value_text(item.get("text").and_then(|v| v.get("content")))?;
                    if !text.units().is_empty() {
                        texts.push(text);
                    }
                }
                if item_kind == "image" {
                    media(&mut refs, "image", item.get("image"));
                }
                if item_kind == "file" {
                    media(&mut refs, "file", item.get("file"));
                }
            }
        }
    } else {
        let text = value_text(body.get("text").and_then(|v| v.get("content")))?;
        if !text.units().is_empty() {
            texts.push(text);
        }
        if msgtype == "voice" || msgtype == "appmsg" {
            let key = if msgtype == "voice" {
                "voice"
            } else {
                "appmsg"
            };
            let property = if msgtype == "voice" {
                "content"
            } else {
                "title"
            };
            let text = value_text(body.get(key).and_then(|v| v.get(property)))?;
            if !text.units().is_empty() {
                texts.push(text);
            }
        }
        media(&mut refs, "image", body.get("image"));
        for media_kind in ["file", "voice", "video"] {
            if msgtype == media_kind {
                media(&mut refs, media_kind, body.get(media_kind));
            }
        }
        if msgtype == "appmsg" {
            media(
                &mut refs,
                "file",
                body.get("appmsg").and_then(|v| v.get("file")),
            );
            media(
                &mut refs,
                "image",
                body.get("appmsg").and_then(|v| v.get("image")),
            );
        }
    }
    let quote = body.get("quote");
    let quote_kind = kind(quote.and_then(|v| v.get("msgtype")))?;
    let reply_text = if quote_kind == "text" || quote_kind == "voice" {
        let value = quote
            .and_then(|v| v.get(&quote_kind))
            .and_then(|v| v.get("content"));
        Some(value_text(value)?).filter(|s| !s.units().is_empty())
    } else {
        None
    };
    if quote_kind == "image" {
        media(&mut refs, "image", quote.and_then(|v| v.get("image")));
    }
    if quote_kind == "file" {
        media(&mut refs, "file", quote.and_then(|v| v.get("file")));
    }
    let mut joined = Vec::new();
    for (i, text) in texts.iter().enumerate() {
        if i > 0 {
            joined.push('\n' as u16);
        }
        joined.extend_from_slice(text.units());
    }
    let text = JsString::from_utf16(crate::runtime::memory::js_trim_units(&joined).to_vec());
    Ok(Parts {
        text,
        reply_text,
        media: refs,
    })
}
pub(crate) fn strip_leading_mention(text: &JsString) -> JsString {
    let units = text.units();
    if units.first() != Some(&(b'@' as u16)) {
        return JsString::from_utf16(crate::runtime::memory::js_trim_units(units).to_vec());
    }
    let mut i = 1;
    while i < units.len() && !crate::runtime::memory::js_trim_units(&units[i..i + 1]).is_empty() {
        i += 1;
    }
    if i == 1 {
        return JsString::from_utf16(crate::runtime::memory::js_trim_units(units).to_vec());
    }
    JsString::from_utf16(crate::runtime::memory::js_trim_units(&units[i..]).to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pilotdeck_protocol::parse_json;
    #[test]
    fn mixed_group_voice_and_quote_match_real_ts_consumer() {
        let fixture = parse_json(include_str!("fixtures/incoming.json")).unwrap();
        let JsValue::Array(callbacks) = fixture.get("callbacks").unwrap() else {
            panic!("callbacks")
        };
        let JsValue::Array(turns) = fixture.get("turns").unwrap() else {
            panic!("turns")
        };
        for (index, (callback, turn)) in callbacks.iter().zip(turns).enumerate() {
            let parts = extract(callback.get("body").unwrap()).unwrap();
            // TS first collects the voice object, then cacheInboundMedia skips
            // it because the oracle supplies neither base64 nor a URL.
            if index == 2 {
                assert_eq!(parts.media.len(), 1);
                assert_eq!(parts.media[0].0, "voice");
                assert!(parts.media[0].1.get("base64").is_none());
                assert!(parts.media[0].1.get("url").is_none());
            } else {
                assert!(parts.media.is_empty());
            }
            let message = if parts.text.units().is_empty() {
                parts.reply_text.unwrap()
            } else {
                parts.text
            };
            let message = if index == 1 {
                strip_leading_mention(&message)
            } else {
                message
            };
            assert_eq!(
                Some(&message),
                turn.get("message").and_then(JsValue::as_string)
            );
        }
    }
}
