//! Port of TS `src/cron/storage/CronStoreMigration.ts` (472 lines; line refs
//! below).
use std::{
    collections::{HashMap, HashSet},
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    thread,
    time::{Duration, UNIX_EPOCH},
};

use icu_collator::CollatorBorrowed;
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json};

use super::paths::{CronPaths, cron_run_events_path, resolve_cron_paths_from};
use super::{
    CronRuntimeLogger, CronStoreError, iso_from_epoch_ms, parent_of, string_or_empty,
    stringify_compact, stringify_pretty, temp_suffix,
};
use crate::{
    metadata::{self, js_trim},
    project_files::node_compare_utf16,
};

/// CronStoreMigration.ts:27.
const LOCK_STALE_MS: f64 = 10.0 * 60_000.0;

/// CronStoreMigration.ts:423: 100 attempts.
const LOCK_ATTEMPTS: usize = 100;

pub struct MigrationInput<'a> {
    pub pilot_home: PathBuf,
    pub logger: Option<&'a dyn CronRuntimeLogger>,
    /// Epoch milliseconds for `new Date()`/`Date.now()`; callers pin it when
    /// the marker's `completedAt` or lock staleness must be deterministic.
    pub now_ms: f64,
    /// CronStoreMigration.ts:445 sleeps 100ms between lock attempts; callers
    /// that exercise the timeout shrink it.
    pub lock_retry_delay: Duration,
}

/// CronStoreMigration.ts:29-106.
pub fn migrate_cron_stores(input: &MigrationInput<'_>) -> Result<(), CronStoreError> {
    // CronStoreMigration.ts:33: `resolve(input.pilotHome, "cron")` — the one
    // resolve that also absolutizes a relative pilot home. Every derived key
    // (snapshot directories, routing targets, blocked directories) must come
    // from this single absolute home: deriving some from the raw relative
    // input would key the same physical directory twice (a relative and an
    // absolute `PathBuf`), and the later empty bucket would overwrite the
    // migrated tasks and runs there. Like Node's one-argument `resolve`, an
    // absolute home never consults the process cwd (which may be gone), and
    // the home keeps its raw bytes — a lossy re-parse would re-create the
    // double key for a non-UTF-8 home.
    let home = metadata::resolve_process_path(Path::new(""), &input.pilot_home)?;
    let root_dir = metadata::resolve(&home, "cron");
    let lock_path = acquire_migration_lock(input, &root_dir)?;
    // CronStoreMigration.ts:102-105: `finally { await releaseLock(); }` runs on
    // a throw too, so a panic in the body must still release the lock.
    let mut release = LockRelease(Some(lock_path.clone()));
    let body = migrate_locked(input, &home, &root_dir);
    // A release failure replaces the body result, like a rejected finally.
    // A successful explicit release disarms the guard: its `Drop` must not
    // remove a lock file another migrator has freshly acquired in between.
    match remove_lock(&lock_path) {
        Ok(()) => {
            release.0.take();
            body
        }
        Err(error) => Err(error.into()),
    }
}

/// The `finally` half of CronStoreMigration.ts:102-105 for panicking bodies.
/// Disarmed by `take()` once the explicit release succeeded.
struct LockRelease(Option<PathBuf>);

impl Drop for LockRelease {
    fn drop(&mut self) {
        if let Some(lock_path) = self.0.take() {
            let _ = remove_lock(&lock_path);
        }
    }
}

