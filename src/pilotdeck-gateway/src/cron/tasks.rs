//! Cron control port: one durable project store shared by RPCs and the scheduler.
mod migration;
mod store;
use super::schedule::{self, display, truthy, validation};
use serde_json::{Value, json};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
pub use store::CronStore;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CronError {
    pub code: String,
    pub message: String,
}
impl CronError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
    pub fn value(&self) -> Value {
        json!({"code":self.code,"message":self.message})
    }
}
impl std::fmt::Display for CronError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for CronError {}
impl From<std::io::Error> for CronError {
    fn from(error: std::io::Error) -> Self {
        Self::new("cron_store_io", error.to_string())
    }
}
impl From<serde_json::Error> for CronError {
    fn from(error: serde_json::Error) -> Self {
        Self::new("cron_store_corrupt", error.to_string())
    }
}
pub type CronResult<T> = Result<T, CronError>;
pub type TaskFuture<'a, T> = Pin<Box<dyn Future<Output = CronResult<T>> + Send + 'a>>;

#[derive(Clone, Debug)]
pub struct CronConfig {
    pub enabled: bool,
    pub timezone: String,
    pub max_concurrent_runs: usize,
    pub run_timeout_minutes: u64,
}
impl Default for CronConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            timezone: "UTC".into(),
            max_concurrent_runs: 1,
            run_timeout_minutes: 60,
        }
    }
}
impl CronConfig {
    /// Configuration diagnostics belong to the existing config parser; this accepts its subsection.
    pub fn from_value(value: &Value) -> CronResult<Self> {
        if !value.is_object() {
            return Err(validation("cron config must be an object."));
        }
        let mut config = Self::default();
        if let Some(enabled) = value["enabled"].as_bool() {
            config.enabled = enabled;
        }
        if let Some(zone) = value["timezone"]
            .as_str()
            .map(str::trim)
            .filter(|zone| schedule::valid_timezone(zone))
        {
            config.timezone = zone.into();
        }
        if let Some(max) =
            positive_integer(&value["maxConcurrentRuns"]).and_then(|v| usize::try_from(v).ok())
        {
            config.max_concurrent_runs = max;
        }
        if let Some(timeout) = positive_integer(&value["runTimeoutMinutes"]) {
            config.run_timeout_minutes = timeout;
        }
        Ok(config)
    }
}
fn positive_integer(value: &Value) -> Option<u64> {
    value.as_u64().filter(|v| *v > 0)
}

pub trait CronTaskHost: Send + Sync {
    fn prepare_task<'a>(&'a self, task: &'a Value) -> TaskFuture<'a, ()>;
    fn release_task<'a>(&'a self, task: &'a Value) -> TaskFuture<'a, ()>;
    /// Mark stopRequested before awaiting abort. Return the actual active run, never a guessed id.
    fn stop_run<'a>(
        &'a self,
        project: &'a str,
        task_id: Option<&'a str>,
        run_id: Option<&'a str>,
    ) -> TaskFuture<'a, Option<Value>>;
    fn poke(&self);
}

