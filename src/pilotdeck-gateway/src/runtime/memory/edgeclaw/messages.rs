//! Transcript normalization for EdgeClaw memory capture.
//!
//! Ports `src/context/memory/edgeclaw-memory-core/src/message-utils.ts` at the
//! fixed TS baseline cc7c4e6: `normalizeMessages`,
//! `normalizeTranscriptMessage`, `inspectTranscriptMessage`,
//! `isSessionBoundaryMarkerMessage` and every private `strip*` helper.
//!
//! JavaScript regexes run on UTF-16 code units. To keep that unit count and
//! unpaired surrogates, [`encode`] maps each unit to one Rust `char` (normal
//! BMP units as themselves, surrogates into the supplementary private-use
//! area), regexes run on that lossless projection, and [`decode`] maps back.
//! ECMAScript `\s` is spelled out because Rust's Unicode `\s` omits U+FEFF.

use crate::{
    model_catalog::lower,
    runtime::memory::{js_is_space_unit, js_trim_units},
};
use icu_locale_core::LanguageIdentifier;
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json};
use regex::Regex;
use std::sync::OnceLock;

const SPACE_BODY: &str =
    r"\t-\r \u{00A0}\u{1680}\u{2000}-\u{200A}\u{2028}\u{2029}\u{202F}\u{205F}\u{3000}\u{FEFF}";
const S: &str =
    r"[\t-\r \u{00A0}\u{1680}\u{2000}-\u{200A}\u{2028}\u{2029}\u{202F}\u{205F}\u{3000}\u{FEFF}]";
const MEMORY_HEADERS: [&str; 2] = [
    "You are using retrieved ClawXMemory file memories for this turn.",
    "You are using multi-level memory indexes for this turn.",
];
const MEMORY_FOOTER: &str = "Treat the above as authoritative prior memory when it is relevant. Prioritize the user's latest request, and do not claim memory is missing or that this is a fresh conversation if the answer is already shown above.";
const RECALL_HEADER: &str = "## ClawXMemory Recall";
const RECALL_INSTRUCTION: &str = "Use the following retrieved ClawXMemory evidence for this turn.";
const RECALL_FOOTER: &str = "Treat the selected evidence above as authoritative historical memory for this turn when it is relevant.";
const RECALL_FINAL: &str = "If the needed answer is already shown above, do not claim that memory is missing or that this is a fresh conversation.";
pub const SESSION_START_PREFIX: &str = "A new session was started via /new or /reset.";
const SLUG_PREFIX: &str = "Based on this conversation, generate a short 1-2 word filename slug";
const MAX_DEPTH: usize = 5;
const SLASH_COMMANDS: &[&str] = &[
    "help",
    "commands",
    "skill",
    "status",
    "allowlist",
    "approve",
    "context",
    "whoami",
    "id",
    "subagents",
    "config",
    "debug",
    "usage",
    "tts",
    "voice",
    "stop",
    "restart",
    "dock-telegram",
    "dock_telegram",
    "dock-discord",
    "dock_discord",
    "dock-slack",
    "dock_slack",
    "activation",
    "send",
    "reset",
    "new",
    "think",
    "thinking",
    "t",
    "verbose",
    "v",
    "reasoning",
    "reason",
    "elevated",
    "elev",
    "exec",
    "model",
    "models",
    "queue",
    "bash",
    "compact",
];
const BANG_COMMANDS: &[&str] = &["poll", "stop"];

// -- UTF-16 <-> regex projection -------------------------------------------

const SURROGATE_BASE: u32 = 0xF0000;

fn encode(value: &JsString) -> String {
    value
        .units()
        .iter()
        .map(|&unit| {
            let point = if (0xD800..=0xDFFF).contains(&unit) {
                SURROGATE_BASE + u32::from(unit - 0xD800)
            } else {
                u32::from(unit)
            };
            char::from_u32(point).expect("BMP unit or private-use surrogate image")
        })
        .collect()
}

fn decode(value: &str) -> JsString {
    let mut units = Vec::with_capacity(value.len());
    for character in value.chars() {
        let point = character as u32;
        if (SURROGATE_BASE..SURROGATE_BASE + 0x800).contains(&point) {
            units.push(0xD800 + (point - SURROGATE_BASE) as u16);
        } else {
            let mut buffer = [0u16; 2];
            units.extend_from_slice(character.encode_utf16(&mut buffer));
        }
    }
    JsString::from_utf16(units)
}

fn rx(pattern: &str) -> Regex {
    Regex::new(&pattern.replace("$C", SPACE_BODY).replace("$S", S))
        .expect("static EdgeClaw message regex")
}

macro_rules! lazy_regex {
    ($name:ident, $pattern:expr) => {
        fn $name() -> &'static Regex {
            static RE: OnceLock<Regex> = OnceLock::new();
            RE.get_or_init(|| rx($pattern))
        }
    };
}

fn re_replace(text: &JsString, re: &Regex, replacement: &str) -> JsString {
    decode(&re.replace_all(&encode(text), replacement))
}

fn re_replace_first(text: &JsString, re: &Regex, replacement: &str) -> JsString {
    decode(&re.replace(&encode(text), replacement))
}

fn re_split(text: &JsString, re: &Regex) -> Vec<JsString> {
    re.split(&encode(text)).map(decode).collect()
}

fn lower_text(value: &JsString) -> JsString {
    lower(value, &LanguageIdentifier::UNKNOWN, false)
}

fn units_of(value: &str) -> Vec<u16> {
    value.encode_utf16().collect()
}

