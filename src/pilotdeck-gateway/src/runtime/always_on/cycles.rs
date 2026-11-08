//! Port of TS `src/always-on/storage/WorkCycleStore.ts` and
//! `DiscoveryPlanStore.ts` — the halves the scheduler and the control surface
//! consume: index reads with the TS tolerance (ENOENT, unparseable,
//! non-schema-1 and non-array bodies all read as the default index), record
//! lookups, plan markdown reads, and the one pre-seam write
//! (`updateStatus({status: "ready"})` ahead of `rerunPlan`'s deep phases,
//! DiscoveryFire.ts:363). The fire-pipeline writes — cycle `create`/`addPlan`
//! and plan `upsert` — stay behind the fail-closed fire seam (`fire.rs`).
//!
//! Cycle records convert to a typed view only after a match: the TS find
//! compares the raw `id`, so a record without a string id can never match, and
//! the port keeps that by matching before converting. Plan records keep their
//! raw `JsObject` through the read-modify-write (`updateStatus` mutates the
//! parsed entry in place, so sibling keys survive with their order).
use std::{io, path::Path};

use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json};

use crate::runtime::cron::stringify_pretty;

use super::{AlwaysOnPaths, paths::plan_markdown_path};

/// `WorkCycleRecord["workspace"]` (`src/always-on/protocol/types.ts`): the
/// strategy, the absolute cwd and the free-form metadata map. `metadata` rides
/// as the raw object because only the seam-side apply prompt reads it
/// (branchName).
#[derive(Clone, Debug)]
pub(crate) struct WorkCycleWorkspace {
    /// Wire-shape fields the apply prompt reads; the read side lands with
    /// the fire seam (explicitly unported in this PR).
    #[allow(dead_code)]
    pub strategy: String,
    pub cwd: JsString,
    #[allow(dead_code)]
    pub metadata: JsObject,
}

/// `WorkCycleRecord` (protocol/types.ts) as the ported halves read it: the
/// scheduler checks `status`/`planIds`, `applyCycle` reads `planIds` and the
/// workspace presence. `appliedAt`/`archivedAt` are write-side only and stay
/// unported.
#[derive(Clone, Debug)]
pub(crate) struct WorkCycleRecord {
    pub raw: JsObject,
    pub id: JsString,
    /// Wire-shape fields the fire seam and its reports read; the read side
    /// lands with the explicitly unported fire half.
    #[allow(dead_code)]
    pub project_key: JsString,
    pub status: String,
    pub workspace: Option<WorkCycleWorkspace>,
    pub plan_ids: Vec<JsString>,
    /// Wire-shape fields the fire seam and its reports read; the read side
    /// lands with the explicitly unported fire half.
    #[allow(dead_code)]
    pub created_at: JsString,
    #[allow(dead_code)]
    pub created_by_run_id: JsString,
}

/// `DiscoveryPlanRecord` as the control surface reads it: `rerunPlan`'s early
/// checks need existence, `applyCycle`'s projection needs `id`/`title`. The
/// remaining fields (summary, rationale, dedupeKey, …) live behind the fire
/// seam.
#[derive(Clone, Debug)]
pub(crate) struct DiscoveryPlanRecord {
    pub raw: JsObject,
    pub id: JsString,
    pub title: Option<JsString>,
}

fn js_string(value: Option<&JsValue>) -> Option<JsString> {
    value.and_then(JsValue::as_string).cloned()
}
fn native_tag(value: Option<&JsValue>) -> String {
    value.and_then(JsValue::as_string).and_then(|value| value.to_utf8().ok()).unwrap_or_default()
}

/// `readIndex` tolerance (WorkCycleStore.ts:20-44 / DiscoveryPlanStore.ts:
/// 11-30): ENOENT and unparseable bodies read as the default; other io
/// errors propagate like the TS rethrow; a body that is not an object with
/// `schemaVersion === 1` and an array under `entries_key` reads as the
/// default too. The body decodes UTF-8 with replacement, like the Node
/// `utf8` read — an invalid byte becomes U+FFFD, which fails `JSON.parse`
/// and reads as the default, not an `InvalidData` error.
fn read_index_object(file: &Path, entries_key: &str) -> io::Result<JsObject> {
    let default = || {
        let mut index = JsObject::new();
        index.insert("schemaVersion".into(), JsValue::Number(1.0));
        index.insert(entries_key.into(), JsValue::Array(Vec::new()));
        index
    };
    let root = super::storage::root_for(file);
    let Some(raw) = super::storage::read(&root, file, super::storage::INDEX_BYTES)? else { return Ok(default()); };
    let raw = String::from_utf8_lossy(&raw);
    super::storage::check_json(&raw)?;
    let parsed = match parse_json(&raw) {
        Ok(parsed) => parsed,
        Err(_) => return Ok(default()),
    };
    let Some(object) = parsed.as_object() else {
        return Ok(default());
    };
    let valid = object.get("schemaVersion").and_then(JsValue::as_number) == Some(1.0)
        && matches!(object.get(entries_key), Some(JsValue::Array(_)));
    if valid {
        Ok(object.clone())
    } else {
        Ok(default())
    }
}

/// `writeIndex` (WorkCycleStore.ts:46-53): `mkdir -p` then the two-space
/// `JSON.stringify` bytes, no trailing newline.
fn write_index_object(file: &Path, index: &JsObject) -> io::Result<()> {
    super::storage::check_object(index)?;
    let text = stringify_pretty(&JsValue::Object(index.clone()));
    super::storage::check_json(&text)?;
    super::storage::write(&super::storage::root_for(file), file, text.as_bytes(), super::storage::INDEX_BYTES)
}

