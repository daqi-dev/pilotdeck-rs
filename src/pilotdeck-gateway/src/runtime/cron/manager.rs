//! Port of TS `src/cron/runtime/CronManager.ts` (the multi-project control
//! provider and lifecycle owner), `src/cron/runtime/CronRuntime.ts` (the six
//! control methods plus start/stop/session recovery and the fire/scheduler
//! halves `bindAgentGateway` constructs), and the native provider half of
//! `CronProjectStorageProvider.ts` (`.cwd` recording and restart discovery).
//! Frame dispatch is #1326; the fire/scheduler halves are #2295 — the timer
//! is the 60s idle poll with an `isDue` filter, and the event-stream turn
//! consumption folds into the synchronous `CronAgentGateway` boundary.
use std::{
    cell::{Cell, RefCell},
    fs, io,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json};
use pilotdeck_transport::system_clock::resolve_local;

use crate::{
    config::{
        session_overrides::{SessionConfigOverrides, UNATTENDED_SESSION_EXCLUDED_TOOLS},
        session_value::SessionValue,
    },
    js_value, metadata, node_io,
    project_files::node_compare_utf16,
    runtime::session::{SessionAdmission, TurnReservation},
};

use super::{
    CRON_SCHEDULE_COMPUTATION_VERSION, ComputedRunAt, CronCreateSchedule, CronStoreError,
    CronTaskStore, CronZoneResolverCache, DelayUnit, MigrationInput, collator, compute_next_run_at,
    delay_to_milliseconds, fire::CronExecutionCallbacks, fire::CronFire, fire::CronFireDeps,
    is_valid_cron_timezone, iso_from_epoch_ms, migrate_cron_stores, resolve_cron_timezone,
    schedule::js_new_date, schedule::once_computed_run_at, scheduler::CronScheduler,
    scheduler::SchedulerDeps, strict_equals, truthy,
};

/// `defaultCronConfig` (`src/cron/config/parseCronConfig.ts`): the defaults a
/// `pilotdeck.yaml` without a `cron:` section gets. `enabled` and `timezone`
/// gate the runtime; the two scheduler knobs feed the firing half (#2295) —
/// `max_concurrent_runs` caps in-flight runs per tick (scheduler.rs),
/// `run_timeout_minutes` bounds a run's turn (fire.rs).
#[derive(Clone, Debug)]
pub struct CronManagerConfig {
    pub enabled: bool,
    pub timezone: String,
    pub max_concurrent_runs: f64,
    pub run_timeout_minutes: f64,
}
impl Default for CronManagerConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            timezone: "UTC".into(),
            max_concurrent_runs: 1.0,
            run_timeout_minutes: 60.0,
        }
    }
}

/// TS `CronRuntimeLogger` (CronRuntime.ts:40-43), the manager/runtime slice:
/// both channels, with the metadata object exactly as the TS call site builds
/// it (an empty object where TS omits the argument).
pub trait CronLogger: Send + Sync {
    fn info(&self, message: &str, meta: &JsObject);
    fn warn(&self, message: &str, meta: &JsObject);
}

/// TS `NOOP_LOGGER` (CronRuntime.ts:64-67).
pub struct NoopCronLogger;
impl CronLogger for NoopCronLogger {
    fn info(&self, _message: &str, _meta: &JsObject) {}
    fn warn(&self, _message: &str, _meta: &JsObject) {}
}

/// The one `MigrationInput` logger the manager can supply: `migrateLegacy`
/// only forwards `warn` (the `CronStoreMigration.ts` logger call sites).
struct MigrationLogger(Arc<dyn CronLogger>);
impl super::CronRuntimeLogger for MigrationLogger {
    fn warn(&self, message: &str, meta: &JsObject) {
        self.0.warn(message, meta);
    }
}

/// `CreateCronManagerOptions` (CronManager.ts:33-45): the port's injectable
/// seams — `now`/`uuid` pin the clocks for fixtures, `session_overrides`
/// shares the live registry with the session owner, `logger` is the runtime
/// logger.
pub struct CronManagerOptions {
    pub config: CronManagerConfig,
    pub pilot_home: String,
    pub session_overrides: Option<Arc<Mutex<SessionConfigOverrides>>>,
    pub now: Option<Arc<dyn Fn() -> f64 + Send + Sync>>,
    pub uuid: Option<Arc<dyn Fn() -> JsString + Send + Sync>>,
    pub logger: Option<Arc<dyn CronLogger>>,
    /// The #2295 test seam for the scheduler's idle poll (`computeDelayMs`,
    /// CronScheduler.ts:8): `None` keeps the 60s production constant; the
    /// golden fixtures pin it low. `delayTask`'s deferral always writes the
    /// constant, never this seam.
    pub scheduler_idle_poll_ms: Option<u64>,
}

/// Every error the six methods and the lifecycle can surface, carrying the
/// `{name, code?, message}` golden shape (the `ManagerError::as_json`
/// precedent): plain `Error`/`TypeError`/`RangeError` keep their name, and
/// `DialogGatewayError` adds its `code`.
#[derive(Clone, Debug)]
pub struct CronError {
    name: String,
    code: Option<String>,
    message: JsString,
}
impl CronError {
    pub fn error(message: impl Into<JsString>) -> Self {
        Self {
            name: "Error".into(),
            code: None,
            message: message.into(),
        }
    }
    pub fn type_error(message: impl Into<JsString>) -> Self {
        Self {
            name: "TypeError".into(),
            code: None,
            message: message.into(),
        }
    }
    pub fn range_error(message: impl Into<JsString>) -> Self {
        Self {
            name: "RangeError".into(),
            code: None,
            message: message.into(),
        }
    }
    /// `DialogGatewayError` (gateway/dialog/errors.ts:1-10): name plus code.
    pub fn gateway(code: &str, message: impl Into<JsString>) -> Self {
        Self {
            name: "DialogGatewayError".into(),
            code: Some(code.to_owned()),
            message: message.into(),
        }
    }
    /// Node fs errors crossing the manager boundary, through the shared libuv
    /// diagnostic formatter. (Errors bubbling up as [`CronStoreError::Io`]
    /// keep the store's plain `Display` — the store does not retain the
    /// syscall context #1216 would need to shape them.)
    fn io(error: io::Error, syscall: &'static str, path: &str) -> Self {
        let (_, message) = node_io::diagnostic_or_unknown(&error, syscall, Some(path));
        Self::error(JsString::from(message))
    }
    pub fn message(&self) -> &JsString {
        &self.message
    }
    pub fn code(&self) -> Option<&str> {
        self.code.as_deref()
    }
    /// Golden/wire shape: `{name, code?, message}`.
    pub fn as_json(&self) -> JsValue {
        let mut out = JsObject::new();
        out.insert("name".into(), JsValue::from(self.name.as_str()));
        if let Some(code) = &self.code {
            out.insert("code".into(), JsValue::from(code.as_str()));
        }
        out.insert("message".into(), JsValue::String(self.message.clone()));
        JsValue::Object(out)
    }
}
impl From<CronStoreError> for CronError {
    fn from(value: CronStoreError) -> Self {
        match value {
            // The `localeCompare` TypeError (CronTaskStore.ts:177) is the
            // representative of this arm; the stack-overflow messages share
            // its name.
            CronStoreError::Type(message) => Self::type_error(JsString::from(message)),
            CronStoreError::LockTimeout(message) => Self::error(JsString::from(message)),
            CronStoreError::Io(error) => Self::error(JsString::from(error.to_string())),
        }
    }
}

static UNDEFINED: JsValue = JsValue::Undefined;

/// `input.<name>` (the CronRuntime.ts method bodies): null/undefined
/// receivers throw V8's reading TypeError, primitives read as `undefined`,
/// and a missing own property reads as `undefined`.
fn property<'a>(input: &'a JsValue, name: &str) -> Result<&'a JsValue, CronError> {
    match input {
        JsValue::Object(_) | JsValue::Array(_) => Ok(input.get(name).unwrap_or(&UNDEFINED)),
        JsValue::Undefined => Err(CronError::type_error(JsString::from(format!(
            "Cannot read properties of undefined (reading '{name}')"
        )))),
        JsValue::Null => Err(CronError::type_error(JsString::from(format!(
            "Cannot read properties of null (reading '{name}')"
        )))),
        _ => Ok(&UNDEFINED),
    }
}

/// `${value}` in the TS template-literal error messages: ECMAScript `String()`,
/// so lone surrogates survive as raw UTF-16 units.
pub(super) fn to_display(value: &JsValue) -> Result<JsString, CronError> {
    js_value::to_string_units(value)
        .map(JsString::from_utf16)
        .map_err(|_| CronError::range_error(JsString::from("Maximum call stack size exceeded")))
}

/// `path.resolve(key)` over raw UTF-16 units (CronManager.ts:182): an absolute
/// key never consults the process cwd, a relative one resolves against it,
/// and the identity folds to UTF-8 only after normalization (a lone surrogate
/// that survives the lexical pass collapses lossily — Node's own fs boundary
/// replaces it with U+FFFD once it reaches a file, and the store keys the
/// same lossy directory).
/// `JsValue::Array` view (the protocol type has no `as_array`).
fn value_array(value: &JsValue) -> Option<&[JsValue]> {
    match value {
        JsValue::Array(items) => Some(items),
        _ => None,
    }
}

/// A `JsString` as lossy UTF-8 (lone surrogates become U+FFFD) — only for
/// Rust-side lookups (`trim`, zone `Set#has`) where a surrogate can never
/// match anyway.
fn js_lossy(text: &JsString) -> String {
    String::from_utf16_lossy(text.units())
}

/// `JsValue::String` from an owned Rust string (the protocol's `From` is
/// `&str` only).
pub(super) fn value_string(text: impl Into<JsString>) -> JsValue {
    JsValue::String(text.into())
}

fn resolve_key_string(text: &JsString) -> Result<String, CronError> {
    metadata::resolve_process_identity(Path::new(""), text)
        .map_err(|error| CronError::io(error, "uv_cwd", ""))
        .map(|identity| identity.unwrap_or_else(|| String::from_utf16_lossy(text.units())))
}

fn resolve_project_key(value: &JsValue) -> Result<String, CronError> {
    let JsValue::String(text) = value else {
        let received = match metadata::argument_received(value) {
            metadata::ArgumentReceived::Text(text) => text,
            metadata::ArgumentReceived::Throws { name, message } => {
                return Err(CronError {
                    name: name.to_owned(),
                    code: None,
                    message,
                });
            }
        };
        return Err(CronError::type_error(metadata::invalid_arg_type_message(
            "paths[0]", &received,
        )));
    };
    resolve_key_string(text)
}

/// `resolve(options.pilotHome)` (CronManager.ts:61).
fn resolve_pilot_home(home: &str) -> Result<String, CronError> {
    metadata::resolve_process_path(Path::new(""), Path::new(home))
        .map(|home| home.to_string_lossy().into_owned())
        .map_err(|error| CronError::io(error, "uv_cwd", ""))
}

// --------------------------------------------------------------- turn port

/// The `submitTurn` input (CronAgentGatewayPort.ts:7-15).
pub struct CronTurnSubmit {
    pub session_key: JsString,
    pub channel_key: JsString,
    pub project_key: Option<JsString>,
    pub message: JsString,
    pub mode: JsString,
    pub run_id: JsString,
    pub timeout_ms: f64,
}

/// TS `CronAgentGatewayPort` (CronAgentGatewayPort.ts:19-28): the only gateway
/// operations a cron run may invoke. `submit_turn` stands for the whole
/// event-stream turn — the port contract is that it returns only after the
/// turn has unwound, so admission is begin/end fenced around it
/// synchronously; consuming and delivering the `GatewayEvent` stream inside
/// that fence is the `CronFire` half's job (fire.rs, #2295).
pub trait CronAgentGateway: Send + Sync {
    /// Rejected when the session cannot be admitted (busy, maintenance,
    /// shutdown) or the turn itself fails.
    fn submit_turn(&self, input: &CronTurnSubmit) -> Result<(), CronError>;
    /// Observe every event inside the existing admission fence. Old
    /// event-free ports retain their synchronous Result contract.
    fn submit_turn_observed(
        &self,
        input: &CronTurnSubmit,
        _observe: &mut dyn FnMut(&JsValue) -> Result<(), CronError>,
    ) -> Result<(), CronError> {
        self.submit_turn(input)
    }
    /// `abortTurn` — the caller decides whether a rejection surfaces
    /// (CronRuntime.ts:373 propagates, CronRuntime.ts:207-209 swallows).
    fn abort_turn(
        &self,
        session_key: &JsString,
        run_id: Option<&JsString>,
        reason: Option<&JsString>,
    ) -> Result<(), CronError>;
    /// `closeSession` — every cron call site swallows rejections
    /// (`.catch(() => undefined)`).
    fn close_session(
        &self,
        session_key: &JsString,
        reason: Option<&JsString>,
    ) -> Result<(), CronError>;
}

