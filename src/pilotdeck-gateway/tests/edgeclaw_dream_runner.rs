//! #773 A6.6-edgeclaw-16: DreamRewriteRunner sub-flows.
//!
//! Each oracle case seeds a real stage repository (SQLite + FileMemoryStore),
//! records one `runCategoryDream` / `mergeGeneralProjectMetas` /
//! `runProjectMetaReview` call against a scripted extractor, and compares the
//! final memory tree, mutation list, trace, call inputs and return value.
//! One `#[test]` per case keeps every negative control bound to a unique
//! `#773 ...` assertion instead of an earlier sibling.
#[path = "common/baseline.rs"]
mod baseline;

use pilotdeck_gateway::runtime::memory::edgeclaw::{
    EdgeClawFileStores,
    dream::{
        helpers::create_dream_trace,
        runner::{
            CategoryDreamResult, DreamEnvironment, DreamExtractor, DreamLogger, DreamRewriteRunner,
            GeneralProjectMergeResult, MetaReviewResult,
        },
    },
    llm::transport::LlmError,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use std::{
    collections::VecDeque,
    fs,
    future::Future,
    path::Path,
    pin::Pin,
    process::Command,
    sync::{Arc, Mutex},
};

const ORACLE: &str = include_str!("fixtures/edgeclaw-dream-runner/oracle.json");
const NOW: &str = "2026-10-22T00:00:00.000Z";

fn oracle() -> JsValue {
    baseline::oracle(ORACLE);
    parse_json(ORACLE).expect("#773 oracle.json must parse")
}

fn text(value: &JsValue) -> String {
    value
        .as_string()
        .expect("#773 expected a string")
        .to_utf8()
        .expect("#773 expected UTF-8")
}

fn string_field(value: &JsValue, key: &str) -> String {
    value
        .get(key)
        .and_then(JsValue::as_string)
        .unwrap_or_else(|| panic!("#773 missing string field {key}"))
        .to_utf8()
        .expect("#773 field must be UTF-8")
}

fn array(value: &JsValue) -> &[JsValue] {
    match value {
        JsValue::Array(items) => items,
        other => panic!("#773 expected an array, got {other:?}"),
    }
}

fn object(value: &JsValue) -> &JsObject {
    value
        .as_object()
        .unwrap_or_else(|| panic!("#773 expected an object"))
}

fn js(object: JsObject) -> JsValue {
    JsValue::Object(object)
}

/// JSON-order-independent comparison: object keys sorted, arrays kept as-is.
fn canonical(value: &JsValue) -> JsValue {
    match value {
        JsValue::Array(items) => JsValue::Array(items.iter().map(canonical).collect()),
        JsValue::Object(input) => {
            let mut entries: Vec<_> = input.entries().to_vec();
            entries.sort_by(|left, right| left.0.units().cmp(right.0.units()));
            let mut out = JsObject::new();
            for (key, value) in entries {
                out.insert(key, canonical(&value));
            }
            JsValue::Object(out)
        }
        other => other.clone(),
    }
}

fn wire(value: &JsValue) -> String {
    match stringify_json(&canonical(value)).expect("#773 value must serialize") {
        Some(text) => text,
        None => "undefined".into(),
    }
}

fn equal(actual: &JsValue, expected: &JsValue, message: &str) {
    assert_eq!(wire(actual), wire(expected), "{message}");
}

/// Replace the per-case absolute store root so the oracle stays path-free.
/// The replacement works on UTF-16 units: `String::from_utf16_lossy` would
/// turn a fixture's lone surrogate into U+FFFD before comparison.
fn clean_paths(value: &JsValue, root: &str) -> JsValue {
    match value {
        JsValue::String(text) => {
            let needle: Vec<u16> = root.encode_utf16().collect();
            let replacement: Vec<u16> = "$ROOT".encode_utf16().collect();
            let units = text.units();
            let mut out = Vec::with_capacity(units.len());
            let mut index = 0;
            while index < units.len() {
                if !needle.is_empty() && units[index..].starts_with(&needle) {
                    out.extend(&replacement);
                    index += needle.len();
                } else {
                    out.push(units[index]);
                    index += 1;
                }
            }
            JsValue::String(JsString::from_utf16(out))
        }
        JsValue::Array(items) => {
            JsValue::Array(items.iter().map(|item| clean_paths(item, root)).collect())
        }
        JsValue::Object(input) => {
            let mut out = JsObject::new();
            for (key, value) in input.entries() {
                out.insert(key.clone(), clean_paths(value, root));
            }
            JsValue::Object(out)
        }
        other => other.clone(),
    }
}

fn tree(root: &Path, dir: &str, out: &mut Vec<JsValue>) {
    let mut entries: Vec<_> = fs::read_dir(root.join(dir))
        .expect("#773 tree read_dir")
        .map(|entry| entry.expect("#773 tree entry"))
        .collect();
    entries.sort_by(|left, right| {
        left.file_name()
            .to_string_lossy()
            .cmp(&right.file_name().to_string_lossy())
    });
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        let relative = if dir.is_empty() {
            name
        } else {
            format!("{dir}/{name}")
        };
        if entry.file_type().expect("#773 tree file type").is_dir() {
            tree(root, &relative, out);
        } else {
            let content = fs::read_to_string(entry.path()).expect("#773 tree file content");
            let mut item = JsObject::new();
            item.insert("relativePath".into(), JsValue::String(relative.into()));
            item.insert("content".into(), JsValue::String(content.into()));
            out.push(js(item));
        }
    }
}

