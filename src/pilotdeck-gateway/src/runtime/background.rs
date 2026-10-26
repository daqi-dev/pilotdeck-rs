#![cfg(unix)]
//! Background tasks (A6.5-pr12), ported from the fixed TS baseline:
//!
//! - `task/runtime/BackgroundTaskRuntime.ts` + `task/storage/TaskOutputStore.ts`
//!   and the POSIX half of
//!   `tool/execution-world/DetachedShellPort.ts`: detached shell tasks, their
//!   combined output buffer and read cursor, the pending → running →
//!   completed / failed / cancelled state machine, stop (SIGTERM → grace →
//!   SIGKILL to the process group) and the drain on dispose.
//! - `agent/sub/BackgroundSubagentRuntime.ts`: the lifecycle registry for
//!   background subagent forks. The fork body is injected (`run`); the agent
//!   loop that produces it is not part of this module.
//! - `cli/ProjectRuntimeRegistry.ts:1707-1751`: the SDK stop / list
//!   projections over both registries.
//!
//! Ownership: each started shell gets one supervisor task owned by this
//! runtime (not by a tool call). It reads both pipes, observes the leader
//! with `waitid(WNOWAIT)` so the group ID stays reserved until the exit is
//! recorded, then reaps. `dispose` stops every running task and waits for
//! each to settle; it is registered as a project resource group
//! ([`BackgroundResources`]). Like the baseline (`child.unref()`, T11), a
//! runtime dropped without `dispose` leaves its children running.
//!
//! Deliberate deviations (see the fixture README):
//! - Output spill is enabled by `disk_spill_dir`; `snapshot_store` adds
//!   restart recovery, including restored `unknown` tasks. Ledger:
//!   specs/gateway-parity/accepted-diffs.md A6.5-BACKGROUND-PERSIST-001 (unapproved).
//! - A spawn that Node reports asynchronously (ENOENT, EACCES, ...) never
//!   emits `exit` in the baseline, so its task stays `running` with no pid
//!   and `stop` / `dispose` wait forever (`probes/p1.out`). Here the visible
//!   state is the same until it is stopped; `stop` (and so `dispose`) then
//!   settles it at once as `cancelled` (interrupted, `endedAt` set, no exit
//!   code), fires its completion event and releases its waiters. There is no
//!   process to signal, so no grace period applies. Already human-approved:
//!   specs/parity/accepted-diffs.md #5 (`background-task-async-spawn`, 2026-10-21).
//!
//! Cancellation: `stop` marks the task interrupted and sends SIGTERM before
//! its first await; the grace → KILL escalation and the disposal body then
//! run as runtime-owned tasks. Dropping the caller's future (host abort,
//! operation deadline) does not cancel them, like the baseline's `rt.stop()`
//! / `dispose()` promises that keep running after the tool call is gone.
use crate::{
    js_value,
    process_group::wait_exit_unreaped,
    runtime::{
        project::{
            DisposeFuture, ProjectRuntime, ProjectRuntimeRegistry, RuntimeError, RuntimeResources,
        },
        tools::{
            ToolCancellation,
            builtin::bash::runner::{
                AbortSignal, SpawnError, classify_spawn_error, inherited_env, prepare_spawn,
                timer_delay,
            },
        },
    },
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use pilotdeck_transport::system_clock::format_epoch_iso;
use std::{
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    process::Stdio,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{io::AsyncReadExt, sync::watch};

mod output;
use output::OutputStore;

/// Epoch milliseconds (`options.now().getTime()`).
pub type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;
/// `onCompletion(event)`; a panicking handler never affects settlement.
pub type CompletionHandler = Arc<dyn Fn(JsObject) + Send + Sync>;

const DEFAULT_GRACE_MS: f64 = 5_000.0;
const DEFAULT_MAX_TASKS: usize = 32;
const DEFAULT_COMPLETION_PREVIEW_BYTES: f64 = 4_000.0;
const DEFAULT_MEMORY_BYTES: usize = 1_000_000;
/// After the leader exits, bytes already in the pipes are read before the
/// exit is recorded (Node delivers them first: `probes/p1.out`, 0/200).
const EXIT_DRAIN: Duration = Duration::from_millis(20);

fn system_now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |elapsed| elapsed.as_millis() as f64)
}

fn units(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

fn output_path(dir: &Path, task_id: &JsString) -> PathBuf {
    dir.join("output")
        .join(format!("{}.log", String::from_utf16_lossy(task_id.units())))
}

fn concat(parts: &[&[u16]]) -> JsString {
    JsString::from_utf16(parts.concat())
}

/// `toISOString()` of an epoch-millisecond `Date`.
pub fn iso(ms: f64) -> JsString {
    JsString::from(format_epoch_iso(ms as i64).as_str())
}

/// `===` for the primitive values these registries compare (ids, statuses).
pub(crate) fn strict_equals(left: &JsValue, right: &JsValue) -> bool {
    match (left, right) {
        (JsValue::Undefined, JsValue::Undefined) | (JsValue::Null, JsValue::Null) => true,
        (JsValue::Bool(a), JsValue::Bool(b)) => a == b,
        (JsValue::Number(a), JsValue::Number(b)) => a == b,
        (JsValue::String(a), JsValue::String(b)) => a == b,
        _ => false,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Cancelled,
    /// Restored after process loss; unreachable without snapshot persistence.
    Unknown,
}

impl TaskStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Unknown => "unknown",
        }
    }
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
    fn matches(self, value: &JsValue) -> bool {
        value
            .as_string()
            .is_some_and(|text| text.equals_str(self.as_str()))
    }
}

/// `PilotDeckBackgroundBashTask` (the fields this port uses). Values the
/// baseline passes through from tool input stay `JsValue`s.
#[derive(Clone, Debug)]
pub struct BackgroundBashTask {
    pub task_id: JsString,
    pub agent_id: JsValue,
    pub session_id: JsValue,
    pub kind: JsValue,
    pub command: JsValue,
    pub cwd: JsValue,
    pub pid: Option<u32>,
    pub status: TaskStatus,
    /// `None` is `undefined`; `Some(None)` is `null` (signal exit).
    pub exit_code: Option<Option<i32>>,
    pub interrupted: bool,
    pub started_at: f64,
    pub ended_at: Option<f64>,
    pub output_bytes: u64,
}

/// `PilotDeckTaskOutputSlice`.
#[derive(Clone, Debug, PartialEq)]
pub struct OutputSlice {
    pub content: String,
    pub next_offset: f64,
    pub total_bytes: f64,
    pub truncated: bool,
}

struct ChildState {
    pid: Option<rustix::process::Pid>,
    exited: bool,
}

struct EntryState {
    task: BackgroundBashTask,
    output: OutputStore,
    child: Option<ChildState>,
}

struct Entry {
    state: Mutex<EntryState>,
    done: watch::Sender<bool>,
    closed: watch::Sender<bool>,
    /// Node reported the spawn failure asynchronously: no `exit` will come.
    never_exits: bool,
}

impl Entry {
    fn append(&self, chunk: &[u8]) {
        let mut state = self.state.lock().unwrap();
        state.output.append(chunk);
        state.task.output_bytes = state.output.total;
    }
    fn snapshot(&self) -> BackgroundBashTask {
        self.state.lock().unwrap().task.clone()
    }
    /// `DetachedShellHandle.terminate(signal)`: the group, or the leader when
    /// no group remains; nothing once the exit has been observed.
    fn terminate(&self, signal: rustix::process::Signal) {
        let state = self.state.lock().unwrap();
        let Some(child) = &state.child else {
            return;
        };
        if child.exited {
            return;
        }
        if let Some(pid) = child.pid
            && rustix::process::kill_process_group(pid, signal).is_err()
        {
            let _ = rustix::process::kill_process(pid, signal);
        }
    }
    async fn wait_done(&self) {
        let mut done = self.done.subscribe();
        let _ = done.wait_for(|done| *done).await;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Active,
    Draining,
    Disposed,
}

impl Phase {
    fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Draining => "draining",
            Self::Disposed => "disposed",
        }
    }
}

struct RuntimeState {
    entries: Vec<(JsString, Arc<Entry>)>,
    /// Capacity reserved by starts that have not published their entry yet.
    pending_starts: usize,
    phase: Phase,
    dispose_started: bool,
}

/// `BackgroundTaskRuntimeOptions` with optional disk spill and snapshot store.
#[derive(Clone, Default)]
pub struct BackgroundTaskRuntimeOptions {
    /// Execution-world state directory; logs use `<dir>/output/<taskId>.log`.
    /// `None` preserves the memory-only runtime. No implicit user-home writes.
    pub disk_spill_dir: Option<std::path::PathBuf>,
    pub snapshot_store:
        Option<Arc<dyn crate::runtime::background_snapshot::BackgroundTaskSnapshotStore>>,
    pub now: Option<Clock>,
    pub max_tasks: Option<usize>,
    pub on_completion: Option<CompletionHandler>,
    pub completion_preview_bytes: Option<f64>,
}

struct Inner {
    state: Mutex<RuntimeState>,
    starts: watch::Sender<usize>,
    disposed: watch::Sender<bool>,
    now: Clock,
    max_tasks: usize,
    on_completion: Option<CompletionHandler>,
    completion_preview_bytes: f64,
    disk_spill_dir: Option<std::path::PathBuf>,
    snapshot_store:
        Option<Arc<dyn crate::runtime::background_snapshot::BackgroundTaskSnapshotStore>>,
}

/// `StartTaskSpec`. `env: None` inherits the gateway's environment, like
/// Node's `undefined`.
#[derive(Clone, Debug)]
pub struct StartTaskSpec {
    pub command: JsValue,
    pub cwd: JsString,
    pub env: Option<Vec<(JsString, JsString)>>,
    pub session_id: JsValue,
    pub agent_id: JsValue,
    pub kind: JsValue,
}