/// The turn-execution seam the live gateway owns (the `CronFire` half is
/// fire.rs, #2295; routing #1326). `run_turn` executes one admitted turn;
/// `abort_turn` and
/// `close_session` are the gateway's halves behind the same
/// [`CronAgentGateway`] surface.
pub trait CronGatewayBody: Send + Sync {
    fn run_turn(&self, input: &CronTurnSubmit) -> Result<(), CronError>;
    fn run_turn_observed(
        &self,
        input: &CronTurnSubmit,
        _observe: &mut dyn FnMut(&JsValue) -> Result<(), CronError>,
    ) -> Result<(), CronError> {
        self.run_turn(input)
    }
    fn abort_turn(
        &self,
        session_key: &JsString,
        run_id: Option<&JsString>,
        reason: Option<&JsString>,
    ) -> Result<(), CronError>;
    fn close_session(
        &self,
        session_key: &JsString,
        reason: Option<&JsString>,
    ) -> Result<(), CronError>;
}

/// The A7 binding over the port: `SessionAdmission` is the volatile
/// SessionRouter fence (begin/end turn keyed by sessionKey), so a duplicate
/// submit for the same session is rejected by the same acceptor the live
/// gateway uses — the second trigger never executes (matrix cell 11).
pub struct SessionAdmissionCronGateway {
    admission: Arc<SessionAdmission>,
    body: Arc<dyn CronGatewayBody>,
}
impl SessionAdmissionCronGateway {
    pub fn new(admission: Arc<SessionAdmission>, body: Arc<dyn CronGatewayBody>) -> Self {
        Self { admission, body }
    }
}
/// Releases the admission fence on every exit path, including unwind: if
/// `run_turn` panics, a plain `end_turn` call after it would be skipped and the
/// session would answer `session_busy` to every later submit until restart.
/// Same shape as the `Cleanup` guard in `runtime/turn/submit.rs`.
struct TurnFence<'a> {
    admission: &'a SessionAdmission,
    session_key: &'a JsString,
    run_id: &'a JsString,
    reservation: Option<TurnReservation>,
}
impl Drop for TurnFence<'_> {
    fn drop(&mut self) {
        if let Some(reservation) = self.reservation.take() {
            self.admission
                .end_turn(self.session_key, Some(self.run_id), Some(&reservation));
        }
    }
}

impl SessionAdmissionCronGateway {
    fn admitted(
        &self,
        input: &CronTurnSubmit,
        run: impl FnOnce() -> Result<(), CronError>,
    ) -> Result<(), CronError> {
        let Some(reservation) = self.admission.begin_turn(&input.session_key, &input.run_id) else {
            // InProcessGateway.ts:1043-1067: the rejected submit surfaces as a
            // terminal `session_busy` error; the port's synchronous boundary
            // carries the same code and message.
            let mut message = "Session ".encode_utf16().collect::<Vec<_>>();
            message.extend_from_slice(input.session_key.units());
            message.extend(" already has an active turn.".encode_utf16());
            return Err(CronError::gateway(
                "session_busy",
                JsString::from_utf16(message),
            ));
        };
        let _fence = TurnFence {
            admission: self.admission.as_ref(),
            session_key: &input.session_key,
            run_id: &input.run_id,
            reservation: Some(reservation),
        };
        run()
    }
}

impl CronAgentGateway for SessionAdmissionCronGateway {
    fn submit_turn(&self, input: &CronTurnSubmit) -> Result<(), CronError> {
        self.admitted(input, || self.body.run_turn(input))
    }
    fn submit_turn_observed(
        &self,
        input: &CronTurnSubmit,
        observe: &mut dyn FnMut(&JsValue) -> Result<(), CronError>,
    ) -> Result<(), CronError> {
        self.admitted(input, || self.body.run_turn_observed(input, observe))
    }
    fn abort_turn(
        &self,
        session_key: &JsString,
        run_id: Option<&JsString>,
        reason: Option<&JsString>,
    ) -> Result<(), CronError> {
        self.body.abort_turn(session_key, run_id, reason)
    }
    fn close_session(
        &self,
        session_key: &JsString,
        reason: Option<&JsString>,
    ) -> Result<(), CronError> {
        self.body.close_session(session_key, reason)
    }
}

// ------------------------------------------------------------ active runs

/// TS `CronActiveRun` (CronFire.ts:8-14). `CronFire.runTask` (fire.rs, #2295)
/// registers one before the first store claim and unregisters it in its
/// finally; the registration pair is also the seam the fixtures drive
/// (matrix cells 8/10/13).
#[derive(Clone, Debug)]
pub struct CronActiveRun {
    pub run_id: JsString,
    pub task_id: JsString,
    pub session_key: JsString,
    /// `schedule.type`, kept as the raw stored string.
    pub schedule_type: JsString,
    pub stop_requested: bool,
}

/// `normalizeSchedule`'s output (CronRuntime.ts:572-597): the delay arm has
/// already folded into `once` with an ISO `runAt`.
pub(super) enum NormalizedSchedule {
    Once {
        run_at: JsValue,
    },
    Cron {
        expression: JsValue,
        timezone: String,
    },
}

impl NormalizedSchedule {
    /// The persisted `task.schedule` value: `{type, runAt}` / `{type,
    /// expression, timezone}` in the TS literal key order.
    fn as_task_value(&self) -> JsValue {
        let mut out = JsObject::new();
        match self {
            Self::Once { run_at } => {
                out.insert("type".into(), JsValue::from("once"));
                out.insert("runAt".into(), run_at.clone());
            }
            Self::Cron {
                expression,
                timezone,
            } => {
                out.insert("type".into(), JsValue::from("cron"));
                out.insert("expression".into(), expression.clone());
                out.insert("timezone".into(), value_string(timezone.clone()));
            }
        }
        JsValue::Object(out)
    }
}

// ---------------------------------------------------- project storage

/// `recordProjectKey` (CronProjectStorageProvider.ts:62-66): the recursive
/// `mkdir` plus the `.cwd` marker write.
fn record_project_key(paths: &super::CronPaths, project_key: &str) -> Result<(), CronError> {
    let shown = paths.project_dir.to_string_lossy().into_owned();
    fs::create_dir_all(&paths.project_dir)
        .map_err(|error| CronError::io(error, "mkdir", &shown))?;
    let marker = paths.project_dir.join(".cwd");
    // `${resolve(projectKey)}\n` — the key arrives already resolved and
    // re-resolving an absolute normalized path is the identity, so the TS
    // double resolve and this write agree byte for byte.
    fs::write(&marker, format!("{project_key}\n"))
        .map_err(|error| CronError::io(error, "open", &marker.to_string_lossy()))
}

/// `discoverNativeCronProjectKeys` (CronProjectStorageProvider.ts:70-96):
/// per project directory, keys from tasks.json, run-history lines, and the
/// `.cwd` marker, de-duplicated and returned in `Array#sort` order (raw
/// UTF-16 code units, not a collation). Unreadable task stores and run
/// histories only warn; a non-`ENOENT` `.cwd` read failure propagates, as in
/// TS.
fn discover_project_keys(
    pilot_home: &str,
    logger: Option<&Arc<dyn CronLogger>>,
) -> Result<Vec<String>, CronError> {
    let home = metadata::resolve_process_path(Path::new(""), Path::new(pilot_home))
        .map_err(|error| CronError::io(error, "uv_cwd", ""))?;
    let projects_dir = metadata::resolve(&metadata::resolve(&home, "cron"), "projects");
    let shown = projects_dir.to_string_lossy().into_owned();
    let entries = match fs::read_dir(&projects_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(CronError::io(error, "scandir", &shown)),
    };
    let mut keys: Vec<String> = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| CronError::io(error, "scandir", &shown))?;
        // `entry.isDirectory()` over the raw dirent type, like Node's
        // `withFileTypes: true`.
        if !entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
            continue;
        }
        let project_dir = metadata::resolve(&projects_dir, &entry.file_name().to_string_lossy());
        collect_keys_from_tasks(&project_dir, &mut keys, logger)?;
        collect_keys_from_runs(&project_dir, &mut keys, logger)?;
        let marker_path = project_dir.join(".cwd");
        match fs::read(&marker_path) {
            Ok(bytes) => {
                let marker_text = String::from_utf8_lossy(&bytes);
                let marker = metadata::js_trim(&marker_text);
                if !marker.is_empty() {
                    push_key(&mut keys, resolve_key_string(&JsString::from(marker))?);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(CronError::io(error, "open", &marker_path.to_string_lossy())),
        }
    }
    keys.sort_by(|left, right| left.encode_utf16().cmp(right.encode_utf16()));
    Ok(keys)
}

/// `collectProjectKeysFromTasks` (CronProjectStorageProvider.ts:98-119): a
/// read or shape failure warns and keeps going; `ENOENT` is silent.
fn collect_keys_from_tasks(
    project_dir: &Path,
    keys: &mut Vec<String>,
    logger: Option<&Arc<dyn CronLogger>>,
) -> Result<(), CronError> {
    let tasks_path = project_dir.join("tasks.json");
    let shown = tasks_path.to_string_lossy().into_owned();
    let bytes = match fs::read(&tasks_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return unreadable_store_warn(
                logger,
                "cron project discovery skipped unreadable task store",
                project_dir,
                &fs_error_message(&error, &shown),
            );
        }
    };
    let parsed = match parse_json(&String::from_utf8_lossy(&bytes)) {
        Ok(parsed) => parsed,
        Err(error) => {
            return unreadable_store_warn(
                logger,
                "cron project discovery skipped unreadable task store",
                project_dir,
                &error.to_string(),
            );
        }
    };
    // `parsed.tasks ?? []` — a `null` document's property read throws before
    // the loop; a non-iterable `tasks` throws at the `for..of`.
    let tasks = match parsed {
        JsValue::Null => {
            return unreadable_store_warn(
                logger,
                "cron project discovery skipped unreadable task store",
                project_dir,
                "Cannot read properties of null (reading 'tasks')",
            );
        }
        parsed => parsed.get("tasks").cloned().unwrap_or(JsValue::Undefined),
    };
    let Some(tasks) = value_array(&tasks) else {
        return unreadable_store_warn(
            logger,
            "cron project discovery skipped unreadable task store",
            project_dir,
            "(parsed.tasks ?? []) is not iterable",
        );
    };
    for task in tasks {
        if let JsValue::String(key) = task.get("projectKey").unwrap_or(&UNDEFINED)
            && !metadata::js_trim(&js_lossy(key)).is_empty()
        {
            push_key(keys, resolve_key_string(key)?);
        }
    }
    Ok(())
}

/// `collectProjectKeysFromRuns` (CronProjectStorageProvider.ts:121-147):
/// unreadable single lines are skipped silently; only a failed file read
/// warns.
fn collect_keys_from_runs(
    project_dir: &Path,
    keys: &mut Vec<String>,
    logger: Option<&Arc<dyn CronLogger>>,
) -> Result<(), CronError> {
    let history_path = project_dir.join("run-history.jsonl");
    let shown = history_path.to_string_lossy().into_owned();
    let bytes = match fs::read(&history_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return unreadable_store_warn(
                logger,
                "cron project discovery skipped unreadable run history",
                project_dir,
                &fs_error_message(&error, &shown),
            );
        }
    };
    let raw = String::from_utf8_lossy(&bytes);
    for line in raw.split('\n') {
        if metadata::js_trim(line).is_empty() {
            continue;
        }
        let Ok(parsed) = parse_json(line) else {
            continue;
        };
        if let JsValue::String(key) = parsed.get("projectKey").unwrap_or(&UNDEFINED)
            && !metadata::js_trim(&js_lossy(key)).is_empty()
        {
            push_key(keys, resolve_key_string(key)?);
        }
    }
    Ok(())
}

/// The `logger?.warn(...)` sites (CronProjectStorageProvider.ts:113-117).
fn unreadable_store_warn(
    logger: Option<&Arc<dyn CronLogger>>,
    message: &str,
    project_dir: &Path,
    error: &str,
) -> Result<(), CronError> {
    if let Some(logger) = logger {
        let mut meta = JsObject::new();
        meta.insert(
            "projectDir".into(),
            value_string(project_dir.to_string_lossy().into_owned()),
        );
        meta.insert("error".into(), JsValue::from(error));
        logger.warn(message, &meta);
    }
    Ok(())
}

/// `error instanceof Error ? error.message : String(error)` for the warn
/// metadata — the readFile diagnostics, through the shared formatter.
fn fs_error_message(error: &io::Error, path: &str) -> String {
    node_io::read_file(error, path)
        .map(|(_, message)| message)
        .unwrap_or_else(|| error.to_string())
}

fn push_key(keys: &mut Vec<String>, key: String) {
    if !keys.contains(&key) {
        keys.push(key);
    }
}

/// One `findActiveRun` selector: `input.<field> && run.<field> !== input.<field>`.
enum RunSelector {
    /// Falsy — no constraint.
    Ignore,
    /// A truthy string — strict equality against the stored key.
    Key(JsString),
    /// A truthy non-string — strict equality with a stored string never
    /// holds, so every run is skipped.
    Never,
}

