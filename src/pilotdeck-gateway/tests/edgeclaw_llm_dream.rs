//! #771 A6.6-edgeclaw-14: Dream-side LLM calls.
//!
//! Each oracle case replays the exact TS request sequence against a Tokio+Hyper
//! loopback listener. Requests (method/url/headers/body bytes) and the
//! normalized result/debug/validation values must match the TS recording.
//! `debug.parsedResult` is the raw model JSON *before*
//! `validateExclusiveClusters`, so the pre-validation output is asserted too.
#[path = "common/baseline.rs"]
mod baseline;
#[path = "support/llm_stub.rs"]
mod llm_stub;

use llm_stub::LlmStub;
use pilotdeck_gateway::runtime::{
    memory::edgeclaw::{
        dream::helpers::{validate_exclusive_clusters, validate_general_project_merge_groups},
        llm::transport::LlmMemoryExtractor,
    },
    model::http::HyperModelTransport,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    sync::{Arc, atomic::AtomicU64},
    time::Duration,
};

const ORACLE: &str = include_str!("fixtures/edgeclaw-llm-dream/oracle.json");

fn oracle() -> JsValue {
    baseline::oracle(ORACLE);
    parse_json(ORACLE).expect("#771 oracle.json must parse")
}

fn js(value: Value) -> JsValue {
    parse_json(&value.to_string()).expect("#771 JSON fixture value must parse")
}

fn text(value: &JsValue) -> String {
    value
        .as_string()
        .expect("#771 expected a string value")
        .to_utf8()
        .expect("#771 expected valid UTF-8")
}

fn array(value: &JsValue) -> &[JsValue] {
    match value {
        JsValue::Array(items) => items,
        other => panic!("#771 expected an array, got {other:?}"),
    }
}

fn string_field(value: &JsValue, key: &str) -> String {
    value
        .get(key)
        .and_then(JsValue::as_string)
        .unwrap_or_else(|| panic!("#771 missing string field {key}"))
        .to_utf8()
        .expect("#771 field must be UTF-8")
}

fn sorted(value: &JsValue, origin: &str) -> JsValue {
    match value {
        JsValue::String(value) => match value.to_utf8() {
            Ok(value) => JsValue::String(value.replace(origin, "$ORIGIN").into()),
            Err(_) => JsValue::String(value.clone()),
        },
        JsValue::Array(items) => {
            JsValue::Array(items.iter().map(|item| sorted(item, origin)).collect())
        }
        JsValue::Object(object) => {
            let mut entries = object.entries().to_vec();
            entries.sort_by(|a, b| a.0.units().cmp(b.0.units()));
            let mut out = JsObject::new();
            for (key, value) in entries {
                out.insert(key, sorted(&value, origin));
            }
            JsValue::Object(out)
        }
        _ => value.clone(),
    }
}

fn equal(actual: &JsValue, expected: &JsValue, origin: &str, message: &str) {
    assert_eq!(
        stringify_json(&sorted(actual, origin)).expect("#771 actual value must serialize"),
        stringify_json(&sorted(expected, origin)).expect("#771 expected value must serialize"),
        "{message}"
    );
}

fn steps(case: &JsValue) -> Vec<Value> {
    serde_json::from_str(
        &stringify_json(case.get("steps").expect("#771 case steps"))
            .expect("#771 steps must serialize")
            .expect("#771 steps must be a value"),
    )
    .expect("#771 steps must be JSON")
}

fn config(origin: &str) -> JsValue {
    js(json!({
        "agents": {"defaults": {"model": {"primary": "p/m"}}},
        "models": {"providers": {"p": {
            "api": "chat",
            "baseUrl": format!("{origin}/v1"),
            "apiKey": "fixture-key",
            "models": [{"id": "m"}]
        }}}
    }))
}

