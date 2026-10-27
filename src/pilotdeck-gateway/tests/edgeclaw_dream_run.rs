//! #774 A6.6-edgeclaw-17: `DreamRewriteRunner.run` orchestration.
//!
//! Each oracle case seeds a real stage repository (SQLite + FileMemoryStore),
//! runs one complete `run()` (or the direct category-error probe for #915)
//! against a scripted extractor, and compares the final memory tree, the
//! returned outcome, the full trace, call inputs and logger warnings.
//! One `#[test]` per case keeps every negative control bound to a unique
//! `#774 ...` assertion instead of an earlier sibling.
#[path = "common/baseline.rs"]
mod baseline;

use pilotdeck_gateway::runtime::memory::edgeclaw::{
    EdgeClawFileStores,
    dream::{
        helpers::create_dream_trace,
        run::DreamExecutionResult,
        runner::{
            DreamEnvironment, DreamExtractor, DreamLogger, DreamRewriteRunner, DreamRunnerError,
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

const ORACLE: &str = include_str!("fixtures/edgeclaw-dream-run/oracle.json");
const NOW: &str = "2026-10-22T00:00:00.000Z";

fn oracle() -> JsValue {
    baseline::oracle(ORACLE);
    parse_json(ORACLE).expect("#774 oracle.json must parse")
}

fn string_field(value: &JsValue, key: &str) -> String {
    value
        .get(key)
        .and_then(JsValue::as_string)
        .unwrap_or_else(|| panic!("#774 missing string field {key}"))
        .to_utf8()
        .expect("#774 field must be UTF-8")
}

fn array(value: &JsValue) -> &[JsValue] {
    match value {
        JsValue::Array(items) => items,
        other => panic!("#774 expected an array, got {other:?}"),
    }
}

fn object(value: &JsValue) -> &JsObject {
    value
        .as_object()
        .unwrap_or_else(|| panic!("#774 expected an object"))
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
    match stringify_json(&canonical(value)).expect("#774 value must serialize") {
        Some(text) => text,
        None => "undefined".into(),
    }
}

fn equal(actual: &JsValue, expected: &JsValue, message: &str) {
    assert_eq!(wire(actual), wire(expected), "{message}");
}

/// Replace the per-case absolute scratch root so the oracle stays path-free.
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
        .unwrap_or_else(|error| panic!("#774 tree read_dir {dir}: {error}"))
        .map(|entry| entry.unwrap_or_else(|error| panic!("#774 tree entry: {error}")))
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
        if entry.file_type().expect("#774 tree file type").is_dir() {
            tree(root, &relative, out);
        } else {
            let content = fs::read_to_string(entry.path())
                .unwrap_or_else(|error| panic!("#774 tree file content {relative}: {error}"));
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
        allow_null: bool,
    ) -> Result<Option<JsObject>, LlmError> {
        let mut state = self.state.lock().expect("#774 extractor state lock");
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
                .expect("#774 throws must be a string")
                .clone();
            let mut error = LlmError::new(message);
            if let Some(name) = step.get("throwsName").and_then(JsValue::as_string) {
                error.name = if name.is_empty() {
                    ""
                } else if name.equals_str("Error") {
                    "Error"
                } else {
                    panic!("#774 unsupported fixture error name {name:?}")
                };
            }
            return Err(error);
        }
        match step.get("result") {
            None | Some(JsValue::Null | JsValue::Undefined) => {
                if allow_null {
                    Ok(None)
                } else {
                    Ok(Some(JsObject::new()))
                }
            }
            Some(JsValue::Object(result)) => Ok(Some(result.clone())),
            Some(_) => Ok(Some(JsObject::new())),
        }
    }
}

type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

impl DreamExtractor for ScriptedExtractor {
    fn plan_dream_clusters<'a>(
        &'a self,
        input: &'a JsObject,
        debug: &'a mut (dyn FnMut(JsValue) + Send),
    ) -> BoxFuture<'a, Result<JsObject, LlmError>> {
        Box::pin(async move {
            self.next("planDreamClusters", input, debug, false)
                .map(|value| value.expect("#774 fixture step must be an object"))
        })
    }
    fn refine_dream_cluster<'a>(
        &'a self,
        input: &'a JsObject,
        debug: &'a mut (dyn FnMut(JsValue) + Send),
    ) -> BoxFuture<'a, Result<JsObject, LlmError>> {
        Box::pin(async move {
            self.next("refineDreamCluster", input, debug, false)
                .map(|value| value.expect("#774 fixture step must be an object"))
        })
    }
    fn plan_general_project_meta_merges<'a>(
        &'a self,
        input: &'a JsObject,
        debug: &'a mut (dyn FnMut(JsValue) + Send),
    ) -> BoxFuture<'a, Result<JsObject, LlmError>> {
        Box::pin(async move {
            self.next("planGeneralProjectMetaMerges", input, debug, false)
                .map(|value| value.expect("#774 fixture step must be an object"))
        })
    }
    fn review_dream_project_meta<'a>(
        &'a self,
        input: &'a JsObject,
        debug: &'a mut (dyn FnMut(JsValue) + Send),
    ) -> BoxFuture<'a, Result<JsObject, LlmError>> {
        Box::pin(async move {
            self.next("reviewDreamProjectMeta", input, debug, false)
                .map(|value| value.expect("#774 fixture step must be an object"))
        })
    }
    fn rewrite_user_profile<'a>(
        &'a self,
        input: &'a JsObject,
        debug: &'a mut (dyn FnMut(JsValue) + Send),
    ) -> BoxFuture<'a, Result<Option<JsObject>, LlmError>> {
        Box::pin(async move { self.next("rewriteUserProfile", input, debug, true) })
    }
}