impl RunSelector {
    fn matches(&self, key: &JsString) -> bool {
        match self {
            Self::Ignore => true,
            Self::Key(wanted) => wanted == key,
            Self::Never => false,
        }
    }
}

fn match_selector(value: &JsValue) -> RunSelector {
    if !truthy(value) {
        return RunSelector::Ignore;
    }
    match value {
        JsValue::String(text) => RunSelector::Key(text.clone()),
        _ => RunSelector::Never,
    }
}

// ---------------------------------------------------------------- runtime

/// TS `CronRuntime` (CronRuntime.ts:70-563), the manager-facing half: the six
/// control methods, the session-override bookkeeping, start/stop recovery,
/// and the fire/scheduler halves `bindAgentGateway` constructs (#2295).
pub struct CronRuntime {
    config: CronManagerConfig,
    project_key: String,
    store: CronTaskStore,
    now: Arc<dyn Fn() -> f64 + Send + Sync>,
    uuid: Arc<dyn Fn() -> JsString + Send + Sync>,
    logger: Arc<dyn CronLogger>,
    /// The manager's #2181 zone cache, shared by every runtime it owns so
    /// `compute_next_run_at` resolves each zone's TZif once per process.
    zone_cache: Arc<CronZoneResolverCache>,
    session_overrides: Arc<Mutex<SessionConfigOverrides>>,
    gateway: Mutex<Option<Arc<dyn CronAgentGateway>>>,
    active_runs: Mutex<Vec<CronActiveRun>>,
    /// `this.scheduler` — the half `bindAgentGateway` creates. Held strongly:
    /// the fire closures hold the runtime only weakly, so this edge is the
    /// one keeping the scheduler thread's world alive after the manager
    /// clears its map (an in-flight fire keeps the runtime itself alive).
    scheduler: Mutex<Option<Arc<CronScheduler>>>,
    /// The owning manager, for the scheduler's manager-wide
    /// `activeRunCount` sum (CronRuntime.ts:123's `sharedActiveRunCount`).
    manager: Weak<CronManager>,
    /// The #2295 test seam threaded from `CronManagerOptions` — the 60s
    /// production constant by default.
    scheduler_idle_poll_ms: u64,
}

impl CronRuntime {
    #[allow(clippy::too_many_arguments)] // The manager's injected seams, one per field.
    fn new(
        config: CronManagerConfig,
        pilot_home: String,
        project_key: String,
        now: Arc<dyn Fn() -> f64 + Send + Sync>,
        uuid: Arc<dyn Fn() -> JsString + Send + Sync>,
        logger: Arc<dyn CronLogger>,
        zone_cache: Arc<CronZoneResolverCache>,
        session_overrides: Arc<Mutex<SessionConfigOverrides>>,
        manager: Weak<CronManager>,
        scheduler_idle_poll_ms: u64,
    ) -> Result<Self, CronError> {
        // Only the process-cwd read can fail, and both inputs are already
        // resolved absolute here, so this is the uv_cwd shape in practice.
        let paths = super::paths::resolve_cron_paths(&pilot_home, &project_key)
            .map_err(|error| CronError::io(error, "uv_cwd", ""))?;
        Ok(Self {
            config,
            project_key,
            store: CronTaskStore::new(paths),
            now,
            uuid,
            logger,
            zone_cache,
            session_overrides,
            gateway: Mutex::new(None),
            active_runs: Mutex::new(Vec::new()),
            scheduler: Mutex::new(None),
            manager,
            scheduler_idle_poll_ms,
        })
    }

    /// `this.projectKey` (CronRuntime.ts:92, `resolve(options.projectKey)`).
    pub fn project_key(&self) -> &str {
        &self.project_key
    }

    /// `this.paths` (CronRuntime.ts:73).
    pub fn paths(&self) -> &super::CronPaths {
        self.store.paths()
    }

    fn bound_gateway(&self) -> Option<Arc<dyn CronAgentGateway>> {
        self.gateway
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// `bindAgentGateway` (CronRuntime.ts:120-175): binds the gateway, then
    /// constructs the fire and scheduler halves over it, in the TS order.
    pub fn bind_agent_gateway(
        self: &Arc<Self>,
        gateway: Arc<dyn CronAgentGateway>,
    ) -> Result<(), CronError> {
        let mut bound = self.gateway.lock().unwrap_or_else(|e| e.into_inner());
        if bound.is_some() {
            return Err(CronError::error(JsString::from(
                "CronRuntime.bindAgentGateway already called.",
            )));
        }
        // The runtime closures run through a weak self — no constructor
        // edge keeps a dropped runtime's world alive. The fire threads
        // upgrade once per spawn and hold the strong handle for the run,
        // which is what TS's GC does for the fire promise chain.
        let runtime = Arc::downgrade(self);
        let callbacks = self
            .manager
            .upgrade()
            .map(|manager| manager.execution_callbacks())
            .unwrap_or_default();
        let fire = Arc::new(CronFire::new(CronFireDeps {
            gateway: gateway.clone(),
            store: CronTaskStore::new(self.store.paths().clone()),
            now: self.now.clone(),
            register_active_run: {
                let runtime = runtime.clone();
                Arc::new(move |run| {
                    if let Some(runtime) = runtime.upgrade() {
                        runtime.register_active_run(run);
                    }
                })
            },
            unregister_active_run: {
                let runtime = runtime.clone();
                Arc::new(move |run_id| {
                    runtime
                        .upgrade()
                        .and_then(|runtime| runtime.unregister_active_run(run_id))
                })
            },
            get_active_run: {
                let runtime = runtime.clone();
                Arc::new(move |run_id| {
                    runtime
                        .upgrade()
                        .and_then(|runtime| runtime.active_run(run_id))
                })
            },
            run_timeout_ms: self.config.run_timeout_minutes * 60_000.0,
            default_timezone: self.config.timezone.clone(),
            release_task_session: {
                let runtime = runtime.clone();
                Arc::new(move |task| {
                    if let Some(runtime) = runtime.upgrade() {
                        runtime.release_task_session(task);
                    }
                })
            },
            on_result_delivery: callbacks.on_result_delivery,
            on_phase_event: callbacks.on_phase_event,
            on_turn_event: callbacks.on_turn_event,
            assistant_text_limit_units: callbacks.assistant_text_limit_units,
            logger: self.logger.clone(),
            zone_cache: self.zone_cache.clone(),
        }));
        // CronRuntime.ts:164-174: `activeRunCount` is the manager-wide sum
        // (`sharedActiveRunCount?.()`), the runtime's own registry when the
        // manager is gone — the `?? this.activeRuns.size` arm.
        let manager = self.manager.clone();
        let active_run_count: Arc<dyn Fn() -> usize + Send + Sync> = Arc::new(move || {
            if let Some(manager) = manager.upgrade() {
                let state = manager.state.lock().unwrap_or_else(|e| e.into_inner());
                return state
                    .runtimes
                    .iter()
                    .map(|runtime| runtime.active_run_count())
                    .sum();
            }
            runtime
                .upgrade()
                .map(|runtime| runtime.active_run_count())
                .unwrap_or(0)
        });
        let scheduler = Arc::new(CronScheduler::new(SchedulerDeps {
            config: self.config.clone(),
            store: CronTaskStore::new(self.store.paths().clone()),
            fire,
            uuid: self.uuid.clone(),
            now: self.now.clone(),
            active_run_count,
            logger: self.logger.clone(),
            zone_cache: self.zone_cache.clone(),
            idle_poll_ms: self.scheduler_idle_poll_ms,
            runtime: Arc::downgrade(self),
        })?);
        *self.scheduler.lock().unwrap_or_else(|e| e.into_inner()) = Some(scheduler);
        *bound = Some(gateway);
        Ok(())
    }

    /// `this.scheduler` — the half `bindAgentGateway` created. The scheduler
    /// tests observe the armed timer through it (`armed_tick_delay_ms`, the
    /// `zone_resolver_cache` "Public for tests" precedent).
    pub(super) fn bound_scheduler(&self) -> Option<Arc<CronScheduler>> {
        self.scheduler
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// `this.scheduler?.poke()` (CronRuntime.ts:277,349,363,379).
    fn poke_scheduler(&self) {
        if let Some(scheduler) = self.bound_scheduler() {
            scheduler.poke();
        }
    }

    pub(super) fn execution_allowed(&self) -> bool {
        self.manager
            .upgrade()
            .is_none_or(|manager| manager.execution_allowed(&self.project_key))
    }

    /// CronRuntime.ts:182-195. The bind check stands in for TS's `scheduler`
    /// presence — the scheduler is exactly what `bindAgentGateway` creates.
    pub fn start(&self) -> Result<(), CronError> {
        if !self.execution_allowed() {
            return Ok(());
        }
        if !self.config.enabled {
            self.logger.info(
                "cron disabled in config; runtime is a no-op.",
                &JsObject::new(),
            );
            return Ok(());
        }
        if self.bound_gateway().is_none() {
            return Err(CronError::error(JsString::from(
                "CronRuntime.start called before bindGateway.",
            )));
        }
        self.migrate_legacy_task_sessions()?;
        self.recover_interrupted_runs()?;
        self.prepare_task_sessions()?;
        if let Some(scheduler) = self.bound_scheduler() {
            scheduler.start()?;
        }
        self.logger.info(
            "cron runtime started",
            &key_meta("projectKey", &JsString::from(self.project_key.clone())),
        );
        Ok(())
    }

    /// CronRuntime.ts:197-214: stop the scheduler first (`await
    /// this.scheduler?.stop()`), then mark every active run, abort each with
    /// `system:cron_shutdown` (rejections swallowed here), then release
    /// every task session. A `listTasks` failure propagates, as in TS.
    pub fn stop(&self) -> Result<(), CronError> {
        if let Some(scheduler) = self.bound_scheduler() {
            scheduler.stop();
        }
        if let Some(gateway) = self.bound_gateway() {
            let active = {
                let mut runs = self.active_runs.lock().unwrap_or_else(|e| e.into_inner());
                for run in runs.iter_mut() {
                    run.stop_requested = true;
                }
                runs.clone()
            };
            for run in &active {
                let _ = gateway.abort_turn(
                    &run.session_key,
                    Some(&run.run_id),
                    Some(&JsString::from("system:cron_shutdown")),
                );
            }
        }
        let tasks = self.store.list_tasks()?;
        for task in &tasks {
            self.release_task_session(task);
        }
        Ok(())
    }

    /// `getActiveRunCount` (CronRuntime.ts:216-218).
    pub fn active_run_count(&self) -> usize {
        self.active_runs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }

    /// `registerActiveRun` (CronRuntime.ts:412-414) — the `CronFire`
    /// dependency that owns a run's liveness; exposed for the fire half
    /// (fire.rs) and the fixtures. TS registers a run exactly once, in the
    /// calling thread before the fire's first await; the port's fire
    /// re-registers the entry the tick pre-registered, so a same-runId
    /// re-register is idempotent on the snapshot fields but must keep a
    /// `stop_requested` a stop task set in the window — TS's single event
    /// loop has no such window (CronFire.ts:58's register runs before any
    /// await can yield to a `stopTask`).
    pub fn register_active_run(&self, run: CronActiveRun) {
        let mut runs = self.active_runs.lock().unwrap_or_else(|e| e.into_inner());
        match runs
            .iter_mut()
            .find(|existing| existing.run_id == run.run_id)
        {
            Some(existing) => {
                let stop_requested = existing.stop_requested || run.stop_requested;
                *existing = run;
                existing.stop_requested = stop_requested;
            }
            None => runs.push(run),
        }
    }

    /// `unregisterActiveRun` (CronRuntime.ts:416-420).
    pub fn unregister_active_run(&self, run_id: &JsString) -> Option<CronActiveRun> {
        let mut runs = self.active_runs.lock().unwrap_or_else(|e| e.into_inner());
        let index = runs.iter().position(|run| &run.run_id == run_id)?;
        Some(runs.remove(index))
    }

    /// `getActiveRun`.
    pub fn active_run(&self, run_id: &JsString) -> Option<CronActiveRun> {
        self.active_runs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|run| &run.run_id == run_id)
            .cloned()
    }

    /// `findActiveRun` (CronRuntime.ts:422-436): the first insertion-order
    /// match. Each of `runId`/`taskId` is a truthiness-guarded strict
    /// equality — a falsy selector imposes no constraint, a truthy
    /// non-string can never equal a stored string, and both falsy matches
    /// nothing (the third guard).
    fn find_active_run(&self, input: &JsValue) -> Result<Option<CronActiveRun>, CronError> {
        let run_id = match_selector(property(input, "runId")?);
        let task_id = match_selector(property(input, "taskId")?);
        if let (RunSelector::Ignore, RunSelector::Ignore) = (&run_id, &task_id) {
            return Ok(None);
        }
        Ok(self
            .active_runs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|run| run_id.matches(&run.run_id) && task_id.matches(&run.task_id))
            .cloned())
    }

    // -------------------------------------------------------- six methods

