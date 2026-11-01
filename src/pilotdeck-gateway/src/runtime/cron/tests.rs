//! Oracle replay for the #1216 cron storage port. Every cell of
//! `tests/fixtures/cron-store/oracle.json` (paths / eventsPaths / store /
//! migration) is replayed against the Rust port by one named test, comparing
//! recorded results and the resulting on-disk trees byte for byte.
//!
//! The generator pins its clock at `Date.UTC(2026, 0, 1)`; the migration
//! replay pins `now_ms` to the same epoch so the marker's `completedAt` and
//! lock staleness match the recorded bytes. Two cells whose recorded bytes
//! depend on `localeCompare` ordering of non-ASCII text
//! (`store:put-sort-missing-and-collation`,
//! `migration:duplicate-runs-dedupe`) run in `LANG=en_US.UTF-8` child
//! processes via `crate::test_process::run_cases`, the same contract the
//! generator itself asserts.
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Read as _, Write as _},
    path::Path,
    sync::LazyLock,
    time::{Duration, UNIX_EPOCH},
};

use base64::Engine as _;
use pilotdeck_protocol::{JsObject, JsString, JsValue, stringify_json};

use super::paths::{cron_run_events_path, resolve_cron_paths};
use super::{CronRuntimeLogger, CronTaskStore, MigrationInput, migrate_cron_stores};
use crate::test_scratch::Scratch;

/// `installFixedClock()`'s default epoch (`Date.UTC(2026, 0, 1)`).
const PINNED_NOW_MS: f64 = 1_767_225_600_000.0;

fn oracle() -> &'static JsValue {
    static ORACLE: LazyLock<JsValue> = LazyLock::new(|| {
        crate::test_baseline::oracle(include_str!(
            "../../../tests/fixtures/cron-store/oracle.json"
        ))
    });
    &ORACLE
}

fn cells(section: &str) -> &'static [JsValue] {
    match oracle().get(section) {
        Some(JsValue::Array(cells)) => cells,
        _ => panic!("#1216 oracle section {section} must be an array"),
    }
}

fn cell(section: &str, name: &str) -> &'static JsValue {
    cells(section)
        .iter()
        .find(|entry| text(entry, "name") == name)
        .unwrap_or_else(|| panic!("#1216 oracle section {section} has no cell {name}"))
}

fn text(value: &JsValue, name: &str) -> String {
    value
        .get(name)
        .and_then(JsValue::as_string)
        .unwrap_or_else(|| panic!("#1216 oracle field {name} must be a string: {value:?}"))
        .to_utf8()
        .unwrap_or_else(|error| panic!("#1216 oracle field {name} must be UTF-8: {error:?}"))
}

fn list<'a>(value: &'a JsValue, name: &str) -> &'a [JsValue] {
    match value.get(name) {
        Some(JsValue::Array(items)) => items,
        _ => panic!("#1216 oracle field {name} must be an array: {value:?}"),
    }
}

fn array(value: &JsValue) -> &[JsValue] {
    match value {
        JsValue::Array(items) => items,
        _ => panic!("#1216 oracle value must be an array: {value:?}"),
    }
}

fn js_lossy(value: &JsString) -> String {
    value
        .to_utf8()
        .unwrap_or_else(|_| String::from_utf16_lossy(value.units()))
}

// -------------------------------------------------------------- disk helpers

/// The generator's `writeInputs`: one fresh file per entry, `text` as UTF-8
/// or `bytes` verbatim.
fn write_inputs(home: &Path, files: &[JsValue]) {
    for file in files {
        let full = home.join(text(file, "path"));
        fs::create_dir_all(full.parent().unwrap_or(home))
            .unwrap_or_else(|error| panic!("#1216 create {}: {error}", full.display()));
        let written = match file
            .get("bytes")
            .filter(|value| !matches!(value, JsValue::Null))
        {
            Some(bytes) => array(bytes)
                .iter()
                .map(|unit| {
                    unit.as_number()
                        .unwrap_or_else(|| panic!("#1216 oracle bytes must be numbers: {unit:?}"))
                        as u8
                })
                .collect::<Vec<u8>>(),
            None => text(file, "text").into_bytes(),
        };
        fs::write(&full, written)
            .unwrap_or_else(|error| panic!("#1216 write {}: {error}", full.display()));
    }
}