// -- scripted clock/random --------------------------------------------------

struct ScriptedEnvironment {
    randoms: Vec<f64>,
    suffix: JsString,
    index: Mutex<usize>,
}

impl ScriptedEnvironment {
    fn next(&self) -> f64 {
        let mut index = self.index.lock().expect("#774 random lock");
        let value = self.randoms.get(*index).copied().unwrap_or(0.5);
        *index += 1;
        value
    }
}

impl DreamEnvironment for ScriptedEnvironment {
    fn now_ms(&self) -> f64 {
        1792627200000.0
    }
    fn random(&self) -> f64 {
        self.next()
    }
    fn now_iso(&self) -> JsString {
        NOW.into()
    }
    fn random_suffix(&self) -> JsString {
        // TS consumes one Math.random() for the trace id before any mutation.
        self.next();
        self.suffix.clone()
    }
}

/// Records the TS `logger.warn` calls the runner forwards.
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
            .expect("#774 logger warnings lock")
            .clone()
    }
}

impl DreamLogger for RecordingLogger {
    fn warn(&self, message: &JsString) {
        self.warnings
            .lock()
            .expect("#774 logger warnings lock")
            .push(message.clone());
    }
}

fn result_value(result: &DreamExecutionResult) -> JsValue {
    let mut out = JsObject::new();
    out.insert(
        "reviewedFiles".into(),
        JsValue::Number(result.reviewed_files),
    );
    out.insert(
        "rewrittenProjects".into(),
        JsValue::Number(result.rewritten_projects),
    );
    out.insert(
        "deletedProjects".into(),
        JsValue::Number(result.deleted_projects),
    );
    out.insert("deletedFiles".into(), JsValue::Number(result.deleted_files));
    out.insert(
        "profileUpdated".into(),
        JsValue::Bool(result.profile_updated),
    );
    out.insert(
        "duplicateTopicCount".into(),
        JsValue::Number(result.duplicate_topic_count),
    );
    out.insert(
        "conflictTopicCount".into(),
        JsValue::Number(result.conflict_topic_count),
    );
    out.insert("summary".into(), JsValue::String(result.summary.clone()));
    out.insert(
        "finishedAt".into(),
        JsValue::String(result.finished_at.clone()),
    );
    out.insert("isNoOp".into(), JsValue::Bool(result.is_no_op));
    out.insert("trace".into(), JsValue::Object(result.trace.clone()));
    js(out)
}

fn find_case<'a>(oracle: &'a JsValue, name: &str) -> &'a JsValue {
    array(oracle.get("cases").expect("#774 oracle cases"))
        .iter()
        .find(|case| string_field(case, "name") == name)
        .unwrap_or_else(|| panic!("#774 case {name} missing"))
}

/// Owns one replay scratch tree and restores the *original* permission modes
/// before removing it, including when an assertion unwinds. Storing the
/// requested fault mode instead (0444/0555) leaves the tree undeletable and
/// leaks one scratch directory per run (#774 P2).
struct ScratchGuard {
    root: std::path::PathBuf,
    modes: Vec<(std::path::PathBuf, fs::Permissions)>,
}