    /// `createTask` (CronRuntime.ts:220-279).
    pub fn create_task(&self, input: &JsValue) -> Result<JsValue, CronError> {
        self.create_task_at(input, (self.now)())
    }

    /// `createTask` against an already-read clock (#2047). `runTaskNow` builds
    /// its `runAt` from the wall clock and then calls `createTask`, whose
    /// `now` check and `createdAt`/`nextRunAt` must be the *same* instant:
    /// reading the clock twice lets a millisecond tick in between turn a valid
    /// run-now into the TS-faithful "One-time Cron tasks must be scheduled in
    /// the future." rejection. That intermittent rejection is the bug being
    /// removed, not a behaviour worth keeping.
    ///
    /// Facts this rests on, without inferring past them: the golden cell
    /// `run-now-shape` (`tests/fixtures/native-cron/oracle.json`, recorded
    /// from real TS at cc7c4e6) records `{"started":true,…}` for this exact
    /// input; the removed path is reachable, not theoretical — the window is
    /// one millisecond wide and a run-now straddled it in 6 of 180 calls on a
    /// loaded runner (6/180 ≈ 3.3%; 「加探针后 3/120」, see the #2047 triage);
    /// and the golden was recorded under a pinned clock, so it does not by
    /// itself exercise the cross-millisecond path. The seam is therefore an
    /// approved accepted difference (2026-11-07, AI 代批，待人复核), and the
    /// append-only `specs/gateway-parity` entry for it is tracked by #2096.
    /// The deterministic regression test is
    /// `run_now_shares_one_clock_read_with_the_task_it_creates`.
    fn create_task_at(&self, input: &JsValue, now_ms: f64) -> Result<JsValue, CronError> {
        if !self.config.enabled {
            return Err(CronError::error(JsString::from(
                "Cron is disabled. Enable it in pilotdeck.yaml to create tasks.",
            )));
        }
        let task_id = (self.uuid)();
        let session_key = build_cron_session_key(&JsValue::String(task_id.clone()));
        let schedule = self.normalize_schedule(input, now_ms)?;
        let timezone = match &schedule {
            NormalizedSchedule::Cron { timezone, .. } => value_string(timezone.clone()),
            NormalizedSchedule::Once { .. } => {
                nullish_or(property(input, "timezone")?, &self.config.timezone)
            }
        };
        let Some((next_ms, next_iso)) = self.computed_next_run_at(&schedule, now_ms)? else {
            return Err(CronError::error(JsString::from(
                "Cron schedule does not produce a valid future run time.",
            )));
        };
        if matches!(schedule, NormalizedSchedule::Once { .. }) && next_ms < now_ms {
            return Err(CronError::error(JsString::from(
                "One-time Cron tasks must be scheduled in the future.",
            )));
        }
        let mut task = JsObject::new();
        task.insert("schemaVersion".into(), JsValue::Number(1.0));
        task.insert("taskId".into(), JsValue::String(task_id));
        task.insert("message".into(), property(input, "message")?.clone());
        task.insert("schedule".into(), schedule.as_task_value());
        task.insert("status".into(), JsValue::from("scheduled"));
        task.insert("sessionKey".into(), JsValue::String(session_key));
        task.insert("channelKey".into(), JsValue::from("cron"));
        task.insert(
            "originSessionKey".into(),
            property(input, "sessionKey")?.clone(),
        );
        task.insert(
            "originChannelKey".into(),
            property(input, "channelKey")?.clone(),
        );
        // Session-scoped callers should pass the originating project
        // explicitly. Keep the runtime root only as a compatibility fallback
        // for direct callers (CronRuntime.ts:248-250).
        task.insert(
            "projectKey".into(),
            nullish_or(property(input, "projectKey")?, &self.project_key),
        );
        task.insert("mode".into(), property(input, "mode")?.clone());
        task.insert("timezone".into(), timezone);
        let now_iso = iso_from_epoch_ms(now_ms);
        task.insert("createdAt".into(), value_string(now_iso.clone()));
        task.insert("updatedAt".into(), value_string(now_iso));
        task.insert("nextRunAt".into(), value_string(next_iso));
        task.insert("revision".into(), JsValue::Number(0.0));
        task.insert(
            "scheduleComputationVersion".into(),
            match &schedule {
                NormalizedSchedule::Cron { .. } => {
                    JsValue::Number(f64::from(CRON_SCHEDULE_COMPUTATION_VERSION))
                }
                NormalizedSchedule::Once { .. } => JsValue::Undefined,
            },
        );
        let task = JsValue::Object(task);
        self.register_task_session(&task);
        if let Err(error) = self.store.put_task(&task) {
            self.release_task_session(&task);
            return Err(error.into());
        }
        self.poke_scheduler();
        let mut out = JsObject::new();
        out.insert("task".into(), task);
        Ok(JsValue::Object(out))
    }

    /// `listTasks` (CronRuntime.ts:281-288). `recentRuns` is assigned after
    /// `tasks`, so it lands second in the result object.
    pub fn list_tasks(&self, input: &JsValue) -> Result<JsValue, CronError> {
        let effective = default_input(input);
        let tasks = self.store.list_tasks()?;
        let mut out = JsObject::new();
        out.insert("tasks".into(), JsValue::Array(tasks));
        if truthy(property(&effective, "includeHistory")?) {
            let runs = self.store.list_runs(history_limit(&effective)?)?;
            out.insert("recentRuns".into(), JsValue::Array(runs));
        }
        Ok(JsValue::Object(out))
    }

    /// `updateTask` (CronRuntime.ts:290-351).
    pub fn update_task(&self, input: &JsValue) -> Result<JsValue, CronError> {
        if !self.config.enabled {
            return Err(CronError::error(JsString::from(
                "Cron is disabled. Enable it in pilotdeck.yaml to update tasks.",
            )));
        }
        // `typeof input.projectKey !== "string" || !input.projectKey.trim()
        // || !this.matchesProject(...)` — all three fold into not_found.
        let project_key = match property(input, "projectKey")? {
            JsValue::String(key) if !metadata::js_trim(&js_lossy(key)).is_empty() => Some(key),
            _ => None,
        };
        let Some(project_key) = project_key else {
            return Ok(not_updated("not_found"));
        };
        if !self.matches_project(project_key)? {
            return Ok(not_updated("not_found"));
        }
        match property(input, "expectedRevision")? {
            // `Number.isSafeInteger` and `>= 0`.
            JsValue::Number(number)
                if number.is_finite()
                    && number.fract() == 0.0
                    && number.abs() <= 9_007_199_254_740_991.0
                    && *number >= 0.0 => {}
            _ => {
                return Err(CronError::error(JsString::from(
                    "Cron task expectedRevision must be a non-negative integer.",
                )));
            }
        }
        let expected_revision = property(input, "expectedRevision")?
            .as_number()
            .expect("validated above");
        match property(input, "message")? {
            JsValue::String(message) if !metadata::js_trim(&js_lossy(message)).is_empty() => {}
            _ => {
                return Err(CronError::error(JsString::from(
                    "Cron task message is required.",
                )));
            }
        }
        match property(input, "timezone")? {
            JsValue::Undefined => {}
            JsValue::String(timezone) if is_valid_cron_timezone(&js_lossy(timezone)) => {}
            other => return Err(CronError::error(invalid_timezone_message(other)?)),
        }
        let now_ms = (self.now)();
        let schedule = self.normalize_schedule(input, now_ms)?;
        let timezone = match &schedule {
            NormalizedSchedule::Cron { timezone, .. } => value_string(timezone.clone()),
            NormalizedSchedule::Once { .. } => {
                nullish_or(property(input, "timezone")?, &self.config.timezone)
            }
        };
        let Some((next_ms, next_iso)) = self.computed_next_run_at(&schedule, now_ms)? else {
            return Err(CronError::error(JsString::from(
                "Cron schedule does not produce a valid future run time.",
            )));
        };
        if matches!(schedule, NormalizedSchedule::Once { .. }) && next_ms < now_ms {
            return Err(CronError::error(JsString::from(
                "One-time Cron tasks must be scheduled in the future.",
            )));
        }
        let input_project_key = resolve_key_string(project_key)?;
        // The closure cannot use `?` (the store takes a plain `Fn` returning
        // `Option`), so every input read happens before it; `input` has
        // already passed the object checks above.
        let message_value = property(input, "message")?.clone();
        let Some(task_id) = property(input, "taskId")?
            .as_string()
            .and_then(|task_id| task_id.to_utf8().ok())
        else {
            // The store compares `taskId` with `===`, so a non-string (or a
            // lone-surrogate) taskId never matches a stored task.
            return Ok(not_updated("not_found"));
        };
        let reason = Cell::new(None);
        let closure_error = RefCell::new(None);
        let updated = self.store.update_task(&task_id, |current| {
            let current_project_key = match current.get("projectKey").unwrap_or(&UNDEFINED) {
                JsValue::Undefined | JsValue::Null => self.project_key.clone(),
                JsValue::String(key) if !metadata::js_trim(&js_lossy(key)).is_empty() => {
                    match resolve_key_string(key) {
                        Ok(resolved) => resolved,
                        Err(error) => {
                            *closure_error.borrow_mut() = Some(error);
                            return Some(current.clone());
                        }
                    }
                }
                // `current.projectKey?.trim()` on any other value throws
                // V8's call-of-undefined TypeError (the property path is the
                // message, not the value; probed on Node v22.22.0).
                _ => {
                    *closure_error.borrow_mut() = Some(CronError::type_error(
                        "current.projectKey?.trim is not a function",
                    ));
                    return Some(current.clone());
                }
            };
            if current_project_key != self.project_key || current_project_key != input_project_key {
                reason.set(Some("not_found"));
                return Some(current.clone());
            }
            if current
                .get("status")
                .and_then(JsValue::as_string)
                .is_some_and(|status| status.equals_str("running"))
            {
                reason.set(Some("running"));
                return Some(current.clone());
            }
            if revision_of(current) != expected_revision {
                reason.set(Some("conflict"));
                return Some(current.clone());
            }
            let updated = spread_task(
                current,
                &[
                    ("message", message_value.clone()),
                    ("schedule", schedule.as_task_value()),
                    ("timezone", timezone.clone()),
                    ("nextRunAt", value_string(next_iso.clone())),
                    ("updatedAt", value_string(iso_from_epoch_ms(now_ms))),
                    ("revision", JsValue::Number(revision_of(current) + 1.0)),
                    (
                        "scheduleComputationVersion",
                        match &schedule {
                            NormalizedSchedule::Cron { .. } => {
                                JsValue::Number(f64::from(CRON_SCHEDULE_COMPUTATION_VERSION))
                            }
                            NormalizedSchedule::Once { .. } => JsValue::Undefined,
                        },
                    ),
                ],
            );
            Some(JsValue::Object(updated))
        });
        if let Some(error) = closure_error.into_inner() {
            // The TS closure throws straight through `store.updateTask`.
            return Err(error);
        }
        match updated {
            Err(error) => Err(error.into()),
            Ok(None) => Ok(not_updated("not_found")),
            Ok(Some(_)) if reason.get().is_some() => Ok(not_updated(reason.get().unwrap())),
            Ok(Some(task)) => {
                self.poke_scheduler();
                let mut out = JsObject::new();
                out.insert("updated".into(), JsValue::Bool(true));
                out.insert("task".into(), task);
                Ok(JsValue::Object(out))
            }
        }
    }

    /// `deleteTask` (CronRuntime.ts:353-365).
    pub fn delete_task(&self, input: &JsValue) -> Result<JsValue, CronError> {
        let mut stopped_run_id: Option<JsValue> = None;
        if truthy(property(input, "stopRunning")?) {
            // stopTask({ taskId: input.taskId }) — runId/projectKey omitted.
            let mut stop_input = JsObject::new();
            stop_input.insert("taskId".into(), property(input, "taskId")?.clone());
            let stopped = self.stop_task(&JsValue::Object(stop_input))?;
            stopped_run_id = stopped
                .get("runId")
                .cloned()
                .filter(|value| !value.is_undefined());
        }
        let deleted = match property(input, "taskId")?
            .as_string()
            .and_then(|task_id| task_id.to_utf8().ok())
        {
            Some(task_id) => self.store.delete_task(&task_id).map_err(CronError::from)?,
            // A non-string taskId never matches the store's `===`.
            None => false,
        };
        if deleted {
            self.release_task_session_by_id(property(input, "taskId")?);
        }
        self.poke_scheduler();
        let mut out = JsObject::new();
        out.insert("deleted".into(), JsValue::Bool(deleted));
        if let Some(stopped_run_id) = stopped_run_id {
            out.insert("stoppedRunId".into(), stopped_run_id);
        }
        Ok(JsValue::Object(out))
    }