fn collect_tree(home: &Path) -> (BTreeMap<String, Vec<u8>>, BTreeSet<String>) {
    fn walk(
        dir: &Path,
        prefix: &str,
        files: &mut BTreeMap<String, Vec<u8>>,
        dirs: &mut BTreeSet<String>,
    ) {
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Err(error) => panic!("#1216 read {}: {error}", dir.display()),
        };
        for entry in entries {
            let entry =
                entry.unwrap_or_else(|error| panic!("#1216 read {}: {error}", dir.display()));
            let name = entry.file_name().to_string_lossy().into_owned();
            let rel = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            let kind = entry
                .file_type()
                .unwrap_or_else(|error| panic!("#1216 stat {}: {error}", entry.path().display()));
            if kind.is_dir() {
                dirs.insert(rel.clone());
                walk(&entry.path(), &rel, files, dirs);
            } else if kind.is_file() {
                let bytes = fs::read(entry.path()).unwrap_or_else(|error| {
                    panic!("#1216 read {}: {error}", entry.path().display())
                });
                files.insert(rel, bytes);
            } else {
                panic!("#1216 {rel} is neither a regular file nor a directory");
            }
        }
    }
    let mut files = BTreeMap::new();
    let mut dirs = BTreeSet::new();
    walk(home, "", &mut files, &mut dirs);
    (files, dirs)
}

fn compare_tree(home: &Path, expected: &JsValue, context: &str) {
    let (actual_files, actual_dirs) = collect_tree(home);
    let mut expected_files = BTreeMap::new();
    let Some(JsValue::Object(recorded)) = expected.get("files") else {
        panic!("#1216 {context}: oracle tree files must be an object");
    };
    for (path, value) in recorded.entries() {
        let encoded = value
            .as_string()
            .unwrap_or_else(|| panic!("#1216 {context}: oracle tree entry must be base64 text"))
            .to_utf8()
            .unwrap_or_else(|error| panic!("#1216 {context}: oracle tree entry UTF-8: {error:?}"));
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&encoded)
            .unwrap_or_else(|error| panic!("#1216 {context}: oracle tree entry base64: {error}"));
        expected_files.insert(js_lossy(path), bytes);
    }
    let expected_dirs: BTreeSet<String> = list(expected, "dirs")
        .iter()
        .map(|value| {
            value
                .as_string()
                .unwrap_or_else(|| panic!("#1216 {context}: oracle dirs must be strings"))
                .to_utf8()
                .unwrap_or_else(|error| panic!("#1216 {context}: oracle dir UTF-8: {error:?}"))
        })
        .collect();
    assert_eq!(
        actual_files, expected_files,
        "#1216 {context}: replayed file tree differs from the TS oracle"
    );
    assert_eq!(
        actual_dirs, expected_dirs,
        "#1216 {context}: replayed directory tree differs from the TS oracle"
    );
}

// ------------------------------------------------------------ A. paths cells

fn replay_paths_cell(entry: &JsValue) {
    let name = text(entry, "name");
    let paths = resolve_cron_paths(&text(entry, "pilotHome"), &text(entry, "projectKey"))
        .unwrap_or_else(|error| panic!("#1216 paths {name}: resolveCronPaths failed: {error}"));
    let recorded = entry
        .get("expect")
        .unwrap_or_else(|| panic!("#1216 paths {name}: oracle cell has no expect record"));
    let expected = |field: &str| {
        recorded
            .get(field)
            .and_then(JsValue::as_string)
            .unwrap_or_else(|| panic!("#1216 paths {name}: expect.{field} must be a string"))
            .to_utf8()
            .unwrap_or_else(|error| panic!("#1216 paths {name}: expect.{field} UTF-8: {error:?}"))
    };
    assert_eq!(
        paths.pilot_home.to_string_lossy(),
        expected("pilotHome"),
        "#1216 paths {name}: pilotHome differs from TS"
    );
    assert_eq!(
        paths.project_key,
        expected("projectKey"),
        "#1216 paths {name}: projectKey differs from TS"
    );
    assert_eq!(
        paths.project_id,
        expected("projectId"),
        "#1216 paths {name}: projectId differs from TS"
    );
    assert_eq!(
        paths.root_dir.to_string_lossy(),
        expected("rootDir"),
        "#1216 paths {name}: rootDir differs from TS"
    );
    assert_eq!(
        paths.project_dir.to_string_lossy(),
        expected("projectDir"),
        "#1216 paths {name}: projectDir differs from TS"
    );
    assert_eq!(
        paths.tasks_file.to_string_lossy(),
        expected("tasksFile"),
        "#1216 paths {name}: tasksFile differs from TS"
    );
    assert_eq!(
        paths.runs_dir.to_string_lossy(),
        expected("runsDir"),
        "#1216 paths {name}: runsDir differs from TS"
    );
    assert_eq!(
        paths.run_history_file.to_string_lossy(),
        expected("runHistoryFile"),
        "#1216 paths {name}: runHistoryFile differs from TS"
    );
}

