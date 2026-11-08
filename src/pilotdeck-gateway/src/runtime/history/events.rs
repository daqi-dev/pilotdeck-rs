//! Supplemental durable history messages: status, error, compact and artifacts.
use super::project::ProjectionError;
use pilotdeck_protocol::{JsObject, JsString, JsValue};
fn f<'a>(v: &'a JsValue, k: &str) -> &'a JsValue {
    static U: JsValue = JsValue::Undefined;
    v.get(k).unwrap_or(&U)
}
fn is(v: &JsValue, s: &str) -> bool {
    v.as_string().is_some_and(|v| v.equals_str(s))
}
fn truth(v: &JsValue) -> bool {
    match v {
        JsValue::Undefined | JsValue::Null => false,
        JsValue::Bool(b) => *b,
        JsValue::Number(n) => *n != 0.0 && !n.is_nan(),
        JsValue::String(s) => !s.units().is_empty(),
        _ => true,
    }
}
fn id(key: &JsString, prefix: &str, parts: &[&JsValue]) -> JsValue {
    let mut u = key.units().to_vec();
    u.extend(prefix.encode_utf16());
    for (i, p) in parts.iter().enumerate() {
        if i > 0 {
            u.push(b'-' as u16);
        }
        match p {
            JsValue::String(s) => u.extend_from_slice(s.units()),
            JsValue::Number(n) => u.extend(crate::js_value::number_string(*n).encode_utf16()),
            JsValue::Undefined => u.extend("undefined".encode_utf16()),
            JsValue::Null => u.extend("null".encode_utf16()),
            _ => {}
        }
    }
    JsValue::String(JsString::from_utf16(u))
}
fn base(
    entry: &JsValue,
    key: &JsString,
    project: Option<&JsValue>,
    identifier: JsValue,
    role: JsValue,
    kind: JsValue,
) -> JsObject {
    let mut o = JsObject::new();
    for (k, v) in [
        ("id", identifier),
        ("sessionKey", JsValue::String(key.clone())),
        ("projectKey", project.cloned().unwrap_or(JsValue::Undefined)),
        ("createdAt", f(entry, "createdAt").clone()),
        ("provider", "pilotdeck".into()),
        ("role", role),
        ("kind", kind),
        ("turnId", f(entry, "turnId").clone()),
        ("sequence", f(entry, "sequence").clone()),
    ] {
        o.insert(k.into(), v);
    }
    o
}
fn source(o: &mut JsObject) {
    o.insert("source".into(), "history".into());
}
pub(super) fn insert_ordered(
    messages: &mut Vec<JsValue>,
    item: JsValue,
    budget: &mut super::budget::Budget,
) -> Result<usize, ProjectionError> {
    budget.charge(&item)?;
    let seq = f(&item, "sequence").as_number().filter(|n| n.is_finite());
    let at = if let Some(seq) = seq {
        messages
            .iter()
            .position(|v| {
                f(v, "sequence")
                    .as_number()
                    .is_some_and(|x| x.is_finite() && x > seq)
            })
            .unwrap_or(messages.len())
    } else {
        let stamp = f(&item, "createdAt")
            .as_string()
            .map(JsString::units)
            .unwrap_or_default();
        messages
            .iter()
            .rposition(|v| {
                f(v, "createdAt")
                    .as_string()
                    .is_some_and(|x| x.units() <= stamp)
            })
            .map_or(0, |i| i + 1)
    };
    messages.insert(at, item);
    Ok(at)
}
fn identifier(entry: &JsValue, key: &JsString, prefix: &str) -> JsValue {
    let v = f(entry, "entryId");
    if !matches!(v, JsValue::Null | JsValue::Undefined) {
        v.clone()
    } else {
        id(key, prefix, &[f(entry, "turnId"), f(entry, "sequence")])
    }
}
fn has_tool_result(entry: &JsValue) -> bool {
    match f(f(entry, "message"), "content") {
        JsValue::Array(v) => v
            .iter()
            .any(|b| is(f(b, "type"), "tool_result") || is(f(b, "type"), "tool_result_reference")),
        _ => false,
    }
}
fn descriptor(v: &JsValue) -> bool {
    matches!(v, JsValue::Object(_)) && f(v, "key").as_string().is_some()
}
// Durable decoder guarantees string turn ids; keep an equality fallback for
// directly supplied non-decoded values without penalizing valid transcripts.
#[derive(Default)]
struct TurnSet<'a> {
    strings: std::collections::HashSet<&'a JsString>,
    other: Vec<&'a JsValue>,
}
impl<'a> TurnSet<'a> {
    fn insert(&mut self, value: &'a JsValue) -> bool {
        if let Some(id) = value.as_string() {
            return self.strings.insert(id);
        }
        if self.other.contains(&value) {
            false
        } else {
            self.other.push(value);
            true
        }
    }
    fn contains(&self, value: &JsValue) -> bool {
        if let Some(id) = value.as_string() {
            self.strings.contains(id)
        } else {
            self.other.contains(&value)
        }
    }
}
pub(super) fn inject_compact(
    e: &JsValue,
    messages: &mut Vec<JsValue>,
    key: &JsString,
    project: Option<&JsValue>,
    budget: &mut super::budget::Budget,
) -> Result<(), ProjectionError> {
    let boundary = f(e, "boundary");
    let mut payload = JsObject::new();
    if is(f(boundary, "subtype"), "compact_boundary")
        && !f(boundary, "compactMetadata").is_undefined()
    {
        let m = f(boundary, "compactMetadata");
        if truth(f(m, "timeline")) {
            payload.insert("timeline".into(), f(m, "timeline").clone());
        }
        if f(m, "compactionId")
            .as_string()
            .is_some_and(|v| !v.units().is_empty())
        {
            payload.insert("compactionId".into(), f(m, "compactionId").clone());
        }
        for k in ["trigger", "preTokens", "postTokens", "messagesSummarized"] {
            payload.insert(k.into(), f(m, k).clone());
        }
        for k in [
            "targetTokens",
            "summaryGenerated",
            "checkpointMerged",
            "finalRatio",
        ] {
            let v = f(m, k);
            if (matches!(k, "targetTokens" | "finalRatio") && v.as_number().is_some())
                || (matches!(k, "summaryGenerated" | "checkpointMerged") && v.as_bool().is_some())
            {
                payload.insert(k.into(), v.clone());
            }
        }
        for k in ["level", "stage", "stageLabel"] {
            payload.insert(k.into(), f(m, k).clone());
        }
    }
    let mut o = base(
        e,
        key,
        project,
        identifier(e, key, "-compact-"),
        "system".into(),
        "compact_boundary".into(),
    );
    // timeline precedes turnId/sequence in the wire shape.
    if truth(payload.get("timeline").unwrap_or(&JsValue::Undefined)) {
        let mut r = JsObject::new();
        for (k, v) in o.entries() {
            if k.equals_str("turnId") {
                r.insert(
                    "timeline".into(),
                    payload
                        .get("timeline")
                        .cloned()
                        .unwrap_or(JsValue::Undefined),
                );
            }
            r.insert(k.clone(), v.clone());
        }
        o = r;
    }
    o.insert("text".into(), "Context compacted".into());
    o.insert("payload".into(), JsValue::Object(payload));
    source(&mut o);
    if truth(f(e, "entryId")) {
        o.insert("entryId".into(), f(e, "entryId").clone());
    }
    insert_ordered(messages, JsValue::Object(o), budget)?;
    Ok(())
}
/// Add entries after canonical messages using the TS history ordering contract.
pub(super) fn inject(
    entries: &[JsValue],
    messages: &mut Vec<JsValue>,
    key: &JsString,
    project: Option<&JsValue>,
    now: &JsValue,
    artifacts_enabled: bool,
    budget: &mut super::budget::Budget,
) -> Result<(), ProjectionError> {
    let mut durable = Vec::<JsValue>::new();
    let mut durable_seen = TurnSet::default();
    let mut completed = TurnSet::default();
    let mut visible_errors = TurnSet::default();
    let mut tools = TurnSet::default();
    for e in entries {
        let typ = f(e, "type");
        let turn = f(e, "turnId");
        if [
            "assistant_message",
            "tool_result_message",
            "durable_message",
        ]
        .iter()
        .any(|t| is(typ, t))
        {
            if durable_seen.insert(turn) {
                durable.push(turn.clone());
            }
            if has_tool_result(e) {
                tools.insert(turn);
            }
        }
        if is(typ, "turn_result") {
            completed.insert(turn);
        }
        if is(typ, "agent_status_message")
            && is(f(e, "kind"), "error")
            && f(f(e, "detail"), "visible") != &JsValue::Bool(false)
        {
            visible_errors.insert(turn);
        }
    }
    // Insertion families run in this order; equal sequence retains prior rows.
    for e in entries {
        if is(f(e, "type"), "control_boundary") && is(f(f(e, "boundary"), "kind"), "compact") {
            inject_compact(e, messages, key, project, budget)?;
        }
    }
    super::subagent::inject(entries, messages, budget)?;
    for e in entries {
        if !artifacts_enabled || !is(f(e, "type"), "file_artifacts") {
            continue;
        }
        let JsValue::Array(a) = f(e, "artifacts") else {
            return Err(ProjectionError("History artifacts must be array".into()));
        };
        let keep: Vec<_> = a
            .iter()
            .filter(|a| tools.contains(f(e, "turnId")) || !is(f(a, "source"), "workspace_diff"))
            .cloned()
            .collect();
        if keep.is_empty() {
            continue;
        }
        let mut o = base(
            e,
            key,
            project,
            identifier(e, key, "-file-artifacts-"),
            "assistant".into(),
            "file_artifacts".into(),
        );
        o.insert("artifacts".into(), JsValue::Array(keep));
        let mut p = JsObject::new();
        p.insert("turnId".into(), f(e, "turnId").clone());
        o.insert("payload".into(), JsValue::Object(p));
        source(&mut o);
        if truth(f(e, "entryId")) {
            o.insert("entryId".into(), f(e, "entryId").clone());
        }
        insert_ordered(messages, JsValue::Object(o), budget)?;
    }
    for e in entries {
        if !is(f(e, "type"), "agent_status_message") {
            continue;
        }
        let kind = f(e, "kind");
        let role = if is(kind, "error") { "error" } else { "system" };
        let mut o = base(
            e,
            key,
            project,
            identifier(e, key, "-agent-status-"),
            role.into(),
            kind.clone(),
        );
        o.insert("text".into(), f(e, "text").clone());
        let detail = f(e, "detail");
        for (src, dst) in [
            ("messageI18n", "contentI18n"),
            ("userHintI18n", "userHintI18n"),
        ] {
            let v = f(detail, src);
            if descriptor(v) {
                o.insert(dst.into(), v.clone());
            }
        }
        let mut p = JsObject::new();
        p.insert("event".into(), f(e, "event").clone());
        if truth(detail) {
            p.insert("detail".into(), detail.clone());
        }
        o.insert("payload".into(), JsValue::Object(p));
        source(&mut o);
        insert_ordered(messages, JsValue::Object(o), budget)?;
    }
    for e in entries {
        if !is(f(e, "type"), "turn_result")
            || !is(f(f(e, "result"), "type"), "error")
            || visible_errors.contains(f(e, "turnId"))
        {
            continue;
        }
        let result = f(e, "result");
        let mut units = Vec::new();
        if let JsValue::Array(errors) = f(result, "errors") {
            for err in errors {
                if let Some(text) = f(err, "message")
                    .as_string()
                    .filter(|s| !s.units().is_empty())
                {
                    if !units.is_empty() {
                        units.push(b'\n' as u16);
                    }
                    units.extend_from_slice(text.units());
                }
            }
        }
        if units.is_empty() {
            units.extend("Turn failed: ".encode_utf16());
            let reason = f(result, "stopReason");
            if let Some(s) = reason.as_string() {
                units.extend_from_slice(s.units());
            } else {
                units.extend("undefined".encode_utf16());
            }
        }
        let mut o = base(
            e,
            key,
            project,
            id(key, "-turn-error-", &[f(e, "turnId")]),
            "error".into(),
            "error".into(),
        );
        o.insert("text".into(), JsValue::String(JsString::from_utf16(units)));
        let mut p = JsObject::new();
        p.insert("code".into(), f(result, "stopReason").clone());
        p.insert("recoverable".into(), JsValue::Bool(false));
        o.insert("payload".into(), JsValue::Object(p));
        source(&mut o);
        insert_ordered(messages, JsValue::Object(o), budget)?;
    }
    let incomplete: Vec<_> = durable
        .into_iter()
        .filter(|v| !completed.contains(v))
        .collect();
    if !incomplete.is_empty() {
        let mut suffix = Vec::new();
        for (i, v) in incomplete.iter().enumerate() {
            if i > 0 {
                suffix.push(b'-' as u16);
            }
            if let Some(s) = v.as_string() {
                suffix.extend_from_slice(s.units());
            }
        }
        let mut identifier = key.units().to_vec();
        identifier.extend("-incomplete-turn-status-".encode_utf16());
        identifier.extend(suffix);
        let mut o = JsObject::new();
        for (k, v) in [
            ("id", JsValue::String(JsString::from_utf16(identifier))),
            ("sessionKey", JsValue::String(key.clone())),
            ("projectKey", project.cloned().unwrap_or(JsValue::Undefined)),
            ("createdAt", now.clone()),
            ("provider", "pilotdeck".into()),
            ("role", "system".into()),
            ("kind", "status".into()),
            (
                "text",
                "本轮记录尚未写入最终结果，已恢复当时产生的工具调用和输出。".into(),
            ),
        ] {
            o.insert(k.into(), v);
        }
        let mut p = JsObject::new();
        p.insert("incompleteTurnIds".into(), JsValue::Array(incomplete));
        o.insert("payload".into(), JsValue::Object(p));
        source(&mut o);
        let value = JsValue::Object(o);
        budget.charge(&value)?;
        messages.push(value);
    }
    Ok(())
}