    /// `stopTask` (CronRuntime.ts:367-386).
    pub fn stop_task(&self, input: &JsValue) -> Result<JsValue, CronError> {
        let Some(active) = self.find_active_run(input)?.filter(|_| {
            // `!active || !this.agentGateway` — both fall to the idle result.
            self.bound_gateway().is_some()
        }) else {
            return Ok(stopped_no_run(
                property(input, "taskId")?,
                property(input, "runId")?,
            ));
        };
        let gateway = self.bound_gateway().expect("checked above");
        {
            let mut runs = self.active_runs.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(run) = runs.iter_mut().find(|run| run.run_id == active.run_id) {
                run.stop_requested = true;
            }
        }
        // abortTurn({ sessionKey, runId }) — no reason, and unlike stop()
        // this call site propagates a rejection.
        gateway.abort_turn(&active.session_key, Some(&active.run_id), None)?;
        let mut deleted_one_time = false;
        if active.schedule_type.equals_str("once") {
            deleted_one_time = match active.task_id.to_utf8() {
                Ok(task_id) => self.store.delete_task(&task_id).map_err(CronError::from)?,
                // A lone-surrogate taskId cannot cross the &str store seam.
                Err(_) => false,
            };
            self.release_task_session_by_id(&JsValue::String(active.task_id.clone()));
        }
        self.poke_scheduler();
        let mut out = JsObject::new();
        out.insert("stopped".into(), JsValue::Bool(true));
        out.insert("taskId".into(), JsValue::String(active.task_id.clone()));
        out.insert("runId".into(), JsValue::String(active.run_id.clone()));
        out.insert("deletedOneTimeTask".into(), JsValue::Bool(deleted_one_time));
        Ok(JsValue::Object(out))
    }

    /// `runTaskNow` (CronRuntime.ts:388-403). TS stamps the one-time task it
    /// creates with `new Date()` here and `Date.now()` again inside
    /// `createTask`; #2047 reads the injected clock once and hands the same
    /// instant to both, so a millisecond tick between the two reads can no
    /// longer turn a valid run-now into the "One-time Cron tasks must be
    /// scheduled in the future." rejection. The production clock is
    /// `store::now_millis` (assembly leaves `CronManagerOptions::now` unset),
    /// i.e. the same wall clock TS reads; tests pin it. The success cell stays
    /// byte-exact because its result carries no timestamp.
    pub fn run_task_now(&self, input: &JsValue) -> Result<JsValue, CronError> {
        let tasks = self.store.list_tasks()?;
        let task_id = property(input, "taskId")?;
        let Some(task) = tasks
            .iter()
            .find(|task| strict_equals(task.get("taskId"), Some(task_id)))
        else {
            let mut out = JsObject::new();
            out.insert("started".into(), JsValue::Bool(false));
            out.insert("reason".into(), JsValue::from("not_found"));
            return Ok(JsValue::Object(out));
        };
        if task
            .get("status")
            .and_then(JsValue::as_string)
            .is_some_and(|status| status.equals_str("running"))
        {
            let mut out = JsObject::new();
            out.insert("started".into(), JsValue::Bool(false));
            out.insert("reason".into(), JsValue::from("already_running"));
            out.insert(
                "taskId".into(),
                task.get("taskId").cloned().unwrap_or(JsValue::Undefined),
            );
            return Ok(JsValue::Object(out));
        }
        let mut run_at_schedule = JsObject::new();
        run_at_schedule.insert("type".into(), JsValue::from("once"));
        // #2047: TS reads `new Date()` here and `Date.now()` again inside
        // `createTask`; a millisecond tick between the two makes the freshly
        // built one-time schedule look stale and the run-now answers with
        // "One-time Cron tasks must be scheduled in the future." instead of
        // starting. Read the clock once and hand the same instant to
        // `createTask` so the answer no longer depends on where the tick
        // lands. `iso_from_epoch_ms` floors, so the schedule value is already
        // integral and compares equal to this `now` (never "in the past").
        let now_ms = (self.now)().floor();
        run_at_schedule.insert("runAt".into(), value_string(iso_from_epoch_ms(now_ms)));
        let mut create_input = JsObject::new();
        create_input.insert(
            "message".into(),
            task.get("message").cloned().unwrap_or(JsValue::Undefined),
        );
        create_input.insert("schedule".into(), JsValue::Object(run_at_schedule));
        // The remaining keys read as `undefined` when absent, which is the
        // same thing to createTask's `??` fallbacks.
        for (name, field) in [
            ("projectKey", "projectKey"),
            ("sessionKey", "originSessionKey"),
            ("channelKey", "originChannelKey"),
            ("mode", "mode"),
        ] {
            if let Some(value) = task.get(field).filter(|value| !value.is_undefined()) {
                create_input.insert(name.into(), value.clone());
            }
        }
        let created = self.create_task_at(&JsValue::Object(create_input), now_ms)?;
        let mut out = JsObject::new();
        out.insert("started".into(), JsValue::Bool(true));
        out.insert(
            "taskId".into(),
            created
                .get("task")
                .and_then(|task| task.get("taskId").cloned())
                .unwrap_or(JsValue::Undefined),
        );
        Ok(JsValue::Object(out))
    }

    /// `runTickOnce` (CronRuntime.ts:405-410).
    pub fn run_tick_once(&self) -> Result<(), CronError> {
        let Some(scheduler) = self.bound_scheduler() else {
            return Err(CronError::error(JsString::from(
                "CronRuntime.runTickOnce called before bindGateway.",
            )));
        };
        scheduler.run_tick_once()
    }

    // ------------------------------------------------------------ private

    /// `matchesProject` (CronRuntime.ts:438-440).
    fn matches_project(&self, project_key: &JsString) -> Result<bool, CronError> {
        Ok(resolve_key_string(project_key)? == self.project_key)
    }

    /// `normalizeSchedule` (CronRuntime.ts:572-597).
    fn normalize_schedule(
        &self,
        input: &JsValue,
        now_ms: f64,
    ) -> Result<NormalizedSchedule, CronError> {
        let schedule = property(input, "schedule")?;
        match property(schedule, "type")? {
            JsValue::String(kind) if kind.equals_str("once") => Ok(NormalizedSchedule::Once {
                run_at: property(schedule, "runAt")?.clone(),
            }),
            JsValue::String(kind) if kind.equals_str("delay") => {
                let (amount, unit) = delay_schedule_fields(schedule)?;
                let run_at = delay_run_at_iso(now_ms, amount, unit)?;
                Ok(NormalizedSchedule::Once {
                    run_at: value_string(run_at),
                })
            }
            _ => {
                let expression = property(schedule, "expression")?.clone();
                // `input.schedule.timezone ?? input.timezone`.
                let requested = match property(schedule, "timezone")? {
                    JsValue::Undefined | JsValue::Null => property(input, "timezone")?.clone(),
                    timezone => timezone.clone(),
                };
                if truthy(&requested)
                    && !matches!(&requested, JsValue::String(timezone)
                        if is_valid_cron_timezone(&js_lossy(timezone)))
                {
                    return Err(CronError::error(invalid_timezone_message(&requested)?));
                }
                let requested = match &requested {
                    JsValue::String(timezone) => Some(js_lossy(timezone)),
                    _ => None,
                };
                let timezone = resolve_cron_timezone(
                    requested.as_deref(),
                    None,
                    Some(self.config.timezone.as_str()),
                );
                Ok(NormalizedSchedule::Cron {
                    expression,
                    timezone,
                })
            }
        }
    }

    /// `computeNextRunAt(schedule, now, timezone)` over this runtime's zone
    /// cache — the shared free function below.
    fn computed_next_run_at(
        &self,
        schedule: &NormalizedSchedule,
        now_ms: f64,
    ) -> Result<Option<(f64, String)>, CronError> {
        computed_next_run_at_with(schedule, now_ms, &self.zone_cache)
    }

    /// `migrateLegacyTaskSessions` (CronRuntime.ts:442-466).
    fn migrate_legacy_task_sessions(&self) -> Result<(), CronError> {
        let tasks = self.store.list_tasks()?;
        let mut migrated_count = 0.0;
        for task in &tasks {
            let next_session_key = build_cron_session_key(task.get("taskId").unwrap_or(&UNDEFINED));
            if task.get("sessionKey").and_then(JsValue::as_string) == Some(&next_session_key)
                && task
                    .get("channelKey")
                    .and_then(JsValue::as_string)
                    .is_some_and(|channel| channel.equals_str("cron"))
            {
                continue;
            }
            migrated_count += 1.0;
            self.release_session_key_only(task);
            self.close_session_swallowed(task, "cron/legacy-session-migrated");
            let migrated = spread_task(
                task,
                &[
                    ("sessionKey", JsValue::String(next_session_key)),
                    ("channelKey", JsValue::from("cron")),
                    ("revision", JsValue::Number(revision_of(task) + 1.0)),
                    ("updatedAt", value_string(iso_from_epoch_ms((self.now)()))),
                ],
            );
            self.store.put_task(&JsValue::Object(migrated))?;
        }
        if migrated_count > 0.0 {
            let mut meta = JsObject::new();
            meta.insert("migratedCount".into(), JsValue::Number(migrated_count));
            self.logger
                .info("cron runtime migrated legacy task sessions", &meta);
        }
        Ok(())
    }

    /// `prepareTaskSessions` (CronRuntime.ts:477-485).
    fn prepare_task_sessions(&self) -> Result<(), CronError> {
        let tasks = self.store.list_tasks()?;
        for task in &tasks {
            self.register_task_session(task);
            self.close_session_swallowed(task, "cron/unattended-policy-refresh");
        }
        Ok(())
    }

    /// `releaseTaskSession` (CronRuntime.ts:487-492).
    fn release_task_session(&self, task: &JsValue) {
        self.release_session_key_only(task);
        self.close_session_swallowed(task, "cron/task-finished");
    }

    /// `releaseTaskSessionById` (CronRuntime.ts:494-500).
    fn release_task_session_by_id(&self, task_id: &JsValue) {
        let session_key = build_cron_session_key(task_id);
        self.session_overrides
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .delete(&session_key);
        if let Some(gateway) = self.bound_gateway() {
            let _ = gateway.close_session(&session_key, Some(&JsString::from("cron/task-removed")));
        }
    }

    /// The `sessionOverrides.delete` half of every release pair. A non-string
    /// `sessionKey` never reaches here through the store (normalize_task
    /// requires the string); the TS `Map#delete` would be a no-op for it
    /// against the string keys registration ever sets.
    fn release_session_key_only(&self, task: &JsValue) {
        if let Some(session_key) = task.get("sessionKey").and_then(JsValue::as_string) {
            self.session_overrides
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .delete(session_key);
        }
    }

    /// The `agentGateway?.closeSession(...).catch(() => undefined)` half.
    fn close_session_swallowed(&self, task: &JsValue, reason: &str) {
        if let (Some(gateway), Some(session_key)) = (
            self.bound_gateway(),
            task.get("sessionKey").and_then(JsValue::as_string),
        ) {
            let _ = gateway.close_session(session_key, Some(&JsString::from(reason)));
        }
    }

    /// `registerTaskSession` (CronRuntime.ts:468-475). A non-string
    /// `sessionKey` never reaches here through the store; the TS `Map#set`
    /// would key it, the port skips it.
    fn register_task_session(&self, task: &JsValue) {
        let Some(session_key) = task.get("sessionKey").and_then(JsValue::as_string) else {
            return;
        };
        let value = SessionValue::object();
        value.set("permissionMode", JsValue::from("bypassPermissions"));
        value.set("bypassAvailable", JsValue::Bool(true));
        value.set("canPrompt", JsValue::Bool(false));
        value.set(
            "excludeTools",
            SessionValue::array(
                UNATTENDED_SESSION_EXCLUDED_TOOLS
                    .iter()
                    .map(|tool| SessionValue::from(JsValue::from(*tool)))
                    .collect(),
            ),
        );
        self.session_overrides
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .set(session_key, &value);
    }