impl ScratchGuard {
    fn new(root: std::path::PathBuf) -> Self {
        Self {
            root,
            modes: Vec::new(),
        }
    }

    fn set_permissions(&mut self, path: &Path, mode: u32) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let original = fs::metadata(path)
                .unwrap_or_else(|error| panic!("#774 scratch metadata {}: {error}", path.display()))
                .permissions();
            self.modes.push((path.to_path_buf(), original));
            fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap_or_else(|error| {
                panic!("#774 scratch chmod {} {mode:o}: {error}", path.display())
            });
        }
        #[cfg(not(unix))]
        {
            let _ = (path, mode);
        }
    }
}

impl std::ops::Deref for ScratchGuard {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.root
    }
}

impl Drop for ScratchGuard {
    fn drop(&mut self) {
        for (path, permissions) in self.modes.drain(..).rev() {
            // A fault mode (0444/0555) blocks `remove_dir_all`; restore the
            // real pre-fault permissions first. Ignore errors here: panicking
            // during unwind would abort the process.
            let _ = fs::set_permissions(path, permissions);
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn replay(name: &str) {
    let oracle = oracle();
    let case = find_case(&oracle, name);
    let mut scratch = ScratchGuard::new(
        std::env::temp_dir().join(format!("pdrs-774-{name}-{}", uuid::Uuid::new_v4())),
    );
    let home = scratch.join("home");
    let base = scratch.join("cases").join(name);
    let mode = string_field(case, "mode");
    let global = base.join("global");
    let (workspace, memory, general) = if mode == "general" {
        let general = home.join(".pilotdeck");
        let workspace = general.clone();
        let memory = workspace.join("workspaces").join(name).join("memory");
        fs::create_dir_all(&home).unwrap_or_else(|error| panic!("#774 home {name}: {error}"));
        (workspace, memory, general)
    } else {
        let workspace = base.join("single");
        let memory = workspace.join("memory");
        (workspace, memory, home.join(".pilotdeck"))
    };
    let db = if mode == "general" {
        workspace
            .join("workspaces")
            .join(name)
            .join("control.sqlite")
    } else {
        workspace.join("control.sqlite")
    };
    let clock = Arc::new(|| NOW.to_string());
    let repository = EdgeClawFileStores::with_workspace_clock(
        &db, &memory, &global, &workspace, &general, clock,
    )
    .unwrap_or_else(|error| panic!("#774 store build {name}: {error}"));
    let store = repository.get_file_memory_store();
    let global_store = repository.get_global_user_store();

    if let Some(setup) = case.get("setup").and_then(JsValue::as_object) {
        if let Some(metas) = setup.get("metas") {
            for meta in array(metas) {
                store
                    .execute("upsertProjectMeta", object(meta))
                    .unwrap_or_else(|error| panic!("#774 seed meta {name}: {error}"));
            }
        }
        if let Some(candidates) = setup.get("candidates") {
            for candidate in array(candidates) {
                store
                    .execute("upsertCandidate", object(candidate))
                    .unwrap_or_else(|error| panic!("#774 seed candidate {name}: {error}"));
            }
        }
        if let Some(notes) = setup.get("userNotes") {
            for note in array(notes) {
                global_store
                    .execute("upsertCandidate", object(note))
                    .unwrap_or_else(|error| panic!("#774 seed user note {name}: {error}"));
            }
        }
        if let Some(profile) = setup.get("userProfile").and_then(JsValue::as_object) {
            global_store
                .execute("upsertUserProfile", profile)
                .unwrap_or_else(|error| panic!("#774 seed user profile {name}: {error}"));
        }
        if case
            .get("setup")
            .and_then(|setup| setup.get("ensureMeta"))
            .is_some_and(|value| matches!(value, JsValue::Bool(true)))
        {
            store
                .execute("ensureProjectMeta", &JsObject::new())
                .unwrap_or_else(|error| panic!("#774 seed ensure meta {name}: {error}"));
        }
    }

    // P1 store-failure goldens: the same permission faults observe.mts applies
    // after seeding, so both replays fail inside the TS `try` (not earlier).
    if let Some(faults) = case.get("faults").and_then(JsValue::as_object) {
        let global_root = global_store.get_root_dir();
        if matches!(faults.get("profileReadOnly"), Some(JsValue::Bool(true))) {
            scratch.set_permissions(&global_root.join("UserIdentity/user-profile.md"), 0o444);
        }
        if matches!(faults.get("notesReadOnly"), Some(JsValue::Bool(true))) {
            scratch.set_permissions(&global_root.join("UserIdentityNotes"), 0o555);
        }
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
        suffix: JsString::from(string_field(case, "randomSuffix")),
        index: Mutex::new(0),
    };
    let logger = RecordingLogger::new();
    let runner =
        DreamRewriteRunner::new(&repository, &extractor, &environment).with_logger(&logger);
    let method = string_field(case, "method");
    let expected = case.get("expected").expect("#774 case expected");

    if method == "run" {
        let trigger = JsString::from(string_field(case, "trigger"));
        let result = futures_block_on(runner.run(&trigger))
            .unwrap_or_else(|error| panic!("#774 run result {name}: {error}"));
        let actual = result_value(&result);
        equal(
            &clean_paths(&actual, &scratch.to_string_lossy()),
            expected.get("result").expect("#774 expected result"),
            &format!("#774 result {name}"),
        );
        equal(
            &clean_paths(&js(result.trace.clone()), &scratch.to_string_lossy()),
            expected.get("trace").expect("#774 expected trace"),
            &format!("#774 trace {name}"),
        );
    } else if method == "runCategoryDreamError" {
        let selection = case.get("selection").expect("#774 selection");
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
            .unwrap_or_else(|error| panic!("#774 input entries {name}: {error}"));
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
        let records = if entries.is_empty() {
            vec![]
        } else {
            repository
                .get_memory_records_by_ids(&ids, 5000.0)
                .unwrap_or_else(|error| panic!("#774 input records {name}: {error}"))
                .as_array()
                .map(|items| {
                    items
                        .iter()
                        .filter_map(JsValue::as_object)
                        .cloned()
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };
        let mut visible = records;
        let drop_records = case
            .get("dropRecords")
            .and_then(JsValue::as_number)
            .unwrap_or(0.0) as usize;
        if drop_records > 0 {
            visible.truncate(visible.len().saturating_sub(drop_records));
        }
        let trace = create_dream_trace(
            &"manual".into(),
            &JsString::from(string_field(case, "startedAt")),
            &JsString::from(string_field(case, "randomSuffix")),
        );
        let mut trace = trace;
        let outcome = futures_block_on(runner.run_category_dream(
            &mut trace,
            &entry_type.clone().into(),
            &entries,
            &visible,
            None,
        ));
        let actual_error = match outcome {
            Ok(_) => "unexpected success".into(),
            Err(DreamRunnerError::Llm(error)) => {
                if error.name.is_empty() {
                    error.message.to_utf8().unwrap_or_default()
                } else if error.message.is_empty() {
                    error.name.to_string()
                } else {
                    format!(
                        "{}: {}",
                        error.name,
                        error.message.to_utf8().unwrap_or_default()
                    )
                }
            }
            Err(DreamRunnerError::Memory(error)) => {
                format!("#774 unexpected memory error: {error}")
            }
        };
        assert_eq!(
            actual_error,
            string_field(expected, "error"),
            "#774 error {name}"
        );
        equal(
            &clean_paths(&js(trace), &scratch.to_string_lossy()),
            expected.get("trace").expect("#774 expected trace"),
            &format!("#774 trace {name}"),
        );
    } else {
        panic!("#774 unknown method {method}")
    }

    let calls = extractor
        .state
        .lock()
        .expect("#774 extractor state lock")
        .calls
        .clone();
    equal(
        &clean_paths(&JsValue::Array(calls), &scratch.to_string_lossy()),
        expected.get("calls").expect("#774 expected calls"),
        &format!("#774 calls {name}"),
    );
    let logs = JsValue::Array(logger.warnings().into_iter().map(JsValue::String).collect());
    equal(
        &clean_paths(&logs, &scratch.to_string_lossy()),
        expected.get("logs").expect("#774 expected logs"),
        &format!("#774 logs {name}"),
    );
    let mut workspace_files = vec![];
    tree(store.get_root_dir(), "", &mut workspace_files);
    let mut global_files = vec![];
    tree(global_store.get_root_dir(), "", &mut global_files);
    let mut tree_value = JsObject::new();
    tree_value.insert("workspace".into(), JsValue::Array(workspace_files));
    tree_value.insert("global".into(), JsValue::Array(global_files));
    equal(
        &js(tree_value),
        expected.get("tree").expect("#774 expected tree"),
        &format!("#774 tree {name}"),
    );
}

/// Minimal block_on for the async runner without pulling a new dev-dependency.
fn futures_block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("#774 tokio runtime")
        .block_on(future)
}

trait AsArray {
    fn as_array(&self) -> Option<&Vec<JsValue>>;
}

impl AsArray for JsValue {
    fn as_array(&self) -> Option<&Vec<JsValue>> {
        match self {
            JsValue::Array(items) => Some(items),
            _ => None,
        }
    }
}

// -- one test per oracle case ----------------------------------------------

#[test]
fn run_empty_single() {
    replay("run-empty-single");
}
#[test]
fn run_empty_general() {
    replay("run-empty-general");
}
#[test]
fn run_single_full() {
    replay("run-single-full");
}
#[test]
fn run_single_null_profile() {
    replay("run-single-null-profile");
}
#[test]
fn run_single_kept_profile() {
    replay("run-single-kept-profile");
}
#[test]
fn run_single_rewrite_error() {
    replay("run-single-rewrite-error");
}
#[test]
fn run_single_whitespace_profile() {
    replay("run-single-whitespace-profile");
}
#[test]
fn run_single_upsert_failure() {
    replay("run-single-upsert-failure");
}
#[test]
fn run_single_delete_failure() {
    replay("run-single-delete-failure");
}
#[test]
fn run_single_meta_only() {
    replay("run-single-meta-only");
}
#[test]
fn run_single_window_truncation() {
    replay("run-single-window-truncation");
}
#[test]
fn run_general_two_projects() {
    replay("run-general-two-projects");
}
#[test]
fn run_general_merge() {
    replay("run-general-merge");
}
#[test]
fn category_warning_then_error() {
    replay("category-warning-then-error");
}

#[test]
fn scratch_guard_removes_scratch_after_restoring_fault_mode_on_return() {
    let root = std::env::temp_dir().join(format!("pdrs-774-guard-return-{}", uuid::Uuid::new_v4()));
    {
        let mut scratch = ScratchGuard::new(root.clone());
        let notes = root.join("UserIdentityNotes");
        fs::create_dir_all(&notes)
            .unwrap_or_else(|error| panic!("#774 guard return mkdir {}: {error}", notes.display()));
        fs::write(notes.join("note.md"), "note")
            .unwrap_or_else(|error| panic!("#774 guard return write: {error}"));
        scratch.set_permissions(&notes, 0o555);
        assert!(
            notes.exists(),
            "#774 guard return fixture must exist before drop"
        );
    }
    assert!(
        !root.exists(),
        "#774 scratch guard must remove the tree after restoring fault permissions on return"
    );
}

#[test]
fn scratch_guard_removes_scratch_after_restoring_fault_mode_on_unwind() {
    let root = std::env::temp_dir().join(format!("pdrs-774-guard-unwind-{}", uuid::Uuid::new_v4()));
    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut scratch = ScratchGuard::new(root.clone());
        let notes = root.join("UserIdentityNotes");
        fs::create_dir_all(&notes)
            .unwrap_or_else(|error| panic!("#774 guard unwind mkdir {}: {error}", notes.display()));
        fs::write(notes.join("note.md"), "note")
            .unwrap_or_else(|error| panic!("#774 guard unwind write: {error}"));
        scratch.set_permissions(&notes, 0o555);
        panic!("#774 injected unwind exercises ScratchGuard::drop");
    }));
    assert!(unwound.is_err(), "#774 injected panic must unwind");
    assert!(
        !root.exists(),
        "#774 scratch guard must remove the tree after restoring fault permissions on unwind"
    );
}

#[test]
fn scratch_root_is_run_exclusive() {
    let shared = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/_shared");
    let output = Command::new("node")
        .arg("--test")
        .arg(shared.join("run_scratch.test.mjs"))
        .arg(shared.join("observe_scratch.test.mjs"))
        .output()
        .expect("#774 node must be available for the scratch-root test");
    assert!(
        output.status.success(),
        "#774 scratch roots must be exclusive and owned\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
#[ignore = "requires fixed PDRS_BASELINE with Node 22 and tsx"]
fn fixture_regenerates_identically() {
    baseline::regenerates_identically("edgeclaw-dream-run");
}