/// The matrix shape is frozen: `generate.mts` asserts the same four counts
/// before publishing `oracle.json`, and one named test below replays every
/// cell, so adding or removing a cell must update both sides.
#[test]
fn oracle_matrix_counts_match_generator() {
    for (section, count) in [
        ("paths", 9),
        ("eventsPaths", 10),
        ("store", 33),
        ("migration", 18),
    ] {
        assert_eq!(
            cells(section).len(),
            count,
            "#1216 oracle section {section} changed cell count"
        );
    }
}

#[test]
fn paths_absolute_matches_ts() {
    replay_paths_cell(cell("paths", "absolute"));
}
#[test]
fn paths_dot_segments_matches_ts() {
    replay_paths_cell(cell("paths", "dot-segments"));
}
#[test]
fn paths_trailing_slash_matches_ts() {
    replay_paths_cell(cell("paths", "trailing-slash"));
}
#[test]
fn paths_repeated_separators_matches_ts() {
    replay_paths_cell(cell("paths", "repeated-separators"));
}
#[test]
fn paths_slug_fold_matches_ts() {
    replay_paths_cell(cell("paths", "slug-fold"));
}
#[test]
fn paths_slug_root_empty_matches_ts() {
    replay_paths_cell(cell("paths", "slug-root-empty"));
}
#[test]
fn paths_backslash_and_dashes_matches_ts() {
    replay_paths_cell(cell("paths", "backslash-and-dashes"));
}
#[test]
fn paths_unicode_slug_matches_ts() {
    replay_paths_cell(cell("paths", "unicode-slug"));
}
#[test]
fn paths_collapse_runs_matches_ts() {
    replay_paths_cell(cell("paths", "collapse-runs"));
}

// ------------------------------------------------------ B. run-events paths

fn replay_events_path_cell(index: usize) {
    let entry = cells("eventsPaths")
        .get(index)
        .unwrap_or_else(|| panic!("#1216 eventsPaths has no cell {index}"));
    let run_id = entry
        .get("runId")
        .and_then(JsValue::as_string)
        .unwrap_or_else(|| panic!("#1216 eventsPaths[{index}]: runId must be a string"));
    let paths = resolve_cron_paths("/w/home", "/w/projects/main").unwrap_or_else(|error| {
        panic!("#1216 eventsPaths[{index}]: resolveCronPaths failed: {error}")
    });
    let actual = cron_run_events_path(&paths, run_id);
    assert_eq!(
        actual.to_string_lossy(),
        text(entry, "expect"),
        "#1216 eventsPaths[{index}] ({}): run events path differs from TS",
        js_lossy(run_id)
    );
}