fn starts_with_str(value: &JsString, needle: &str) -> bool {
    value.units().starts_with(&units_of(needle))
}

fn contains_str(value: &JsString, needle: &str) -> bool {
    let needle = units_of(needle);
    needle.is_empty()
        || value
            .units()
            .windows(needle.len())
            .any(|part| part == needle)
}

fn trim_js(value: &JsString) -> JsString {
    JsString::from_utf16(js_trim_units(value.units()).to_vec())
}

fn trim_start_js(value: &JsString) -> JsString {
    let start = value
        .units()
        .iter()
        .position(|unit| !js_is_space_unit(*unit))
        .unwrap_or(value.len_utf16());
    JsString::from_utf16(value.units()[start..].to_vec())
}

fn trim_lines(value: &[u16]) -> Vec<&[u16]> {
    value.split(|unit| *unit == u16::from(b'\n')).collect()
}

fn join_lines(lines: &[&[u16]]) -> JsString {
    let mut units = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        if index > 0 {
            units.push(u16::from(b'\n'));
        }
        units.extend(*line);
    }
    JsString::from_utf16(units)
}

fn join_text(parts: &[JsString], separator: &str) -> JsString {
    let mut units = Vec::new();
    for (index, part) in parts.iter().enumerate() {
        if index > 0 {
            units.extend(separator.encode_utf16());
        }
        units.extend(part.units());
    }
    JsString::from_utf16(units)
}

fn fold_upper(unit: u16) -> Option<char> {
    let character = char::from_u32(u32::from(unit))?;
    let mut upper = character.to_uppercase();
    let first = upper.next()?;
    upper.next().is_none().then_some(first)
}

fn ci_eq(left: u16, right: u16) -> bool {
    fold_upper(left).is_some_and(|upper| fold_upper(right) == Some(upper))
}

fn starts_with_ci(value: &[u16], at: usize, needle: &str) -> bool {
    let needle = units_of(needle);
    value
        .get(at..at + needle.len())
        .is_some_and(|slice| slice.iter().zip(&needle).all(|(a, b)| ci_eq(*a, *b)))
}

fn find_ci(value: &[u16], needle: &str, from: usize) -> Option<usize> {
    let needle = units_of(needle);
    if value.len() < needle.len() {
        return None;
    }
    (from..=value.len() - needle.len()).find(|&index| {
        value[index..index + needle.len()]
            .iter()
            .zip(&needle)
            .all(|(a, b)| ci_eq(*a, *b))
    })
}

// -- text extraction --------------------------------------------------------

fn push_unique(parts: &mut Vec<JsString>, value: &JsString) {
    let value = trim_js(value);
    if !value.is_empty() && !parts.contains(&value) {
        parts.push(value);
    }
}

fn extract_text_from_content(content: &JsValue, depth: usize) -> JsString {
    if depth > MAX_DEPTH {
        return JsString::default();
    }
    match content {
        JsValue::String(text) => {
            crate::runtime::memory::edgeclaw::text::decode_escaped_unicode_text(text, false)
        }
        JsValue::Array(items) => {
            let mut blocks = Vec::new();
            for item in items {
                push_unique(&mut blocks, &extract_text_from_content(item, depth + 1));
            }
            join_text(&blocks, "\n")
        }
        JsValue::Object(object) => extract_text_from_object(object, depth),
        _ => JsString::default(),
    }
}

fn equals_any(value: &JsString, list: &[&str]) -> bool {
    list.iter().any(|candidate| value.equals_str(candidate))
}

fn extract_text_from_object(content: &JsObject, depth: usize) -> JsString {
    let kind = content
        .get("type")
        .and_then(JsValue::as_string)
        .map(lower_text);
    if kind.as_ref().is_some_and(|kind| {
        equals_any(
            kind,
            &[
                "toolcall",
                "toolresult",
                "image",
                "input_image",
                "file",
                "media",
            ],
        )
    }) {
        return JsString::default();
    }
    let text = content.get("text").and_then(JsValue::as_string);
    if let Some(text) = text
        && kind.as_ref().is_some_and(|kind| {
            equals_any(
                kind,
                &[
                    "text",
                    "input_text",
                    "output_text",
                    "paragraph",
                    "message_text",
                ],
            )
        })
    {
        return crate::runtime::memory::edgeclaw::text::decode_escaped_unicode_text(text, false);
    }
    let mut parts = Vec::new();
    if let Some(text) = text {
        push_unique(
            &mut parts,
            &crate::runtime::memory::edgeclaw::text::decode_escaped_unicode_text(text, false),
        );
    }
    for key in [
        "content", "body", "message", "caption", "parts", "items", "blocks", "segments", "chunks",
    ] {
        push_unique(
            &mut parts,
            &extract_text_from_content(content.get(key).unwrap_or(&UNDEFINED), depth + 1),
        );
    }
    trim_js(&join_text(&parts, "\n"))
}

pub fn extract_text_from_content_value(content: &JsValue) -> JsString {
    extract_text_from_content(content, 0)
}

pub fn extract_text_from_object_value(content: &JsValue, depth: usize) -> JsString {
    content
        .as_object()
        .map_or_else(JsString::default, |object| {
            extract_text_from_object(object, depth)
        })
}

// -- injected memory / recall context --------------------------------------