fn legacy_object(root: &Path, file: &Path) -> io::Result<Option<JsObject>> {
    let raw = match super::storage::read(root, file, super::storage::INDEX_BYTES) {
        Ok(Some(raw)) => raw,
        Ok(None) => return Ok(None),
        Err(error) if super::storage::is_policy_error(&error) => return Err(error),
        Err(_) => return Ok(None),
    };
    let text = String::from_utf8_lossy(&raw);
    super::storage::check_json(&text)?;
    Ok(parse_json(&text).ok().and_then(|value| value.as_object().cloned()))
}
fn relative_path(paths: &AlwaysOnPaths, value: &JsString) -> JsString {
    let root = JsString::from(paths.project_dir().to_string_lossy().as_ref());
    let relative = crate::runtime::tools::builtin::relative(&root, value);
    if relative.is_empty() || relative.units().starts_with(&[46, 46]) { value.clone() } else { relative }
}
fn without_field(object: &JsObject, field: &str) -> JsObject {
    let mut result = JsObject::new();
    for (key, value) in object.entries() { if !key.equals_str(field) { result.insert(key.clone(), value.clone()); } }
    result
}

/// The TS `find` over the entries: match on the raw entry before any typed
/// conversion, so a record without a string `id` (or `status`) never matches.
fn find_entry(
    index: &JsObject,
    entries_key: &str,
    predicate: impl Fn(&JsObject) -> bool,
) -> Option<JsObject> {
    let JsValue::Array(items) = index.get(entries_key)? else {
        return None;
    };
    items.iter().find_map(|item| {
        let entry = item.as_object()?;
        predicate(entry).then(|| entry.clone())
    })
}

fn cycle_record(entry: &JsObject) -> WorkCycleRecord {
    let workspace = entry
        .get("workspace")
        .and_then(JsValue::as_object)
        .map(|workspace| WorkCycleWorkspace {
            strategy: native_tag(workspace.get("strategy")),
            cwd: js_string(workspace.get("cwd")).unwrap_or_default(),
            metadata: match workspace.get("metadata") {
                Some(JsValue::Object(metadata)) => metadata.clone(),
                _ => JsObject::new(),
            },
        });
    // `planIds` entries that are not strings are dropped — unreachable for
    // schema-conformant records, whose ids are strings by construction.
    let plan_ids = match entry.get("planIds") {
        Some(JsValue::Array(items)) => items
            .iter()
            .filter_map(|item| item.as_string().cloned())
            .collect(),
        _ => Vec::new(),
    };
    WorkCycleRecord {
        raw: entry.clone(),
        id: entry.get("id").and_then(JsValue::as_string).cloned().unwrap_or_default(),
        project_key: js_string(entry.get("projectKey")).unwrap_or_default(),
        status: native_tag(entry.get("status")),
        workspace,
        plan_ids,
        created_at: js_string(entry.get("createdAt")).unwrap_or_default(),
        created_by_run_id: js_string(entry.get("createdByRunId")).unwrap_or_default(),
    }
}

fn plan_record(entry: &JsObject) -> DiscoveryPlanRecord {
    DiscoveryPlanRecord {
        raw: entry.clone(),
        id: entry.get("id").and_then(JsValue::as_string).cloned().unwrap_or_default(),
        title: entry.get("title").and_then(JsValue::as_string).cloned(),
    }
}

/// `WorkCycleStore` (WorkCycleStore.ts:17-18): one `cycles/index.json` per
/// project directory.
#[derive(Clone)]
pub(crate) struct WorkCycleStore {
    paths: AlwaysOnPaths,
}

impl WorkCycleStore {
    pub(crate) fn new(paths: AlwaysOnPaths) -> Self {
        Self { paths }
    }

    /// Failure cleanup may remove only a workspace with no durable cycle
    /// reference. Scan all entries, including duplicate ids, before deciding.
    pub(crate) fn references_workspace(&self, cwd: &JsString) -> io::Result<bool> {
        let root = super::storage::root_for(&self.paths.state_file);
        let Some(raw) = super::storage::read(&root, &self.paths.cycle_index_file, super::storage::INDEX_BYTES)? else { return Ok(false); };
        let raw = String::from_utf8_lossy(&raw); super::storage::check_json(&raw)?;
        let parsed = parse_json(&raw).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
        let index = parsed.as_object().filter(|index| index.get("schemaVersion") == Some(&JsValue::Number(1.0)) && matches!(index.get("cycles"), Some(JsValue::Array(_))))
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Cycle ownership index is not verifiable."))?;
        Ok(find_entry(index, "cycles", |entry| entry.get("workspace").and_then(JsValue::as_object).and_then(|workspace| workspace.get("cwd")).and_then(JsValue::as_string) == Some(cwd)).is_some())
    }

    fn read_index(&self) -> io::Result<JsObject> {
        read_index_object(&self.paths.cycle_index_file, "cycles")
    }

    /// `getRecord` (WorkCycleStore.ts:55-58): first entry whose `id` equals
    /// the query.
    pub(crate) fn get_record(&self, cycle_id: &str) -> io::Result<Option<WorkCycleRecord>> {
        self.get_record_js(&JsString::from(cycle_id))
    }

    pub(crate) fn get_record_js(&self, cycle_id: &JsString) -> io::Result<Option<WorkCycleRecord>> {
        let index = self.read_index()?;
        Ok(find_entry(&index, "cycles", |entry| {
            entry.get("id").and_then(JsValue::as_string) == Some(cycle_id)
        }).map(|entry| cycle_record(&entry)))
    }

