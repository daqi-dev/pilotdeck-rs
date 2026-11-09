//! Discovery's recent-chat digest over the real bounded session catalog.
use super::chat_tool::ChatCatalog;
use crate::runtime::{history::HistoryFiles, tools::ToolError};
use pilotdeck_protocol::{JsString, JsValue};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

pub(crate) struct DigestItem {
    pub alias: JsString,
    pub title: JsString,
    pub modified: JsString,
    pub prompts: Vec<JsString>,
}
pub(crate) struct ChatDigest {
    pub sessions: Vec<DigestItem>,
    pub aliases: HashMap<JsString, JsString>,
}
fn ws(c: u16) -> bool {
    matches!(c, 0x0009..=0x000d | 0x0020 | 0x00a0 | 0x1680 | 0x2000..=0x200a
        | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff)
}
fn trim(v: &[u16]) -> &[u16] {
    let a = v.iter().position(|c| !ws(*c)).unwrap_or(v.len());
    let b = v.iter().rposition(|c| !ws(*c)).map_or(a, |i| i + 1);
    &v[a..b]
}
fn extract_prompts(head: &str, tail: &str) -> Vec<JsString> {
    let mut found = Vec::new();
    let mut seen = HashSet::new();
    for line in head.lines().chain(tail.lines()) {
        // The fixed TS lite reader deliberately tests this compact raw token.
        if !line.contains("\"type\":\"accepted_input\"") {
            continue;
        }
        let Ok(entry) = pilotdeck_protocol::parse_json(line) else {
            continue;
        };
        let Some(JsValue::Array(messages)) = entry.get("messages") else {
            continue;
        };
        for message in messages {
            let Some(JsValue::Array(blocks)) = message.get("content") else {
                continue;
            };
            for block in blocks {
                if !block
                    .get("type")
                    .and_then(JsValue::as_string)
                    .is_some_and(|s| s.equals_str("text"))
                {
                    continue;
                }
                let Some(text) = block.get("text").and_then(JsValue::as_string) else {
                    continue;
                };
                let value = JsString::from_utf16(trim(text.units()).to_vec());
                if value.is_empty() || !seen.insert(value.clone()) {
                    continue;
                }
                let mut units = value.units().iter().copied().take(500).collect::<Vec<_>>();
                if value.len_utf16() > 500 {
                    units.extend("...".encode_utf16());
                }
                found.push(JsString::from_utf16(units));
                if found.len() == 8 {
                    return found;
                }
            }
        }
    }
    found
}
pub(crate) async fn build_digest(
    catalog: Arc<dyn ChatCatalog>,
    files: &HistoryFiles,
) -> Result<ChatDigest, ToolError> {
    let rows = catalog.list().await?;
    let mut result = ChatDigest {
        sessions: Vec::new(),
        aliases: HashMap::new(),
    };
    let excluded: Vec<u16> = "always-on-execute:".encode_utf16().collect();
    for row in rows
        .iter()
        .filter(|row| {
            row.get("sessionId")
                .and_then(JsValue::as_string)
                .is_some_and(|s| !s.units().starts_with(&excluded))
        })
        .take(10)
    {
        let Some(session) = row.get("sessionId").and_then(JsValue::as_string) else {
            continue;
        };
        let Some((head, tail)) = files
            .prompt_lite(session)
            .await
            .map_err(|e| ToolError::failed(e.as_str()))?
        else {
            continue;
        };
        let prompts = extract_prompts(&head, &tail);
        if prompts.is_empty() {
            continue;
        }
        let alias = JsString::from(format!("chat_{}", result.sessions.len() + 1).as_str());
        let title = row
            .get("summary")
            .and_then(JsValue::as_string)
            .cloned()
            .unwrap_or_else(|| "undefined".into());
        let ms = row
            .get("lastModified")
            .and_then(JsValue::as_number)
            .unwrap_or(f64::NAN);
        if !ms.is_finite() || ms.abs() > 8.64e15 {
            return Err(ToolError::failed("Invalid time value"));
        }
        let modified = JsString::from(crate::runtime::cron::iso_from_epoch_ms(ms).as_str());
        result.aliases.insert(alias.clone(), session.clone());
        result.sessions.push(DigestItem {
            alias,
            title,
            modified,
            prompts,
        });
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fixed_typescript_prompt_digest_oracle() {
        let fixture = pilotdeck_protocol::parse_json(include_str!(
            "../../../tests/fixtures/always-on-cli/digest-oracle.json"
        ))
        .unwrap();
        let JsValue::Array(cases) = fixture.get("records").unwrap() else {
            panic!("cases")
        };
        assert_eq!(cases.len(), 6);
        for case in cases {
            let id = case.get("id").and_then(JsValue::as_string).unwrap();
            let input = case
                .get("input")
                .and_then(JsValue::as_string)
                .unwrap()
                .to_utf8()
                .unwrap();
            let result = JsValue::Array(
                extract_prompts(&input, "")
                    .into_iter()
                    .map(JsValue::String)
                    .collect(),
            );
            assert_eq!(Some(&result), case.get("result"), "{id:?}");
        }
    }
    #[test]
    fn compact_raw_token_deduplicates_and_truncates_utf16() {
        let exact = r#"{"type":"accepted_input","messages":[{"content":[{"type":"text","text":"  hello  "}]}]}"#;
        let spaced = r#"{"type": "accepted_input","messages":[{"content":[{"type":"text","text":"ignored"}]}]}"#;
        let head = format!("{spaced}\n{exact}");
        let values = extract_prompts(&head, exact);
        assert_eq!(values.len(), 1);
        assert!(values[0].equals_str("hello"));
        let long = format!("{}🙂", "x".repeat(499));
        let encoded = serde_json::json!({"type":"accepted_input","messages":[{"content":[{"type":"text","text":long}]}]}).to_string();
        let values = extract_prompts(&encoded, "");
        assert_eq!(values[0].len_utf16(), 503);
        assert_eq!(values[0].units()[499], 0xd83d);
    }
}