pub fn strip_injected_memory_context(text: &JsString) -> JsString {
    let mut cleaned = trim_js(text);
    for (index, pattern) in memory_context_patterns().iter().enumerate() {
        if contains_str(&cleaned, MEMORY_HEADERS[index]) {
            cleaned = trim_js(&re_replace(&cleaned, pattern, ""));
        }
    }
    if contains_str(&cleaned, RECALL_HEADER) && contains_str(&cleaned, RECALL_INSTRUCTION) {
        cleaned = trim_js(&re_replace(&cleaned, recall_context_pattern(), ""));
        let without_cr: Vec<u16> = cleaned
            .units()
            .iter()
            .copied()
            .filter(|unit| *unit != u16::from(b'\r'))
            .collect();
        let lines = trim_lines(&without_cr);
        let mut start = 0;
        while start < lines.len() {
            let value = js_trim_units(lines[start]);
            let line = JsString::from_utf16(value.to_vec());
            if value.is_empty() || system_prefix_pattern().is_match(&encode(&line)) {
                start += 1;
            } else {
                break;
            }
        }
        cleaned = trim_js(&join_lines(&lines[start..]));
    }
    cleaned
}

fn memory_context_patterns() -> &'static [Regex; 2] {
    static RE: OnceLock<[Regex; 2]> = OnceLock::new();
    RE.get_or_init(|| {
        let footer = regex::escape(MEMORY_FOOTER);
        MEMORY_HEADERS.map(|header| rx(&format!(r"(?s){}.*?{}{S}*", regex::escape(header), footer)))
    })
}

fn recall_context_pattern() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        rx(&format!(
            r"(?s){}{S}*{}.*?{}{S}*{}{S}*",
            regex::escape(RECALL_HEADER),
            regex::escape(RECALL_INSTRUCTION),
            regex::escape(RECALL_FOOTER),
            regex::escape(RECALL_FINAL)
        ))
    })
}

lazy_regex!(system_prefix_pattern, r"(?i)^System:");

// -- whitespace and plugin scaffolding -------------------------------------

pub fn compact_whitespace(text: &JsString) -> JsString {
    let without_cr: Vec<u16> = text
        .units()
        .iter()
        .copied()
        .filter(|unit| *unit != u16::from(b'\r'))
        .collect();
    let mut compact: Vec<&[u16]> = Vec::new();
    let mut previous_empty = false;
    for line in trim_lines(&without_cr) {
        let end = line
            .iter()
            .rposition(|unit| !js_is_space_unit(*unit))
            .map_or(0, |index| index + 1);
        let line = &line[..end];
        let empty = js_trim_units(line).is_empty();
        if !(empty && previous_empty) {
            compact.push(line);
            previous_empty = empty;
        }
    }
    trim_js(&join_lines(&compact))
}

pub fn normalize_plugin_noise_probe(text: &JsString) -> JsString {
    let mut value = encode(text).replace('\r', "");
    value = value.replace("**", "").replace('`', "");
    value = value.replace(['\u{2028}', '\u{2029}'], "\n");
    value = probe_marker_pattern().replace_all(&value, "").to_string();
    value = probe_space_pattern().replace_all(&value, " ").to_string();
    trim_js(&decode(&value))
}

lazy_regex!(probe_marker_pattern, r"(?m)^[>\-*$C]+");
lazy_regex!(probe_space_pattern, r"(?s)$S+");
lazy_regex!(
    marker_pattern,
    r"(?i)(?-u:\b)project state maintained by(?-u:\b)"
);
lazy_regex!(session_pattern, r"(?i)(?-u:\b)session status(?-u:\b)");
lazy_regex!(
    git_status_pattern,
    r"(?i)(?-u:\b)git status(?: summary)?$S*:"
);
lazy_regex!(
    start_project_pattern,
    r"(?i)^project state maintained by(?-u:\b)"
);
lazy_regex!(start_session_pattern, r"(?i)^session status(?-u:\b)");
lazy_regex!(start_branch_pattern, r"(?i)^current git branch$S*:");
lazy_regex!(start_git_pattern, r"(?i)^git status(?: summary)?$S*:");

fn marker_word_patterns() -> &'static [Regex] {
    static RE: OnceLock<Vec<Regex>> = OnceLock::new();
    RE.get_or_init(|| {
        [
            "agent",
            "host",
            "workspace",
            "os",
            "node",
            "model",
            "current git branch",
        ]
        .iter()
        .map(|word| rx(&format!(r"(?i)(?-u:\b){word}$S*:")))
        .collect()
    })
}

pub fn count_plugin_state_markers(text: &JsString) -> usize {
    let encoded = encode(&normalize_plugin_noise_probe(text));
    let count = usize::from(marker_pattern().is_match(&encoded))
        + usize::from(session_pattern().is_match(&encoded))
        + usize::from(git_status_pattern().is_match(&encoded));
    count
        + marker_word_patterns()
            .iter()
            .filter(|pattern| pattern.is_match(&encoded))
            .count()
}

pub fn starts_with_plugin_state_line(text: &JsString) -> bool {
    let encoded = encode(&normalize_plugin_noise_probe(text));
    start_project_pattern().is_match(&encoded)
        || start_session_pattern().is_match(&encoded)
        || start_branch_pattern().is_match(&encoded)
        || start_git_pattern().is_match(&encoded)
}

pub fn line_looks_like_plugin_noise(text: &JsString) -> bool {
    let normalized = normalize_plugin_noise_probe(text);
    !normalized.is_empty()
        && (starts_with_plugin_state_line(&normalized)
            || count_plugin_state_markers(&normalized) >= 4)
}

