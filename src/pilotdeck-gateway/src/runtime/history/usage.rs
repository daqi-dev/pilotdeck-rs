//! The last context/compact/turn token budget in a durable history.
use pilotdeck_protocol::{JsObject, JsValue};
fn f<'a>(v: &'a JsValue, k: &str) -> &'a JsValue {
    static U: JsValue = JsValue::Undefined;
    v.get(k).unwrap_or(&U)
}
fn is(v: &JsValue, s: &str) -> bool {
    v.as_string().is_some_and(|v| v.equals_str(s))
}
fn pos(v: &JsValue) -> Option<f64> {
    v.as_number().filter(|n| n.is_finite() && *n > 0.0)
}
fn number(o: &mut JsObject, k: &str, n: f64) {
    o.insert(k.into(), JsValue::Number(n));
}
/// Options are supplied by the configured native host when known.
pub fn token_usage(
    entries: &[JsValue],
    max_context: Option<f64>,
    max_output: Option<f64>,
) -> Option<JsValue> {
    let mut budget: Option<(usize, JsObject)> = None;
    let mut compact: Option<(usize, JsValue)> = None;
    let mut turn: Option<JsValue> = None;
    for (index, e) in entries.iter().enumerate() {
        if is(f(e, "type"), "agent_status_message") && is(f(e, "event"), "context_budget") {
            let d = f(e, "detail");
            if let (Some(used), Some(total)) = (
                pos(f(d, "displayUsed")).or_else(|| pos(f(d, "used"))),
                pos(f(d, "total")),
            ) {
                let mut o = JsObject::new();
                number(&mut o, "used", used);
                for k in ["displayUsed", "budgetUsed"] {
                    if let Some(v) = pos(f(d, k)) {
                        number(&mut o, k, v);
                    }
                }
                number(&mut o, "total", total);
                number(
                    &mut o,
                    "effectiveTotal",
                    pos(f(d, "effectiveTotal")).unwrap_or(total),
                );
                number(
                    &mut o,
                    "reservedOutputTokens",
                    pos(f(d, "reservedOutputTokens")).unwrap_or(0.0),
                );
                if f(d, "state").as_string().is_some() {
                    o.insert("state".into(), f(d, "state").clone());
                }
                if let Some(r) = f(d, "ratio").as_number().filter(|n| n.is_finite()) {
                    number(&mut o, "ratio", r);
                }
                o.insert("source".into(), "history".into());
                o.insert("exact".into(), JsValue::Bool(true));
                budget = Some((index, o));
                continue;
            }
        }
        let b = f(e, "boundary");
        let m = f(b, "compactMetadata");
        if is(f(e, "type"), "control_boundary")
            && is(f(b, "kind"), "compact")
            && is(f(b, "subtype"), "compact_boundary")
            && pos(f(m, "postTokens")).is_some()
        {
            compact = Some((index, m.clone()));
            continue;
        }
        if is(f(e, "type"), "turn_result") {
            let usage = f(f(e, "result"), "usage");
            if [
                "inputTokens",
                "outputTokens",
                "cacheReadTokens",
                "cacheWriteTokens",
                "totalTokens",
            ]
            .iter()
            .any(|k| pos(f(usage, k)).is_some())
            {
                turn = Some(usage.clone());
            }
        }
    }
    if let Some((i, m)) = compact
        && budget.as_ref().is_none_or(|(at, _)| i > *at)
    {
        let prev = budget.as_ref().map(|(_, o)| o);
        let used = pos(f(&m, "postTokens"))?.ceil();
        let total = prev
            .and_then(|o| o.get("total"))
            .and_then(pos)
            .or(max_context.filter(|n| n.is_finite() && *n > 0.0))
            .unwrap_or(200000.0);
        let reserved = prev
            .and_then(|o| o.get("reservedOutputTokens"))
            .and_then(pos)
            .or(max_output.filter(|n| n.is_finite() && *n > 0.0))
            .unwrap_or(0.0);
        let effective = prev
            .and_then(|o| o.get("effectiveTotal"))
            .and_then(pos)
            .unwrap_or((total - reserved).max(1.0));
        let ratio = used / effective;
        let mut o = JsObject::new();
        for (k, n) in [
            ("used", used),
            ("displayUsed", used),
            ("budgetUsed", used),
            ("total", total),
            ("effectiveTotal", effective),
            ("reservedOutputTokens", reserved),
            ("ratio", ratio),
        ] {
            number(&mut o, k, n);
        }
        o.insert(
            "state".into(),
            (if ratio >= 0.95 {
                "blocking"
            } else if ratio >= 0.8 {
                "warning"
            } else {
                "ok"
            })
            .into(),
        );
        o.insert("source".into(), "compact".into());
        o.insert("exact".into(), JsValue::Bool(false));
        o.insert("compacted".into(), JsValue::Bool(true));
        for (a, b) in [
            ("preTokens", "preCompactUsed"),
            ("messagesSummarized", "messagesSummarized"),
        ] {
            if let Some(n) = pos(f(&m, a)) {
                number(&mut o, b, n);
            }
        }
        return Some(JsValue::Object(o));
    }
    if let Some((_, value)) = budget {
        return Some(JsValue::Object(value));
    }
    let v = turn?;
    let input = pos(f(&v, "inputTokens"));
    let output = pos(f(&v, "outputTokens")).unwrap_or(0.0);
    let read = pos(f(&v, "cacheReadTokens")).unwrap_or(0.0);
    let write = pos(f(&v, "cacheWriteTokens")).unwrap_or(0.0);
    let total_tokens = pos(f(&v, "totalTokens"));
    let used = if let Some(n) = input {
        (n + read + write).ceil()
    } else {
        (total_tokens? - output).ceil().max(0.0)
    };
    if used <= 0.0 {
        return None;
    }
    let total = max_context
        .filter(|n| n.is_finite() && *n > 0.0)
        .unwrap_or(200000.0);
    let reserved = max_output
        .filter(|n| n.is_finite() && *n > 0.0)
        .unwrap_or(0.0);
    let effective = (total - reserved).max(1.0);
    let mut o = JsObject::new();
    for (k, n) in [
        ("used", used),
        ("total", total),
        ("effectiveTotal", effective),
        ("reservedOutputTokens", reserved),
    ] {
        number(&mut o, k, n);
    }
    o.insert("source".into(), "history".into());
    o.insert("exact".into(), JsValue::Bool(true));
    let mut b = JsObject::new();
    for (k, n) in [
        ("input", input.unwrap_or(0.0)),
        ("cacheRead", read),
        ("cacheWrite", write),
        ("output", output),
        ("total", total_tokens.unwrap_or((used + output).ceil())),
    ] {
        number(&mut b, k, n);
    }
    o.insert("breakdown".into(), JsValue::Object(b));
    Some(JsValue::Object(o))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pilotdeck_protocol::{parse_json, stringify_json};
    #[test]
    fn observed_token_usage_matches_ts() {
        let oracle = parse_json(include_str!(
            "../../../tests/fixtures/native-history/typical-r1.json"
        ))
        .unwrap();
        let Some(JsValue::Array(cases)) = oracle.get("cases") else {
            panic!("cases")
        };
        let case = cases
            .iter()
            .find(|x| is(f(x, "id"), "token-usage"))
            .unwrap();
        let JsValue::Array(entries) = f(case, "loadedEntries") else {
            panic!("entries")
        };
        let actual = token_usage(entries, Some(100.0), Some(10.0)).unwrap();
        let actual = parse_json(&stringify_json(&actual).unwrap().unwrap()).unwrap();
        assert_eq!(&actual, f(f(case, "result"), "tokenUsage"));
    }
}