fn migrate_locked(
    input: &MigrationInput<'_>,
    home: &Path,
    root_dir: &Path,
) -> Result<(), CronStoreError> {
    let snapshots = read_snapshots(&metadata::resolve(root_dir, "projects"))?;
    if snapshots.is_empty() {
        return Ok(());
    }

    // CronStoreMigration.ts:39-44.
    let blocked_task_dirs: Vec<PathBuf> = snapshots
        .iter()
        .filter(|snapshot| !snapshot.task_file_writable)
        .map(|snapshot| snapshot.dir.clone())
        .collect();
    for dir in &blocked_task_dirs {
        warn(
            input.logger,
            "cron migration preserved an unreadable task store",
            &[("projectDir", display_value(dir))],
        );
    }
    let task_projects = build_task_project_index(&snapshots)?;
    // TS `Map` insertion order: a task's first target directory registers the
    // slot, snapshot directories without routed-in data are appended by the
    // normalization loop below, and both write passes iterate this order.
    let mut final_tasks: Vec<(PathBuf, Vec<JsValue>)> = Vec::new();
    let mut final_runs: Vec<(PathBuf, RunBucket)> = Vec::new();
    let mut run_targets: HashMap<JsString, HashSet<PathBuf>> = HashMap::new();

    for snapshot in &snapshots {
        for task in &snapshot.tasks {
            let target_dir =
                resolve_task_target_dir(home, &snapshot.dir, task, &blocked_task_dirs)?;
            match final_tasks.iter_mut().find(|(dir, _)| *dir == target_dir) {
                Some((_, values)) => values.push(task.clone()),
                None => final_tasks.push((target_dir, vec![task.clone()])),
            }
        }
        for line in &snapshot.invalid_run_lines {
            run_bucket(&mut final_runs, &snapshot.dir)
                .invalid_lines
                .push(line.clone());
        }
        for run in &snapshot.runs {
            let target_dir = resolve_run_target_dir(
                home,
                &snapshot.dir,
                run,
                &task_projects,
                &blocked_task_dirs,
            )?;
            run_bucket(&mut final_runs, &target_dir)
                .records
                .push(run.clone());
            if let Some(run_id) = run.get("runId").and_then(JsValue::as_string) {
                run_targets
                    .entry(run_id.clone())
                    .or_default()
                    .insert(target_dir.clone());
            }
        }
    }

    // CronStoreMigration.ts:81-86.
    for snapshot in &snapshots {
        if !final_tasks.iter().any(|(dir, _)| dir == &snapshot.dir) {
            final_tasks.push((snapshot.dir.clone(), Vec::new()));
        }
        if !final_runs.iter().any(|(dir, _)| dir == &snapshot.dir) {
            final_runs.push((snapshot.dir.clone(), RunBucket::default()));
        }
    }
    for (dir, tasks) in &mut final_tasks {
        let deduped = dedupe_tasks(tasks, input, dir)?;
        *tasks = deduped;
    }
    for (_, bucket) in &mut final_runs {
        bucket.records = dedupe_runs(&bucket.records)?;
    }

    stage_destination_data(input, &snapshots, &final_tasks, &final_runs)?;
    migrate_run_events(input, &snapshots, &run_targets)?;
    write_final_snapshots(input, &snapshots, &final_tasks, &final_runs)?;

    // CronStoreMigration.ts:98-102: the marker is a plain write, not atomic.
    let mut marker = JsObject::new();
    marker.insert("version".into(), JsValue::Number(1.0));
    marker.insert(
        "completedAt".into(),
        JsValue::String(iso_from_epoch_ms(input.now_ms).into()),
    );
    fs::write(
        root_dir.join("store-migration-v1.json"),
        format!("{}\n", stringify_pretty(&JsValue::Object(marker))),
    )?;
    Ok(())
}

/// CronStoreMigration.ts:19-25,108-127.
struct StoreSnapshot {
    dir: PathBuf,
    tasks: Vec<JsValue>,
    task_file_writable: bool,
    invalid_run_lines: Vec<String>,
    runs: Vec<JsValue>,
}

#[derive(Default)]
struct RunBucket {
    records: Vec<JsValue>,
    invalid_lines: Vec<String>,
}

/// CronStoreMigration.ts:465-472. `map.set` keeps the first insertion's slot,
/// so repeated lookups never move an existing bucket.
fn run_bucket<'a>(map: &'a mut Vec<(PathBuf, RunBucket)>, key: &Path) -> &'a mut RunBucket {
    let index = match map.iter().position(|(dir, _)| dir == key) {
        Some(index) => index,
        None => {
            map.push((key.to_path_buf(), RunBucket::default()));
            map.len() - 1
        }
    };
    &mut map[index].1
}