#[test]
fn events_path_plain_matches_ts() {
    replay_events_path_cell(0);
}
#[test]
fn events_path_space_and_slash_matches_ts() {
    replay_events_path_cell(1);
}
#[test]
fn events_path_whitespace_only_matches_ts() {
    replay_events_path_cell(2);
}
#[test]
fn events_path_empty_matches_ts() {
    replay_events_path_cell(3);
}
#[test]
fn events_path_dot_runs_matches_ts() {
    replay_events_path_cell(4);
}
#[test]
fn events_path_cjk_matches_ts() {
    replay_events_path_cell(5);
}
#[test]
fn events_path_lone_surrogate_matches_ts() {
    replay_events_path_cell(6);
}
#[test]
fn events_path_edge_dashes_matches_ts() {
    replay_events_path_cell(7);
}
#[test]
fn events_path_collapse_runs_matches_ts() {
    replay_events_path_cell(8);
}
#[test]
fn events_path_mixed_allowed_matches_ts() {
    replay_events_path_cell(9);
}

// ---------------------------------------------------------- C. store cells

fn assert_step(actual: &JsValue, expected: &JsValue, name: &str, op: &str) {
    let actual = stringify_json(actual)
        .unwrap_or_else(|error| panic!("#1216 store {name}: op {op} stringify: {error}"))
        .unwrap_or_else(|| panic!("#1216 store {name}: op {op} result must be JSON"));
    let expected = stringify_json(expected)
        .unwrap_or_else(|error| panic!("#1216 store {name}: op {op} oracle stringify: {error}"))
        .unwrap_or_else(|| panic!("#1216 store {name}: op {op} oracle result must be JSON"));
    assert_eq!(
        actual, expected,
        "#1216 store {name}: op {op} result differs from the TS oracle"
    );
}

fn replay_store_cell(entry: &JsValue) {
    let name = text(entry, "name");
    let scratch = Scratch::new("cron-store");
    let home = scratch.path().join("home");
    write_inputs(&home, list(entry, "initial"));
    let paths = resolve_cron_paths(&home.to_string_lossy(), &text(entry, "projectKey"))
        .unwrap_or_else(|error| panic!("#1216 store {name}: resolveCronPaths failed: {error}"));
    let store = CronTaskStore::new(paths);
    let steps = list(entry, "steps");
    let results = list(entry, "results");
    assert_eq!(
        steps.len(),
        results.len(),
        "#1216 store {name}: every step must have one recorded result"
    );
    let mut handle: Option<fs::File> = None;
    for (step, expected) in steps.iter().zip(results) {
        let op = text(step, "op");
        let actual = match op.as_str() {
            "list" => JsValue::Array(
                store
                    .list_tasks()
                    .unwrap_or_else(|error| panic!("#1216 store {name}: listTasks: {error}")),
            ),
            "get" => store
                .get_task(&text(step, "taskId"))
                .unwrap_or_else(|error| panic!("#1216 store {name}: getTask: {error}"))
                .unwrap_or(JsValue::Null),
            "put" => {
                let task = step
                    .get("task")
                    .unwrap_or_else(|| panic!("#1216 store {name}: put step has no task"));
                store
                    .put_task(task)
                    .unwrap_or_else(|error| panic!("#1216 store {name}: putTask: {error}"));
                JsValue::Null
            }
            // `Promise.all` of synchronous calls: the per-file promise chain
            // serializes the puts in array order.
            "put-concurrent" => {
                for task in list(step, "tasks") {
                    store
                        .put_task(task)
                        .unwrap_or_else(|error| panic!("#1216 store {name}: putTask: {error}"));
                }
                JsValue::Null
            }
            "replace" => {
                let task = step
                    .get("task")
                    .unwrap_or_else(|| panic!("#1216 store {name}: replace step has no task"));
                JsValue::Bool(
                    store
                        .replace_task(task)
                        .unwrap_or_else(|error| panic!("#1216 store {name}: replaceTask: {error}")),
                )
            }
            "update" => {
                let mode = text(step, "mode");
                let updated = store
                    .update_task(&text(step, "taskId"), |existing| {
                        if mode == "remove" {
                            return None;
                        }
                        // `{...existing, message, updatedAt}`: the spread keeps
                        // the stored key order; both keys already exist.
                        let mut edited = existing
                            .as_object()
                            .unwrap_or_else(|| {
                                panic!("#1216 store {name}: update target must be an object")
                            })
                            .clone();
                        edited.insert("message".into(), JsValue::String(JsString::from("edited")));
                        edited.insert(
                            "updatedAt".into(),
                            JsValue::String(JsString::from("2026-09-09T00:00:00.000Z")),
                        );
                        Some(JsValue::Object(edited))
                    })
                    .unwrap_or_else(|error| panic!("#1216 store {name}: updateTask: {error}"));
                updated.unwrap_or(JsValue::Null)
            }
            "delete" => JsValue::Bool(
                store
                    .delete_task(&text(step, "taskId"))
                    .unwrap_or_else(|error| panic!("#1216 store {name}: deleteTask: {error}")),
            ),
            "append-run" => {
                let record = step
                    .get("record")
                    .unwrap_or_else(|| panic!("#1216 store {name}: append-run step has no record"));
                store
                    .append_run(record)
                    .unwrap_or_else(|error| panic!("#1216 store {name}: appendRun: {error}"));
                JsValue::Null
            }
            "list-runs" => {
                let limit = step
                    .get("limit")
                    .and_then(JsValue::as_number)
                    .map(|limit| limit as i64);
                JsValue::Array(
                    store
                        .list_runs(limit)
                        .unwrap_or_else(|error| panic!("#1216 store {name}: listRuns: {error}")),
                )
            }
            "append-run-event" => {
                let run_id = step
                    .get("runId")
                    .and_then(JsValue::as_string)
                    .unwrap_or_else(|| panic!("#1216 store {name}: append-run-event runId"))
                    .clone();
                let event = step
                    .get("event")
                    .unwrap_or_else(|| panic!("#1216 store {name}: append-run-event event"));
                store
                    .append_run_event(&run_id, event)
                    .unwrap_or_else(|error| panic!("#1216 store {name}: appendRunEvent: {error}"));
                JsValue::Null
            }
            "open-fd" => {
                handle = Some(
                    fs::File::open(&store.paths().tasks_file)
                        .unwrap_or_else(|error| panic!("#1216 store {name}: open fd: {error}")),
                );
                JsValue::Null
            }
            "read-fd" => {
                let mut file = handle
                    .take()
                    .unwrap_or_else(|| panic!("#1216 store {name}: read-fd without open-fd"));
                let mut content = String::new();
                file.read_to_string(&mut content)
                    .unwrap_or_else(|error| panic!("#1216 store {name}: read fd: {error}"));
                JsValue::String(JsString::from(content))
            }
            other => panic!("#1216 store {name}: unknown step op {other}"),
        };
        assert_step(&actual, expected, &name, &op);
    }
    compare_tree(
        &home,
        entry
            .get("final")
            .unwrap_or_else(|| panic!("#1216 store {name}: oracle cell has no final tree")),
        &format!("store {name}"),
    );
}

