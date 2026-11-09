use super::{NativeHost, RuntimeError};
use crate::cron::tasks::{CronError, CronStore, CronTaskHost, CronTasks, TaskFuture};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::{Notify, watch},
    task::JoinHandle,
};

#[derive(Clone)]
pub(super) struct ActiveRun {
    pub project: String,
    pub task: Value,
    pub stopped: bool,
    pub cancel: watch::Sender<bool>,
}

/// One owner for the global concurrency budget, including all discovered projects.
pub struct CronRuntime {
    pub(super) tasks: CronTasks,
    pub(super) host: Arc<dyn NativeHost>,
    pub(super) active: Mutex<HashMap<String, ActiveRun>>,
    jobs: Mutex<Vec<JoinHandle<Result<(), RuntimeError>>>>,
    timer: Mutex<Option<JoinHandle<()>>>,
    wake: Arc<Notify>,
    shutdown: watch::Sender<bool>,
    stopping: AtomicBool,
    started: AtomicBool,
    tick_gate: tokio::sync::Mutex<()>,
    lifecycle: tokio::sync::Mutex<()>,
    pub(super) errors: Mutex<Vec<RuntimeError>>,
}
impl CronRuntime {
    pub fn new(tasks: CronTasks, host: Arc<dyn NativeHost>) -> Arc<Self> {
        Arc::new(Self {
            tasks,
            host,
            active: Mutex::new(HashMap::new()),
            jobs: Mutex::new(vec![]),
            timer: Mutex::new(None),
            wake: Arc::new(Notify::new()),
            shutdown: watch::channel(false).0,
            stopping: AtomicBool::new(false),
            started: AtomicBool::new(false),
            tick_gate: tokio::sync::Mutex::new(()),
            lifecycle: tokio::sync::Mutex::new(()),
            errors: Mutex::new(vec![]),
        })
    }
    pub fn tasks(&self) -> &CronTasks {
        &self.tasks
    }
    pub fn active_run_count(&self) -> usize {
        self.active.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
    /// Includes background execution/storage failures, never converted into an empty success.
    pub fn take_errors(&self) -> Vec<RuntimeError> {
        std::mem::take(&mut *self.errors.lock().unwrap_or_else(|e| e.into_inner()))
    }
    pub(super) fn report(&self, error: RuntimeError) {
        eprintln!("cron runtime failure [{}]: {}", error.code, error.message);
        self.errors
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(error);
    }
    pub async fn start(self: &Arc<Self>) -> Result<(), RuntimeError> {
        let _lifecycle = self.lifecycle.lock().await;
        if self.stopping.load(Ordering::SeqCst) {
            return Err(RuntimeError::new(
                "gateway_disposed",
                "Cron runtime is stopped.",
            ));
        }
        if self.started.load(Ordering::SeqCst) || !self.tasks.config().enabled {
            return Ok(());
        }
        let _tick = self.tick_gate.lock().await;
        for store in self.tasks.discover().map_err(runtime_error)? {
            for task in store
                .migrate_sessions(self.tasks.now_ms())
                .map_err(runtime_error)?
            {
                self.host.release_task(&task).await?;
            }
            for task in store
                .recover(self.tasks.now_ms(), &self.tasks.config().timezone)
                .map_err(runtime_error)?
            {
                self.host.release_task(&task).await?;
            }
            for task in store.list_tasks().map_err(runtime_error)? {
                self.host.close(string(&task, "sessionKey")).await?;
                self.host.prepare_task(&task).await?;
            }
            store
                .recalculate(self.tasks.now_ms(), &self.tasks.config().timezone)
                .map_err(runtime_error)?;
        }
        self.started.store(true, Ordering::SeqCst);
        let weak = Arc::downgrade(self);
        let wake = self.wake.clone();
        let mut shutdown = self.shutdown.subscribe();
        let timer = tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = shutdown.changed() => break,
                    _ = wake.notified() => {
                        tokio::select! {
                            _ = shutdown.changed() => break,
                            _ = tokio::time::sleep(Duration::from_millis(250)) => {}
                        }
                    },
                    _ = tokio::time::sleep(Duration::from_secs(60)) => {}
                }
                let Some(runtime) = weak.upgrade() else {
                    break;
                };
                if let Err(error) = runtime.run_tick_once().await {
                    runtime.report(error);
                }
            }
        });
        *self.timer.lock().unwrap_or_else(|e| e.into_inner()) = Some(timer);
        Ok(())
    }
    pub fn poke(&self) {
        if self.started.load(Ordering::SeqCst) && !self.stopping.load(Ordering::SeqCst) {
            self.wake.notify_one();
        }
    }
    pub async fn run_tick_once(self: &Arc<Self>) -> Result<(), RuntimeError> {
        let _tick = self.tick_gate.lock().await;
        if self.stopping.load(Ordering::SeqCst) || !self.tasks.config().enabled {
            return Ok(());
        }
        self.reap(false).await;
        for store in self.tasks.stores().map_err(runtime_error)? {
            for snapshot in store.due(self.tasks.now_ms()).map_err(runtime_error)? {
                if self.active_run_count() >= self.tasks.config().max_concurrent_runs {
                    store
                        .delay(&snapshot, self.tasks.now_ms())
                        .map_err(runtime_error)?;
                    continue;
                }
                let run_id = uuid::Uuid::new_v4().to_string();
                if let Some(task) = store
                    .claim(&snapshot, &run_id, self.tasks.now_ms())
                    .map_err(runtime_error)?
                {
                    self.active
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert(
                            run_id.clone(),
                            ActiveRun {
                                project: store.project_key.clone(),
                                task: task.clone(),
                                stopped: false,
                                cancel: watch::channel(false).0,
                            },
                        );
                    let runtime = self.clone();
                    let store = store.clone();
                    let job = tokio::spawn(async move { runtime.fire(store, task, run_id).await });
                    self.jobs
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .push(job);
                }
            }
        }
        Ok(())
    }
    async fn reap(&self, all: bool) {
        let jobs = {
            let mut pending = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
            let mut jobs = vec![];
            let mut index = 0;
            while index < pending.len() {
                if all || pending[index].is_finished() {
                    jobs.push(pending.swap_remove(index));
                } else {
                    index += 1;
                }
            }
            jobs
        };
        for job in jobs {
            match job.await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => self.report(error),
                Err(error) => self.report(RuntimeError::new("cron_run_failed", error.to_string())),
            }
        }
    }
    pub async fn drain(&self) {
        self.reap(true).await;
    }
    pub async fn stop(&self) -> Result<(), RuntimeError> {
        let _lifecycle = self.lifecycle.lock().await;
        self.stopping.store(true, Ordering::SeqCst);
        self.shutdown.send_replace(true);
        let timer = self.timer.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(timer) = timer {
            let _ = timer.await;
        }
        let _tick = self.tick_gate.lock().await;
        let active = {
            let mut active = self.active.lock().unwrap_or_else(|e| e.into_inner());
            for run in active.values_mut() {
                run.stopped = true;
                run.cancel.send_replace(true);
            }
            active
                .iter()
                .map(|(id, run)| (id.clone(), run.task.clone()))
                .collect::<Vec<_>>()
        };
        for (id, task) in active {
            match tokio::time::timeout(
                Duration::from_secs(5),
                self.host
                    .abort(string(&task, "sessionKey"), &id, "system:cron_shutdown"),
            )
            .await
            {
                Ok(Ok(())) => {}
                Ok(Err(error)) => self.report(error),
                Err(_) => self.report(RuntimeError::new(
                    "cron_abort_failed",
                    "Native shutdown abort did not settle.",
                )),
            }
        }
        self.reap(true).await;
        for store in self.tasks.stores().map_err(runtime_error)? {
            for task in store.list_tasks().map_err(runtime_error)? {
                if let Err(error) = self.host.release_task(&task).await {
                    self.report(error);
                }
            }
        }
        Ok(())
    }
    pub(super) fn remove_active(&self, id: &str) -> Option<ActiveRun> {
        self.active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id)
    }
}

