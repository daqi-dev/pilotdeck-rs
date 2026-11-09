use super::{
    CronError, CronResult, absolute, bump_revision, revision_of, set_computation_version, text,
};
use crate::cron::schedule;
use serde_json::{Value, json};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct CronStore {
    pub project_key: String,
    pub project_dir: PathBuf,
    pub(super) home: PathBuf,
}
impl CronStore {
    pub fn new(home: &Path, project: &str) -> CronResult<Self> {
        let home = absolute(home)?;
        let project = absolute(Path::new(project))?;
        Ok(Self {
            project_key: project.to_string_lossy().into_owned(),
            project_dir: home
                .join("cron/projects")
                .join(crate::metadata::project_id(&project)),
            home,
        })
    }
    fn locked<T>(&self, action: impl FnOnce() -> CronResult<T>) -> CronResult<T> {
        let _migration = global_lock(&self.home, true)?;
        let _project = exclusive_lock(&self.project_dir.join(".tasks.lock"))?;
        action()
    }
    pub(super) fn record_project(&self) -> CronResult<()> {
        self.locked(|| {
            if let Some(marker) = read_optional(&self.project_dir.join(".cwd"))?
                && !marker.trim().is_empty()
                && absolute(Path::new(marker.trim()))?.to_string_lossy() != self.project_key
            {
                return Err(CronError::new("cron_project_collision", "Cron project storage id is already owned by another project; original data preserved"));
            }
            atomic_write(
                &self.project_dir.join(".cwd"),
                format!("{}\n", self.project_key).as_bytes(),
            )
        })
    }
    pub fn list_tasks(&self) -> CronResult<Vec<Value>> {
        self.locked(|| read_tasks(&self.project_dir.join("tasks.json")))
    }
    pub fn get_task(&self, id: &str) -> CronResult<Option<Value>> {
        Ok(self
            .list_tasks()?
            .into_iter()
            .find(|task| text(task, "taskId") == id))
    }
    pub fn put_task(&self, task: &Value) -> CronResult<()> {
        let normalized = normalize_task(task)?;
        self.locked(|| {
            let mut tasks = read_tasks(&self.project_dir.join("tasks.json"))?;
            if let Some(old) = tasks
                .iter_mut()
                .find(|old| old["taskId"] == normalized["taskId"])
            {
                *old = normalized;
            } else {
                tasks.push(normalized);
            }
            write_tasks(&self.project_dir, &mut tasks)
        })
    }
    pub fn update_task<F>(&self, id: &str, update: F) -> CronResult<Option<Value>>
    where
        F: FnOnce(&Value) -> CronResult<Option<Value>>,
    {
        self.locked(|| {
            let mut tasks = read_tasks(&self.project_dir.join("tasks.json"))?;
            let Some(index) = tasks.iter().position(|task| text(task, "taskId") == id) else {
                return Ok(None);
            };
            let updated = update(&tasks[index])?;
            if let Some(task) = &updated {
                tasks[index] = normalize_task(task)?;
            } else {
                tasks.remove(index);
            }
            write_tasks(&self.project_dir, &mut tasks)?;
            Ok(updated)
        })
    }
    pub fn delete_task(&self, id: &str) -> CronResult<bool> {
        self.locked(|| {
            let mut tasks = read_tasks(&self.project_dir.join("tasks.json"))?;
            let len = tasks.len();
            tasks.retain(|task| text(task, "taskId") != id);
            if tasks.len() == len {
                return Ok(false);
            }
            write_tasks(&self.project_dir, &mut tasks)?;
            Ok(true)
        })
    }
    pub fn append_run(&self, record: &Value) -> CronResult<()> {
        validate_run(record)?;
        self.locked(|| {
            let history = self.project_dir.join("run-history.jsonl");
            read_runs(&history)?;
            append_json(&history, record)
        })
    }
    pub fn list_runs(&self, limit: Option<f64>) -> CronResult<Vec<Value>> {
        self.locked(|| {
            let mut records = read_runs(&self.project_dir.join("run-history.jsonl"))?;
            let limit = limit.unwrap_or(50.0);
            // JavaScript slice(-0) keeps all rows, including limit <= 0 / NaN.
            if limit > 0.0 {
                let count = limit.trunc().min(records.len() as f64) as usize;
                if count > 0 {
                    records = records.split_off(records.len() - count);
                }
            }
            records.reverse();
            Ok(records)
        })
    }
    pub fn append_run_event(&self, run_id: &str, event: &Value) -> CronResult<()> {
        self.locked(|| {
            append_json(
                &self
                    .project_dir
                    .join("runs")
                    .join(format!("{}.events.jsonl", safe_id(run_id))),
                &json!({"schemaVersion":1,"runId":run_id,"event":event}),
            )
        })
    }
    pub fn due(&self, now_ms: i64) -> CronResult<Vec<Value>> {
        let mut due = Vec::new();
        for task in self.list_tasks()? {
            if task["status"] != "running"
                && let Some(next) = task.get("nextRunAt").and_then(Value::as_str)
                && schedule::parse_date(next)?.is_some_and(|at| at <= now_ms)
            {
                due.push(task);
            }
        }
        Ok(due)
    }
    pub fn claim(&self, snapshot: &Value, run_id: &str, now_ms: i64) -> CronResult<Option<Value>> {
        let mut claimed = false;
        let result = self.update_task(text(snapshot, "taskId"), |current| {
            if current["status"] != "scheduled" || !matches_snapshot(current, snapshot) {
                return Ok(Some(current.clone()));
            }
            let mut task = current.clone();
            task["status"] = json!("running");
            task["lastRunId"] = json!(run_id);
            task["updatedAt"] = json!(schedule::iso(now_ms));
            bump_revision(&mut task)?;
            claimed = true;
            Ok(Some(task))
        })?;
        Ok(if claimed { result } else { None })
    }
    pub fn delay(&self, snapshot: &Value, now_ms: i64) -> CronResult<bool> {
        let mut delayed = false;
        self.update_task(text(snapshot, "taskId"), |current| {
            if current["status"] != "scheduled" || !matches_snapshot(current, snapshot) {
                return Ok(Some(current.clone()));
            }
            let mut task = current.clone();
            task["nextRunAt"] = json!(schedule::iso(now_ms + 60_000));
            task["updatedAt"] = json!(schedule::iso(now_ms));
            bump_revision(&mut task)?;
            delayed = true;
            Ok(Some(task))
        })?;
        Ok(delayed)
    }
    pub fn finalize(
        &self,
        claimed: &Value,
        run_id: &str,
        finished_ms: i64,
        default_timezone: &str,
    ) -> CronResult<bool> {
        let mut changed = false;
        self.update_task(text(claimed, "taskId"), |current| {
            if current["status"] != "running"
                || text(current, "lastRunId") != run_id
                || revision_of(current) != revision_of(claimed)
            {
                return Ok(Some(current.clone()));
            }
            changed = true;
            if claimed["schedule"]["type"] == "once" {
                return Ok(None);
            }
            let mut task = current.clone();
            reschedule(&mut task, finished_ms, default_timezone)?;
            Ok(Some(task))
        })?;
        Ok(changed)
    }
    /// Record interruptions and reset tasks under the same project transaction.
    pub fn recover(&self, now_ms: i64, default_timezone: &str) -> CronResult<Vec<Value>> {
        self.locked(|| {
            let mut tasks=read_tasks(&self.project_dir.join("tasks.json"))?;
            let history=self.project_dir.join("run-history.jsonl");
            let terminals:std::collections::HashSet<String>=read_runs(&history)?.into_iter()
                .filter(|run|run.get("finishedAt").is_some_and(super::truthy) && run.get("outcome").is_some_and(super::truthy))
                .map(|run|text(&run,"runId").to_owned()).collect();
            let mut recovered=Vec::new();
            let mut result=Vec::new();
            for mut task in tasks.drain(..) {
                if task["status"]!="running" { result.push(task); continue; }
                recovered.push(task.clone());
                let run=text(&task,"lastRunId");
                if !run.is_empty() && !terminals.contains(run) {
                    let started=if schedule::parse_date(text(&task,"updatedAt"))?.is_some(){text(&task,"updatedAt").to_owned()}else{schedule::iso(now_ms)};
                    let mut record=json!({"schemaVersion":1,"runId":run,"taskId":task["taskId"],"sessionKey":task["sessionKey"],"startedAt":started,
                        "finishedAt":schedule::iso(now_ms),"outcome":"failed","error":{"code":"cron_run_interrupted","message":"Cron run was interrupted before the runtime restarted."}});
                    if let Some(project)=task.get("projectKey"){record["projectKey"]=project.clone();}
                    append_json(&history,&record)?;
                }
                if task["schedule"]["type"]!="once" {reschedule(&mut task,now_ms,default_timezone)?;result.push(task);}
            }
            if !recovered.is_empty() {write_tasks(&self.project_dir,&mut result)?;}
            Ok(recovered)
        })
    }
    pub fn migrate_sessions(&self, now_ms: i64) -> CronResult<Vec<Value>> {
        self.locked(|| {
            let mut tasks = read_tasks(&self.project_dir.join("tasks.json"))?;
            let mut old = Vec::new();
            for task in &mut tasks {
                let session = format!("cron:{}", text(task, "taskId"));
                if text(task, "sessionKey") == session && task["channelKey"] == "cron" {
                    continue;
                }
                old.push(task.clone());
                task["sessionKey"] = json!(session);
                task["channelKey"] = json!("cron");
                bump_revision(task)?;
                task["updatedAt"] = json!(schedule::iso(now_ms));
            }
            if !old.is_empty() {
                write_tasks(&self.project_dir, &mut tasks)?;
            }
            Ok(old)
        })
    }
    pub fn recalculate(&self, now_ms: i64, default_timezone: &str) -> CronResult<()> {
        for snapshot in self.list_tasks()? {
            if snapshot["schedule"]["type"] == "once" {
                if snapshot.get("nextRunAt").is_some_and(super::truthy) {
                    continue;
                }
                let next =
                    schedule::compute_next_run_at(&snapshot["schedule"], now_ms, default_timezone)?;
                self.update_task(text(&snapshot, "taskId"), |current| {
                    if !matches_snapshot(current, &snapshot) {
                        return Ok(Some(current.clone()));
                    }
                    let Some(next) = next else {
                        return Ok(None);
                    };
                    let mut task = current.clone();
                    task["nextRunAt"] = json!(schedule::iso(next));
                    task["updatedAt"] = json!(schedule::iso(now_ms));
                    bump_revision(&mut task)?;
                    Ok(Some(task))
                })?;
                continue;
            }
            if snapshot["scheduleComputationVersion"] == 3
                && snapshot.get("nextRunAt").is_some_and(super::truthy)
            {
                continue;
            }
            let timezone = schedule::resolve_timezone(
                snapshot["schedule"]["timezone"].as_str(),
                snapshot["timezone"].as_str(),
                Some(default_timezone),
            );
            let mut normalized = snapshot["schedule"].clone();
            normalized["timezone"] = json!(timezone);
            let cached = if snapshot["scheduleComputationVersion"] == 2 {
                snapshot
                    .get("nextRunAt")
                    .and_then(Value::as_str)
                    .map(schedule::parse_date)
                    .transpose()?
                    .flatten()
            } else {
                None
            };
            let reuse = cached.is_some_and(|cached| {
                cached <= now_ms
                    || !schedule::cron_day_fields_use_or(text(&normalized, "expression"))
            });
            let computed = if reuse {
                None
            } else {
                schedule::compute_next_run_at(&normalized, now_ms, &timezone)?
            };
            let next = match (cached, computed) {
                (Some(cached), Some(computed)) => Some(cached.min(computed)),
                (Some(cached), None) => Some(cached),
                (None, next) => next,
            };
            self.update_task(text(&snapshot, "taskId"), |current| {
                if !matches_snapshot(current, &snapshot) {
                    return Ok(Some(current.clone()));
                }
                let mut task = current.clone();
                task["schedule"] = normalized;
                task["timezone"] = json!(timezone);
                task["status"] = json!("scheduled");
                set_next(&mut task, next);
                task["updatedAt"] = json!(schedule::iso(now_ms));
                bump_revision(&mut task)?;
                set_computation_version(&mut task);
                Ok(Some(task))
            })?;
        }
        Ok(())
    }
}