/// CronStoreMigration.ts:108-127.
fn read_snapshots(projects_dir: &Path) -> Result<Vec<StoreSnapshot>, CronStoreError> {
    let entries = match fs::read_dir(projects_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut snapshots = Vec::new();
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let dir = metadata::resolve(projects_dir, &entry.file_name().to_string_lossy());
        let (tasks, task_file_writable) = read_tasks(&dir.join("tasks.json"))?;
        let (runs, invalid_run_lines) = read_runs(&dir.join("run-history.jsonl"))?;
        snapshots.push(StoreSnapshot {
            dir,
            tasks,
            task_file_writable,
            invalid_run_lines,
            runs,
        });
    }
    Ok(snapshots)
}

/// CronStoreMigration.ts:129-144.
fn read_tasks(path: &Path) -> Result<(Vec<JsValue>, bool), CronStoreError> {
    let raw = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok((Vec::new(), true)),
        // The `catch` treats every other read error (EACCES, EISDIR, …) as the
        // same blocked, preserved store as a parse failure.
        Err(_) => return Ok((Vec::new(), false)),
    };
    let parsed = match parse_json(&String::from_utf8_lossy(&raw)) {
        Ok(parsed) => parsed,
        Err(_) => return Ok((Vec::new(), false)),
    };
    match parsed.as_object().and_then(|object| object.get("tasks")) {
        Some(JsValue::Array(tasks)) => Ok((tasks.clone(), true)),
        _ => Ok((Vec::new(), false)),
    }
}

/// CronStoreMigration.ts:146-172.
fn read_runs(path: &Path) -> Result<(Vec<JsValue>, Vec<String>), CronStoreError> {
    let raw = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok((Vec::new(), Vec::new()));
        }
        Err(error) => return Err(error.into()),
    };
    let raw = String::from_utf8_lossy(&raw);
    let mut runs = Vec::new();
    let mut invalid_run_lines = Vec::new();
    for line in raw.split('\n') {
        if js_trim(line).is_empty() {
            continue;
        }
        match parse_json(line) {
            Ok(value) if value.as_object().is_some() => runs.push(value),
            _ => invalid_run_lines.push(line.to_string()),
        }
    }
    Ok((runs, invalid_run_lines))
}

/// CronStoreMigration.ts:174-187.
fn build_task_project_index(
    snapshots: &[StoreSnapshot],
) -> Result<HashMap<JsString, std::collections::BTreeSet<String>>, CronStoreError> {
    let mut index: HashMap<JsString, std::collections::BTreeSet<String>> = HashMap::new();
    for snapshot in snapshots {
        for task in &snapshot.tasks {
            let Some(record) = task.as_object() else {
                continue;
            };
            let Some(task_id) = record.get("taskId").and_then(JsValue::as_string) else {
                continue;
            };
            let Some(project_key) = normalized_project_key(record.get("projectKey"))? else {
                continue;
            };
            index
                .entry(task_id.clone())
                .or_default()
                .insert(project_key);
        }
    }
    Ok(index)
}

/// CronStoreMigration.ts:189-201.
fn resolve_task_target_dir(
    home: &Path,
    source_dir: &Path,
    task: &JsValue,
    blocked_task_dirs: &[PathBuf],
) -> Result<PathBuf, CronStoreError> {
    let Some(record) = task.as_object() else {
        return Ok(source_dir.to_path_buf());
    };
    let Some(project_key) = normalized_project_key(record.get("projectKey"))? else {
        return Ok(source_dir.to_path_buf());
    };
    let target_dir = resolve_cron_paths_from(home, &project_key)?.project_dir;
    Ok(if blocked_task_dirs.contains(&target_dir) {
        source_dir.to_path_buf()
    } else {
        target_dir
    })
}