// -- scripted extractor -----------------------------------------------------

struct ExtractorState {
    steps: VecDeque<JsObject>,
    calls: Vec<JsValue>,
}

struct ScriptedExtractor {
    state: Mutex<ExtractorState>,
}

impl ScriptedExtractor {
    fn new(steps: Vec<JsObject>) -> Self {
        Self {
            state: Mutex::new(ExtractorState {
                steps: steps.into(),
                calls: vec![],
            }),
        }
    }

    fn next(
        &self,
        method: &str,
        input: &JsObject,
        debug: &mut (dyn FnMut(JsValue) + Send),
    ) -> Result<JsObject, LlmError> {
        let mut state = self.state.lock().expect("#773 extractor state lock");
        let mut call = JsObject::new();
        call.insert("method".into(), JsValue::String(method.into()));
        call.insert("input".into(), JsValue::Object(input.clone()));
        state.calls.push(js(call));
        let Some(step) = state.steps.pop_front() else {
            return Err(LlmError::new(format!(
                "fixture script ended before {method}"
            )));
        };
        let scripted = string_field(&js(step.clone()), "method");
        if scripted != method {
            return Err(LlmError::new(format!(
                "fixture script expected {scripted} but got {method}"
            )));
        }
        if let Some(event) = step.get("debug") {
            debug(event.clone());
        }
        if let Some(message) = step.get("throws") {
            let message = message
                .as_string()
                .expect("#773 throws must be a string")
                .clone();
            let mut error = LlmError::new(message);
            if let Some(name) = step.get("throwsName").and_then(JsValue::as_string) {
                error.name = if name.is_empty() {
                    ""
                } else if name.equals_str("Error") {
                    "Error"
                } else {
                    panic!("#773 unsupported fixture error name {name:?}")
                };
            }
            return Err(error);
        }
        Ok(step
            .get("result")
            .and_then(JsValue::as_object)
            .cloned()
            .unwrap_or_default())
    }
}

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