impl CronTaskHost for CronRuntime {
    fn prepare_task<'a>(&'a self, task: &'a Value) -> TaskFuture<'a, ()> {
        Box::pin(async move {
            if self.stopping.load(Ordering::SeqCst) {
                return Err(CronError::new(
                    "gateway_disposed",
                    "Cron runtime is stopped.",
                ));
            }
            self.host.prepare_task(task).await.map_err(task_error)
        })
    }
    fn release_task<'a>(&'a self, task: &'a Value) -> TaskFuture<'a, ()> {
        Box::pin(async move { self.host.release_task(task).await.map_err(task_error) })
    }
    fn stop_run<'a>(
        &'a self,
        project: &'a str,
        task_id: Option<&'a str>,
        run_id: Option<&'a str>,
    ) -> TaskFuture<'a, Option<Value>> {
        Box::pin(async move {
            let selected = {
                let mut active = self.active.lock().unwrap_or_else(|e| e.into_inner());
                active
                    .iter_mut()
                    .find(|(id, run)| {
                        run.project == project
                            && (task_id.is_some() || run_id.is_some())
                            && task_id.is_none_or(|id| string(&run.task, "taskId") == id)
                            && run_id.is_none_or(|wanted| id.as_str() == wanted)
                    })
                    .map(|(id, run)| {
                        run.stopped = true;
                        run.cancel.send_replace(true);
                        (id.clone(), run.task.clone())
                    })
            };
            let Some((id, task)) = selected else {
                return Ok(None);
            };
            tokio::time::timeout(
                Duration::from_secs(5),
                self.host.abort(string(&task, "sessionKey"), &id, "aborted"),
            )
            .await
            .map_err(|_| CronError::new("cron_abort_failed", "Native abort did not settle."))?
            .map_err(task_error)?;
            Ok(Some(
                json!({"taskId":task["taskId"],"runId":id,"scheduleType":task["schedule"]["type"]}),
            ))
        })
    }
    fn poke(&self) {
        CronRuntime::poke(self);
    }
}

pub(super) fn string<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or("")
}
pub(super) fn runtime_error(error: CronError) -> RuntimeError {
    RuntimeError::new(error.code, error.message)
}
fn task_error(error: RuntimeError) -> CronError {
    CronError::new(error.code, error.message)
}

// The finalizer is registered separately so panic/cancellation also releases capacity.
pub(super) struct ActiveGuard<'a>(pub &'a CronRuntime, pub String);
impl Drop for ActiveGuard<'_> {
    fn drop(&mut self) {
        self.0.remove_active(&self.1);
    }
}

impl CronRuntime {
    pub(super) async fn fire(
        self: Arc<Self>,
        store: CronStore,
        task: Value,
        run_id: String,
    ) -> Result<(), RuntimeError> {
        let _guard = ActiveGuard(&self, run_id.clone());
        super::fire::execute(&self, store, task, run_id).await
    }
}
