//! Always-On report normalization over JavaScript UTF-16 strings.
use pilotdeck_protocol::{JsObject, JsString, JsValue};

const SECTIONS: [&str; 7] = [
    "Plan Reference",
    "Steps Performed",
    "Files Changed",
    "Command Output",
    "Verification Results",
    "Follow-ups",
    "Notes",
];

#[derive(Clone)]
pub(crate) struct ReportMetadata {
    pub run_id: JsString,
    pub plan_id: JsString,
    pub started_at: JsString,
    pub finished_at: JsString,
    pub outcome: JsString,
    pub workspace_strategy: JsString,
    pub workspace_handle: JsString,
}

pub(crate) struct ParsedReport {
    pub title: JsString,
    pub sections: JsObject,
    pub fallbacks: Vec<JsString>,
    pub raw: JsString,
}

// ECMAScript WhiteSpace and LineTerminator code units, used by trim and /\s/u.
fn whitespace(c: u16) -> bool {
    matches!(c, 0x0009..=0x000d | 0x0020 | 0x00a0 | 0x1680 | 0x2000..=0x200a
        | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff)
}
fn trim(v: &[u16]) -> &[u16] {
    let first = v.iter().position(|c| !whitespace(*c)).unwrap_or(v.len());
    let end = v
        .iter()
        .rposition(|c| !whitespace(*c))
        .map_or(first, |i| i + 1);
    &v[first..end]
}
fn starts(v: &[u16], ascii: &str) -> bool {
    let bytes = ascii.as_bytes();
    v.len() >= bytes.len() && v.iter().zip(bytes).all(|(a, b)| *a == u16::from(*b))
}
fn section(name: &[u16]) -> bool {
    SECTIONS
        .iter()
        .any(|s| name.iter().copied().eq(s.encode_utf16()))
}
fn append(buf: &mut Vec<u16>, text: &str) {
    buf.extend(text.encode_utf16());
}
fn append_value(buf: &mut Vec<u16>, value: &JsString) {
    buf.extend_from_slice(value.units());
}

fn save(sections: &mut JsObject, name: Option<JsString>, buffer: &mut Vec<u16>) {
    if let Some(name) = name {
        while buffer.last().is_some_and(|c| whitespace(*c)) {
            buffer.pop();
        }
        // Ordinary JS object assignment to __proto__ with a primitive is ignored.
        if !name.equals_str("__proto__") {
            sections.insert(
                name,
                JsValue::String(JsString::from_utf16(std::mem::take(buffer))),
            );
        }
    }
    buffer.clear();
}

