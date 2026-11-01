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
    path::{Path, PathBuf},
    sync::{Arc, Barrier, LazyLock},
    thread,
    time::{Duration, UNIX_EPOCH},
};

use base64::Engine as _;
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};

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
        ("migration", 23),
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

fn migration_input<'a>(pilot_home: PathBuf, logger: &'a WarningRecorder) -> MigrationInput<'a> {
    MigrationInput {
        pilot_home,
        logger: Some(logger),
        now_ms: PINNED_NOW_MS,
        lock_retry_delay: Duration::from_millis(1),
    }
}

/// Lexical `path.relative(cwd, home)`: a caller that passes a relative
/// pilotHome, which the entry resolve must fold back to the same physical
/// home (fix-r1 P0).
fn relative_pilot_home(home: &Path) -> PathBuf {
    let cwd = std::env::current_dir()
        .unwrap_or_else(|error| panic!("#1216 migration relative home: current_dir: {error}"));
    let cwd: Vec<_> = cwd.components().collect();
    let target: Vec<_> = home.components().collect();
    let mut shared = 0;
    while shared < cwd.len() && shared < target.len() && cwd[shared] == target[shared] {
        shared += 1;
    }
    let mut out = PathBuf::new();
    for _ in shared..cwd.len() {
        out.push("..");
    }
    for component in &target[shared..] {
        out.push(component.as_os_str());
    }
    assert!(
        !out.as_os_str().is_empty(),
        "#1216 migration relative home: the scratch home must differ from the cwd"
    );
    out
}

/// chmod 000 seeds mirroring the generator's read-error inputs: the blocked
/// file's bytes are recorded through the restore, because the migration
/// leaves a blocked store untouched. The original mode is restored, not a
/// blanket 0644.
#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(mode);
    fs::set_permissions(path, permissions)
}

