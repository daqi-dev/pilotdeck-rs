//! JSON envelope extraction and loose model-output normalization.
//!
//! Ports the live pure helpers of the fixed TS baseline
//! `src/context/memory/edgeclaw-memory-core/src/core/skills/llm-extraction.ts`
//! (`:832-844`, `:1313-1419`, `:1619-1658`, `:2025-2041`). Model output is
//! untrusted text: a cut emoji can end in an unpaired surrogate, so every
//! string is a [`JsString`] and every operation works on UTF-16 code units.
//!
//! `escapeRegexLiteral` has no Rust counterpart: the loose property extractors
//! scan units directly instead of building a `RegExp`. They reproduce the `/i`
//! matcher (ECMAScript `Canonicalize`), JavaScript `\s`, lazy/greedy
//! `[\s\S]` backtracking and the `$` anchor, so Rust regex semantics cannot
//! leak in. The dead `pickLongest`, `uniqueById` and `fallbackEvidenceNote`
//! helpers near these TS ranges are not ported (#751 §1.4).

use crate::{
    model_catalog::lower,
    runtime::memory::edgeclaw::text::normalize_text,
    runtime::memory::{js_is_space_unit, js_trim_units},
};
use icu_locale_core::LanguageIdentifier;
use pilotdeck_protocol::{JsString, JsValue};
use std::fmt;

const QUOTE: u16 = b'"' as u16;
const BACKSLASH: u16 = b'\\' as u16;
const OPEN_BRACE: u16 = b'{' as u16;
const CLOSE_BRACE: u16 = b'}' as u16;
const COLON: u16 = b':' as u16;
const COMMA: u16 = b',' as u16;

/// A TS `throw new Error(...)` from the envelope extractors, message intact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnvelopeError {
    Empty,
    NoObject,
    Incomplete,
    NoEnvelope,
}

impl EnvelopeError {
    pub fn message(self) -> &'static str {
        match self {
            Self::Empty => "Empty extraction response",
            Self::NoObject => "No JSON object found in extraction response",
            Self::Incomplete => "Incomplete JSON object in extraction response",
            Self::NoEnvelope => "No JSON envelope found in extraction response",
        }
    }
}

impl fmt::Display for EnvelopeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message())
    }
}

impl std::error::Error for EnvelopeError {}

// -- `llm-extraction.ts:832-844` -------------------------------------------

/// `truncate`: `value.length <= maxLength`, else `value.slice(0, max).trim()`.
pub fn truncate(value: &JsString, max_length: f64) -> JsString {
    if (value.len_utf16() as f64) <= max_length {
        return value.clone();
    }
    let end = slice_end(max_length, value.len_utf16());
    JsString::from_utf16(js_trim_units(&value.units()[..end]).to_vec())
}

/// `normalizeWhitespace`: `value.replace(/\s+/g, " ").trim()`.
pub fn normalize_whitespace(value: &JsString) -> JsString {
    // Already ported and fixture-locked by A6.6-edgeclaw-1.
    normalize_text(value)
}

/// `truncateForPrompt`: normalize first, then truncate.
pub fn truncate_for_prompt(value: &JsString, max_length: f64) -> JsString {
    truncate(&normalize_whitespace(value), max_length)
}

// -- `llm-extraction.ts:1313-1419` -----------------------------------------

/// `extractFirstJsonObject`: whole trimmed input if brace-bounded, else the
/// first balanced `{...}` found by a string/escape-aware scan.
pub fn extract_first_json_object(raw: &JsString) -> Result<JsString, EnvelopeError> {
    let trimmed = js_trim_units(raw.units());
    if trimmed.is_empty() {
        return Err(EnvelopeError::Empty);
    }
    if trimmed[0] == OPEN_BRACE && trimmed[trimmed.len() - 1] == CLOSE_BRACE {
        return Ok(JsString::from_utf16(trimmed.to_vec()));
    }
    let Some(start) = trimmed.iter().position(|&unit| unit == OPEN_BRACE) else {
        return Err(EnvelopeError::NoObject);
    };
    let mut depth = 0i64;
    let mut in_string = false;
    let mut escaped = false;
    for (index, &unit) in trimmed.iter().enumerate().skip(start) {
        if in_string {
            if escaped {
                escaped = false;
            } else if unit == BACKSLASH {
                escaped = true;
            } else if unit == QUOTE {
                in_string = false;
            }
            continue;
        }
        if unit == QUOTE {
            in_string = true;
            continue;
        }
        if unit == OPEN_BRACE {
            depth += 1;
        }
        if unit == CLOSE_BRACE {
            depth -= 1;
            if depth == 0 {
                return Ok(JsString::from_utf16(trimmed[start..=index].to_vec()));
            }
        }
    }
    Err(EnvelopeError::Incomplete)
}