async fn invoke(
    extractor: &LlmMemoryExtractor,
    case: &JsValue,
    input: &JsObject,
) -> (JsValue, Vec<JsValue>) {
    let method = string_field(case, "fn");
    let mut debug = Vec::new();
    let outcome = match method.as_str() {
        "rewriteUserProfile" => {
            let candidate = extractor
                .rewrite_user_profile_with_debug(input, |event| debug.push(event))
                .await;
            let mut result = JsObject::new();
            result.insert(
                "result".into(),
                candidate.map_or(JsValue::Null, JsValue::Object),
            );
            JsValue::Object(result)
        }
        "planDreamClusters" => {
            let result = extractor
                .plan_dream_clusters_with_debug(input, |event| debug.push(event))
                .await;
            method_outcome(result)
        }
        "refineDreamCluster" => {
            let result = extractor
                .refine_dream_cluster_with_debug(input, |event| debug.push(event))
                .await;
            method_outcome(result)
        }
        "planGeneralProjectMetaMerges" => {
            let result = extractor
                .plan_general_project_meta_merges_with_debug(input, |event| debug.push(event))
                .await;
            method_outcome(result)
        }
        "reviewDreamProjectMeta" => {
            let result = extractor
                .review_dream_project_meta_with_debug(input, |event| debug.push(event))
                .await;
            method_outcome(result)
        }
        other => panic!("#771 oracle has an unknown method {other}"),
    };
    (outcome, debug)
}

fn method_outcome(
    result: Result<
        JsObject,
        pilotdeck_gateway::runtime::memory::edgeclaw::llm::transport::LlmError,
    >,
) -> JsValue {
    let mut outcome = JsObject::new();
    match result {
        Ok(value) => {
            outcome.insert("result".into(), JsValue::Object(value));
        }
        Err(error) => {
            let mut detail = JsObject::new();
            detail.insert("name".into(), JsValue::String(error.name.into()));
            detail.insert("message".into(), JsValue::String(error.message));
            outcome.insert("error".into(), JsValue::Object(detail));
        }
    };
    JsValue::Object(outcome)
}

fn check_validation(
    expected: &JsValue,
    input: &JsObject,
    outcome: &JsValue,
    origin: &str,
    name: &str,
) {
    if matches!(expected, JsValue::Null) {
        return;
    }
    let kind = string_field(expected, "kind");
    let actual = match kind.as_str() {
        "clusters" => {
            let result = outcome
                .get("result")
                .unwrap_or_else(|| panic!("#771 validation {name} needs a result"));
            let clusters = result
                .get("clusters")
                .expect("#771 cluster result must carry clusters");
            let allowed: HashSet<JsString> = array(
                input
                    .get("headers")
                    .expect("#771 cluster input must carry headers"),
            )
            .iter()
            .filter_map(|header| header.get("relativePath"))
            .filter_map(JsValue::as_string)
            .cloned()
            .collect();
            let validated = validate_exclusive_clusters(clusters, &allowed, 4.0)
                .unwrap_or_else(|error| panic!("#771 cluster validation {name}: {error:?}"));
            let mut wrapper = JsObject::new();
            wrapper.insert("kind".into(), JsValue::String("clusters".into()));
            wrapper.insert("result".into(), JsValue::Object(validated));
            JsValue::Object(wrapper)
        }
        "merge" => {
            let result = outcome
                .get("result")
                .unwrap_or_else(|| panic!("#771 validation {name} needs a result"));
            let groups = result
                .get("mergeGroups")
                .expect("#771 merge result must carry mergeGroups");
            let mut validator_input = JsObject::new();
            validator_input.insert(
                "metas".into(),
                input
                    .get("projectMetas")
                    .expect("#771 merge input must carry projectMetas")
                    .clone(),
            );
            validator_input.insert("groups".into(), groups.clone());
            let validated = validate_general_project_merge_groups(&validator_input)
                .unwrap_or_else(|error| panic!("#771 merge validation {name}: {error:?}"));
            let mut wrapper = JsObject::new();
            wrapper.insert("kind".into(), JsValue::String("merge".into()));
            wrapper.insert("result".into(), JsValue::Object(validated));
            JsValue::Object(wrapper)
        }
        other => panic!("#771 unknown validation kind {other}"),
    };
    let expected = expected
        .get("result")
        .expect("#771 expected validation must carry result");
    equal(
        actual.get("result").expect("#771 actual validation result"),
        expected,
        origin,
        &format!("#771 validation {name}"),
    );
}

