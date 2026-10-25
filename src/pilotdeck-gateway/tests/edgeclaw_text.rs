//! Real TS oracle for A6.6-edgeclaw-1 pure helpers.
use pilotdeck_gateway::runtime::memory::edgeclaw::text::{
    CleanedRecallSections, build_external_project_logical_id, build_external_record_id,
    build_fact_id, build_l0_index_id, build_l1_index_id, build_l2_project_index_id,
    build_l2_time_index_id, build_link_id, build_memory_recall_system_context,
    decode_escaped_unicode_text, decode_escaped_unicode_value, general_workspace_dir_in,
    get_workspace_memory_mode_in, hash_text, is_external_record_id, is_general_workspace_dir_in,
    is_general_workspace_dir_with_home, looks_like_raw_recall_context,
    normalize_case_trace_context_preview, normalize_text, normalize_workspace_path,
    normalize_workspace_path_from, now_iso_at, parse_external_record_id, parse_raw_recall_sections,
    safe_json_parse, score_match, trace_i18n, truncate,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use std::path::Path;

#[path = "common/baseline.rs"]
mod baseline;
const ORACLE: &str = include_str!("fixtures/edgeclaw-text/oracle.json");

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

fn expand(value: &JsValue) -> JsValue {
    match value {
        JsValue::String(text) => {
            let cwd = std::env::current_dir()
                .unwrap()
                .to_string_lossy()
                .into_owned();
            JsValue::String(replace_units(text, "$CWD", &cwd))
        }
        JsValue::Array(items) => JsValue::Array(items.iter().map(expand).collect()),
        JsValue::Object(object) => {
            let mut out = JsObject::new();
            for (key, item) in object.entries() {
                out.insert(key.clone(), expand(item));
            }
            JsValue::Object(out)
        }
        other => other.clone(),
    }
}

fn replace_units(text: &JsString, from: &str, to: &str) -> JsString {
    let from: Vec<u16> = from.encode_utf16().collect();
    let to: Vec<u16> = to.encode_utf16().collect();
    let units = text.units();
    if from.is_empty() {
        return text.clone();
    }
    let mut out = Vec::new();
    let mut i = 0;
    while i < units.len() {
        if units[i..].starts_with(&from) {
            out.extend(&to);
            i += from.len();
        } else {
            out.push(units[i]);
            i += 1;
        }
    }
    JsString::from_utf16(out)
}

fn wire(value: &JsValue) -> String {
    match stringify_json(value).unwrap() {
        Some(text) => text,
        None => "undefined".into(),
    }
}

fn group(name: &str) -> &'static str {
    match name {
        "truncate"
        | "decodeEscapedUnicodeText"
        | "decodeEscapedUnicodeValue"
        | "normalizeText"
        | "scoreMatch"
        | "safeJsonParse" => "text",
        "hashText"
        | "nowIso"
        | "buildL0IndexId"
        | "buildL1IndexId"
        | "buildL2TimeIndexId"
        | "buildL2ProjectIndexId"
        | "buildFactId"
        | "buildLinkId" => "ids",
        "normalizeWorkspacePath"
        | "generalWorkspaceDir"
        | "generalWorkspaceDirEmptyHome"
        | "isGeneralWorkspaceDir"
        | "isGeneralWorkspaceDirEmptyHome"
        | "getWorkspaceMemoryMode"
        | "getWorkspaceMemoryModeEmptyHome"
        | "buildExternalProjectLogicalId"
        | "buildExternalRecordId"
        | "parseExternalRecordId"
        | "isExternalRecordId" => "general",
        "traceI18n" => "trace",
        "parseRawRecallSections"
        | "buildMemoryRecallSystemContext"
        | "looksLikeRawRecallContext" => "recall",
        "normalizeCaseTraceContextPreview" => "preview",
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

fn number(case: &JsValue, index: usize) -> f64 {
    arg(case, index).as_number().expect("number argument")
}

fn boolean(case: &JsValue, index: usize) -> bool {
    arg(case, index).as_bool().expect("boolean argument")
}

fn strings(case: &JsValue, index: usize) -> Vec<JsString> {
    items(arg(case, index))
        .iter()
        .map(|item| item.as_string().unwrap().clone())
        .collect()
}

fn sections_value(sections: CleanedRecallSections) -> JsValue {
    let mut object = JsObject::new();
    for (key, bodies) in [
        ("userProfile", sections.user_profile),
        ("projectMeta", sections.project_meta),
        ("projectMemory", sections.project_memory),
        ("feedbackMemory", sections.feedback_memory),
    ] {
        object.insert(
            key.into(),
            JsValue::Array(bodies.into_iter().map(JsValue::String).collect()),
        );
    }
    JsValue::Object(object)
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
        "truncate" => JsValue::String(truncate(text(case, 0), number(case, 1))),
        "decodeEscapedUnicodeText" => {
            JsValue::String(decode_escaped_unicode_text(text(case, 0), boolean(case, 1)))
        }
        "decodeEscapedUnicodeValue" => decode_escaped_unicode_value(arg(case, 0), boolean(case, 1)),
        "normalizeText" => JsValue::String(normalize_text(text(case, 0))),
        "scoreMatch" => JsValue::Number(score_match(text(case, 0), text(case, 1))),
        "safeJsonParse" => safe_json_parse(text(case, 0), arg(case, 1).clone()),
        "hashText" => JsValue::String(JsString::from(hash_text(text(case, 0)))),
        "nowIso" => JsValue::String(JsString::from(now_iso_at(clock_ms))),
        "buildL0IndexId" => JsValue::String(build_l0_index_id(
            text(case, 0),
            text(case, 1),
            text(case, 2),
        )),
        "buildL1IndexId" => JsValue::String(build_l1_index_id(text(case, 0), &strings(case, 1))),
        "buildL2TimeIndexId" => JsValue::String(build_l2_time_index_id(text(case, 0))),
        "buildL2ProjectIndexId" => JsValue::String(build_l2_project_index_id(text(case, 0))),
        "buildFactId" => JsValue::String(build_fact_id(text(case, 0))),
        "buildLinkId" => JsValue::String(build_link_id(
            text(case, 0),
            text(case, 1),
            text(case, 2),
            text(case, 3),
        )),
        "normalizeWorkspacePath" => {
            JsValue::String(normalize_workspace_path(text(case, 0)).unwrap())
        }
        "generalWorkspaceDir" => {
            JsValue::String(general_workspace_dir_in(Path::new("/fixture/home")))
        }
        "generalWorkspaceDirEmptyHome" => JsValue::String(general_workspace_dir_in(Path::new(""))),
        "isGeneralWorkspaceDir" => JsValue::Bool(
            is_general_workspace_dir_in(text(case, 0), Path::new("/fixture/home")).unwrap(),
        ),
        "isGeneralWorkspaceDirEmptyHome" => {
            JsValue::Bool(is_general_workspace_dir_in(text(case, 0), Path::new("")).unwrap())
        }
        "getWorkspaceMemoryMode" => JsValue::String(JsString::from(
            get_workspace_memory_mode_in(text(case, 0), Path::new("/fixture/home")).unwrap(),
        )),
        "getWorkspaceMemoryModeEmptyHome" => JsValue::String(JsString::from(
            get_workspace_memory_mode_in(text(case, 0), Path::new("")).unwrap(),
        )),
        "buildExternalProjectLogicalId" => JsValue::String(
            build_external_project_logical_id(text(case, 0), text(case, 1)).unwrap(),
        ),
        "buildExternalRecordId" => {
            JsValue::String(build_external_record_id(text(case, 0), text(case, 1)).unwrap())
        }
        "parseExternalRecordId" => match parse_external_record_id(text(case, 0)) {
            Some(parsed) => {
                let mut object = JsObject::new();
                object.insert("workspaceKey".into(), JsValue::String(parsed.workspace_key));
                object.insert("relativePath".into(), JsValue::String(parsed.relative_path));
                JsValue::Object(object)
            }
            None => JsValue::Null,
        },
        "isExternalRecordId" => JsValue::Bool(is_external_record_id(text(case, 0))),
        "traceI18n" => {
            let args = match items(case.get("args").unwrap()).get(2) {
                Some(JsValue::Array(args)) => args.clone(),
                _ => Vec::new(),
            };
            JsValue::Object(trace_i18n(text(case, 0), text(case, 1), &args).unwrap())
        }
        "parseRawRecallSections" => sections_value(parse_raw_recall_sections(text(case, 0))),
        "buildMemoryRecallSystemContext" => {
            JsValue::String(build_memory_recall_system_context(text(case, 0)))
        }
        "looksLikeRawRecallContext" => {
            let value = arg(case, 0).as_string();
            JsValue::Bool(looks_like_raw_recall_context(value))
        }
        "normalizeCaseTraceContextPreview" => JsValue::Object(
            normalize_case_trace_context_preview(arg(case, 0).as_object().unwrap()),
        ),
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
        let expected = expand(decoded.get("expected").unwrap());
        assert_eq!(
            wire(&actual),
            wire(&expected),
            "{name} args={:?}",
            decoded.get("args").unwrap()
        );
    }
    assert!(seen > 0, "no {wanted} cases in the oracle");
}

#[test]
fn text_helpers_match_ts() {
    replay("text");
}

#[test]
fn ids_match_ts() {
    replay("ids");
}

#[test]
fn general_workspace_match_ts() {
    replay("general");
}

#[test]
fn trace_i18n_matches_ts() {
    replay("trace");
}

#[test]
fn recall_context_matches_ts() {
    replay("recall");
}

#[test]
fn case_trace_preview_matches_ts() {
    replay("preview");
}

#[test]
#[ignore = "requires fixed PDRS_BASELINE with Node 22 and tsx"]
fn fixture_regenerates_identically() {
    baseline::regenerates_identically("edgeclaw-text");
}

#[test]
fn normalize_workspace_path_reports_a_missing_cwd() {
    // Node probe: with the cwd deleted, `path.resolve('/abs/x/../y')` still
    // returns `/abs/y`, while `path.resolve('rel/x')` throws
    // `ENOENT: no such file or directory, uv_cwd`.
    let dead_cwd = || Err(std::io::Error::from_raw_os_error(libc::ENOENT));
    let error = normalize_workspace_path_from(dead_cwd(), &JsString::from("rel/x")).unwrap_err();
    assert_eq!(error.io_code(), Some("ENOENT"));
    assert_eq!(
        normalize_workspace_path_from(dead_cwd(), &JsString::from("/abs/x/../y")).unwrap(),
        JsString::from("/abs/y")
    );
}

#[test]
fn general_workspace_reports_a_missing_homedir() {
    // TS evaluates `GENERAL_WORKSPACE_DIR` at module load, so a failing
    // `os.homedir()` rejects the import instead of returning false.
    let error = is_general_workspace_dir_with_home(
        &JsString::from("/fixture/home/.pilotdeck"),
        Err(
            pilotdeck_gateway::runtime::memory::edgeclaw::FileMemoryError::Invalid(
                "os.homedir() is unavailable".into(),
            ),
        ),
    )
    .unwrap_err();
    assert_eq!(error.to_string(), "os.homedir() is unavailable");
}

#[test]
fn decode_escaped_unicode_value_handles_100k_nesting() {
    let mut value = JsValue::String(JsString::from("\\u0041"));
    for _ in 0..100_000 {
        value = JsValue::Array(vec![value]);
    }
    let mut decoded = &decode_escaped_unicode_value(&value, false);
    let mut depth = 0;
    while let JsValue::Array(items) = decoded {
        assert_eq!(items.len(), 1);
        decoded = &items[0];
        depth += 1;
    }
    assert_eq!(depth, 100_000);
    assert_eq!(decoded, &JsValue::String(JsString::from("A")));
}