/// CronStoreMigration.ts:203-220.
fn resolve_run_target_dir(
    home: &Path,
    source_dir: &Path,
    run: &JsValue,
    task_projects: &HashMap<JsString, std::collections::BTreeSet<String>>,
    blocked_task_dirs: &[PathBuf],
) -> Result<PathBuf, CronStoreError> {
    let run = run.as_object();
    if let Some(explicit_project) =
        normalized_project_key(run.and_then(|record| record.get("projectKey")))?
    {
        let target_dir = resolve_cron_paths_from(home, &explicit_project)?.project_dir;
        return Ok(if blocked_task_dirs.contains(&target_dir) {
            source_dir.to_path_buf()
        } else {
            target_dir
        });
    }
    let Some(task_id) = run
        .and_then(|record| record.get("taskId"))
        .and_then(JsValue::as_string)
    else {
        return Ok(source_dir.to_path_buf());
    };
    let Some(projects) = task_projects.get(task_id) else {
        return Ok(source_dir.to_path_buf());
    };
    if projects.len() != 1 {
        return Ok(source_dir.to_path_buf());
    }
    let project_key = projects.iter().next().expect("the only project key");
    let target_dir = resolve_cron_paths_from(home, project_key)?.project_dir;
    Ok(if blocked_task_dirs.contains(&target_dir) {
        source_dir.to_path_buf()
    } else {
        target_dir
    })
}

/// CronStoreMigration.ts:222-247.
fn stage_destination_data(
    input: &MigrationInput<'_>,
    snapshots: &[StoreSnapshot],
    final_tasks: &[(PathBuf, Vec<JsValue>)],
    final_runs: &[(PathBuf, RunBucket)],
) -> Result<(), CronStoreError> {
    for (dir, tasks) in final_tasks {
        let original = snapshots.iter().find(|snapshot| &snapshot.dir == dir);
        if original.is_some_and(|snapshot| !snapshot.task_file_writable) {
            continue;
        }
        let mut merged = Vec::new();
        if let Some(snapshot) = original {
            merged.extend(snapshot.tasks.iter().cloned());
        }
        merged.extend(tasks.iter().cloned());
        write_task_file(dir, &dedupe_tasks(&merged, input, dir)?, input.now_ms)?;
    }
    for (dir, bucket) in final_runs {
        let original = snapshots.iter().find(|snapshot| &snapshot.dir == dir);
        let mut merged = Vec::new();
        if let Some(snapshot) = original {
            merged.extend(snapshot.runs.iter().cloned());
        }
        merged.extend(bucket.records.iter().cloned());
        // `[...new Set([...original.invalidRunLines, ...bucket.invalidLines])]`
        // keeps first occurrences only.
        let mut invalid_lines = Vec::new();
        let mut invalid_seen = HashSet::new();
        for lines in [
            original.map(|snapshot| &snapshot.invalid_run_lines),
            Some(&bucket.invalid_lines),
        ]
        .into_iter()
        .flatten()
        {
            for line in lines {
                if invalid_seen.insert(line.clone()) {
                    invalid_lines.push(line.clone());
                }
            }
        }
        write_run_file(
            dir,
            &RunBucket {
                records: dedupe_runs(&merged)?,
                invalid_lines,
            },
            input.now_ms,
        )?;
    }
    Ok(())
}

/// CronStoreMigration.ts:249-263.
fn write_final_snapshots(
    input: &MigrationInput<'_>,
    snapshots: &[StoreSnapshot],
    final_tasks: &[(PathBuf, Vec<JsValue>)],
    final_runs: &[(PathBuf, RunBucket)],
) -> Result<(), CronStoreError> {
    for (dir, tasks) in final_tasks {
        let original = snapshots.iter().find(|snapshot| &snapshot.dir == dir);
        if original.is_some_and(|snapshot| !snapshot.task_file_writable) {
            continue;
        }
        write_task_file(dir, tasks, input.now_ms)?;
    }
    for (dir, bucket) in final_runs {
        write_run_file(dir, bucket, input.now_ms)?;
    }
    Ok(())
}

/// CronStoreMigration.ts:265-270.
fn write_task_file(dir: &Path, tasks: &[JsValue], now_ms: f64) -> Result<(), CronStoreError> {
    let mut file = JsObject::new();
    file.insert("schemaVersion".into(), JsValue::Number(1.0));
    file.insert("tasks".into(), JsValue::Array(tasks.to_vec()));
    atomic_write(
        &dir.join("tasks.json"),
        &format!("{}\n", stringify_pretty(&JsValue::Object(file))),
        now_ms,
    )
}