pub(crate) fn parse_report(content: &JsString, metadata: &ReportMetadata) -> ParsedReport {
    let mut normalized = Vec::with_capacity(content.len_utf16());
    let mut source = content.units().iter().copied().peekable();
    while let Some(c) = source.next() {
        if c == 13 && source.peek() == Some(&10) {
            source.next();
            normalized.push(10);
        } else {
            normalized.push(c);
        }
    }
    let lines: Vec<&[u16]> = normalized.split(|c| *c == 10).collect();
    let mut at = 0;
    while at < lines.len() && trim(lines[at]).is_empty() {
        at += 1;
    }
    let mut fallbacks = Vec::new();
    let line = lines.get(at).copied().unwrap_or(&[]);
    let title = if starts(line, "# ") && !section(trim(&line[2..])) {
        at += 1;
        JsString::from_utf16(trim(&line[2..]).to_vec())
    } else {
        fallbacks.push(JsString::from("title-missing"));
        JsString::from("Always-On Discovery Run")
    };
    while at < lines.len() && trim(lines[at]).is_empty() {
        at += 1;
    }
    while at < lines.len() && starts(lines[at], ">") {
        at += 1;
    }
    while at < lines.len() && trim(lines[at]).is_empty() {
        at += 1;
    }
    let mut sections = JsObject::new();
    let mut current = None;
    let mut body = Vec::new();
    let mut body_lines = 0usize;
    for line in &lines[at..] {
        let h2 = starts(line, "## ");
        let h1 = starts(line, "# ") && section(trim(&line[2..]));
        if h2 || h1 {
            save(&mut sections, current.take(), &mut body);
            body_lines = 0;
            let name = JsString::from_utf16(trim(&line[if h2 { 3 } else { 2 }..]).to_vec());
            if h1 {
                let mut f = "h1-downgraded(".encode_utf16().collect::<Vec<_>>();
                f.extend_from_slice(name.units());
                f.push(u16::from(b')'));
                fallbacks.push(JsString::from_utf16(f));
            }
            current = Some(name);
        } else if current.is_some() {
            if body_lines > 0 {
                body.push(10);
            }
            body.extend_from_slice(line);
            body_lines += 1;
        }
    }
    save(&mut sections, current, &mut body);
    for name in SECTIONS {
        if !sections.contains_key(name) {
            let text = if name == "Notes" {
                format!("- fallback: section-missing({name})")
            } else {
                "(empty)".into()
            };
            sections.insert(name.into(), JsValue::String(text.as_str().into()));
            fallbacks.push(format!("section-missing({name})").as_str().into());
        }
    }
    if !fallbacks.is_empty() {
        let notes = sections
            .get("Notes")
            .and_then(JsValue::as_string)
            .expect("notes");
        let mut lines: Vec<Vec<u16>> = notes
            .units()
            .split(|c| *c == 10)
            .map(<[u16]>::to_vec)
            .collect();
        for fallback in &fallbacks {
            let mut line = "- fallback: ".encode_utf16().collect::<Vec<_>>();
            line.extend_from_slice(fallback.units());
            if !lines.contains(&line) {
                lines.push(line);
            }
        }
        let mut text = Vec::new();
        for (index, line) in lines.iter().enumerate() {
            if index > 0 {
                text.push(10);
            }
            text.extend_from_slice(line);
        }
        sections.insert("Notes".into(), JsValue::String(JsString::from_utf16(text)));
    }
    let raw = rebuild(&title, metadata, &sections);
    ParsedReport {
        title,
        sections,
        fallbacks,
        raw,
    }
}

fn rebuild(title: &JsString, m: &ReportMetadata, sections: &JsObject) -> JsString {
    let mut out = Vec::new();
    append(&mut out, "# ");
    append_value(&mut out, title);
    append(&mut out, "\n\n> Always-On Discovery Run Report\n> runId: ");
    append_value(&mut out, &m.run_id);
    append(&mut out, "\n> planId: ");
    append_value(&mut out, &m.plan_id);
    append(&mut out, "\n> startedAt: ");
    append_value(&mut out, &m.started_at);
    append(&mut out, "\n> finishedAt: ");
    append_value(&mut out, &m.finished_at);
    append(&mut out, "\n> outcome: ");
    append_value(&mut out, &m.outcome);
    append(&mut out, "\n> workspaceStrategy: ");
    append_value(&mut out, &m.workspace_strategy);
    append(&mut out, "\n> workspaceHandle: ");
    append_value(&mut out, &m.workspace_handle);
    for name in SECTIONS {
        append(&mut out, "\n\n## ");
        append(&mut out, name);
        append(&mut out, "\n");
        if let Some(value) = sections.get(name).and_then(JsValue::as_string) {
            append_value(&mut out, value);
        }
    }
    for (name, value) in sections.own_entries() {
        if section(name.units()) {
            continue;
        }
        append(&mut out, "\n\n## ");
        append_value(&mut out, name);
        append(&mut out, "\n");
        if let Some(value) = value.as_string() {
            append_value(&mut out, value);
        }
    }
    out.push(10);
    JsString::from_utf16(out)
}