    /// `recoverInterruptedRuns` (CronRuntime.ts:502-562).
    fn recover_interrupted_runs(&self) -> Result<(), CronError> {
        let now_ms = (self.now)();
        let tasks = self.store.list_tasks()?;
        // listRuns(Number.MAX_SAFE_INTEGER) — every record.
        let terminal_run_ids: Vec<JsString> = self
            .store
            .list_runs(Some(i64::MAX))?
            .into_iter()
            .filter(|run| {
                truthy(run.get("finishedAt").unwrap_or(&UNDEFINED))
                    && truthy(run.get("outcome").unwrap_or(&UNDEFINED))
            })
            .filter_map(|run| run.get("runId").and_then(JsValue::as_string).cloned())
            .collect();
        let mut recovered_count = 0.0;
        for task in &tasks {
            if !task
                .get("status")
                .and_then(JsValue::as_string)
                .is_some_and(|status| status.equals_str("running"))
            {
                continue;
            }
            recovered_count += 1.0;
            let last_run_id = task.get("lastRunId").unwrap_or(&UNDEFINED);
            if truthy(last_run_id)
                && !last_run_id
                    .as_string()
                    .is_some_and(|run_id| terminal_run_ids.contains(run_id))
            {
                // `Number.isNaN(new Date(task.updatedAt).getTime())
                //   ? now.toISOString() : task.updatedAt` — the raw stored
                // string survives when it parses.
                let started_at = match task.get("updatedAt").and_then(JsValue::as_string) {
                    Some(updated_at)
                        if matches!(
                            once_computed_run_at(updated_at, &resolve_local),
                            ComputedRunAt::Valid(_)
                        ) =>
                    {
                        JsValue::String(updated_at.clone())
                    }
                    _ => value_string(iso_from_epoch_ms(now_ms)),
                };
                let mut error = JsObject::new();
                error.insert("code".into(), JsValue::from("cron_run_interrupted"));
                error.insert(
                    "message".into(),
                    JsValue::from("Cron run was interrupted before the runtime restarted."),
                );
                let mut record = JsObject::new();
                record.insert("schemaVersion".into(), JsValue::Number(1.0));
                record.insert("runId".into(), last_run_id.clone());
                record.insert(
                    "taskId".into(),
                    task.get("taskId").cloned().unwrap_or(JsValue::Undefined),
                );
                record.insert(
                    "sessionKey".into(),
                    task.get("sessionKey")
                        .cloned()
                        .unwrap_or(JsValue::Undefined),
                );
                record.insert(
                    "projectKey".into(),
                    task.get("projectKey")
                        .cloned()
                        .unwrap_or(JsValue::Undefined),
                );
                record.insert("startedAt".into(), started_at);
                record.insert("finishedAt".into(), value_string(iso_from_epoch_ms(now_ms)));
                record.insert("outcome".into(), JsValue::from("failed"));
                record.insert("error".into(), JsValue::Object(error));
                self.store.append_run(&JsValue::Object(record))?;
            }
            if task
                .get("schedule")
                .and_then(|schedule| schedule.get("type"))
                .and_then(JsValue::as_string)
                .is_some_and(|kind| kind.equals_str("once"))
            {
                if let Some(task_id) = task
                    .get("taskId")
                    .and_then(JsValue::as_string)
                    .and_then(|task_id| task_id.to_utf8().ok())
                {
                    self.store.delete_task(&task_id).map_err(CronError::from)?;
                }
                self.release_task_session(task);
                continue;
            }
            // resolveCronTimezone(task.schedule.timezone, task.timezone,
            // this.config.timezone) — a non-string value is never a valid
            // zone, so it falls through like TS's `Set#has`.
            let schedule_timezone = timezone_str(
                task.get("schedule")
                    .and_then(|schedule| schedule.get("timezone")),
            );
            let task_timezone = timezone_str(task.get("timezone"));
            let timezone = resolve_cron_timezone(
                schedule_timezone.as_deref(),
                task_timezone.as_deref(),
                Some(self.config.timezone.as_str()),
            );
            // {...task.schedule, timezone} — replaced in place, appended
            // when the stored schedule has no timezone key.
            let mut schedule = task
                .get("schedule")
                .and_then(JsValue::as_object)
                .expect("normalize_task keeps a truthy schedule object")
                .clone();
            schedule.insert("timezone".into(), value_string(timezone.clone()));
            let schedule_value = JsValue::Object(schedule);
            let next_run_at = match self.computed_next_run_at(
                &task_schedule_shape(&schedule_value, &timezone, now_ms)?,
                now_ms,
            )? {
                Some((_, iso)) => value_string(iso),
                None => JsValue::Undefined,
            };
            // `nextRunAt: computeNextRunAt(...)?.toISOString()` — the `?.`
            // keeps the key with an `undefined` value, which every
            // serialization drops.
            let recovered = spread_task(
                task,
                &[
                    ("schedule", schedule_value),
                    ("timezone", value_string(timezone)),
                    ("status", JsValue::from("scheduled")),
                    ("nextRunAt", next_run_at),
                    ("revision", JsValue::Number(revision_of(task) + 1.0)),
                    (
                        "scheduleComputationVersion",
                        JsValue::Number(f64::from(CRON_SCHEDULE_COMPUTATION_VERSION)),
                    ),
                    ("updatedAt", value_string(iso_from_epoch_ms(now_ms))),
                ],
            );
            self.store.put_task(&JsValue::Object(recovered))?;
        }
        if recovered_count > 0.0 {
            let mut meta = JsObject::new();
            meta.insert("recoveredCount".into(), JsValue::Number(recovered_count));
            self.logger
                .warn("cron runtime recovered interrupted runs", &meta);
        }
        Ok(())
    }
}

impl Drop for CronRuntime {
    /// Waking the scheduler's timer seat on the way out: the thread waits on
    /// a condvar between arms and only re-checks liveness when woken, so the
    /// drop is what lets it notice the world is gone instead of waiting
    /// forever (TS GC just collects the loop).
    fn drop(&mut self) {
        if let Some(scheduler) = self
            .scheduler
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            scheduler.wake();
        }
    }
}

/// The `{type:"delay", amount, unit}` fields (CronRuntime.ts:576-580): the
/// amount must be a number (`delayToMilliseconds`'s `Number.isFinite` gate
/// folds anything else into the "positive finite" error); the unit passes
/// through unvalidated — `DELAY_UNIT_MS[<anything else>]` is `undefined`, so
/// the multiply below is NaN.
fn delay_schedule_fields(schedule: &JsValue) -> Result<(f64, Option<DelayUnit>), CronError> {
    let amount = match property(schedule, "amount")? {
        JsValue::Number(number) => number,
        _ => {
            return Err(CronError::error(JsString::from(
                "Cron delay schedule must use a positive finite amount.",
            )));
        }
    };
    let unit = match property(schedule, "unit")?.as_string() {
        Some(unit) if unit.equals_str("second") => Some(DelayUnit::Second),
        Some(unit) if unit.equals_str("minute") => Some(DelayUnit::Minute),
        Some(unit) if unit.equals_str("hour") => Some(DelayUnit::Hour),
        Some(unit) if unit.equals_str("day") => Some(DelayUnit::Day),
        _ => None,
    };
    Ok((*amount, unit))
}

/// The delay fold both writers share: `new Date(after.getTime() +
/// delayMs).toISOString()` (CronRuntime.ts:581 and the recovery write at
/// :552). `delayToMilliseconds` gates the amount; a unit other than the four
/// keys multiplies as NaN, `new Date(NaN)` is an Invalid Date — not nullish,
/// so the `?.` never saves it — and its `toISOString()` throws
/// `RangeError: Invalid time value`.
fn delay_run_at_iso(
    now_ms: f64,
    amount: f64,
    unit: Option<DelayUnit>,
) -> Result<String, CronError> {
    if !amount.is_finite() || amount <= 0.0 {
        return Err(CronError::error(JsString::from(
            "Cron delay schedule must use a positive finite amount.",
        )));
    }
    let delay_ms = unit.map_or(f64::NAN, |unit| {
        delay_to_milliseconds(amount, unit).expect("the guard above keeps the fold Some")
    });
    let raw = now_ms + delay_ms;
    if raw.is_nan() {
        return Err(CronError::range_error(JsString::from("Invalid time value")));
    }
    // An out-of-TimeClip sum throws the same RangeError in TS; the
    // registered A6.4-CLOCK-TIMECLIP-001 divergence saturates instead
    // (only an injected clock or a huge delay reaches it).
    Ok(match js_new_date(raw) {
        ComputedRunAt::Valid(epoch_ms) => iso_from_epoch_ms(epoch_ms),
        _ => pilotdeck_transport::system_clock::format_epoch_iso(raw as i64),
    })
}

/// `buildCronSessionKey` (CronRuntime.ts:599-601): the template literal, so
/// a non-string taskId goes through `String()`.
fn build_cron_session_key(task_id: &JsValue) -> JsString {
    let mut units = "cron:".encode_utf16().collect::<Vec<_>>();
    match to_display(task_id) {
        Ok(text) => units.extend_from_slice(text.units()),
        Err(_) => units.extend("undefined".encode_utf16()),
    }
    JsString::from_utf16(units)
}

fn not_updated(reason: &str) -> JsValue {
    let mut out = JsObject::new();
    out.insert("updated".into(), JsValue::Bool(false));
    out.insert("reason".into(), JsValue::from(reason));
    JsValue::Object(out)
}

fn stopped_no_run(task_id: &JsValue, run_id: &JsValue) -> JsValue {
    let mut out = JsObject::new();
    out.insert("stopped".into(), JsValue::Bool(false));
    if !task_id.is_undefined() {
        out.insert("taskId".into(), task_id.clone());
    }
    if !run_id.is_undefined() {
        out.insert("runId".into(), run_id.clone());
    }
    JsValue::Object(out)
}

/// `listTasks(input = {})` (CronRuntime.ts:281): `undefined` takes the
/// default, `null` still throws on the property read.
fn default_input(input: &JsValue) -> JsValue {
    if input.is_undefined() {
        JsValue::Object(JsObject::new())
    } else {
        input.clone()
    }
}

/// `value ?? fallback` for the persisted timezone field: the raw wire value
/// passes through (a non-string stays a non-string, as in TS), only nullish
/// falls back.
fn nullish_or(value: &JsValue, fallback: &str) -> JsValue {
    match value {
        JsValue::Undefined | JsValue::Null => JsValue::from(fallback),
        other => other.clone(),
    }
}

/// `Invalid Cron timezone: ${input.timezone}` — the raw value rendered by
/// `String()`.
fn invalid_timezone_message(timezone: &JsValue) -> Result<JsString, CronError> {
    template_message("Invalid Cron timezone: ", "", timezone)
}

/// `<prefix>${value}<suffix>` with `String()` semantics on the value.
fn template_message(prefix: &str, suffix: &str, value: &JsValue) -> Result<JsString, CronError> {
    let mut units = prefix.encode_utf16().collect::<Vec<_>>();
    units.extend_from_slice(to_display(value)?.units());
    units.extend(suffix.encode_utf16());
    Ok(JsString::from_utf16(units))
}

/// `current.revision ?? 0` (CronRuntime.ts:331,342): the store's normalized
/// revision is always a safe non-negative integer.
pub(super) fn revision_of(task: &JsValue) -> f64 {
    task.get("revision")
        .and_then(JsValue::as_number)
        .unwrap_or(0.0)
}

/// `{...current, <overrides>}` over the stored task: `normalizeTask`
/// (CronTaskStore.ts:201-224) always creates every key — the undefined-valued
/// ones included, dropped only by serialization — so an overridden key keeps
/// its slot and the result stays in the literal order. The port's store drops
/// undefined values, so the spread rebuilds the literal order instead of
/// letting missing slots append.
pub(super) fn spread_task(current: &JsValue, overrides: &[(&str, JsValue)]) -> JsObject {
    let mut out = JsObject::new();
    for key in [
        "schemaVersion",
        "taskId",
        "message",
        "schedule",
        "status",
        "sessionKey",
        "channelKey",
        "projectKey",
        "mode",
        "timezone",
        "createdAt",
        "updatedAt",
        "nextRunAt",
        "lastRunId",
        "revision",
        "scheduleComputationVersion",
        "originSessionKey",
        "originChannelKey",
    ] {
        let value = overrides
            .iter()
            .find(|(name, _)| *name == key)
            .map(|(_, value)| value.clone())
            .unwrap_or_else(|| current.get(key).cloned().unwrap_or(JsValue::Undefined));
        out.insert(key.into(), value);
    }
    out
}

/// `input.limit ?? 50` for the run-history read: a nullish limit keeps the
/// store default, anything else goes through `Number()` (so a non-number
/// limit coerces exactly like the `slice` arithmetic it feeds — `NaN` is 0
/// and keeps every record, per the store's own clamp).
fn history_limit(input: &JsValue) -> Result<Option<i64>, CronError> {
    Ok(match property(input, "limit")? {
        JsValue::Undefined | JsValue::Null => None,
        other => Some(js_value::to_number(other).map_err(|_| {
            CronError::range_error(JsString::from("Maximum call stack size exceeded"))
        })? as i64),
    })
}

/// `new Date(runAt)` for the once arm (CronSchedule.ts:25-27): a string goes
/// through the Node v22.22.0 date parser, and the other primitives follow the
/// `Date` constructor's value semantics (verified on Node v22.22.0: `null`
/// is epoch 0, `true` is 1 ms, an array parses as its joined string, an
/// object's `"[object Object]"` does not parse). A NaN date maps to
/// `undefined` here, matching the `Number.isNaN(getTime())` fold.
fn compute_once(run_at: &JsValue) -> Result<ComputedRunAt, CronError> {
    match run_at {
        JsValue::String(text) => Ok(once_computed_run_at(text, &resolve_local)),
        JsValue::Number(number) => Ok(js_new_date(*number)),
        JsValue::Null => Ok(ComputedRunAt::Valid(0.0)),
        JsValue::Bool(bit) => Ok(ComputedRunAt::Valid(f64::from(*bit))),
        // Object/array: `new Date(x)` stringifies through ToPrimitive.
        other => Ok(once_computed_run_at(&to_display(other)?, &resolve_local)),
    }
}

