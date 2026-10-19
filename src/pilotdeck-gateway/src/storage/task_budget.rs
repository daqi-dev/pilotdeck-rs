//! SDK task-budget JSONL persistence (ProjectRuntimeRegistry.ts:1375-1540).
//! A store belongs to one runtime registry. Like TS, it caches its first load,
//! mutates memory before writing, and has no cross-process writer lock. Runtime
//! session configuration and validation belong to A6.3-pr8.
use crate::{
    js_value,
    metadata::{object, resolve_units},
};
use pilotdeck_protocol::{JsString, JsValue, parse_json, stringify_json};
use std::{
    fmt, fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

const DEFAULT_COMPACT_AFTER: usize = 512;
static UNDEFINED: JsValue = JsValue::Undefined;
fn field<'a>(v: &'a JsValue, name: &str) -> &'a JsValue {
    v.get(name).unwrap_or(&UNDEFINED)
}
fn absent(v: &JsValue) -> bool {
    matches!(v, JsValue::Undefined)
}
fn nullish(v: &JsValue) -> bool {
    matches!(v, JsValue::Undefined | JsValue::Null)
}
fn text(s: &str) -> JsValue {
    JsValue::String(s.into())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskBudgetScope {
    Session,
    Project,
}
impl TaskBudgetScope {
    fn as_str(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Project => "project",
        }
    }
    fn from_json(value: &JsValue) -> Self {
        if value.as_string().is_some_and(|s| s.equals_str("project")) {
            Self::Project
        } else {
            Self::Session
        }
    }
}

/// Optional metadata deliberately remains JSON: the TS loader validates the
/// required fields only, so null and other legacy values must survive reload.
#[derive(Clone, Debug, PartialEq)]
pub struct TaskBudgetState {
    pub spent_usd: f64,
    pub settled_run_ids: Vec<JsString>,
    pub total_usd: JsValue,
    pub project_retention_ms: JsValue,
    pub last_activity_at_ms: JsValue,
}
impl Default for TaskBudgetState {
    fn default() -> Self {
        Self {
            spent_usd: 0.0,
            settled_run_ids: vec![],
            total_usd: JsValue::Undefined,
            project_retention_ms: JsValue::Undefined,
            last_activity_at_ms: JsValue::Undefined,
        }
    }
}

#[derive(Debug)]
pub enum TaskBudgetError {
    Io(io::Error),
    TotalConflict(String),
    RetentionConflict,
    Invalid(String),
}
impl TaskBudgetError {
    pub fn code(&self) -> Option<&'static str> {
        match self {
            Self::TotalConflict(_) => Some("SDK_PROJECT_TASK_BUDGET_TOTAL_CONFLICT"),
            Self::RetentionConflict => Some("SDK_PROJECT_TASK_BUDGET_RETENTION_CONFLICT"),
            _ => None,
        }
    }
}
impl fmt::Display for TaskBudgetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => e.fmt(f),
            Self::TotalConflict(s) | Self::Invalid(s) => f.write_str(s),
            Self::RetentionConflict => f.write_str("Project taskBudget.projectRetentionMs is fixed by the first project budget configuration."),
        }
    }
}
impl std::error::Error for TaskBudgetError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}
impl From<io::Error> for TaskBudgetError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}
type Result<T> = std::result::Result<T, TaskBudgetError>;

