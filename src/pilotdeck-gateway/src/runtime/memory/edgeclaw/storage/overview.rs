//! Dashboard overview, UI snapshot and repository-level memory entry queries.
//! Source: fixed baseline `core/storage/sqlite.ts:1660-1775`.
//!
//! The overview reports live counts from both stores plus the Dream snapshot
//! read from `super::bundle`; list/count merge both stores' manifest entries
//! and re-sort with `localeCompare`, matching the TS ordering.
use super::{EdgeClawStorage, IndexingSettings, StorageError};
use crate::project_files::{node_collator, node_compare_utf16, node_locale};
use crate::runtime::memory::edgeclaw::file_memory::{EdgeClawFileStores, FileMemoryError};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
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
/// `sortManifestEntries` (`sqlite.ts:385-391`): `updatedAt` descending, ties
/// broken by `relativePath` ascending, both with `localeCompare`.
fn sort_manifest_entries(entries: &mut [JsObject]) -> Result<()> {
    if entries.len() < 2 {
        return Ok(());
    }
    let collator = node_collator(&node_locale(|key| std::env::var(key).ok()))
        .map_err(|error| FileMemoryError::Invalid(format!("{error:?}")))?;
    entries.sort_by(|a, b| {
        let updated_a = text(a, "updatedAt");
        let updated_b = text(b, "updatedAt");
        if updated_a != updated_b {
            node_compare_utf16(
                &collator,
                &JsString::from(updated_b),
                &JsString::from(updated_a),
            )
        } else {
            node_compare_utf16(
                &collator,
                &JsString::from(text(a, "relativePath")),
                &JsString::from(text(b, "relativePath")),
            )
        }
    });
    Ok(())
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
    /// `getOverview` (`sqlite.ts:1660-1706`).
    pub fn get_overview(&self) -> Result<JsValue> {
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
        let readable_projects: Vec<JsObject> = if self.get_workspace_mode() == "general" {
            objects(self.list_readable_project_catalog()?)
                .into_iter()
                .filter(|project| text(project, "sourceType") != "workspace_external")
                .collect()
        } else {
            vec![]
        };
        let workspace_has_project_memory =
            number_of("projectMemories") + number_of("feedbackMemories") > 0.0;
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
    /// `getUiSnapshot` (`sqlite.ts:1708-1718`).
    pub fn get_ui_snapshot(&self, limit: Option<f64>) -> Result<JsValue> {
        let options = obj(&[("limit", JsValue::Number(limit.unwrap_or(50.0)))]);
        let settings = self.storage.get_indexing_settings(&IndexingSettings {
            reasoning_mode: "answer_first".into(),
            auto_index_interval_minutes: 30,
            auto_dream_interval_minutes: 60,
        })?;
        Ok(JsValue::Object(obj(&[
            ("overview", self.get_overview()?),
            ("settings", settings.to_js()),
            ("recentMemoryFiles", self.list_memory_entries(&options)?),
        ])))
    }
    /// `listMemoryEntries` (`sqlite.ts:1720-1750`).
    pub fn list_memory_entries(&self, options: &JsObject) -> Result<JsValue> {
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
        sort_manifest_entries(&mut filtered)?;
        // `Math.max(0, options.offset ?? 0)` / `Math.max(1, options.limit ?? …)`:
        // JS `Math.max` keeps NaN, so a NaN offset or limit empties the slice.
        // Rust's `f64::max` returns the other operand, so NaN is checked before
        // the clamps instead of after them.
        let raw_offset = options
            .get("offset")
            .and_then(JsValue::as_number)
            .unwrap_or(0.0);
        let raw_limit = match options.get("limit").and_then(JsValue::as_number) {
            Some(value) => value,
            None => (filtered.len().max(1)) as f64,
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