#[test]
fn store_list_missing_matches_ts() {
    replay_store_cell(cell("store", "list-missing"));
}
#[test]
fn store_list_corrupt_matches_ts() {
    replay_store_cell(cell("store", "list-corrupt"));
}
#[test]
fn store_list_bom_matches_ts() {
    replay_store_cell(cell("store", "list-bom"));
}
#[test]
fn store_list_empty_object_matches_ts() {
    replay_store_cell(cell("store", "list-empty-object"));
}
#[test]
fn store_list_tasks_not_array_matches_ts() {
    replay_store_cell(cell("store", "list-tasks-not-array"));
}
#[test]
fn store_list_schema_version_2_matches_ts() {
    replay_store_cell(cell("store", "list-schema-version-2"));
}
#[test]
fn store_list_normalize_matches_ts() {
    replay_store_cell(cell("store", "list-normalize"));
}
#[test]
fn store_get_found_and_missing_matches_ts() {
    replay_store_cell(cell("store", "get-found-and-missing"));
}
#[test]
fn store_put_new_matches_ts() {
    replay_store_cell(cell("store", "put-new"));
}
#[test]
fn store_put_new_with_optionals_matches_ts() {
    replay_store_cell(cell("store", "put-new-with-optionals"));
}
#[test]
fn store_put_same_key_replaces_and_sorts_matches_ts() {
    replay_store_cell(cell("store", "put-same-key-replaces-and-sorts"));
}
#[test]
fn store_put_after_corrupt_starts_empty_matches_ts() {
    replay_store_cell(cell("store", "put-after-corrupt-starts-empty"));
}
#[test]
fn store_replace_missing_writes_nothing_matches_ts() {
    replay_store_cell(cell("store", "replace-missing-writes-nothing"));
}
#[test]
fn store_replace_found_matches_ts() {
    replay_store_cell(cell("store", "replace-found"));
}
#[test]
fn store_update_modify_matches_ts() {
    replay_store_cell(cell("store", "update-modify"));
}
#[test]
fn store_update_remove_matches_ts() {
    replay_store_cell(cell("store", "update-remove"));
}
#[test]
fn store_update_missing_matches_ts() {
    replay_store_cell(cell("store", "update-missing"));
}
#[test]
fn store_delete_found_matches_ts() {
    replay_store_cell(cell("store", "delete-found"));
}
#[test]
fn store_delete_missing_matches_ts() {
    replay_store_cell(cell("store", "delete-missing"));
}
#[test]
fn store_concurrent_puts_serialize_matches_ts() {
    replay_store_cell(cell("store", "concurrent-puts-serialize"));
}
#[test]
fn store_atomic_replace_keeps_old_inode_bytes_matches_ts() {
    replay_store_cell(cell("store", "atomic-replace-keeps-old-inode-bytes"));
}
#[test]
fn store_append_run_new_matches_ts() {
    replay_store_cell(cell("store", "append-run-new"));
}
#[test]
fn store_append_run_twice_matches_ts() {
    replay_store_cell(cell("store", "append-run-twice"));
}
#[test]
fn store_list_runs_missing_matches_ts() {
    replay_store_cell(cell("store", "list-runs-missing"));
}
#[test]
fn store_list_runs_mixed_matches_ts() {
    replay_store_cell(cell("store", "list-runs-mixed"));
}
#[test]
fn store_list_runs_limit_0_matches_ts() {
    replay_store_cell(cell("store", "list-runs-limit-0"));
}
#[test]
fn store_list_runs_limit_negative_matches_ts() {
    replay_store_cell(cell("store", "list-runs-limit-negative"));
}
#[test]
fn store_list_runs_limit_2_matches_ts() {
    replay_store_cell(cell("store", "list-runs-limit-2"));
}
#[test]
fn store_list_runs_limit_large_matches_ts() {
    replay_store_cell(cell("store", "list-runs-limit-large"));
}
#[test]
fn store_list_runs_invalid_utf8_matches_ts() {
    replay_store_cell(cell("store", "list-runs-invalid-utf8"));
}
#[test]
fn store_append_run_event_matches_ts() {
    replay_store_cell(cell("store", "append-run-event"));
}
#[test]
fn store_append_run_event_sanitizes_matches_ts() {
    replay_store_cell(cell("store", "append-run-event-sanitizes"));
}

