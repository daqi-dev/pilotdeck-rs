use super::*;
use crate::{metadata::js_trim, runtime::memory::js_is_space_unit};
use sha1::{Digest, Sha1};
use std::collections::HashSet;

pub(super) fn obj(fields: &[(&str, JsValue)]) -> JsObject {
    let mut o = JsObject::new();
    for (k, v) in fields {
        o.insert((*k).into(), v.clone());
    }
    o
}
pub(super) fn s(v: impl AsRef<str>) -> JsValue {
    JsValue::from(v.as_ref())
}
pub(super) fn num(v: usize) -> JsValue {
    JsValue::Number(v as f64)
}
pub(super) fn text(o: &JsObject, k: &str) -> String {
    o.get(k)
        .and_then(JsValue::as_string)
        .map(|s| String::from_utf16_lossy(s.units()))
        .unwrap_or_default()
}
pub(super) fn flag(o: &JsObject, k: &str) -> bool {
    matches!(o.get(k), Some(JsValue::Bool(true)))
}
pub(super) fn strings(v: &[String]) -> JsValue {
    JsValue::Array(v.iter().map(s).collect())
}
pub(super) fn objects(v: Vec<JsObject>) -> JsValue {
    JsValue::Array(v.into_iter().map(JsValue::Object).collect())
}
pub(super) fn array(o: &JsObject, k: &str) -> Vec<String> {
    match o.get(k) {
        Some(JsValue::Array(a)) => a
            .iter()
            .filter_map(|v| v.as_string())
            .map(|s| String::from_utf16_lossy(s.units()))
            .collect(),
        _ => vec![],
    }
}
pub(super) fn norm_js(value: &JsString) -> JsString {
    let mut out = vec![];
    let mut space = false;
    for &u in value.units() {
        if js_is_space_unit(u) {
            space = !out.is_empty();
        } else {
            if space {
                out.push(32);
            }
            out.push(u);
            space = false;
        }
    }
    JsString::from_utf16(out)
}
pub(super) fn unique_js(values: impl IntoIterator<Item = JsString>) -> Vec<JsString> {
    let mut seen = HashSet::new();
    values
        .into_iter()
        .map(|v| norm_js(&v))
        .filter(|v| !v.is_empty() && seen.insert(v.clone()))
        .take(50)
        .collect()
}
pub(super) fn unique_array_value(o: &JsObject, k: &str) -> JsValue {
    let values = match o.get(k) {
        Some(JsValue::Array(a)) => a.iter().filter_map(|v| v.as_string().cloned()).collect(),
        _ => vec![],
    };
    JsValue::Array(unique_js(values).into_iter().map(JsValue::String).collect())
}
pub(super) fn unique_array(o: &JsObject, k: &str) -> Vec<String> {
    match unique_array_value(o, k) {
        JsValue::Array(ref a) => a
            .iter()
            .filter_map(JsValue::as_string)
            .map(|v| String::from_utf16_lossy(v.units()))
            .collect(),
        _ => vec![],
    }
}
pub(super) fn query_matches(entry: &JsObject, query: &JsObject) -> bool {
    let query = query
        .get("query")
        .and_then(JsValue::as_string)
        .cloned()
        .unwrap_or_default();
    let query = crate::model_catalog::lower(
        &norm_js(&query),
        &icu_locale_core::LanguageIdentifier::UNKNOWN,
        false,
    );
    if query.is_empty() {
        return true;
    }
    let haystack = JsString::from(format!(
        "{} {} {}",
        text(entry, "name"),
        text(entry, "description"),
        text(entry, "relativePath")
    ));
    let haystack = crate::model_catalog::lower(
        &haystack,
        &icu_locale_core::LanguageIdentifier::UNKNOWN,
        false,
    );
    haystack
        .units()
        .windows(query.len_utf16())
        .any(|w| w == query.units())
}
fn user_identity(c: &JsObject) -> Vec<String> {
    let raw = ["profile", "summary", "description"]
        .iter()
        .filter_map(|k| c.get(k).and_then(JsValue::as_string))
        .find(|s| !s.is_empty())
        .cloned()
        .unwrap_or_default();
    let punctuation = "\r\n，,；;。.!?".encode_utf16().collect::<Vec<_>>();
    let mut values = raw
        .units()
        .split(|u| punctuation.contains(u))
        .map(|u| norm_js(&JsString::from_utf16(u.to_vec())))
        .filter(|v| v.len_utf16() >= 2)
        .collect::<Vec<_>>();
    if let Some(JsValue::Array(a)) = c.get("relationships") {
        values.extend(a.iter().filter_map(|v| v.as_string().cloned()));
    }
    unique_js(values)
        .iter()
        .map(|s| String::from_utf16_lossy(s.units()))
        .collect()
}
pub(super) fn norm(v: &str) -> String {
    let mut out = String::new();
    let mut space = false;
    for c in v.chars() {
        if u16::try_from(c as u32).is_ok_and(js_is_space_unit) {
            space = !out.is_empty();
        } else {
            if space {
                out.push(' ');
            }
            out.push(c);
            space = false;
        }
    }
    out
}
pub(super) fn n(o: &JsObject, k: &str) -> String {
    norm(&text(o, k))
}
pub(super) fn first(values: &[String]) -> String {
    values
        .iter()
        .find(|v| !v.is_empty())
        .cloned()
        .unwrap_or_default()
}
pub(super) fn desc(value: &str, fallback: &str) -> String {
    first(&[norm(value), norm(fallback)])
}
pub(super) fn unique(values: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    values
        .into_iter()
        .map(|v| norm(&v))
        .filter(|v| !v.is_empty() && seen.insert(v.clone()))
        .take(50)
        .collect()
}
pub(super) fn slug(value: &str) -> String {
    let v = norm(value).to_lowercase();
    let mut result = String::new();
    let mut dash = false;
    for c in v.chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() || ('\u{4e00}'..='\u{9fff}').contains(&c) {
            if dash && !result.is_empty() {
                result.push('-');
            }
            result.push(c);
            dash = false;
        } else {
            dash = true;
        }
    }
    if result.is_empty() {
        "memory-item".into()
    } else {
        result
    }
}
pub(super) fn hash(value: &str) -> String {
    format!("{:x}", Sha1::digest(value.as_bytes()))[..10].into()
}
pub(super) fn copy(o: &mut JsObject, source: &JsObject, keys: &[&str]) {
    for key in keys {
        if let Some(v) = source.get(key) {
            o.insert((*key).into(), v.clone());
        }
    }
}
pub(super) fn parse(raw: &str, now: &str) -> Option<(JsObject, String)> {
    if !raw.starts_with("---\n") {
        return None;
    }
    let end = raw[4..].find("\n---\n")? + 4;
    let values = header(&raw[4..end]);
    if !["user", "project", "feedback", "general_project_meta"]
        .contains(&text(&values, "type").as_str())
        || !["global", "project"].contains(&text(&values, "scope").as_str())
    {
        return None;
    }
    let mut fm = obj(&[
        ("name", s(text(&values, "name"))),
        ("description", s(text(&values, "description"))),
        ("type", s(text(&values, "type"))),
        ("scope", s(text(&values, "scope"))),
    ]);
    for (disk, key) in FM_FIELDS {
        let value = text(&values, disk);
        if *key == "updatedAt" {
            fm.insert(
                (*key).into(),
                s(if values.contains_key(disk) {
                    value
                } else {
                    now.into()
                }),
            );
        } else if !value.is_empty() {
            fm.insert((*key).into(), s(value));
        }
    }
    let dep = text(&values, "deprecated");
    if dep == "true" || dep == "false" {
        fm.insert("deprecated".into(), JsValue::Bool(dep == "true"));
    }
    // Number.parseInt accepts a decimal prefix (including a leading sign).
    let attempts = text(&values, "dream_attempts");
    let prefix: String = attempts
        .chars()
        .enumerate()
        .take_while(|(i, c)| c.is_ascii_digit() || (*i == 0 && (*c == '+' || *c == '-')))
        .map(|(_, c)| c)
        .collect();
    if let Ok(value) = prefix.parse::<f64>()
        && value.is_finite()
    {
        fm.insert("dreamAttempts".into(), JsValue::Number(value));
    }
    Some((fm, raw[end + 5..].trim_start_matches('\n').into()))
}
pub(super) const FM_FIELDS: &[(&str, &str)] = &[
    ("project_id", "projectId"),
    ("source_kind", "sourceKind"),
    ("source_workspace_path", "sourceWorkspacePath"),
    ("source_project_id", "sourceProjectId"),
    ("updated_at", "updatedAt"),
    ("dream_updated_at", "dreamUpdatedAt"),
    ("captured_at", "capturedAt"),
    ("source_session_key", "sourceSessionKey"),
];
pub(super) fn header(raw: &str) -> JsObject {
    let mut o = JsObject::new();
    for line in raw.replace("\r\n", "\n").split('\n') {
        if let Some((k, v)) = line.split_once(':')
            && !k.is_empty()
        {
            o.insert(js_trim(k).into(), s(js_trim(v)));
        }
    }
    o
}
pub(super) fn render(fm: &JsObject, body: &str) -> String {
    let mut lines = vec![
        "---".into(),
        format!("name: {}", n(fm, "name")),
        format!(
            "description: {}",
            desc(&text(fm, "description"), &text(fm, "name"))
        ),
        format!("type: {}", text(fm, "type")),
        format!("scope: {}", text(fm, "scope")),
    ];
    for (disk, key) in FM_FIELDS {
        let v = text(fm, key);
        if *key == "updatedAt" || !v.is_empty() {
            lines.push(format!("{disk}: {v}"));
        }
    }
    if let Some(JsValue::Bool(v)) = fm.get("deprecated") {
        lines.push(format!("deprecated: {v}"));
    }
    if let Some(JsValue::Number(v)) = fm.get("dreamAttempts") {
        lines.push(format!(
            "dream_attempts: {}",
            pilotdeck_protocol::number::number_to_string(*v)
        ));
    }
    lines.push("---".into());
    lines.push(js_trim(body).into());
    format!("{}\n", lines.join("\n"))
}
pub(super) fn sections(body: &str) -> BTreeMap<String, Vec<String>> {
    let mut result = BTreeMap::new();
    let mut key = String::new();
    let mut lines = vec![];
    for line in js_trim(body).replace("\r\n", "\n").split('\n') {
        if let Some(rest) = line.strip_prefix("##")
            && rest
                .chars()
                .next()
                .is_some_and(|c| u16::try_from(c as u32).is_ok_and(js_is_space_unit))
            && !js_trim(rest).is_empty()
        {
            if !key.is_empty() {
                result.insert(key, lines);
            }
            key = js_trim(rest).to_lowercase();
            lines = vec![];
        } else if !key.is_empty() {
            lines.push(line.to_string());
        }
    }
    if !key.is_empty() {
        result.insert(key, lines);
    }
    result
}
pub(super) fn para(sections: &BTreeMap<String, Vec<String>>, key: &str) -> String {
    sections
        .get(key)
        .map(|v| norm(&v.join(" ")))
        .unwrap_or_default()
}
fn strip_list(v: &str) -> &str {
    js_trim(v)
        .strip_prefix('-')
        .map(js_trim)
        .unwrap_or_else(|| js_trim(v))
}
pub(super) fn list(sections: &BTreeMap<String, Vec<String>>, key: &str) -> Vec<String> {
    unique(
        sections
            .get(key)
            .into_iter()
            .flatten()
            .map(|v| strip_list(v).into())
            .collect(),
    )
}
pub(super) fn split_facts(v: &str) -> Vec<String> {
    unique(
        v.split(['\r', '\n', '，', ',', '；', ';', '。', '.', '!', '?'])
            .map(norm)
            .filter(|v| v.encode_utf16().count() >= 2)
            .collect(),
    )
}
pub(super) fn facts(sections: &BTreeMap<String, Vec<String>>) -> Vec<String> {
    unique(
        sections
            .get("身份背景")
            .into_iter()
            .flatten()
            .flat_map(|line| {
                if js_trim(line).starts_with('-') {
                    vec![strip_list(line).into()]
                } else {
                    split_facts(line)
                }
            })
            .collect(),
    )
}
pub(super) fn body(c: &JsObject) -> String {
    if !n(c, "body").is_empty() {
        return format!("{}\n", js_trim(&text(c, "body")));
    }
    let kind = text(c, "type");
    let mut lines: Vec<String> = vec![];
    match kind.as_str() {
        "user" => {
            let values = user_identity(c);
            lines.push("## 身份背景".into());
            if values.is_empty() {
                lines.push("- 暂无稳定用户画像信息。".into());
            } else {
                lines.extend(values.iter().map(|v| format!("- {v}")));
            }
        }
        "feedback" => {
            lines.extend([
                "## Rule".into(),
                norm(&first(&[
                    text(c, "rule"),
                    text(c, "description"),
                    text(c, "summary"),
                    text(c, "name"),
                ])),
                "".into(),
            ]);
            for (key, title) in [("why", "Why"), ("howToApply", "How To Apply")] {
                if !n(c, key).is_empty() {
                    lines.extend([format!("## {title}"), n(c, key), "".into()]);
                }
            }
            push_list(&mut lines, "Notes", unique_array(c, "notes"));
        }
        "general_project_meta" => {
            lines.extend([
                "## Summary".into(),
                desc(&text(c, "description"), &text(c, "name")),
                "".into(),
                "## Status".into(),
                first(&[n(c, "stage"), "in_progress".into()]),
                "".into(),
            ]);
            let mut notes = vec![];
            if text(c, "sourceKind") == "workspace_external_mirror" {
                notes.push("This project mirrors a read-only external workspace project.".into());
            }
            for (key, label) in [
                ("sourceWorkspacePath", "Source workspace"),
                ("sourceProjectId", "Source project id"),
            ] {
                if !text(c, key).is_empty() {
                    notes.push(format!("{label}: {}", text(c, key)));
                }
            }
            push_list(&mut lines, "Source", unique(notes));
        }
        _ => {
            if !n(c, "stage").is_empty() {
                lines.extend(["## Current Stage".into(), n(c, "stage"), "".into()]);
            }
            for (key, title) in [
                ("decisions", "Decisions"),
                ("constraints", "Constraints"),
                ("nextSteps", "Next Steps"),
                ("blockers", "Blockers"),
                ("timeline", "Timeline"),
                ("notes", "Notes"),
            ] {
                push_list(&mut lines, title, unique_array(c, key));
            }
            if !n(c, "summary").is_empty() {
                lines.extend(["## Summary".into(), n(c, "summary"), "".into()]);
            }
        }
    }
    format!("{}\n", js_trim(&lines.join("\n")))
}
fn push_list(lines: &mut Vec<String>, title: &str, values: Vec<String>) {
    if !values.is_empty() {
        lines.push(format!("## {title}"));
        lines.extend(values.iter().map(|v| format!("- {v}")));
        lines.push("".into());
    }
}
pub(super) fn candidate_description(c: &JsObject) -> String {
    let fallback = match text(c, "type").as_str() {
        "user" => first(&[text(c, "profile"), text(c, "summary"), text(c, "name")]),
        "feedback" => first(&[text(c, "rule"), text(c, "summary"), text(c, "name")]),
        _ => first(&[
            text(c, "summary"),
            text(c, "stage"),
            unique_array(c, "blockers")
                .first()
                .cloned()
                .unwrap_or_default(),
            unique_array(c, "decisions")
                .first()
                .cloned()
                .unwrap_or_default(),
            text(c, "name"),
        ]),
    };
    desc(&text(c, "description"), &fallback)
}
pub(super) fn frontmatter(c: &JsObject, old: &JsObject, now: &str) -> JsObject {
    let kind = text(c, "type");
    let scope = if kind == "user" {
        "global".into()
    } else {
        first(&[text(c, "scope"), "project".into()])
    };
    let mut fm = obj(&[
        (
            "name",
            s(first(&[n(c, "name"), n(old, "name"), "memory-item".into()])),
        ),
        (
            "description",
            s(first(&[
                candidate_description(c),
                n(old, "description"),
                "memory-item".into(),
            ])),
        ),
        ("type", s(&kind)),
        ("scope", s(&scope)),
    ]);
    if scope == "project" {
        fm.insert(
            "projectId".into(),
            s(first(&[
                n(c, "projectId"),
                n(old, "projectId"),
                CURRENT_PROJECT_ID.into(),
            ])),
        );
    }
    for key in ["sourceKind", "sourceWorkspacePath", "sourceProjectId"] {
        let value = first(&[
            if kind == "general_project_meta" {
                text(c, key)
            } else {
                String::new()
            },
            text(old, key),
        ]);
        if !value.is_empty() {
            fm.insert(key.into(), s(value));
        }
    }
    fm.insert("updatedAt".into(), s(now));
    copy(
        &mut fm,
        old,
        &["dreamUpdatedAt", "deprecated", "dreamAttempts"],
    );
    for key in ["capturedAt", "sourceSessionKey"] {
        let value = first(&[text(c, key), text(old, key)]);
        if !value.is_empty() {
            fm.insert(key.into(), s(value));
        }
    }
    fm
}
pub(super) fn to_candidate(r: &JsObject) -> JsObject {
    let kind = text(r, "type");
    let content = text(r, "content");
    let sec = sections(&content);
    let mut c = obj(&[
        ("type", s(&kind)),
        (
            "scope",
            s(if kind == "user" { "global" } else { "project" }),
        ),
        ("name", s(text(r, "name"))),
        ("description", s(text(r, "description"))),
        ("body", s(&content)),
    ]);
    copy(&mut c, r, &["projectId", "capturedAt", "sourceSessionKey"]);
    match kind.as_str() {
        "general_project_meta" => {
            copy(
                &mut c,
                r,
                &["sourceKind", "sourceWorkspacePath", "sourceProjectId"],
            );
            c.insert("stage".into(), s(para(&sec, "status")));
        }
        "user" => {
            let f = facts(&sec);
            c.insert("profile".into(), s(first(&[f.join("；"), norm(&content)])));
            c.insert("relationships".into(), strings(&f));
        }
        "feedback" => {
            for (key, title) in [
                ("rule", "rule"),
                ("why", "why"),
                ("howToApply", "how to apply"),
            ] {
                c.insert(key.into(), s(para(&sec, title)));
            }
            c.insert("notes".into(), strings(&list(&sec, "notes")));
        }
        _ => {
            for (key, title) in [("stage", "current stage"), ("summary", "summary")] {
                c.insert(key.into(), s(para(&sec, title)));
            }
            for (key, title) in [
                ("decisions", "decisions"),
                ("constraints", "constraints"),
                ("nextSteps", "next steps"),
                ("blockers", "blockers"),
                ("timeline", "timeline"),
                ("notes", "notes"),
            ] {
                c.insert(key.into(), strings(&list(&sec, title)));
            }
        }
    }
    c
}
pub(super) fn merge(primary: &JsObject, incoming: &JsObject) -> JsObject {
    let mut c = primary.clone();
    let kind = text(primary, "type");
    if kind != text(incoming, "type") {
        return c;
    }
    c.insert(
        "description".into(),
        s(desc(
            &text(incoming, "description"),
            &text(primary, "description"),
        )),
    );
    let (scalar, arrays): (&[&str], &[&str]) = match kind.as_str() {
        "user" => (
            &["profile"],
            &["preferences", "constraints", "relationships"],
        ),
        "feedback" => (&["name", "rule", "why", "howToApply"], &["notes"]),
        "general_project_meta" => (
            &[
                "projectId",
                "name",
                "stage",
                "sourceKind",
                "sourceWorkspacePath",
                "sourceProjectId",
            ],
            &[],
        ),
        _ => (
            &["name", "summary", "stage"],
            &[
                "decisions",
                "constraints",
                "nextSteps",
                "blockers",
                "timeline",
                "notes",
            ],
        ),
    };
    for key in scalar {
        let mut value = first(&[text(incoming, key), text(primary, key)]);
        if *key == "profile" {
            value = first(&[
                value,
                text(primary, "description"),
                text(incoming, "description"),
            ]);
        }
        if *key == "rule" {
            value = first(&[
                value,
                text(incoming, "description"),
                text(primary, "description"),
            ]);
        }
        c.insert((*key).into(), s(norm(&value)));
    }
    for key in arrays {
        let mut values = array(primary, key);
        values.extend(array(incoming, key));
        c.insert((*key).into(), strings(&unique(values)));
    }
    c
}
pub(super) fn read(path: &Path) -> Result<String> {
    Ok(String::from_utf8_lossy(&fs::read(path)?).into_owned())
}
pub(super) fn preview(body: &str) -> JsValue {
    static HEADING: OnceLock<regex::Regex> = OnceLock::new();
    let re = HEADING.get_or_init(|| regex::Regex::new(r"(?m)^#+[\t\n\x0B\x0C\r \u{00a0}\u{1680}\u{2000}-\u{200a}\u{2028}\u{2029}\u{202f}\u{205f}\u{3000}\u{feff}]+").expect("constant regex"));
    let normalized = norm(&re.replace_all(body, ""));
    let units: Vec<u16> = normalized.encode_utf16().collect();
    if units.len() <= 220 {
        s(normalized)
    } else {
        let mut u = units[..220].to_vec();
        u.extend("...".encode_utf16());
        JsValue::String(JsString::from_utf16(u))
    }
}

pub(super) fn remove(o: &mut JsObject, key: &str) {
    let mut next = JsObject::new();
    for (k, v) in o.entries() {
        if !k.equals_str(key) {
            next.insert(k.clone(), v.clone());
        }
    }
    *o = next;
}

pub(super) fn utf16_prefix(value: &str, length: usize) -> String {
    String::from_utf16_lossy(&value.encode_utf16().take(length).collect::<Vec<_>>())
}

pub(super) fn normalize_status(value: &str) -> String {
    first(&[norm(value), "in_progress".into()])
}

pub(super) fn norm_field(o: &JsObject, key: &str) -> JsString {
    norm_js(
        &o.get(key)
            .and_then(JsValue::as_string)
            .cloned()
            .unwrap_or_default(),
    )
}
pub(super) fn input_error(prefix: &str, input: &JsObject, key: &str) -> FileMemoryError {
    let mut units = prefix.encode_utf16().collect::<Vec<_>>();
    if let Some(value) = input.get(key).and_then(JsValue::as_string) {
        units.extend(value.units());
    }
    FileMemoryError::InvalidUtf16(JsString::from_utf16(units))
}