pub fn paragraph_looks_like_plugin_noise(text: &JsString) -> bool {
    let normalized = normalize_plugin_noise_probe(text);
    if normalized.is_empty() {
        return false;
    }
    if starts_with_plugin_state_line(&normalized) {
        return true;
    }
    let units = text.units().to_vec();
    let lines: Vec<JsString> = trim_lines(&units)
        .into_iter()
        .map(|line| normalize_plugin_noise_probe(&JsString::from_utf16(line.to_vec())))
        .filter(|line| !line.is_empty())
        .collect();
    let noisy = lines
        .iter()
        .filter(|line| starts_with_plugin_state_line(line) || count_plugin_state_markers(line) >= 3)
        .count();
    let markers = count_plugin_state_markers(&normalized);
    if paragraph_session_pattern().is_match(&encode(&normalized)) && markers >= 2 {
        return true;
    }
    markers >= 4 && noisy >= lines.len().div_ceil(2).max(1)
}

pub fn strip_plugin_state_scaffolding(text: &JsString) -> JsString {
    let normalized = compact_whitespace(text);
    if normalized.is_empty() {
        return normalized;
    }
    let mut paragraphs: Vec<JsString> = re_split(&normalized, paragraph_pattern())
        .iter()
        .map(trim_js)
        .filter(|paragraph| !paragraph.is_empty())
        .collect();
    while paragraphs
        .first()
        .is_some_and(paragraph_looks_like_plugin_noise)
    {
        paragraphs.remove(0);
    }
    let mut cleaned = trim_js(&join_text(&paragraphs, "\n\n"));
    if cleaned.is_empty() {
        return cleaned;
    }
    let units = cleaned.units().to_vec();
    let lines = trim_lines(&units);
    let mut start = 0;
    while start < lines.len()
        && line_looks_like_plugin_noise(&JsString::from_utf16(lines[start].to_vec()))
    {
        start += 1;
    }
    cleaned = compact_whitespace(&join_lines(&lines[start..]));
    if cleaned.is_empty() {
        return cleaned;
    }
    if paragraph_looks_like_plugin_noise(&cleaned) {
        JsString::default()
    } else {
        cleaned
    }
}

lazy_regex!(paragraph_pattern, r"\n$S*\n");
lazy_regex!(paragraph_session_pattern, r"(?i)session status");

// -- sender metadata / quoted context --------------------------------------

pub fn normalize_sender_hint(value: &JsString) -> JsString {
    let mut out = Vec::new();
    let mut in_space = false;
    for unit in lower_text(value).units().iter().copied() {
        if js_is_space_unit(unit) {
            in_space = true;
        } else {
            if in_space && !out.is_empty() {
                out.push(u16::from(b' '));
            }
            in_space = false;
            out.push(unit);
        }
    }
    JsString::from_utf16(out)
}

fn json_text(value: &JsString) -> String {
    let mut text = String::new();
    for unit in char::decode_utf16(value.units().iter().copied()) {
        match unit {
            Ok(character) => text.push(character),
            Err(error) => text.push_str(&format!("\\u{:04x}", error.unpaired_surrogate())),
        }
    }
    text
}

fn add_sender_hints(raw: &str, hints: &mut Vec<JsString>) {
    let text = json_text(&decode(raw));
    let Ok(parsed) = parse_json(&text) else {
        return;
    };
    let Some(object) = parsed.as_object() else {
        return;
    };
    for key in ["sender", "sender_id", "sender_label"] {
        if let Some(value) = object.get(key).and_then(JsValue::as_string) {
            let hint = normalize_sender_hint(value);
            if !hint.is_empty() && !hints.contains(&hint) {
                hints.push(hint);
            }
        }
    }
}

pub fn collect_sender_hints(text: &JsString) -> Vec<JsString> {
    let encoded = encode(text);
    let mut hints = Vec::new();
    for capture in sender_block_pattern().captures_iter(&encoded) {
        add_sender_hints(&capture[1], &mut hints);
    }
    for capture in sender_inline_pattern().captures_iter(&encoded) {
        add_sender_hints(&capture[1], &mut hints);
    }
    hints
}

pub fn strip_untrusted_sender_metadata(text: &JsString) -> JsString {
    let text = re_replace(text, sender_block_strip_pattern(), "");
    re_replace(&text, sender_inline_strip_pattern(), "")
}

pub fn strip_quoted_context_blocks(text: &JsString) -> JsString {
    let text = re_replace(text, quoted_pattern(), "");
    strip_chat_context_segments(&text)
}

fn strip_chat_context_segments(text: &JsString) -> JsString {
    let value = text.units();
    let marker = "[Chat messages since your last reply - for context]";
    let current = "[Current message - respond to this]";
    let mut out = Vec::new();
    let mut copied = 0;
    let mut pos = 0;
    while let Some(start) = find_ci(value, marker, pos) {
        out.extend(&value[copied..start]);
        let end = find_ci(value, current, start + marker.len()).unwrap_or(value.len());
        copied = end;
        pos = end;
        if end == value.len() {
            break;
        }
    }
    out.extend(&value[copied..]);
    JsString::from_utf16(out)
}

pub fn extract_current_message_segment(text: &JsString) -> JsString {
    let marker = units_of("[Current message - respond to this]");
    let value = text.units();
    if value.len() < marker.len() {
        return text.clone();
    }
    let Some(index) = value.windows(marker.len()).rposition(|part| part == marker) else {
        return text.clone();
    };
    trim_js(&JsString::from_utf16(
        value[index + marker.len()..].to_vec(),
    ))
}

pub fn strip_leading_timestamp_prefix(text: &JsString) -> JsString {
    re_replace_first(text, timestamp_pattern(), "")
}

fn is_line_terminator(unit: u16) -> bool {
    matches!(unit, 0x0A | 0x0D | 0x2028 | 0x2029)
}