/// CronStoreMigration.ts:272-281.
fn write_run_file(dir: &Path, bucket: &RunBucket, now_ms: f64) -> Result<(), CronStoreError> {
    let mut lines = Vec::with_capacity(bucket.records.len() + bucket.invalid_lines.len());
    for record in &bucket.records {
        lines.push(stringify_compact(record)?);
    }
    lines.extend(bucket.invalid_lines.iter().cloned());
    let content = if lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", lines.join("\n"))
    };
    atomic_write(&dir.join("run-history.jsonl"), &content, now_ms)
}

/// CronStoreMigration.ts:283-322.
fn migrate_run_events(
    input: &MigrationInput<'_>,
    snapshots: &[StoreSnapshot],
    run_targets: &HashMap<JsString, HashSet<PathBuf>>,
) -> Result<(), CronStoreError> {
    for snapshot in snapshots {
        for run in &snapshot.runs {
            let Some(run_id) = run.get("runId").and_then(JsValue::as_string) else {
                continue;
            };
            let Some(targets) = run_targets.get(run_id) else {
                continue;
            };
            if targets.len() != 1 {
                continue;
            }
            let target_dir = targets.iter().next().expect("the only target directory");
            if target_dir == &snapshot.dir {
                continue;
            }
            let source_path = cron_run_events_path(&paths_for_dir(&snapshot.dir), run_id);
            let target_path = cron_run_events_path(&paths_for_dir(target_dir), run_id);
            let source_raw = match fs::read(&source_path) {
                Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            let target_raw = match fs::read(&target_path) {
                Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
                Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
                Err(error) => return Err(error.into()),
            };
            let merged = merge_json_lines(&target_raw, &source_raw);
            atomic_write(&target_path, &merged, input.now_ms)?;
            if let Err(error) = fs::remove_file(&source_path)
                && error.kind() != io::ErrorKind::NotFound
            {
                warn(
                    input.logger,
                    "cron migration could not remove source run event file",
                    &[
                        ("sourcePath", display_value(&source_path)),
                        ("error", JsValue::String(error.to_string().into())),
                    ],
                );
            }
        }
    }
    Ok(())
}

/// CronStoreMigration.ts:324-335.
fn paths_for_dir(project_dir: &Path) -> CronPaths {
    CronPaths {
        pilot_home: PathBuf::new(),
        project_key: String::new(),
        project_id: String::new(),
        root_dir: metadata::resolve(project_dir, "../.."),
        project_dir: project_dir.to_path_buf(),
        tasks_file: metadata::resolve(project_dir, "tasks.json"),
        runs_dir: metadata::resolve(project_dir, "runs"),
        run_history_file: metadata::resolve(project_dir, "run-history.jsonl"),
    }
}

/// CronStoreMigration.ts:337-361.
fn dedupe_tasks(
    tasks: &[JsValue],
    input: &MigrationInput<'_>,
    project_dir: &Path,
) -> Result<Vec<JsValue>, CronStoreError> {
    let collator = super::collator()?;
    let mut anonymous = Vec::new();
    let mut by_id: Vec<(JsString, JsValue)> = Vec::new();
    for task in tasks {
        let Some(record) = task.as_object() else {
            anonymous.push(task.clone());
            continue;
        };
        let Some(task_id) = record.get("taskId").and_then(JsValue::as_string) else {
            anonymous.push(task.clone());
            continue;
        };
        if let Some((_, current)) = by_id.iter_mut().find(|(id, _)| *id == *task_id) {
            let incoming = record.get("updatedAt");
            let incumbent = current
                .as_object()
                .and_then(|record| record.get("updatedAt"));
            // compareUpdatedAt(task, current): the incoming task wins ties.
            if compare_updated_at(&collator, incoming, incumbent)? != std::cmp::Ordering::Less {
                warn(
                    input.logger,
                    "cron migration resolved duplicate task",
                    &[
                        ("taskId", JsValue::String(task_id.clone())),
                        ("projectDir", display_value(project_dir)),
                    ],
                );
                *current = task.clone();
            }
        } else {
            by_id.push((task_id.clone(), task.clone()));
        }
    }
    Ok(by_id
        .into_iter()
        .map(|(_, value)| value)
        .chain(anonymous)
        .collect())
}

/// CronStoreMigration.ts:363-378.
fn dedupe_runs(runs: &[JsValue]) -> Result<Vec<JsValue>, CronStoreError> {
    let collator = super::collator()?;
    let mut anonymous = Vec::new();
    let mut by_id: Vec<(JsString, JsValue)> = Vec::new();
    for run in runs {
        let Some(run_id) = run.get("runId").and_then(JsValue::as_string) else {
            anonymous.push(run.clone());
            continue;
        };
        if let Some((_, current)) = by_id.iter_mut().find(|(id, _)| *id == *run_id) {
            if compare_run(&collator, run, current)? != std::cmp::Ordering::Less {
                *current = run.clone();
            }
        } else {
            by_id.push((run_id.clone(), run.clone()));
        }
    }
    let combined: Vec<JsValue> = by_id.into_iter().map(|(_, value)| value).collect();
    let mut keyed: Vec<(JsString, JsValue)> = Vec::with_capacity(combined.len() + anonymous.len());
    for value in combined.into_iter().chain(anonymous) {
        let key = string_or_empty(value.as_object().and_then(|record| record.get("startedAt")))?;
        keyed.push((key, value));
    }
    keyed.sort_by(|(left, _), (right, _)| node_compare_utf16(&collator, left, right));
    Ok(keyed.into_iter().map(|(_, value)| value).collect())
}

/// CronStoreMigration.ts:380-383.
fn compare_updated_at(
    collator: &CollatorBorrowed<'static>,
    left: Option<&JsValue>,
    right: Option<&JsValue>,
) -> Result<std::cmp::Ordering, CronStoreError> {
    let left = string_or_empty(left)?;
    let right = string_or_empty(right)?;
    Ok(node_compare_utf16(collator, &left, &right))
}

/// CronStoreMigration.ts:385-391.
fn compare_run(
    collator: &CollatorBorrowed<'static>,
    left: &JsValue,
    right: &JsValue,
) -> Result<std::cmp::Ordering, CronStoreError> {
    let terminal = |value: &JsValue| {
        value
            .as_object()
            .and_then(|record| record.get("finishedAt"))
            .and_then(JsValue::as_string)
            .is_some()
    };
    let (left_terminal, right_terminal) = (terminal(left), terminal(right));
    if left_terminal != right_terminal {
        return Ok(if left_terminal {
            std::cmp::Ordering::Greater
        } else {
            std::cmp::Ordering::Less
        });
    }
    let key = |value: &JsValue| {
        // `String(left.finishedAt ?? left.startedAt ?? "")` — the fallback only
        // applies when `finishedAt` is nullish.
        let record = value.as_object();
        let finished = record.and_then(|record| record.get("finishedAt"));
        match finished {
            Some(finished) if !finished.is_undefined() && !matches!(finished, JsValue::Null) => {
                string_or_empty(Some(finished))
            }
            _ => string_or_empty(record.and_then(|record| record.get("startedAt"))),
        }
    };
    let (left, right) = (key(left)?, key(right)?);
    Ok(node_compare_utf16(collator, &left, &right))
}

/// CronStoreMigration.ts:393-398.
fn merge_json_lines(left: &str, right: &str) -> String {
    let mut seen = HashSet::new();
    let mut lines = Vec::new();
    for line in format!("{left}\n{right}").split('\n') {
        if js_trim(line).is_empty() {
            continue;
        }
        if seen.insert(line.to_string()) {
            lines.push(line.to_string());
        }
    }
    if lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", lines.join("\n"))
    }
}