pub struct TaskBudgetStore {
    path: PathBuf,
    // Vec preserves JS Map insertion order, including delete + reinsert.
    ledgers: Vec<(JsString, TaskBudgetState)>,
    loaded: bool,
    load_error: Option<TaskBudgetError>,
    record_count: usize,
    compact_after: usize,
}
impl TaskBudgetStore {
    /// `None` uses TS's default threshold (512). Zero also selects the default.
    /// The caller owns environment parsing and injects the clock at each write.
    pub fn new(pilot_home: impl AsRef<Path>, compact_after: Option<usize>) -> Self {
        Self {
            path: pilot_home
                .as_ref()
                .join("gateway/sdk-task-budget-ledger.jsonl"),
            ledgers: vec![],
            loaded: false,
            load_error: None,
            record_count: 0,
            compact_after: compact_after
                .filter(|n| *n > 0)
                .unwrap_or(DEFAULT_COMPACT_AFTER),
        }
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    /// Resolve a project key lexically like TS `registry.resolve` / `path.resolve`,
    /// without filesystem canonicalization. An empty key resolves to the cwd.
    /// A6.3-pr8 callers must resolve roots before calling the three write methods.
    pub fn resolve_project_root(project_root: &JsString) -> Result<JsString> {
        resolve_project_root(project_root)
    }
    /// Count of accepted records, including replayed duplicate settlements.
    pub fn record_count(&self) -> usize {
        self.record_count
    }
    pub fn load(&mut self) -> Result<()> {
        if let Some(error) = &self.load_error {
            return Err(copy_load_error(error));
        }
        if self.loaded {
            return Ok(());
        }
        let result = self.load_records();
        self.loaded = true;
        if let Err(error) = &result {
            self.load_error = Some(copy_load_error(error));
        }
        result
    }
    fn load_records(&mut self) -> Result<()> {
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == io::ErrorKind::NotFound => vec![],
            Err(e) => return Err(e.into()),
        };
        // Node readFile("utf8") replaces malformed UTF-8. A complete last line
        // without LF is valid; malformed/unknown lines stay on disk until compact.
        for line in String::from_utf8_lossy(&bytes).split('\n') {
            let Ok(record) = parse_json(line) else {
                continue;
            };
            if !valid_record(&record) {
                continue;
            }
            self.record_count += 1;
            let scope = TaskBudgetScope::from_json(field(&record, "scope"));
            let key = ledger_key(
                field(&record, "projectRoot").as_string().unwrap(),
                scope,
                field(&record, "sessionKey").as_string().unwrap(),
            )?;
            let kind = field(&record, "kind").as_string().unwrap();
            if kind.equals_str("cleared") {
                self.remove(&key);
                continue;
            }
            let ledger = self.entry(key);
            if kind.equals_str("snapshot") {
                ledger.spent_usd = field(&record, "spentUsd").as_number().unwrap();
                ledger.settled_run_ids.clear();
                if let JsValue::Array(ids) = field(&record, "settledRunIds") {
                    for id in ids {
                        let id = id.as_string().unwrap();
                        if !ledger.settled_run_ids.contains(id) {
                            ledger.settled_run_ids.push(id.clone());
                        }
                    }
                }
                ledger.total_usd = field(&record, "totalUsd").clone();
                ledger.project_retention_ms = field(&record, "projectRetentionMs").clone();
                ledger.last_activity_at_ms = field(&record, "lastActivityAtMs").clone();
            } else if kind.equals_str("configured") {
                for (target, name) in [
                    (&mut ledger.total_usd, "totalUsd"),
                    (&mut ledger.project_retention_ms, "projectRetentionMs"),
                    (&mut ledger.last_activity_at_ms, "lastActivityAtMs"),
                ] {
                    if nullish(target) {
                        *target = field(&record, name).clone();
                    }
                }
            } else {
                let run_id = field(&record, "runId").as_string().unwrap();
                if ledger.settled_run_ids.contains(run_id) {
                    continue;
                }
                ledger.settled_run_ids.push(run_id.clone());
                ledger.spent_usd += field(&record, "turnSpentUsd").as_number().unwrap();
                let at = field(&record, "lastActivityAtMs");
                if !absent(at) {
                    let old = if nullish(&ledger.last_activity_at_ms) {
                        0.0
                    } else {
                        number(&ledger.last_activity_at_ms)?
                    };
                    let new = number(at)?;
                    ledger.last_activity_at_ms = JsValue::Number(if old.is_nan() || new.is_nan() {
                        f64::NAN
                    } else {
                        old.max(new)
                    });
                }
            }
        }
        Ok(())
    }
    pub fn get(
        &mut self,
        project_root: &JsString,
        scope: TaskBudgetScope,
        session_key: &JsString,
    ) -> Result<Option<&TaskBudgetState>> {
        self.load()?;
        let key = ledger_key(project_root, scope, session_key)?;
        Ok(self.ledgers.iter().find(|(k, _)| *k == key).map(|(_, v)| v))
    }
    /// `project_root` must already be resolved, as by TS `registry.resolve`.
    /// Use [`Self::resolve_project_root`] at the runtime boundary (A6.3-pr8);
    /// this method preserves the supplied root in the appended record.
    pub fn configure_project(
        &mut self,
        project_root: &JsString,
        session_key: &JsString,
        total_usd: f64,
        retention_ms: Option<f64>,
        now_ms: f64,
    ) -> Result<()> {
        self.load()?;
        let key = ledger_key(project_root, TaskBudgetScope::Project, session_key)?;
        if let Some((_, ledger)) = self.ledgers.iter().find(|(k, _)| *k == key)
            && !absent(&ledger.project_retention_ms)
            && !absent(&ledger.last_activity_at_ms)
        {
            let elapsed = now_ms - number(&ledger.last_activity_at_ms)?;
            // NaN < retention is false in JS, so NaN also expires.
            if elapsed.partial_cmp(&number(&ledger.project_retention_ms)?)
                != Some(std::cmp::Ordering::Less)
            {
                self.remove(&key);
                let mut fields = vec![("version", JsValue::Number(1.0)), ("kind", text("cleared"))];
                fields.extend(identity(&key));
                self.append(object(fields))?;
            }
        }
        let ledger = self.entry(key);
        let retention = retention_ms.map_or(JsValue::Undefined, JsValue::Number);
        if !absent(&ledger.total_usd) {
            if ledger.total_usd.as_number() != Some(total_usd) {
                let fixed = match &ledger.total_usd {
                    JsValue::Number(n) => pilotdeck_protocol::number::to_fixed_6(*n),
                    JsValue::Null => {
                        return Err(TaskBudgetError::Invalid(
                            "Cannot read properties of null (reading 'toFixed')".into(),
                        ));
                    }
                    _ => {
                        return Err(TaskBudgetError::Invalid(
                            "ledger.totalUsd.toFixed is not a function".into(),
                        ));
                    }
                };
                return Err(TaskBudgetError::TotalConflict(format!(
                    "Project taskBudget.total is fixed at ${fixed}; received ${}.",
                    pilotdeck_protocol::number::to_fixed_6(total_usd)
                )));
            }
            if ledger.project_retention_ms != retention {
                return Err(TaskBudgetError::RetentionConflict);
            }
            return Ok(());
        }
        ledger.total_usd = JsValue::Number(total_usd);
        ledger.project_retention_ms = retention.clone();
        if retention_ms.is_some() {
            ledger.last_activity_at_ms = JsValue::Number(now_ms);
        }
        self.append(object(vec![
            ("version", JsValue::Number(1.0)),
            ("kind", text("configured")),
            ("projectRoot", JsValue::String(project_root.clone())),
            ("sessionKey", JsValue::String(session_key.clone())),
            ("scope", text("project")),
            ("totalUsd", JsValue::Number(total_usd)),
            ("projectRetentionMs", retention),
            (
                "lastActivityAtMs",
                retention_ms.map_or(JsValue::Undefined, |_| JsValue::Number(now_ms)),
            ),
        ]))
    }
    /// Caller supplies the configured scope; no budget means the runtime skips
    /// this call. Invalid spend and duplicate run IDs do not append or touch time.
    /// `project_root` must already be resolved, as by TS `registry.resolve`.
    /// Use [`Self::resolve_project_root`] at the runtime boundary (A6.3-pr8);
    /// this method preserves the supplied root in the appended record.
    pub fn settle(
        &mut self,
        project_root: &JsString,
        scope: TaskBudgetScope,
        session_key: &JsString,
        run_id: &JsString,
        turn_spent_usd: f64,
        now_ms: f64,
    ) -> Result<()> {
        if !turn_spent_usd.is_finite() || turn_spent_usd < 0.0 {
            return Ok(());
        }
        self.load()?;
        let ledger = self.entry(ledger_key(project_root, scope, session_key)?);
        if ledger.settled_run_ids.contains(run_id) {
            return Ok(());
        }
        ledger.settled_run_ids.push(run_id.clone());
        ledger.spent_usd += turn_spent_usd;
        if scope == TaskBudgetScope::Project && !absent(&ledger.project_retention_ms) {
            ledger.last_activity_at_ms = JsValue::Number(now_ms);
        }
        let at = ledger.last_activity_at_ms.clone();
        self.append(object(vec![
            ("version", JsValue::Number(1.0)),
            ("kind", text("settled")),
            ("projectRoot", JsValue::String(project_root.clone())),
            ("sessionKey", JsValue::String(session_key.clone())),
            (
                "scope",
                if scope == TaskBudgetScope::Project {
                    text("project")
                } else {
                    JsValue::Undefined
                },
            ),
            ("runId", JsValue::String(run_id.clone())),
            ("turnSpentUsd", JsValue::Number(turn_spent_usd)),
            ("lastActivityAtMs", at),
        ]))
    }
    /// Matches TS's synchronous clear: it deliberately does not force a load.
    /// Runtime callers wanting to retain existing ledgers must load first.
    /// `project_root` must already be resolved, as by TS `registry.resolve`.
    /// Use [`Self::resolve_project_root`] at the runtime boundary (A6.3-pr8);
    /// this method preserves the supplied root in the appended record.
    pub fn clear_session(&mut self, project_root: &JsString, session_key: &JsString) -> Result<()> {
        self.remove(&ledger_key(
            project_root,
            TaskBudgetScope::Session,
            session_key,
        )?);
        self.append(object(vec![
            ("version", JsValue::Number(1.0)),
            ("kind", text("cleared")),
            ("projectRoot", JsValue::String(project_root.clone())),
            ("sessionKey", JsValue::String(session_key.clone())),
        ]))
    }
    fn entry(&mut self, key: JsString) -> &mut TaskBudgetState {
        let index = self
            .ledgers
            .iter()
            .position(|(k, _)| *k == key)
            .unwrap_or_else(|| {
                self.ledgers.push((key, TaskBudgetState::default()));
                self.ledgers.len() - 1
            });
        &mut self.ledgers[index].1
    }
    fn remove(&mut self, key: &JsString) {
        self.ledgers.retain(|(k, _)| k != key);
    }
    fn append(&mut self, record: JsValue) -> Result<()> {
        fs::create_dir_all(self.path.parent().unwrap())?;
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        file.write_all(json_line(&record)?.as_bytes())?;
        self.record_count += 1;
        if self.record_count >= self.compact_after {
            let mut contents = String::new();
            for (key, ledger) in &self.ledgers {
                let mut fields = vec![
                    ("version", JsValue::Number(1.0)),
                    ("kind", text("snapshot")),
                ];
                fields.extend(identity(key));
                let mut ids = ledger.settled_run_ids.clone();
                ids.sort(); // JsString orders UTF-16 code units, like Array#sort.
                fields.extend([
                    ("spentUsd", JsValue::Number(ledger.spent_usd)),
                    (
                        "settledRunIds",
                        JsValue::Array(ids.into_iter().map(JsValue::String).collect()),
                    ),
                    ("totalUsd", ledger.total_usd.clone()),
                ]);
                if !absent(&ledger.project_retention_ms) {
                    fields.extend([
                        ("projectRetentionMs", ledger.project_retention_ms.clone()),
                        ("lastActivityAtMs", ledger.last_activity_at_ms.clone()),
                    ]);
                }
                contents.push_str(&json_line(&object(fields))?);
            }
            let mut temporary_name = self.path.file_name().unwrap().to_os_string();
            temporary_name.push(format!(".{}.tmp", uuid::Uuid::new_v4()));
            let temporary = self.path.with_file_name(temporary_name);
            // Same durability assumption as TS writeFileSync + renameSync:
            // neither the file nor its directory is fsynced. Load reads only the
            // ledger; load and later compactions leave crash-residual .tmp files
            // untouched. A successful rename consumes only this new temporary.
            fs::write(&temporary, contents)?;
            fs::rename(temporary, &self.path)?;
            self.record_count = self.ledgers.len();
        }
        Ok(())
    }
}
fn copy_load_error(error: &TaskBudgetError) -> TaskBudgetError {
    match error {
        TaskBudgetError::Io(e) => TaskBudgetError::Io(match e.raw_os_error() {
            Some(code) => io::Error::from_raw_os_error(code),
            None => io::Error::new(e.kind(), e.to_string()),
        }),
        TaskBudgetError::Invalid(message) => TaskBudgetError::Invalid(message.clone()),
        _ => unreachable!("load cannot report configuration conflicts"),
    }
}
fn number(v: &JsValue) -> Result<f64> {
    // JSON can shadow Object.prototype.toString with a non-callable property.
    // Neither valueOf nor that property then yields a primitive. Arrays join
    // recursively and can encounter the same failure in a nested object.
    let mut pending = vec![(v, 0)];
    while let Some((value, depth)) = pending.pop() {
        match value {
            JsValue::Object(o) if o.contains_key("toString") => {
                return Err(TaskBudgetError::Invalid(
                    "Cannot convert object to primitive value".into(),
                ));
            }
            JsValue::Array(items) => {
                if depth >= js_value::MAX_ARRAY_DEPTH {
                    return Err(TaskBudgetError::Invalid(
                        "Maximum call stack size exceeded".into(),
                    ));
                }
                pending.extend(items.iter().rev().map(|item| (item, depth + 1)));
            }
            _ => {}
        }
    }
    js_value::to_number(v)
        .map_err(|_| TaskBudgetError::Invalid("Maximum call stack size exceeded".into()))
}
fn json_line(v: &JsValue) -> Result<String> {
    stringify_json(v)
        .map(|s| s.unwrap() + "\n")
        .map_err(|e| TaskBudgetError::Invalid(e.to_string()))
}
fn valid_record(record: &JsValue) -> bool {
    if field(record, "version").as_number() != Some(1.0)
        || field(record, "projectRoot").as_string().is_none()
        || field(record, "sessionKey").as_string().is_none()
    {
        return false;
    }
    let scope = field(record, "scope");
    if !absent(scope)
        && !scope
            .as_string()
            .is_some_and(|s| s.equals_str("project") || s.equals_str("session"))
    {
        return false;
    }
    let Some(kind) = field(record, "kind").as_string() else {
        return false;
    };
    if kind.equals_str("cleared") {
        return true;
    }
    if kind.equals_str("configured") {
        return TaskBudgetScope::from_json(scope) == TaskBudgetScope::Project
            && field(record, "totalUsd").as_number().is_some();
    }
    if kind.equals_str("settled") {
        return field(record, "runId").as_string().is_some()
            && field(record, "turnSpentUsd").as_number().is_some();
    }
    kind.equals_str("snapshot")
        && field(record, "spentUsd").as_number().is_some()
        && matches!(field(record, "settledRunIds"), JsValue::Array(ids) if ids.iter().all(|v| v.as_string().is_some()))
}
// Node path.resolve is lexical, not realpath. Work in UTF-16 so lone surrogates
// and NULs in legacy identities survive; these keys never become filesystem paths.
fn resolve_project_root(root: &JsString) -> Result<JsString> {
    let base = if root.units().first() == Some(&47) {
        String::new()
    } else {
        std::env::current_dir()?.to_string_lossy().into_owned()
    };
    Ok(resolve_units(&base, root))
}
fn ledger_key(root: &JsString, scope: TaskBudgetScope, session: &JsString) -> Result<JsString> {
    let mut units = resolve_project_root(root)?.units().to_vec();
    units.push(0);
    units.extend(scope.as_str().encode_utf16());
    units.push(0);
    if scope == TaskBudgetScope::Project {
        units.push(42);
    } else {
        units.extend_from_slice(session.units());
    }
    Ok(JsString::from_utf16(units))
}
fn identity(key: &JsString) -> Vec<(&'static str, JsValue)> {
    let mut parts = key.units().split(|u| *u == 0);
    let root = JsString::from_utf16(parts.next().unwrap_or_default().to_vec());
    let scope = JsValue::String(JsString::from_utf16(
        parts.next().unwrap_or_default().to_vec(),
    ));
    let session = JsString::from_utf16(parts.next().unwrap_or_default().to_vec());
    vec![
        ("projectRoot", JsValue::String(root)),
        ("scope", text(TaskBudgetScope::from_json(&scope).as_str())),
        ("sessionKey", JsValue::String(session)),
    ]
}