fn skip_js_space(value: &[u16], mut index: usize) -> usize {
    while index < value.len() && js_is_space_unit(value[index]) {
        index += 1;
    }
    index
}

fn strip_media_attached(value: &[u16]) -> Vec<u16> {
    let mut out = Vec::new();
    let mut copied = 0;
    let mut index = 0;
    while index < value.len() {
        let line_start = index == 0 || is_line_terminator(value[index - 1]);
        if line_start && starts_with_ci(value, index, "[media attached:") {
            let line_end = value[index..]
                .iter()
                .position(|unit| *unit == u16::from(b'\n'))
                .map_or(value.len(), |offset| index + offset);
            if let Some(close) = value[index..line_end]
                .iter()
                .rposition(|unit| *unit == u16::from(b']'))
                .map(|offset| index + offset)
            {
                out.extend(&value[copied..index]);
                copied = skip_js_space(value, close + 1);
                index = copied;
                continue;
            }
        }
        index += 1;
    }
    out.extend(&value[copied..]);
    out
}

fn strip_send_image_lines(value: &[u16]) -> Vec<u16> {
    let mut out = Vec::new();
    let mut copied = 0;
    let mut index = 0;
    while index < value.len() {
        let line_start = index == 0 || is_line_terminator(value[index - 1]);
        if line_start && starts_with_ci(value, index, "To send an image back,") {
            out.extend(&value[copied..index]);
            let line_end = value[index..]
                .iter()
                .position(|unit| *unit == u16::from(b'\n'))
                .map_or(value.len(), |offset| index + offset);
            copied = line_end;
            index = line_end + usize::from(line_end < value.len());
            continue;
        }
        index += 1;
    }
    out.extend(&value[copied..]);
    out
}

pub fn strip_attachment_scaffolding(text: &JsString) -> JsString {
    let value = strip_send_image_lines(&strip_media_attached(text.units()));
    let mut encoded = encode(&JsString::from_utf16(value));
    if let Some(found) = file_boundary_pattern().find(&encoded) {
        encoded.truncate(found.start());
    }
    decode(&file_tag_pattern().replace_all(&encoded, ""))
}

pub fn strip_leading_current_message_envelope(text: &JsString) -> JsString {
    let encoded = encode(text);
    let Some(capture) = current_envelope_pattern().captures(&encoded) else {
        return text.clone();
    };
    trim_js(&decode(&capture[2]))
}

pub fn looks_like_opaque_sender_label(label: &JsString) -> bool {
    let value = trim_js(label);
    let encoded = encode(&value);
    !value.is_empty()
        && value.len_utf16() <= 120
        && opaque_label_pattern().is_match(&encoded)
        && (opaque_digit_pattern().is_match(&encoded) || opaque_word_pattern().is_match(&encoded))
}

pub fn strip_leading_sender_prefix(text: &JsString, hints: &[JsString]) -> JsString {
    let units = text.units().to_vec();
    let lines = trim_lines(&units);
    let first = trim_js(&JsString::from_utf16(lines[0].to_vec()));
    let encoded = encode(&first);
    let Some(capture) = sender_prefix_pattern().captures(&encoded) else {
        return text.clone();
    };
    let raw_label = decode(&capture[1]);
    let label = normalize_sender_hint(&raw_label);
    if !hints.contains(&label) && !looks_like_opaque_sender_label(&raw_label) {
        return text.clone();
    }
    let mut parts = vec![decode(&capture[2])];
    parts.extend(
        lines[1..]
            .iter()
            .map(|line| JsString::from_utf16(line.to_vec())),
    );
    trim_js(&join_text(&parts, "\n"))
}

pub fn strip_leading_mentions(text: &JsString) -> JsString {
    let mut cleaned = trim_start_js(text);
    loop {
        let encoded = encode(&cleaned);
        let next = mention_pattern().replace(&encoded, "").to_string();
        if next == encoded {
            return cleaned;
        }
        cleaned = trim_start_js(&decode(&next));
    }
}

pub fn strip_leading_message_envelope_lines(text: &JsString) -> JsString {
    let units = text.units().to_vec();
    let lines = trim_lines(&units);
    let mut start = 0;
    while start < lines.len() && is_noise_line(&JsString::from_utf16(lines[start].to_vec())) {
        start += 1;
    }
    trim_js(&join_lines(&lines[start..]))
}

pub fn strip_user_noise(text: &JsString) -> JsString {
    if text.is_empty() {
        return JsString::default();
    }
    let sender_hints = collect_sender_hints(text);
    let mut cleaned = strip_injected_memory_context(text);
    cleaned = extract_current_message_segment(&cleaned);
    cleaned = strip_quoted_context_blocks(&cleaned);
    cleaned = strip_attachment_scaffolding(&cleaned);
    cleaned = strip_untrusted_sender_metadata(&cleaned);
    cleaned = strip_leading_current_message_envelope(&cleaned);
    cleaned = strip_leading_timestamp_prefix(&cleaned);
    cleaned = strip_leading_message_envelope_lines(&cleaned);
    cleaned = strip_leading_sender_prefix(&cleaned, &sender_hints);
    cleaned = strip_leading_mentions(&cleaned);
    cleaned = strip_plugin_state_scaffolding(&cleaned);
    cleaned = strip_leading_message_envelope_lines(&cleaned);
    cleaned = strip_leading_timestamp_prefix(&cleaned);
    compact_whitespace(&cleaned)
}

// -- assistant thinking -----------------------------------------------------

