//! Real TS oracle for A6.6-edgeclaw-9 retrieval helpers.
use icu_collator::{Collator, CollatorBorrowed, CollatorPreferences};
use icu_locale_core::Locale;
use pilotdeck_gateway::runtime::memory::edgeclaw::retrieval::{
    MemoryMessage, ReadableProjectCatalogEntry, RecallHeaderEntry, StepOptions,
    build_project_shortlist, build_trace_id, create_trace, expand_cjk_search_token,
    fallback_selection, normalize_query_key, push_step, recent_user_messages, render_context,
    render_project_meta_block, render_selected_records_block, render_user_summary_block,
    tokenize_search_text,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};

#[path = "common/baseline.rs"]
mod baseline;
const ORACLE: &str = include_str!("fixtures/edgeclaw-retrieval-helpers/oracle.json");

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
    match stringify_json(value).unwrap() {
        Some(text) => text,
        None => "undefined".into(),
    }
}

fn group(name: &str) -> &'static str {
    match name {
        "normalizeQueryKey" | "buildTraceId" => "text",
        "renderUserSummaryBlock"
        | "renderProjectMetaBlock"
        | "renderSelectedRecordsBlock"
        | "renderContext" => "render",
        "recentUserMessages" | "fallbackSelection" => "selection",
        "expandCjkSearchToken" | "tokenizeSearchText" => "tokenize",
        "buildProjectShortlist" => "shortlist",
        "createTrace" | "traceScenario" => "trace",
        other => panic!("oracle has an unknown function {other}"),
    }
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

fn objects(case: &JsValue, index: usize) -> Vec<JsObject> {
    items(arg(case, index))
        .iter()
        .map(|item| item.as_object().unwrap().clone())
        .collect()
}

fn optional_object(value: &JsValue) -> Option<&JsObject> {
    match value {
        JsValue::Undefined | JsValue::Null => None,
        other => Some(other.as_object().expect("object argument")),
    }
}

fn object(value: &JsValue) -> &JsObject {
    value.as_object().expect("object argument")
}

fn string_array(values: &[JsString]) -> JsValue {
    JsValue::Array(values.iter().cloned().map(JsValue::String).collect())
}

fn collator() -> CollatorBorrowed<'static> {
    let locale: Locale = "en-US".parse().unwrap();
    let preferences: CollatorPreferences = locale.into();
    Collator::try_new(preferences, Default::default()).unwrap()
}

fn trace_scenario(case: &JsValue, clock_ms: i64) -> JsValue {
    let scenario = object(arg(case, 0));
    let query = scenario.get("query").and_then(JsValue::as_string).unwrap();
    let mode = scenario.get("mode").and_then(JsValue::as_string).unwrap();
    let token = scenario.get("token").and_then(JsValue::as_string).unwrap();
    let mut trace = create_trace(query, mode, clock_ms, token);
    for step in items(scenario.get("steps").unwrap()) {
        let step = step.as_object().unwrap();
        let options = step.get("options").and_then(JsValue::as_object).unwrap();
        let option = |name: &str| options.get(name).cloned();
        let step_options = StepOptions {
            refs: option("refs"),
            metrics: option("metrics"),
            details: option("details"),
            prompt_debug: option("promptDebug"),
            title_i18n: option("titleI18n"),
            input_summary_i18n: option("inputSummaryI18n"),
            output_summary_i18n: option("outputSummaryI18n"),
        };
        let field = |name: &str| step.get(name).and_then(JsValue::as_string).unwrap();
        push_step(
            &mut trace,
            field("kind"),
            field("title"),
            field("status"),
            field("inputSummary"),
            field("outputSummary"),
            &step_options,
        );
    }
    JsValue::Object(trace)
}