    /// `getActiveCycle` (WorkCycleStore.ts:60-63): first entry whose `status`
    /// is `"active"`.
    pub(crate) fn get_active_cycle(&self) -> io::Result<Option<WorkCycleRecord>> {
        let index = self.read_index()?;
        Ok(find_entry(&index, "cycles", |entry| {
            entry.get("status").and_then(JsValue::as_string).is_some_and(|status| status.equals_str("active"))
        })
        .map(|entry| cycle_record(&entry)))
    }

    pub(crate) async fn read_apply(&self, cycle_id: JsString) -> io::Result<Option<(WorkCycleRecord, Vec<DiscoveryPlanRecord>)>> {
        let store = self.clone();
        super::storage::run(move || {
            let Some(cycle) = store.get_record_js(&cycle_id)? else { return Ok(None); };
            let plans = DiscoveryPlanStore::new(store.paths.clone()).list_records()?.into_iter()
                .filter(|plan| cycle.plan_ids.iter().any(|id| id == &plan.id)).collect();
            Ok(Some((cycle, plans)))
        }).await
    }

    pub(crate) fn write_index(&self, index: &JsObject) -> io::Result<()> {
        write_index_object(&self.paths.cycle_index_file, index)
    }

    pub(crate) fn create(&self, workspace: &JsObject, run_id: JsString, cycle_id: JsString, now_ms: f64) -> io::Result<WorkCycleRecord> {
        let mut index = self.read_index()?;
        let mut record = JsObject::new();
        record.insert("id".into(), JsValue::String(cycle_id));
        record.insert("projectKey".into(), self.paths.project_key.as_str().into());
        record.insert("status".into(), "active".into());
        record.insert("workspace".into(), JsValue::Object(workspace.clone()));
        record.insert("planIds".into(), JsValue::Array(Vec::new()));
        record.insert("createdAt".into(), JsValue::String(crate::runtime::cron::iso_from_epoch_ms(now_ms).as_str().into()));
        record.insert("createdByRunId".into(), JsValue::String(run_id));
        let Some(JsValue::Array(cycles)) = index.get("cycles") else { unreachable!() };
        let mut cycles = cycles.clone(); cycles.push(JsValue::Object(record.clone()));
        index.insert("cycles".into(), JsValue::Array(cycles));
        self.write_index(&index)?;
        Ok(cycle_record(&record))
    }

    pub(crate) fn migrate_from_legacy(&self, cycle_id: JsString, now_ms: f64) -> io::Result<Option<WorkCycleRecord>> {
        let mut index = self.read_index()?;
        let Some(JsValue::Array(existing)) = index.get("cycles") else { unreachable!() };
        if !existing.is_empty() { return Ok(None); }
        let root = super::storage::root_for(&self.paths.state_file);
        let Some(mut state) = legacy_object(&root, &self.paths.state_file)? else { return Ok(None); };
        let Some(workspace) = state.get("currentWorkspace").and_then(JsValue::as_object).cloned() else { return Ok(None); };
        let Some(cwd) = workspace.get("cwd").and_then(JsValue::as_string) else { return Ok(None); };
        let cwd_path = cwd.to_utf8().map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "Always-On legacy workspace path is not representable by the native filesystem."))?;
        if !Path::new(&cwd_path).exists() { return Ok(None); }
        let mut plans = legacy_object(&root, &self.paths.plan_index_file)?;
        let plan_ids = plans.as_ref().and_then(|index| index.get("plans")).and_then(|value| {
            if let JsValue::Array(plans) = value { Some(plans.iter().filter_map(|plan| {
                let plan = plan.as_object()?;
                let matches = plan.get("workspace").and_then(JsValue::as_object).and_then(|workspace| workspace.get("cwd")).and_then(JsValue::as_string) == Some(cwd);
                matches.then(|| plan.get("id").cloned().unwrap_or(JsValue::Undefined))
            }).collect::<Vec<_>>()) } else { None }
        }).unwrap_or_default();
        let mut record = JsObject::new();
        record.insert("id".into(), JsValue::String(cycle_id.clone()));
        record.insert("projectKey".into(), self.paths.project_key.as_str().into());
        record.insert("status".into(), "active".into());
        let mut selected = JsObject::new();
        for field in ["strategy", "cwd", "metadata"] {
            selected.insert(field.into(), workspace.get(field).cloned().unwrap_or_else(|| if field == "metadata" { JsValue::Object(JsObject::new()) } else { JsValue::Undefined }));
        }
        record.insert("workspace".into(), JsValue::Object(selected));
        record.insert("planIds".into(), JsValue::Array(plan_ids.clone()));
        record.insert("createdAt".into(), crate::runtime::cron::iso_from_epoch_ms(now_ms).as_str().into());
        record.insert("createdByRunId".into(), workspace.get("runId").cloned().unwrap_or(JsValue::Undefined));
        index.insert("cycles".into(), JsValue::Array(vec![JsValue::Object(record.clone())]));
        self.write_index(&index)?;
        state.insert("activeWorkCycleId".into(), JsValue::String(cycle_id.clone()));
        state = without_field(&state, "currentWorkspace");
        if let Err(error) = write_index_object(&self.paths.state_file, &state) {
            if super::storage::is_policy_error(&error) { return Err(error); }
        }
        if !plan_ids.is_empty() && let Some(plan_index) = plans.as_mut() {
            if let Some(JsValue::Array(entries)) = plan_index.get("plans") {
                let entries = entries.iter().map(|entry| {
                    let Some(plan) = entry.as_object() else { return entry.clone(); };
                    if !plan_ids.iter().any(|id| plan.get("id") == Some(id)) { return entry.clone(); }
                    let mut plan = without_field(plan, "workspace");
                    plan.insert("workCycleId".into(), JsValue::String(cycle_id.clone()));
                    JsValue::Object(plan)
                }).collect();
                plan_index.insert("plans".into(), JsValue::Array(entries));
                if let Err(error) = write_index_object(&self.paths.plan_index_file, plan_index) {
                    if super::storage::is_policy_error(&error) { return Err(error); }
                }
            }
        }
        Ok(Some(cycle_record(&record)))
    }

    pub(crate) fn update_status(&self, cycle_id: &JsString, status: &str, now_ms: f64) -> io::Result<Option<WorkCycleRecord>> {
        let mut index = self.read_index()?;
        let Some(JsValue::Array(entries)) = index.get("cycles") else { unreachable!() };
        let mut entries = entries.clone(); let mut result = None;
        for entry in &mut entries {
            let JsValue::Object(record) = entry else { continue; };
            if record.get("id").and_then(JsValue::as_string) != Some(cycle_id) { continue; }
            record.insert("status".into(), status.into());
            let field = match status { "applied" => Some("appliedAt"), "archived" => Some("archivedAt"), _ => None };
            if let Some(field) = field { record.insert(field.into(), crate::runtime::cron::iso_from_epoch_ms(now_ms).as_str().into()); }
            result = Some(cycle_record(record)); break;
        }
        if result.is_some() { index.insert("cycles".into(), JsValue::Array(entries)); self.write_index(&index)?; }
        Ok(result)
    }

    pub(crate) fn add_plan(&self, cycle_id: &JsString, plan_id: JsString) -> io::Result<()> {
        let mut index = self.read_index()?;
        let Some(JsValue::Array(entries)) = index.get("cycles") else { unreachable!() };
        let mut entries = entries.clone();
        for entry in &mut entries {
            let JsValue::Object(record) = entry else { continue; };
            if record.get("id").and_then(JsValue::as_string) != Some(cycle_id) { continue; }
            let mut ids = match record.get("planIds") { Some(JsValue::Array(ids)) => ids.clone(), _ => Vec::new() };
            if ids.iter().any(|id| id.as_string() == Some(&plan_id)) { return Ok(()); }
            ids.push(JsValue::String(plan_id)); record.insert("planIds".into(), JsValue::Array(ids));
            index.insert("cycles".into(), JsValue::Array(entries));
            return self.write_index(&index);
        }
        Ok(())
    }
}

