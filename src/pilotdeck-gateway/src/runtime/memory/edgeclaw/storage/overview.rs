//! Dashboard overview, UI snapshot and repository-level memory entry queries.
//! Source: fixed baseline `core/storage/sqlite.ts:1660-1775`.
//!
//! The overview reports live counts from both stores plus the Dream snapshot
//! read from `super::bundle`; list/count merge both stores' manifest entries
//! and re-sort with `localeCompare`, matching the TS ordering.
use super::{EdgeClawStorage, IndexingSettings, StorageError};
use crate::runtime::memory::edgeclaw::file_memory::{
    EdgeClawFileStores, FileMemoryError, OperationGuard, helpers::sort_manifest_entries,
};
use pilotdeck_protocol::{JsObject, JsValue};
type Result<T> = std::result::Result<T, FileMemoryError>;

const LAST_INDEXED_AT_STATE_KEY: &str = "lastIndexedAt";
const LAST_DREAM_AT_STATE_KEY: &str = "lastDreamAt";
const LAST_DREAM_STATUS_STATE_KEY: &str = "lastDreamStatus";
const LAST_DREAM_SUMMARY_STATE_KEY: &str = "lastDreamSummary";

// -- local JS-object helpers (same shapes as `storage/accessors.rs`) --------

fn obj(fields: &[(&str, JsValue)]) -> JsObject {
    let mut object = JsObject::new();
    for (key, value) in fields {
        object.insert((*key).into(), value.clone());
    }
    object
}
fn s(value: impl AsRef<str>) -> JsValue {
    JsValue::from(value.as_ref())
}
fn text(object: &JsObject, key: &str) -> String {
    object
        .get(key)
        .and_then(JsValue::as_string)
        .map(|value| String::from_utf16_lossy(value.units()))
        .unwrap_or_default()
}
fn objects(value: JsValue) -> Vec<JsObject> {
    match &value {
        JsValue::Array(items) => items
            .iter()
            .filter_map(|value| value.as_object().cloned())
            .collect(),
        _ => vec![],
    }
}
fn values(items: Vec<JsObject>) -> JsValue {
    JsValue::Array(items.into_iter().map(JsValue::Object).collect())
}
/// JS `Number(value)` for the `Math.max` coercions in `listMemoryEntries`,
/// on top of the shared ECMAScript ports in [`crate::js_value`]. Only objects
/// and arrays can fail: ToPrimitive on a wire object with an own `toString`
/// property throws `TypeError: Cannot convert object to primitive value`
/// (JSON cannot hold a callable, so it shadows the prototype method with a
/// non-callable, and `valueOf` never yields a primitive) — directly and inside
/// arrays, whose join converts their elements; a too-deep array joins with
/// the same "Maximum call stack size exceeded" `RangeError` as TS.
fn to_number(value: &JsValue) -> Result<f64> {
    match value {
        JsValue::Object(_) | JsValue::Array(_) => {
            let units = crate::js_value::to_string_units_checked(value).map_err(|error| {
                FileMemoryError::Type(
                    match error {
                        crate::js_value::ToStringError::NotPrimitive => {
                            crate::js_value::ToStringError::NOT_PRIMITIVE
                        }
                        crate::js_value::ToStringError::StackOverflow => {
                            crate::js_value::StackOverflow::MESSAGE
                        }
                    }
                    .into(),
                )
            })?;
            Ok(crate::js_value::string_to_number(
                &String::from_utf16_lossy(&units),
            ))
        }
        _ => crate::js_value::to_number(value)
            .map_err(|_| FileMemoryError::Type(crate::js_value::StackOverflow::MESSAGE.into())),
    }
}
impl EdgeClawStorage {
    /// `sqlite.ts:1662` — `COUNT(DISTINCT session_key) ... WHERE indexed = 0`.
    fn count_pending_sessions(&self) -> std::result::Result<f64, StorageError> {
        Ok(self
            .conn()?
            .query_row(
                "SELECT COUNT(DISTINCT session_key) AS count FROM l0_sessions WHERE indexed = 0",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map(|count| count as f64)?)
    }
}
impl EdgeClawFileStores {
    /// TS repository-level reads (`getOverview`, `getUiSnapshot`,
    /// `listMemoryEntries`, `countMemoryEntries`) are single synchronous
    /// functions on a single thread, so no writer can interleave between
    /// their storage, workspace-store and global-store reads. The per-root
    /// operation locks approximate that window; every acquisition in this
    /// module (and the store calls it reaches) follows one order:
    ///
    ///   own workspace root → own external-catalog cache → sibling
    ///   workspace roots (general-mode scan) → global user root
    ///
    /// - normal workspace reads/writes and provider nesting take the
    ///   workspace root first and the global root second — `read_scope`,
    ///   the import critical section (`bundle.rs`), `snapshot_operation`
    ///   (`snapshots.rs`) and the `manifest_user_entries_provider` closure;
    /// - the general-mode catalog scan locks sibling workspace roots
    ///   (constructing and querying their snapshot stores, including the
    ///   external-catalog cache mutex) while only the own workspace root is
    ///   held — never the global root: sibling repositories hold their own
    ///   workspace root while waiting for the shared global root, so
    ///   scanning under the global lock lets two plain reads deadlock
    ///   (B→G→B; the `overview` tests pin that interleaving).
    ///
    /// The workspace guard therefore covers the whole read, while the global
    /// guard starts only after the scan. Locks are reentrant per thread, so
    /// store-level `execute` calls, `getUiSnapshot`'s composite reads and
    /// nested providers keep working under an already-held guard. Mutual
    /// general-to-general cataloging (two general repositories scanning each
    /// other) is the one layout this order does not cover: both scans would
    /// hold their own workspace root while taking each other's.
    fn read_scope(&self) -> Result<(OperationGuard<'_>, OperationGuard<'_>)> {
        let workspace = self.workspace_memory.operation_scope()?;
        let global = self.global_user_memory.operation_scope()?;
        Ok((workspace, global))
    }
    /// `getOverview`'s general-mode readable-project list
    /// (`sqlite.ts:1671-1673`): the catalog scan minus its
    /// `workspace_external` filter target. Runs before the global root's
    /// lock is taken, because the scan locks sibling workspace roots, which
    /// their own repositories hold ahead of the shared global root (see
    /// `read_scope`).
    fn general_catalog(&self) -> Result<Vec<JsObject>> {
        if self.get_workspace_mode() != "general" {
            return Ok(vec![]);
        }
        Ok(objects(self.list_readable_project_catalog()?)
            .into_iter()
            .filter(|project| text(project, "sourceType") != "workspace_external")
            .collect())
    }
    /// `getOverview` (`sqlite.ts:1660-1706`). The workspace root's guard
    /// opens the consistency window; the general-mode scan runs inside it
    /// but before the global root's guard, whose lock the scanned siblings
    /// take behind their own workspace root (`read_scope`).
    pub fn get_overview(&self) -> Result<JsValue> {
        let _workspace_lock = self.workspace_memory.operation_scope()?;
        let readable_projects = self.general_catalog()?;
        let _global_lock = self.global_user_memory.operation_scope()?;
        self.overview_locked(&readable_projects)
    }
    /// The `getOverview` body with the workspace and global guards already
    /// held; inner acquisitions reenter them as no-ops. `readable_projects`
    /// is the general-mode catalog scanned by the caller before the global
    /// guard (`sqlite.ts:1671-1673`).
    fn overview_locked(&self, readable_projects: &[JsObject]) -> Result<JsValue> {
        let pending_sessions = self.storage.count_pending_sessions()?;
        let last_dream_at = self.storage.get_pipeline_state(LAST_DREAM_AT_STATE_KEY)?;
        let mut file_input = JsObject::new();
        if let Some(JsValue::String(value)) = &last_dream_at {
            file_input.insert("lastDreamAt".into(), JsValue::String(value.clone()));
        }
        let file_overview = self
            .workspace_memory
            .get_overview(&file_input)?
            .as_object()
            .cloned()
            .unwrap_or_default();
        let number_of = |key: &str| {
            file_overview
                .get(key)
                .and_then(JsValue::as_number)
                .unwrap_or(0.0)
        };
        let workspace_has_project_memory =
            number_of("projectMemories") + number_of("feedbackMemories") > 0.0;
        // `limit: 10` is the TS literal too (sqlite.ts:1674-1677): the probe
        // only inspects the first 10 global user entries, by design.
        let user_profile_count = usize::from(
            objects(self.list_global_memory_entries(&obj(&[
                ("kinds", JsValue::Array(vec![s("user")])),
                ("scope", s("global")),
                ("limit", JsValue::Number(10.0)),
            ]))?)
            .iter()
            .any(|entry| {
                text(entry, "relativePath")
                    == super::bundle::to_exposed_global_relative_path(
                        "UserIdentity/user-profile.md",
                    )
            }),
        );
        let mut overview = obj(&[
            ("pendingSessions", JsValue::Number(pending_sessions)),
            ("workspaceMode", s(self.get_workspace_mode())),
            (
                "currentProjectCount",
                JsValue::Number(match self.get_workspace_mode() {
                    "general" => readable_projects.len(),
                    _ if workspace_has_project_memory || number_of("projectMetaCount") > 0.0 => 1,
                    _ => 0,
                } as f64),
            ),
            (
                "projectMetaPresent",
                JsValue::Bool(number_of("projectMetaCount") > 0.0),
            ),
            (
                "projectMemoryCount",
                JsValue::Number(number_of("projectMemories")),
            ),
            (
                "feedbackMemoryCount",
                JsValue::Number(number_of("feedbackMemories")),
            ),
            (
                "userProfileCount",
                JsValue::Number(user_profile_count as f64),
            ),
            (
                "recentRecallTraceCount",
                JsValue::Number(self.storage.list_recent_case_traces(12)?.len() as f64),
            ),
            (
                "recentIndexTraceCount",
                JsValue::Number(self.storage.list_recent_index_traces(30)?.len() as f64),
            ),
            (
                "recentDreamTraceCount",
                JsValue::Number(self.storage.list_recent_dream_traces(30)?.len() as f64),
            ),
        ]);
        if let Some(JsValue::String(ref value)) =
            self.storage.get_pipeline_state(LAST_INDEXED_AT_STATE_KEY)?
        {
            overview.insert("lastIndexedAt".into(), JsValue::String(value.clone()));
        }
        if let Some(JsValue::String(ref value)) = last_dream_at {
            overview.insert("lastDreamAt".into(), JsValue::String(value.clone()));
        }
        if let Some(status) = super::bundle::sanitize_dream_status(
            self.storage
                .get_pipeline_state(LAST_DREAM_STATUS_STATE_KEY)?
                .as_ref(),
        ) {
            overview.insert("lastDreamStatus".into(), JsValue::String(status));
        }
        if let Some(JsValue::String(ref value)) = self
            .storage
            .get_pipeline_state(LAST_DREAM_SUMMARY_STATE_KEY)?
        {
            overview.insert("lastDreamSummary".into(), JsValue::String(value.clone()));
        }
        // `...(lastDreamSnapshot ? { lastDreamSnapshot } : {})` (sqlite.ts:1704):
        // the overview is either an object (truthy) or `undefined`.
        let last_dream_snapshot = self.get_last_dream_snapshot_overview()?;
        if !matches!(last_dream_snapshot, JsValue::Undefined | JsValue::Null) {
            overview.insert("lastDreamSnapshot".into(), last_dream_snapshot);
        }
        Ok(JsValue::Object(overview))
    }
    /// `getUiSnapshot` (`sqlite.ts:1708-1718`). The `limit = 50` default
    /// parameter applies only when the argument is `undefined`; any other
    /// value reaches `listMemoryEntries` untouched, whose `Math.max` coerces
    /// it (`getUiSnapshot(null)` lists everything, `'abc'` lists nothing).
    /// Like `get_overview`, the general-mode scan runs before the global
    /// root's guard so a sibling waiting on the shared global root cannot
    /// cycle with it (`read_scope`); the composite reads then share one
    /// workspace-plus-global window.
    pub fn get_ui_snapshot(&self, limit: Option<&JsValue>) -> Result<JsValue> {
        let _workspace_lock = self.workspace_memory.operation_scope()?;
        let limit = match limit {
            None | Some(JsValue::Undefined) => JsValue::Number(50.0),
            Some(value) => value.clone(),
        };
        let options = obj(&[("limit", limit)]);
        let readable_projects = self.general_catalog()?;
        let _global_lock = self.global_user_memory.operation_scope()?;
        // Object-literal evaluation order (`sqlite.ts:1710-1716`): the
        // overview, then the settings, then the entry list.
        let overview = self.overview_locked(&readable_projects)?;
        let settings = self.storage.get_indexing_settings(&IndexingSettings {
            reasoning_mode: "answer_first".into(),
            auto_index_interval_minutes: 30,
            auto_dream_interval_minutes: 60,
        })?;
        Ok(JsValue::Object(obj(&[
            ("overview", overview),
            ("settings", settings.to_js()),
            ("recentMemoryFiles", self.list_memory_entries(&options)?),
        ])))
    }
    /// `listMemoryEntries` (`sqlite.ts:1720-1750`).
    pub fn list_memory_entries(&self, options: &JsObject) -> Result<JsValue> {
        let (_workspace_lock, _global_lock) = self.read_scope()?;
        let scope = text(options, "scope");
        let mut filtered = vec![];
        if scope != "global" {
            let mut input = options.clone();
            input.insert("limit".into(), JsValue::Number(5000.0));
            input.insert("offset".into(), JsValue::Number(0.0));
            filtered.extend(objects(self.workspace_memory.list_memory_entries(&input)?));
        }
        if scope != "project" {
            let mut input = options.clone();
            input.insert("limit".into(), JsValue::Number(5000.0));
            input.insert("offset".into(), JsValue::Number(0.0));
            filtered.extend(objects(self.list_global_memory_entries(&input)?));
        }
        // `sortManifestEntries` (`sqlite.ts:385-391`): the shared helper the
        // store-level listing uses, so both ports of the TS function keep one
        // collation and tie-break implementation.
        sort_manifest_entries(
            &mut filtered,
            |entry| text(entry, "updatedAt"),
            "relativePath",
        )?;
        // `Math.max(0, options.offset ?? 0)` / `Math.max(1, options.limit ?? …)`:
        // `??` only treats null/undefined as missing, and `Math.max` coerces its
        // arguments with JS ToNumber, so `'3'` → 3, `true` → 1 and `'abc'` →
        // NaN. NaN survives JS `Math.max`, emptying the slice; Rust's `f64::max`
        // returns the other operand, so NaN is checked before the clamps
        // instead of after them.
        let raw_offset = match options.get("offset") {
            None | Some(JsValue::Undefined) | Some(JsValue::Null) => 0.0,
            Some(value) => to_number(value)?,
        };
        let raw_limit = match options.get("limit") {
            None | Some(JsValue::Undefined) | Some(JsValue::Null) => (filtered.len().max(1)) as f64,
            Some(value) => to_number(value)?,
        };
        if raw_offset.is_nan() || raw_limit.is_nan() {
            return Ok(values(vec![]));
        }
        let offset = raw_offset.max(0.0);
        let limit = raw_limit.max(1.0);
        let start = (offset as usize).min(filtered.len());
        let end = ((offset + limit) as usize).min(filtered.len());
        Ok(values(
            filtered.into_iter().skip(start).take(end - start).collect(),
        ))
    }
    /// `countMemoryEntries` (`sqlite.ts:1752-1775`).
    pub fn count_memory_entries(&self, options: &JsObject) -> Result<JsValue> {
        let (_workspace_lock, _global_lock) = self.read_scope()?;
        let scope = text(options, "scope");
        let mut input = options.clone();
        input.insert("limit".into(), JsValue::Number(5000.0));
        input.insert("offset".into(), JsValue::Number(0.0));
        let mut count = 0.0;
        if scope != "global" {
            count += objects(self.workspace_memory.list_memory_entries(&input)?).len() as f64;
        }
        if scope != "project" {
            count += objects(self.list_global_memory_entries(&input)?).len() as f64;
        }
        Ok(JsValue::Number(count))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The overview's pipeline-state reads and store reads form one window: a
    /// writer holding the workspace store's operation lock while flipping the
    /// `lastDreamAt` state must not be able to land between them. Without the
    /// overview-side locks the thread reads the old state before blocking on
    /// the store's own lock; with them, it observes the writer's state.
    #[test]
    fn overview_reads_state_and_files_in_one_window() {
        let parent = std::env::temp_dir().join(format!(
            "pdrs-overview-window-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&parent).unwrap();
        let clock: crate::runtime::memory::edgeclaw::file_memory::FileMemoryClock =
            std::sync::Arc::new(|| "unit-test".into());
        let stores = EdgeClawFileStores::with_workspace_clock(
            parent.join("control.sqlite"),
            parent.join("memory"),
            parent.join("global"),
            "/fixture/Unit",
            parent.join("home/.pilotdeck"),
            clock,
        )
        .unwrap();
        stores
            .storage
            .set_pipeline_state(LAST_DREAM_AT_STATE_KEY, &s("pre-writer"))
            .unwrap();
        let writer_lock = stores.workspace_memory.operation_scope().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let overview = std::thread::scope(|scope| {
            let reader = scope.spawn({
                let stores = &stores;
                move || {
                    let _ = tx.send(());
                    stores.get_overview()
                }
            });
            rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
            assert!(
                rx.recv_timeout(std::time::Duration::from_secs(1)).is_err(),
                "the overview must block while a writer holds the store's operation lock"
            );
            stores
                .storage
                .set_pipeline_state(LAST_DREAM_AT_STATE_KEY, &s("post-writer"))
                .unwrap();
            drop(writer_lock);
            reader.join().unwrap().unwrap()
        });
        assert_eq!(
            text(overview.as_object().unwrap(), "lastDreamAt"),
            "post-writer"
        );
        stores.close().unwrap();
        std::fs::remove_dir_all(&parent).unwrap();
    }

    /// Round 7's P0 regression: a general repository A and a single
    /// repository B under one workspaces root share the global store G. B's
    /// `listMemoryEntries` takes B's workspace root before G, while A's
    /// general-mode catalog scan takes sibling workspace roots — B's. Taking
    /// G before that scan lets the two plain reads deadlock (B→G→B), so the
    /// scan must run while only A's workspace root is held.
    ///
    /// The interleaving is pinned with channels: the B thread parks holding
    /// B's workspace-root lock — as any in-flight B store operation would —
    /// then runs `list_memory_entries`, which waits on G; the A thread runs
    /// `get_overview`, which waits on B's root inside the scan. With the
    /// scan ordered before G both sides return within the deadline; with
    /// the pre-fix order neither does, so the joins below fail on the
    /// deadline instead of hanging (the parked threads are detached, not
    /// joined by `thread::scope`). Round 1 parks in the scan's
    /// snapshot-store construction (cold catalog cache); round 2, warmed by
    /// round 1, parks in the catalog query (`accessors.rs`
    /// `listReadableProjectCatalog`'s `getProjectMeta`).
    #[test]
    fn overview_scan_does_not_cycle_with_sibling_global_wait() {
        let parent = std::env::temp_dir().join(format!(
            "pdrs-overview-cycle-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&parent).unwrap();
        let clock: crate::runtime::memory::edgeclaw::file_memory::FileMemoryClock =
            std::sync::Arc::new(|| "unit-test".into());
        let open = |dir: &str, workspace: &str| {
            EdgeClawFileStores::with_workspace_clock(
                parent.join(dir).join("control.sqlite"),
                parent.join(dir).join("memory"),
                parent.join("global"),
                parent.join(workspace),
                parent.join("home/.pilotdeck"),
                clock.clone(),
            )
            .unwrap()
        };
        let a = std::sync::Arc::new(open("workspaces/a", "home/.pilotdeck"));
        let b = std::sync::Arc::new(open("workspaces/b", "workspaces/b-project"));
        // The scan discovers B through its `workspaceDir` pipeline state.
        b.storage
            .set_pipeline_state(
                "workspaceDir",
                &s(parent.join("workspaces/b-project").to_string_lossy()),
            )
            .unwrap();
        for round in 0..2 {
            let (b_held, held) = std::sync::mpsc::channel::<()>();
            let (b_go, go) = std::sync::mpsc::channel::<()>();
            let (b_finished, b_done) = std::sync::mpsc::channel::<()>();
            let (a_started, started) = std::sync::mpsc::channel::<()>();
            let (a_finished, a_done) = std::sync::mpsc::channel::<()>();
            let b_path = std::thread::spawn({
                let b = b.clone();
                move || {
                    // Hold B's workspace root across the parked handoff, as
                    // an in-flight B store operation would; the call below
                    // reenters it and then waits on the shared global root.
                    let _b_operation = b.workspace_memory.operation_scope().unwrap();
                    let _ = b_held.send(());
                    go.recv_timeout(std::time::Duration::from_secs(30)).unwrap();
                    b.list_memory_entries(&JsObject::new()).unwrap();
                    let _ = b_finished.send(());
                }
            });
            held.recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            let a_path = std::thread::spawn({
                let a = a.clone();
                move || {
                    let _ = a_started.send(());
                    a.get_overview().unwrap();
                    let _ = a_finished.send(());
                }
            });
            started
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            // `get_overview` is now parked on B's workspace root under either
            // lock order — the same blocked-window idiom as the test above —
            // so releasing B must let it through unless some guard is held
            // in the wrong order.
            assert!(
                matches!(
                    a_done.recv_timeout(std::time::Duration::from_secs(1)),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout)
                ),
                "round {round}: the general scan must park on the sibling's workspace root"
            );
            let _ = b_go.send(());
            let deadline = std::time::Duration::from_secs(10);
            a_done.recv_timeout(deadline).unwrap_or_else(|_| {
                panic!("round {round}: get_overview did not return: the scan holds the shared global root while the sibling waits on it (B→G→B)")
            });
            b_done.recv_timeout(deadline).unwrap_or_else(|_| {
                panic!("round {round}: the sibling's list_memory_entries did not return")
            });
            a_path.join().unwrap();
            b_path.join().unwrap();
        }
        // Same-thread nesting: the composite reenters the held workspace
        // root through `overview_locked` and `list_memory_entries`.
        a.get_ui_snapshot(None).unwrap();
        a.close().unwrap();
        b.close().unwrap();
        std::fs::remove_dir_all(&parent).unwrap();
    }
}