/// CronStoreMigration.ts:400-415. Unlike the store's writer there is no
/// `finally` unlink: a non-`EPERM`/`EACCES` rename failure leaves the temp
/// file behind, exactly like the TS control flow.
fn atomic_write(path: &Path, content: &str, now_ms: f64) -> Result<(), CronStoreError> {
    fs::create_dir_all(parent_of(path)?)?;
    // `${path}.${suffix}.tmp`: built on the raw `OsStr` so a non-UTF-8
    // directory keeps its bytes (`display()` would fold them to U+FFFD and
    // place the temp file in a directory that does not exist).
    let mut temp = path.as_os_str().to_os_string();
    temp.push(temp_suffix(now_ms));
    let temp = PathBuf::from(temp);
    fs::write(&temp, content)?;
    match fs::rename(&temp, path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            fs::copy(&temp, path)?;
            let _ = fs::remove_file(&temp);
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

/// CronStoreMigration.ts:417-449. The stale branch keeps the TS `stat` →
/// `rm` pair as-is: it is not atomic (two concurrent migrators that both
/// judged the same stale lock can end up both holding the next lock), which
/// is the baseline's own defect and stays ported, not fixed here.
fn acquire_migration_lock(
    input: &MigrationInput<'_>,
    root_dir: &Path,
) -> Result<PathBuf, CronStoreError> {
    fs::create_dir_all(root_dir)?;
    let lock_path = root_dir.join(".store-migration.lock");
    for _ in 0..LOCK_ATTEMPTS {
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
        {
            Ok(mut handle) => {
                let mut content = JsObject::new();
                content.insert("pid".into(), JsValue::Number(std::process::id() as f64));
                content.insert(
                    "startedAt".into(),
                    JsValue::String(iso_from_epoch_ms(input.now_ms).into()),
                );
                handle.write_all(
                    format!("{}\n", stringify_compact(&JsValue::Object(content))?).as_bytes(),
                )?;
                return Ok(lock_path);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let metadata = match fs::metadata(&lock_path) {
                    Ok(metadata) => metadata,
                    Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(error.into()),
                };
                let age = input.now_ms - mtime_ms(&metadata);
                if age > LOCK_STALE_MS {
                    warn(
                        input.logger,
                        "cron store migration removed a stale lock",
                        &[
                            ("lockPath", display_value(&lock_path)),
                            ("ageMs", JsValue::Number(age)),
                        ],
                    );
                    // `rm(lockPath, {force: true})` and `continue`: a second
                    // migrator that also judged this lock stale may already
                    // have removed it, and the ENOENT here is not a failure.
                    remove_lock(&lock_path)?;
                    continue;
                }
                thread::sleep(input.lock_retry_delay);
            }
            Err(error) => return Err(error.into()),
        }
    }
    Err(CronStoreError::LockTimeout(format!(
        "Timed out waiting for Cron store migration lock: {}",
        lock_path.display()
    )))
}

/// `rm(lockPath, {force: true})`: a vanished lock is success, both on the
/// stale-removal path (CronStoreMigration.ts:438) and in the `finally`
/// release (`:445-447`).
pub(super) fn remove_lock(lock_path: &Path) -> io::Result<()> {
    match fs::remove_file(lock_path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// CronStoreMigration.ts:451-453: `typeof value === "string" && value.trim()`
/// guards the one-argument `path.resolve`.
fn normalized_project_key(value: Option<&JsValue>) -> Result<Option<String>, CronStoreError> {
    let Some(JsValue::String(text)) = value else {
        return Ok(None);
    };
    // `resolve` keeps a lone surrogate in the JS string. The key only feeds
    // the ASCII `createProjectId` fold, where every non-allowed unit (the
    // surrogate or its U+FFFD replacement) collapses into the same `-`, so
    // lossy UTF-16 conversion preserves the routed project id.
    let text = String::from_utf16_lossy(text.units());
    if js_trim(&text).is_empty() {
        return Ok(None);
    }
    // Node's one-argument `resolve`: an absolute key never reads the process
    // cwd, a relative one resolves against it.
    let resolved = metadata::resolve_process_path(Path::new(""), Path::new(&text))?;
    Ok(Some(resolved.to_string_lossy().into_owned()))
}

fn warn(logger: Option<&dyn CronRuntimeLogger>, message: &str, fields: &[(&str, JsValue)]) {
    let Some(logger) = logger else { return };
    let mut meta = JsObject::new();
    for (key, value) in fields {
        meta.insert((*key).into(), value.clone());
    }
    logger.warn(message, &meta);
}

fn display_value(path: &Path) -> JsValue {
    JsValue::String(path.to_string_lossy().into_owned().into())
}

fn mtime_ms(metadata: &fs::Metadata) -> f64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|elapsed| elapsed.as_secs() as f64 * 1000.0 + f64::from(elapsed.subsec_nanos()) / 1e6)
        .unwrap_or(0.0)
}
