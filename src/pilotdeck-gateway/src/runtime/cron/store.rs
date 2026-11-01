//! Port of TS `src/cron/storage/CronTaskStore.ts` (250 lines; line refs
//! below). Synchronous where the TS class chains per-file promise tails
//! (`taskFileMutationTails`, CronTaskStore.ts:13,158-173): every read-modify-
//! write mutation holds this module's per-`tasksFile` mutex across the whole
//! read→write span, which is what the promise chain guarantees — including
//! across separate store instances, since the TS map is module-level.
use std::{
    collections::HashMap,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
    time::SystemTime,
};

use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json};

use super::paths::{CronPaths, cron_run_events_path};
use super::{CronStoreError, parent_of, stringify_compact, stringify_pretty, temp_suffix};
use crate::{js_value, metadata::js_trim, project_files::node_compare_utf16};

/// CronTaskStore.ts:8-11, minus the always-1 `schemaVersion` the write
/// functions re-attach.
struct TaskFile {
    tasks: Vec<JsValue>,
}

/// The `taskFileMutationTails` map: one tail per tasks file, process-wide.
static TASK_FILE_TAILS: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();

pub(super) fn task_file_tail(tasks_file: &Path) -> Arc<Mutex<()>> {
    let registry = TASK_FILE_TAILS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut tails = registry
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    tails.entry(tasks_file.to_path_buf()).or_default().clone()
}

/// CronTaskStore.ts:168-172: the finishing mutation drops the map entry when
/// its tail is still the map's value — no later mutation chained against it,
/// which here means no other thread holds a clone of the mutex. A waiting
/// thread keeps the entry (and its serialization) intact.
///
/// The decision takes ownership so it can drop this mutation's own clone
/// *under the registry lock*, before counting the rest: two finishing
/// mutations serialized here each re-read the count after their own drop, so
/// the last one out removes the entry. A check that counted first and
/// dropped later (the pre-fix-r3 shape) let both finishers observe the
/// other's still-live clone (`strong_count == 3`) and leave the sole
/// registry-owned reference behind with no one left to clean it up.
pub(super) fn release_task_file_tail(tasks_file: &Path, tail: Arc<Mutex<()>>) {
    let registry = TASK_FILE_TAILS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut tails = registry
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(current) = tails.get(tasks_file)
        && Arc::ptr_eq(current, &tail)
    {
        drop(tail);
        if Arc::strong_count(current) == 1 {
            tails.remove(tasks_file);
        }
    }
}

/// CronTaskStore.ts:157-173 `mutateTaskFile`: serialize the whole
/// read-modify-write span on the per-file tail, then drop the registry entry
/// when this mutation is still the last one.
fn mutate_task_file<T>(
    tasks_file: &Path,
    mutation: impl FnOnce() -> Result<T, CronStoreError>,
) -> Result<T, CronStoreError> {
    let tail = task_file_tail(tasks_file);
    let result = {
        let _guard = tail.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        mutation()
    };
    release_task_file_tail(tasks_file, tail);
    result
}

/// Whether the per-file tail registry currently holds `tasks_file` — only
/// the mutation-tail removal test observes this process-wide map.
#[cfg(test)]
pub(super) fn task_file_tail_registered(tasks_file: &Path) -> bool {
    TASK_FILE_TAILS.get().is_some_and(|registry| {
        registry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contains_key(tasks_file)
    })
}

pub struct CronTaskStore {
    paths: CronPaths,
}

impl CronTaskStore {
    pub fn new(paths: CronPaths) -> Self {
        Self { paths }
    }

    pub fn paths(&self) -> &CronPaths {
        &self.paths
    }

    /// CronTaskStore.ts:18-20.
    pub fn list_tasks(&self) -> Result<Vec<JsValue>, CronStoreError> {
        Ok(self.read_task_file()?.tasks)
    }