pub(crate) fn fallback_report(
    metadata: &ReportMetadata,
    title: &JsString,
    reason: &JsString,
    partial: Option<&JsString>,
) -> JsString {
    let mut out = Vec::new();
    append(&mut out, "# ");
    append_value(&mut out, title);
    append(
        &mut out,
        " - Work Report\n\n> Always-On Discovery Run Report\n> runId: ",
    );
    append_value(&mut out, &metadata.run_id);
    append(&mut out, "\n> planId: ");
    append_value(&mut out, &metadata.plan_id);
    append(&mut out, "\n> startedAt: ");
    append_value(&mut out, &metadata.started_at);
    append(&mut out, "\n> finishedAt: ");
    append_value(&mut out, &metadata.finished_at);
    append(&mut out, "\n> outcome: ");
    append_value(&mut out, &metadata.outcome);
    append(&mut out, "\n> workspaceStrategy: ");
    append_value(&mut out, &metadata.workspace_strategy);
    append(&mut out, "\n> workspaceHandle: ");
    append_value(&mut out, &metadata.workspace_handle);
    append(
        &mut out,
        "\n\n## Plan Reference\n(unavailable: report tool was not invoked)\n\n## Steps Performed\n(empty)\n\n## Files Changed\n(none recorded)\n\n## Command Output\n(none recorded)\n\n## Verification Results\n- [ ] (unverified) - report tool was not invoked\n\n## Follow-ups\n- Investigate why AlwaysOnReportTool was not called.\n\n## Notes\n- fallback: ",
    );
    append_value(&mut out, reason);
    if let Some(text) = partial.map(|s| trim(s.units())).filter(|s| !s.is_empty()) {
        append(&mut out, "\n\n## Partial Tool Payload\n");
        out.extend_from_slice(text);
    }
    out.push(10);
    JsString::from_utf16(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_typescript_report_contract_oracle() {
        let fixture = pilotdeck_protocol::parse_json(include_str!(
            "../../../tests/fixtures/always-on-cli/report-oracle.json"
        ))
        .expect("real TS report oracle parses with UTF-16 codec");
        let meta = fixture.get("metadata").expect("metadata");
        let field = |name| {
            meta.get(name)
                .and_then(JsValue::as_string)
                .expect("metadata string")
                .clone()
        };
        let metadata = ReportMetadata {
            run_id: field("runId"),
            plan_id: field("planId"),
            started_at: field("startedAt"),
            finished_at: field("finishedAt"),
            outcome: field("outcome"),
            workspace_strategy: field("workspaceStrategy"),
            workspace_handle: field("workspaceHandle"),
        };
        let JsValue::Array(cases) = fixture.get("records").expect("records") else {
            panic!("records array")
        };
        assert_eq!(cases.len(), 13);
        for case in cases {
            let id = case.get("id").and_then(JsValue::as_string).expect("id");
            if id.equals_str("fallback") {
                let actual = fallback_report(
                    &metadata,
                    &"Demo".into(),
                    &"failed".into(),
                    Some(&" partial ".into()),
                );
                assert_eq!(
                    Some(&JsValue::String(actual)),
                    case.get("result"),
                    "fallback"
                );
                continue;
            }
            let input = case
                .get("input")
                .and_then(JsValue::as_string)
                .expect("input");
            let expected = case.get("result").expect("result");
            let actual = parse_report(input, &metadata);
            assert_eq!(
                Some(&JsValue::String(actual.title)),
                expected.get("title"),
                "title {id:?}"
            );
            assert_eq!(
                pilotdeck_protocol::stringify_json(&JsValue::Object(actual.sections)).unwrap(),
                pilotdeck_protocol::stringify_json(expected.get("sections").expect("sections"))
                    .unwrap(),
                "sections {id:?}"
            );
            assert_eq!(
                Some(&JsValue::Array(
                    actual.fallbacks.into_iter().map(JsValue::String).collect()
                )),
                expected.get("fallbacks"),
                "fallbacks {id:?}"
            );
            assert_eq!(
                Some(&JsValue::String(actual.raw)),
                expected.get("rawContent"),
                "raw {id:?}"
            );
        }
    }
}