/// `extractLooseJsonEnvelope`: first `{` through last `}`, inclusive.
pub fn extract_loose_json_envelope(raw: &JsString) -> Result<JsString, EnvelopeError> {
    let trimmed = js_trim_units(raw.units());
    if trimmed.is_empty() {
        return Err(EnvelopeError::Empty);
    }
    let start = trimmed.iter().position(|&unit| unit == OPEN_BRACE);
    let end = trimmed.iter().rposition(|&unit| unit == CLOSE_BRACE);
    match (start, end) {
        (Some(start), Some(end)) if end > start => {
            Ok(JsString::from_utf16(trimmed[start..=end].to_vec()))
        }
        _ => Err(EnvelopeError::NoEnvelope),
    }
}

/// `decodeLooseJsonString`: the six global replacements, in TS order.
pub fn decode_loose_json_string(value: &JsString) -> JsString {
    let mut units = value.units().to_vec();
    for (from, to) in [
        (b"\\r\\n".as_slice(), b"\n".as_slice()),
        (b"\\n".as_slice(), b"\n".as_slice()),
        (b"\\r".as_slice(), b"\r".as_slice()),
        (b"\\t".as_slice(), b"\t".as_slice()),
        (b"\\\"".as_slice(), b"\"".as_slice()),
        (b"\\\\".as_slice(), b"\\".as_slice()),
    ] {
        units = replace_all(&units, from, to);
    }
    JsString::from_utf16(units)
}

/// `extractLooseJsonBooleanProperty`: first `"key"\s*:\s*(true|false)`, `/i`.
pub fn extract_loose_json_boolean_property(source: &JsString, key: &JsString) -> Option<bool> {
    let units = source.units();
    let key = key.units();
    for at in 0..units.len() {
        if units[at] != QUOTE || !key_at(units, at, key) {
            continue;
        }
        let mut index = skip_space(units, at + key.len() + 2);
        if units.get(index) != Some(&COLON) {
            continue;
        }
        index = skip_space(units, index + 1);
        for word in ["true", "false"] {
            if !word_at(units, index, word.as_bytes()) {
                continue;
            }
            // `match[1].toLowerCase() === "true"`: the capture keeps its
            // source case, so compare the lowercased form.
            let matched = JsString::from_utf16(units[index..index + word.len()].to_vec());
            let normalized = lower(&matched, &LanguageIdentifier::UNKNOWN, false);
            return Some(normalized.equals_str("true"));
        }
    }
    None
}

/// `extractLooseJsonStringProperty`, including the two TS regex shapes: with
/// next keys the string group is lazy and followed by `"\s*,\s*"next"\s*:`;
/// without them it is greedy and followed by `"\s*}\s*$`.
pub fn extract_loose_json_string_property(
    source: &JsString,
    key: &JsString,
    next_keys: &[JsString],
) -> Option<JsString> {
    let units = source.units();
    let key = key.units();
    for at in 0..units.len() {
        if units[at] != QUOTE || !key_at(units, at, key) {
            continue;
        }
        let mut index = skip_space(units, at + key.len() + 2);
        if units.get(index) != Some(&COLON) {
            continue;
        }
        index = skip_space(units, index + 1);
        if units.get(index) != Some(&QUOTE) {
            continue;
        }
        let group_start = index + 1;
        let group_end = if next_keys.is_empty() {
            greedy_group_end(units, group_start)
        } else {
            lazy_group_end(units, group_start, next_keys)
        };
        let Some(group_end) = group_end else { continue };
        // `match?.[1]` treats an empty capture as no value, without searching
        // for a later match.
        if group_end == group_start {
            return None;
        }
        return Some(decode_loose_json_string(&JsString::from_utf16(
            units[group_start..group_end].to_vec(),
        )));
    }
    None
}