    /// CronTaskStore.ts:22-24.
    pub fn get_task(&self, task_id: &str) -> Result<Option<JsValue>, CronStoreError> {
        let wanted = JsValue::String(JsString::from(task_id));
        Ok(self
            .list_tasks()?
            .into_iter()
            .find(|task| super::strict_equals(task.get("taskId"), Some(&wanted))))
    }

    /// CronTaskStore.ts:26-37.
    pub fn put_task(&self, task: &JsValue) -> Result<(), CronStoreError> {
        mutate_task_file(&self.paths.tasks_file, || {
            let mut file = self.read_task_file()?;
            let wanted = task.get("taskId");
            match file
                .tasks
                .iter()
                .position(|entry| super::strict_equals(entry.get("taskId"), wanted))
            {
                Some(index) => file.tasks[index] = task.clone(),
                None => file.tasks.push(task.clone()),
            }
            sort_tasks(&mut file.tasks)?;
            self.write_task_file(&file)
        })
    }

    /// CronTaskStore.ts:39-50.
    pub fn replace_task(&self, task: &JsValue) -> Result<bool, CronStoreError> {
        mutate_task_file(&self.paths.tasks_file, || {
            let mut file = self.read_task_file()?;
            let wanted = task.get("taskId");
            let Some(index) = file
                .tasks
                .iter()
                .position(|entry| super::strict_equals(entry.get("taskId"), wanted))
            else {
                return Ok(false);
            };
            file.tasks[index] = task.clone();
            sort_tasks(&mut file.tasks)?;
            self.write_task_file(&file)?;
            Ok(true)
        })
    }

    /// CronTaskStore.ts:52-65. `update` runs for every task whose `taskId`
    /// matches; a `None` return drops that task from the file, and the last
    /// invocation's value is what the method returns.
    pub fn update_task(
        &self,
        task_id: &str,
        update: impl Fn(&JsValue) -> Option<JsValue>,
    ) -> Result<Option<JsValue>, CronStoreError> {
        mutate_task_file(&self.paths.tasks_file, || {
            let wanted = JsValue::String(JsString::from(task_id));
            let file = self.read_task_file()?;
            let mut tasks = Vec::with_capacity(file.tasks.len());
            let mut updated: Option<JsValue> = None;
            for task in file.tasks {
                if !super::strict_equals(task.get("taskId"), Some(&wanted)) {
                    tasks.push(task);
                    continue;
                }
                match update(&task) {
                    Some(value) => {
                        updated = Some(value.clone());
                        tasks.push(value);
                    }
                    None => updated = None,
                }
            }
            sort_tasks(&mut tasks)?;
            self.write_task_file(&TaskFile { tasks })?;
            Ok(updated)
        })
    }

    /// CronTaskStore.ts:67-76.
    pub fn delete_task(&self, task_id: &str) -> Result<bool, CronStoreError> {
        mutate_task_file(&self.paths.tasks_file, || {
            let wanted = JsValue::String(JsString::from(task_id));
            let file = self.read_task_file()?;
            let before = file.tasks.len();
            let tasks: Vec<JsValue> = file
                .tasks
                .into_iter()
                .filter(|task| !super::strict_equals(task.get("taskId"), Some(&wanted)))
                .collect();
            if tasks.len() == before {
                return Ok(false);
            }
            self.write_task_file(&TaskFile { tasks })?;
            Ok(true)
        })
    }

    /// CronTaskStore.ts:78-81.
    pub fn append_run(&self, record: &JsValue) -> Result<(), CronStoreError> {
        append_line(&self.paths.run_history_file, &stringify_compact(record)?)
    }