/// Errors thrown by the baseline runtime, as `Error.message` / `name`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackgroundTaskError {
    pub name: &'static str,
    pub message: JsString,
}

impl BackgroundTaskError {
    fn error(message: JsString) -> Self {
        Self {
            name: "Error",
            message,
        }
    }
    fn unknown_task(task_id: &JsString) -> Self {
        Self::error(concat(&[&units("Unknown taskId: "), task_id.units()]))
    }
}

/// `WaitTaskResult`.
#[derive(Clone, Debug)]
pub struct WaitTaskResult {
    pub task: BackgroundBashTask,
    pub timed_out: bool,
    pub outcome: &'static str,
    pub waited_ms: f64,
}

/// `PilotDeckBackgroundTaskListFilter`, as the tool forwards it.
#[derive(Clone, Debug)]
pub struct TaskListFilter {
    pub agent_id: JsValue,
    pub status: JsValue,
    pub kind: JsValue,
}

impl Default for TaskListFilter {
    fn default() -> Self {
        Self {
            agent_id: JsValue::Undefined,
            status: JsValue::Undefined,
            kind: JsValue::Undefined,
        }
    }
}

/// `BackgroundTaskAccess`: `None` is the owner's unrestricted view.
pub type TaskAccess<'a> = Option<&'a JsValue>;

#[derive(Clone)]
pub struct BackgroundTaskRuntime {
    inner: Arc<Inner>,
    #[cfg(test)]
    before_spawn: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl Default for BackgroundTaskRuntime {
    fn default() -> Self {
        Self::new(BackgroundTaskRuntimeOptions::default())
    }
}

fn truthy(value: &JsValue) -> bool {
    match value {
        JsValue::Undefined | JsValue::Null => false,
        JsValue::Bool(value) => *value,
        JsValue::Number(value) => *value != 0.0 && !value.is_nan(),
        JsValue::String(value) => !value.is_empty(),
        _ => true,
    }
}

impl BackgroundTaskRuntime {
    pub fn new(options: BackgroundTaskRuntimeOptions) -> Self {
        // Validate snapshot store requires disk spill
        if options.snapshot_store.is_some() && options.disk_spill_dir.is_none() {
            panic!("Background task durable recovery requires disk_spill_dir for output recovery.");
        }

        let runtime = Self {
            inner: Arc::new(Inner {
                state: Mutex::new(RuntimeState {
                    entries: Vec::new(),
                    pending_starts: 0,
                    phase: Phase::Active,
                    dispose_started: false,
                }),
                starts: watch::channel(0).0,
                disposed: watch::channel(false).0,
                now: options.now.unwrap_or_else(|| Arc::new(system_now)),
                max_tasks: options.max_tasks.unwrap_or(DEFAULT_MAX_TASKS),
                on_completion: options.on_completion,
                disk_spill_dir: options.disk_spill_dir,
                snapshot_store: options.snapshot_store,
                completion_preview_bytes: options
                    .completion_preview_bytes
                    .unwrap_or(DEFAULT_COMPLETION_PREVIEW_BYTES),
            }),
            #[cfg(test)]
            before_spawn: None,
        };

        // Restore snapshots if store is available
        runtime.restore_snapshots();
        runtime
    }

    fn find(&self, task_id: &JsString, access: TaskAccess<'_>) -> Option<Arc<Entry>> {
        let state = self.inner.state.lock().unwrap();
        let (_, entry) = state.entries.iter().find(|(id, _)| id == task_id)?;
        let entry = Arc::clone(entry);
        drop(state);
        let allowed = access.is_none_or(|session| {
            strict_equals(&entry.state.lock().unwrap().task.session_id, session)
        });
        allowed.then_some(entry)
    }

    /// `list(filter, access)` in insertion order.
    pub fn list(&self, filter: &TaskListFilter, access: TaskAccess<'_>) -> Vec<BackgroundBashTask> {
        let entries: Vec<Arc<Entry>> = self
            .inner
            .state
            .lock()
            .unwrap()
            .entries
            .iter()
            .map(|(_, entry)| Arc::clone(entry))
            .collect();
        let wanted: Option<Vec<JsValue>> = truthy(&filter.status).then(|| match &filter.status {
            JsValue::Array(items) => items.clone(),
            other => vec![other.clone()],
        });
        entries
            .iter()
            .map(|entry| entry.snapshot())
            .filter(|task| access.is_none_or(|session| strict_equals(&task.session_id, session)))
            .filter(|task| {
                !truthy(&filter.agent_id) || strict_equals(&task.agent_id, &filter.agent_id)
            })
            .filter(|task| !truthy(&filter.kind) || strict_equals(&task.kind, &filter.kind))
            .filter(|task| {
                wanted
                    .as_ref()
                    .is_none_or(|wanted| wanted.iter().any(|status| task.status.matches(status)))
            })
            .collect()
    }

    pub fn get(&self, task_id: &JsString, access: TaskAccess<'_>) -> Option<BackgroundBashTask> {
        self.find(task_id, access).map(|entry| entry.snapshot())
    }

    /// `getOutput(taskId, offset, maxBytes, access)`.
    pub fn get_output(
        &self,
        task_id: &JsString,
        offset: &JsValue,
        max_bytes: &JsValue,
        access: TaskAccess<'_>,
    ) -> Result<OutputSlice, BackgroundTaskError> {
        let entry = self
            .find(task_id, access)
            .ok_or_else(|| BackgroundTaskError::unknown_task(task_id))?;
        let state = entry.state.lock().unwrap();
        Ok(state.output.read_slice(offset, max_bytes))
    }

    /// `wait(taskId, { timeoutMs, abortSignal }, access)`.
    pub async fn wait(
        &self,
        task_id: &JsString,
        timeout_ms: &JsValue,
        abort: Option<AbortSignal>,
        access: TaskAccess<'_>,
    ) -> Result<Option<WaitTaskResult>, BackgroundTaskError> {
        let Some(entry) = self.find(task_id, access) else {
            return Ok(None);
        };
        let started = Instant::now();
        if entry.snapshot().status == TaskStatus::Unknown {
            return Ok(Some(WaitTaskResult {
                task: entry.snapshot(),
                timed_out: false,
                outcome: "unknown",
                waited_ms: started.elapsed().as_millis() as f64,
            }));
        }
        // `Math.max(0, Math.floor(options.timeoutMs ?? 0))`; NaN never times out.
        let requested = match timeout_ms {
            JsValue::Undefined | JsValue::Null => 0.0,
            value => js_value::to_number(value)
                .map_err(|_| BackgroundTaskError::error(js_value::StackOverflow::MESSAGE.into()))?
                .floor(),
        };
        let timeout = (requested > 0.0).then(|| timer_delay(requested));
        // Promise.race([done, timeout, abort]) settles with the first entry
        // that is already resolved, in that order.
        let outcome = tokio::select! {
            biased;
            () = entry.wait_done() => "completed",
            () = async { tokio::time::sleep(timeout.unwrap()).await }, if timeout.is_some() => "timeout",
            () = async { abort.as_ref().unwrap().wait().await }, if abort.is_some() => "aborted",
        };
        Ok(Some(WaitTaskResult {
            task: entry.snapshot(),
            timed_out: outcome != "completed",
            outcome,
            waited_ms: started.elapsed().as_millis() as f64,
        }))
    }

