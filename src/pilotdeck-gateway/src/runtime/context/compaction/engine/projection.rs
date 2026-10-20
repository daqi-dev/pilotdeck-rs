use super::prompt::{decode_units, encode_units, join, regex_replace, visible};
use super::*;
use regex::Regex;
use std::{
    collections::{HashMap, HashSet},
    sync::LazyLock,
};

pub(super) fn project_summary(messages: &[JsValue]) -> Vec<JsValue> {
    let messages = strip_multimedia::strip_multimedia_from_messages(messages);
    let names = collect_tool_names_by_call_id(&messages);
    // Deduplicate Node's UTF-8 hash input, including the replacement of lone
    // surrogates. No second JSON serializer or hash dependency is needed.
    let mut seen = HashSet::new();
    messages
        .iter()
        .map(|m| {
            replace_content(
                m,
                content(m)
                    .iter()
                    .map(|b| {
                        if is_block(b, "tool_call") {
                            object([
                                ("type", text("tool_call")),
                                ("id", field(b, "id").clone()),
                                ("name", field(b, "name").clone()),
                                ("input", prune_input(field(b, "input"), 0)),
                            ])
                        } else if is_block(b, "tool_result") {
                            let flattened = trim(&flatten_tool_result_block_text(b));
                            let id = string_field(b, "toolCallId").cloned().unwrap_or_default();
                            let name = names.get(&id).cloned().unwrap_or_else(|| "unknown".into());
                            let hash_input = String::from_utf16_lossy(flattened.units());
                            let duplicate =
                                flattened.len_utf16() > 2000 && seen.contains(&hash_input);
                            if flattened.len_utf16() > 320 {
                                seen.insert(hash_input);
                            }
                            let blocks = if duplicate {
                                vec![text_block(join(
                                    &[
                                        "[Duplicate tool output omitted for ".into(),
                                        name,
                                        " call ".into(),
                                        id,
                                        ". A newer call produced the same large output.]".into(),
                                    ],
                                    "",
                                ))]
                            } else if flattened.len_utf16() > 2000 {
                                vec![text_block(result_preview(
                                    &name,
                                    &id,
                                    &flattened,
                                    field(b, "isError") == &JsValue::Bool(true),
                                ))]
                            } else {
                                content(b).to_vec()
                            };
                            object([
                                ("type", text("tool_result")),
                                ("toolCallId", field(b, "toolCallId").clone()),
                                ("isError", field(b, "isError").clone()),
                                ("content", JsValue::Array(blocks)),
                            ])
                        } else if is_block(b, "tool_result_reference") {
                            let mut result = b.clone();
                            for (name, max, required) in [
                                ("path", 600, true),
                                ("readFilePath", 600, false),
                                ("preview", 1200, true),
                                ("reason", 1200, false),
                            ] {
                                if required || js_truthy(field(b, name)) {
                                    set(
                                        &mut result,
                                        name,
                                        JsValue::String(truncate_anchor(
                                            &string_field(b, name).cloned().unwrap_or_default(),
                                            max,
                                        )),
                                    );
                                }
                            }
                            result
                        } else {
                            b.clone()
                        }
                    })
                    .collect(),
            )
        })
        .filter(|m| !content(m).is_empty())
        .collect()
}