#[cfg(unix)]
fn mode_of(path: &Path) -> std::io::Result<u32> {
    use std::os::unix::fs::PermissionsExt;
    Ok(fs::metadata(path)?.permissions().mode())
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
    // A relative pilotHome cell passes the scratch home the way a caller
    // would: relative to the process cwd.
    let pilot_home = if entry
        .get("pilotHomeRelative")
        .is_some_and(|value| !matches!(value, JsValue::Null | JsValue::Bool(false)))
    {
        relative_pilot_home(&home)
    } else {
        home.to_path_buf()
    };
    // The generator's `chmod000` seeds: unreadable before the migration,
    // restored to the saved mode before the tree is recorded.
    let chmod000: Vec<String> = match entry.get("chmod000") {
        Some(JsValue::Array(items)) => items
            .iter()
            .map(|value| {
                js_lossy(value.as_string().unwrap_or_else(|| {
                    panic!("#1216 migration {name}: chmod000 entries must be strings")
                }))
            })
            .collect(),
        _ => Vec::new(),
    };
    #[cfg(unix)]
    let blocked_modes: Vec<(PathBuf, u32)> = chmod000
        .iter()
        .map(|path| {
            let full = home.join(path);
            let mode = mode_of(&full).unwrap_or_else(|error| {
                panic!(
                    "#1216 migration {name}: stat chmod seed {}: {error}",
                    full.display()
                )
            });
            set_mode(&full, 0o000).unwrap_or_else(|error| {
                panic!(
                    "#1216 migration {name}: chmod 000 {}: {error}",
                    full.display()
                )
            });
            (full, mode)
        })
        .collect();
    let recorder = WarningRecorder::default();
    let outcome = migrate_cron_stores(&migration_input(pilot_home, &recorder));
    #[cfg(unix)]
    for (full, mode) in &blocked_modes {
        set_mode(full, *mode).unwrap_or_else(|error| {
            panic!(
                "#1216 migration {name}: restore mode {mode:o} {}: {error}",
                full.display()
            )
        });
    }
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
        migrate_cron_stores(&migration_input(home.to_path_buf(), &recorder))
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
fn migration_relative_home_same_dir_projectkey_matches_ts() {
    replay_migration_cell(cell("migration", "relative-home-same-dir-projectkey"));
}
#[test]
fn migration_relative_home_blocked_dir_matches_ts() {
    replay_migration_cell(cell("migration", "relative-home-blocked-dir"));
}
#[cfg(unix)]
#[test]
fn migration_blocked_dir_tasks_eacces_matches_ts() {
    if rustix::process::geteuid().as_raw() == 0 {
        // chmod 000 cannot make the tasks file unreadable for root: the cell
        // would fail on its recorded warning/tree, not on parity. The read
        // error itself is still covered by the EISDIR cell below.
        return;
    }
    replay_migration_cell(cell("migration", "blocked-dir-tasks-eacces"));
}
#[test]
fn migration_blocked_dir_tasks_isdir_matches_ts() {
    replay_migration_cell(cell("migration", "blocked-dir-tasks-isdir"));
}
#[test]
fn migration_lone_surrogate_projectkey_matches_ts() {
    replay_migration_cell(cell("migration", "lone-surrogate-projectkey"));
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

// ------------------------------------------------------- fix-r1 added guards

/// fix-r1: `rm(lockPath, {force: true})` on both the stale-removal path and
/// the `finally` release tolerates a lock file another migrator already
/// removed; an ENOENT there must not fail the migration.
#[test]
fn lock_removal_tolerates_a_vanished_lock_file() {
    let scratch = Scratch::new("cron-lock-removal");
    let missing = scratch.path().join(".store-migration.lock");
    super::migration::remove_lock(&missing).unwrap_or_else(|error| {
        panic!("#1216 remove_lock must tolerate a vanished lock file: {error}")
    });
}

/// A CronTask record in the canonical key order, for tests that build their
/// own tasks instead of replaying a recorded one.
fn fresh_task(task_id: &str) -> JsValue {
    let raw = format!(
        r#"{{"schemaVersion":1,"taskId":"{task_id}","message":"m","schedule":{{"type":"cron","expression":"* * * * *"}},"status":"scheduled","sessionKey":"s","channelKey":"im","createdAt":"2026-01-01T00:00:00.000Z","updatedAt":"2026-01-01T00:00:00.000Z"}}"#
    );
    parse_json(&raw).unwrap_or_else(|error| {
        panic!("#1216 concurrent store: task {task_id} must parse: {error:?}")
    })
}

/// fix-r1 P0: `taskFileMutationTails` serializes every read-modify-write over
/// one tasks file process-wide (across store instances), so concurrent
/// callers must not lose updates. Every thread starts at one barrier and
/// drives its own store over the same tasks file, exactly like concurrent
/// requests over shared gateway state.
#[test]
fn store_concurrent_puts_across_threads_keep_every_task() {
    let scratch = Scratch::new("cron-store-concurrent");
    let home = Arc::new(scratch.path().join("home").to_string_lossy().into_owned());
    const THREADS: usize = 8;
    const ROUNDS: usize = 24;
    let barrier = Arc::new(Barrier::new(THREADS));
    let mut handles = Vec::new();
    for thread in 0..THREADS {
        let home = Arc::clone(&home);
        let barrier = Arc::clone(&barrier);
        handles.push(thread::spawn(move || {
            let paths = resolve_cron_paths(&home, "/w/my-project").unwrap_or_else(|error| {
                panic!("#1216 concurrent store: resolveCronPaths failed: {error}")
            });
            let store = CronTaskStore::new(paths);
            barrier.wait();
            for round in 0..ROUNDS {
                store
                    .put_task(&fresh_task(&format!("t-{thread:02}-{round:02}")))
                    .unwrap_or_else(|error| {
                        panic!("#1216 concurrent store: thread {thread} put {round}: {error}")
                    });
            }
        }));
    }
    for (index, handle) in handles.into_iter().enumerate() {
        handle
            .join()
            .unwrap_or_else(|_| panic!("#1216 concurrent store: worker {index} panicked"));
    }
    let paths = resolve_cron_paths(&home, "/w/my-project")
        .unwrap_or_else(|error| panic!("#1216 concurrent store: resolveCronPaths failed: {error}"));
    let listed = CronTaskStore::new(paths)
        .list_tasks()
        .unwrap_or_else(|error| panic!("#1216 concurrent store: listTasks failed: {error}"));
    assert_eq!(
        listed.len(),
        THREADS * ROUNDS,
        "#1216 concurrent store: lost updates — only {} of {} tasks survived concurrent puts",
        listed.len(),
        THREADS * ROUNDS
    );
}

// --------------------------------------------------------- fix-r2 added guards

/// fix-r2 P1: a non-UTF-8 pilot home must keep one physical key. The entry
/// resolve keeps the raw home bytes; re-parsing the home through a lossy
/// string would key the same physical directory twice and the later empty
/// bucket would overwrite the migrated tasks (the `/w/same` projectKey
/// targets its own source directory).
#[cfg(unix)]
#[test]
fn migration_non_utf8_home_keeps_one_physical_key() {
    use std::os::unix::ffi::OsStrExt;
    let scratch = Scratch::new("cron-migration-non-utf8");
    let home = scratch
        .path()
        .join(std::ffi::OsStr::from_bytes(b"home-\xff"));
    let project = home.join("cron/projects/w-same");
    fs::create_dir_all(&project).unwrap_or_else(|error| {
        panic!(
            "#1216 non-UTF-8 home: create {}: {error}",
            project.display()
        )
    });
    fs::write(
        project.join("tasks.json"),
        "{\n  \"schemaVersion\": 1,\n  \"tasks\": [\n    {\n      \"schemaVersion\": 1,\n      \"taskId\": \"a1\",\n      \"message\": \"hello\",\n      \"schedule\": {\"type\": \"cron\", \"expression\": \"*/5 * * * *\"},\n      \"status\": \"scheduled\",\n      \"sessionKey\": \"sess-1\",\n      \"channelKey\": \"im\",\n      \"projectKey\": \"/w/same\",\n      \"createdAt\": \"2026-01-01T00:00:00.000Z\",\n      \"updatedAt\": \"2026-01-01T00:00:00.000Z\"\n    }\n  ]\n}\n",
    )
    .unwrap_or_else(|error| panic!("#1216 non-UTF-8 home: write tasks.json: {error}"));
    let recorder = WarningRecorder::default();
    migrate_cron_stores(&migration_input(home.clone(), &recorder))
        .unwrap_or_else(|error| panic!("#1216 non-UTF-8 home: migration failed: {error}"));
    let tasks = fs::read(project.join("tasks.json"))
        .unwrap_or_else(|error| panic!("#1216 non-UTF-8 home: read tasks.json back: {error}"));
    assert!(
        String::from_utf8_lossy(&tasks).contains("\"a1\""),
        "#1216 non-UTF-8 home: the same-directory projectKey must keep one physical key; a lossy re-parse of the home overwrites the source tasks.json with an empty bucket"
    );
    assert!(
        home.join("cron/store-migration-v1.json").is_file(),
        "#1216 non-UTF-8 home: the migration marker must be written"
    );
}

/// fix-r2 P2 + fix-r3 P2: an absolute pilot home and an absolute projectKey
/// must not depend on the process cwd. Node's `resolve(input.pilotHome,
/// "cron")` and the one-argument `resolve(projectKey)` only consult
/// `process.cwd()` when the argument is relative (verified against node
/// v22.22.0: with the cwd removed, `process.cwd()` throws ENOENT while
/// `path.resolve("/tmp/abs-home", "cron")` returns normally, and
/// `resolveCronPaths` + the projectKey-routed `migrateCronStores` complete —
/// `fix-r3/probes/deleted-cwd-project-key.mjs`).
#[test]
fn migration_absolute_home_survives_deleted_cwd() {
    crate::test_process::run_cases(
        "runtime::cron::tests::cron_deleted_cwd_worker",
        vec![(
            "absolute-home-deleted-cwd".to_owned(),
            vec![(
                "PDRS_CRON_CASE".to_owned(),
                "absolute-home-deleted-cwd".to_owned(),
            )],
        )],
    );
}

#[test]
#[ignore = "only launched by a cwd-controlling parent"]
fn cron_deleted_cwd_worker() {
    let case = std::env::var("PDRS_CRON_CASE").unwrap_or_else(|error| {
        panic!("#1216 cron deleted-cwd worker: PDRS_CRON_CASE must be set: {error}")
    });
    assert_eq!(
        case, "absolute-home-deleted-cwd",
        "#1216 cron deleted-cwd worker: unknown case {case}"
    );
    // Delete this process's own cwd: a cwd-dependent resolve would fail with
    // ENOENT here, which is exactly what an absolute home must not read.
    let cwd_scratch = Scratch::new("cron-deleted-cwd");
    std::env::set_current_dir(cwd_scratch.path()).unwrap_or_else(|error| {
        panic!("#1216 cron deleted-cwd worker: chdir into the scratch cwd: {error}")
    });
    fs::remove_dir_all(cwd_scratch.path()).unwrap_or_else(|error| {
        panic!("#1216 cron deleted-cwd worker: remove the scratch cwd: {error}")
    });
    let home_scratch = Scratch::new("cron-deleted-cwd-home");
    let home = home_scratch.path().join("home");
    let project = home.join("cron/projects/alpha");
    fs::create_dir_all(&project).unwrap_or_else(|error| {
        panic!(
            "#1216 cron deleted-cwd worker: create {}: {error}",
            project.display()
        )
    });
    fs::write(
        project.join("tasks.json"),
        "{\n  \"schemaVersion\": 1,\n  \"tasks\": [\n    {\n      \"schemaVersion\": 1,\n      \"taskId\": \"a1\",\n      \"message\": \"hello\",\n      \"schedule\": {\"type\": \"cron\", \"expression\": \"*/5 * * * *\"},\n      \"status\": \"scheduled\",\n      \"sessionKey\": \"sess-1\",\n      \"channelKey\": \"im\",\n      \"projectKey\": \"/w/absolute-project\",\n      \"createdAt\": \"2026-01-01T00:00:00.000Z\",\n      \"updatedAt\": \"2026-01-01T00:00:00.000Z\"\n    }\n  ]\n}\n",
    )
    .unwrap_or_else(|error| {
        panic!("#1216 cron deleted-cwd worker: write tasks.json: {error}")
    });
    // fix-r3 P2: the public entry must resolve an absolute projectKey with
    // the process cwd gone — the same one-argument `resolve` the routing
    // target derives from (paths.rs), verified against node v22.22.0 in
    // `_tmp/20261026-issue-1216/fix-r3/probes/deleted-cwd-project-key.mjs`.
    let routed =
        resolve_cron_paths(&home.to_string_lossy(), "/w/absolute-project").unwrap_or_else(
            |error| panic!(
                "#1216 cron deleted-cwd worker: resolveCronPaths must not depend on the process cwd: {error}"
            ),
        );
    let recorder = WarningRecorder::default();
    migrate_cron_stores(&migration_input(home.clone(), &recorder)).unwrap_or_else(|error| {
        panic!(
            "#1216 cron deleted-cwd worker: absolute cron inputs (pilot home, projectKey) must not depend on the process cwd: {error}"
        )
    });
    let routed_tasks = fs::read(&routed.tasks_file).unwrap_or_else(|error| {
        panic!(
            "#1216 cron deleted-cwd worker: read the routed tasks.json {}: {error}",
            routed.tasks_file.display()
        )
    });
    assert!(
        String::from_utf8_lossy(&routed_tasks).contains("\"a1\""),
        "#1216 cron deleted-cwd worker: the projectKey-routed task must land in the routed project dir without a process cwd"
    );
    assert!(
        home.join("cron/store-migration-v1.json").is_file(),
        "#1216 cron deleted-cwd worker: the migration marker must be written"
    );
    crate::test_process::receipt(&case);
}

/// fix-r2 P2: the `LockRelease` Drop guard is the `finally` half of the TS
/// migration (CronStoreMigration.ts:102-105): a panicking body must still
/// release the migration lock. An unparsable tasks.json marks the store
/// blocked, so the body warns — and this logger panics inside that warn —
/// while the lock is held.
#[test]
fn panicking_migration_body_still_releases_the_lock() {
    struct PanickingWarnLogger;
    impl CronRuntimeLogger for PanickingWarnLogger {
        fn warn(&self, _message: &str, _meta: &JsObject) {
            panic!("#1216 lock guard probe: warn() panics like a failing runtime sink");
        }
    }
    let scratch = Scratch::new("cron-lock-panic");
    let home = scratch.path().join("home");
    let project = home.join("cron/projects/alpha");
    fs::create_dir_all(&project)
        .unwrap_or_else(|error| panic!("#1216 lock guard: create {}: {error}", project.display()));
    fs::write(project.join("tasks.json"), "not json\n")
        .unwrap_or_else(|error| panic!("#1216 lock guard: write tasks.json: {error}"));
    let logger = PanickingWarnLogger;
    let input = MigrationInput {
        pilot_home: home.clone(),
        logger: Some(&logger),
        now_ms: PINNED_NOW_MS,
        lock_retry_delay: Duration::from_millis(1),
    };
    let lock_path = home.join("cron/.store-migration.lock");
    let caught =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| migrate_cron_stores(&input)));
    assert!(
        caught.is_err(),
        "#1216 lock guard: the seeded logger must panic inside the migration body"
    );
    assert!(
        !lock_path.exists(),
        "#1216 lock guard: the Drop guard must release the migration lock after a panic"
    );
}