/// `tryParseLooseMemoryCreatePayload`: `null` unless the three required
/// strings were captured; envelope errors still propagate as TS throws.
pub fn try_parse_loose_memory_create_payload(
    raw: &JsString,
) -> Result<Option<LooseMemoryCreatePayload>, EnvelopeError> {
    let envelope = extract_loose_json_envelope(raw)?;
    let skip = extract_loose_json_boolean_property(&envelope, &JsString::from("skip"));
    let reason = extract_loose_json_string_property(
        &envelope,
        &JsString::from("reason"),
        &strings(["name", "description", "markdown"]),
    );
    let name = extract_loose_json_string_property(
        &envelope,
        &JsString::from("name"),
        &strings(["description", "markdown"]),
    );
    let description = extract_loose_json_string_property(
        &envelope,
        &JsString::from("description"),
        &strings(["markdown"]),
    );
    let markdown = extract_loose_json_string_property(&envelope, &JsString::from("markdown"), &[]);
    let payload = LooseMemoryCreatePayload {
        skip,
        reason,
        name,
        description,
        markdown,
    };
    if payload.name.is_some() && payload.description.is_some() && payload.markdown.is_some() {
        Ok(Some(payload))
    } else {
        Ok(None)
    }
}

/// The subset `RawMemoryCreatePayload` that the loose fallback can recover.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LooseMemoryCreatePayload {
    pub skip: Option<bool>,
    pub reason: Option<JsString>,
    pub name: Option<JsString>,
    pub description: Option<JsString>,
    pub markdown: Option<JsString>,
}

// -- `llm-extraction.ts:1619-1658,2025-2041` -------------------------------

/// `normalizeStringArray`: trim, drop non-strings/blank items, slice by index.
pub fn normalize_string_array(items: &JsValue, max_items: f64) -> Vec<JsString> {
    let mut out = Vec::new();
    match items {
        JsValue::String(text) => {
            let trimmed = js_trim_units(text.units());
            if !trimmed.is_empty() {
                out.push(JsString::from_utf16(trimmed.to_vec()));
            }
        }
        JsValue::Array(items) => {
            for item in items {
                let JsValue::String(text) = item else {
                    continue;
                };
                let trimmed = js_trim_units(text.units());
                if !trimmed.is_empty() {
                    out.push(JsString::from_utf16(trimmed.to_vec()));
                }
            }
        }
        _ => return Vec::new(),
    }
    out.truncate(slice_end(max_items, out.len()));
    out
}

/// `uniqueStrings`: trim, drop blanks, dedupe by code units, then slice.
pub fn unique_strings(items: &[JsString], max_items: f64) -> Vec<JsString> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for item in items {
        let trimmed = js_trim_units(item.units());
        if trimmed.is_empty() {
            continue;
        }
        let value = JsString::from_utf16(trimmed.to_vec());
        if seen.insert(value.clone()) {
            out.push(value);
        }
    }
    out.truncate(slice_end(max_items, out.len()));
    out
}

/// `normalizeMemoryRoute`: the four literals, case-sensitive, else `none`.
pub fn normalize_memory_route(value: &JsValue) -> MemoryRoute {
    match value {
        JsValue::String(text) if text.equals_str("user") => MemoryRoute::User,
        JsValue::String(text) if text.equals_str("project") => MemoryRoute::Project,
        JsValue::String(text) if text.equals_str("mix") => MemoryRoute::Mix,
        _ => MemoryRoute::None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryRoute {
    User,
    Project,
    Mix,
    None,
}

impl MemoryRoute {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Project => "project",
            Self::Mix => "mix",
            Self::None => "none",
        }
    }
}

/// `normalizeBoolean`: booleans pass through, `"true"`/`"false"` after JS
/// trim and `toLowerCase`, everything else returns the fallback.
pub fn normalize_boolean(value: &JsValue, fallback: bool) -> bool {
    match value {
        JsValue::Bool(value) => *value,
        JsValue::String(text) => {
            let trimmed = JsString::from_utf16(js_trim_units(text.units()).to_vec());
            let normalized = lower(&trimmed, &LanguageIdentifier::UNKNOWN, false);
            if normalized.equals_str("true") {
                true
            } else if normalized.equals_str("false") {
                false
            } else {
                fallback
            }
        }
        _ => fallback,
    }
}

// -- helpers ---------------------------------------------------------------

/// `Array.prototype.slice(0, end)` / `String.prototype.slice(0, end)` index:
/// NaN is 0, fractions truncate, negative values count from the end, and the
/// result clamps to `0..=len`.
fn slice_end(end: f64, len: usize) -> usize {
    if end.is_nan() || end == f64::NEG_INFINITY {
        return 0;
    }
    if end >= len as f64 {
        return len;
    }
    let end = end.trunc();
    if end < 0.0 {
        let shifted = len as f64 + end;
        if shifted <= 0.0 { 0 } else { shifted as usize }
    } else {
        end as usize
    }
}

