use super::{AlwaysOnError, AlwaysOnService, RunTools, chat, first_error, now, prompts, storage};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
};
use tokio::{sync::watch, task::JoinHandle, time::Instant};

pub(super) struct Automation {
    leases: Mutex<BTreeMap<String, Value>>,
    busy: Mutex<HashSet<String>>,
    signals: Mutex<HashMap<String, Instant>>,
    stop: watch::Sender<bool>,
    handles: Mutex<Option<Vec<JoinHandle<()>>>>,
}
impl Default for Automation {
    fn default() -> Self {
        Self {
            leases: Mutex::new(BTreeMap::new()),
            busy: Mutex::new(HashSet::new()),
            signals: Mutex::new(HashMap::new()),
            stop: watch::channel(false).0,
            handles: Mutex::new(None),
        }
    }
}
fn milliseconds(value: &str) -> Option<f64> {
    pilotdeck_transport::system_clock::parse_node22_date(&value.into())
        .ok()
        .flatten()
}
fn current_ms() -> f64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as f64
}
fn key(project: &str, channel: &str, writer: &str) -> String {
    format!("{project}\u{1}{channel}\u{1}{writer}")
}
fn ignored(path: &Path) -> bool {
    path.components().any(|part| {
        matches!(
            part.as_os_str().to_str(),
            Some(
                ".git"
                    | "node_modules"
                    | ".pilotdeck"
                    | ".pilotdeck-always-on"
                    | "dist"
                    | ".DS_Store"
            )
        )
    })
}

impl AlwaysOnService {
    /// Called by application composition after the native host is bound.
    pub async fn start(self: &Arc<Self>) -> Result<(), AlwaysOnError> {
        let mut handles = self
            .automation
            .handles
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if handles.is_some() || *self.automation.stop.borrow() {
            return Ok(());
        }
        let mut running = Vec::new();
        for (project, config) in self.config["projects"].as_object().into_iter().flatten() {
            if config["enabled"] != true {
                continue;
            }
            // Restore dormancy from disk before publishing a running scheduler.
            self.store(project)?.state(&now())?;
        }
        for (project, config) in self.config["projects"].as_object().into_iter().flatten() {
            if config["enabled"] != true {
                continue;
            }
            let service = self.clone();
            let project = project.clone();
            running.push(tokio::spawn(async move {
                service.schedule_project(project).await;
            }));
        }
        *handles = Some(running);
        Ok(())
    }

    pub(super) async fn stop_schedulers(&self) {
        self.automation.stop.send_replace(true);
        let handles = self
            .automation
            .handles
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .unwrap_or_default();
        for handle in handles {
            let _ = handle.await;
        }
    }