/// `DiscoveryPlanStore` (DiscoveryPlanStore.ts:8-9): one `plans/index.json`
/// per project directory.
#[derive(Clone)]
pub(crate) struct DiscoveryPlanStore {
    paths: AlwaysOnPaths,
}

impl DiscoveryPlanStore {
    pub(crate) fn new(paths: AlwaysOnPaths) -> Self {
        Self { paths }
    }

    fn read_index(&self) -> io::Result<JsObject> {
        read_index_object(&self.paths.plan_index_file, "plans")
    }

    pub(crate) fn write_index(&self, index: &JsObject) -> io::Result<()> {
        write_index_object(&self.paths.plan_index_file, index)
    }

    pub(crate) fn upsert(&self, record: &JsObject) -> io::Result<DiscoveryPlanRecord> {
        let mut index = self.read_index()?;
        super::storage::check_object(record)?;
        let mut record = record.clone();
        for field in ["planFilePath", "reportFilePath"] {
            let original = record.get(field).cloned().unwrap_or(JsValue::Undefined);
            let transformed = match original.as_string() {
                Some(value) if field == "planFilePath" || !value.is_empty() => {
                    let root = JsString::from(self.paths.project_dir().to_string_lossy().as_ref());
                    let relative = crate::runtime::tools::builtin::relative(&root, value);
                    if relative.is_empty() || relative.units().starts_with(&[46, 46]) { original }
                    else { JsValue::String(relative) }
                }
                _ => JsValue::Undefined,
            };
            record.insert(field.into(), transformed);
        }
        let id = record.get("id").cloned().unwrap_or(JsValue::Undefined);
        let Some(JsValue::Array(entries)) = index.get("plans") else { unreachable!() };
        let mut entries = entries.clone();
        if let Some(existing) = entries.iter_mut().find(|entry| entry.as_object().is_some_and(|entry| entry.get("id") == Some(&id))) {
            *existing = JsValue::Object(record.clone());
        } else { entries.push(JsValue::Object(record.clone())); }
        index.insert("plans".into(), JsValue::Array(entries));
        self.write_index(&index)?;
        Ok(plan_record(&record))
    }

    pub(crate) fn write_plan_markdown(&self, plan_id: &JsString, markdown: &JsString) -> io::Result<std::path::PathBuf> {
        let path = plan_markdown_path(&self.paths, plan_id);
        // Node's UTF-8 filesystem writer replaces unpaired surrogates.
        let text = String::from_utf16_lossy(markdown.units());
        super::storage::write(&super::storage::root_for(&self.paths.state_file), &path, text.as_bytes(), super::storage::MARKDOWN_BYTES)?;
        Ok(path)
    }

