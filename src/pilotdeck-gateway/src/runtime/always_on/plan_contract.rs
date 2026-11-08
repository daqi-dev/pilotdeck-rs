//! Validation of Always-On plan markdown, preserving JS UTF-16 code units.
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use super::AlwaysOnError;

const REQUIRED: [&str; 6] = ["Summary", "Rationale", "Context Signals", "Proposed Change", "Execution Steps", "Verification"];
const METADATA: [&str; 5] = ["id", "sourceRunId", "createdAt", "projectRoot", "dedupeKey"];

pub(crate) struct ParsedPlan {
    pub title: JsString,
    pub metadata: JsObject,
    pub sections: JsObject,
    pub raw: JsString,
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
fn starts(v: &[u16], s: &str) -> bool {
    let bytes = s.as_bytes(); v.len() >= bytes.len() && v.iter().zip(bytes).all(|(a,b)| *a == u16::from(*b))
}
fn bad(message: impl Into<JsString>) -> AlwaysOnError { AlwaysOnError::new("plan_invalid", message) }
fn joined(lines: &[JsString]) -> Vec<u16> {
    let mut result = Vec::new();
    for (i,line) in lines.iter().enumerate() { if i > 0 { result.push(10); } result.extend_from_slice(line.units()); }
    result
}
fn array(lines: &[JsString]) -> JsValue { JsValue::Array(lines.iter().cloned().map(JsValue::String).collect()) }
fn list_item(line: &[u16], marker: u16) -> bool {
    let s = trim_start(line); s.len() >= 2 && s[0] == marker && ws(s[1])
}
fn trim_start(v: &[u16]) -> &[u16] { &v[v.iter().position(|c| !ws(*c)).unwrap_or(v.len())..] }
fn ascii_word(c: u16) -> bool { (48..=57).contains(&c) || (65..=90).contains(&c) || (97..=122).contains(&c) || c == 95 }

pub(crate) fn parse_plan(content: &JsString, max_size: usize, summary_max: usize) -> Result<ParsedPlan, AlwaysOnError> {
    let mut normalized = Vec::with_capacity(content.len_utf16());
    let mut source = content.units().iter().copied().peekable();
    while let Some(mut c) = source.next() {
        if c == 13 && source.peek() == Some(&10) { source.next(); c = 10; }
        if c == 0xa0 { c = 32; }
        normalized.push(c);
    }
    if normalized.is_empty() { return Err(bad("plan content is empty.")); }
    if normalized.len() > max_size { return Err(bad(format!("plan content exceeds maxResultSizeChars ({max_size}).").as_str())); }
    let lines: Vec<&[u16]> = normalized.split(|c| *c == 10).collect();
    let mut at = 0;
    while at < lines.len() && trim(lines[at]).is_empty() { at += 1; }
    let heading = lines.get(at).copied().unwrap_or(&[]);
    if !starts(heading, "# ") { return Err(bad("plan must start with a level-1 markdown heading.")); }
    let title = JsString::from_utf16(trim(&heading[2..]).to_vec());
    if title.is_empty() { return Err(bad("plan title must not be empty.")); }
    at += 1;
    while at < lines.len() && trim(lines[at]).is_empty() { at += 1; }
    let mut metadata_lines = Vec::new();
    while at < lines.len() && starts(lines[at], ">") {
        let line = &lines[at][1..];
        metadata_lines.push(if line.first().is_some_and(|c| ws(*c)) { &line[1..] } else { line });
        at += 1;
    }
    if metadata_lines.is_empty() { return Err(bad("plan must include a metadata blockquote immediately after the title.")); }
    if !trim(metadata_lines[0]).iter().copied().eq("Always-On Discovery Plan".encode_utf16()) {
        return Err(bad("plan metadata blockquote first line must be \"Always-On Discovery Plan\"."));
    }
    let mut metadata = JsObject::new();
    for key in METADATA { metadata.insert(key.into(), JsValue::String("".into())); }
    for line in &metadata_lines[1..] {
        let value = trim(line); if value.is_empty() { continue; }
        let Some(colon) = value.iter().position(|c| *c == u16::from(b':')) else {
            let mut message = "plan metadata line \"".encode_utf16().collect::<Vec<_>>(); message.extend_from_slice(line);
            message.extend("\" is not in \"key: value\" form.".encode_utf16()); return Err(bad(JsString::from_utf16(message)));
        };
        let key = JsString::from_utf16(trim(&value[..colon]).to_vec());
        let inherited = ["__proto__", "constructor", "toString", "valueOf", "hasOwnProperty",
            "isPrototypeOf", "propertyIsEnumerable", "toLocaleString", "__defineGetter__",
            "__defineSetter__", "__lookupGetter__", "__lookupSetter__"];
        if !METADATA.iter().chain(inherited.iter()).any(|k| key.equals_str(k)) {
            let mut message = "plan metadata contains unknown key \"".encode_utf16().collect::<Vec<_>>(); message.extend_from_slice(key.units());
            message.extend("\".".encode_utf16()); return Err(bad(JsString::from_utf16(message)));
        }
        if !key.equals_str("__proto__") {
            metadata.insert(key, JsValue::String(JsString::from_utf16(trim(&value[colon+1..]).to_vec())));
        }
    }
    for key in METADATA {
        if metadata.get(key).and_then(JsValue::as_string).is_none_or(JsString::is_empty) {
            return Err(bad(format!("plan metadata is missing required key \"{key}\".").as_str()));
        }
    }
    while at < lines.len() && trim(lines[at]).is_empty() { at += 1; }
    let mut sections: Vec<(JsString, Vec<JsString>)> = Vec::new();
    for line in &lines[at..] {
        if starts(line, "## ") {
            let name = JsString::from_utf16(trim(&line[3..]).to_vec());
            if name.is_empty() { return Err(bad("plan section heading must not be empty.")); }
            if sections.iter().any(|(existing,_)| existing == &name) {
                let mut message = "plan contains duplicate section \"".encode_utf16().collect::<Vec<_>>(); message.extend_from_slice(name.units());
                message.extend("\".".encode_utf16()); return Err(bad(JsString::from_utf16(message)));
            }
            sections.push((name, Vec::new()));
        } else if starts(line, "# ") {
            return Err(bad("plan must contain exactly one level-1 heading at the top."));
        } else if let Some((_, body)) = sections.last_mut() {
            body.push(JsString::from_utf16(line.to_vec()));
        }
    }
    // A plain JS object's __proto__ assignment changes its prototype rather
    // than creating an own key, while the duplicate-heading set still sees it.
    sections.retain(|(name, _)| !name.equals_str("__proto__"));
    if sections.len() != REQUIRED.len() {
        return Err(bad("plan must contain exactly the required sections: Summary, Rationale, Context Signals, Proposed Change, Execution Steps, Verification."));
    }
    for (i, (name,_)) in sections.iter().enumerate() {
        if !name.equals_str(REQUIRED[i]) {
            let mut message = format!("plan section #{} must be \"{}\", got \"",i+1,REQUIRED[i]).encode_utf16().collect::<Vec<_>>();
            message.extend_from_slice(name.units()); message.extend("\".".encode_utf16()); return Err(bad(JsString::from_utf16(message)));
        }
    }
    let summary = joined(&sections[0].1); let summary = trim(&summary);
    if summary.is_empty() { return Err(bad("plan section \"Summary\" must not be empty.")); }
    if summary.len() > summary_max { return Err(bad(format!("plan Summary exceeds {summary_max} characters.").as_str())); }
    let mut previous_blank = false;
    for line in summary.split(|c| *c == 10) {
        if trim(line).is_empty() { if previous_blank { return Err(bad("plan Summary must be a single paragraph.")); } previous_blank = true; }
        else if previous_blank { return Err(bad("plan Summary must be a single paragraph.")); }
    }
    if trim(&joined(&sections[1].1)).is_empty() { return Err(bad("plan section \"Rationale\" must not be empty.")); }
    if !sections[2].1.iter().any(|line| list_item(line.units(),u16::from(b'-'))) { return Err(bad("plan Context Signals must contain at least one unordered list item.")); }
    let proposal = joined(&sections[3].1); let proposal = trim(&proposal);
    if proposal.is_empty() { return Err(bad("plan section \"Proposed Change\" must not be empty.")); }
    let lower: Vec<u16> = proposal.iter().map(|c| if (65..=90).contains(c) { *c + 32 } else { *c }).collect();
    if starts(&lower, "todo") && lower.get(4).is_none_or(|c| !ascii_word(*c)) {
        return Err(bad("plan Proposed Change must not be fuzzy (\"^\\s*TODO\\b\")."));
    }
    let chinese: Vec<u16> = "待补充".encode_utf16().collect();
    if proposal.starts_with(&chinese) && proposal.get(chinese.len()).is_some_and(|c| ascii_word(*c)) {
        return Err(bad("plan Proposed Change must not be fuzzy (\"^\\s*待补充\\b\")."));
    }
    let mut ordered = false;
    for line in &sections[4].1 {
        let s = trim(line.units()); if s.is_empty() { continue; }
        let digits = s.iter().take_while(|c| (48..=57).contains(*c)).count();
        if digits > 0 && s.get(digits) == Some(&46) && s.get(digits+1).is_some_and(|c| ws(*c)) { ordered = true; continue; }
        if s.len() > 1 && [45,42,43].contains(&s[0]) && ws(s[1]) { return Err(bad("plan Execution Steps must use an ordered list (no unordered items).")); }
    }
    if !ordered { return Err(bad("plan Execution Steps must contain at least one ordered list item.")); }
    if !sections[5].1.iter().any(|line| list_item(line.units(),u16::from(b'-'))) { return Err(bad("plan Verification must contain at least one unordered list item.")); }
    let mut result = JsObject::new(); for (name,body) in sections { result.insert(name,array(&body)); }
    while normalized.last().is_some_and(|c| ws(*c)) { normalized.pop(); } normalized.push(10);
    Ok(ParsedPlan { title, metadata, sections: result, raw: JsString::from_utf16(normalized) })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn real_typescript_plan_contract_oracle() {
        let fixture = pilotdeck_protocol::parse_json(include_str!(
            "../../../tests/fixtures/always-on-cli/plan-oracle.json"
        )).expect("UTF-16 plan oracle");
        let JsValue::Array(cases) = fixture.get("records").expect("records") else { panic!("records array") };
        assert_eq!(cases.len(), 19);
        for case in cases {
            let id = case.get("id").and_then(JsValue::as_string).expect("id");
            let input = case.get("input").and_then(JsValue::as_string).expect("input");
            match parse_plan(input, 100_000, 200) {
                Ok(actual) => {
                    let expected = case.get("result").unwrap_or_else(|| panic!("{id:?}: expected failure"));
                    assert_eq!(Some(&JsValue::String(actual.title)), expected.get("title"), "title {id:?}");
                    assert_eq!(pilotdeck_protocol::stringify_json(&JsValue::Object(actual.metadata)).unwrap(),
                        pilotdeck_protocol::stringify_json(expected.get("metadata").unwrap()).unwrap(), "metadata {id:?}");
                    assert_eq!(pilotdeck_protocol::stringify_json(&JsValue::Object(actual.sections)).unwrap(),
                        pilotdeck_protocol::stringify_json(expected.get("sections").unwrap()).unwrap(), "sections {id:?}");
                    assert_eq!(Some(&JsValue::String(actual.raw)), expected.get("rawContent"), "raw {id:?}");
                },
                Err(actual) => {
                    let expected = case.get("error").unwrap_or_else(|| panic!("{id:?}: unexpected failure {actual:?}"));
                    assert!(expected.get("code").and_then(JsValue::as_string).is_some_and(|s| s.equals_str(actual.code())), "code {id:?}");
                    assert_eq!(Some(&JsValue::String(actual.message())), expected.get("message"), "message {id:?}");
                },
            }
        }
    }
}
