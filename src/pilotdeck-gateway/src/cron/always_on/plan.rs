use super::AlwaysOnError;
use serde_json::{Value, json};

const SECTIONS: [&str; 6] = [
    "Summary",
    "Rationale",
    "Context Signals",
    "Proposed Change",
    "Execution Steps",
    "Verification",
];
fn invalid(message: impl Into<String>) -> AlwaysOnError {
    AlwaysOnError::new("plan_invalid", message)
}

pub fn parse(content: &str) -> Result<(String, Value, String), AlwaysOnError> {
    let normalized = content.replace("\r\n", "\n").replace('\u{a0}', " ");
    if normalized.is_empty() || normalized.encode_utf16().count() > 100_000 {
        return Err(invalid(
            "plan content is empty or exceeds maxResultSizeChars (100000).",
        ));
    }
    let lines: Vec<&str> = normalized.lines().collect();
    let mut at = lines
        .iter()
        .position(|line| !line.trim().is_empty())
        .unwrap_or(lines.len());
    let title = lines
        .get(at)
        .and_then(|line| line.strip_prefix("# "))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| invalid("plan must start with a non-empty level-1 markdown heading."))?
        .to_owned();
    at += 1;
    while lines.get(at).is_some_and(|line| line.trim().is_empty()) {
        at += 1;
    }
    let mut metadata =
        json!({"id":"","sourceRunId":"","createdAt":"","projectRoot":"","dedupeKey":""});
    let first = lines
        .get(at)
        .and_then(|line| line.strip_prefix('>'))
        .map(str::trim);
    if first != Some("Always-On Discovery Plan") {
        return Err(invalid(
            "plan metadata blockquote first line must be Always-On Discovery Plan.",
        ));
    }
    at += 1;
    while let Some(line) = lines.get(at).and_then(|line| line.strip_prefix('>')) {
        let line = line.trim();
        if !line.is_empty() {
            let (key, value) = line
                .split_once(':')
                .ok_or_else(|| invalid("plan metadata must be in key: value form."))?;
            let key = key.trim();
            if metadata.get(key).is_none() {
                return Err(invalid(format!(
                    "plan metadata contains unknown key {key}."
                )));
            }
            metadata[key] = json!(value.trim());
        }
        at += 1;
    }
    if metadata
        .as_object()
        .expect("object")
        .values()
        .any(|v| v.as_str().is_none_or(str::is_empty))
    {
        return Err(invalid("plan metadata is missing a required key."));
    }
    let mut sections: Vec<(String, Vec<&str>)> = Vec::new();
    for line in &lines[at..] {
        if let Some(heading) = line.strip_prefix("## ") {
            let heading = heading.trim();
            if heading.is_empty() || sections.iter().any(|(old, _)| old == heading) {
                return Err(invalid("plan contains an empty or duplicate section."));
            }
            sections.push((heading.into(), Vec::new()));
        } else if line.starts_with("# ") {
            return Err(invalid(
                "plan must contain exactly one level-1 heading at the top.",
            ));
        } else if let Some((_, content)) = sections.last_mut() {
            content.push(line);
        }
    }
    if sections
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>()
        != SECTIONS
    {
        return Err(invalid(
            "plan must contain exactly the required sections in order.",
        ));
    }
    let text = |i: usize| sections[i].1.join("\n").trim().to_owned();
    let summary = text(0);
    if summary.is_empty()
        || summary.encode_utf16().count() > 200
        || regex::Regex::new(r"\n\s*\n")
            .expect("constant")
            .is_match(&summary)
    {
        return Err(invalid(
            "plan Summary must be one non-empty paragraph of at most 200 characters.",
        ));
    }
    if text(1).is_empty() || text(3).is_empty() {
        return Err(invalid(
            "plan Rationale and Proposed Change must not be empty.",
        ));
    }
    if regex::Regex::new(r"(?i)^\s*TODO(?-u:\b)|^\s*待补充(?-u:\b)")
        .expect("constant")
        .is_match(&text(3))
    {
        return Err(invalid("plan Proposed Change must not be fuzzy."));
    }
    let bullet = regex::Regex::new(r"^\s*-\s+").expect("constant");
    if [2, 5]
        .iter()
        .any(|i| !sections[*i].1.iter().any(|line| bullet.is_match(line)))
    {
        return Err(invalid(
            "plan Context Signals and Verification require an unordered list item.",
        ));
    }
    let ordered = regex::Regex::new(r"^\d+\.\s+").expect("constant");
    let unordered = regex::Regex::new(r"^[-*+]\s+").expect("constant");
    if !sections[4]
        .1
        .iter()
        .any(|line| ordered.is_match(line.trim()))
        || sections[4]
            .1
            .iter()
            .any(|line| unordered.is_match(line.trim()))
    {
        return Err(invalid(
            "plan Execution Steps must use an ordered list (no unordered items).",
        ));
    }
    Ok((title, metadata, format!("{}\n", normalized.trim_end())))
}
