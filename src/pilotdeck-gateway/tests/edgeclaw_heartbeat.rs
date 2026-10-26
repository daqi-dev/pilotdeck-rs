//! Real TS oracle for A6.6-edgeclaw-12 heartbeat helpers.
use pilotdeck_gateway::runtime::memory::edgeclaw::heartbeat::helpers::{
    build_candidate_memory_preview, build_candidate_routing_query, build_general_project_shortlist,
    build_index_trace_id, common_prefix_length, create_batch_trace, create_step,
    derive_focus_turns, expose_stored_relative_path, flatten_batch_messages, has_new_content,
    infer_storage_kind, json_detail, kv_detail, list_detail, merge_session_messages,
    messages_from_value, normalize_trigger, note_detail, preview_text, same_message, text_detail,
    tokenize_search_text,
};
use pilotdeck_gateway::runtime::memory::edgeclaw::messages::MemoryMessage;
use pilotdeck_gateway::runtime::memory::edgeclaw::storage::L0SessionRecord;
use pilotdeck_gateway::runtime::memory::edgeclaw::text::now_iso_at;
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};

#[path = "common/baseline.rs"]
mod baseline;
const ORACLE: &str = include_str!("fixtures/edgeclaw-heartbeat/oracle.json");

fn oracle() -> JsValue {
    baseline::oracle(ORACLE);
    parse_json(ORACLE).unwrap()
}

fn decode(value: &JsValue) -> JsValue {
    match value {
        JsValue::Array(items) => JsValue::Array(items.iter().map(decode).collect()),
        JsValue::Object(object) => {
            if object.len() == 1 && matches!(object.get("$undefined"), Some(JsValue::Bool(true))) {
                return JsValue::Undefined;
            }
            if object.len() == 1
                && let Some(JsValue::String(kind)) = object.get("$number")
                && let Ok(kind) = kind.to_utf8()
            {
                return JsValue::Number(match kind.as_str() {
                    "NaN" => f64::NAN,
                    "Infinity" => f64::INFINITY,
                    "-Infinity" => f64::NEG_INFINITY,
                    "-0" => -0.0,
                    other => other.parse().expect("number sentinel"),
                });
            }
            let mut decoded = JsObject::new();
            for (key, item) in object.entries() {
                decoded.insert(key.clone(), decode(item));
            }
            JsValue::Object(decoded)
        }
        other => other.clone(),
    }
}

fn wire(value: &JsValue) -> String {
    match stringify_json(value).unwrap() {
        Some(text) => text,
        None => "undefined".into(),
    }
}

fn items(value: &JsValue) -> &[JsValue] {
    match value {
        JsValue::Array(items) => items,
        other => panic!("expected an array, got {other:?}"),
    }
}

static UNDEFINED: JsValue = JsValue::Undefined;

fn arg(case: &JsValue, index: usize) -> &JsValue {
    let args = items(case.get("args").unwrap());
    args.get(index).unwrap_or(&UNDEFINED)
}

fn text_arg(case: &JsValue, index: usize) -> &JsString {
    arg(case, index).as_string().expect("string argument")
}

fn name(case: &JsValue) -> String {
    case.get("fn")
        .unwrap()
        .as_string()
        .unwrap()
        .to_utf8()
        .unwrap()
}

fn object(value: &JsValue) -> JsObject {
    decode(value).as_object().expect("object argument").clone()
}

fn message(value: &JsValue) -> MemoryMessage {
    messages_from_value(&decode(&JsValue::Array(vec![value.clone()])))
        .into_iter()
        .next()
        .expect("message")
}

fn optional_message(value: &JsValue) -> Option<MemoryMessage> {
    let value = decode(value);
    match value {
        JsValue::Undefined | JsValue::Null => None,
        other => Some(message(&other)),
    }
}

fn messages(value: &JsValue) -> Vec<MemoryMessage> {
    messages_from_value(&decode(value))
}

fn sessions(value: &JsValue) -> Vec<L0SessionRecord> {
    items(value)
        .iter()
        .map(|session| {
            let field = |key: &str| {
                session
                    .get(key)
                    .and_then(JsValue::as_string)
                    .map(|value| value.to_utf8().expect("UTF-8 session field"))
                    .unwrap_or_default()
            };
            L0SessionRecord {
                l0_index_id: field("l0IndexId"),
                session_key: field("sessionKey"),
                timestamp: field("timestamp"),
                messages: session
                    .get("messages")
                    .cloned()
                    .unwrap_or(JsValue::Array(Vec::new())),
                source: field("source"),
                indexed: session
                    .get("indexed")
                    .and_then(JsValue::as_bool)
                    .unwrap_or(false),
                created_at: field("createdAt"),
            }
        })
        .collect()
}