#[derive(Clone)]
pub struct CronTasks {
    home: PathBuf,
    config: CronConfig,
    now: Arc<dyn Fn() -> i64 + Send + Sync>,
    uuid: Arc<dyn Fn() -> String + Send + Sync>,
    loaded: Arc<Mutex<Vec<CronStore>>>,
}
impl CronTasks {
    pub fn new(home: PathBuf, config: CronConfig) -> CronResult<Self> {
        Self::with_sources(
            home,
            config,
            Arc::new(system_now_ms),
            Arc::new(|| uuid::Uuid::new_v4().to_string()),
        )
    }
    pub fn with_sources(
        home: PathBuf,
        config: CronConfig,
        now: Arc<dyn Fn() -> i64 + Send + Sync>,
        uuid: Arc<dyn Fn() -> String + Send + Sync>,
    ) -> CronResult<Self> {
        if config.max_concurrent_runs == 0 || config.run_timeout_minutes == 0 {
            return Err(validation(
                "Cron concurrency and timeout must be positive integers.",
            ));
        }
        Ok(Self {
            home: absolute(&home)?,
            config,
            now,
            uuid,
            loaded: Default::default(),
        })
    }
    pub fn config(&self) -> &CronConfig {
        &self.config
    }
    pub fn now_ms(&self) -> i64 {
        (self.now)()
    }
    pub fn store(&self, project: &str) -> CronResult<CronStore> {
        let candidate = CronStore::new(&self.home, project)?;
        let mut loaded = self
            .loaded
            .lock()
            .map_err(|_| validation("Cron project registry lock poisoned."))?;
        if let Some(store) = loaded
            .iter()
            .find(|store| store.project_key == candidate.project_key)
        {
            return Ok(store.clone());
        }
        candidate.record_project()?;
        loaded.push(candidate.clone());
        Ok(candidate)
    }
    pub fn stores(&self) -> CronResult<Vec<CronStore>> {
        self.loaded
            .lock()
            .map(|v| v.clone())
            .map_err(|_| validation("Cron project registry lock poisoned."))
    }
    pub fn discover(&self) -> CronResult<Vec<CronStore>> {
        migration::migrate(&self.home, self.now_ms())?;
        for project in migration::discover_projects(&self.home)? {
            self.store(&project)?;
        }
        self.stores()
    }
    pub fn handle<'a>(
        &'a self,
        method: &str,
        params: &'a Value,
        host: &'a dyn CronTaskHost,
    ) -> Option<TaskFuture<'a, Value>> {
        let method = match method {
            "cron_create" => 0,
            "cron_list" => 1,
            "cron_update" => 2,
            "cron_delete" => 3,
            "cron_stop" => 4,
            "cron_run_now" => 5,
            _ => return None,
        };
        Some(Box::pin(async move {
            match method {
                0 => self.create_task(params, host).await,
                1 => self.list_tasks(params),
                2 => self.update_task(params, host).await,
                3 => self.delete_task(params, host).await,
                4 => self.stop_task(params, host).await,
                _ => self.run_task_now(params, host).await,
            }
        }))
    }
    pub async fn create_task(&self, input: &Value, host: &dyn CronTaskHost) -> CronResult<Value> {
        let project = required_project(input)?;
        let store = self.store(project)?;
        if !self.config.enabled {
            return Err(validation(
                "Cron is disabled. Enable it in pilotdeck.yaml to create tasks.",
            ));
        }
        let now = self.now_ms();
        let id = (self.uuid)();
        self.create_in_store(input, &store, now, id, host).await
    }
    async fn create_in_store(
        &self,
        input: &Value,
        store: &CronStore,
        now: i64,
        id: String,
        host: &dyn CronTaskHost,
    ) -> CronResult<Value> {
        let message = required_string(input, "message")?;
        let (schedule, timezone, next) = self.normalized(input, now)?;
        let mut task = json!({"schemaVersion":1,"taskId":id,"message":message,"schedule":schedule,
            "status":"scheduled","sessionKey":format!("cron:{id}"),"channelKey":"cron",
            "projectKey":store.project_key,"timezone":timezone,"createdAt":schedule::iso(now),"updatedAt":schedule::iso(now),
            "nextRunAt":schedule::iso(next),"revision":0});
        for (source, target) in [
            ("sessionKey", "originSessionKey"),
            ("channelKey", "originChannelKey"),
            ("mode", "mode"),
        ] {
            if let Some(value) = input.get(source) {
                task[target] = value.clone();
            }
        }
        if schedule["type"] == "cron" {
            task["scheduleComputationVersion"] = json!(3);
        }
        host.prepare_task(&task).await?;
        if let Err(error) = store.put_task(&task) {
            let _ = host.release_task(&task).await;
            return Err(error);
        }
        host.poke();
        Ok(json!({"task":task}))
    }
    fn normalized(&self, input: &Value, now: i64) -> CronResult<(Value, Value, i64)> {
        let schedule = schedule::normalize_schedule(
            &input["schedule"],
            input.get("timezone"),
            &self.config.timezone,
            now,
            true,
        )?;
        let timezone = if schedule["type"] == "cron" {
            schedule["timezone"].clone()
        } else {
            input
                .get("timezone")
                .filter(|v| !v.is_null())
                .cloned()
                .unwrap_or_else(|| json!(self.config.timezone))
        };
        let next = schedule::compute_next_run_at(
            &schedule,
            now,
            timezone.as_str().unwrap_or(&self.config.timezone),
        )?
        .ok_or_else(|| validation("Cron schedule does not produce a valid future run time."))?;
        if schedule["type"] == "once" && next < now {
            return Err(validation(
                "One-time Cron tasks must be scheduled in the future.",
            ));
        }
        Ok((schedule, timezone, next))
    }
    pub fn list_tasks(&self, input: &Value) -> CronResult<Value> {
        let history = input.get("includeHistory").is_some_and(truthy);
        let limit = input
            .get("limit")
            .filter(|v| !v.is_null())
            .map(|v| v.as_f64().unwrap_or(f64::NAN));
        if let Some(project) = optional_project(input)? {
            let store = self.store(project)?;
            let mut result = json!({"tasks":store.list_tasks()?});
            if history {
                result["recentRuns"] = json!(store.list_runs(limit)?);
            }
            return Ok(result);
        }
        let mut tasks = Vec::new();
        let mut runs = Vec::new();
        for store in self.stores()? {
            tasks.extend(store.list_tasks()?);
            if history {
                runs.extend(store.list_runs(limit)?);
            }
        }
        tasks.sort_by(|a, b| text(a, "nextRunAt").cmp(text(b, "nextRunAt")));
        let mut result = json!({"tasks":tasks});
        if history {
            runs.sort_by(|a, b| text(b, "startedAt").cmp(text(a, "startedAt")));
            let end = slice_end(runs.len(), limit.unwrap_or(50.0));
            runs.truncate(end);
            result["recentRuns"] = json!(runs);
        }
        Ok(result)
    }
    pub async fn update_task(&self, input: &Value, host: &dyn CronTaskHost) -> CronResult<Value> {
        if !self.config.enabled {
            return Err(validation(
                "Cron is disabled. Enable it in pilotdeck.yaml to update tasks.",
            ));
        }
        let id = required_string(input, "taskId")?;
        let Some(store) = self.find_task_store(id, optional_project(input)?)? else {
            return Ok(json!({"updated":false,"reason":"not_found"}));
        };
        let Some(project) = input
            .get("projectKey")
            .and_then(Value::as_str)
            .filter(|p| !p.trim().is_empty())
        else {
            return Ok(json!({"updated":false,"reason":"not_found"}));
        };
        if absolute(Path::new(project))?.to_string_lossy() != store.project_key {
            return Ok(json!({"updated":false,"reason":"not_found"}));
        }
        let revision = safe_revision(input.get("expectedRevision")).ok_or_else(|| {
            validation("Cron task expectedRevision must be a non-negative integer.")
        })?;
        let message = input
            .get("message")
            .and_then(Value::as_str)
            .filter(|m| !m.trim().is_empty())
            .ok_or_else(|| validation("Cron task message is required."))?;
        if let Some(zone) = input.get("timezone")
            && !zone.as_str().is_some_and(schedule::valid_timezone)
        {
            return Err(validation(format!(
                "Invalid Cron timezone: {}",
                display(zone)
            )));
        }
        let now = self.now_ms();
        let (schedule, timezone, next) = self.normalized(input, now)?;
        let mut reason = None;
        let updated = store.update_task(id, |current| {
            let current_project = current
                .get("projectKey")
                .and_then(Value::as_str)
                .filter(|p| !p.trim().is_empty())
                .unwrap_or(&store.project_key);
            if absolute(Path::new(current_project))?.to_string_lossy() != store.project_key {
                reason = Some("not_found");
                return Ok(Some(current.clone()));
            }
            if current["status"] == "running" {
                reason = Some("running");
                return Ok(Some(current.clone()));
            }
            if revision_of(current) != revision {
                reason = Some("conflict");
                return Ok(Some(current.clone()));
            }
            let mut task = current.clone();
            task["message"] = json!(message);
            task["schedule"] = schedule.clone();
            task["timezone"] = timezone;
            task["nextRunAt"] = json!(schedule::iso(next));
            task["updatedAt"] = json!(schedule::iso(now));
            bump_revision(&mut task)?;
            set_computation_version(&mut task);
            Ok(Some(task))
        })?;
        if let Some(reason) = reason {
            return Ok(json!({"updated":false,"reason":reason}));
        }
        match updated {
            Some(task) => {
                host.poke();
                Ok(json!({"updated":true,"task":task}))
            }
            None => Ok(json!({"updated":false,"reason":"not_found"})),
        }
    }
    pub async fn delete_task(&self, input: &Value, host: &dyn CronTaskHost) -> CronResult<Value> {
        let id = required_string(input, "taskId")?;
        let Some(store) = self.find_task_store(id, optional_project(input)?)? else {
            return Ok(json!({"deleted":false}));
        };
        let mut stopped = None;
        if input.get("stopRunning").is_some_and(truthy) {
            let result = self.stop_in_store(&store, Some(id), None, host).await?;
            stopped = result.get("runId").cloned();
        }
        let old = store.get_task(id)?;
        let deleted = store.delete_task(id)?;
        if deleted && let Some(task) = old {
            let _ = host.release_task(&task).await;
        }
        host.poke();
        let mut result = json!({"deleted":deleted});
        if let Some(run) = stopped {
            result["stoppedRunId"] = run;
        }
        Ok(result)
    }
    pub async fn stop_task(&self, input: &Value, host: &dyn CronTaskHost) -> CronResult<Value> {
        let task_id = optional_id(input, "taskId")?;
        let run_id = optional_id(input, "runId")?;
        let store = if let Some(project) = optional_project(input)? {
            Some(self.store(project)?)
        } else {
            let mut matches = Vec::new();
            for store in self.stores()? {
                if store.list_tasks()?.iter().any(|task| {
                    task_id.is_some_and(|id| text(task, "taskId") == id)
                        || run_id.is_some_and(|id| text(task, "lastRunId") == id)
                }) {
                    matches.push(store);
                }
            }
            if matches.len() > 1 {
                return Err(validation(format!(
                    "Cron run identifier is ambiguous across projects: {}",
                    run_id.or(task_id).unwrap_or("undefined")
                )));
            }
            matches.pop()
        };
        match store {
            Some(store) => self.stop_in_store(&store, task_id, run_id, host).await,
            None => Ok(not_stopped(task_id, run_id)),
        }
    }
    async fn stop_in_store(
        &self,
        store: &CronStore,
        task_id: Option<&str>,
        run_id: Option<&str>,
        host: &dyn CronTaskHost,
    ) -> CronResult<Value> {
        if task_id.is_none() && run_id.is_none() {
            return Ok(not_stopped(task_id, run_id));
        }
        let Some(active) = host.stop_run(&store.project_key, task_id, run_id).await? else {
            return Ok(not_stopped(task_id, run_id));
        };
        let id = required_string(&active, "taskId")?;
        let actual_run = required_string(&active, "runId")?;
        if task_id.is_some_and(|wanted| wanted != id)
            || run_id.is_some_and(|wanted| wanted != actual_run)
        {
            return Err(CronError::new(
                "cron_host_contract",
                "stop_run returned an unrelated active run",
            ));
        }
        let mut deleted = false;
        if active["scheduleType"] == "once" {
            let task=store.get_task(id)?.unwrap_or_else(||json!({"taskId":id,"sessionKey":format!("cron:{id}"),"projectKey":store.project_key}));
            deleted = store.delete_task(id)?;
            let _ = host.release_task(&task).await;
        }
        host.poke();
        Ok(json!({"stopped":true,"taskId":id,"runId":actual_run,"deletedOneTimeTask":deleted}))
    }
    pub async fn run_task_now(&self, input: &Value, host: &dyn CronTaskHost) -> CronResult<Value> {
        let id = required_string(input, "taskId")?;
        let Some(store) = self.find_task_store(id, optional_project(input)?)? else {
            return Ok(json!({"started":false,"reason":"not_found"}));
        };
        let Some(task) = store.get_task(id)? else {
            return Ok(json!({"started":false,"reason":"not_found"}));
        };
        if task["status"] == "running" {
            return Ok(json!({"started":false,"reason":"already_running","taskId":id}));
        }
        if !self.config.enabled {
            return Err(validation(
                "Cron is disabled. Enable it in pilotdeck.yaml to create tasks.",
            ));
        }
        // Sample once so creating an immediate copy cannot fail merely because the clock advanced.
        let now = self.now_ms();
        let mut input = json!({"message":task["message"],"projectKey":store.project_key,"schedule":{"type":"once","runAt":schedule::iso(now)}});
        for (source, target) in [
            ("originSessionKey", "sessionKey"),
            ("originChannelKey", "channelKey"),
            ("mode", "mode"),
        ] {
            if let Some(value) = task.get(source) {
                input[target] = value.clone();
            }
        }
        let created = self
            .create_in_store(&input, &store, now, (self.uuid)(), host)
            .await?;
        Ok(json!({"started":true,"taskId":created["task"]["taskId"]}))
    }
    fn find_task_store(&self, id: &str, project: Option<&str>) -> CronResult<Option<CronStore>> {
        if let Some(project) = project {
            let store = self.store(project)?;
            return Ok(store.get_task(id)?.is_some().then_some(store));
        }
        let mut found = None;
        for store in self.stores()? {
            if store.get_task(id)?.is_some() {
                if found.is_some() {
                    return Err(validation(format!(
                        "Cron task id is ambiguous across projects: {id}"
                    )));
                }
                found = Some(store);
            }
        }
        Ok(found)
    }
}