// ------------------------------------------------------ D. migration cells

#[derive(Default)]
struct WarningRecorder(RefCell<Vec<(String, JsObject)>>);

impl CronRuntimeLogger for WarningRecorder {
    fn warn(&self, message: &str, meta: &JsObject) {
        self.0.borrow_mut().push((message.to_owned(), meta.clone()));
    }
}

/// The lock scenarios pre-create `cron/.store-migration.lock` with an mtime
/// fixed relative to the pinned clock, exactly like the generator's
/// `utimesSync(lock, stamp, stamp)`.
fn seed_lock(home: &Path, age_ms: f64) {
    let cron = home.join("cron");
    fs::create_dir_all(&cron).unwrap_or_else(|error| panic!("#1216 seed lock directory: {error}"));
    let lock = cron.join(".store-migration.lock");
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock)
        .unwrap_or_else(|error| panic!("#1216 seed lock {}: {error}", lock.display()));
    file.write_all(b"held\n")
        .unwrap_or_else(|error| panic!("#1216 write lock {}: {error}", lock.display()));
    let stamp = UNIX_EPOCH + Duration::from_millis((PINNED_NOW_MS - age_ms) as u64);
    file.set_modified(stamp)
        .unwrap_or_else(|error| panic!("#1216 stamp lock {}: {error}", lock.display()));
}