fn skip_space(units: &[u16], from: usize) -> usize {
    let mut index = from;
    while index < units.len() && js_is_space_unit(units[index]) {
        index += 1;
    }
    index
}

/// `Canonicalize` for a non-unicode `/i` RegExp: ASCII folds through
/// `ToUpperCase`, non-ASCII pairs fold only when both upper-case to the same
/// single code point (Node: `é`/`É` and `σ`/`ς` match, `ß`/`ẞ` and
/// `Å`/`Å` do not), and ASCII never matches non-ASCII (`"skıp"` is not
/// `"skip"`).
fn unit_equals_ignoring_case(left: u16, right: u16) -> bool {
    if left == right {
        return true;
    }
    match (left < 0x80, right < 0x80) {
        (true, true) => ascii_upper(left) == ascii_upper(right),
        (false, false) => simple_upper(left) == simple_upper(right),
        _ => false,
    }
}

fn ascii_upper(unit: u16) -> u16 {
    u16::from((unit as u8).to_ascii_uppercase())
}

fn simple_upper(unit: u16) -> u32 {
    match char::from_u32(u32::from(unit)) {
        Some(value) => {
            let mut upper = value.to_uppercase();
            match (upper.next(), upper.next()) {
                (Some(first), None) => u32::from(first),
                _ => u32::from(unit),
            }
        }
        None => u32::from(unit),
    }
}

/// The literal `"key"` at `at`, matched case-insensitively like the escaped
/// RegExp source does.
fn key_at(units: &[u16], at: usize, key: &[u16]) -> bool {
    if units.get(at) != Some(&QUOTE) || units.len() < at + key.len() + 2 {
        return false;
    }
    if !key
        .iter()
        .enumerate()
        .all(|(index, &unit)| unit_equals_ignoring_case(units[at + 1 + index], unit))
    {
        return false;
    }
    units[at + key.len() + 1] == QUOTE
}

fn word_at(units: &[u16], at: usize, word: &[u8]) -> bool {
    units.len() >= at + word.len()
        && word
            .iter()
            .enumerate()
            .all(|(index, &unit)| unit_equals_ignoring_case(units[at + index], u16::from(unit)))
}

fn strings<const N: usize>(items: [&str; N]) -> Vec<JsString> {
    items.into_iter().map(JsString::from).collect()
}

/// Lazy `[\s\S]*?` then `"\s*,\s*"next"\s*:`: the first closing quote that
/// lets the rest match wins.
fn lazy_group_end(units: &[u16], start: usize, next_keys: &[JsString]) -> Option<usize> {
    (start..units.len()).find(|&end| units[end] == QUOTE && comma_key_at(units, end + 1, next_keys))
}

fn comma_key_at(units: &[u16], from: usize, next_keys: &[JsString]) -> bool {
    let mut index = skip_space(units, from);
    if units.get(index) != Some(&COMMA) {
        return false;
    }
    index = skip_space(units, index + 1);
    if units.get(index) != Some(&QUOTE) {
        return false;
    }
    next_keys.iter().any(|key| {
        key_at(units, index, key.units())
            && units.get(skip_space(units, index + key.len_utf16() + 2)) == Some(&COLON)
    })
}

/// Greedy `[\s\S]*` then `"\s*}\s*$`: the last `"` whose suffix to the final
/// `}` is only JavaScript whitespace.
fn greedy_group_end(units: &[u16], start: usize) -> Option<usize> {
    let mut end = units.len();
    while end > 0 && js_is_space_unit(units[end - 1]) {
        end -= 1;
    }
    if end == 0 || units[end - 1] != CLOSE_BRACE || end <= start + 1 {
        return None;
    }
    let brace = end - 1;
    let quote = (start..brace).rev().find(|&index| units[index] == QUOTE)?;
    if units[quote + 1..brace]
        .iter()
        .any(|&unit| !js_is_space_unit(unit))
    {
        return None;
    }
    Some(quote)
}

fn replace_all(haystack: &[u16], from: &[u8], to: &[u8]) -> Vec<u16> {
    let from: Vec<u16> = from.iter().map(|&unit| u16::from(unit)).collect();
    let to: Vec<u16> = to.iter().map(|&unit| u16::from(unit)).collect();
    let mut out = Vec::with_capacity(haystack.len());
    let mut index = 0;
    while index < haystack.len() {
        if haystack[index..].starts_with(&from) {
            out.extend_from_slice(&to);
            index += from.len();
        } else {
            out.push(haystack[index]);
            index += 1;
        }
    }
    out
}
