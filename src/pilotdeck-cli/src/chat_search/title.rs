//! SessionList.ts title projection over the native raw bytes. Preview fields
//! deliberately use the baseline's raw-line captures, even for partial JSON.
use super::{Text, string, trim};
use fancy_regex::Regex;
use pilotdeck_protocol::{JsValue, parse_json};
use std::{collections::HashMap, sync::LazyLock};
const LITE_BYTES: usize = 65_536;
const SPACE: &str =
    r"\x09-\x0d\x20\xa0\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}";

pub(super) fn read(bytes: &[u8]) -> Option<Text> {
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(LITE_BYTES)]);
    let tail = String::from_utf8_lossy(&bytes[bytes.len().saturating_sub(LITE_BYTES)..]);
    let source = format!("{head}\n{tail}");
    let fast = (|| {
        let custom = captured(&source, 0)?;
        let ai = captured(&source, 1)?;
        let prompt = prompt(&tail, true).or_else(|| prompt(&head, false));
        let summary = custom.or(ai).or(prompt).filter(|s| !s.0.is_empty());
        // readSessionInfo's firstCreatedAt capture is also parsed inside the
        // caller's catch; malformed escapes invalidate the fast info.
        first_created(&head)?;
        Ok::<_, ()>(summary)
    })()
    .ok()
    .flatten();
    if fast.is_some() && bytes.len() <= LITE_BYTES {
        return fast;
    }
    let mut metadata = HashMap::new();
    // A full trailing snapshot can skip recovery of older patches.
    let snapshot = if bytes.len() > LITE_BYTES {
        let mut latest = None;
        for line in tail.split('\n').skip(1) {
            if !line.contains("\"type\":\"session_metadata\"") {
                continue;
            }
            latest = strict(line);
            if latest.is_none() {
                break;
            }
        }
        latest.filter(|m| {
            m.get("isSnapshot") == Some(&JsValue::Bool(true))
                && ["title", "aiTitle", "lastPrompt", "firstPrompt"]
                    .iter()
                    .any(|key| string(m, key).is_some_and(|s| !trim(&s).0.is_empty()))
        })
    } else {
        None
    };
    if let Some(snapshot) = snapshot {
        merge(&mut metadata, &snapshot);
    } else {
        let source = String::from_utf8_lossy(bytes);
        for line in source.split('\n') {
            if let Some(value) = strict(line) {
                merge(&mut metadata, &value);
            }
        }
    }
    let metadata_summary = ["title", "aiTitle", "lastPrompt", "firstPrompt"]
        .into_iter()
        .find_map(|k| metadata.get(k))
        .filter(|s| !trim(s).0.is_empty());
    if let Some(summary) = metadata_summary {
        if first_created(&head).is_err() {
            return None;
        }
        return metadata
            .get("title")
            .or_else(|| metadata.get("aiTitle"))
            .or(Some(summary))
            .cloned();
    }
    fast
}
fn merge(target: &mut HashMap<&'static str, Text>, metadata: &JsValue) {
    for key in ["title", "aiTitle", "lastPrompt", "firstPrompt"] {
        if let Some(text) = string(metadata, key) {
            target.insert(key, text);
        }
    }
}
fn strict(line: &str) -> Option<JsValue> {
    if !line.contains("\"type\":\"session_metadata\"") {
        return None;
    }
    let entry = parse_json(line).ok()?;
    if !string(&entry, "type").is_some_and(|s| s.0.equals_str("session_metadata")) {
        return None;
    }
    entry
        .get("metadata")
        .filter(|v| v.as_object().is_some())
        .cloned()
}
fn expressions() -> &'static [Regex; 7] {
    static EXPRESSIONS: LazyLock<[Regex; 7]> = LazyLock::new(|| {
        [
            "title",
            "aiTitle",
            "tag",
            "parentSessionId",
            "forkedFromTurnId",
            "createdAt",
            "createdAt",
        ]
        .map(|key| {
            Regex::new(&format!(r#""{key}"[{SPACE}]*:[{SPACE}]*"((?:\\.|[^"])*)""#))
                .expect("fixed JSON field regex")
        })
    });
    &EXPRESSIONS
}
fn captured(source: &str, wanted: usize) -> Result<Option<Text>, ()> {
    let mut values: [Option<Text>; 6] = Default::default();
    // All fast metadata captures are evaluated, not only the displayed fields.
    for line in source
        .split('\n')
        .filter(|line| line.contains("\"session_metadata\""))
    {
        for (index, value) in values.iter_mut().enumerate().take(5) {
            if let Some(capture) = expressions()[index].captures(line).map_err(|_| ())?
                && !capture[1].is_empty()
            {
                *value = Some(decode(&capture[1])?);
            }
        }
    }
    Ok(values[wanted].take())
}
fn decode(text: &str) -> Result<Text, ()> {
    parse_json(&format!("\"{text}\""))
        .ok()
        .and_then(|v| v.as_string().map(|s| Text(s.clone())))
        .ok_or(())
}
fn first_created(source: &str) -> Result<(), ()> {
    if let Some(capture) = expressions()[6].captures(source).map_err(|_| ())?
        && !capture[1].is_empty()
    {
        decode(&capture[1])?;
    }
    Ok(())
}
fn prompt(source: &str, last: bool) -> Option<Text> {
    let mut value = None;
    for line in source
        .split('\n')
        .filter(|line| line.contains("\"type\":\"accepted_input\""))
    {
        let text = (|| {
            let entry = parse_json(line).map_err(|_| ())?;
            let messages = match entry.get("messages") {
                Some(JsValue::Array(messages)) => messages,
                None | Some(JsValue::Null) => return Ok(None),
                _ => return Err(()),
            };
            for message in messages {
                if matches!(message, JsValue::Null) {
                    return Err(());
                }
                let blocks = match message.get("content") {
                    Some(JsValue::Array(blocks)) => blocks.as_slice(),
                    None | Some(JsValue::Null) => continue,
                    Some(block) => std::slice::from_ref(block),
                };
                for block in blocks {
                    if matches!(block, JsValue::Null) {
                        return Err(());
                    }
                    if string(block, "type").is_some_and(|s| s.0.equals_str("text")) {
                        return match block.get("text") {
                            None | Some(JsValue::Null) => Ok(None),
                            Some(JsValue::String(s)) => {
                                let text = Text(s.clone());
                                Ok((!trim(&text).0.is_empty()).then(|| trim(&text)))
                            }
                            _ => Err(()),
                        };
                    }
                }
            }
            Ok(None)
        })();
        match text {
            Ok(Some(text)) if !last => return Some(text),
            Ok(Some(text)) => value = Some(text),
            Err(()) if !last => return None,
            _ => {}
        }
    }
    value
}