fn message_values(values: &[MemoryMessage]) -> JsValue {
    JsValue::Array(values.iter().map(MemoryMessage::to_value).collect())
}

fn focus_value(values: &[(String, Vec<MemoryMessage>)]) -> JsValue {
    JsValue::Array(
        values
            .iter()
            .map(|(id, messages)| {
                let mut entry = JsObject::new();
                entry.insert("l0IndexId".into(), JsValue::String(id.as_str().into()));
                entry.insert("newMessages".into(), message_values(messages));
                JsValue::Object(entry)
            })
            .collect(),
    )
}

fn merge_value(merged: &[MemoryMessage], new_messages: &[MemoryMessage]) -> JsValue {
    let mut result = JsObject::new();
    result.insert("mergedMessages".into(), message_values(merged));
    result.insert("newMessages".into(), message_values(new_messages));
    JsValue::Object(result)
}

fn label_i18n(case: &JsValue, index: usize) -> Option<JsValue> {
    match arg(case, index) {
        JsValue::Undefined => None,
        value => Some(decode(value)),
    }
}

fn call(case: &JsValue, clock_ms: i64) -> JsValue {
    let name = name(case);
    match name.as_str() {
        "sameMessage" => JsValue::Bool(same_message(
            optional_message(arg(case, 0)).as_ref(),
            optional_message(arg(case, 1)).as_ref(),
        )),
        "hasNewContent" => JsValue::Bool(has_new_content(
            &messages(arg(case, 0)),
            &messages(arg(case, 1)),
        )),
        "tokenizeSearchText" => JsValue::Array(
            tokenize_search_text(text_arg(case, 0))
                .into_iter()
                .map(JsValue::String)
                .collect(),
        ),
        "buildGeneralProjectShortlist" => JsValue::Array(build_general_project_shortlist(
            items(arg(case, 0)),
            text_arg(case, 1),
        )),
        "buildCandidateRoutingQuery" => JsValue::String(build_candidate_routing_query(
            &decode(arg(case, 0)),
            &message(arg(case, 1)),
        )),
        "buildCandidateMemoryPreview" => {
            JsValue::String(build_candidate_memory_preview(&decode(arg(case, 0))))
        }
        "commonPrefixLength" => JsValue::Number(common_prefix_length(
            &messages(arg(case, 0)),
            &messages(arg(case, 1)),
        ) as f64),
        "mergeSessionMessages" => {
            let merged = merge_session_messages(&messages(arg(case, 0)), &messages(arg(case, 1)));
            merge_value(&merged.merged_messages, &merged.new_messages)
        }
        "flattenBatchMessages" => message_values(&flatten_batch_messages(
            &sessions(arg(case, 0)),
            &messages(arg(case, 1)),
        )),
        "deriveFocusTurns" => focus_value(&derive_focus_turns(
            &messages(arg(case, 0)),
            &sessions(arg(case, 1)),
        )),
        "buildIndexTraceId" => {
            let ids: Vec<JsString> = items(arg(case, 2))
                .iter()
                .map(|id| id.as_string().unwrap().clone())
                .collect();
            JsValue::String(build_index_trace_id(
                text_arg(case, 0),
                text_arg(case, 1),
                &ids,
            ))
        }
        "normalizeTrigger" => {
            let reason = arg(case, 0).as_string();
            JsValue::String(normalize_trigger(reason).into())
        }
        "previewText" => {
            let max_chars = arg(case, 1).as_number().map_or(220, |value| value as usize);
            JsValue::String(preview_text(text_arg(case, 0), max_chars))
        }
        "inferStorageKind" => {
            let record = decode(arg(case, 0));
            JsValue::String(
                infer_storage_kind(
                    record.get("type").and_then(JsValue::as_string).unwrap(),
                    record
                        .get("relativePath")
                        .and_then(JsValue::as_string)
                        .unwrap(),
                )
                .into(),
            )
        }
        "exposeStoredRelativePath" => {
            let record = decode(arg(case, 0));
            JsValue::String(expose_stored_relative_path(
                record.get("scope").and_then(JsValue::as_string).unwrap(),
                record
                    .get("relativePath")
                    .and_then(JsValue::as_string)
                    .unwrap(),
            ))
        }
        "textDetail" => JsValue::Object(
            text_detail(
                text_arg(case, 0).to_utf8().unwrap().as_str(),
                text_arg(case, 1).to_utf8().unwrap().as_str(),
                text_arg(case, 2),
                label_i18n(case, 3).as_ref(),
            )
            .as_object()
            .unwrap()
            .clone(),
        ),
        "noteDetail" => JsValue::Object(
            note_detail(
                text_arg(case, 0).to_utf8().unwrap().as_str(),
                text_arg(case, 1).to_utf8().unwrap().as_str(),
                text_arg(case, 2),
                label_i18n(case, 3).as_ref(),
            )
            .as_object()
            .unwrap()
            .clone(),
        ),
        "listDetail" => {
            let items: Vec<JsString> = items(arg(case, 2))
                .iter()
                .map(|item| item.as_string().unwrap().clone())
                .collect();
            list_detail(
                text_arg(case, 0).to_utf8().unwrap().as_str(),
                text_arg(case, 1).to_utf8().unwrap().as_str(),
                &items,
                label_i18n(case, 3).as_ref(),
            )
        }
        "kvDetail" => {
            let entries: Vec<JsValue> = items(arg(case, 2)).iter().map(decode).collect();
            kv_detail(
                text_arg(case, 0).to_utf8().unwrap().as_str(),
                text_arg(case, 1).to_utf8().unwrap().as_str(),
                &entries,
                label_i18n(case, 3).as_ref(),
            )
        }
        "jsonDetail" => json_detail(
            text_arg(case, 0).to_utf8().unwrap().as_str(),
            text_arg(case, 1).to_utf8().unwrap().as_str(),
            &decode(arg(case, 2)),
            label_i18n(case, 3).as_ref(),
        ),
        "createStep" => {
            let mut trace = object(arg(case, 0));
            create_step(
                &mut trace,
                text_arg(case, 1).to_utf8().unwrap().as_str(),
                text_arg(case, 2),
                text_arg(case, 3).to_utf8().unwrap().as_str(),
                text_arg(case, 4),
                text_arg(case, 5),
                &object(arg(case, 6)),
            );
            JsValue::Object(trace)
        }
        "createStepSequence" => {
            let mut trace = object(arg(case, 0));
            for step in items(arg(case, 1)) {
                let step = items(step);
                create_step(
                    &mut trace,
                    step[0].as_string().unwrap().to_utf8().unwrap().as_str(),
                    step[1].as_string().unwrap(),
                    step[2].as_string().unwrap().to_utf8().unwrap().as_str(),
                    step[3].as_string().unwrap(),
                    step[4].as_string().unwrap(),
                    step[5].as_object().unwrap(),
                );
            }
            JsValue::Object(trace)
        }
        "createBatchTrace" => JsValue::Object(create_batch_trace(
            text_arg(case, 0),
            &sessions(arg(case, 1)),
            text_arg(case, 2).to_utf8().unwrap().as_str(),
            arg(case, 3).as_number().unwrap() as usize,
            &JsString::from(now_iso_at(clock_ms)),
        )),
        other => panic!("oracle has an unknown function {other}"),
    }
}

#[test]
fn heartbeat_helpers_match_ts() {
    let oracle = oracle();
    let clock_ms = oracle.get("clockMs").unwrap().as_number().unwrap() as i64;
    let mut failures = Vec::new();
    for case in items(oracle.get("cases").unwrap()) {
        let actual = call(case, clock_ms);
        let expected = decode(case.get("expected").unwrap());
        if wire(&actual) != wire(&expected) {
            failures.push(format!(
                "{}/{}: expected {} got {}",
                case.get("group")
                    .unwrap()
                    .as_string()
                    .unwrap()
                    .to_utf8()
                    .unwrap(),
                name(case),
                wire(&expected),
                wire(&actual)
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} heartbeat cases differ:\n{}",
        failures.len(),
        items(oracle.get("cases").unwrap()).len(),
        failures.join("\n")
    );
}

#[ignore]
#[test]
fn regenerates_identically() {
    baseline::regenerates_identically("edgeclaw-heartbeat");
}