    /// CronTaskStore.ts:83-105. Node's `readFile(path, "utf-8")` replaces
    /// invalid sequences, so reads are lossy, and invalid JSON lines drop.
    pub fn list_runs(&self, limit: Option<i64>) -> Result<Vec<JsValue>, CronStoreError> {
        let bytes = match fs::read(&self.paths.run_history_file) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error.into()),
        };
        let raw = String::from_utf8_lossy(&bytes);
        let mut records = Vec::new();
        for line in raw.split('\n') {
            if js_trim(line).is_empty() {
                continue;
            }
            if let Ok(parsed) = parse_json(line)
                && let Some(record) = normalize_run(&parsed)
            {
                records.push(record);
            }
        }
        // CronTaskStore.ts:104: `slice(-Math.max(0, limit)).reverse()`. In JS
        // `-0 === 0`, so a limit clamped to 0 slices from index 0 and keeps
        // every record.
        let limit = limit.unwrap_or(50).max(0) as usize;
        let skip = if limit == 0 {
            0
        } else {
            records.len().saturating_sub(limit)
        };
        records.drain(..skip);
        records.reverse();
        Ok(records)
    }

    /// CronTaskStore.ts:107-111.
    pub fn append_run_event(
        &self,
        run_id: &JsString,
        event: &JsValue,
    ) -> Result<(), CronStoreError> {
        let path = cron_run_events_path(&self.paths, run_id);
        let mut wrapper = JsObject::new();
        wrapper.insert("schemaVersion".into(), JsValue::Number(1.0));
        wrapper.insert("runId".into(), JsValue::String(run_id.clone()));
        wrapper.insert("event".into(), event.clone());
        append_line(&path, &stringify_compact(&JsValue::Object(wrapper))?)
    }

    /// CronTaskStore.ts:113-136. `readFile` is lossy, `JSON.parse` failures
    /// (including a `null` document, whose property read throws) fall back to
    /// the empty task file.
    fn read_task_file(&self) -> Result<TaskFile, CronStoreError> {
        let bytes = match fs::read(&self.paths.tasks_file) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(TaskFile { tasks: Vec::new() });
            }
            Err(error) => return Err(error.into()),
        };
        Ok(TaskFile {
            tasks: parse_task_file(&String::from_utf8_lossy(&bytes)),
        })
    }

    /// CronTaskStore.ts:138-156: temp → rename, `EPERM`/`EACCES` (both map to
    /// `PermissionDenied`) fall back to a copy, and a `finally` unlink cleans
    /// the temp on every exit path.
    fn write_task_file(&self, file: &TaskFile) -> Result<(), CronStoreError> {
        let mut document = JsObject::new();
        document.insert("schemaVersion".into(), JsValue::Number(1.0));
        document.insert("tasks".into(), JsValue::Array(file.tasks.clone()));
        write_task_file_atomic(
            &self.paths.tasks_file,
            &stringify_pretty(&JsValue::Object(document)),
        )
    }
}

fn append_line(path: &std::path::Path, line: &str) -> Result<(), CronStoreError> {
    fs::create_dir_all(parent_of(path)?)?;
    let mut handle = fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(path)?;
    handle.write_all(format!("{line}\n").as_bytes())?;
    Ok(())
}

fn write_task_file_atomic(path: &Path, payload: &str) -> Result<(), CronStoreError> {
    let now_ms = now_millis();
    fs::create_dir_all(parent_of(path)?)?;
    // `${tasksFile}.${suffix}.tmp` on the raw `OsStr`: a non-UTF-8 directory
    // keeps its bytes instead of folding them to U+FFFD.
    let mut temp = path.as_os_str().to_os_string();
    temp.push(temp_suffix(now_ms));
    let temp = PathBuf::from(temp);
    let result = (|| -> Result<(), CronStoreError> {
        fs::write(&temp, payload)?;
        match fs::rename(&temp, path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
                fs::copy(&temp, path)?;
            }
            Err(error) => return Err(error.into()),
        }
        Ok(())
    })();
    let _ = fs::remove_file(&temp);
    result
}

pub(crate) fn now_millis() -> f64 {
    SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as f64 * 1000.0 + f64::from(elapsed.subsec_millis()))
        .unwrap_or(0.0)
}

