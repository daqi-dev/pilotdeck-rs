#![cfg(unix)]
//! Background tasks (A6.5-pr12), ported from the fixed TS baseline:
//!
//! - `task/runtime/BackgroundTaskRuntime.ts` + `task/storage/TaskOutputStore.ts`
//!   (in-memory ring only) + the POSIX half of
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
//! - Disk spill and snapshot persistence are not ported; output lives in the
//!   1 MB ring and restored `unknown` tasks cannot occur. Ledger:
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
    collections::VecDeque,
    future::Future,
    path::Path,
    pin::Pin,
    process::Stdio,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{io::AsyncReadExt, sync::watch};

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

/// `TaskOutputStore` without disk spill: a byte ring of `max` bytes over the
/// combined stdout/stderr stream with a monotonically increasing total.
struct OutputStore {
    chunks: VecDeque<Vec<u8>>,
    memory: usize,
    total: u64,
    dropped: u64,
    max: usize,
    closed: bool,
}

impl OutputStore {
    fn new(max: usize) -> Self {
        Self {
            chunks: VecDeque::new(),
            memory: 0,
            total: 0,
            dropped: 0,
            max,
            closed: false,
        }
    }
    fn append(&mut self, chunk: &[u8]) {
        if self.closed || chunk.is_empty() {
            return;
        }
        self.total += chunk.len() as u64;
        self.chunks.push_back(chunk.to_vec());
        self.memory += chunk.len();
        while self.memory > self.max {
            let overflow = self.memory - self.max;
            let Some(oldest) = self.chunks.front_mut() else {
                break;
            };
            if oldest.len() <= overflow {
                let length = oldest.len();
                self.chunks.pop_front();
                self.memory -= length;
                self.dropped += length as u64;
            } else {
                oldest.drain(..overflow);
                self.memory -= overflow;
                self.dropped += overflow as u64;
            }
        }
    }
    /// `readSlice(offset, maxBytes)`: `offset` is used only when it is a
    /// finite number; a `maxBytes` that is not a finite number (after `??
    /// Infinity`) reads up to the ring size.
    fn read_slice(&self, offset: &JsValue, max_bytes: &JsValue) -> OutputSlice {
        let head = self.total as f64;
        let mut cursor = match offset {
            JsValue::Number(n) if n.is_finite() => n.floor().max(0.0),
            _ => 0.0,
        };
        let mut remaining = match max_bytes {
            JsValue::Number(n) if n.is_finite() => n.floor().max(0.0),
            _ => self.max as f64,
        };
        let mut truncated = false;
        let slice = |content: Vec<u8>, next_offset: f64, truncated: bool| OutputSlice {
            content: String::from_utf8_lossy(&content).into_owned(),
            next_offset,
            total_bytes: head,
            truncated,
        };
        if remaining <= 0.0 {
            return slice(Vec::new(), cursor, truncated);
        }
        let dropped = self.dropped as f64;
        if cursor < dropped {
            truncated = true;
            cursor = dropped;
        }
        if cursor >= head {
            return slice(Vec::new(), cursor.min(head), truncated);
        }
        let wanted = (head - cursor).min(remaining);
        remaining = wanted;
        let mut skip = (cursor - dropped) as usize;
        let mut content = Vec::new();
        for chunk in &self.chunks {
            if remaining <= 0.0 {
                break;
            }
            if skip >= chunk.len() {
                skip -= chunk.len();
                continue;
            }
            let end = chunk.len().min(skip + remaining as usize);
            content.extend_from_slice(&chunk[skip..end]);
            remaining -= (end - skip) as f64;
            skip = 0;
        }
        let collected = content.len() as f64;
        slice(content, cursor + collected, truncated)
    }
    /// `close()`: the readable buffer is released; totals stay.
    fn close(&mut self) {
        self.closed = true;
        self.chunks.clear();
        self.memory = 0;
    }
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

/// `BackgroundTaskRuntimeOptions` (without disk spill / snapshot store).
#[derive(Clone, Default)]
pub struct BackgroundTaskRuntimeOptions {
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
        Self {
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
                completion_preview_bytes: options
                    .completion_preview_bytes
                    .unwrap_or(DEFAULT_COMPLETION_PREVIEW_BYTES),
            }),
            #[cfg(test)]
            before_spawn: None,
        }
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
        #[cfg(test)]
        if let Some(before_spawn) = &self.before_spawn {
            before_spawn();
        }
        let result = self.spawn_task(task_id, entry, spec);
        self.inner.starts.send_modify(|starts| *starts -= 1);
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
        let new_entry =
            |task: BackgroundBashTask, never_exits: bool, child: Option<ChildState>| Entry {
                state: Mutex::new(EntryState {
                    task,
                    output: OutputStore::new(DEFAULT_MEMORY_BYTES),
                    child,
                }),
                done: watch::channel(false).0,
                closed: watch::channel(false).0,
                never_exits,
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
            for entry in &entries {
                entry.state.lock().unwrap().output.close();
                entry.closed.send_replace(true);
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
    fn settle(&self, entry: &Entry) {
        let event = {
            let mut state = entry.state.lock().unwrap();
            state.task.output_bytes = state.output.total;
            let task = &state.task;
            match task.ended_at {
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
            }
        };
        if let (Some(event), Some(handler)) = (event, &self.on_completion) {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handler(event)));
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

/// A rejection from the SDK projections, as `{ name, code?, message }`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackgroundProjectionError {
    pub name: &'static str,
    pub code: Option<&'static str>,
    pub message: JsString,
}

impl From<BackgroundTaskError> for BackgroundProjectionError {
    fn from(error: BackgroundTaskError) -> Self {
        Self {
            name: error.name,
            code: None,
            message: error.message,
        }
    }
}

impl From<RuntimeError> for BackgroundProjectionError {
    fn from(error: RuntimeError) -> Self {
        Self {
            name: "Error",
            code: None,
            message: JsString::from(error.message.as_str()),
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
            name: "DialogGatewayError",
            code: Some("BACKGROUND_TASK_NOT_FOUND"),
            message: concat(&[
                &units("Background task "),
                task_id.units(),
                &units(" is not owned by session "),
                session_key.units(),
                &units("."),
            ]),
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
            name: "Error",
            code: None,
            message: JsString::from("Project runtime has no background task resources."),
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
    use tokio::sync::oneshot;

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

    #[test]
    fn output_ring_drops_oldest_bytes_and_reports_the_gap() {
        let mut store = OutputStore::new(4);
        store.append(b"abc");
        store.append(b"def");
        assert_eq!((store.total, store.dropped, store.memory), (6, 2, 4));
        let slice = store.read_slice(&JsValue::Number(0.0), &JsValue::Undefined);
        assert_eq!(
            (slice.content.as_str(), slice.next_offset, slice.truncated),
            ("cdef", 6.0, true)
        );
        let slice = store.read_slice(&JsValue::Number(3.5), &JsValue::Number(2.0));
        assert_eq!(
            (slice.content.as_str(), slice.next_offset, slice.truncated),
            ("de", 5.0, false)
        );
        let slice = store.read_slice(&JsValue::from("3"), &JsValue::Number(0.0));
        assert_eq!((slice.content.as_str(), slice.next_offset), ("", 0.0));
        store.close();
        store.append(b"x");
        let slice = store.read_slice(&JsValue::Number(0.0), &JsValue::Undefined);
        assert_eq!(
            (slice.content.as_str(), slice.next_offset, slice.total_bytes),
            ("", 2.0, 6.0)
        );
    }
}