#[tokio::test]
async fn wire_matrix() {
    let oracle = oracle();
    let cases = array(oracle.get("cases").expect("#771 oracle cases"));
    assert_eq!(cases.len(), 23, "#771 oracle case count");
    let mut seen = HashSet::new();
    for case in cases {
        let name = string_field(case, "name");
        seen.insert(string_field(case, "fn"));
        let input = case
            .get("input")
            .and_then(JsValue::as_object)
            .unwrap_or_else(|| panic!("#771 case {name} input"));
        let mut script = steps(case);
        if script.is_empty() {
            // Early-return cases expect no request. A fallback response turns a
            // mutant that wrongly calls the model into a request-count failure
            // instead of a server-task panic.
            script.push(json!({
                "status": 200,
                "body": "{\"choices\":[{\"message\":{\"content\":\"{}\"}}]}"
            }));
        }
        let server = LlmStub::start(script, Arc::new(AtomicU64::new(0))).await;
        let _closed = &server.closed;
        let extractor = LlmMemoryExtractor::new(
            config(&server.origin),
            Arc::new(HyperModelTransport::new()),
            Arc::new(()),
        );
        let (outcome, debug) =
            tokio::time::timeout(Duration::from_secs(5), invoke(&extractor, case, input))
                .await
                .unwrap_or_else(|_| panic!("#771 case {name} timed out"));
        let expected = case.get("expected").expect("#771 case expected");
        if let Some(result) = expected.get("result") {
            equal(
                outcome
                    .get("result")
                    .unwrap_or_else(|| panic!("#771 case {name} did not return a result")),
                result,
                &server.origin,
                &format!("#771 result {name}"),
            );
        } else {
            let error = outcome
                .get("error")
                .unwrap_or_else(|| panic!("#771 case {name} did not return an error"));
            equal(
                error,
                expected.get("error").expect("#771 case error"),
                &server.origin,
                &format!("#771 error {name}"),
            );
        }
        equal(
            &JsValue::Array(debug),
            expected.get("debug").expect("#771 case debug"),
            &server.origin,
            &format!("#771 debug {name}"),
        );
        let expected_requests = array(expected.get("requests").expect("#771 case requests"));
        {
            let actual_requests = server.requests.lock().expect("#771 stub request lock");
            assert_eq!(
                actual_requests.len(),
                expected_requests.len(),
                "#771 request count {name}"
            );
            for (index, (actual_request, expected_request)) in actual_requests
                .iter()
                .zip(expected_requests.iter())
                .enumerate()
            {
                let actual_request = js(actual_request.clone());
                assert_eq!(
                    text(actual_request.get("body").expect("#771 request body")),
                    text(
                        expected_request
                            .get("body")
                            .expect("#771 expected request body")
                    ),
                    "#771 request body {name}#{index}"
                );
                equal(
                    &actual_request,
                    expected_request,
                    &server.origin,
                    &format!("#771 request envelope {name}#{index}"),
                );
            }
        }
        if let Some(validation) = expected.get("validation") {
            check_validation(validation, input, &outcome, &server.origin, &name);
        }
        server.close().await;
    }
    assert_eq!(seen.len(), 5, "#771 every Dream LLM method must be covered");
}

#[test]
#[ignore]
fn regenerates_identically() {
    baseline::regenerates_identically("edgeclaw-llm-dream");
}
