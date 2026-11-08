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
    pub cwd: String,
    #[allow(dead_code)]
    pub metadata: JsObject,
}

/// `WorkCycleRecord` (protocol/types.ts) as the ported halves read it: the
/// scheduler checks `status`/`planIds`, `applyCycle` reads `planIds` and the
/// workspace presence. `appliedAt`/`archivedAt` are write-side only and stay
/// unported.
#[derive(Clone, Debug)]
pub(crate) struct WorkCycleRecord {
    pub id: String,
    /// Wire-shape fields the fire seam and its reports read; the read side
    /// lands with the explicitly unported fire half.
    #[allow(dead_code)]
    pub project_key: String,
    pub status: String,
    pub workspace: Option<WorkCycleWorkspace>,
    pub plan_ids: Vec<String>,
    /// Wire-shape fields the fire seam and its reports read; the read side
    /// lands with the explicitly unported fire half.
    #[allow(dead_code)]
    pub created_at: String,
    #[allow(dead_code)]
    pub created_by_run_id: String,
}

/// `DiscoveryPlanRecord` as the control surface reads it: `rerunPlan`'s early
/// checks need existence, `applyCycle`'s projection needs `id`/`title`. The
/// remaining fields (summary, rationale, dedupeKey, …) live behind the fire
/// seam.
#[derive(Clone, Debug)]
pub(crate) struct DiscoveryPlanRecord {
    pub id: String,
    pub title: Option<String>,
}

fn js_string(value: Option<&JsValue>) -> Option<String> {
    value
        .and_then(JsValue::as_string)
        .and_then(|value| value.to_utf8().ok())
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
    let raw = match std::fs::read(file) {
        Ok(raw) => raw,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(default()),
        Err(error) => return Err(error),
    };
    let raw = String::from_utf8_lossy(&raw);
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
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(file, stringify_pretty(&JsValue::Object(index.clone())))
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
            strategy: js_string(workspace.get("strategy")).unwrap_or_default(),
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
            .filter_map(|item| js_string(Some(item)))
            .collect(),
        _ => Vec::new(),
    };
    WorkCycleRecord {
        id: js_string(entry.get("id")).unwrap_or_default(),
        project_key: js_string(entry.get("projectKey")).unwrap_or_default(),
        status: js_string(entry.get("status")).unwrap_or_default(),
        workspace,
        plan_ids,
        created_at: js_string(entry.get("createdAt")).unwrap_or_default(),
        created_by_run_id: js_string(entry.get("createdByRunId")).unwrap_or_default(),
    }
}

fn plan_record(entry: &JsObject) -> DiscoveryPlanRecord {
    DiscoveryPlanRecord {
        id: js_string(entry.get("id")).unwrap_or_default(),
        title: js_string(entry.get("title")),
    }
}

/// `WorkCycleStore` (WorkCycleStore.ts:17-18): one `cycles/index.json` per
/// project directory.
pub(crate) struct WorkCycleStore {
    paths: AlwaysOnPaths,
}

impl WorkCycleStore {
    pub(crate) fn new(paths: AlwaysOnPaths) -> Self {
        Self { paths }
    }

    fn read_index(&self) -> io::Result<JsObject> {
        read_index_object(&self.paths.cycle_index_file, "cycles")
    }

    /// `getRecord` (WorkCycleStore.ts:55-58): first entry whose `id` equals
    /// the query.
    pub(crate) fn get_record(&self, cycle_id: &str) -> io::Result<Option<WorkCycleRecord>> {
        let index = self.read_index()?;
        Ok(find_entry(&index, "cycles", |entry| {
            js_string(entry.get("id")).as_deref() == Some(cycle_id)
        })
        .map(|entry| cycle_record(&entry)))
    }

    /// `getActiveCycle` (WorkCycleStore.ts:60-63): first entry whose `status`
    /// is `"active"`.
    pub(crate) fn get_active_cycle(&self) -> io::Result<Option<WorkCycleRecord>> {
        let index = self.read_index()?;
        Ok(find_entry(&index, "cycles", |entry| {
            js_string(entry.get("status")).as_deref() == Some("active")
        })
        .map(|entry| cycle_record(&entry)))
    }
}

/// `DiscoveryPlanStore` (DiscoveryPlanStore.ts:8-9): one `plans/index.json`
/// per project directory.
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

    /// `getRecord` (DiscoveryPlanStore.ts:98-101).
    pub(crate) fn get_record(&self, plan_id: &str) -> io::Result<Option<DiscoveryPlanRecord>> {
        let index = self.read_index()?;
        Ok(find_entry(&index, "plans", |entry| {
            js_string(entry.get("id")).as_deref() == Some(plan_id)
        })
        .map(|entry| plan_record(&entry)))
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
        let path = plan_markdown_path(&self.paths, &JsString::from(plan_id));
        match std::fs::read(&path) {
            Ok(bytes) => Ok(Some(String::from_utf8_lossy(&bytes).into_owned())),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
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
            if js_string(entry.get("id")).as_deref() != Some(plan_id) {
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
            record.workspace.as_ref().map(|ws| ws.cwd.as_str()),
            Some("/w"),
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
        negative_assert_eq!(record.id, "p1", "#2294 id");
        negative_assert_eq!(record.title.as_deref(), Some("T"), "#2294 title");
        negative_assert_eq!(
            store.get_record("p-missing").expect("#2294 read").is_none(),
            true,
            "#2294 missing plan"
        );
        let listed = store.list_records().expect("#2294 list");
        negative_assert_eq!(
            listed.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
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
        negative_assert_eq!(updated.id, "p1", "#2294 updated view");
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