impl DreamExtractor for ScriptedExtractor {
    fn plan_dream_clusters<'a>(
        &'a self,
        input: &'a JsObject,
        debug: &'a mut (dyn FnMut(JsValue) + Send),
    ) -> BoxFuture<'a, Result<JsObject, LlmError>> {
        Box::pin(async move { self.next("planDreamClusters", input, debug) })
    }
    fn refine_dream_cluster<'a>(
        &'a self,
        input: &'a JsObject,
        debug: &'a mut (dyn FnMut(JsValue) + Send),
    ) -> BoxFuture<'a, Result<JsObject, LlmError>> {
        Box::pin(async move { self.next("refineDreamCluster", input, debug) })
    }
    fn plan_general_project_meta_merges<'a>(
        &'a self,
        input: &'a JsObject,
        debug: &'a mut (dyn FnMut(JsValue) + Send),
    ) -> BoxFuture<'a, Result<JsObject, LlmError>> {
        Box::pin(async move { self.next("planGeneralProjectMetaMerges", input, debug) })
    }
    fn review_dream_project_meta<'a>(
        &'a self,
        input: &'a JsObject,
        debug: &'a mut (dyn FnMut(JsValue) + Send),
    ) -> BoxFuture<'a, Result<JsObject, LlmError>> {
        Box::pin(async move { self.next("reviewDreamProjectMeta", input, debug) })
    }
    fn rewrite_user_profile<'a>(
        &'a self,
        _input: &'a JsObject,
        _debug: &'a mut (dyn FnMut(JsValue) + Send),
    ) -> BoxFuture<'a, Result<Option<JsObject>, LlmError>> {
        // The #773 sub-flow corpus never runs `run()`, which is the only
        // caller of the user-profile rewrite.
        Box::pin(async move {
            Err(LlmError::new(
                "#773 scripted extractor does not implement rewriteUserProfile",
            ))
        })
    }
}

// -- scripted clock/random --------------------------------------------------

struct ScriptedEnvironment {
    randoms: Vec<f64>,
    index: Mutex<usize>,
}

impl DreamEnvironment for ScriptedEnvironment {
    fn now_ms(&self) -> f64 {
        1792627200000.0
    }
    fn random(&self) -> f64 {
        let mut index = self.index.lock().expect("#773 random lock");
        let value = self.randoms.get(*index).copied().unwrap_or(0.5);
        *index += 1;
        value
    }
    fn now_iso(&self) -> JsString {
        NOW.into()
    }
}

/// Records the TS `logger.warn` calls the runner forwards
/// (`dream-review.ts:838`). Only `warn` is exercised by these sub-flows.
struct RecordingLogger {
    warnings: Mutex<Vec<JsString>>,
}

impl RecordingLogger {
    fn new() -> Self {
        Self {
            warnings: Mutex::new(vec![]),
        }
    }

    fn warnings(&self) -> Vec<JsString> {
        self.warnings
            .lock()
            .expect("#773 logger warnings lock")
            .clone()
    }
}

impl DreamLogger for RecordingLogger {
    fn warn(&self, message: &JsString) {
        self.warnings
            .lock()
            .expect("#773 logger warnings lock")
            .push(message.clone());
    }
}

fn category_value(result: &CategoryDreamResult) -> JsValue {
    let mut out = JsObject::new();
    out.insert(
        "plannedClusters".into(),
        JsValue::Number(result.planned_clusters),
    );
    out.insert(
        "refinedClusters".into(),
        JsValue::Number(result.refined_clusters),
    );
    out.insert("deletedFiles".into(), JsValue::Number(result.deleted_files));
    out.insert(
        "droppedWarnings".into(),
        JsValue::Array(result.dropped_warnings.clone()),
    );
    js(out)
}

fn merge_value(result: &GeneralProjectMergeResult) -> JsValue {
    let mut out = JsObject::new();
    out.insert(
        "mergedProjects".into(),
        JsValue::Number(result.merged_projects),
    );
    out.insert(
        "deletedProjects".into(),
        JsValue::Number(result.deleted_projects),
    );
    out.insert(
        "relinkedFiles".into(),
        JsValue::Number(result.relinked_files),
    );
    if let Some(summary) = &result.plan_summary {
        out.insert("planSummary".into(), JsValue::String(summary.clone()));
    }
    out.insert(
        "droppedWarnings".into(),
        JsValue::Array(result.dropped_warnings.clone()),
    );
    out.insert("groups".into(), JsValue::Array(result.groups.clone()));
    js(out)
}

fn meta_value(result: &MetaReviewResult) -> JsValue {
    let mut out = JsObject::new();
    out.insert("updated".into(), JsValue::Bool(result.updated));
    out.insert("reason".into(), JsValue::String(result.reason.clone()));
    js(out)
}

fn find_case<'a>(oracle: &'a JsValue, name: &str) -> &'a JsValue {
    array(oracle.get("cases").expect("#773 oracle cases"))
        .iter()
        .find(|case| string_field(case, "name") == name)
        .unwrap_or_else(|| panic!("#773 case {name} missing"))
}