/// `computeNextRunAt(schedule, now, timezone)` followed by the two throws
/// and the `.toISOString()` reads of createTask/updateTask — the body of the
/// method above, shared with the fire's post-run update and the scheduler's
/// recalculation (#2295): `Ok(None)` is the falsy `undefined`; the pair is
/// the Date's `getTime()` plus ISO text. The once arm maps every NaN Date to
/// `undefined` exactly like `Number.isNaN(runAt.getTime()) ? undefined :
/// runAt` (CronSchedule.ts:27), so an Invalid Date here is unreachable —
/// the delay arm's saturation already handled its own.
pub(super) fn computed_next_run_at_with(
    schedule: &NormalizedSchedule,
    now_ms: f64,
    zone_cache: &Arc<CronZoneResolverCache>,
) -> Result<Option<(f64, String)>, CronError> {
    match schedule {
        NormalizedSchedule::Once { run_at } => Ok(match compute_once(run_at)? {
            ComputedRunAt::Valid(epoch_ms) => Some((epoch_ms, iso_from_epoch_ms(epoch_ms))),
            ComputedRunAt::Undefined | ComputedRunAt::Invalid => None,
        }),
        NormalizedSchedule::Cron {
            expression,
            timezone,
        } => {
            let JsValue::String(expression) = expression else {
                // parseCronExpression's `expression.trim()` on a
                // non-string expression (CronSchedule.ts:108).
                return Err(CronError::type_error(JsString::from(
                    "expression.trim is not a function",
                )));
            };
            let computed = compute_next_run_at(
                &CronCreateSchedule::Cron {
                    expression: expression.clone(),
                    timezone: Some(JsString::from(timezone.clone())),
                },
                now_ms,
                timezone,
                zone_cache,
            )
            .map_err(|_| CronError::range_error(JsString::from("Invalid time value")))?;
            Ok(match computed {
                ComputedRunAt::Valid(epoch_ms) => Some((epoch_ms, iso_from_epoch_ms(epoch_ms))),
                ComputedRunAt::Undefined | ComputedRunAt::Invalid => None,
            })
        }
    }
}

/// The schedule shape `recoverInterruptedRuns` computes over: a stored task
/// schedule plus the resolved timezone, dispatching like `computeNextRunAt`'s
/// fall-through (an unknown `type` lands on the cron arm whose
/// `expression.trim()` throws). The delay fold uses the recovery's `now`.
pub(super) fn task_schedule_shape(
    schedule: &JsValue,
    timezone: &str,
    now_ms: f64,
) -> Result<NormalizedSchedule, CronError> {
    match schedule.get("type").and_then(JsValue::as_string) {
        Some(kind) if kind.equals_str("once") => Ok(NormalizedSchedule::Once {
            run_at: schedule.get("runAt").cloned().unwrap_or(JsValue::Undefined),
        }),
        Some(kind) if kind.equals_str("delay") => {
            let (amount, unit) = delay_schedule_fields(schedule)?;
            if !amount.is_finite() || amount <= 0.0 {
                // Recovery writes `computeNextRunAt(...)?.toISOString()`
                // (CronRuntime.ts:552): the `undefined` fold drops the key
                // instead of erroring — normalizeSchedule never runs here.
                return Ok(NormalizedSchedule::Once {
                    run_at: JsValue::Undefined,
                });
            }
            let run_at = delay_run_at_iso(now_ms, amount, unit)?;
            Ok(NormalizedSchedule::Once {
                run_at: value_string(run_at),
            })
        }
        _ => Ok(NormalizedSchedule::Cron {
            expression: schedule
                .get("expression")
                .cloned()
                .unwrap_or(JsValue::Undefined),
            timezone: timezone.to_owned(),
        }),
    }
}

/// A `resolveCronTimezone` argument: `None` for anything that is not a
/// representable string (never a valid zone, so TS's `Set#has` skips it too).
pub(super) fn timezone_str(value: Option<&JsValue>) -> Option<String> {
    value
        .and_then(JsValue::as_string)
        // The zone lookup is a UTF-8 `Set#has`; a lone surrogate can never
        // name a real zone, so the lossy fold and TS agree on the miss.
        .map(js_lossy)
}

fn key_meta(name: &str, value: &JsString) -> JsObject {
    let mut meta = JsObject::new();
    meta.insert(name.into(), JsValue::String(value.clone()));
    meta
}

fn count_meta(name: &str, count: usize) -> JsObject {
    let mut meta = JsObject::new();
    meta.insert(name.into(), JsValue::Number(count as f64));
    meta
}

// ---------------------------------------------------------------- manager

/// TS `CronManager` (CronManager.ts:48-267).
pub struct CronManager {
    options: CronManagerOptions,
    pilot_home: String,
    /// The #2181 per-zone resolver cache every runtime of this manager
    /// shares (one TZif parse per zone per process).
    zone_cache: Arc<CronZoneResolverCache>,
    gateway: Mutex<Option<Arc<dyn CronAgentGateway>>>,
    state: Mutex<ManagerState>,
    /// #2494: the one shared stop gate. TS has a single stopper (the
    /// teardown); this embedding adds the switch machine's S1 face, and
    /// both paths land here — the face through `CronStopHandle::stop_once`
    /// (its own once-flag only governs that handle), the teardown through
    /// the automation bundle's provider stop. Whichever runs first owns
    /// the stop; the later one is a no-op `Ok`, including after a *failed*
    /// first attempt (the "flag set even by failed stop" intent the face
    /// pins, lifted onto the shared object so the teardown cannot retry a
    /// stop the face already attempted). A later `start` re-arms it, the
    /// same lifecycle the `started` flag keeps.
    stop_attempted: AtomicBool,
    /// The weak self the runtimes' `sharedActiveRunCount` closures sum
    /// through (CronManager.ts:188's `activeRunCount: () =>
    /// this.activeRunCount()`): the constructor hands it out via
    /// `Arc::new_cyclic` because the manager only exists behind an `Arc`.
    self_weak: Weak<CronManager>,
}

/// Optional host scope; absent keeps the legacy multi-project manager policy.
pub type CronExecutionProjectFilter = Arc<dyn Fn(&str) -> bool + Send + Sync>;

struct ManagerState {
    started: bool,
    execution_project_filter: Option<CronExecutionProjectFilter>,
    execution_callbacks: Option<CronExecutionCallbacks>,
    /// The TS `Map<string, CronRuntime>` in insertion order; the resolved
    /// project key is `runtime.project_key()`.
    runtimes: Vec<Arc<CronRuntime>>,
}

impl CronManager {
    pub fn new(options: CronManagerOptions) -> Result<Arc<Self>, CronError> {
        let pilot_home = resolve_pilot_home(&options.pilot_home)?;
        Ok(Arc::new_cyclic(|self_weak| Self {
            options,
            pilot_home,
            zone_cache: Arc::new(CronZoneResolverCache::new()),
            gateway: Mutex::new(None),
            state: Mutex::new(ManagerState {
                started: false,
                execution_project_filter: None,
                execution_callbacks: None,
                runtimes: Vec::new(),
            }),
            stop_attempted: AtomicBool::new(false),
            self_weak: self_weak.clone(),
        }))
    }