fn migration_input<'a>(home: &Path, logger: &'a WarningRecorder) -> MigrationInput<'a> {
    MigrationInput {
        pilot_home: home.to_path_buf(),
        logger: Some(logger),
        now_ms: PINNED_NOW_MS,
        lock_retry_delay: Duration::from_millis(1),
    }
}

fn compare_warnings(recorder: &WarningRecorder, entry: &JsValue, home: &str) {
    let name = text(entry, "name");
    let actual = recorder.0.borrow();
    let expected = list(entry, "warnings");
    assert_eq!(
        actual.len(),
        expected.len(),
        "#1216 migration {name}: warning count differs from the TS oracle"
    );
    for ((message, meta), want) in actual.iter().zip(expected) {
        assert_eq!(
            message,
            &text(want, "message"),
            "#1216 migration {name}: warning message differs from the TS oracle"
        );
        let actual_meta = stringify_json(&JsValue::Object(meta.clone()))
            .unwrap_or_else(|error| {
                panic!("#1216 migration {name}: warning meta stringify: {error}")
            })
            .unwrap_or_else(|| panic!("#1216 migration {name}: warning meta must be JSON"))
            .replace(home, "{HOME}");
        let expected_meta = stringify_json(
            want.get("meta")
                .unwrap_or_else(|| panic!("#1216 migration {name}: warning has no meta")),
        )
        .unwrap_or_else(|error| panic!("#1216 migration {name}: oracle meta stringify: {error}"))
        .unwrap_or_else(|| panic!("#1216 migration {name}: oracle meta must be JSON"));
        assert_eq!(
            actual_meta, expected_meta,
            "#1216 migration {name}: warning meta differs from the TS oracle"
        );
    }
}

fn replay_migration_cell(entry: &JsValue) {
    let name = text(entry, "name");
    let scratch = Scratch::new("cron-migration");
    let home = scratch.path().join("home");
    write_inputs(&home, list(entry, "input"));
    // `staleLockMs: 11 * 60_000` / `freshLockMs: 60_000` in the generator.
    match name.as_str() {
        "stale-lock-removed" => seed_lock(&home, 11.0 * 60_000.0),
        "fresh-lock-times-out" => seed_lock(&home, 60_000.0),
        _ => {}
    }
    let recorder = WarningRecorder::default();
    let outcome = migrate_cron_stores(&migration_input(&home, &recorder));
    match (
        &outcome,
        entry
            .get("error")
            .filter(|value| !matches!(value, JsValue::Null)),
    ) {
        (Ok(()), None) => {}
        (Err(error), Some(_)) => {
            let home_text = home.to_string_lossy();
            let actual = format!("Error: {error}").replace(home_text.as_ref(), "{HOME}");
            assert_eq!(
                actual,
                text(entry, "error"),
                "#1216 migration {name}: error differs from the TS oracle"
            );
        }
        (Err(error), None) => panic!("#1216 migration {name}: unexpected error: {error}"),
        (Ok(()), Some(_)) => panic!(
            "#1216 migration {name}: expected the TS oracle error {}",
            text(entry, "error")
        ),
    }
    compare_tree(
        &home,
        entry
            .get("once")
            .unwrap_or_else(|| panic!("#1216 migration {name}: oracle cell has no once tree")),
        &format!("migration {name} once"),
    );
    if let Some(twice) = entry
        .get("twice")
        .filter(|value| !matches!(value, JsValue::Null))
    {
        migrate_cron_stores(&migration_input(&home, &recorder))
            .unwrap_or_else(|error| panic!("#1216 migration {name}: second run failed: {error}"));
        compare_tree(&home, twice, &format!("migration {name} twice"));
    }
    // The generator shares one logger across both runs, so the recorded
    // warnings are compared only after the second one.
    compare_warnings(&recorder, entry, &home.to_string_lossy());
}