    /// `start(spec)`.
    pub async fn start(
        &self,
        spec: StartTaskSpec,
    ) -> Result<BackgroundBashTask, BackgroundTaskError> {
        let (task_id, entry) = {
            let mut state = self.inner.state.lock().unwrap();
            if state.phase != Phase::Active {
                return Err(BackgroundTaskError::error(JsString::from(
                    format!(
                        "Background task runtime is {}; refusing a new task.",
                        state.phase.as_str()
                    )
                    .as_str(),
                )));
            }
            let active = state
                .entries
                .iter()
                .filter(|(_, entry)| {
                    matches!(
                        entry.state.lock().unwrap().task.status,
                        TaskStatus::Pending | TaskStatus::Running
                    )
                })
                .count();
            if active + state.pending_starts >= self.inner.max_tasks {
                return Err(BackgroundTaskError::error(JsString::from(
                    format!(
                        "BackgroundTaskRuntime: max tasks ({}) exceeded.",
                        self.inner.max_tasks
                    )
                    .as_str(),
                )));
            }
            self.inner.starts.send_modify(|starts| *starts += 1);
            let task_id = JsString::from(uuid::Uuid::new_v4().to_string().as_str());
            let task = BackgroundBashTask {
                task_id: task_id.clone(),
                agent_id: spec.agent_id.clone(),
                session_id: spec.session_id.clone(),
                kind: match &spec.kind {
                    JsValue::Undefined | JsValue::Null => JsValue::from("bash"),
                    kind => kind.clone(),
                },
                command: spec.command.clone(),
                cwd: JsValue::String(spec.cwd.clone()),
                pid: None,
                status: TaskStatus::Pending,
                exit_code: None,
                interrupted: false,
                started_at: (self.inner.now)(),
                ended_at: None,
                output_bytes: 0,
            };
            state.pending_starts += 1;
            (task_id, task)
        };
        // Queue the initial metadata before spawning can produce output or
        // settle the task. Pending is recoverable as Unknown after a crash.
        let initial_snapshot = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.inner.snapshot_store.as_ref().map(|store| {
                use crate::runtime::background_snapshot::{
                    PersistedBackgroundTask, PersistedOutput, persist_task,
                };
                store.write(PersistedBackgroundTask {
                    task: persist_task(&entry),
                    output: PersistedOutput {
                        total_bytes: 0,
                        persisted: false,
                    },
                })
            })
        }));
        let initial_snapshot = match initial_snapshot {
            Ok(snapshot) => snapshot,
            Err(payload) => {
                // No child or entry exists yet. Release capacity before waking
                // dispose's start-drain observers, like TS start's finally.
                self.inner.state.lock().unwrap().pending_starts -= 1;
                self.inner.starts.send_modify(|starts| *starts -= 1);
                let message = payload
                    .downcast_ref::<&str>()
                    .copied()
                    .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
                    .unwrap_or("Background initial snapshot construction failed.");
                return Err(BackgroundTaskError::error(JsString::from(message)));
            }
        };
        #[cfg(test)]
        if let Some(before_spawn) = &self.before_spawn {
            before_spawn();
        }
        let result = self.spawn_task(task_id, entry, spec);
        self.inner.starts.send_modify(|starts| *starts -= 1);
        // Release the start reservation before awaiting: dropping the caller
        // must not strand dispose(), and the store owns the accepted write.
        if let Some(snapshot) = initial_snapshot {
            let _ = snapshot.await;
        }
        result
    }

    fn insert(&self, task_id: &JsString, entry: Entry) -> Arc<Entry> {
        let entry = Arc::new(entry);
        let mut state = self.inner.state.lock().unwrap();
        state.entries.push((task_id.clone(), Arc::clone(&entry)));
        state.pending_starts -= 1;
        entry
    }

    fn spawn_task(
        &self,
        task_id: JsString,
        task: BackgroundBashTask,
        spec: StartTaskSpec,
    ) -> Result<BackgroundBashTask, BackgroundTaskError> {
        // The child state is installed before the entry is published, so a
        // concurrent `stop` can never observe a running entry with no handle.
        let disk_spill_dir = self.inner.disk_spill_dir.clone();
        let new_entry = |task: BackgroundBashTask, never_exits: bool, child: Option<ChildState>| {
            let output = OutputStore::with_spill(
                DEFAULT_MEMORY_BYTES,
                disk_spill_dir
                    .as_ref()
                    .map(|dir| output_path(dir, &task.task_id)),
            );

            Entry {
                state: Mutex::new(EntryState {
                    task,
                    output,
                    child,
                }),
                done: watch::channel(false).0,
                closed: watch::channel(false).0,
                never_exits,
            }
        };
        let inherited;
        let resolve_env: &[(JsString, JsString)] = match &spec.env {
            Some(env) => env,
            None => {
                inherited = inherited_env();
                &inherited
            }
        };
        // `Err((asynchronous, message))`: Node either throws from `spawn()`
        // (the task fails at once) or returns a pid-less handle and reports
        // the error as an event.
        // Node `String()`s a non-string argv entry (`probes/p2.out`); the
        // task keeps the original value.
        let command = match &spec.command {
            JsValue::String(command) => Ok(command.clone()),
            other => js_value::to_string_units(other)
                .map(JsString::from_utf16)
                .map_err(|_| JsString::from(js_value::StackOverflow::MESSAGE)),
        };
        let spawned =
            command
                .and_then(|command| {
                    prepare_spawn(&command, &spec.cwd, spec.env.as_deref(), resolve_env)
                })
                .map_err(|message| (false, message))
                .and_then(|prepared| {
                    // PreparedSpawn installs the shared child-only signal reset.
                    prepared.command(Stdio::null()).spawn().map_err(|error| {
                        match classify_spawn_error(&error, &prepared.shell) {
                            SpawnError::Async(message) => (true, message),
                            SpawnError::Sync(message) => (false, message),
                        }
                    })
                });
        match spawned {
            Err((true, message)) => {
                let mut task = task;
                task.status = TaskStatus::Running;
                let entry = self.insert(
                    &task_id,
                    new_entry(
                        task,
                        true,
                        Some(ChildState {
                            pid: None,
                            exited: false,
                        }),
                    ),
                );
                // `start` resolves before the error event lands.
                let snapshot = entry.snapshot();
                let text = concat(&[&units("error: "), message.units(), &units("\n")]);
                entry.append(String::from_utf16_lossy(text.units()).as_bytes());
                Ok(snapshot)
            }
            Err((false, message)) => {
                let mut task = task;
                task.status = TaskStatus::Failed;
                task.ended_at = Some((self.inner.now)());
                let entry = self.insert(&task_id, new_entry(task, false, None));
                let text = concat(&[&units("spawn error: "), message.units(), &units("\n")]);
                entry.append(String::from_utf16_lossy(text.units()).as_bytes());
                self.inner.settle(&entry);
                Ok(entry.snapshot())
            }
            Ok(mut child) => {
                let pid = child.id();
                let leader = pid.and_then(|pid| rustix::process::Pid::from_raw(pid as i32));
                let mut task = task;
                task.status = TaskStatus::Running;
                task.pid = pid;
                let entry = self.insert(
                    &task_id,
                    new_entry(
                        task,
                        false,
                        Some(ChildState {
                            pid: leader,
                            exited: false,
                        }),
                    ),
                );
                let stdout = child.stdout.take();
                let stderr = child.stderr.take();
                tokio::spawn(supervise(
                    Arc::clone(&self.inner),
                    Arc::clone(&entry),
                    child,
                    leader,
                    stdout,
                    stderr,
                ));
                Ok(entry.snapshot())
            }
        }
    }

    /// `stop(taskId, { graceMs }, access)`: SIGTERM, wait `graceMs`, SIGKILL.
    pub async fn stop(
        &self,
        task_id: &JsString,
        grace_ms: &JsValue,
        access: TaskAccess<'_>,
    ) -> Result<(), BackgroundTaskError> {
        let entry = self
            .find(task_id, access)
            .ok_or_else(|| BackgroundTaskError::unknown_task(task_id))?;
        {
            let mut state = entry.state.lock().unwrap();
            if state.task.status == TaskStatus::Unknown {
                return Err(BackgroundTaskError {
                    name: "BackgroundTaskStateUnknownError",
                    message: concat(&[
                        &units("Task "),
                        task_id.units(),
                        &units(" has an unknown restored state and cannot be waited or stopped."),
                    ]),
                });
            }
            if state.task.status != TaskStatus::Running || state.child.is_none() {
                return Ok(());
            }
            state.task.interrupted = true;
        }
        let grace = match grace_ms {
            JsValue::Undefined | JsValue::Null => DEFAULT_GRACE_MS,
            value => js_value::to_number(value).unwrap_or(f64::NAN),
        };
        if entry.never_exits {
            // Deliberate deviation (module docs): no exit will ever come.
            let escalation = tokio::spawn(settle_never_exits(Arc::clone(&self.inner), entry));
            let _ = escalation.await;
            return Ok(());
        }
        // Like the baseline, TERM is sent while the caller is running; the
        // grace → KILL escalation then lives in a runtime-owned task, so
        // dropping this future (host abort / operation deadline) cannot
        // cancel it.
        entry.terminate(rustix::process::Signal::TERM);
        let escalation = tokio::spawn(escalate(entry, grace));
        let _ = escalation.await;
        Ok(())
    }

    /// `killForAgent(agentId)`.
    pub async fn kill_for_agent(&self, agent_id: &JsValue) {
        let targets = self.running_ids(|task| strict_equals(&task.agent_id, agent_id));
        self.stop_all(targets).await;
    }

    /// `killAll()`.
    pub async fn kill_all(&self) {
        let targets = self.running_ids(|_| true);
        self.stop_all(targets).await;
    }

    fn running_ids(&self, select: impl Fn(&BackgroundBashTask) -> bool) -> Vec<JsString> {
        let state = self.inner.state.lock().unwrap();
        state
            .entries
            .iter()
            .filter(|(_, entry)| {
                let entry = entry.state.lock().unwrap();
                entry.task.status == TaskStatus::Running && select(&entry.task)
            })
            .map(|(id, _)| id.clone())
            .collect()
    }

    async fn stop_all(&self, targets: Vec<JsString>) {
        let mut stops = tokio::task::JoinSet::new();
        for task_id in targets {
            let runtime = self.clone();
            stops.spawn(async move {
                let _ = runtime.stop(&task_id, &JsValue::Undefined, None).await;
            });
        }
        while stops.join_next().await.is_some() {}
    }

    fn restore_snapshots(&self) {
        use crate::runtime::background_snapshot::restore_task;

        let store = match &self.inner.snapshot_store {
            Some(store) => store,
            None => return,
        };

        let disk_spill_dir = match &self.inner.disk_spill_dir {
            Some(dir) => dir,
            None => return,
        };

        let snapshots = store.load();
        let mut state = self.inner.state.lock().unwrap();

        for snapshot in snapshots {
            let task = match restore_task(&snapshot.task) {
                Ok(task) => task,
                Err(_) => continue, // Skip invalid snapshots
            };

            let was_unsettled = matches!(task.status, TaskStatus::Pending | TaskStatus::Running);
            let mut task = task;

            if was_unsettled {
                task.status = TaskStatus::Unknown;
                task.pid = None;
                task.exit_code = None;
                task.ended_at = None;
            }

            // Restore output from spill file
            let output = if snapshot.output.persisted {
                match OutputStore::restore_from_spill(
                    DEFAULT_MEMORY_BYTES,
                    output_path(disk_spill_dir, &task.task_id),
                    snapshot.output.total_bytes,
                ) {
                    Ok(store) => store,
                    // A missing or incomplete log cannot recover this snapshot.
                    Err(_) => continue,
                }
            } else {
                OutputStore::with_spill(DEFAULT_MEMORY_BYTES, None)
            };

            let entry = Arc::new(Entry {
                state: Mutex::new(EntryState {
                    task,
                    output,
                    child: None,
                }),
                done: watch::channel(true).0,
                closed: watch::channel(false).0,
                never_exits: false,
            });

            state.entries.push((
                entry.state.lock().unwrap().task.task_id.clone(),
                entry.clone(),
            ));

            // Best-effort persist the reconciliation
            if was_unsettled {
                let store = Arc::clone(store);
                let entry_clone = Arc::clone(&entry);
                tokio::spawn(async move {
                    let _ = Self::persist_entry_static(&store, &entry_clone).await;
                });
            }
        }
    }

    async fn persist_entry_static(
        store: &Arc<dyn crate::runtime::background_snapshot::BackgroundTaskSnapshotStore>,
        entry: &Arc<Entry>,
    ) -> std::io::Result<()> {
        use crate::runtime::background_snapshot::{
            PersistedBackgroundTask, PersistedOutput, persist_task,
        };

        let flushed = entry.state.lock().unwrap().output.flush();
        flushed.await;
        let (task, total_bytes, has_durable) = {
            let state = entry.state.lock().unwrap();
            (
                state.task.clone(),
                state.output.total,
                state.output.has_durable_output(),
            )
        };

        if total_bytes > 0 && !has_durable {
            return Err(std::io::Error::other(
                "Background task output is not fully persisted.",
            ));
        }

        let snapshot = PersistedBackgroundTask {
            task: persist_task(&task),
            output: PersistedOutput {
                total_bytes,
                persisted: has_durable,
            },
        };

        store.write(snapshot).await
    }

    #[allow(dead_code)]
    async fn persist_entry(&self, entry: &Arc<Entry>) {
        if let Some(store) = &self.inner.snapshot_store {
            let _ = Self::persist_entry_static(store, entry).await;
        }
    }

    /// `dispose()`: refuse new tasks, settle accepted starts, stop every
    /// running task and wait for all to settle, then release the buffers.
    /// Concurrent calls share one disposal.
    pub async fn dispose(&self) {
        let first = {
            let mut state = self.inner.state.lock().unwrap();
            let first = !state.dispose_started;
            state.dispose_started = true;
            if state.phase == Phase::Active {
                state.phase = Phase::Draining;
            }
            first
        };
        if first {
            // Runtime-owned: a dropped first caller must not leave
            // `dispose_started` set with no disposal running.
            let runtime = self.clone();
            drop(tokio::spawn(async move { runtime.dispose_now().await }));
        }
        let mut disposed = self.inner.disposed.subscribe();
        let _ = disposed.wait_for(|disposed| *disposed).await;
    }

    async fn dispose_now(&self) {
        {
            let mut starts = self.inner.starts.subscribe();
            let _ = starts.wait_for(|starts| *starts == 0).await;
            self.kill_all().await;
            let entries: Vec<Arc<Entry>> = self
                .inner
                .state
                .lock()
                .unwrap()
                .entries
                .iter()
                .map(|(_, entry)| Arc::clone(entry))
                .collect();
            for entry in &entries {
                entry.wait_done().await;
            }
            // Freeze accepted output before taking each write-chain snapshot.
            let flush_tasks: Vec<_> = entries
                .iter()
                .map(|entry| entry.state.lock().unwrap().output.begin_close())
                .collect();
            for flushed in flush_tasks {
                flushed.await;
            }
            // Close outputs
            for entry in &entries {
                entry.state.lock().unwrap().output.close();
                entry.closed.send_replace(true);
            }

            // Persist the final, flushed state without relying on spawned-task timing.
            if let Some(store) = &self.inner.snapshot_store {
                for entry in &entries {
                    let _ = Self::persist_entry_static(store, entry).await;
                }
                let _ = store.flush().await;
            }

            self.inner.state.lock().unwrap().phase = Phase::Disposed;
            self.inner.disposed.send_replace(true);
        }
    }
}