pub fn strip_assistant_thinking(text: &JsString) -> JsString {
    let mut value = encode(text);
    value = think_close_pattern().replace(&value, "").to_string();
    value = thinking_close_pattern().replace(&value, "").to_string();
    value = think_pair_pattern().replace_all(&value, "").to_string();
    value = thinking_pair_pattern().replace_all(&value, "").to_string();
    value = think_tag_pattern().replace_all(&value, "").to_string();
    value = thinking_tag_pattern().replace_all(&value, "").to_string();
    let cleaned = decode(&value);
    let units = cleaned.units().to_vec();
    let lines = trim_lines(&units);
    let mut compact: Vec<&[u16]> = Vec::new();
    let mut previous_empty = false;
    for line in lines {
        let end = line
            .iter()
            .rposition(|unit| !js_is_space_unit(*unit))
            .map_or(0, |index| index + 1);
        let line = &line[..end];
        let empty = js_trim_units(line).is_empty();
        if !(empty && previous_empty) {
            compact.push(line);
            previous_empty = empty;
        }
    }
    let normalized = trim_js(&join_lines(&compact));
    let paragraphs: Vec<JsString> = re_split(&normalized, paragraph_pattern())
        .iter()
        .map(trim_js)
        .filter(|paragraph| !paragraph.is_empty())
        .collect();
    if paragraphs.len() >= 2 && is_meta_reasoning(&paragraphs[0]) {
        return trim_js(&strip_plugin_state_scaffolding(&join_text(
            &paragraphs[1..],
            "\n\n",
        )));
    }
    strip_plugin_state_scaffolding(&normalized)
}

fn is_meta_reasoning(value: &JsString) -> bool {
    let any = |list: &[&str]| list.iter().any(|needle| contains_str(value, needle));
    (starts_with_str(value, "用户")
        && any(&["需要", "应该", "这是", "表达", "分享", "说明", "记录"]))
        || (starts_with_str(value, "搜索结果") && any(&["需要", "无法", "不太理想"]))
        || (starts_with_str(value, "这搜索") && any(&["不太全", "看不到", "找不到"]))
}

fn is_noise_line(line: &JsString) -> bool {
    let value = trim_js(line);
    if value.is_empty() {
        return true;
    }
    let encoded = encode(&value);
    if value.equals_str("```")
        || lower_text(&value).equals_str("```json")
        || value.equals_str("{")
        || value.equals_str("}")
    {
        return true;
    }
    [
        untrusted_prefix_pattern(),
        quoted_prefix_pattern(),
        system_timestamp_pattern(),
        message_id_pattern(),
        context_bracket_pattern(),
        timestamp_label_pattern(),
    ]
    .iter()
    .any(|pattern| pattern.is_match(&encoded))
}

// -- message inspection -----------------------------------------------------

fn truthy(value: &JsValue) -> bool {
    match value {
        JsValue::Undefined | JsValue::Null => false,
        JsValue::Bool(value) => *value,
        JsValue::Number(value) => *value != 0.0 && !value.is_nan(),
        JsValue::String(value) => !value.is_empty(),
        JsValue::Array(_) | JsValue::Object(_) => true,
    }
}

static UNDEFINED: JsValue = JsValue::Undefined;

fn field<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    value.get(key).unwrap_or(&UNDEFINED)
}

fn as_record(value: &JsValue) -> Option<&JsObject> {
    match value {
        JsValue::Object(object) => Some(object),
        _ => None,
    }
}

fn raw_content<'a>(raw: &'a JsValue, nested: Option<&'a JsObject>) -> &'a JsValue {
    let content = field(raw, "content");
    if matches!(content, JsValue::Undefined | JsValue::Null) {
        nested
            .and_then(|nested| nested.get("content"))
            .unwrap_or(&UNDEFINED)
    } else {
        content
    }
}

