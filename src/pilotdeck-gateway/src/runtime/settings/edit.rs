//! Conservative source-range YAML editing. Unrecognized topology fails closed.
use super::{SettingsError, normalize::Action};
#[cfg(test)]
use pilotdeck_protocol::JsValue;
use pilotdeck_protocol::{JsString, stringify_json};
use pilotdeck_yaml::{
    Options,
    nodes::{Node, NodeKind, ScalarValue},
    parse_document,
};

fn invalid(message: &str) -> SettingsError {
    SettingsError::local_settings(
        "LOCAL_SETTINGS_YAML_INVALID",
        format!("Unable to update local settings: {message}"),
    )
}
fn unsupported() -> SettingsError {
    SettingsError::local_settings(
        "LOCAL_SETTINGS_YAML_UNSUPPORTED",
        "Unable to safely edit this YAML structure; the original file was preserved.",
    )
}
fn key_is(node: &Node, name: &str) -> bool {
    matches!(&node.kind,NodeKind::Scalar(ScalarValue::String(s)) if s.equals_str(name))
}
fn value_text(action: &Action, rest: &[&str]) -> Result<String, SettingsError> {
    let Some(value) = &action.value else {
        return Err(unsupported());
    };
    let mut raw = stringify_json(value)
        .map_err(|e| invalid(&e.to_string()))?
        .ok_or_else(unsupported)?;
    for name in rest.iter().rev() {
        raw = format!("{{\"{name}\":{raw}}}");
    }
    Ok(raw)
}
fn line_start(source: &[u16], at: usize) -> usize {
    source[..at]
        .iter()
        .rposition(|&c| c == 10)
        .map_or(0, |i| i + 1)
}
fn indentation(source: &[u16], at: usize) -> Result<String, SettingsError> {
    let start = line_start(source, at);
    let units = &source[start..at];
    if units.iter().any(|&u| u != 32) {
        return Err(unsupported());
    }
    Ok(" ".repeat(units.len()))
}
fn replace(
    source: &mut Vec<u16>,
    start: usize,
    end: usize,
    text: &str,
) -> Result<(), SettingsError> {
    if start > end || end > source.len() {
        return Err(unsupported());
    }
    source.splice(start..end, text.encode_utf16());
    Ok(())
}
fn edit_one(source: &mut Vec<u16>, action: &Action) -> Result<(), SettingsError> {
    let doc = parse_document(
        &JsString::from_utf16(source.clone()),
        Options {
            pretty_errors: false,
            ..Options::default()
        },
    );
    if !doc.errors.is_empty() {
        return Err(invalid("Invalid YAML document."));
    }
    let mut id = doc
        .contents
        .ok_or_else(|| invalid("Config root must be a YAML mapping."))?;
    if !matches!(doc.nodes[id].kind, NodeKind::Map(_)) {
        return Err(invalid("Config root must be a YAML mapping."));
    }
    for (depth, name) in action.path.iter().enumerate() {
        let parent = &doc.nodes[id];
        let NodeKind::Map(pairs) = &parent.kind else {
            return Err(unsupported());
        };
        let found = pairs
            .iter()
            .position(|pair| key_is(&doc.nodes[pair.key], name));
        let Some(index) = found else {
            if action.value.is_none() {
                return Ok(());
            }
            let value = value_text(action, &action.path[depth + 1..])?;
            if parent.flow {
                let end = parent.range[1];
                if end == 0 || end > source.len() || source[end - 1] != 125 {
                    return Err(unsupported());
                }
                if source[parent.range[0]..end]
                    .iter()
                    .any(|&u| u == 10 || u == 35)
                {
                    return Err(unsupported());
                }
                let prefix = if pairs.is_empty() { "" } else { ", " };
                return replace(
                    source,
                    end - 1,
                    end - 1,
                    &format!("{prefix}{name}: {value}"),
                );
            }
            let indent = if let Some(pair) = pairs.first() {
                indentation(source, doc.nodes[pair.key].range[0])?
            } else {
                return Err(unsupported());
            };
            let at = parent.range[2];
            let prefix = if at > 0 && source[at - 1] != 10 {
                "\n"
            } else {
                ""
            };
            return replace(
                source,
                at,
                at,
                &format!("{prefix}{indent}{name}: {value}\n"),
            );
        };
        let pair = &pairs[index];
        if depth + 1 < action.path.len() {
            id = pair.value.ok_or_else(unsupported)?;
            if !matches!(doc.nodes[id].kind, NodeKind::Map(_)) {
                return Err(unsupported());
            }
            continue;
        }
        let Some(value_id) = pair.value else {
            return Err(unsupported());
        };
        let value = &doc.nodes[value_id];
        if value.anchor.is_some() {
            return Err(unsupported());
        }
        if action.value.is_some() {
            let replacement = value_text(action, &[])?;
            return replace(source, value.range[0], value.range[1], &replacement);
        }
        if parent.flow {
            let start = parent.range[0];
            let end = parent.range[1];
            if end > source.len() || source[start..end].iter().any(|&u| u == 10 || u == 35) {
                return Err(unsupported());
            }
            let (from, to) = if let Some(next) = pairs.get(index + 1) {
                (doc.nodes[pair.key].range[0], doc.nodes[next.key].range[0])
            } else if index > 0 {
                let previous = &pairs[index - 1];
                let prev_value = previous.value.ok_or_else(unsupported)?;
                (doc.nodes[prev_value].range[1], value.range[1])
            } else {
                (doc.nodes[pair.key].range[0], value.range[1])
            };
            return replace(source, from, to, "");
        }
        let start = line_start(source, doc.nodes[pair.key].range[0]);
        indentation(source, doc.nodes[pair.key].range[0])?;
        return replace(source, start, value.range[2], "");
    }
    Ok(())
}
pub(super) fn edit(raw: &str, actions: &[Action]) -> Result<String, SettingsError> {
    let mut source: Vec<u16> = raw.encode_utf16().collect();
    // Parse even no-op requests, matching updatePilotLocalSettings' file validation.
    if actions.is_empty() {
        let doc = parse_document(
            &JsString::from_utf16(source.clone()),
            Options {
                pretty_errors: false,
                ..Options::default()
            },
        );
        if !doc.errors.is_empty()
            || !doc
                .contents
                .is_some_and(|id| matches!(doc.nodes[id].kind, NodeKind::Map(_)))
        {
            return Err(invalid("Config root must be a YAML mapping."));
        }
    }
    for action in actions {
        edit_one(&mut source, action)?;
    }
    String::from_utf16(&source).map_err(|_| unsupported())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pilotdeck_protocol::parse_json;
    #[test]
    fn edits_preserve_human_comments_and_match_ts_values() {
        let oracle = parse_json(include_str!(
            "../../../tests/fixtures/native-settings/writer-oracle.json"
        ))
        .unwrap();
        let JsValue::Array(cases) = oracle.get("cases").unwrap() else {
            panic!("cases")
        };
        for case in cases {
            if case.get("error").is_some() {
                continue;
            }
            let id = case
                .get("id")
                .unwrap()
                .as_string()
                .unwrap()
                .to_utf8()
                .unwrap();
            let before = case
                .get("before")
                .unwrap()
                .as_string()
                .unwrap()
                .to_utf8()
                .unwrap();
            let expected = case.get("after").unwrap().as_string().unwrap();
            let actions =
                super::super::normalize::normalize(case.get("settings").unwrap()).unwrap();
            let actual = edit(&before, &actions)
                .unwrap_or_else(|e| panic!("{id}: {}", e.message().to_utf8().unwrap()));
            assert!(actual.contains("# human config"), "{id}");
            assert!(actual.contains("# keep model") || id == "flow", "{id}");
            let actual_doc = parse_document(&JsString::from(actual), Options::default());
            let expected_doc = parse_document(expected, Options::default());
            assert!(actual_doc.errors.is_empty(), "{id}");
            assert_eq!(
                actual_doc.to_json().unwrap(),
                expected_doc.to_json().unwrap(),
                "{id}"
            );
        }
    }
}
