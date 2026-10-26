//! Real TS oracle for A6.6-edgeclaw-4 transcript normalization.
use pilotdeck_gateway::runtime::memory::edgeclaw::messages::{
    CaptureStrategy, MemoryMessage, NormalizeOptions, collect_sender_hints, compact_whitespace,
    count_plugin_state_markers, extract_current_message_segment, extract_text_from_content_value,
    extract_text_from_object_value, has_tool_call_content, inspect_transcript_message,
    is_command_only_user_text, is_session_boundary_marker_message, is_session_startup_marker_text,
    line_looks_like_plugin_noise, looks_like_opaque_sender_label, normalize_messages,
    normalize_plugin_noise_probe, normalize_sender_hint, normalize_transcript_message,
    paragraph_looks_like_plugin_noise, parse_bang_command, parse_leading_slash_command,
    should_skip_assistant_message, should_skip_user_message, starts_with_plugin_state_line,
    strip_assistant_thinking, strip_attachment_scaffolding, strip_injected_memory_context,
    strip_leading_current_message_envelope, strip_leading_mentions,
    strip_leading_message_envelope_lines, strip_leading_sender_prefix,
    strip_leading_timestamp_prefix, strip_plugin_state_scaffolding, strip_quoted_context_blocks,
    strip_untrusted_sender_metadata, strip_user_noise,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};

#[path = "common/baseline.rs"]
mod baseline;
const ORACLE: &str = include_str!("fixtures/edgeclaw-messages/oracle.json");

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
    stringify_json(value)
        .unwrap()
        .unwrap_or_else(|| "undefined".into())
}

fn items(value: &JsValue) -> &[JsValue] {
    match value {
        JsValue::Array(items) => items,
        other => panic!("expected an array, got {other:?}"),
    }
}

fn arg(case: &JsValue, index: usize) -> &JsValue {
    &items(case.get("args").unwrap())[index]
}

fn text(case: &JsValue, index: usize) -> &JsString {
    arg(case, index).as_string().expect("string argument")
}

fn boolean(case: &JsValue, index: usize) -> bool {
    arg(case, index).as_bool().expect("boolean argument")
}

fn number(case: &JsValue, index: usize) -> f64 {
    arg(case, index).as_number().expect("number argument")
}

fn option_value(value: Option<JsString>) -> JsValue {
    value.map_or(JsValue::Undefined, JsValue::String)
}

fn message_value(value: Option<MemoryMessage>) -> JsValue {
    value.map_or(JsValue::Undefined, |message| message.to_value())
}

fn options(case: &JsValue, index: usize) -> (bool, f64, CaptureStrategy) {
    let options = arg(case, index).as_object().expect("options object");
    let include = options
        .get("includeAssistant")
        .and_then(JsValue::as_bool)
        .expect("includeAssistant");
    let max = options
        .get("maxMessageChars")
        .and_then(JsValue::as_number)
        .expect("maxMessageChars");
    let strategy = match options
        .get("captureStrategy")
        .and_then(JsValue::as_string)
        .and_then(|value| value.to_utf8().ok())
        .as_deref()
    {
        Some("full_session") => CaptureStrategy::FullSession,
        _ => CaptureStrategy::LastTurn,
    };
    (include, max, strategy)
}