/// Runtime-owned tail of `stop`: wait `grace`, SIGKILL the group, then wait
/// for the exit to be recorded. It runs in its own task so dropping the
/// caller's future cannot cancel the escalation.
async fn escalate(entry: Arc<Entry>, grace: f64) {
    tokio::select! {
        biased;
        () = entry.wait_done() => {}
        () = tokio::time::sleep(timer_delay(grace)) => {
            entry.terminate(rustix::process::Signal::KILL);
        }
    }
    entry.wait_done().await;
}

/// Runtime-owned tail of `stop` for a task whose spawn error Node reports
/// asynchronously: there is no process to signal and no `exit` will ever
/// arrive, so settle it as cancelled instead of hanging (module docs).
async fn settle_never_exits(inner: Arc<Inner>, entry: Arc<Entry>) {
    {
        let mut state = entry.state.lock().unwrap();
        if state.task.status != TaskStatus::Running {
            return;
        }
        state.task.status = TaskStatus::Cancelled;
        state.task.ended_at = Some((inner.now)());
    }
    inner.settle(&entry);
}

impl Inner {
    /// `settleEntry` + `notifyCompletion`, then resolve `done`.
    fn settle(&self, entry: &Arc<Entry>) {
        let (event, should_persist) = {
            let mut state = entry.state.lock().unwrap();
            state.task.output_bytes = state.output.total;
            let task = &state.task;
            let should_persist = task.status.is_terminal();
            let event = match task.ended_at {
                Some(ended_at) if task.status.is_terminal() => {
                    let total = state.output.total as f64;
                    let preview_bytes = self.completion_preview_bytes.max(0.0);
                    let slice = state.output.read_slice(
                        &JsValue::Number((total - preview_bytes).max(0.0)),
                        &JsValue::Number(preview_bytes),
                    );
                    let mut event = JsObject::new();
                    event.insert("taskId".into(), JsValue::String(task.task_id.clone()));
                    event.insert("sessionId".into(), task.session_id.clone());
                    event.insert("status".into(), JsValue::from(task.status.as_str()));
                    event.insert("exitCode".into(), exit_code_value(task.exit_code));
                    event.insert(
                        "outputPreview".into(),
                        JsValue::from(slice.content.as_str()),
                    );
                    event.insert("totalBytes".into(), JsValue::Number(total));
                    event.insert("startedAt".into(), JsValue::String(iso(task.started_at)));
                    event.insert("endedAt".into(), JsValue::String(iso(ended_at)));
                    Some(event)
                }
                _ => None,
            };
            (event, should_persist)
        };
        if let (Some(event), Some(handler)) = (event, &self.on_completion) {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handler(event)));
        }
        // Persist terminal state after completion notification
        // Spawn as fire-and-forget; dispose() will flush the snapshot store
        if should_persist && let Some(store) = &self.snapshot_store {
            let store = Arc::clone(store);
            let entry = Arc::clone(entry);
            tokio::spawn(async move {
                // Call write() to queue the persist, it will be flushed on dispose
                let _ = BackgroundTaskRuntime::persist_entry_static(&store, &entry).await;
            });
        }
        entry.done.send_replace(true);
    }
}

/// `exitCode` as a wire value: `undefined`, `null` or the number.
pub fn exit_code_value(exit_code: Option<Option<i32>>) -> JsValue {
    match exit_code {
        None => JsValue::Undefined,
        Some(None) => JsValue::Null,
        Some(Some(code)) => JsValue::Number(f64::from(code)),
    }
}

async fn read_chunk<R: tokio::io::AsyncRead + Unpin>(
    stream: &mut Option<R>,
    buffer: &mut [u8],
) -> Option<usize> {
    match stream.as_mut()?.read(buffer).await {
        Ok(length) if length > 0 => Some(length),
        _ => {
            *stream = None;
            None
        }
    }
}

/// Reads what is already buffered in a pipe, bounded by `deadline`.
async fn drain<R: tokio::io::AsyncRead + Unpin>(
    stream: &mut Option<R>,
    buffer: &mut [u8],
    entry: &Entry,
    deadline: tokio::time::Instant,
) {
    while stream.is_some() {
        match tokio::time::timeout_at(deadline, read_chunk(stream, buffer)).await {
            Ok(Some(length)) => entry.append(&buffer[..length]),
            Ok(None) | Err(_) => return,
        }
    }
}

async fn supervise(
    inner: Arc<Inner>,
    entry: Arc<Entry>,
    mut child: tokio::process::Child,
    leader: Option<rustix::process::Pid>,
    mut stdout: Option<tokio::process::ChildStdout>,
    mut stderr: Option<tokio::process::ChildStderr>,
) {
    let mut closed = entry.closed.subscribe();
    let mut out_buffer = vec![0u8; 64 * 1024];
    let mut err_buffer = vec![0u8; 64 * 1024];
    let mut exited = false;
    loop {
        if exited && stdout.is_none() && stderr.is_none() {
            break;
        }
        tokio::select! {
            biased;
            () = async { let _ = closed.wait_for(|closed| *closed).await; } => break,
            length = read_chunk(&mut stdout, &mut out_buffer), if stdout.is_some() => {
                if let Some(length) = length {
                    entry.append(&out_buffer[..length]);
                }
            }
            length = read_chunk(&mut stderr, &mut err_buffer), if stderr.is_some() => {
                if let Some(length) = length {
                    entry.append(&err_buffer[..length]);
                }
            }
            observed = async {
                match leader {
                    Some(leader) => wait_exit_unreaped(leader).await.ok().flatten(),
                    None => None,
                }
            }, if !exited => {
                let deadline = tokio::time::Instant::now() + EXIT_DRAIN;
                drain(&mut stdout, &mut out_buffer, &entry, deadline).await;
                drain(&mut stderr, &mut err_buffer, &entry, deadline).await;
                exited = true;
                // Recorded before reaping: `terminate` never signals a group
                // ID that could be reused after the wait below.
                if let Some(state) = entry.state.lock().unwrap().child.as_mut() {
                    state.exited = true;
                }
                let status = child.wait().await.ok();
                let (code, signal) = match observed {
                    Some(exit) => (exit.code, exit.signal),
                    None => {
                        use std::os::unix::process::ExitStatusExt;
                        (
                            status.and_then(|status| status.code()),
                            status.and_then(|status| status.signal()),
                        )
                    }
                };
                {
                    let mut state = entry.state.lock().unwrap();
                    let task = &mut state.task;
                    task.ended_at = Some((inner.now)());
                    task.exit_code = Some(code);
                    let killed = matches!(signal, Some(libc::SIGTERM | libc::SIGKILL));
                    task.status = if task.interrupted || killed {
                        TaskStatus::Cancelled
                    } else if code == Some(0) {
                        TaskStatus::Completed
                    } else {
                        TaskStatus::Failed
                    };
                }
                inner.settle(&entry);
            }
        }
    }
}