/// fix-r2 P3: `taskFileMutationTails` drops a tail that is still the last
/// one (CronTaskStore.ts:168-172), so the process-wide registry is bounded by
/// the in-flight tasks files, not by every file ever touched.
#[test]
fn store_tail_entry_removed_after_uncontended_mutation() {
    let scratch = Scratch::new("cron-store-tail");
    let home = scratch.path().join("home");
    let paths = resolve_cron_paths(&home.to_string_lossy(), "/w/my-project")
        .unwrap_or_else(|error| panic!("#1216 tail removal: resolveCronPaths failed: {error}"));
    let tasks_file = paths.tasks_file.clone();
    let store = CronTaskStore::new(paths);
    store
        .put_task(&fresh_task("t-tail-1"))
        .unwrap_or_else(|error| panic!("#1216 tail removal: putTask: {error}"));
    assert!(
        !super::store::task_file_tail_registered(&tasks_file),
        "#1216 tail removal: an uncontended mutation must drop its finished tail from the registry"
    );
}

/// fix-r3 P2: two overlapping mutations serialize their finish under the
/// registry lock (store.rs `release_task_file_tail`). The first finisher must
/// keep the entry while the sibling's clone is still alive — removing it
/// there would hand the next mutation a second tail for the same file and
/// lose the per-file serialization — and the last finisher must remove it.
/// Both barriers make the interleaving deterministic: both clones exist
/// before either release starts, and the second release only runs after the
/// first one returned, whatever the scheduler does inside them.
#[test]
fn store_tail_entry_removed_after_contended_mutation_pair() {
    let scratch = Scratch::new("cron-store-tail-contended");
    let home = scratch.path().join("home");
    let paths = resolve_cron_paths(&home.to_string_lossy(), "/w/my-project")
        .unwrap_or_else(|error| panic!("#1216 tail contended: resolveCronPaths failed: {error}"));
    let tasks_file = paths.tasks_file.clone();
    let both_cloned = Arc::new(Barrier::new(2));
    let first_returned = Arc::new(Barrier::new(2));
    let mut handles = Vec::new();
    for early_finisher in [true, false] {
        let tasks_file = tasks_file.clone();
        let both_cloned = Arc::clone(&both_cloned);
        let first_returned = Arc::clone(&first_returned);
        handles.push(thread::spawn(move || {
            let tail = super::store::task_file_tail(&tasks_file);
            both_cloned.wait();
            if early_finisher {
                super::store::release_task_file_tail(&tasks_file, tail);
                first_returned.wait();
            } else {
                first_returned.wait();
                assert!(
                    super::store::task_file_tail_registered(&tasks_file),
                    "#1216 tail contended: a finished release must not remove the tail entry while the sibling mutation's clone is still alive"
                );
                super::store::release_task_file_tail(&tasks_file, tail);
            }
        }));
    }
    for handle in handles {
        handle
            .join()
            .unwrap_or_else(|_| panic!("#1216 tail contended: a finishing thread panicked"));
    }
    assert!(
        !super::store::task_file_tail_registered(&tasks_file),
        "#1216 tail contended: the last finishing mutation must still remove the tail entry after the overlapping finisher kept it"
    );
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