    /// Application adapters update this from actual foreground turn ownership.
    pub fn set_project_busy(&self, project: &str, busy: bool) {
        let mut projects = self
            .automation
            .busy
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if busy {
            projects.insert(project.into());
        } else {
            projects.remove(project);
        }
    }
    pub fn set_lease(&self, mut lease: Value) -> Result<Value, AlwaysOnError> {
        let project = super::required(&lease, "projectKey")?.to_owned();
        let channel = super::required(&lease, "channelKey")?.to_owned();
        let writer = super::required(&lease, "writerId")?.to_owned();
        super::required(&lease, "sessionKey")?;
        if !lease["agentBusy"].is_boolean() {
            return Err(AlwaysOnError::new(
                "invalid_params",
                "agentBusy must be a boolean",
            ));
        }
        lease["schemaVersion"] = json!(1);
        if lease.get("writtenAt").is_none() {
            lease["writtenAt"] = json!(now());
        }
        if lease.get("lastUserMsgAt").is_none() {
            lease["lastUserMsgAt"] = Value::Null;
        }
        self.automation
            .leases
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key(&project, &channel, &writer), lease.clone());
        Ok(lease)
    }
    pub fn mark_busy(&self, project: &str, channel: &str, writer: &str) {
        self.mark_lease(project, channel, writer, true);
    }
    pub fn mark_idle(&self, project: &str, channel: &str, writer: &str) {
        self.mark_lease(project, channel, writer, false);
    }
    fn mark_lease(&self, project: &str, channel: &str, writer: &str, busy: bool) {
        if let Some(lease) = self
            .automation
            .leases
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(&key(project, channel, writer))
        {
            let timestamp = now();
            lease["agentBusy"] = json!(busy);
            lease["writtenAt"] = json!(timestamp);
            if !busy {
                lease["lastUserMsgAt"] = json!(timestamp);
            }
        }
    }
    pub fn remove_lease(&self, project: &str, channel: &str, writer: &str) {
        self.automation
            .leases
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&key(project, channel, writer));
    }
    pub fn remove_writer(&self, writer: &str) {
        self.automation
            .leases
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|_, lease| lease["writerId"] != writer);
    }
    pub fn leases(&self) -> Vec<Value> {
        self.automation
            .leases
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect()
    }
    pub fn file_signal(&self, project: &str, path: &Path) {
        if path.as_os_str().is_empty() {
            return;
        }
        let relative = path.strip_prefix(project).unwrap_or(path);
        if !ignored(relative) {
            self.automation
                .signals
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(project.into(), Instant::now());
        }
    }
    fn trigger(&self, name: &str, default: f64) -> f64 {
        self.config["trigger"][name]
            .as_f64()
            .filter(|v| v.is_finite() && *v >= 0.0)
            .unwrap_or(default)
    }
    fn tick_interval(&self) -> Duration {
        let millis = (self.trigger("tickIntervalMinutes", 5.0) * 60_000.0).max(1000.0);
        // Node setTimeout clamps an overflowing delay to 1 ms.
        Duration::from_millis(if millis > f64::from(i32::MAX) || !millis.is_finite() {
            1
        } else {
            millis.trunc() as u64
        })
    }
    fn stopped(&self) -> bool {
        self.active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .stopping
    }

    async fn schedule_project(self: Arc<Self>, project: String) {
        let mut stop = self.automation.stop.subscribe();
        let mut next = Instant::now() + self.tick_interval();
        let mut previous = None;
        loop {
            if *stop.borrow() {
                break;
            }
            let wake = next.min(Instant::now() + Duration::from_millis(250));
            tokio::select! {_ = tokio::time::sleep_until(wake)=>{},_ = stop.changed()=>{if *stop.borrow(){break;}}}
            if *stop.borrow() {
                break;
            }
            let result = self.poll_signal(&project, &mut previous).await;
            match result {
                Ok(true) => {
                    next = Instant::now() + self.tick_interval();
                }
                Ok(false) => {}
                Err(error) => {
                    eprintln!("always-on signal watcher: {error}");
                    previous = None;
                }
            }
            if Instant::now() >= next {
                if let Err(error) = self.run_tick_once(&project).await {
                    eprintln!("always-on tick failed: {error}");
                }
                next = Instant::now() + self.tick_interval();
            }
        }
    }

    async fn poll_signal(
        &self,
        project: &str,
        previous: &mut Option<BTreeMap<PathBuf, (u64, u128)>>,
    ) -> Result<bool, AlwaysOnError> {
        let store = self.store(project)?;
        if store.state(&now())?.get("dormant").is_none() {
            *previous = None;
            self.automation
                .signals
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(project);
            return Ok(false);
        }
        let root = PathBuf::from(project);
        let current = tokio::task::spawn_blocking(move || fingerprint(&root))
            .await
            .map_err(|e| AlwaysOnError::new("watch_failed", e.to_string()))??;
        if previous.as_ref().is_some_and(|old| old != &current) {
            self.file_signal(project, Path::new("."));
        }
        *previous = Some(current);
        let elapsed = self
            .automation
            .signals
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(project)
            .is_some_and(|at| at.elapsed() >= Duration::from_secs(2));
        if !elapsed {
            return Ok(false);
        }
        self.automation
            .signals
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(project);
        store.update_state(&now(), json!({"dormant":null}))?;
        *previous = None;
        Ok(true)
    }

    pub async fn run_tick_once(&self, project: &str) -> Result<Value, AlwaysOnError> {
        if self.stopped() || !self.enabled(project) {
            return Ok(json!({"outcome":"blocked","reason":"project_disabled"}));
        }
        let store = self.store(project)?;
        let timestamp = now();
        let state = store.state(&timestamp)?;
        let fresh: Vec<Value> = self
            .leases()
            .into_iter()
            .filter(|l| {
                l["projectKey"] == project
                    && l["writtenAt"]
                        .as_str()
                        .and_then(milliseconds)
                        .is_some_and(|t| {
                            t >= current_ms() - self.trigger("heartbeatStaleSeconds", 90.0) * 1000.0
                        })
            })
            .collect();
        let block = if !Path::new(project).exists() {
            Some("project_missing")
        } else if state.get("dormant").is_some() {
            Some("dormant_no_signal")
        } else if self
            .automation
            .busy
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(project)
            || fresh.iter().any(|l| l["agentBusy"] == true)
        {
            Some("agent_busy")
        } else if fresh
            .iter()
            .filter_map(|l| l["lastUserMsgAt"].as_str().and_then(milliseconds))
            .any(|t| current_ms() - t < self.trigger("recentUserMsgMinutes", 5.0) * 60_000.0)
        {
            Some("recent_user_msg")
        } else if state["lastFireCompletedAt"]
            .as_str()
            .and_then(milliseconds)
            .is_some_and(|t| current_ms() - t < self.trigger("cooldownMinutes", 60.0) * 60_000.0)
        {
            Some("cooldown")
        } else if state["todayRunCount"].as_f64().unwrap_or(0.0) >= self.trigger("dailyBudget", 4.0)
        {
            Some("daily_budget")
        } else if store.root.join("locks/discovery.lock").exists() {
            Some("lock_busy")
        } else {
            None
        };
        if let Some(reason) = block {
            return Ok(json!({"outcome":"blocked","reason":reason}));
        }
        if let Some(cycle) = state["activeWorkCycleId"]
            .as_str()
            .map(|id| store.record("cycles", id))
            .transpose()?
            .flatten()
            && cycle["status"] == "active"
            && cycle["planIds"]
                .as_array()
                .is_some_and(|ids| ids.len() >= 3)
        {
            return Ok(json!({"outcome":"blocked","reason":"cycle_full"}));
        }
        let lock = self
            .project_locks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(project.into())
            .or_default()
            .clone();
        let Ok(_project_guard) = lock.try_lock() else {
            return Ok(json!({"outcome":"blocked","reason":"lock_busy"}));
        };
        let run = uuid::Uuid::new_v4().to_string();
        let Some(_lock) = DiscoveryLock::acquire(&store, &run, &timestamp)? else {
            return Ok(json!({"outcome":"blocked","reason":"lock_busy"}));
        };
        let _admission = self.admit(
            project,
            &run,
            &["discovery", "workspace", "execute", "report"],
        )?;
        store.mark_started(&timestamp, &run)?;
        let result = self.discover(&store, &run, &timestamp).await;
        match result {
            Ok(()) => Ok(json!({"outcome":"fired"})),
            Err(error) => Err(error),
        }
    }

    async fn discover(
        &self,
        store: &storage::Store,
        run: &str,
        started: &str,
    ) -> Result<(), AlwaysOnError> {
        self.event(store, run, "discovery_started", json!({}))?;
        let state = store.state(started)?;
        let active = state["activeWorkCycleId"]
            .as_str()
            .map(|id| store.record("cycles", id))
            .transpose()?
            .flatten();
        let workspace = active
            .as_ref()
            .filter(|c| c["status"] == "active")
            .map(|c| &c["workspace"])
            .or_else(|| state.get("currentWorkspace"))
            .filter(|ws| {
                ws["cwd"]
                    .as_str()
                    .is_some_and(|cwd| Path::new(cwd).exists())
            });
        let cwd = workspace
            .and_then(|w| w["cwd"].as_str())
            .map(PathBuf::from)
            .unwrap_or_else(|| store.project.clone());
        let home = store.home.clone();
        let project = store.project.clone();
        let digest = tokio::task::spawn_blocking(move || chat::digest(&home, &project))
            .await
            .map_err(|e| AlwaysOnError::new("discovery_failed", e.to_string()))??;
        let plans = store.index("plans")?;
        let tools = Arc::new(RunTools::new(
            store.clone(),
            run,
            "discovery",
            Value::Null,
            Value::Null,
            started.into(),
        ));
        tools.set_digest(&digest);
        let prompt = prompts::discovery(
            self.zh(),
            &store.project.to_string_lossy(),
            run,
            started,
            workspace,
            &digest,
            &plans,
        );
        let events = self
            .phase(store, run, "discovery", &cwd, prompt, tools.clone())
            .await;
        let error = match events {
            Ok(events) => first_error(&events, "discovery_failed"),
            Err(error) => Some(error),
        };
        if let Some(plan) = tools.submitted_plan() {
            let id = super::required(&plan, "id")?;
            self.event(
                store,
                run,
                "plan_produced",
                json!({"title":plan["title"],"planId":id}),
            )?;
            let _ = self
                .rerun(&store.project.to_string_lossy(), id, run, Some(started))
                .await?;
            return Ok(());
        }
        let finished = now();
        if let Some(error) = error {
            self.event(
                store,
                run,
                "run_failed",
                json!({"outcome":"failed","error":error.value()}),
            )?;
            store.complete(&finished, run, "", "failed")?;
            store.append("run-history.jsonl",&json!({"schemaVersion":1,"runId":run,"startedAt":started,"finishedAt":finished,"outcome":"failed","error":error.value()}))?;
        } else {
            self.event(store, run, "no_plan", json!({"outcome":"no_plan"}))?;
            store.complete(&finished, run, "", "no_plan")?;
            store.update_state(
                &finished,
                json!({"dormant":{"since":finished,"lastBaselineAt":finished}}),
            )?;
            store.append("run-history.jsonl",&json!({"schemaVersion":1,"runId":run,"startedAt":started,"finishedAt":finished,"outcome":"no_plan"}))?;
        }
        Ok(())
    }
}