// ---- background subagents ----------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubagentKind {
    Background,
    Observer,
}

impl SubagentKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Background => "background",
            Self::Observer => "observer",
        }
    }
}

/// `BackgroundSubagentTask`.
#[derive(Clone, Debug)]
pub struct BackgroundSubagentTask {
    pub task_id: JsString,
    pub kind: SubagentKind,
    pub session_id: JsString,
    pub parent_turn_id: JsString,
    pub subagent_id: JsString,
    pub subagent_type: JsString,
    pub observed_subagent_id: Option<JsString>,
    pub status: TaskStatus,
    pub started_at: f64,
    pub ended_at: Option<f64>,
    pub error: Option<JsString>,
}

pub type SubagentFuture = Pin<Box<dyn Future<Output = Result<(), JsString>> + Send>>;
/// The fork body: `run(signal)`; `Err` is the rejection's message.
pub type SubagentRun = Box<dyn FnOnce(ToolCancellation) -> SubagentFuture + Send>;

/// `StartBackgroundSubagentInput`.
pub struct StartBackgroundSubagentInput {
    pub kind: Option<SubagentKind>,
    pub session_id: JsString,
    pub parent_turn_id: JsString,
    pub subagent_id: JsString,
    pub subagent_type: JsString,
    pub observed_subagent_id: Option<JsString>,
    pub run: SubagentRun,
}

struct SubagentEntry {
    task: Mutex<BackgroundSubagentTask>,
    controller: ToolCancellation,
    done: watch::Sender<bool>,
}

/// Filter for [`BackgroundSubagentRuntime::list`].
#[derive(Clone, Debug, Default)]
pub struct SubagentListFilter {
    pub session_id: Option<JsString>,
    pub kind: Option<SubagentKind>,
    pub status: Option<Vec<TaskStatus>>,
}

/// `BackgroundSubagentRuntime`.
#[derive(Clone)]
pub struct BackgroundSubagentRuntime {
    entries: Arc<Mutex<Vec<Arc<SubagentEntry>>>>,
    now: Clock,
    #[cfg(test)]
    before_cancel: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl Default for BackgroundSubagentRuntime {
    fn default() -> Self {
        Self::new(None)
    }
}

impl BackgroundSubagentRuntime {
    /// `now` stands in for `new Date()` (not injectable in the baseline).
    pub fn new(now: Option<Clock>) -> Self {
        Self {
            entries: Arc::default(),
            now: now.unwrap_or_else(|| Arc::new(system_now)),
            #[cfg(test)]
            before_cancel: None,
        }
    }

    /// `start(input)`: the task is `pending` until the body is first polled,
    /// as the baseline defers it to a microtask.
    pub fn start(&self, input: StartBackgroundSubagentInput) -> BackgroundSubagentTask {
        let task = BackgroundSubagentTask {
            task_id: JsString::from(uuid::Uuid::new_v4().to_string().as_str()),
            kind: input.kind.unwrap_or(SubagentKind::Background),
            session_id: input.session_id,
            parent_turn_id: input.parent_turn_id,
            subagent_id: input.subagent_id,
            subagent_type: input.subagent_type,
            observed_subagent_id: input.observed_subagent_id.filter(|id| !id.is_empty()),
            status: TaskStatus::Pending,
            started_at: (self.now)(),
            ended_at: None,
            error: None,
        };
        let entry = Arc::new(SubagentEntry {
            task: Mutex::new(task.clone()),
            controller: ToolCancellation::default(),
            done: watch::channel(false).0,
        });
        self.entries.lock().unwrap().push(Arc::clone(&entry));
        let now = Arc::clone(&self.now);
        let run = input.run;
        tokio::spawn(async move {
            let body = {
                let mut task = entry.task.lock().unwrap();
                if task.status == TaskStatus::Cancelled {
                    None
                } else {
                    task.status = TaskStatus::Running;
                    Some(run(entry.controller.clone()))
                }
            };
            if let Some(body) = body {
                // A panicking body settles as a failure instead of leaving
                // `stopAndWait` waiting forever.
                let result = match tokio::spawn(body).await {
                    Ok(result) => result,
                    Err(_) => Err(JsString::from("Background subagent task panicked.")),
                };
                let mut task = entry.task.lock().unwrap();
                match result {
                    Ok(()) => {
                        // `stop` commits cancellation under this same lock
                        // before signalling the body. Preserve that decision
                        // even when the signal has not been delivered yet.
                        if task.status != TaskStatus::Cancelled {
                            task.status = TaskStatus::Completed;
                        }
                    }
                    Err(message) => {
                        if task.status != TaskStatus::Cancelled {
                            task.status = if entry.controller.is_cancelled() {
                                TaskStatus::Cancelled
                            } else {
                                TaskStatus::Failed
                            };
                            task.error = Some(message);
                        }
                    }
                }
            }
            {
                let mut task = entry.task.lock().unwrap();
                if task.ended_at.is_none() {
                    task.ended_at = Some(now());
                }
            }
            entry.done.send_replace(true);
        });
        task
    }

    fn find(&self, task_id: &JsString) -> Option<Arc<SubagentEntry>> {
        self.entries
            .lock()
            .unwrap()
            .iter()
            .find(|entry| entry.task.lock().unwrap().task_id == *task_id)
            .cloned()
    }

    pub fn get(&self, task_id: &JsString) -> Option<BackgroundSubagentTask> {
        self.find(task_id)
            .map(|entry| entry.task.lock().unwrap().clone())
    }

    pub fn list(&self, filter: &SubagentListFilter) -> Vec<BackgroundSubagentTask> {
        self.entries
            .lock()
            .unwrap()
            .iter()
            .map(|entry| entry.task.lock().unwrap().clone())
            .filter(|task| {
                filter
                    .session_id
                    .as_ref()
                    .is_none_or(|id| *id == task.session_id)
                    && filter.kind.is_none_or(|kind| kind == task.kind)
                    && filter
                        .status
                        .as_ref()
                        .is_none_or(|statuses| statuses.contains(&task.status))
            })
            .collect()
    }

    pub fn stop(&self, task_id: &JsString) -> bool {
        let Some(entry) = self.find(task_id) else {
            return false;
        };
        let mut task = entry.task.lock().unwrap();
        if !matches!(task.status, TaskStatus::Pending | TaskStatus::Running) {
            return false;
        }
        task.status = TaskStatus::Cancelled;
        task.ended_at = Some((self.now)());
        drop(task);
        #[cfg(test)]
        if let Some(before_cancel) = &self.before_cancel {
            before_cancel();
        }
        entry.controller.cancel_synchronously();
        true
    }

    pub async fn stop_and_wait(&self, task_id: &JsString) -> bool {
        let Some(entry) = self.find(task_id) else {
            return false;
        };
        let stopped = self.stop(task_id);
        let mut done = entry.done.subscribe();
        let _ = done.wait_for(|done| *done).await;
        stopped
    }

    pub fn shutdown(&self) {
        let ids: Vec<JsString> = self
            .entries
            .lock()
            .unwrap()
            .iter()
            .map(|entry| entry.task.lock().unwrap().task_id.clone())
            .collect();
        for task_id in ids {
            self.stop(&task_id);
        }
    }
}

// ---- project resources and SDK projections -------------------------------------

/// The project-scoped pair `runtime.backgroundSubagents` and
/// `runtime.executionWorld.backgroundTasks`. Disposal follows
/// `disposeRuntime`: subagents are shut down, then shell tasks are drained.
#[derive(Clone, Default)]
pub struct BackgroundResources {
    pub tasks: BackgroundTaskRuntime,
    pub subagents: BackgroundSubagentRuntime,
}

impl RuntimeResources for BackgroundResources {
    fn shutdown(&self) {
        self.subagents.shutdown();
    }
    fn dispose(&self) -> DisposeFuture<'_> {
        Box::pin(async move {
            self.subagents.shutdown();
            self.tasks.dispose().await;
            Ok(())
        })
    }
}

/// A rejection from the SDK projections, as `{ name, code?, message,
/// requires_host }`.
///
/// The SDK methods (`ProjectRuntimeRegistry.ts:1707-1751`) do not catch a
/// failed `this.resolve(...)`: the original error object reaches the caller.
/// The memory family is the last resolve stage, so a throwing memory factory
/// (`DialogGatewayError`, `requires_host`) must survive this boundary with its
/// `name`/`code`/UTF-16 `message`/`requires_host` intact, exactly like the
/// memory projection (`From<RuntimeError> for MemoryProviderError`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackgroundProjectionError {
    pub name: String,
    pub code: Option<String>,
    pub message: JsString,
    pub requires_host: bool,
}

impl From<BackgroundTaskError> for BackgroundProjectionError {
    fn from(error: BackgroundTaskError) -> Self {
        Self {
            name: error.name.into(),
            code: None,
            message: error.message,
            requires_host: false,
        }
    }
}