pub(super) fn matches_snapshot(current: &Value, snapshot: &Value) -> bool {
    current["status"] == snapshot["status"]
        && revision_of(current) == revision_of(snapshot)
        && current.get("nextRunAt") == snapshot.get("nextRunAt")
        && current.get("lastRunId") == snapshot.get("lastRunId")
}
fn reschedule(task: &mut Value, now_ms: i64, default_timezone: &str) -> CronResult<()> {
    let timezone = schedule::resolve_timezone(
        task["schedule"]["timezone"].as_str(),
        task["timezone"].as_str(),
        Some(default_timezone),
    );
    task["schedule"]["timezone"] = json!(timezone);
    task["timezone"] = json!(timezone);
    task["status"] = json!("scheduled");
    let next = schedule::compute_next_run_at(&task["schedule"], now_ms, &timezone)?;
    set_next(task, next);
    task["updatedAt"] = json!(schedule::iso(now_ms));
    bump_revision(task)?;
    set_computation_version(task);
    Ok(())
}
fn set_next(task: &mut Value, next: Option<i64>) {
    if let Some(next) = next {
        task["nextRunAt"] = json!(schedule::iso(next));
    } else if let Some(object) = task.as_object_mut() {
        object.remove("nextRunAt");
    }
}

pub(super) fn read_tasks(path: &Path) -> CronResult<Vec<Value>> {
    let Some(raw) = read_optional(path)? else {
        return Ok(Vec::new());
    };
    let value: Value =
        serde_json::from_str(&raw).map_err(|error| corrupt(path, error.to_string()))?;
    if value["schemaVersion"] != 1 || !value["tasks"].is_array() {
        return Err(corrupt(path, "Invalid task store schema"));
    }
    value["tasks"]
        .as_array()
        .expect("checked array")
        .iter()
        .map(normalize_task)
        .collect()
}
fn normalize_task(task: &Value) -> CronResult<Value> {
    let invalid = || {
        CronError::new(
            "cron_store_corrupt",
            "Invalid Cron task record; original data preserved",
        )
    };
    if task["schemaVersion"] != 1 || !task.is_object() {
        return Err(invalid());
    }
    for key in [
        "taskId",
        "message",
        "sessionKey",
        "channelKey",
        "createdAt",
        "updatedAt",
    ] {
        if !task[key].is_string() {
            return Err(invalid());
        }
    }
    match task["schedule"]["type"].as_str() {
        Some("once") if task["schedule"]["runAt"].is_string() => {}
        Some("cron") if task["schedule"]["expression"].is_string() => {}
        _ => return Err(invalid()),
    }
    let mut result = json!({"schemaVersion":1,"taskId":task["taskId"],"message":task["message"],"schedule":task["schedule"],
        "sessionKey":task["sessionKey"],"channelKey":task["channelKey"],"createdAt":task["createdAt"],"updatedAt":task["updatedAt"],
        "status":if task["status"]=="running"{"running"}else{"scheduled"},"revision":revision_of(task)});
    for key in [
        "projectKey",
        "timezone",
        "nextRunAt",
        "lastRunId",
        "originSessionKey",
        "originChannelKey",
    ] {
        if task[key].is_string() {
            result[key] = task[key].clone();
        }
    }
    if let Some(mode) = task.get("mode") {
        result["mode"] = mode.clone();
    }
    if task["scheduleComputationVersion"] == 2 || task["scheduleComputationVersion"] == 3 {
        result["scheduleComputationVersion"] = task["scheduleComputationVersion"].clone();
    }
    Ok(result)
}
pub(super) fn write_tasks(dir: &Path, tasks: &mut [Value]) -> CronResult<()> {
    tasks.sort_by(|a, b| text(a, "nextRunAt").cmp(text(b, "nextRunAt")));
    atomic_write(
        &dir.join("tasks.json"),
        serde_json::to_vec_pretty(&json!({"schemaVersion":1,"tasks":tasks}))?.as_slice(),
    )
}
pub(super) fn read_optional(path: &Path) -> CronResult<Option<String>> {
    match fs::read_to_string(path) {
        Ok(raw) => Ok(Some(raw)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}
pub(super) fn read_runs(path: &Path) -> CronResult<Vec<Value>> {
    let Some(raw) = read_optional(path)? else {
        return Ok(Vec::new());
    };
    raw.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let record: Value =
                serde_json::from_str(line).map_err(|error| corrupt(path, error.to_string()))?;
            validate_run(&record)?;
            Ok(record)
        })
        .collect()
}
fn validate_run(record: &Value) -> CronResult<()> {
    if record["schemaVersion"] != 1
        || ["runId", "taskId", "sessionKey", "startedAt"]
            .iter()
            .any(|key| !record[*key].is_string())
    {
        return Err(CronError::new(
            "cron_store_corrupt",
            "Invalid Cron run record; original data preserved",
        ));
    }
    Ok(())
}
pub(super) fn append_json(path: &Path, value: &Value) -> CronResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(&bytes)?;
    file.sync_data()?;
    Ok(())
}
pub(super) fn atomic_write(path: &Path, bytes: &[u8]) -> CronResult<()> {
    let parent = path
        .parent()
        .ok_or_else(|| CronError::new("cron_store_io", "Invalid store path"))?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(".cron-{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> CronResult<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)?;
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}
pub(super) fn exclusive_lock(path: &Path) -> CronResult<File> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    file.lock()?;
    Ok(file)
}
pub(super) fn global_lock(home: &Path, shared: bool) -> CronResult<File> {
    let path = home.join("cron/.store-migration.lock");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    if shared {
        file.lock_shared()?;
    } else {
        file.lock()?;
    }
    Ok(file)
}
pub(super) fn safe_id(value: &str) -> String {
    let mut output = String::new();
    let mut replacing = false;
    for c in value.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
            output.push(c);
            replacing = false;
        } else if !replacing {
            output.push('-');
            replacing = true;
        }
    }
    let result = output.trim_matches('-');
    if result.is_empty() {
        "unnamed".into()
    } else {
        result.into()
    }
}
pub(super) fn corrupt(path: &Path, message: impl std::fmt::Display) -> CronError {
    CronError::new(
        "cron_store_corrupt",
        format!("{}: {message}; original data preserved", path.display()),
    )
}