/// CronTaskStore.ts:124-136 (the parse half of `readTaskFile`).
fn parse_task_file(raw: &str) -> Vec<JsValue> {
    let Ok(parsed) = parse_json(raw) else {
        return Vec::new();
    };
    let Some(object) = parsed.as_object() else {
        return Vec::new();
    };
    if object.get("schemaVersion").and_then(JsValue::as_number) != Some(1.0) {
        return Vec::new();
    }
    let Some(JsValue::Array(tasks)) = object.get("tasks") else {
        return Vec::new();
    };
    tasks.iter().filter_map(normalize_task).collect()
}

/// CronTaskStore.ts:176-178. Stable sort by `nextRunAt ?? ""` under
/// `localeCompare`. V8 throws the `TypeError` only when the comparator is
/// actually invoked with a non-string first operand — a single-element array
/// never calls it — while this port rejects any non-string, non-nullish
/// `nextRunAt` up front, so it is stricter on that invalid shape.
fn sort_tasks(tasks: &mut [JsValue]) -> Result<(), CronStoreError> {
    let collator = super::collator()?;
    for task in tasks.iter() {
        if let Some(value) = task.get("nextRunAt")
            && !matches!(
                value,
                JsValue::String(_) | JsValue::Undefined | JsValue::Null
            )
        {
            let shown = js_value::to_string_units(value).unwrap_or_default();
            return Err(CronStoreError::Type(format!(
                "{}.localeCompare is not a function",
                String::from_utf16_lossy(&shown)
            )));
        }
    }
    let key = |task: &JsValue| match task.get("nextRunAt") {
        Some(JsValue::String(text)) => text.clone(),
        _ => JsString::from(""),
    };
    tasks.sort_by(|left, right| node_compare_utf16(&collator, &key(left), &key(right)));
    Ok(())
}