fn call(case: &JsValue) -> JsValue {
    let name = case
        .get("fn")
        .unwrap()
        .as_string()
        .unwrap()
        .to_utf8()
        .unwrap();
    match name.as_str() {
        "extractTextFromContent" => JsValue::String(extract_text_from_content_value(arg(case, 0))),
        "extractTextFromObject" => JsValue::String(extract_text_from_object_value(
            arg(case, 0),
            number(case, 1) as usize,
        )),
        "stripInjectedMemoryContext" => {
            JsValue::String(strip_injected_memory_context(text(case, 0)))
        }
        "compactWhitespace" => JsValue::String(compact_whitespace(text(case, 0))),
        "normalizePluginNoiseProbe" => JsValue::String(normalize_plugin_noise_probe(text(case, 0))),
        "countPluginStateMarkers" => {
            JsValue::Number(count_plugin_state_markers(text(case, 0)) as f64)
        }
        "startsWithPluginStateLine" => JsValue::Bool(starts_with_plugin_state_line(text(case, 0))),
        "lineLooksLikePluginNoise" => JsValue::Bool(line_looks_like_plugin_noise(text(case, 0))),
        "paragraphLooksLikePluginNoise" => {
            JsValue::Bool(paragraph_looks_like_plugin_noise(text(case, 0)))
        }
        "stripPluginStateScaffolding" => {
            JsValue::String(strip_plugin_state_scaffolding(text(case, 0)))
        }
        "normalizeSenderHint" => JsValue::String(normalize_sender_hint(text(case, 0))),
        "collectSenderHints" => JsValue::Array(
            collect_sender_hints(text(case, 0))
                .into_iter()
                .map(JsValue::String)
                .collect(),
        ),
        "stripUntrustedSenderMetadata" => {
            JsValue::String(strip_untrusted_sender_metadata(text(case, 0)))
        }
        "stripQuotedContextBlocks" => JsValue::String(strip_quoted_context_blocks(text(case, 0))),
        "extractCurrentMessageSegment" => {
            JsValue::String(extract_current_message_segment(text(case, 0)))
        }
        "stripLeadingTimestampPrefix" => {
            JsValue::String(strip_leading_timestamp_prefix(text(case, 0)))
        }
        "stripAttachmentScaffolding" => {
            JsValue::String(strip_attachment_scaffolding(text(case, 0)))
        }
        "stripLeadingCurrentMessageEnvelope" => {
            JsValue::String(strip_leading_current_message_envelope(text(case, 0)))
        }
        "looksLikeOpaqueSenderLabel" => {
            JsValue::Bool(looks_like_opaque_sender_label(text(case, 0)))
        }
        "stripLeadingSenderPrefix" => {
            let hints: Vec<JsString> = items(arg(case, 1))
                .iter()
                .map(|hint| hint.as_string().unwrap().clone())
                .collect();
            JsValue::String(strip_leading_sender_prefix(text(case, 0), &hints))
        }
        "stripLeadingMentions" => JsValue::String(strip_leading_mentions(text(case, 0))),
        "stripLeadingMessageEnvelopeLines" => {
            JsValue::String(strip_leading_message_envelope_lines(text(case, 0)))
        }
        "stripUserNoise" => JsValue::String(strip_user_noise(text(case, 0))),
        "stripAssistantThinking" => JsValue::String(strip_assistant_thinking(text(case, 0))),
        "hasToolCallContent" => JsValue::Bool(has_tool_call_content(arg(case, 0))),
        "shouldSkipUserMessage" => JsValue::Bool(should_skip_user_message(text(case, 0))),
        "shouldSkipAssistantMessage" => {
            JsValue::Bool(should_skip_assistant_message(arg(case, 0), text(case, 1)))
        }
        "parseLeadingSlashCommand" => option_value(parse_leading_slash_command(text(case, 0))),
        "parseBangCommand" => option_value(parse_bang_command(text(case, 0))),
        "isSessionStartupMarkerText" => {
            JsValue::Bool(is_session_startup_marker_text(text(case, 0)))
        }
        "isCommandOnlyUserText" => JsValue::Bool(is_command_only_user_text(text(case, 0))),
        "inspectTranscriptMessage" => {
            let info = inspect_transcript_message(arg(case, 0));
            let mut object = JsObject::new();
            object.insert(
                "role".into(),
                info.role.map_or(JsValue::Undefined, |role| {
                    JsValue::String(role.as_str().into())
                }),
            );
            object.insert("content".into(), JsValue::String(info.content));
            object.insert("hasToolCalls".into(), JsValue::Bool(info.has_tool_calls));
            JsValue::Object(object)
        }
        "isSessionBoundaryMarkerMessage" => {
            JsValue::Bool(is_session_boundary_marker_message(arg(case, 0)))
        }
        "normalizeSingleMessage" => {
            let (include, max, _) = options(case, 1);
            message_value(normalize_transcript_message(arg(case, 0), include, max))
        }
        "normalizeTranscriptMessage" => message_value(normalize_transcript_message(
            arg(case, 0),
            boolean(case, 1),
            number(case, 2),
        )),
        "normalizeMessages" => {
            let (include, max, strategy) = options(case, 1);
            let messages = normalize_messages(
                items(arg(case, 0)),
                &NormalizeOptions {
                    include_assistant: include,
                    max_message_chars: max,
                    capture_strategy: strategy,
                },
            );
            JsValue::Array(messages.iter().map(MemoryMessage::to_value).collect())
        }
        other => panic!("missing dispatch for {other}"),
    }
}

fn replay(groups: &[&str]) {
    let oracle = oracle();
    let mut seen = 0usize;
    for case in items(oracle.get("cases").unwrap()) {
        let group = case
            .get("group")
            .unwrap()
            .as_string()
            .unwrap()
            .to_utf8()
            .unwrap();
        if !groups.contains(&group.as_str()) {
            continue;
        }
        seen += 1;
        let decoded = decode(case);
        let actual = call(&decoded);
        let expected = decode(decoded.get("expected").unwrap());
        assert_eq!(
            wire(&actual),
            wire(&expected),
            "{group}/{:?} args={:?}",
            decoded.get("fn").unwrap(),
            decoded.get("args").unwrap()
        );
    }
    assert!(seen > 0, "no cases for {groups:?}");
}

#[test]
fn extraction_matches_ts() {
    replay(&["extract", "injected", "whitespace", "probe"]);
}

#[test]
fn user_strip_matches_ts() {
    replay(&[
        "quoted",
        "timestamp",
        "attachment",
        "envelope",
        "mentions",
        "envelope-lines",
        "user",
    ]);
}

#[test]
fn plugin_matches_ts() {
    replay(&["plugin"]);
}

#[test]
fn sender_matches_ts() {
    replay(&["sender", "sender-prefix", "opaque"]);
}

#[test]
fn assistant_matches_ts() {
    replay(&["thinking", "skip-assistant"]);
}

#[test]
fn commands_match_ts() {
    replay(&["commands", "skip-user"]);
}

#[test]
fn tools_match_ts() {
    replay(&["tools"]);
}

#[test]
fn inspect_matches_ts() {
    replay(&["inspect"]);
}

#[test]
fn normalize_matches_ts() {
    replay(&["normalize", "normalize-one"]);
}

#[test]
fn case_fold_matches_ts() {
    replay(&["case-fold"]);
}

#[test]
fn code_unit_edges_match_ts() {
    replay(&["edge"]);
}

#[test]
#[ignore = "requires fixed PDRS_BASELINE with Node 22 and tsx"]
fn fixture_regenerates_identically() {
    baseline::regenerates_identically("edgeclaw-messages");
}
