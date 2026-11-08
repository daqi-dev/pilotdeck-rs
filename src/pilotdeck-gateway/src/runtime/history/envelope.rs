use super::{entries, page, project::ProjectionError, session, usage};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
fn f<'a>(v: &'a JsValue, k: &str) -> &'a JsValue {
    static U: JsValue = JsValue::Undefined;
    v.get(k).unwrap_or(&U)
}
fn nullish(v: &JsValue) -> bool {
    matches!(v, JsValue::Undefined | JsValue::Null)
}
pub struct Options<'a> {
    pub project_root: &'a JsValue,
    pub now: &'a JsValue,
    pub catalog: Option<&'a JsValue>,
    pub include_artifacts: bool,
    pub max_context: Option<f64>,
    pub max_output: Option<f64>,
}
/// Project decoded durable entries into a history response before stream merge.
pub fn project(
    entries: &[JsValue],
    input: &JsValue,
    options: &Options<'_>,
) -> Result<JsValue, ProjectionError> {
    let key = f(input, "sessionKey")
        .as_string()
        .ok_or_else(|| ProjectionError("History sessionKey must be a string".into()))?;
    let project = input.get("projectKey");
    let all = entries::messages_with_artifacts(
        entries,
        key,
        project,
        options.now,
        options.include_artifacts,
    )?;
    let (range, cursor) = page::page(all.len(), input)?;
    let total = all.len();
    let selected: Vec<JsValue> = all
        .into_iter()
        .skip(range.start)
        .take(range.end.saturating_sub(range.start))
        .collect();
    let background = f(input, "sessionKind").as_string().is_some_and(|s| s.equals_str("background_task"))
        && f(input, "relativeTranscriptPath").as_string().is_some_and(|s| !s.units().is_empty());
    let fallback = if background { JsValue::Undefined } else { session::fallback(entries, key, options.project_root) };
    let source = if background { &fallback } else { options.catalog.unwrap_or(&fallback) };
    // Reserve each copied field before constructing metadata: long transcript
    // titles and summaries can otherwise multiply beyond the frame budget.
    let mut response_budget = super::budget::Budget::default();
    for message in &selected { response_budget.charge(message)?; }
    for value in [f(source, "sessionId"), &JsValue::String(key.clone()), f(source, "summary"), f(source, "lastModified"), f(source, "fileSize"), f(source, "customTitle"), f(source, "aiTitle"), f(source, "firstPrompt"), f(source, "cwd"), f(source, "tag"), f(source, "createdAt"), f(source, "parentSessionId"), f(input, "parentSessionId"), f(input, "relativeTranscriptPath"), f(source, "forkedFromTurnId")] {
        response_budget.charge(value)?;
    }
    // Account for object syntax and a small RPC/stream envelope reserve.
    response_budget.reserve(1024)?;
    let mut metadata = JsObject::new();
    let session_id = f(source, "sessionId");
    metadata.insert(
        "sessionId".into(),
        if nullish(session_id) {
            JsValue::String(key.clone())
        } else {
            session_id.clone()
        },
    );
    metadata.insert("sessionKey".into(), JsValue::String(key.clone()));
    let summary = f(source, "summary");
    metadata.insert(
        "summary".into(),
        if nullish(summary) {
            JsValue::String(key.clone())
        } else {
            summary.clone()
        },
    );
    let modified = f(source, "lastModified");
    metadata.insert(
        "lastModified".into(),
        if nullish(modified) {
            JsValue::Number(0.0)
        } else {
            modified.clone()
        },
    );
    for field in [
        "fileSize",
        "customTitle",
        "aiTitle",
        "firstPrompt",
        "cwd",
        "tag",
        "createdAt",
    ] {
        metadata.insert(field.into(), f(source, field).clone());
    }
    if background { metadata.insert("sessionKind".into(), "background_task".into()); }
    let parent = f(input, "parentSessionId");
    metadata.insert(
        "parentSessionId".into(),
        if nullish(parent) {
            f(source, "parentSessionId").clone()
        } else {
            parent.clone()
        },
    );
    metadata.insert(
        "relativeTranscriptPath".into(),
        f(input, "relativeTranscriptPath").clone(),
    );
    metadata.insert(
        "forkedFromTurnId".into(),
        f(source, "forkedFromTurnId").clone(),
    );
    let mut result = JsObject::new();
    result.insert("messages".into(), JsValue::Array(selected));
    result.insert(
        "nextCursor".into(),
        cursor.map_or(JsValue::Undefined, |v| JsValue::String(JsString::from(v))),
    );
    result.insert("total".into(), JsValue::Number(total as f64));
    result.insert(
        "tokenUsage".into(),
        usage::token_usage(entries, options.max_context, options.max_output)
            .unwrap_or(JsValue::Undefined),
    );
    result.insert("session".into(), JsValue::Object(metadata));
    Ok(JsValue::Object(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pilotdeck_protocol::{parse_json, stringify_json};
    fn fixture(raw: &str) {
        let oracle = parse_json(raw).unwrap();
        let JsValue::Array(cases) = f(&oracle, "cases") else {
            panic!("cases")
        };
        for case in cases {
            let name = f(case, "id")
                .as_string()
                .and_then(|s| s.to_utf8().ok())
                .unwrap_or_default();
            let JsValue::Array(entries) = f(case, "loadedEntries") else {
                // Path rejection occurs before decoding and is tested by reader.
                assert!(case.get("error").is_some(), "{name}: entries");
                continue;
            };
            let input = f(case, "input");
            let config = f(case, "options");
            let root = f(config, "projectRoot");
            let home = f(config, "pilotHome");
            let spec = f(case, "spec");
            let key = f(input, "sessionKey");
            let safe = key
                .as_string()
                .map(crate::storage::sanitize_session_id)
                .map(JsString::from);
            let catalog = match f(spec, "catalog") {
                JsValue::Array(rows) => rows.iter().find(|row| {
                    f(row, "sessionId") == key
                        || safe
                            .as_ref()
                            .is_some_and(|s| f(row, "sessionId") == &JsValue::String(s.clone()))
                }),
                _ => None,
            };
            let opts = Options {
                project_root: root,
                now: f(config, "now"),
                catalog,
                include_artifacts: root != home,
                max_context: f(config, "maxContextTokens").as_number(),
                max_output: f(config, "maxOutputTokens").as_number(),
            };
            let actual = project(entries, input, &opts);
            if let Some(error) = case.get("error") {
                let err = actual.unwrap_err();
                assert_eq!(
                    err.0,
                    f(error, "message").as_string().unwrap().to_utf8().unwrap(),
                    "{name}"
                );
                continue;
            }
            let actual = actual.unwrap_or_else(|error| panic!("{name}: {error}"));
            let actual = parse_json(&stringify_json(&actual).unwrap().unwrap()).unwrap();
            assert_eq!(&actual, f(case, "result"), "{name}");
        }
    }
    #[test]
    fn complete_history_matches_ts_goldens() {
        fixture(include_str!(
            "../../../tests/fixtures/native-history/typical-r1.json"
        ));
        fixture(include_str!(
            "../../../tests/fixtures/native-history/boundary-r1.json"
        ));
        fixture(include_str!(
            "../../../tests/fixtures/native-history/extra-r1.json"
        ));
        fixture(include_str!(
            "../../../tests/fixtures/native-history/cursor-image-r1.json"
        ));
        fixture(include_str!("../../../tests/fixtures/native-history/coercion-r2.json"));
        fixture(include_str!("../../../tests/fixtures/native-history/background-r3.json"));
    }
}