/// CronTaskStore.ts:180-225. The output literal fixes the key order; a missing
/// optional is exactly a `JSON.stringify`-invisible `undefined` slot.
pub(crate) fn normalize_task(value: &JsValue) -> Option<JsValue> {
    let candidate = value.as_object()?;
    if candidate.get("schemaVersion").and_then(JsValue::as_number) != Some(1.0) {
        return None;
    }
    for field in [
        "taskId",
        "message",
        "sessionKey",
        "channelKey",
        "createdAt",
        "updatedAt",
    ] {
        candidate.get(field).and_then(JsValue::as_string)?;
    }
    let schedule = candidate.get("schedule")?;
    if !super::truthy(schedule) {
        return None;
    }
    // `schedule.type === "once"` / `=== "cron"` (CronTaskStore.ts:198,201).
    let schedule_kind = |name: &str| {
        schedule
            .get("type")
            .and_then(JsValue::as_string)
            .is_some_and(|kind| kind.equals_str(name))
    };
    if schedule_kind("once") && schedule.get("runAt").and_then(JsValue::as_string).is_none() {
        return None;
    }
    if schedule_kind("cron")
        && schedule
            .get("expression")
            .and_then(JsValue::as_string)
            .is_none()
    {
        return None;
    }
    let string_or_drop = |field: &str| {
        candidate
            .get(field)
            .and_then(JsValue::as_string)
            .map(|text| JsValue::String(text.clone()))
    };
    let pass_through = |field: &str| {
        candidate
            .get(field)
            .filter(|value| !value.is_undefined())
            .cloned()
    };
    let revision = match candidate.get("revision").and_then(JsValue::as_number) {
        Some(number)
            if number.is_finite()
                && number.fract() == 0.0
                && number.abs() <= 9_007_199_254_740_991.0
                && number >= 0.0 =>
        {
            JsValue::Number(number)
        }
        _ => JsValue::Number(0.0),
    };
    let computation = match candidate
        .get("scheduleComputationVersion")
        .and_then(JsValue::as_number)
    {
        // `2` is the pre-#1324 store schema, kept accepted on purpose; the
        // current version is the ported `CRON_SCHEDULE_COMPUTATION_VERSION`
        // (CronSchedule.ts:4, pinned against the oracle).
        Some(number)
            if number == 2.0 || number == f64::from(super::CRON_SCHEDULE_COMPUTATION_VERSION) =>
        {
            JsValue::Number(number)
        }
        _ => JsValue::Undefined,
    };
    let mut out = JsObject::new();
    out.insert("schemaVersion".into(), JsValue::Number(1.0));
    out.insert("taskId".into(), candidate.get("taskId")?.clone());
    out.insert("message".into(), candidate.get("message")?.clone());
    out.insert("schedule".into(), schedule.clone());
    out.insert(
        "status".into(),
        if candidate
            .get("status")
            .and_then(JsValue::as_string)
            .is_some_and(|status| status.equals_str("running"))
        {
            "running".into()
        } else {
            "scheduled".into()
        },
    );
    out.insert("sessionKey".into(), candidate.get("sessionKey")?.clone());
    out.insert("channelKey".into(), candidate.get("channelKey")?.clone());
    if let Some(value) = string_or_drop("projectKey") {
        out.insert("projectKey".into(), value);
    }
    if let Some(value) = pass_through("mode") {
        out.insert("mode".into(), value);
    }
    if let Some(value) = string_or_drop("timezone") {
        out.insert("timezone".into(), value);
    }
    out.insert("createdAt".into(), candidate.get("createdAt")?.clone());
    out.insert("updatedAt".into(), candidate.get("updatedAt")?.clone());
    if let Some(value) = string_or_drop("nextRunAt") {
        out.insert("nextRunAt".into(), value);
    }
    if let Some(value) = string_or_drop("lastRunId") {
        out.insert("lastRunId".into(), value);
    }
    out.insert("revision".into(), revision);
    if !computation.is_undefined() {
        out.insert("scheduleComputationVersion".into(), computation);
    }
    if let Some(value) = string_or_drop("originSessionKey") {
        out.insert("originSessionKey".into(), value);
    }
    if let Some(value) = string_or_drop("originChannelKey") {
        out.insert("originChannelKey".into(), value);
    }
    Some(JsValue::Object(out))
}

/// CronTaskStore.ts:227-250.
pub(crate) fn normalize_run(value: &JsValue) -> Option<JsValue> {
    let candidate = value.as_object()?;
    if candidate.get("schemaVersion").and_then(JsValue::as_number) != Some(1.0) {
        return None;
    }
    for field in ["runId", "taskId", "sessionKey", "startedAt"] {
        candidate.get(field).and_then(JsValue::as_string)?;
    }
    let string_or_drop = |field: &str| {
        candidate
            .get(field)
            .and_then(JsValue::as_string)
            .map(|text| JsValue::String(text.clone()))
    };
    let pass_through = |field: &str| {
        candidate
            .get(field)
            .filter(|value| !value.is_undefined())
            .cloned()
    };
    let mut out = JsObject::new();
    out.insert("schemaVersion".into(), JsValue::Number(1.0));
    out.insert("runId".into(), candidate.get("runId")?.clone());
    out.insert("taskId".into(), candidate.get("taskId")?.clone());
    out.insert("sessionKey".into(), candidate.get("sessionKey")?.clone());
    if let Some(value) = string_or_drop("projectKey") {
        out.insert("projectKey".into(), value);
    }
    out.insert("startedAt".into(), candidate.get("startedAt")?.clone());
    if let Some(value) = string_or_drop("finishedAt") {
        out.insert("finishedAt".into(), value);
    }
    if let Some(value) = pass_through("outcome") {
        out.insert("outcome".into(), value);
    }
    if let Some(value) = pass_through("error") {
        out.insert("error".into(), value);
    }
    Some(JsValue::Object(out))
}