pub fn has_tool_call_content(content: &JsValue) -> bool {
    let JsValue::Array(items) = content else {
        return false;
    };
    items.iter().any(|block| {
        block.as_object().is_some_and(|block| {
            block
                .get("type")
                .and_then(JsValue::as_string)
                .is_some_and(|kind| {
                    kind.equals_str("toolCall")
                        || kind.equals_str("toolResult")
                        || kind.equals_str("tool_use")
                        || kind.equals_str("tool_result")
                })
        })
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryRole {
    User,
    Assistant,
}

impl MemoryRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TranscriptMessageInfo {
    pub role: Option<MemoryRole>,
    pub content: JsString,
    pub has_tool_calls: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MemoryMessage {
    pub msg_id: Option<JsString>,
    pub role: MemoryRole,
    pub content: JsString,
}

impl MemoryMessage {
    /// TS inserts `role`, `content`, then the optional `msgId`.
    pub fn to_value(&self) -> JsValue {
        let mut object = JsObject::new();
        object.insert("role".into(), JsValue::String(self.role.as_str().into()));
        object.insert("content".into(), JsValue::String(self.content.clone()));
        if let Some(msg_id) = &self.msg_id {
            object.insert("msgId".into(), JsValue::String(msg_id.clone()));
        }
        JsValue::Object(object)
    }
}

pub fn inspect_transcript_message(raw: &JsValue) -> TranscriptMessageInfo {
    if !truthy(raw) || !matches!(raw, JsValue::Object(_) | JsValue::Array(_)) {
        return TranscriptMessageInfo {
            role: None,
            content: JsString::default(),
            has_tool_calls: false,
        };
    }
    let nested = as_record(field(raw, "message"));
    let role = field(raw, "role").as_string().or_else(|| {
        nested
            .and_then(|nested| nested.get("role"))
            .and_then(JsValue::as_string)
    });
    let normalized = match role {
        Some(role) if role.equals_str("user") => Some(MemoryRole::User),
        Some(role) if role.equals_str("assistant") => Some(MemoryRole::Assistant),
        _ => None,
    };
    let content = raw_content(raw, nested);
    let raw_text = trim_js(&extract_text_from_content(content, 0));
    let normalized_content = match normalized {
        Some(MemoryRole::User) => strip_user_noise(&raw_text),
        Some(MemoryRole::Assistant) => strip_assistant_thinking(&raw_text),
        None => JsString::default(),
    };
    TranscriptMessageInfo {
        role: normalized,
        content: normalized_content,
        has_tool_calls: has_tool_call_content(content),
    }
}

pub fn is_session_boundary_marker_message(raw: &JsValue) -> bool {
    let info = inspect_transcript_message(raw);
    info.role == Some(MemoryRole::User) && is_session_startup_marker_text(&info.content)
}

pub fn is_session_startup_marker_text(text: &JsString) -> bool {
    let normalized = compact_whitespace(&strip_user_noise(text));
    if normalized.is_empty() {
        return false;
    }
    contains_str(&normalized, SESSION_START_PREFIX)
        || (contains_str(&normalized, SESSION_START_PREFIX) && session_sequence(&normalized))
}

fn session_sequence(value: &JsString) -> bool {
    session_sequence_pattern().is_match(&encode(value))
}

pub fn is_command_only_user_text(text: &JsString) -> bool {
    let normalized = compact_whitespace(&strip_user_noise(text));
    if normalized.is_empty() || is_session_startup_marker_text(&normalized) {
        return false;
    }
    parse_leading_slash_command(&normalized).is_some() || parse_bang_command(&normalized).is_some()
}

pub fn parse_leading_slash_command(text: &JsString) -> Option<JsString> {
    let trimmed = compact_whitespace(text);
    let encoded = encode(&trimmed);
    let capture = slash_command_pattern().captures(&encoded)?;
    let command = lower_text(&decode(&capture[1]));
    let command = command.to_utf8().ok()?;
    SLASH_COMMANDS
        .contains(&command.as_str())
        .then_some(JsString::from(command))
}

pub fn parse_bang_command(text: &JsString) -> Option<JsString> {
    let trimmed = compact_whitespace(text);
    let units = trimmed.units();
    if units.first() != Some(&u16::from(b'!')) {
        return None;
    }
    let after = 1 + units[1..]
        .iter()
        .position(|unit| !js_is_space_unit(*unit))
        .unwrap_or(units.len() - 1);
    if after > 1 && after < units.len() {
        return Some(JsString::from("bash"));
    }
    let encoded = encode(&trimmed);
    let capture = bang_command_pattern().captures(&encoded)?;
    let command = lower_text(&decode(&capture[1])).to_utf8().ok()?;
    BANG_COMMANDS
        .contains(&command.as_str())
        .then_some(JsString::from(command))
}

pub fn should_skip_user_message(content: &JsString) -> bool {
    content.is_empty()
        || is_session_startup_marker_text(content)
        || is_command_only_user_text(content)
        || starts_with_str(content, SLUG_PREFIX)
}

pub fn should_skip_assistant_message(raw_content: &JsValue, content: &JsString) -> bool {
    if content.is_empty() {
        return true;
    }
    if has_tool_call_content(raw_content) {
        return true;
    }
    if contains_str(content, "\n") {
        return false;
    }
    let compact = trim_js(content);
    (starts_with_str(&compact, "用户")
        && ["需要", "应该", "这是", "表达", "分享", "记录"]
            .iter()
            .any(|needle| contains_str(&compact, needle)))
        || (starts_with_str(&compact, "搜索结果")
            && ["需要", "无法", "不太理想"]
                .iter()
                .any(|needle| contains_str(&compact, needle)))
}

// -- normalization ----------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureStrategy {
    LastTurn,
    FullSession,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NormalizeOptions {
    pub include_assistant: bool,
    pub max_message_chars: f64,
    pub capture_strategy: CaptureStrategy,
}

fn normalize_single_message(
    raw: &JsValue,
    include_assistant: bool,
    max_message_chars: f64,
) -> Option<MemoryMessage> {
    if !truthy(raw) || !matches!(raw, JsValue::Object(_) | JsValue::Array(_)) {
        return None;
    }
    let nested = as_record(field(raw, "message"));
    let tool_result = field(raw, "toolUseResult");
    if field(raw, "type")
        .as_string()
        .is_some_and(|kind| kind.equals_str("user"))
        && (!matches!(tool_result, JsValue::Undefined | JsValue::Null)
            || truthy(field(raw, "isMeta"))
            || truthy(field(raw, "isVisibleInTranscriptOnly"))
            || truthy(field(raw, "isCompactSummary"))
            || truthy(field(raw, "isVirtual")))
    {
        return None;
    }
    let info = inspect_transcript_message(raw);
    let role = info.role?;
    if role == MemoryRole::Assistant && !include_assistant {
        return None;
    }
    let content = raw_content(raw, nested).clone();
    if role == MemoryRole::User && should_skip_user_message(&info.content) {
        return None;
    }
    if role == MemoryRole::Assistant && should_skip_assistant_message(&content, &info.content) {
        return None;
    }
    if info.content.is_empty() {
        return None;
    }
    let msg_id = field(raw, "id")
        .as_string()
        .or_else(|| {
            nested
                .and_then(|nested| nested.get("id"))
                .and_then(JsValue::as_string)
        })
        .filter(|id| !id.is_empty())
        .cloned();
    Some(MemoryMessage {
        msg_id,
        role,
        content: crate::runtime::memory::edgeclaw::text::truncate(&info.content, max_message_chars),
    })
}

pub fn normalize_transcript_message(
    raw: &JsValue,
    include_assistant: bool,
    max_message_chars: f64,
) -> Option<MemoryMessage> {
    normalize_single_message(raw, include_assistant, max_message_chars)
}

pub fn normalize_messages(raw: &[JsValue], options: &NormalizeOptions) -> Vec<MemoryMessage> {
    let mut all = Vec::new();
    for message in raw {
        if let Some(normalized) = normalize_single_message(
            message,
            options.include_assistant,
            options.max_message_chars,
        ) {
            all.push(normalized);
        }
    }
    if options.capture_strategy == CaptureStrategy::FullSession {
        return all;
    }
    if let Some(last_user) = all
        .iter()
        .rposition(|message| message.role == MemoryRole::User)
    {
        return all.split_off(last_user);
    }
    if all.iter().any(|message| message.role == MemoryRole::User) {
        Vec::new()
    } else {
        all.split_off(all.len().saturating_sub(2))
    }
}

lazy_regex!(think_close_pattern, r"(?is)^.*?</think>");
lazy_regex!(thinking_close_pattern, r"(?is)^.*?</thinking>");
lazy_regex!(think_pair_pattern, r"(?is)<think>.*?</think>");
lazy_regex!(thinking_pair_pattern, r"(?is)<thinking>.*?</thinking>");
lazy_regex!(think_tag_pattern, r"(?i)</?think[^>]*>");
lazy_regex!(thinking_tag_pattern, r"(?i)</?thinking[^>]*>");
lazy_regex!(
    session_sequence_pattern,
    r"(?i)(?-u:\b)(?:Run|Execute) your Session Startup sequence(?-u:\b)"
);

// -- static patterns --------------------------------------------------------

lazy_regex!(
    sender_block_pattern,
    r"(?is)(?:Conversation info|Sender)$S*\(untrusted metadata\)$S*:$S*```(?:json)?$S*(.*?)$S*```"
);
lazy_regex!(
    sender_inline_pattern,
    r"(?is)(?:Conversation info|Sender)$S*\(untrusted metadata\)$S*:$S*(\{.*?\})"
);
lazy_regex!(
    sender_block_strip_pattern,
    r"(?is)(?:Conversation info|Sender)$S*\(untrusted metadata\)$S*:$S*```(?:json)?.*?```$S*"
);
lazy_regex!(
    sender_inline_strip_pattern,
    r"(?is)(?:Conversation info|Sender)$S*\(untrusted metadata\)$S*:$S*\{.*?\}$S*"
);
lazy_regex!(
    quoted_pattern,
    r"(?is)(?:Chat history since last reply|Replied message)$S*\(untrusted[^)]*\)$S*:$S*```(?:json)?.*?```$S*"
);
lazy_regex!(
    timestamp_pattern,
    r"(?i)^$S*\[[^\]\n]*(?:[0-9]{4}-[0-9]{1,2}-[0-9]{1,2}|GMT|UTC)[^\]\n]*\]$S*"
);
lazy_regex!(file_boundary_pattern, r"(?i)\n<file(?-u:\b)");
lazy_regex!(file_tag_pattern, r"(?i)<file(?-u:\b)[^>]*>");
lazy_regex!(
    current_envelope_pattern,
    r"(?s)^(?:\[[^\]\n]{1,320}\]$S*)+([^:\n]{1,120}):$S*(.*)$"
);
lazy_regex!(opaque_label_pattern, r"^[A-Za-z0-9_.@()\- ]+$");
lazy_regex!(opaque_digit_pattern, r"[0-9_]");
lazy_regex!(opaque_word_pattern, r"^[A-Za-z][A-Za-z0-9_.@()\- ]{4,}$");
lazy_regex!(sender_prefix_pattern, r"(?s)^([^:\n]{1,120}):$S*(.*)$");
lazy_regex!(mention_pattern, r"(?i)^@[A-Za-z0-9_.-]+$S*");
lazy_regex!(
    untrusted_prefix_pattern,
    r"(?i)^(?:Conversation info|Sender)$S*\(untrusted metadata\)$S*:?"
);
lazy_regex!(
    quoted_prefix_pattern,
    r"(?i)^(?:Chat history since last reply|Replied message)$S*\(untrusted[^)]*\)$S*:?"
);
lazy_regex!(
    system_timestamp_pattern,
    r"(?i)^System:$S*\[[^\]\n]*(?:[0-9]{4}-[0-9]{1,2}-[0-9]{1,2}|GMT|UTC)[^\]\n]*\]$S*"
);
lazy_regex!(message_id_pattern, r"(?i)^\[message_id:[^\]]+\]$");
lazy_regex!(
    context_bracket_pattern,
    r"(?i)^\[[^\]\n]*(?:for context|Current message - respond to this)[^\]\n]*\]$"
);
lazy_regex!(
    timestamp_label_pattern,
    r"(?i)^\[[^\]\n]*(?:[0-9]{4}-[0-9]{1,2}-[0-9]{1,2}|GMT|UTC|channel:[0-9]+)[^\]\n]*\]$S*[^:\n]{1,120}:$S*"
);
lazy_regex!(
    slash_command_pattern,
    r"^/([A-Za-z][A-Za-z0-9_-]*):?(?:$S|$)"
);
lazy_regex!(bang_command_pattern, r"^!([A-Za-z][A-Za-z0-9_-]*)(?:$S|$)");