    pub(crate) fn update(&self, plan_id: &JsString, update: &JsObject) -> io::Result<Option<DiscoveryPlanRecord>> {
        let mut index = self.read_index()?;
        let Some(JsValue::Array(entries)) = index.get("plans") else { unreachable!() };
        let mut entries = entries.clone(); let mut result = None;
        for entry in &mut entries {
            let JsValue::Object(record) = entry else { continue; };
            if record.get("id").and_then(JsValue::as_string) != Some(plan_id) { continue; }
            if let Some(status) = update.get("status").filter(|value| !value.is_undefined()) {
                record.insert("status".into(), status.clone());
                if record.contains_key("executionStatus") && status.as_string().is_some_and(|status| ["completed", "completed_no_report", "failed"].iter().any(|value| status.equals_str(value))) {
                    record.insert("executionStatus".into(), status.clone());
                }
            }
            if let Some(path) = update.get("reportFilePath").filter(|value| !value.is_undefined()) {
                let path = path.as_string().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "Always-On reportFilePath must be a string."))?;
                record.insert("reportFilePath".into(), JsValue::String(relative_path(&self.paths, path)));
            }
            if let Some(cycle) = update.get("workCycleId").filter(|value| !value.is_undefined()) {
                record.insert("workCycleId".into(), cycle.clone());
                *record = without_field(record, "workspace");
            }
            result = Some(plan_record(record)); break;
        }
        if result.is_some() { index.insert("plans".into(), JsValue::Array(entries)); self.write_index(&index)?; }
        Ok(result)
    }

    /// `getRecord` (DiscoveryPlanStore.ts:98-101).
    pub(crate) fn get_record(&self, plan_id: &str) -> io::Result<Option<DiscoveryPlanRecord>> {
        self.get_record_js(&JsString::from(plan_id))
    }

    pub(crate) fn get_record_js(&self, plan_id: &JsString) -> io::Result<Option<DiscoveryPlanRecord>> {
        let index = self.read_index()?;
        Ok(find_entry(&index, "plans", |entry| {
            entry.get("id").and_then(JsValue::as_string) == Some(plan_id)
        }).map(|entry| plan_record(&entry)))
    }

    /// The plans array as typed views — the `applyCycle` projection source
    /// (AlwaysOnRuntime.ts:348-351 filters this by the cycle's id list).
    pub(crate) fn list_records(&self) -> io::Result<Vec<DiscoveryPlanRecord>> {
        let index = self.read_index()?;
        let items = match index.get("plans") {
            Some(JsValue::Array(items)) => items,
            _ => unreachable!("#2294 read_index_object guarantees a plans array"),
        };
        Ok(items
            .iter()
            .filter_map(|item| item.as_object().map(plan_record))
            .collect())
    }

    /// `readPlanMarkdown` (DiscoveryPlanStore.ts:44-54): ENOENT reads as
    /// `None`; the body decodes UTF-8 with replacement, like the Node utf8
    /// read.
    pub(crate) fn read_plan_markdown(&self, plan_id: &str) -> io::Result<Option<String>> {
        self.read_plan_markdown_js(&JsString::from(plan_id))
    }

    pub(crate) fn read_plan_markdown_js(&self, plan_id: &JsString) -> io::Result<Option<String>> {
        let path = plan_markdown_path(&self.paths, plan_id);
        Ok(super::storage::read(&super::storage::root_for(&self.paths.state_file), &path, super::storage::MARKDOWN_BYTES)?
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned()))
    }

    /// `updateStatus(planId, { status })` (DiscoveryPlanStore.ts:69-96) — the
    /// one write the pre-seam rerun path makes. The entry mutates in place,
    /// so sibling keys and their order survive; a missing plan writes
    /// nothing. The `executionStatus` mirror only fires for terminal
    /// statuses, which the ported path (`"ready"`) never sets.
    pub(crate) fn update_status(
        &self,
        plan_id: &str,
        status: &str,
    ) -> io::Result<Option<DiscoveryPlanRecord>> {
        let mut index = self.read_index()?;
        let mut items = match index.get("plans") {
            Some(JsValue::Array(items)) => items.clone(),
            _ => Vec::new(),
        };
        let mut updated = None;
        for item in items.iter_mut() {
            let JsValue::Object(entry) = item else {
                continue;
            };
            if entry.get("id").and_then(JsValue::as_string) != Some(&JsString::from(plan_id)) {
                continue;
            }
            entry.insert("status".into(), JsValue::String(JsString::from(status)));
            if entry.get("executionStatus").is_some()
                && matches!(status, "completed" | "completed_no_report" | "failed")
            {
                entry.insert(
                    "executionStatus".into(),
                    JsValue::String(JsString::from(status)),
                );
            }
            updated = Some(plan_record(entry));
            break;
        }
        if updated.is_none() {
            return Ok(None);
        }
        index.insert("plans".into(), JsValue::Array(items));
        write_index_object(&self.paths.plan_index_file, &index)?;
        Ok(updated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::negative_assert_eq;
    use std::path::PathBuf;

    fn scratch(tag: &str) -> PathBuf {
        let home = std::env::temp_dir().join(format!("always-on-2294-cycles-{tag}"));
        let _ = std::fs::remove_dir_all(&home);
        home
    }

    fn paths(home: &Path) -> AlwaysOnPaths {
        AlwaysOnPaths {
            project_key: "/proj/a".into(),
            state_file: home.join("state.json"),
            plans_dir: home.join("plans"),
            plan_index_file: home.join("plans").join("index.json"),
            cycle_index_file: home.join("cycles").join("index.json"),
            reports_dir: home.join("reports"),
            discovery_lock_file: home.join("locks").join("discovery.lock"),
        }
    }

    fn write_raw(file: &Path, body: &str) {
        std::fs::create_dir_all(file.parent().expect("#2294 parent")).expect("#2294 mkdir");
        std::fs::write(file, body).expect("#2294 write");
    }

    fn write_value(file: &Path, value: &serde_json::Value) {
        write_raw(
            file,
            &serde_json::to_string_pretty(value).expect("#2294 pretty"),
        );
    }

    // The `readIndex` tolerance (WorkCycleStore.ts:20-44): a missing,
    // malformed, wrong-schema or non-array body all read as the default empty
    // index — and none of them write anything back.
    #[test]
    fn cycle_index_tolerates_missing_malformed_and_wrong_schema() {
        let bodies: [(&str, Option<String>); 5] = [
            ("missing", None),
            ("malformed", Some("{oops".into())),
            (
                "wrong-schema",
                Some("{\"schemaVersion\":2,\"cycles\":[]}".into()),
            ),
            (
                "non-array",
                Some("{\"schemaVersion\":1,\"cycles\":{}}".into()),
            ),
            ("not-object", Some("[1,2,3]".into())),
        ];
        for (tag, body) in bodies {
            let home = scratch(tag);
            let paths = paths(&home);
            if let Some(body) = body.as_deref() {
                write_raw(&paths.cycle_index_file, body);
            }
            let store = WorkCycleStore::new(paths.clone());
            negative_assert_eq!(
                store.get_record("c1").expect("#2294 read").is_none(),
                true,
                "#2294 {tag} cycle index reads as the default"
            );
            negative_assert_eq!(
                store.get_active_cycle().expect("#2294 read").is_none(),
                true,
                "#2294 {tag} active cycle reads as the default"
            );
            negative_assert_eq!(
                paths.cycle_index_file.exists(),
                body.is_some(),
                "#2294 {tag} read leaves the file untouched"
            );
        }
        // Invalid UTF-8 decodes with U+FFFD replacement like the Node
        // `utf8` read, so the body stays unparseable and reads as the
        // default instead of surfacing an io error.
        let home = scratch("non-utf8");
        let paths = paths(&home);
        let file = &paths.cycle_index_file;
        std::fs::create_dir_all(file.parent().expect("#2294 parent")).expect("#2294 mkdir");
        std::fs::write(file, [0xff, b'{', b'}']).expect("#2294 non-utf8 cycle index");
        let store = WorkCycleStore::new(paths.clone());
        negative_assert_eq!(
            store.get_record("c1").expect("#2294 read").is_none(),
            true,
            "#2294 non-utf8 cycle index reads as the default, not an error"
        );
    }

    // The probe's `t10` record shape: `getRecord` matches by id and
    // `getActiveCycle` by status, and the typed view carries the plan list
    // the cycle-full check counts.
    #[test]
    fn cycle_records_match_by_id_and_status() {
        let home = scratch("t10-shape");
        let paths = paths(&home);
        write_value(
            &paths.cycle_index_file,
            &serde_json::json!({
                "schemaVersion": 1,
                "cycles": [
                    {
                        "id": "c1",
                        "projectKey": "/proj/a",
                        "status": "active",
                        "workspace": { "strategy": "snapshot-copy", "cwd": "/w", "metadata": { "branchName": "b" } },
                        "planIds": ["p1", "p2", "p3"],
                        "createdAt": "2026-01-01T00:00:00.000Z",
                        "createdByRunId": "r0"
                    },
                    { "id": "c2", "projectKey": "/proj/a", "status": "archived", "planIds": [], "createdAt": "x", "createdByRunId": "r0" },
                    { "projectKey": "/proj/a", "status": "active", "planIds": [] }
                ]
            }),
        );
        let store = WorkCycleStore::new(paths);
        let record = store
            .get_record("c1")
            .expect("#2294 read")
            .expect("#2294 c1");
        negative_assert_eq!(record.plan_ids.len(), 3, "#2294 t10 planIds");
        negative_assert_eq!(record.status, "active", "#2294 status");
        negative_assert_eq!(
            record.workspace.as_ref().map(|ws| ws.cwd.to_utf8().unwrap()),
            Some("/w".to_owned()),
            "#2294 workspace cwd"
        );
        // A record without a string id never matches (the TS find compares
        // the raw value), and the id lookup is not the status lookup.
        negative_assert_eq!(
            store.get_record("").expect("#2294 read").is_none(),
            true,
            "#2294 no idless match"
        );
        negative_assert_eq!(
            store.get_active_cycle().expect("#2294 read").map(|c| c.id),
            Some("c1".into()),
            "#2294 first active cycle wins"
        );
    }

    // The probe's `r1` workspace-less cycle: the apply path reads
    // `cycle.workspace.strategy` on the raw record, which is the missing
    // workspace case (`fire.rs` carries the documented envelope).
    #[test]
    fn workspaceless_cycles_read_as_none() {
        let home = scratch("r1-shape");
        let paths = paths(&home);
        write_value(
            &paths.cycle_index_file,
            &serde_json::json!({
                "schemaVersion": 1,
                "cycles": [
                    { "id": "c1", "projectKey": "/proj/a", "status": "active", "planIds": [], "createdAt": "2026-01-01T00:00:00.000Z", "createdByRunId": "r0" }
                ]
            }),
        );
        let store = WorkCycleStore::new(paths);
        let record = store
            .get_record("c1")
            .expect("#2294 read")
            .expect("#2294 c1");
        negative_assert_eq!(record.workspace.is_none(), true, "#2294 no workspace key");
    }

    // The probe's `r1` plan record: `getRecord` matches by id with the full
    // field set riding along, and the missing plan reads as `None`.
    #[test]
    fn plan_records_match_by_id() {
        let home = scratch("r1-plans");
        let paths = paths(&home);
        write_value(
            &paths.plan_index_file,
            &serde_json::json!({
                "schemaVersion": 1,
                "plans": [
                    {
                        "id": "p1",
                        "title": "T",
                        "createdAt": "2026-01-01T00:00:00.000Z",
                        "status": "ready",
                        "summary": "s",
                        "rationale": "r",
                        "dedupeKey": "d",
                        "sourceRunId": "r0",
                        "planFilePath": "plans/p1.md"
                    }
                ]
            }),
        );
        let store = DiscoveryPlanStore::new(paths);
        let record = store
            .get_record("p1")
            .expect("#2294 read")
            .expect("#2294 p1");
        negative_assert_eq!(record.id, JsString::from("p1"), "#2294 id");
        negative_assert_eq!(record.title.as_ref().map(|title| title.to_utf8().unwrap()), Some("T".to_owned()), "#2294 title");
        negative_assert_eq!(
            store.get_record("p-missing").expect("#2294 read").is_none(),
            true,
            "#2294 missing plan"
        );
        let listed = store.list_records().expect("#2294 list");
        negative_assert_eq!(
            listed.iter().map(|r| r.id.to_utf8().unwrap()).collect::<Vec<_>>(),
            vec!["p1"],
            "#2294 list_records projects id and title"
        );
    }

    // `readPlanMarkdown` (DiscoveryPlanStore.ts:44-54) routes through the
    // sanitized plan path and reads ENOENT as `None`.
    #[test]
    fn read_plan_markdown_routes_through_the_sanitized_path() {
        let home = scratch("markdown");
        let paths = paths(&home);
        std::fs::create_dir_all(&paths.plans_dir).expect("#2294 mkdir");
        std::fs::write(paths.plans_dir.join("p-1.md"), "# body").expect("#2294 write");
        let store = DiscoveryPlanStore::new(paths);
        negative_assert_eq!(
            store
                .read_plan_markdown("p 1")
                .expect("#2294 read")
                .as_deref(),
            Some("# body"),
            "#2294 'p 1' resolves to plans/p-1.md"
        );
        negative_assert_eq!(
            store.read_plan_markdown("p2").expect("#2294 read"),
            None,
            "#2294 missing markdown reads as None"
        );
    }

    // `updateStatus` (DiscoveryPlanStore.ts:69-96): the entry mutates in
    // place, so sibling keys and their order survive; a missing plan writes
    // nothing; the executionStatus mirror fires only for terminal statuses.
    #[test]
    fn update_status_preserves_sibling_fields_and_order() {
        let home = scratch("update-status");
        let paths = paths(&home);
        // The fixture is written as the raw TS bytes: `serde_json`'s map
        // sorts keys, but the claim under test is that the store preserves
        // the file's own key order through the round trip.
        write_raw(
            &paths.plan_index_file,
            "{\n  \"schemaVersion\": 1,\n  \"plans\": [\n    {\n      \"id\": \"p1\",\n                   \"title\": \"T\",\n      \"status\": \"draft\",\n      \"summary\": \"s\",\n                   \"executionStatus\": \"completed\"\n    }\n  ]\n}",
        );
        let store = DiscoveryPlanStore::new(paths.clone());
        let updated = store
            .update_status("p1", "ready")
            .expect("#2294 update")
            .expect("#2294 p1");
        negative_assert_eq!(updated.id, JsString::from("p1"), "#2294 updated view");
        let on_disk = std::fs::read_to_string(&paths.plan_index_file).expect("#2294 read");
        negative_assert_eq!(
            on_disk,
            "{\n  \"schemaVersion\": 1,\n  \"plans\": [\n    {\n      \"id\": \"p1\",\n      \"title\": \
             \"T\",\n      \"status\": \"ready\",\n      \"summary\": \"s\",\n      \"executionStatus\": \
             \"completed\"\n    }\n  ]\n}",
            "#2294 updateStatus keeps every sibling key in place"
        );
        // A terminal status mirrors executionStatus (DiscoveryPlanStore.ts:83-85).
        store.update_status("p1", "failed").expect("#2294 update");
        let mirrored = std::fs::read_to_string(&paths.plan_index_file).expect("#2294 read");
        negative_assert_eq!(
            serde_json::from_str::<serde_json::Value>(&mirrored).expect("#2294 parse")["plans"][0]
                ["executionStatus"],
            "failed",
            "#2294 terminal statuses mirror executionStatus"
        );
        // A missing plan leaves the file untouched.
        let before = std::fs::read_to_string(&paths.plan_index_file).expect("#2294 read");
        negative_assert_eq!(
            store
                .update_status("p-missing", "ready")
                .expect("#2294 update")
                .is_none(),
            true,
            "#2294 missing plan reads as None"
        );
        negative_assert_eq!(
            std::fs::read_to_string(&paths.plan_index_file).expect("#2294 read"),
            before,
            "#2294 missing plan writes nothing"
        );
    }
}

#[cfg(test)]
mod native_storage_tests {
    use super::*;
    use std::path::PathBuf;
    struct Scratch(PathBuf);
    impl Drop for Scratch { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }
    fn paths(root: &Path) -> AlwaysOnPaths { AlwaysOnPaths {
        project_key: "/fixture-project".into(), state_file: root.join("state.json"),
        plans_dir: root.join("plans"), plan_index_file: root.join("plans/index.json"),
        cycle_index_file: root.join("cycles/index.json"), reports_dir: root.join("reports"), discovery_lock_file: root.join("locks/discovery.lock"),
    } }
    #[tokio::test]
    async fn apply_snapshot_preserves_utf16_and_plan_index_order_and_unknown_fields() {
        let root = Scratch(std::env::temp_dir().join(format!("pdrs-always-storage-{}", uuid::Uuid::new_v4())));
        let paths = paths(&root.0);
        let cycle_id = JsString::from_utf16(vec![99, 0xd800]);
        let plan_id = JsString::from_utf16(vec![112, 0xdc00]);
        let paths_for_job = paths.clone(); let query_cycle = cycle_id.clone(); let query_plan = plan_id.clone();
        super::super::storage::run(move || {
            let cycles = WorkCycleStore::new(paths_for_job.clone());
            let mut workspace = JsObject::new(); workspace.insert("strategy".into(), "snapshot-copy".into());
            workspace.insert("cwd".into(), JsValue::String(JsString::from_utf16(vec![47, 0xd800])));
            let created = cycles.create(&workspace, "run".into(), query_cycle.clone(), 0.0)?;
            assert_eq!(created.id, query_cycle);
            cycles.add_plan(&created.id, query_plan.clone())?; cycles.add_plan(&created.id, "second".into())?;
            let plans = DiscoveryPlanStore::new(paths_for_job);
            for id in [JsString::from("second"), JsString::from("outside"), query_plan] {
                let mut record = JsObject::new(); record.insert("id".into(), JsValue::String(id));
                record.insert("title".into(), JsValue::String(JsString::from_utf16(vec![0xd800])));
                record.insert("fixtureUnknown".into(), JsValue::Bool(true)); plans.upsert(&record)?;
            }
            Ok(())
        }).await.unwrap();
        let (cycle, selected) = WorkCycleStore::new(paths).read_apply(cycle_id.clone()).await.unwrap().unwrap();
        assert_eq!(cycle.id, cycle_id);
        assert_eq!(cycle.workspace.unwrap().cwd.units(), &[47, 0xd800]);
        assert_eq!(selected.iter().map(|record| record.id.clone()).collect::<Vec<_>>(), vec![JsString::from("second"), plan_id]);
        assert_eq!(selected[1].title.as_ref().unwrap().units(), &[0xd800]);
        assert_eq!(selected[1].raw.get("fixtureUnknown"), Some(&JsValue::Bool(true)));
    }

    #[tokio::test]
    async fn legacy_migration_uses_real_workspace_and_preserves_utf16_plan_identity_and_updates() {
        let root = Scratch(std::env::temp_dir().join(format!("pdrs-always-migrate-{}", uuid::Uuid::new_v4())));
        let workspace = root.0.join("workspace"); std::fs::create_dir_all(&workspace).unwrap();
        let paths = paths(&root.0);
        super::super::storage::run(move || {
            let mut old_workspace = JsObject::new(); old_workspace.insert("runId".into(), "legacy-run".into());
            old_workspace.insert("strategy".into(), "snapshot-copy".into()); old_workspace.insert("cwd".into(), workspace.to_string_lossy().as_ref().into());
            old_workspace.insert("metadata".into(), JsValue::Object(JsObject::new()));
            let mut state = JsObject::new(); state.insert("schemaVersion".into(), JsValue::Number(1.0));
            state.insert("fixtureUnknown".into(), JsValue::Bool(true)); state.insert("currentWorkspace".into(), JsValue::Object(old_workspace.clone()));
            write_index_object(&paths.state_file, &state)?;
            let id = JsString::from_utf16(vec![112, 0xd800]);
            let mut plan = JsObject::new(); plan.insert("id".into(), JsValue::String(id.clone()));
            plan.insert("workspace".into(), JsValue::Object(old_workspace)); plan.insert("executionStatus".into(), "pending".into());
            let plans = DiscoveryPlanStore::new(paths.clone()); plans.upsert(&plan)?;
            let cycles = WorkCycleStore::new(paths.clone());
            let record = cycles.migrate_from_legacy("cycle".into(), 0.0)?.unwrap();
            assert_eq!(record.plan_ids, vec![id.clone()]);
            assert!(cycles.migrate_from_legacy("unused".into(), 0.0)?.is_none());
            let state = legacy_object(&root_for_test(&paths), &paths.state_file)?.unwrap();
            assert_eq!(state.get("fixtureUnknown"), Some(&JsValue::Bool(true)));
            assert!(state.get("currentWorkspace").is_none());
            assert!(state.get("activeWorkCycleId").and_then(JsValue::as_string).unwrap().equals_str("cycle"));
            let migrated = plans.get_record_js(&id)?.unwrap();
            assert!(migrated.raw.get("workspace").is_none());
            assert!(migrated.raw.get("workCycleId").and_then(JsValue::as_string).unwrap().equals_str("cycle"));
            let mut update = JsObject::new(); update.insert("status".into(), "completed".into());
            update.insert("reportFilePath".into(), paths.reports_dir.join("run.md").to_string_lossy().as_ref().into());
            let updated = plans.update(&id, &update)?.unwrap();
            assert!(updated.raw.get("executionStatus").and_then(JsValue::as_string).unwrap().equals_str("completed"));
            assert!(updated.raw.get("reportFilePath").and_then(JsValue::as_string).unwrap().equals_str("reports/run.md"));
            Ok(())
        }).await.unwrap();
    }
    fn root_for_test(paths: &AlwaysOnPaths) -> std::path::PathBuf { super::super::storage::root_for(&paths.state_file) }

    #[tokio::test]
    async fn legacy_best_effort_does_not_silently_hide_native_regular_file_refusal() {
        let root = Scratch(std::env::temp_dir().join(format!("pdrs-always-migrate-fifo-{}", uuid::Uuid::new_v4())));
        std::fs::create_dir_all(&root.0).unwrap(); let paths = paths(&root.0);
        assert!(std::process::Command::new("/usr/bin/mkfifo").arg(&paths.state_file).status().unwrap().success());
        let store = WorkCycleStore::new(paths.clone());
        let result = super::super::storage::run(move || store.migrate_from_legacy("cycle".into(), 0.0)).await;
        assert!(result.is_err()); assert!(!paths.cycle_index_file.exists());
    }
}