impl From<RuntimeError> for BackgroundProjectionError {
    /// Keep the JS shape retained on `RuntimeError.js_error`; a plain runtime
    /// error keeps the ordinary `Error` shape and its display message.
    fn from(error: RuntimeError) -> Self {
        let RuntimeError {
            message, js_error, ..
        } = error;
        match js_error {
            Some(error) => Self {
                name: error.name,
                code: error.code,
                message: error.message,
                requires_host: error.requires_host,
            },
            None => Self {
                name: "Error".into(),
                code: None,
                message: JsString::from(message),
                requires_host: false,
            },
        }
    }
}

fn object<const N: usize>(entries: [(&str, JsValue); N]) -> JsObject {
    let mut result = JsObject::new();
    for (key, value) in entries {
        result.insert(key.into(), value);
    }
    result
}

/// `stopBackgroundTaskForSdk` on a resolved project runtime.
pub async fn stop_background_task_for_sdk(
    resources: &BackgroundResources,
    session_key: &JsString,
    task_id: &JsString,
) -> Result<JsObject, BackgroundProjectionError> {
    if let Some(task) = resources.subagents.get(task_id)
        && task.kind == SubagentKind::Background
        && task.session_id == *session_key
    {
        let stopped = resources.subagents.stop_and_wait(task_id).await;
        let mut result = object([("stopped", JsValue::Bool(stopped))]);
        if let Some(status) = resources.subagents.get(task_id).map(|task| task.status)
            && status != TaskStatus::Unknown
        {
            result.insert("status".into(), JsValue::from(status.as_str()));
        }
        return Ok(result);
    }
    let owned = resources
        .tasks
        .get(task_id, None)
        .is_some_and(|task| strict_equals(&task.session_id, &JsValue::String(session_key.clone())));
    if !owned {
        return Err(BackgroundProjectionError {
            name: "DialogGatewayError".into(),
            code: Some("BACKGROUND_TASK_NOT_FOUND".into()),
            message: concat(&[
                &units("Background task "),
                task_id.units(),
                &units(" is not owned by session "),
                session_key.units(),
                &units("."),
            ]),
            requires_host: false,
        });
    }
    resources
        .tasks
        .stop(task_id, &JsValue::Undefined, None)
        .await?;
    let mut result = object([("stopped", JsValue::Bool(true))]);
    if let Some(task) = resources.tasks.get(task_id, None)
        && task.status != TaskStatus::Unknown
    {
        result.insert("status".into(), JsValue::from(task.status.as_str()));
    }
    Ok(result)
}

/// `backgroundTasksForSdk` on a resolved project runtime. `task_id` is the
/// input value (`undefined` when absent); only a truthy value selects a task.
pub fn background_tasks_for_sdk(
    resources: &BackgroundResources,
    session_key: &JsString,
    task_id: &JsValue,
) -> JsObject {
    let subagent_tasks = resources.subagents.list(&SubagentListFilter {
        session_id: Some(session_key.clone()),
        kind: Some(SubagentKind::Background),
        status: Some(vec![TaskStatus::Pending, TaskStatus::Running]),
    });
    let selected = truthy(task_id);
    let is = |id: &JsString| strict_equals(&JsValue::String(id.clone()), task_id);
    if selected && subagent_tasks.iter().any(|task| is(&task.task_id)) {
        return object([
            ("backgrounded", JsValue::Bool(true)),
            ("taskIds", JsValue::Array(vec![task_id.clone()])),
        ]);
    }
    if !selected && !subagent_tasks.is_empty() {
        return object([
            ("backgrounded", JsValue::Bool(true)),
            (
                "taskIds",
                JsValue::Array(
                    subagent_tasks
                        .into_iter()
                        .map(|task| JsValue::String(task.task_id))
                        .collect(),
                ),
            ),
        ]);
    }
    let session = JsValue::String(session_key.clone());
    let running: Vec<BackgroundBashTask> = resources
        .tasks
        .list(
            &TaskListFilter {
                status: JsValue::from("running"),
                ..TaskListFilter::default()
            },
            None,
        )
        .into_iter()
        .filter(|task| strict_equals(&task.session_id, &session))
        .collect();
    let not_found = || {
        object([
            ("backgrounded", JsValue::Bool(false)),
            ("reason", JsValue::from("task_not_found")),
        ])
    };
    if selected && !running.iter().any(|task| is(&task.task_id)) {
        return not_found();
    }
    if !running.is_empty() || !selected {
        return object([
            ("backgrounded", JsValue::Bool(false)),
            ("reason", JsValue::from("no_foreground_tasks")),
        ]);
    }
    not_found()
}

fn project_resources(
    runtime: &ProjectRuntime,
) -> Result<&BackgroundResources, BackgroundProjectionError> {
    runtime
        .resources()
        .get::<BackgroundResources>()
        .ok_or_else(|| BackgroundProjectionError {
            name: "Error".into(),
            code: None,
            message: JsString::from("Project runtime has no background task resources."),
            requires_host: false,
        })
}

/// `ProjectRuntimeRegistry.stopBackgroundTaskForSdk`: resolve, then project.
pub async fn stop_background_task_in_registry(
    registry: &ProjectRuntimeRegistry,
    project_key: Option<&Path>,
    session_key: &JsString,
    task_id: &JsString,
) -> Result<JsObject, BackgroundProjectionError> {
    let runtime = registry.resolve(project_key)?;
    stop_background_task_for_sdk(project_resources(&runtime)?, session_key, task_id).await
}