    /// Limit automatic execution to projects served by this native host.
    /// Configure once before creating runtimes or starting the manager. This
    /// does not change the unfiltered library default or legacy store migration.
    pub fn set_execution_project_filter(
        &self,
        filter: CronExecutionProjectFilter,
    ) -> Result<(), CronError> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.started || !state.runtimes.is_empty() || state.execution_project_filter.is_some() {
            return Err(CronError::error(
                "Cron execution scope must be configured before loading runtimes.",
            ));
        }
        state.execution_project_filter = Some(filter);
        Ok(())
    }

    /// Bind optional callbacks/resource policy before any runtime exists.
    pub fn set_execution_callbacks(
        &self,
        callbacks: CronExecutionCallbacks,
    ) -> Result<(), CronError> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.started || !state.runtimes.is_empty() || state.execution_callbacks.is_some() {
            return Err(CronError::error(
                "Cron execution callbacks must be configured before loading runtimes.",
            ));
        }
        state.execution_callbacks = Some(callbacks);
        Ok(())
    }

    fn execution_callbacks(&self) -> CronExecutionCallbacks {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .execution_callbacks
            .clone()
            .unwrap_or_default()
    }

    fn execution_allowed(&self, project: &str) -> bool {
        let filter = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .execution_project_filter
            .clone();
        filter.is_none_or(|filter| filter(project))
    }

    /// `bindAgentGateway` (CronManager.ts:70-75).
    pub fn bind_agent_gateway(&self, gateway: Arc<dyn CronAgentGateway>) -> Result<(), CronError> {
        let mut bound = self.gateway.lock().unwrap_or_else(|e| e.into_inner());
        if bound.is_some() {
            return Err(CronError::error(JsString::from(
                "CronManager.bindAgentGateway already called.",
            )));
        }
        *bound = Some(gateway);
        Ok(())
    }

    /// `start` (CronManager.ts:82-103): disabled is a logged no-op, the bind
    /// gate throws, migration runs before `started` flips, then every
    /// discovered project key gets a started runtime.
    pub fn start(&self) -> Result<(), CronError> {
        if !self.options.config.enabled {
            self.logger().info(
                "cron disabled in config; manager is a no-op.",
                &JsObject::new(),
            );
            return Ok(());
        }
        if self
            .gateway
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_none()
        {
            return Err(CronError::error(JsString::from(
                "CronManager.start called before bindAgentGateway.",
            )));
        }
        let migration_logger = self.options.logger.clone().map(MigrationLogger);
        let migration = MigrationInput {
            pilot_home: PathBuf::from(&self.pilot_home),
            logger: migration_logger
                .as_ref()
                .map(|logger| logger as &dyn super::CronRuntimeLogger),
            now_ms: super::store::now_millis(),
            lock_retry_delay: Duration::from_millis(100),
        };
        let scope = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .execution_project_filter
            .clone();
        match scope {
            Some(scope) => {
                super::migration::migrate_cron_stores_scoped(&migration, scope.as_ref())?
            }
            None => migrate_cron_stores(&migration)?,
        }
        self.state.lock().unwrap_or_else(|e| e.into_inner()).started = true;
        // #2494: a fresh lifecycle re-arms the shared stop latch — the one
        // consumer that can legitimately stop again after a stop is a
        // manager that started again (no production path does; the reload
        // ladder stages fresh managers), and `started` flips in the same
        // place.
        self.stop_attempted.store(false, Ordering::SeqCst);
        let project_keys = discover_project_keys(&self.pilot_home, self.options.logger.as_ref())?;
        for project_key in project_keys {
            if self.execution_allowed(&project_key) {
                self.ensure_runtime(&JsValue::String(JsString::from(project_key)))?;
            }
        }
        self.logger().info(
            "cron manager started",
            &count_meta(
                "projectCount",
                self.state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .runtimes
                    .len(),
            ),
        );
        Ok(())
    }

    /// `stop` (CronManager.ts:105-113). A failing `runtime.stop()` propagates
    /// and leaves the map un-cleared, as the TS `await` chain does.
    /// #2494: gated by the one shared stop latch — see the field docs. The
    /// gate is taken *before* the work, so a failed first stop is never
    /// retried by the second path either; only a later `start` re-arms it.
    pub fn stop(&self) -> Result<(), CronError> {
        if self.stop_attempted.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let runtimes = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.started = false;
            state.runtimes.clone()
        };
        for runtime in &runtimes {
            runtime.stop()?;
        }
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .runtimes
            .clear();
        Ok(())
    }

    /// Native opt-in finalization; legacy stop retains its original TS contract.
    pub fn stop_and_drain(
        &self,
        budget: Duration,
        stopping: impl FnOnce(),
    ) -> Result<(), CronError> {
        let deadline = std::time::Instant::now() + budget;
        let runtimes = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.started = false;
            state.runtimes.clone()
        };
        let mut failure = None;
        for runtime in &runtimes {
            if let Err(error) = runtime.stop()
                && failure.is_none()
            {
                failure = Some(error);
            }
        }
        // Mark every pre-registered run stopped before canceling a native body
        // whose first submission may not have reached SessionOwner yet.
        stopping();
        for runtime in &runtimes {
            if let Some(scheduler) = runtime.bound_scheduler() {
                scheduler.drain_fires_until(deadline)?;
            }
        }
        if let Some(error) = failure {
            return Err(error);
        }
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .runtimes
            .clear();
        Ok(())
    }

    /// `createTask` (CronManager.ts:115-119): requireProjectKey,
    /// ensureRuntime, then `{...input, projectKey}` — the spread replaces the
    /// projectKey value in place, keeping the caller's key order.
    pub fn create_task(&self, input: &JsValue) -> Result<JsValue, CronError> {
        let project_key = require_project_key(property(input, "projectKey")?)?;
        let runtime = self.ensure_runtime(&JsValue::String(JsString::from(project_key.clone())))?;
        let mut spread = match input {
            JsValue::Object(object) => object.clone(),
            // `{...primitive}` is `{}`; only objects pass requireProjectKey.
            _ => JsObject::new(),
        };
        spread.insert(
            "projectKey".into(),
            JsValue::String(JsString::from(project_key)),
        );
        runtime.create_task(&JsValue::Object(spread))
    }

    /// `listTasks` (CronManager.ts:121-142): the global merge sorts tasks by
    /// `(nextRunAt ?? "").localeCompare(...)` and, with history, orders runs
    /// by `startedAt` descending before the `slice(0, limit ?? 50)`.
    pub fn list_tasks(&self, input: &JsValue) -> Result<JsValue, CronError> {
        let effective = default_input(input);
        let project_key = property(&effective, "projectKey")?;
        if truthy(project_key) {
            let runtime = self.ensure_runtime(project_key)?;
            return runtime.list_tasks(&effective);
        }
        let include_history = truthy(property(&effective, "includeHistory")?);
        let runtimes = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .runtimes
            .clone();
        let mut tasks: Vec<JsValue> = Vec::new();
        let mut runs: Vec<JsValue> = Vec::new();
        for runtime in &runtimes {
            let result = runtime.list_tasks(&effective)?;
            let result = result.as_object().expect("runtime listTasks result");
            tasks.extend(
                result
                    .get("tasks")
                    .and_then(value_array)
                    .expect("tasks array")
                    .iter()
                    .cloned(),
            );
            if include_history {
                runs.extend(
                    result
                        .get("recentRuns")
                        .and_then(value_array)
                        .expect("recentRuns array when includeHistory")
                        .iter()
                        .cloned(),
                );
            }
        }
        let collator = collator().map_err(CronError::from)?;
        // `(left.nextRunAt ?? "").localeCompare(right.nextRunAt ?? "")` — the
        // same pre-validation the store's sortTasks uses: V8 only throws once
        // the comparator actually runs, so this is stricter on a single
        // non-string element.
        for task in &tasks {
            if let Some(value) = task.get("nextRunAt")
                && !matches!(
                    value,
                    JsValue::String(_) | JsValue::Undefined | JsValue::Null
                )
            {
                return Err(CronError::type_error(JsString::from(
                    "(left.nextRunAt ?? \"\").localeCompare is not a function",
                )));
            }
        }
        tasks.sort_by(|left, right| {
            node_compare_utf16(&collator, &next_run_key(left), &next_run_key(right))
        });
        let mut out = JsObject::new();
        out.insert("tasks".into(), JsValue::Array(tasks));
        if include_history {
            // `right.startedAt.localeCompare(left.startedAt)` — normalize_run
            // guarantees the string.
            runs.sort_by(|left, right| {
                let left = run_started_at(left);
                let right = run_started_at(right);
                node_compare_utf16(&collator, right, left)
            });
            // `slice(0, input.limit ?? 50)` (CronManager.ts:139, baseline
            // cc7c4e65 checked byte for byte): `??` only falls through on
            // nullish, so `undefined` and an explicit `null` take the same
            // default `history_limit` and the store use. A bare
            // `slice(0, limit)` would keep everything for `undefined` and
            // nothing for `null` instead.
            let end = match property(&effective, "limit")? {
                JsValue::Undefined | JsValue::Null => js_slice_end(runs.len(), 50.0),
                other => js_slice_end(runs.len(), js_value::to_number(other).unwrap_or(f64::NAN)),
            };
            runs.truncate(end);
            out.insert("recentRuns".into(), JsValue::Array(runs));
        }
        Ok(JsValue::Object(out))
    }

    /// `updateTask` (CronManager.ts:144-148).
    pub fn update_task(&self, input: &JsValue) -> Result<JsValue, CronError> {
        Ok(
            match self
                .resolve_task_runtime(property(input, "taskId")?, property(input, "projectKey")?)?
            {
                None => not_updated("not_found"),
                Some(runtime) => runtime.update_task(input)?,
            },
        )
    }

    /// `deleteTask` (CronManager.ts:150-154).
    pub fn delete_task(&self, input: &JsValue) -> Result<JsValue, CronError> {
        Ok(
            match self
                .resolve_task_runtime(property(input, "taskId")?, property(input, "projectKey")?)?
            {
                None => {
                    let mut out = JsObject::new();
                    out.insert("deleted".into(), JsValue::Bool(false));
                    JsValue::Object(out)
                }
                Some(runtime) => runtime.delete_task(input)?,
            },
        )
    }

    /// `stopTask` (CronManager.ts:156-162).
    pub fn stop_task(&self, input: &JsValue) -> Result<JsValue, CronError> {
        Ok(match self.resolve_stop_runtime(input)? {
            None => stopped_no_run(property(input, "taskId")?, property(input, "runId")?),
            Some(runtime) => runtime.stop_task(input)?,
        })
    }

    /// `runTaskNow` (CronManager.ts:164-168).
    pub fn run_task_now(&self, input: &JsValue) -> Result<JsValue, CronError> {
        Ok(
            match self
                .resolve_task_runtime(property(input, "taskId")?, property(input, "projectKey")?)?
            {
                None => {
                    let mut out = JsObject::new();
                    out.insert("started".into(), JsValue::Bool(false));
                    out.insert("reason".into(), JsValue::from("not_found"));
                    JsValue::Object(out)
                }
                Some(runtime) => runtime.run_task_now(input)?,
            },
        )
    }

    /// `runTickOnce` (CronManager.ts:170-178): "Public for tests" — one tick
    /// for one project (loaded like any control method would load it) or one
    /// for every loaded runtime.
    pub fn run_tick_once(&self, project_key: Option<&str>) -> Result<(), CronError> {
        if let Some(project_key) = project_key {
            return self
                .ensure_runtime(&JsValue::String(JsString::from(project_key)))?
                .run_tick_once();
        }
        let runtimes = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .runtimes
            .clone();
        for runtime in &runtimes {
            runtime.run_tick_once()?;
        }
        Ok(())
    }

    /// The loaded runtime for a resolved project key — the seam the
    /// `runTickOnce`-style entries use (`Public for tests`, CronManager.ts:170).
    pub fn loaded_runtime(&self, project_key: &str) -> Option<Arc<CronRuntime>> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .runtimes
            .iter()
            .find(|runtime| runtime.project_key() == project_key)
            .cloned()
    }

    /// The #2181 zone cache shared by every runtime of this manager — the
    /// observation surface (`parse_count`) for the zone-reuse tests (the
    /// `loaded_runtime` `Public for tests` precedent).
    pub fn zone_resolver_cache(&self) -> &Arc<CronZoneResolverCache> {
        &self.zone_cache
    }

    pub fn runtime_count(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .runtimes
            .len()
    }

    fn logger(&self) -> Arc<dyn CronLogger> {
        self.options
            .logger
            .clone()
            .unwrap_or_else(|| Arc::new(NoopCronLogger))
    }

    /// `ensureRuntime` (CronManager.ts:181-217): the map is keyed by the
    /// resolved project key; a new runtime is inserted before the `.cwd`
    /// record, and only a started manager starts it.
    fn ensure_runtime(&self, project_key_input: &JsValue) -> Result<Arc<CronRuntime>, CronError> {
        let project_key = resolve_project_key(project_key_input)?;
        {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(existing) = state.find_runtime(&project_key) {
                return Ok(existing);
            }
        }
        let runtime = Arc::new(CronRuntime::new(
            self.options.config.clone(),
            self.pilot_home.clone(),
            project_key.clone(),
            self.options
                .now
                .clone()
                .unwrap_or_else(|| Arc::new(super::store::now_millis)),
            self.options
                .uuid
                .clone()
                .unwrap_or_else(|| Arc::new(|| JsString::from(uuid::Uuid::new_v4().to_string()))),
            self.logger(),
            self.zone_cache.clone(),
            self.options
                .session_overrides
                .clone()
                .unwrap_or_else(|| Arc::new(Mutex::new(SessionConfigOverrides::new()))),
            self.self_weak.clone(),
            self.options
                .scheduler_idle_poll_ms
                .unwrap_or(super::scheduler::DEFAULT_IDLE_POLL_MS),
        )?);
        if let Some(gateway) = self
            .gateway
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        {
            runtime.bind_agent_gateway(gateway)?;
        }
        {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(existing) = state.find_runtime(&project_key) {
                return Ok(existing);
            }
            state.runtimes.push(runtime.clone());
        }
        // recordProjectKey runs after the map insert in TS; its failure
        // leaves the runtime loaded (the next call returns the existing one).
        record_project_key(runtime.paths(), &project_key)?;
        if self.state.lock().unwrap_or_else(|e| e.into_inner()).started {
            runtime.start()?;
        }
        Ok(runtime)
    }

    /// `resolveTaskRuntime` (CronManager.ts:227-247).
    fn resolve_task_runtime(
        &self,
        task_id: &JsValue,
        project_key: &JsValue,
    ) -> Result<Option<Arc<CronRuntime>>, CronError> {
        if truthy(project_key) {
            let runtime = self.ensure_runtime(project_key)?;
            let has_task = runtime_has_task(&runtime, task_id)?;
            return Ok(has_task.then_some(runtime));
        }
        let runtimes = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .runtimes
            .clone();
        let mut matches = Vec::new();
        for runtime in &runtimes {
            if runtime_has_task(runtime, task_id)? {
                matches.push(runtime.clone());
            }
        }
        if matches.len() > 1 {
            return Err(CronError::error(template_message(
                "Cron task id is ambiguous across projects: ",
                "",
                task_id,
            )?));
        }
        Ok(matches.into_iter().next())
    }

    /// `resolveStopRuntime` (CronManager.ts:249-266): a truthy projectKey
    /// returns the runtime WITHOUT a task-existence check; without one, a
    /// task matches by taskId or by a truthy lastRunId.
    fn resolve_stop_runtime(&self, input: &JsValue) -> Result<Option<Arc<CronRuntime>>, CronError> {
        let project_key = property(input, "projectKey")?;
        if truthy(project_key) {
            return Ok(Some(self.ensure_runtime(project_key)?));
        }
        let task_id = property(input, "taskId")?;
        let run_id = property(input, "runId")?;
        let runtimes = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .runtimes
            .clone();
        let mut matches = Vec::new();
        for runtime in &runtimes {
            let tasks = runtime_task_list(runtime)?;
            if tasks.iter().any(|task| {
                (truthy(task_id) && strict_equals(task.get("taskId"), Some(task_id)))
                    || (truthy(run_id) && strict_equals(task.get("lastRunId"), Some(run_id)))
            }) {
                matches.push(runtime.clone());
            }
        }
        if matches.len() > 1 {
            // `${input.runId ?? input.taskId}`.
            let identifier = match run_id {
                JsValue::Undefined | JsValue::Null => task_id,
                other => other,
            };
            return Err(CronError::error(template_message(
                "Cron run identifier is ambiguous across projects: ",
                "",
                identifier,
            )?));
        }
        Ok(matches.into_iter().next())
    }
}

impl ManagerState {
    fn find_runtime(&self, project_key: &str) -> Option<Arc<CronRuntime>> {
        self.runtimes
            .iter()
            .find(|runtime| runtime.project_key() == project_key)
            .cloned()
    }
}

fn runtime_has_task(runtime: &CronRuntime, task_id: &JsValue) -> Result<bool, CronError> {
    Ok(runtime_task_list(runtime)?
        .iter()
        .any(|task| strict_equals(task.get("taskId"), Some(task_id))))
}

fn runtime_task_list(runtime: &CronRuntime) -> Result<Vec<JsValue>, CronError> {
    let tasks = runtime.list_tasks(&JsValue::Undefined)?;
    Ok(tasks
        .get("tasks")
        .and_then(value_array)
        .expect("tasks array")
        .to_vec())
}

/// `requireProjectKey` (CronManager.ts:273-278): the trim check runs before
/// the resolve, on the untrimmed value.
fn require_project_key(project_key: &JsValue) -> Result<String, CronError> {
    match project_key {
        JsValue::Undefined | JsValue::Null => Err(CronError::error(JsString::from(
            "Cron task creation requires a projectKey.",
        ))),
        JsValue::String(text) if metadata::js_trim(&js_lossy(text)).is_empty() => Err(
            CronError::error(JsString::from("Cron task creation requires a projectKey.")),
        ),
        JsValue::String(text) => resolve_key_string(text),
        // `projectKey?.trim()` — any other value throws V8's
        // "projectKey?.trim is not a function" (probed on Node v22.22.0).
        _ => Err(CronError::type_error(JsString::from(
            "projectKey?.trim is not a function",
        ))),
    }
}

fn next_run_key(task: &JsValue) -> JsString {
    match task.get("nextRunAt") {
        Some(JsValue::String(text)) => text.clone(),
        _ => JsString::from(""),
    }
}

fn run_started_at(run: &JsValue) -> &JsString {
    run.get("startedAt")
        .and_then(JsValue::as_string)
        .expect("normalize_run keeps startedAt")
}

/// `runs.slice(0, end)` end semantics: `NaN` becomes 0 (empty), negatives
/// count from the end, beyond-length clamps.
fn js_slice_end(len: usize, end: f64) -> usize {
    if end.is_nan() {
        return 0;
    }
    let end = end.trunc();
    if end < 0.0 {
        (len as f64 + end).max(0.0) as usize
    } else {
        end.min(len as f64) as usize
    }
}