fn replay(name: &str) {
    let oracle = oracle();
    let case = find_case(&oracle, name);
    let scratch = std::env::temp_dir().join(format!("pdrs-773-{name}-{}", uuid::Uuid::new_v4()));
    let mode = string_field(case, "mode");
    let general = scratch.join("pilotdeck");
    let (workspace, memory, db) = if mode == "general" {
        let workspace = general.clone();
        let memory = workspace.join("workspaces/w1/memory");
        let db = workspace.join("workspaces/w1/control.sqlite");
        (workspace, memory, db)
    } else {
        let workspace = scratch.join("single");
        let memory = workspace.join("memory");
        let db = workspace.join("control.sqlite");
        (workspace, memory, db)
    };
    let global = scratch.join("global");
    let clock = Arc::new(|| NOW.to_string());
    if let Some(external) = case
        .get("setup")
        .and_then(JsValue::as_object)
        .and_then(|setup| setup.get("external"))
        .and_then(JsValue::as_object)
    {
        let name = string_field(&js(external.clone()), "name");
        let workspace = string_field(&js(external.clone()), "workspace");
        let dir = general.join("workspaces").join(&name);
        let memory = dir.join("memory");
        let db = dir.join("control.sqlite");
        let global = scratch.join("global-external");
        let repository = EdgeClawFileStores::with_workspace_clock(
            &db,
            &memory,
            &global,
            &workspace,
            &general,
            clock.clone(),
        )
        .unwrap_or_else(|error| panic!("#773 external store build {name}: {error}"));
        repository
            .storage
            .set_pipeline_state("workspaceDir", &JsValue::String(workspace.clone().into()))
            .unwrap_or_else(|error| panic!("#773 external workspace state {name}: {error}"));
        let store = repository.get_file_memory_store();
        if let Some(metas) = external.get("metas") {
            for meta in array(metas) {
                store
                    .execute("upsertProjectMeta", object(meta))
                    .unwrap_or_else(|error| panic!("#773 external meta {name}: {error}"));
            }
        }
        if let Some(candidates) = external.get("candidates") {
            for candidate in array(candidates) {
                store
                    .execute("upsertCandidate", object(candidate))
                    .unwrap_or_else(|error| panic!("#773 external candidate {name}: {error}"));
            }
        }
        repository
            .close()
            .unwrap_or_else(|error| panic!("#773 external close {name}: {error}"));
    }
    let repository = EdgeClawFileStores::with_workspace_clock(
        &db, &memory, &global, &workspace, &general, clock,
    )
    .unwrap_or_else(|error| panic!("#773 store build {name}: {error}"));
    let store = repository.get_file_memory_store();

    if let Some(setup) = case.get("setup").and_then(JsValue::as_object) {
        if let Some(metas) = setup.get("metas") {
            for meta in array(metas) {
                store
                    .execute("upsertProjectMeta", object(meta))
                    .unwrap_or_else(|error| panic!("#773 seed meta {name}: {error}"));
            }
        }
        if let Some(candidates) = setup.get("candidates") {
            for candidate in array(candidates) {
                store
                    .execute("upsertCandidate", object(candidate))
                    .unwrap_or_else(|error| panic!("#773 seed candidate {name}: {error}"));
            }
        }
    }
    if case
        .get("ensureMeta")
        .is_some_and(|value| matches!(value, JsValue::Bool(true)))
    {
        store
            .execute("ensureProjectMeta", &JsObject::new())
            .unwrap_or_else(|error| panic!("#773 seed meta {name}: {error}"));
    }

    let steps: Vec<JsObject> = case
        .get("script")
        .map(array)
        .unwrap_or_default()
        .iter()
        .map(|step| object(step).clone())
        .collect();
    let extractor = ScriptedExtractor::new(steps);
    let randoms: Vec<f64> = case
        .get("randoms")
        .map(array)
        .unwrap_or_default()
        .iter()
        .filter_map(JsValue::as_number)
        .collect();
    let environment = ScriptedEnvironment {
        randoms,
        index: Mutex::new(0),
    };
    let logger = RecordingLogger::new();
    let runner =
        DreamRewriteRunner::new(&repository, &extractor, &environment).with_logger(&logger);
    let started_at = JsString::from(string_field(case, "startedAt"));
    let suffix = JsString::from(string_field(case, "randomSuffix"));
    let mut trace = create_dream_trace(&JsString::from("manual"), &started_at, &suffix);
    let method = string_field(case, "method");
    let expected = case.get("expected").expect("#773 case expected");

    let actual_result = if method == "runCategoryDream" {
        let kind = JsString::from(string_field(case, "kind"));
        let selection = case.get("selection").expect("#773 selection");
        let entry_type = string_field(selection, "type");
        let mut options = JsObject::new();
        options.insert(
            "kinds".into(),
            JsValue::Array(vec!["project".into(), "feedback".into()]),
        );
        options.insert("scope".into(), "project".into());
        options.insert("includeDeprecated".into(), JsValue::Bool(false));
        options.insert("limit".into(), JsValue::Number(5000.0));
        let entries_value = repository
            .list_memory_entries(&options)
            .unwrap_or_else(|error| panic!("#773 input entries {name}: {error}"));
        let entries: Vec<JsObject> = array(&entries_value)
            .iter()
            .filter(|entry| {
                entry
                    .get("type")
                    .and_then(JsValue::as_string)
                    .is_some_and(|value| value.equals_str(&entry_type))
            })
            .filter_map(JsValue::as_object)
            .cloned()
            .collect();
        let ids = JsValue::Array(
            entries
                .iter()
                .filter_map(|entry| entry.get("relativePath").cloned())
                .collect(),
        );
        let records_value = if entries.is_empty() {
            JsValue::Array(vec![])
        } else {
            repository
                .get_memory_records_by_ids(&ids, 5000.0)
                .unwrap_or_else(|error| panic!("#773 input records {name}: {error}"))
        };
        let records: Vec<JsObject> = array(&records_value)
            .iter()
            .filter_map(JsValue::as_object)
            .cloned()
            .collect();
        // Same "entries outlived the record file" fixture as observe.mts:
        // drop the last N records before building recordsByPath.
        let mut records = records;
        let drop_records = case
            .get("dropRecords")
            .and_then(JsValue::as_number)
            .unwrap_or(0.0) as usize;
        if drop_records > 0 {
            records.truncate(records.len().saturating_sub(drop_records));
        }
        let project_meta = case
            .get("projectMeta")
            .filter(|value| !value.is_undefined())
            .map(|value| {
                let id = text(value);
                let mut lookup = JsObject::new();
                if id != "current" {
                    lookup.insert("projectId".into(), JsValue::String(id.into()));
                }
                store
                    .execute("getProjectMeta", &lookup)
                    .unwrap_or_else(|error| panic!("#773 input meta {name}: {error}"))
                    .as_object()
                    .cloned()
                    .unwrap_or_else(|| panic!("#773 input meta {name} missing"))
            });
        let mut input = JsObject::new();
        input.insert("kind".into(), JsValue::String(kind.clone()));
        input.insert(
            "entries".into(),
            JsValue::Array(entries.iter().cloned().map(JsValue::Object).collect()),
        );
        input.insert(
            "records".into(),
            JsValue::Array(records.iter().cloned().map(JsValue::Object).collect()),
        );
        if let Some(meta) = &project_meta {
            input.insert("projectMeta".into(), JsValue::Object(meta.clone()));
        }
        equal(
            &clean_paths(&js(input), &memory.to_string_lossy()),
            expected.get("input").expect("#773 expected input"),
            &format!("#773 input {name}"),
        );
        let result = futures_block_on(runner.run_category_dream(
            &mut trace,
            &kind,
            &entries,
            &records,
            project_meta.as_ref(),
        ))
        .unwrap_or_else(|error| panic!("#773 result {name}: {error}"));
        category_value(&result)
    } else if method == "mergeGeneralProjectMetas" {
        let result = futures_block_on(runner.merge_general_project_metas(&mut trace))
            .unwrap_or_else(|error| panic!("#773 result {name}: {error}"));
        merge_value(&result)
    } else if method == "runProjectMetaReview" {
        let id = case
            .get("projectMeta")
            .map(text)
            .unwrap_or_else(|| panic!("#773 projectMeta {name}"));
        let mut lookup = JsObject::new();
        if id != "current" {
            lookup.insert("projectId".into(), JsValue::String(id.into()));
        }
        let project_meta = store
            .execute("getProjectMeta", &lookup)
            .unwrap_or_else(|error| panic!("#773 result meta {name}: {error}"))
            .as_object()
            .cloned()
            .unwrap_or_else(|| panic!("#773 result meta {name} missing"));
        let result = futures_block_on(runner.run_project_meta_review(&mut trace, &project_meta))
            .unwrap_or_else(|error| panic!("#773 result {name}: {error}"));
        meta_value(&result)
    } else {
        panic!("#773 unknown method {method}")
    };

    equal(
        &clean_paths(&actual_result, &memory.to_string_lossy()),
        expected.get("result").expect("#773 expected result"),
        &format!("#773 result {name}"),
    );
    equal(
        &clean_paths(&js(trace), &memory.to_string_lossy()),
        expected.get("trace").expect("#773 expected trace"),
        &format!("#773 trace {name}"),
    );
    let calls = extractor
        .state
        .lock()
        .expect("#773 extractor state lock")
        .calls
        .clone();
    equal(
        &JsValue::Array(calls),
        expected.get("calls").expect("#773 expected calls"),
        &format!("#773 calls {name}"),
    );
    let logs = JsValue::Array(logger.warnings().into_iter().map(JsValue::String).collect());
    equal(
        &logs,
        expected.get("logs").expect("#773 expected logs"),
        &format!("#773 logs {name}"),
    );
    let mut files = vec![];
    tree(store.get_root_dir(), "", &mut files);
    equal(
        &JsValue::Array(files),
        expected.get("tree").expect("#773 expected tree"),
        &format!("#773 tree {name}"),
    );
    let _ = fs::remove_dir_all(&scratch);
}