fn call(case: &JsValue, clock_ms: i64) -> JsValue {
    let name = case
        .get("fn")
        .unwrap()
        .as_string()
        .unwrap()
        .to_utf8()
        .unwrap();
    match name.as_str() {
        "normalizeQueryKey" => JsValue::String(normalize_query_key(text(case, 0))),
        "buildTraceId" => JsValue::String(build_trace_id(
            text(case, 0),
            text(case, 1),
            clock_ms,
            text(case, 2),
        )),
        "renderUserSummaryBlock" => string_array(&render_user_summary_block(object(arg(case, 0)))),
        "renderProjectMetaBlock" => {
            string_array(&render_project_meta_block(optional_object(arg(case, 0))))
        }
        "renderSelectedRecordsBlock" => {
            string_array(&render_selected_records_block(&objects(case, 0)))
        }
        "renderContext" => JsValue::String(render_context(
            text(case, 0),
            object(arg(case, 1)),
            optional_object(arg(case, 2)),
            &objects(case, 3),
        )),
        "recentUserMessages" => {
            let messages: Option<Vec<MemoryMessage>> = match arg(case, 0) {
                JsValue::Undefined | JsValue::Null => None,
                value => Some(
                    items(value)
                        .iter()
                        .map(|item| item.as_object().unwrap().clone())
                        .collect(),
                ),
            };
            JsValue::Array(
                recent_user_messages(messages.as_deref())
                    .into_iter()
                    .map(JsValue::Object)
                    .collect(),
            )
        }
        "expandCjkSearchToken" => string_array(&expand_cjk_search_token(text(case, 0))),
        "tokenizeSearchText" => string_array(&tokenize_search_text(text(case, 0))),
        "buildProjectShortlist" => {
            let catalog: Vec<ReadableProjectCatalogEntry> = objects(case, 0);
            let messages: Vec<MemoryMessage> = objects(case, 2);
            let collator = collator();
            JsValue::Array(
                build_project_shortlist(&catalog, text(case, 1), &messages, &collator)
                    .into_iter()
                    .map(JsValue::Object)
                    .collect(),
            )
        }
        "createTrace" => JsValue::Object(create_trace(
            text(case, 0),
            text(case, 1),
            clock_ms,
            text(case, 2),
        )),
        "traceScenario" => trace_scenario(case, clock_ms),
        "fallbackSelection" => {
            let manifest: Vec<RecallHeaderEntry> = objects(case, 1);
            string_array(&fallback_selection(text(case, 0), &manifest))
        }
        other => panic!("missing dispatch for {other}"),
    }
}

fn replay(wanted: &str) {
    let oracle = oracle();
    let clock_ms = oracle.get("clockMs").unwrap().as_number().unwrap() as i64;
    let mut seen = 0usize;
    for case in items(oracle.get("cases").unwrap()) {
        let name = case
            .get("fn")
            .unwrap()
            .as_string()
            .unwrap()
            .to_utf8()
            .unwrap();
        if group(&name) != wanted {
            continue;
        }
        seen += 1;
        let decoded = decode(case);
        let actual = call(&decoded, clock_ms);
        let expected = decoded.get("expected").unwrap();
        assert_eq!(
            wire(&actual),
            wire(expected),
            "{name} args={:?}",
            decoded.get("args").unwrap()
        );
    }
    assert!(seen > 0, "no {wanted} cases in the oracle");
}

#[test]
fn normalize_and_trace_id_match_ts() {
    replay("text");
}

#[test]
fn render_context_matches_ts() {
    replay("render");
}

#[test]
fn selection_and_empty_result_match_ts() {
    replay("selection");
}

#[test]
fn cjk_tokenization_matches_ts() {
    replay("tokenize");
}

#[test]
fn project_shortlist_matches_ts() {
    replay("shortlist");
}

#[test]
fn trace_and_steps_match_ts() {
    replay("trace");
}

#[test]
#[ignore = "requires fixed PDRS_BASELINE with Node 22 and tsx"]
fn fixture_regenerates_identically() {
    baseline::regenerates_identically("edgeclaw-retrieval-helpers");
}