#[test]
fn migration_no_cron_dir_matches_ts() {
    replay_migration_cell(cell("migration", "no-cron-dir"));
}
#[test]
fn migration_empty_projects_dir_matches_ts() {
    replay_migration_cell(cell("migration", "empty-projects-dir"));
}
#[test]
fn migration_tasks_without_projectkey_stay_matches_ts() {
    replay_migration_cell(cell("migration", "tasks-without-projectkey-stay"));
}
#[test]
fn migration_tasks_move_to_projectkey_dir_matches_ts() {
    replay_migration_cell(cell("migration", "tasks-move-to-projectkey-dir"));
}
#[test]
fn migration_runs_by_explicit_projectkey_matches_ts() {
    replay_migration_cell(cell("migration", "runs-by-explicit-projectkey"));
}
#[test]
fn migration_runs_by_unique_task_project_matches_ts() {
    replay_migration_cell(cell("migration", "runs-by-unique-task-project"));
}
#[test]
fn migration_runs_ambiguous_task_stay_matches_ts() {
    replay_migration_cell(cell("migration", "runs-ambiguous-task-stay"));
}
#[test]
fn migration_blocked_dir_keeps_source_tasks_matches_ts() {
    replay_migration_cell(cell("migration", "blocked-dir-keeps-source-tasks"));
}
#[test]
fn migration_blocked_dir_tasks_not_array_matches_ts() {
    replay_migration_cell(cell("migration", "blocked-dir-tasks-not-array"));
}
#[test]
fn migration_invalid_run_lines_preserved_matches_ts() {
    replay_migration_cell(cell("migration", "invalid-run-lines-preserved"));
}
#[test]
fn migration_run_events_merge_and_move_matches_ts() {
    replay_migration_cell(cell("migration", "run-events-merge-and-move"));
}
#[test]
fn migration_duplicate_tasks_dedupe_matches_ts() {
    replay_migration_cell(cell("migration", "duplicate-tasks-dedupe"));
}
#[test]
fn migration_missing_tasks_file_is_writable_matches_ts() {
    replay_migration_cell(cell("migration", "missing-tasks-file-is-writable"));
}
#[test]
fn migration_non_directory_entries_skipped_matches_ts() {
    replay_migration_cell(cell("migration", "non-directory-entries-skipped"));
}
#[test]
fn migration_twice_is_idempotent_matches_ts() {
    replay_migration_cell(cell("migration", "migration-twice-is-idempotent"));
}
#[test]
fn migration_stale_lock_removed_matches_ts() {
    replay_migration_cell(cell("migration", "stale-lock-removed"));
}
#[test]
fn migration_fresh_lock_times_out_matches_ts() {
    replay_migration_cell(cell("migration", "fresh-lock-times-out"));
}

// --------------------------------------------------------- locale-gated cells

fn run_locale_case(case: &str) {
    crate::test_process::run_cases(
        "runtime::cron::tests::cron_locale_worker",
        vec![(
            case.to_owned(),
            vec![
                ("LANG".to_owned(), "en_US.UTF-8".to_owned()),
                ("PDRS_CRON_CASE".to_owned(), case.to_owned()),
            ],
        )],
    );
}

#[test]
#[ignore = "only launched by locale-pinned parents"]
fn cron_locale_worker() {
    let case = std::env::var("PDRS_CRON_CASE")
        .unwrap_or_else(|error| panic!("#1216 PDRS_CRON_CASE must be set: {error}"));
    let (section, name) = case
        .split_once(':')
        .unwrap_or_else(|| panic!("#1216 PDRS_CRON_CASE must be 'section:cell': {case}"));
    match section {
        "store" => replay_store_cell(cell("store", name)),
        "migration" => replay_migration_cell(cell("migration", name)),
        other => panic!("#1216 unknown PDRS_CRON_CASE section {other}"),
    }
    crate::test_process::receipt(&case);
}

/// `localeCompare` orders `ä` against `z`/`Z`: recorded under `en-US`, so the
/// cell replays in a `LANG=en_US.UTF-8` child like every other
/// collation-sensitive port.
#[test]
fn store_put_sort_missing_and_collation_matches_ts() {
    run_locale_case("store:put-sort-missing-and-collation");
}

/// Run dedupe + ordering under recorded collation cells (`z`/`a` keys).
#[test]
fn migration_duplicate_runs_dedupe_matches_ts() {
    run_locale_case("migration:duplicate-runs-dedupe");
}