/// Minimal block_on for the async runner without pulling a new dev-dependency.
fn futures_block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("#773 tokio runtime")
        .block_on(future)
}

// -- one test per oracle case ----------------------------------------------

#[test]
fn category_project_cluster() {
    replay("category-project-cluster");
}
#[test]
fn category_feedback_cluster() {
    replay("category-feedback-cluster");
}
#[test]
fn category_empty() {
    replay("category-empty");
}
#[test]
fn category_skip_single() {
    replay("category-skip-single");
}
#[test]
fn category_general_scope() {
    replay("category-general-scope");
}
#[test]
fn category_dropped_clusters() {
    replay("category-dropped-clusters");
}
#[test]
fn category_missing_record() {
    replay("category-missing-record");
}
#[test]
fn category_refine_null() {
    replay("category-refine-null");
}
#[test]
fn merge_basic() {
    replay("merge-basic");
}
#[test]
fn merge_skip_single() {
    replay("merge-skip-single");
}
#[test]
fn merge_plan_error() {
    replay("merge-plan-error");
}
#[test]
fn merge_plan_error_empty_message() {
    replay("merge-plan-error-empty-message");
}
#[test]
fn merge_plan_error_empty_name() {
    replay("merge-plan-error-empty-name");
}
#[test]
fn merge_plan_error_lone_surrogate() {
    replay("merge-plan-error-lone-surrogate");
}
#[test]
fn merge_invalid_groups() {
    replay("merge-invalid-groups");
}
#[test]
fn meta_review_update() {
    replay("meta-review-update");
}
#[test]
fn meta_review_keep() {
    replay("meta-review-keep");
}
#[test]
fn meta_review_empty() {
    replay("meta-review-empty");
}
#[test]
fn meta_review_general() {
    replay("meta-review-general");
}
#[test]
fn meta_review_mirror() {
    replay("meta-review-mirror");
}

#[test]
fn scratch_root_is_run_exclusive() {
    let shared = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/_shared");
    let output = Command::new("node")
        .arg("--test")
        .arg(shared.join("run_scratch.test.mjs"))
        .arg(shared.join("observe_scratch.test.mjs"))
        .output()
        .expect("#773 node must be available for the scratch-root test");
    assert!(
        output.status.success(),
        "#773 scratch roots must be exclusive and owned\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
#[ignore = "requires fixed PDRS_BASELINE with Node 22 and tsx"]
fn fixture_regenerates_identically() {
    baseline::regenerates_identically("edgeclaw-dream-runner");
}