pub fn system_now_ms() -> i64 {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(d) => d.as_millis().min(i64::MAX as u128) as i64,
        Err(e) => -(e.duration().as_millis().min(i64::MAX as u128) as i64),
    }
}
pub(super) fn absolute(path: &Path) -> CronResult<PathBuf> {
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                result.pop();
            }
            part => result.push(part.as_os_str()),
        }
    }
    Ok(result)
}
fn required_project(input: &Value) -> CronResult<&str> {
    input
        .get("projectKey")
        .and_then(Value::as_str)
        .filter(|p| !p.trim().is_empty())
        .ok_or_else(|| validation("Cron task creation requires a projectKey."))
}
fn optional_project(input: &Value) -> CronResult<Option<&str>> {
    match input.get("projectKey") {
        None | Some(Value::Null) => Ok(None),
        Some(v) if !truthy(v) => Ok(None),
        Some(Value::String(v)) => Ok(Some(v)),
        _ => Err(validation("Cron projectKey must be a string.")),
    }
}
fn required_string<'a>(input: &'a Value, key: &str) -> CronResult<&'a str> {
    input
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| validation(format!("Cron task {key} must be a string.")))
}
fn optional_id<'a>(input: &'a Value, key: &str) -> CronResult<Option<&'a str>> {
    match input.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(v)) => Ok((!v.is_empty()).then_some(v.as_str())),
        _ => Err(validation(format!("Cron task {key} must be a string."))),
    }
}
pub(super) fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}
pub(super) fn safe_revision(value: Option<&Value>) -> Option<u64> {
    let number = value?.as_f64()?;
    (number.is_finite()
        && (0.0..=9_007_199_254_740_991.0).contains(&number)
        && number.fract() == 0.0)
        .then_some(number as u64)
}
pub(super) fn revision_of(task: &Value) -> u64 {
    safe_revision(task.get("revision")).unwrap_or(0)
}
pub(super) fn bump_revision(task: &mut Value) -> CronResult<()> {
    let next = revision_of(task)
        .checked_add(1)
        .filter(|v| *v <= 9_007_199_254_740_991)
        .ok_or_else(|| validation("Cron task revision exceeded safe integer range."))?;
    task["revision"] = json!(next);
    Ok(())
}
pub(super) fn set_computation_version(task: &mut Value) {
    if task["schedule"]["type"] == "cron" {
        task["scheduleComputationVersion"] = json!(3);
    } else if let Some(object) = task.as_object_mut() {
        object.remove("scheduleComputationVersion");
    }
}
fn not_stopped(task: Option<&str>, run: Option<&str>) -> Value {
    let mut result = json!({"stopped":false});
    if let Some(task) = task {
        result["taskId"] = json!(task);
    }
    if let Some(run) = run {
        result["runId"] = json!(run);
    }
    result
}
fn slice_end(len: usize, end: f64) -> usize {
    if end.is_nan() {
        return 0;
    }
    if end < 0.0 {
        len.saturating_sub((-end.trunc()).min(len as f64) as usize)
    } else {
        end.trunc().min(len as f64) as usize
    }
}

#[cfg(test)]
#[path = "tasks/tests.rs"]
mod tests;