// The source bound is four container levels and twenty children at each
// level. This is deliberately bounded recursion, never a general serializer.
fn prune_input(value: &JsValue, depth: usize) -> JsValue {
    match value {
        JsValue::String(s) => JsValue::String(truncate_input(s)),
        JsValue::Array(_) | JsValue::Object(_) if depth >= 4 => text("[Truncated]"),
        JsValue::Array(items) => JsValue::Array(
            items
                .iter()
                .take(20)
                .map(|v| prune_input(v, depth + 1))
                .collect(),
        ),
        JsValue::Object(value) => {
            let mut out = JsObject::new();
            for (key, value) in value.own_entries().into_iter().take(20) {
                let value = prune_input(value, depth + 1);
                // TS builds `{}` and assigns with the legacy prototype setter.
                if !key.equals_str("__proto__") {
                    out.insert(key.clone(), value);
                }
            }
            JsValue::Object(out)
        }
        _ => value.clone(),
    }
}
fn truncate_input(value: &JsString) -> JsString {
    if value.len_utf16() <= 2000 {
        value.clone()
    } else {
        join(
            &[
                trim_end(&JsString::from_utf16(value.units()[..1984].to_vec())),
                "...[truncated]".into(),
            ],
            "",
        )
    }
}
pub(super) fn truncate_anchor(value: &JsString, max: usize) -> JsString {
    if value.len_utf16() <= max {
        value.clone()
    } else {
        join(
            &[
                JsString::from_utf16(value.units()[..max.saturating_sub(24)].to_vec()),
                "\n...[truncated]".into(),
            ],
            "",
        )
    }
}
fn stringify_anchor(value: &JsValue, max: usize) -> Result<JsString, HostError> {
    let serialized = match crate::js_value::json_stringify(value) {
        Ok(Some(s)) => s.into(),
        // JSON.stringify(undefined) makes truncateForAnchor throw; the TS
        // fallback is String(undefined). Deep arrays preserve V8's error.
        _ => JsString::from_utf16(
            crate::js_value::to_string_units(value)
                .map_err(|_| failure(crate::js_value::StackOverflow::MESSAGE))?,
        ),
    };
    Ok(truncate_anchor(&serialized, max))
}
fn json(value: &JsValue) -> Result<JsString, HostError> {
    Ok(crate::js_value::json_stringify(value)
        .map_err(|_| failure(crate::js_value::StackOverflow::MESSAGE))?
        .unwrap_or_default()
        .into())
}
fn result_preview(name: &JsString, id: &JsString, flattened: &JsString, error: bool) -> JsString {
    static SPACE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"[\t\n\x0b\x0c\r \u{a0}\u{1680}\u{2000}-\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}\u{feff}]+").unwrap()
    });
    let normalized = trim(&regex_replace(flattened, &SPACE, " "));
    let preview = if normalized.len_utf16() <= 2000 {
        normalized
    } else {
        join(
            &[
                trim_end(&JsString::from_utf16(normalized.units()[..800].to_vec())),
                "\n...[truncated]...\n".into(),
                trim_start(&JsString::from_utf16(
                    normalized.units()[normalized.len_utf16() - 240..].to_vec(),
                )),
            ],
            "",
        )
    };
    join(
        &[
            "[".into(),
            name.clone(),
            if error {
                "] error for call ".into()
            } else {
                "] output for call ".into()
            },
            id.clone(),
            ": ".into(),
            preview,
        ],
        "",
    )
}
pub(super) fn project_retained(
    messages: &[JsValue],
    names: &HashMap<JsString, JsString>,
) -> (Vec<JsValue>, bool) {
    let mut changed = false;
    let out =
        messages
            .iter()
            .map(|m| {
                replace_content(
                    m,
                    content(m)
                        .iter()
                        .map(|b| {
                            if !is_block(b, "tool_result") {
                                return b.clone();
                            }
                            let flat = trim(&flatten_tool_result_block_text(b));
                            if flat.len_utf16() <= 2000 {
                                return b.clone();
                            }
                            changed = true;
                            let id = string_field(b, "toolCallId").cloned().unwrap_or_default();
                            let name = names.get(&id).cloned().unwrap_or_else(|| "unknown".into());
                            let is_error = field(b, "isError") == &JsValue::Bool(true);
                            let mut out = object([
                                ("type", text("tool_result")),
                                ("toolCallId", JsValue::String(id.clone())),
                            ]);
                            if is_error {
                                set(&mut out, "isError", JsValue::Bool(true));
                            }
                            set(
                                &mut out,
                                "content",
                                JsValue::Array(vec![text_block(join(&[
            result_preview(&name,&id,&flat,is_error),
            "\n[Full output remains in the durable session transcript.]".into()], ""))]),
                            );
                            out
                        })
                        .collect(),
                )
            })
            .collect();
    (out, changed)
}
pub(super) fn stable_anchors(messages: &[JsValue]) -> Result<Option<JsString>, HostError> {
    static TOOL: LazyLock<Regex> =
        LazyLock::new(|| Regex::new("(?i-u:skill|file|knowledge)").unwrap());
    static ID: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i-u:\b(?:r[0-9]{2}[-_][A-Za-z0-9_.-]+|kdoc[_-][A-Za-z0-9_.-]+|kchunk[_-][A-Za-z0-9_.-]+|(?:skill|file|knowledge|document)[-_][A-Za-z0-9_.:/-]+)\b)").unwrap()
    });
    let mut lines = vec![JsString::from("<stable-source-anchors>")];
    for b in messages.iter().flat_map(content) {
        if !is_block(b, "tool_call")
            || !string_field(b, "name").is_some_and(|n| TOOL.is_match(&encode_units(n)))
        {
            continue;
        }
        let mut values = vec![stringify_anchor(field(b, "input"), 2000)?];
        for r in messages
            .iter()
            .flat_map(content)
            .filter(|r| field(r, "toolCallId") == field(b, "id"))
        {
            if is_block(r, "tool_result_reference") {
                values.extend(
                    ["path", "readFilePath", "preview"]
                        .map(|k| string_field(r, k).cloned().unwrap_or_default()),
                );
            } else if is_block(r, "tool_result") {
                values.push(flatten_tool_result_block_text(r));
            }
        }
        let source = encode_units(&join(&values, "\n"));
        let mut seen = HashSet::new();
        let ids: Vec<_> = ID
            .find_iter(&source)
            .map(|m| decode_units(m.as_str()))
            .filter(|s| seen.insert(s.clone()))
            .map(JsValue::String)
            .collect();
        if !ids.is_empty() {
            lines.push(json(&object([
                ("toolName", field(b, "name").clone()),
                ("identifiers", JsValue::Array(ids)),
            ]))?);
        }
        if lines.len() == 13 {
            break;
        }
    }
    if lines.len() == 1 {
        return Ok(None);
    }
    lines.push("</stable-source-anchors>".into());
    Ok(Some(join(&lines, "\n")))
}
pub(super) fn protected_anchors(
    messages: &[JsValue],
    protected: &ProtectedNames,
) -> Result<Option<JsString>, HostError> {
    // protectedContext splits complete user turns, unlike the engine planner.
    let mut turns: Vec<Vec<JsValue>> = Vec::new();
    let mut current = Vec::new();
    for m in messages {
        if is_role(m, "user") && !is_tool_result_only_strict(m) && !current.is_empty() {
            turns.push(std::mem::take(&mut current));
        }
        current.push(m.clone());
    }
    if !current.is_empty() {
        turns.push(current);
    }
    let mut anchors = Vec::new();
    for (index, turn) in turns.iter().enumerate() {
        let user = truncate_anchor(
            &join(
                &turn
                    .iter()
                    .filter(|m| is_role(m, "user"))
                    .map(visible)
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>(),
                "\n",
            ),
            1200,
        );
        let mut results = HashMap::new();
        for b in turn.iter().flat_map(content) {
            if (is_block(b, "tool_result") || is_block(b, "tool_result_reference"))
                && let Some(id) = string_field(b, "toolCallId")
            {
                results.insert(id.clone(), b);
            }
        }
        for b in turn
            .iter()
            .filter(|m| is_role(m, "assistant"))
            .flat_map(content)
            .filter(|b| is_block(b, "tool_call"))
        {
            if !string_field(b, "name").is_some_and(|n| protected.contains(n)) {
                continue;
            }
            let mut anchor = object([
                ("turn", num(index)),
                ("toolName", field(b, "name").clone()),
                ("toolCallId", field(b, "id").clone()),
            ]);
            if !user.is_empty() {
                set(&mut anchor, "userRequest", JsValue::String(user.clone()));
            }
            if !field(b, "input").is_undefined() {
                set(
                    &mut anchor,
                    "input",
                    JsValue::String(stringify_anchor(field(b, "input"), 2000)?),
                );
            }
            if let Some(result) = string_field(b, "id").and_then(|id| results.get(id)) {
                let preview = if is_block(result, "tool_result") {
                    flatten_tool_result_block_text(result)
                } else {
                    string_field(result, "preview").cloned().unwrap_or_default()
                };
                let preview = truncate_anchor(&preview, 1200);
                if !preview.is_empty() {
                    set(&mut anchor, "resultPreview", JsValue::String(preview));
                }
                if !field(result, "isError").is_undefined() {
                    set(
                        &mut anchor,
                        "resultIsError",
                        field(result, "isError").clone(),
                    );
                }
                if is_block(result, "tool_result_reference") {
                    let mut reference = object([(
                        "path",
                        JsValue::String(truncate_anchor(
                            &string_field(result, "path").cloned().unwrap_or_default(),
                            600,
                        )),
                    )]);
                    if js_truthy(field(result, "readFilePath")) {
                        set(
                            &mut reference,
                            "readFilePath",
                            JsValue::String(truncate_anchor(
                                &string_field(result, "readFilePath")
                                    .cloned()
                                    .unwrap_or_default(),
                                600,
                            )),
                        );
                    }
                    set(
                        &mut reference,
                        "originalBytes",
                        field(result, "originalBytes").clone(),
                    );
                    set(&mut reference, "hasMore", field(result, "hasMore").clone());
                    if js_truthy(field(result, "mimeType")) {
                        set(
                            &mut reference,
                            "mimeType",
                            field(result, "mimeType").clone(),
                        );
                    }
                    if js_truthy(field(result, "reason")) {
                        set(
                            &mut reference,
                            "reason",
                            JsValue::String(truncate_anchor(
                                &string_field(result, "reason").cloned().unwrap_or_default(),
                                1200,
                            )),
                        );
                    }
                    set(&mut anchor, "resultReference", reference);
                }
            }
            anchors.push(anchor);
        }
    }
    if anchors.is_empty() {
        return Ok(None);
    }
    let mut lines: Vec<JsString> = vec![
        "<compact-summary-anchors>".into(),
        "Each following line is a JSON object with visible facts from one protected tool call."
            .into(),
    ];
    let mut length = join(&lines, "\n").len_utf16();
    let mut included = 0;
    for anchor in anchors.iter().take(12) {
        let line = json(anchor)?;
        if length + line.len_utf16() + 1 > 16_000 {
            break;
        }
        length += line.len_utf16() + 1;
        included += 1;
        lines.push(line);
    }
    if included < anchors.len() {
        lines.push(json(&object([
            ("truncated", JsValue::Bool(true)),
            ("omittedProtectedToolCalls", num(anchors.len() - included)),
        ]))?);
    }
    lines.push("</compact-summary-anchors>".into());
    Ok(Some(join(&lines, "\n")))
}