fn fingerprint(root: &Path) -> std::io::Result<BTreeMap<PathBuf, (u64, u128)>> {
    let mut result = BTreeMap::new();
    let mut pending = vec![root.to_owned()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let path = entry.path();
            let rel = path.strip_prefix(root).unwrap_or(&path);
            if ignored(rel) {
                continue;
            }
            let metadata = path.symlink_metadata()?;
            let changed = metadata
                .modified()?
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            result.insert(
                rel.into(),
                if metadata.is_dir() {
                    (0, 0)
                } else {
                    (metadata.len(), changed)
                },
            );
            if metadata.is_dir() {
                pending.push(path);
            }
        }
    }
    Ok(result)
}

struct DiscoveryLock {
    path: PathBuf,
    run: String,
}
impl DiscoveryLock {
    fn acquire(
        store: &storage::Store,
        run: &str,
        started: &str,
    ) -> Result<Option<Self>, AlwaysOnError> {
        let path = store.root.join("locks/discovery.lock");
        fs::create_dir_all(path.parent().expect("parent"))?;
        let mut file = match fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let guard = Self {
            path,
            run: run.into(),
        };
        file.write_all(&serde_json::to_vec_pretty(
            &json!({"pid":std::process::id(),"startedAt":started,"runId":run}),
        )?)?;
        Ok(Some(guard))
    }
}
impl Drop for DiscoveryLock {
    fn drop(&mut self) {
        if fs::read(&self.path)
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .is_some_and(|v| v["runId"] == self.run)
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct NoTurn;
    impl crate::cron::always_on::AlwaysOnHost for NoTurn {
        fn turn(
            &self,
            _: crate::cron::always_on::TurnRequest,
        ) -> super::super::HostFuture<'_, Vec<Value>> {
            Box::pin(async { panic!("gated scheduler must not submit a turn") })
        }
        fn abort<'a>(
            &'a self,
            _: &'a str,
            _: &'a str,
            _: &'a str,
        ) -> super::super::HostFuture<'a, ()> {
            Box::pin(async { Ok(()) })
        }
        fn close<'a>(&'a self, _: &'a str) -> super::super::HostFuture<'a, ()> {
            Box::pin(async { Ok(()) })
        }
        fn on_turn_event(&self, _: &str, _: &str, _: &Value) {}
    }
    struct Fixture {
        root: PathBuf,
        project: String,
        service: Arc<AlwaysOnService>,
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir()
                .join(format!("pdrs-always-scheduler-{}", uuid::Uuid::new_v4()));
            let project = root.join("project");
            fs::create_dir_all(&project).unwrap();
            let project = project.to_string_lossy().into_owned();
            let config = json!({"projects":{project.clone():{"enabled":true}}});
            let service = Arc::new(AlwaysOnService::new(
                root.join("home"),
                config,
                Arc::new(NoTurn),
            ));
            Self {
                root,
                project,
                service,
            }
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn dormant_real_file_changes_wake_after_debounce_and_ignored_paths_do_not() {
        let f = Fixture::new();
        let store = f.service.store(&f.project).unwrap();
        store
            .update_state(
                &now(),
                json!({"dormant":{"since":now(),"lastBaselineAt":now()}}),
            )
            .unwrap();
        fs::write(Path::new(&f.project).join("source.txt"), "initial").unwrap();
        let mut previous = None;
        assert!(
            !f.service
                .poll_signal(&f.project, &mut previous)
                .await
                .unwrap()
        );
        fs::create_dir_all(Path::new(&f.project).join("node_modules")).unwrap();
        fs::write(
            Path::new(&f.project).join("node_modules/ignored"),
            "ignored",
        )
        .unwrap();
        assert!(
            !f.service
                .poll_signal(&f.project, &mut previous)
                .await
                .unwrap()
        );
        tokio::time::advance(Duration::from_secs(3)).await;
        assert!(
            !f.service
                .poll_signal(&f.project, &mut previous)
                .await
                .unwrap()
        );
        assert!(store.state(&now()).unwrap().get("dormant").is_some());
        fs::write(
            Path::new(&f.project).join("source.txt"),
            "changed file content",
        )
        .unwrap();
        assert!(
            !f.service
                .poll_signal(&f.project, &mut previous)
                .await
                .unwrap()
        );
        tokio::time::advance(Duration::from_secs(2)).await;
        assert!(
            f.service
                .poll_signal(&f.project, &mut previous)
                .await
                .unwrap()
        );
        assert!(store.state(&now()).unwrap().get("dormant").is_none());
    }

    #[tokio::test]
    async fn gates_budget_cycle_capacity_and_existing_lock_before_native_execution() {
        let f = Fixture::new();
        let store = f.service.store(&f.project).unwrap();
        store
            .update_state(&now(), json!({"todayRunCount":4}))
            .unwrap();
        assert_eq!(
            f.service.run_tick_once(&f.project).await.unwrap()["reason"],
            "daily_budget"
        );
        store
            .update_state(&now(), json!({"todayRunCount":0}))
            .unwrap();
        let lock = DiscoveryLock::acquire(&store, "owner", &now())
            .unwrap()
            .unwrap();
        assert_eq!(
            f.service.run_tick_once(&f.project).await.unwrap()["reason"],
            "lock_busy"
        );
        drop(lock);
        store
            .insert_record(
                "cycles",
                json!({"id":"cycle","status":"active","planIds":["one","two","three"]}),
            )
            .unwrap();
        store
            .update_state(&now(), json!({"activeWorkCycleId":"cycle"}))
            .unwrap();
        assert_eq!(
            f.service.run_tick_once(&f.project).await.unwrap()["reason"],
            "cycle_full"
        );
        f.service.stop().await;
        assert_eq!(
            f.service.run_tick_once(&f.project).await.unwrap()["reason"],
            "project_disabled"
        );
    }

    #[tokio::test]
    async fn start_restores_dormancy_and_shutdown_joins_schedulers() {
        let f = Fixture::new();
        let store = f.service.store(&f.project).unwrap();
        store
            .update_state(
                &now(),
                json!({"dormant":{"since":now(),"lastBaselineAt":now()}}),
            )
            .unwrap();
        f.service.start().await.unwrap();
        f.service.start().await.unwrap();
        assert_eq!(
            f.service
                .automation
                .handles
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .len(),
            1
        );
        tokio::time::timeout(Duration::from_secs(3), f.service.stop())
            .await
            .unwrap();
        assert!(f.service.automation.handles.lock().unwrap().is_none());
        assert!(store.state(&now()).unwrap().get("dormant").is_some());
        f.service.start().await.unwrap();
        assert!(f.service.automation.handles.lock().unwrap().is_none());
    }
}