/// `ProjectRuntimeRegistry.backgroundTasksForSdk`: resolve, then project.
pub fn background_tasks_in_registry(
    registry: &ProjectRuntimeRegistry,
    project_key: Option<&Path>,
    session_key: &JsString,
    task_id: &JsValue,
) -> Result<JsObject, BackgroundProjectionError> {
    let runtime = registry.resolve(project_key)?;
    Ok(background_tasks_for_sdk(
        project_resources(&runtime)?,
        session_key,
        task_id,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ConstructionSnapshot {
        first_write: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    }
    impl crate::runtime::background_snapshot::BackgroundTaskSnapshotStore for ConstructionSnapshot {
        fn load(&self) -> Vec<crate::runtime::background_snapshot::PersistedBackgroundTask> {
            vec![]
        }
        fn write(
            &self,
            _: crate::runtime::background_snapshot::PersistedBackgroundTask,
        ) -> Pin<Box<dyn Future<Output = std::io::Result<()>> + Send + '_>> {
            let first = self.first_write.lock().unwrap().take();
            if let Some(first) = first {
                first();
            }
            Box::pin(async { Ok(()) })
        }
        fn flush(&self) -> Pin<Box<dyn Future<Output = std::io::Result<()>> + Send + '_>> {
            Box::pin(async { Ok(()) })
        }
    }

    fn construction_spec() -> StartTaskSpec {
        StartTaskSpec {
            // An invalid command exercises admission without creating a child.
            command: "\0".into(),
            cwd: "/".into(),
            env: None,
            session_id: "s".into(),
            agent_id: "a".into(),
            kind: JsValue::Undefined,
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn initial_snapshot_construction_panic_releases_all_reservations() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let oracle_text = include_str!("../../tests/fixtures/runtime-spawn-panic/oracle.json");
        crate::test_baseline::oracle(oracle_text);
        let oracle: serde_json::Value = serde_json::from_str(oracle_text).unwrap();
        for retry in [false, true] {
            let id = if retry {
                "construct-background-retry"
            } else {
                "construct-background"
            };
            let case = oracle["cases"]
                .as_array()
                .unwrap()
                .iter()
                .find(|c| c["id"] == id)
                .unwrap();
            let scratch = crate::test_scratch::Scratch::new("796-construction");
            let (started, entered) = tokio::sync::oneshot::channel();
            let (resume, paused) = std::sync::mpsc::channel();
            let store = Arc::new(ConstructionSnapshot {
                first_write: Mutex::new(Some(Box::new(move || {
                    let _ = started.send(());
                    let _ = paused.recv_timeout(Duration::from_secs(5));
                    panic!("injected construction panic");
                }))),
            });
            let mut runtime = BackgroundTaskRuntime::new(BackgroundTaskRuntimeOptions {
                max_tasks: Some(1),
                snapshot_store: Some(store),
                disk_spill_dir: Some(scratch.path().to_path_buf()),
                ..Default::default()
            });
            let spawned = Arc::new(AtomicUsize::new(0));
            let counted = spawned.clone();
            runtime.before_spawn = Some(Arc::new(move || {
                counted.fetch_add(1, Ordering::SeqCst);
            }));
            let starting = runtime.clone();
            let start = tokio::spawn(async move { starting.start(construction_spec()).await });
            tokio::time::timeout(Duration::from_secs(5), entered)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(*runtime.inner.starts.borrow(), 1);
            assert_eq!(runtime.inner.state.lock().unwrap().pending_starts, 1);
            let mut disposers = vec![];
            if !retry {
                disposers = (0..3).map(|_| Box::pin(runtime.dispose())).collect();
                for dispose in &mut disposers {
                    assert!(
                        dispose
                            .as_mut()
                            .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
                            .is_pending()
                    );
                }
            }
            resume.send(()).unwrap();
            let error = tokio::time::timeout(Duration::from_secs(5), start)
                .await
                .unwrap()
                .unwrap()
                .unwrap_err();
            assert_eq!(
                String::from_utf16_lossy(error.message.units()),
                case["error"].as_str().unwrap(),
                "construction panic error"
            );
            let released = *runtime.inner.starts.borrow() == 0
                && runtime.inner.state.lock().unwrap().pending_starts == 0;
            let empty = runtime.inner.state.lock().unwrap().entries.is_empty();
            let not_spawned = spawned.load(Ordering::SeqCst) == 0;
            let mut retry_accepted = false;
            if retry {
                retry_accepted = tokio::time::timeout(
                    Duration::from_secs(5),
                    runtime.start(construction_spec()),
                )
                .await
                .unwrap()
                .unwrap()
                .status
                    == TaskStatus::Failed;
                assert_eq!(
                    spawned.load(Ordering::SeqCst),
                    1,
                    "only retry reaches spawn"
                );
                disposers = (0..3).map(|_| Box::pin(runtime.dispose())).collect();
            }
            for dispose in disposers {
                tokio::time::timeout(Duration::from_secs(5), dispose)
                    .await
                    .expect("construction dispose deadline");
            }
            let actual = serde_json::json!({"id":id, "error":String::from_utf16_lossy(error.message.units()),
                "released":released, "empty":empty, "notSpawned":not_spawned, "retryAccepted":retry_accepted,
                "disposed":runtime.inner.state.lock().unwrap().phase == Phase::Disposed});
            assert_eq!(&actual, case, "construction reservation matrix");
        }
    }

    #[tokio::test]
    async fn initial_snapshot_construction_panic_payloads() {
        for owned in [false, true] {
            let scratch = crate::test_scratch::Scratch::new("796-payload");
            let store = Arc::new(ConstructionSnapshot {
                first_write: Mutex::new(Some(Box::new(move || {
                    if owned {
                        std::panic::panic_any(String::from("owned construction panic"));
                    }
                    std::panic::panic_any(7_u8);
                }))),
            });
            let runtime = BackgroundTaskRuntime::new(BackgroundTaskRuntimeOptions {
                snapshot_store: Some(store),
                disk_spill_dir: Some(scratch.path().to_path_buf()),
                ..Default::default()
            });
            let error = runtime.start(construction_spec()).await.unwrap_err();
            assert_eq!(
                error.message,
                JsString::from(if owned {
                    "owned construction panic"
                } else {
                    "Background initial snapshot construction failed."
                }),
                "construction panic payload"
            );
            assert_eq!(*runtime.inner.starts.borrow(), 0);
            assert_eq!(runtime.inner.state.lock().unwrap().pending_starts, 0);
            tokio::time::timeout(Duration::from_secs(5), runtime.dispose())
                .await
                .unwrap();
        }
    }
    use tokio::sync::oneshot;

    #[tokio::test]
    async fn start_persists_unsettled_task_before_dispose_for_restart() {
        use crate::runtime::background_snapshot::{
            BackgroundTaskSnapshotStore, JsonFileBackgroundTaskSnapshotStore,
        };
        let scratch = crate::test_scratch::Scratch::new("617-start-snapshot");
        let path = scratch.path().join("snapshot.json");
        let runtime = BackgroundTaskRuntime::new(BackgroundTaskRuntimeOptions {
            disk_spill_dir: Some(scratch.path().to_path_buf()),
            snapshot_store: Some(Arc::new(
                JsonFileBackgroundTaskSnapshotStore::new(path.clone()).unwrap(),
            )),
            ..Default::default()
        });
        let task = runtime
            .start(StartTaskSpec {
                command: JsValue::from("sleep 30"),
                cwd: "/".into(),
                env: None,
                session_id: JsValue::from("session"),
                agent_id: JsValue::from("agent"),
                kind: JsValue::Undefined,
            })
            .await
            .unwrap();
        // Capture on-disk state before any terminal transition or dispose.
        let store = JsonFileBackgroundTaskSnapshotStore::new(path).unwrap();
        let persisted = store.load();
        let live_status = runtime.get(&task.task_id, None).unwrap().status;
        let restored = BackgroundTaskRuntime::new(BackgroundTaskRuntimeOptions {
            disk_spill_dir: Some(scratch.path().to_path_buf()),
            snapshot_store: Some(Arc::new(store)),
            ..Default::default()
        });
        let recovered = restored.get(&task.task_id, None);
        // Reap the real process before assertions can fail.
        runtime.dispose().await;
        restored.dispose().await;

        assert_eq!(live_status, TaskStatus::Running);
        assert_eq!(persisted.len(), 1);
        assert_eq!(persisted[0].task.status, "pending");
        assert_eq!(persisted[0].output.total_bytes, 0);
        let recovered = recovered.expect("start must persist without dispose or settle");
        assert_eq!(recovered.status, TaskStatus::Unknown);
        assert_eq!(recovered.task_id, task.task_id);
        assert_eq!(recovered.session_id, task.session_id);
        assert_eq!(recovered.agent_id, task.agent_id);
        assert_eq!(recovered.pid, None);
        assert_eq!(recovered.exit_code, None);
    }

    #[tokio::test]
    async fn dispose_waits_for_gated_output_write_before_returning() {
        tokio::time::timeout(Duration::from_secs(10), async {
            use crate::runtime::background_snapshot::JsonFileBackgroundTaskSnapshotStore;
            let scratch = crate::test_scratch::Scratch::new("553-gated-dispose");
            let snapshot_path = scratch.path().join("snapshot.json");
            let runtime = BackgroundTaskRuntime::new(BackgroundTaskRuntimeOptions {
                disk_spill_dir: Some(scratch.path().to_path_buf()),
                snapshot_store: Some(Arc::new(
                    JsonFileBackgroundTaskSnapshotStore::new(snapshot_path.clone()).unwrap(),
                )),
                ..Default::default()
            });
            let task = runtime
                .start(StartTaskSpec {
                    command: JsValue::from("exit 0"),
                    cwd: "/".into(),
                    env: None,
                    session_id: JsValue::Undefined,
                    agent_id: JsValue::Undefined,
                    kind: JsValue::Undefined,
                })
                .await
                .unwrap();
            let entry = runtime.find(&task.task_id, None).unwrap();
            entry.wait_done().await;
            // Feed the same input as the real TS configured case, after the
            // process exits so the single write cannot race gate installation.
            let oracle: serde_json::Value = serde_json::from_str(include_str!(
                "../../tests/fixtures/background-output/oracle.json"
            ))
            .unwrap();
            let steps = oracle["cases"][0]["steps"].as_array().unwrap();
            let mut bytes = Vec::new();
            for step in steps.iter().take_while(|step| step["op"] != "close") {
                if step["op"] == "append" {
                    bytes.extend_from_slice(
                        step["text"]
                            .as_str()
                            .unwrap()
                            .repeat(step["repeat"].as_u64().unwrap() as usize)
                            .as_bytes(),
                    );
                }
            }
            let (entered, release) = entry.state.lock().unwrap().output.gate_next_write();
            entry.append(&bytes);
            entered.await.unwrap();
            let path = scratch.path().join("output").join(format!(
                "{}.log",
                String::from_utf16_lossy(task.task_id.units())
            ));
            assert!(!path.exists(), "the accepted write must still be gated");
            let mut dispose = Box::pin(runtime.dispose());
            assert!(
                tokio::time::timeout(Duration::from_millis(100), &mut dispose)
                    .await
                    .is_err(),
                "dispose returned before the gated write completed"
            );
            assert!(!path.exists(), "no filesystem write before gate release");
            entry.append(b"rejected after begin_close");
            assert_eq!(entry.snapshot().output_bytes, bytes.len() as u64);
            release.send(()).unwrap();
            dispose.await;
            let written = std::fs::read(path).unwrap();
            assert_eq!(written, bytes, "dispose must wait for the complete log");
            use sha2::{Digest, Sha256};
            let expected = &steps
                .iter()
                .rev()
                .find(|step| step["op"] == "disk")
                .unwrap()["result"];
            assert_eq!(written.len() as u64, expected["bytes"].as_u64().unwrap());
            assert_eq!(
                format!("{:x}", Sha256::digest(&written)),
                expected["sha256"].as_str().unwrap()
            );
            let restored = BackgroundTaskRuntime::new(BackgroundTaskRuntimeOptions {
                disk_spill_dir: Some(scratch.path().to_path_buf()),
                snapshot_store: Some(Arc::new(
                    JsonFileBackgroundTaskSnapshotStore::new(snapshot_path).unwrap(),
                )),
                ..Default::default()
            });
            let slice = restored
                .get_output(
                    &task.task_id,
                    &JsValue::Number(0.0),
                    &JsValue::Number(bytes.len() as f64),
                    None,
                )
                .unwrap();
            assert_eq!(slice.content.as_bytes(), bytes);
            assert_eq!(slice.total_bytes, bytes.len() as f64);
            assert!(!slice.truncated);
            restored.dispose().await;
        })
        .await
        .expect("bounded gated dispose probe");
    }

    fn pause_once(
        entered: oneshot::Sender<()>,
        resume: std::sync::mpsc::Receiver<()>,
    ) -> Arc<dyn Fn() + Send + Sync> {
        let gate = Mutex::new(Some((entered, resume)));
        Arc::new(move || {
            if let Some((entered, resume)) = gate.lock().unwrap().take() {
                let _ = entered.send(());
                // Dropping the sender also releases the worker on test failure.
                let _ = resume.recv();
            }
        })
    }

    fn subagent_input(run: SubagentRun) -> StartBackgroundSubagentInput {
        StartBackgroundSubagentInput {
            kind: None,
            session_id: "session".into(),
            parent_turn_id: "turn".into(),
            subagent_id: "child".into(),
            subagent_type: "general".into(),
            observed_subagent_id: None,
            run,
        }
    }

    /// stop() runs controller.abort() in the handler's call stack. Unlike a
    /// later timer event, it precedes overdue but undispatched web responses.
    #[tokio::test(start_paused = true)]
    async fn subagent_stop_synchronously_aborts_overdue_exchange() {
        use crate::runtime::network::{Exchange, ExchangeFuture, exchange};
        for synchronous_stop in [true, false] {
            let runtime = BackgroundSubagentRuntime::default();
            let task = runtime.start(subagent_input(Box::new(|signal| {
                Box::pin(async move {
                    signal.cancelled().await;
                    Ok(())
                })
            })));
            let entry = runtime.find(&task.task_id).unwrap();
            let mut pending = Box::pin(exchange(
                ExchangeFuture::after(Duration::from_millis(60), ()),
                entry.controller.cancelled(),
                || entry.controller.abort_event_at(),
                Some(tokio::time::Instant::now() + Duration::from_millis(100)),
            ));
            std::future::poll_fn(|cx| {
                assert!(pending.as_mut().poll(cx).is_pending());
                std::task::Poll::Ready(())
            })
            .await;
            tokio::time::advance(Duration::from_millis(120)).await;
            if synchronous_stop {
                assert!(runtime.stop(&task.task_id));
                assert!(entry.controller.is_cancelled());
                assert!(matches!(pending.await, Exchange::Aborted));
            } else {
                // Negative control: an event delivered later preserves the
                // response whose timer was due first.
                entry.controller.cancel();
                assert!(matches!(pending.await, Exchange::Response((), true)));
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn subagent_stop_wins_completion_before_cancel_signal() {
        let tick = std::sync::atomic::AtomicU64::new(1000);
        let mut runtime = BackgroundSubagentRuntime::new(Some(Arc::new(move || {
            tick.fetch_add(1, std::sync::atomic::Ordering::SeqCst) as f64
        })));
        let (stopping, stopped) = oneshot::channel();
        let (resume, paused) = std::sync::mpsc::channel();
        runtime.before_cancel = Some(pause_once(stopping, paused));
        let (started, running) = oneshot::channel();
        let (finish, finished) = oneshot::channel();
        let task = runtime.start(subagent_input(Box::new(move |signal| {
            Box::pin(async move {
                assert!(started.send(signal).is_ok());
                finished.await.unwrap();
                Ok(())
            })
        })));
        let signal = running.await.unwrap();
        let entry = runtime.find(&task.task_id).unwrap();
        let stop_runtime = runtime.clone();
        let task_id = task.task_id.clone();
        let stop = tokio::spawn(async move { stop_runtime.stop_and_wait(&task_id).await });

        tokio::time::timeout(Duration::from_secs(5), stopped)
            .await
            .unwrap()
            .unwrap();
        let at_stop = runtime.get(&task.task_id).unwrap();
        finish.send(()).unwrap();
        let mut done = entry.done.subscribe();
        tokio::time::timeout(Duration::from_secs(5), done.wait_for(|done| *done))
            .await
            .unwrap()
            .unwrap();
        let at_completion = runtime.get(&task.task_id).unwrap();
        let signalled_before_resume = signal.is_cancelled();
        resume.send(()).unwrap();
        let stopped = stop.await.unwrap();

        assert_eq!(at_stop.status, TaskStatus::Cancelled);
        assert!(!signalled_before_resume, "completion must precede cancel()");
        assert!(stopped);
        assert_eq!(at_completion.status, TaskStatus::Cancelled);
        assert_eq!(at_completion.ended_at, at_stop.ended_at);
        assert_eq!(at_completion.error, None);
        assert!(signal.is_cancelled());
        let final_task = runtime.get(&task.task_id).unwrap();
        assert_eq!(final_task.status, TaskStatus::Cancelled);
        assert_eq!(final_task.ended_at, at_stop.ended_at);
        assert!(!runtime.stop_and_wait(&task.task_id).await);
    }

    #[tokio::test]
    async fn subagent_completion_before_stop_remains_completed() {
        let runtime = BackgroundSubagentRuntime::default();
        let task = runtime.start(subagent_input(Box::new(|_| Box::pin(async { Ok(()) }))));
        let entry = runtime.find(&task.task_id).unwrap();
        let mut done = entry.done.subscribe();
        tokio::time::timeout(Duration::from_secs(5), done.wait_for(|done| *done))
            .await
            .unwrap()
            .unwrap();
        let completed = runtime.get(&task.task_id).unwrap();

        assert!(!runtime.stop_and_wait(&task.task_id).await);
        let after_stop = runtime.get(&task.task_id).unwrap();
        assert_eq!(completed.status, TaskStatus::Completed);
        assert_eq!(after_stop.status, TaskStatus::Completed);
        assert_eq!(after_stop.ended_at, completed.ended_at);
        assert!(!entry.controller.is_cancelled());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_start_reserves_capacity_before_publishing() {
        let runtime = BackgroundTaskRuntime::new(BackgroundTaskRuntimeOptions {
            max_tasks: Some(1),
            ..Default::default()
        });
        let spec = StartTaskSpec {
            command: JsValue::from("exit 0"),
            cwd: "/".into(),
            env: None,
            session_id: JsValue::Undefined,
            agent_id: JsValue::Undefined,
            kind: JsValue::Undefined,
        };
        let mut starting_runtime = runtime.clone();
        let (enter, entered) = oneshot::channel();
        let (resume, paused) = std::sync::mpsc::channel();
        starting_runtime.before_spawn = Some(pause_once(enter, paused));
        let first_spec = spec.clone();
        let start = tokio::spawn(async move { starting_runtime.start(first_spec).await });
        tokio::time::timeout(Duration::from_secs(5), entered)
            .await
            .unwrap()
            .unwrap();

        let listed = runtime.list(&TaskListFilter::default(), None);
        let second = runtime.start(spec.clone()).await;
        resume.send(()).unwrap();
        let first = start.await.unwrap().unwrap();
        runtime
            .wait(&first.task_id, &JsValue::Number(5000.0), None, None)
            .await
            .unwrap();
        // Publishing transfers the reservation to the entry; completion
        // frees the capacity so another start can use it.
        let replacement = runtime.start(spec).await;
        runtime.dispose().await;

        assert!(
            listed.is_empty(),
            "the first start must still be unpublished"
        );
        assert_eq!(
            second.unwrap_err().message,
            JsString::from("BackgroundTaskRuntime: max tasks (1) exceeded.")
        );
        assert!(replacement.is_ok());
        assert_eq!(runtime.inner.state.lock().unwrap().pending_starts, 0);
    }

    /// #636 fix-r1: the SDK projection consumes `this.resolve(...)` without a
    /// catch (`ProjectRuntimeRegistry.ts:1707-1711`), so a memory factory
    /// rejection during resolve must keep its JS shape here. Measured against
    /// the real TS baseline: `stopBackgroundTask` returns the exact thrown
    /// object (`name/code/requires_host`, lone-surrogate message).
    #[test]
    fn resolve_error_keeps_the_memory_factory_js_shape() {
        use crate::runtime::project::RuntimeJsError;

        let message = JsString::from_utf16(
            "factory boom "
                .encode_utf16()
                .chain([0xD800])
                .collect::<Vec<_>>(),
        );
        let error = BackgroundProjectionError::from(
            RuntimeError::new(String::from_utf16_lossy(message.units())).with_js_error(
                RuntimeJsError {
                    name: "DialogGatewayError".into(),
                    code: Some("FACTORY_FAILED".into()),
                    message: message.clone(),
                    requires_host: true,
                },
            ),
        );
        assert_eq!(error.name, "DialogGatewayError");
        assert_eq!(error.code.as_deref(), Some("FACTORY_FAILED"));
        assert_eq!(error.message.units(), message.units());
        assert!(
            !error.message.units().contains(&0xFFFD),
            "a lone surrogate must not be replacement-decoded"
        );
        assert!(error.requires_host);
    }

    #[test]
    fn plain_runtime_error_keeps_the_ordinary_error_shape() {
        let error = BackgroundProjectionError::from(RuntimeError::new("dispose boom"));
        assert_eq!(error.name, "Error");
        assert_eq!(error.code, None);
        assert_eq!(error.message, JsString::from("dispose boom"));
        assert!(!error.requires_host);
    }

    /// The real projection entry point: a failing `registry.resolve` (the
    /// memory family is the last resolve stage) reaches the SDK caller with
    /// the original shape instead of a fixed `Error`.
    #[tokio::test]
    async fn stop_background_task_resolve_failure_keeps_the_js_error() {
        use crate::runtime::project::{ResourceBundle, RuntimeJsError, RuntimeResult};

        let message = JsString::from_utf16(
            "factory boom "
                .encode_utf16()
                .chain([0xD800])
                .collect::<Vec<_>>(),
        );
        let thrown = message.clone();
        let registry = ProjectRuntimeRegistry::new(
            PathBuf::from("/pdrs-fixture/fallback"),
            Arc::new(move |_: &Path, _: &mut ResourceBundle| -> RuntimeResult {
                Err(
                    RuntimeError::new(String::from_utf16_lossy(thrown.units())).with_js_error(
                        RuntimeJsError {
                            name: "DialogGatewayError".into(),
                            code: Some("FACTORY_FAILED".into()),
                            message: thrown.clone(),
                            requires_host: true,
                        },
                    ),
                )
            }),
        );
        let error = stop_background_task_in_registry(
            &registry,
            Some(Path::new("/pdrs-fixture/project")),
            &JsString::from("session"),
            &JsString::from("task"),
        )
        .await
        .expect_err("the resolve failure must stop the SDK projection");
        assert_eq!(error.name, "DialogGatewayError");
        assert_eq!(error.code.as_deref(), Some("FACTORY_FAILED"));
        assert_eq!(error.message.units(), message.units());
        assert!(!error.message.units().contains(&0xFFFD));
        assert!(error.requires_host);
        registry.dispose().await.unwrap();
    }
}
