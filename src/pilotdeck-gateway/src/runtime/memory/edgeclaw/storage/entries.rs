//! Repository entry operations and non-transactional clear ordering.
//! Fixed TS baseline: `core/storage/sqlite.ts:1752-2004`.
use super::super::file_memory::{EdgeClawFileStores, FileMemoryError};
use crate::metadata::js_trim;
use pilotdeck_protocol::{JsObject, JsString, JsValue, stringify_json};
use sha1::{Digest, Sha1};
use std::collections::{HashMap, HashSet};

type Result<T> = std::result::Result<T, FileMemoryError>;
fn s(v: &str) -> JsValue {
    JsValue::from(v)
}
fn object(fields: &[(&str, JsValue)]) -> JsObject {
    let mut o = JsObject::new();
    for (k, v) in fields {
        o.insert((*k).into(), v.clone());
    }
    o
}
fn array(v: JsValue) -> Vec<JsValue> {
    if let JsValue::Array(a) = &v {
        a.clone()
    } else {
        vec![]
    }
}
fn field<'a>(v: &'a JsValue, k: &str) -> &'a JsValue {
    static UNDEFINED: JsValue = JsValue::Undefined;
    v.get(k).unwrap_or(&UNDEFINED)
}
fn global(id: &JsString) -> bool {
    id.units().len() >= 7
        && id.units()[..6] == [103, 108, 111, 98, 97, 108]
        && matches!(id.units()[6], 47 | 92)
}
fn internal(id: &JsString) -> JsString {
    let u: Vec<u16> = id
        .units()
        .iter()
        .map(|u| if *u == 92 { 47 } else { *u })
        .collect();
    JsString::from_utf16(if global(id) { u[7..].to_vec() } else { u })
}
fn exposed(v: &JsValue) -> JsValue {
    let Some(id) = v.as_string() else {
        return v.clone();
    };
    let id = internal(id);
    let mut units: Vec<u16> = "global/".encode_utf16().collect();
    units.extend(id.units());
    JsValue::String(JsString::from_utf16(units))
}
fn map_record(v: JsValue) -> Result<JsValue> {
    let mut o = v
        .as_object()
        .ok_or_else(|| {
            FileMemoryError::Invalid("Internal error: file record is not an object".into())
        })?
        .clone();
    o.insert("relativePath".into(), exposed(field(&v, "relativePath")));
    Ok(JsValue::Object(o))
}
fn ids_input(ids: Vec<JsValue>, key: &str) -> JsObject {
    object(&[(key, JsValue::Array(ids))])
}
/// `ids.filter(...)` at the TS repository boundary. A missing or non-array ids
/// argument throws `TypeError` before the first store call; array elements are
/// returned verbatim for the caller's own element checks.
fn batch_ids(ids: &JsValue) -> Result<&[JsValue]> {
    match ids {
        JsValue::Array(items) => Ok(items),
        JsValue::Undefined => Err(FileMemoryError::Type(
            "Cannot read properties of undefined (reading 'filter')".into(),
        )),
        JsValue::Null => Err(FileMemoryError::Type(
            "Cannot read properties of null (reading 'filter')".into(),
        )),
        _ => Err(FileMemoryError::Type("ids.filter is not a function".into())),
    }
}
/// `deleteMemoryEntries` / `deprecateMemoryEntries` / `restoreMemoryEntries`
/// call `isGlobalRelativePath(id)` through `Array.prototype.filter`, whose
/// `normalizeRelativePath` reaches `String.prototype.replace`. Validate every
/// element before any snapshot or store work, in TS element order.
fn mutation_ids(ids: &JsValue) -> Result<Vec<JsString>> {
    batch_ids(ids)?
        .iter()
        .map(|value| match value {
            JsValue::String(id) => Ok(id.clone()),
            JsValue::Undefined => Err(FileMemoryError::Type(
                "Cannot read properties of undefined (reading 'replace')".into(),
            )),
            JsValue::Null => Err(FileMemoryError::Type(
                "Cannot read properties of null (reading 'replace')".into(),
            )),
            _ => Err(FileMemoryError::Type(
                "relativePath.replace is not a function".into(),
            )),
        })
        .collect()
}

impl EdgeClawFileStores {
    /// The both-store query shared by `listMemoryEntries` /
    /// `countMemoryEntries` (`sqlite.ts:1720-1775`): each store is capped at
    /// its own 5000-entry page. The public ports live in `storage/overview.rs`;
    /// `clearSessionMemoryData` (`sqlite.ts:1989`) reuses this internal one.
    fn entry_query(&self, options: &JsObject) -> Result<Vec<JsValue>> {
        let mut input = options.clone();
        input.insert("limit".into(), JsValue::Number(5000.0));
        input.insert("offset".into(), JsValue::Number(0.0));
        let mut entries = if input.get("scope") == Some(&s("global")) {
            vec![]
        } else {
            array(self.workspace_memory.list_memory_entries(&input)?)
        };
        if input.get("scope") != Some(&s("project")) {
            let mut global_input = object(&[
                ("limit", JsValue::Number(5000.0)),
                ("offset", JsValue::Number(0.0)),
            ]);
            for key in ["kinds", "query", "scope", "includeDeprecated"] {
                if let Some(v) = input.get(key) {
                    global_input.insert(key.into(), v.clone());
                }
            }
            entries.extend(array(self.list_global_memory_entries(&global_input)?));
        }
        Ok(entries)
    }
    /// Filter/dedupe only for reading, then replay original ids (duplicates retained).
    /// `ids` is the raw JS argument: `ids.filter` throws before any read when it
    /// is not an array; non-string elements are ignored exactly as TS.
    pub fn get_memory_records_by_ids(&self, ids: &JsValue, max_lines: f64) -> Result<JsValue> {
        self.get_memory_records_by_ids_raw(ids, &JsValue::Number(max_lines))
    }
    pub(crate) fn get_memory_records_by_ids_raw(
        &self,
        ids: &JsValue,
        max_lines: &JsValue,
    ) -> Result<JsValue> {
        let ids = batch_ids(ids)?
            .iter()
            .filter_map(JsValue::as_string)
            .cloned()
            .collect::<Vec<_>>();
        self.get_memory_records_by_id_strings(&ids, max_lines)
    }
    fn get_memory_records_by_id_strings(
        &self,
        ids: &[JsString],
        max_lines: &JsValue,
    ) -> Result<JsValue> {
        let mut seen = HashSet::new();
        let unique = ids
            .iter()
            .filter(|id| {
                !crate::runtime::memory::js_trim_units(id.units()).is_empty()
                    && seen.insert((*id).clone())
            })
            .collect::<Vec<_>>();
        let mut workspace = vec![];
        let mut globals = vec![];
        let mut external = vec![];
        for id in unique {
            if id
                .units()
                .starts_with(&[101, 120, 116, 101, 114, 110, 97, 108, 58])
            {
                external.push(id.clone());
            } else if global(id) {
                globals.push(JsValue::String(internal(id)));
            } else {
                workspace.push(JsValue::String(id.clone()));
            }
        }
        let read = |store: &super::super::file_memory::FileMemoryStore, ids| {
            let mut input = ids_input(ids, "ids");
            input.insert("maxLines".into(), max_lines.clone());
            store.get_memory_records_by_ids(&input)
        };
        let mut records = array(read(&self.workspace_memory, workspace)?);
        records.extend(
            array(read(&self.global_user_memory, globals)?)
                .into_iter()
                .map(map_record)
                .collect::<Result<Vec<_>>>()?,
        );
        records.extend(self.entry_external_records(&external, max_lines)?);
        let by_id = records
            .into_iter()
            .filter_map(|r| {
                field(&r, "relativePath")
                    .as_string()
                    .cloned()
                    .map(|id| (id, r))
            })
            .collect::<HashMap<_, _>>();
        Ok(JsValue::Array(
            ids.iter().filter_map(|id| by_id.get(id).cloned()).collect(),
        ))
    }
    pub fn edit_project_meta(&self, input: &JsObject) -> Result<JsValue> {
        self.snapshot_operation(|| self.workspace_memory.edit_project_meta(input))
    }
    pub fn ensure_project_meta(&self, input: &JsObject) -> Result<JsValue> {
        self.snapshot_operation(|| self.workspace_memory.ensure_project_meta(input))
    }
    pub fn get_project_meta(&self) -> Result<JsValue> {
        self.workspace_memory.get_project_meta(&JsObject::new())
    }
    pub fn edit_memory_entry(&self, input: &JsObject) -> Result<JsValue> {
        self.snapshot_operation(|| {
            // sqlite.ts:1845 routes through normalizeRelativePath before any IO.
            let undefined = JsValue::Undefined;
            let id = match input.get("id").unwrap_or(&undefined) {
                JsValue::String(id) => id,
                value => {
                    let message = match value {
                        JsValue::Undefined => {
                            "Cannot read properties of undefined (reading 'replace')"
                        }
                        JsValue::Null => "Cannot read properties of null (reading 'replace')",
                        _ => "relativePath.replace is not a function",
                    };
                    return Err(FileMemoryError::Type(message.into()));
                }
            };
            let is_global = global(id);
            let mut next = input.clone();
            next.insert(
                "relativePath".into(),
                JsValue::String(if is_global { internal(id) } else { id.clone() }),
            );
            let record = if is_global {
                self.global_user_memory.edit_entry(&next)?
            } else {
                self.workspace_memory.edit_entry(&next)?
            };
            if is_global {
                map_record(record)
            } else {
                Ok(record)
            }
        })
    }
    fn mutate_entries(&self, ids: &[JsString], method: &str) -> Result<JsValue> {
        self.snapshot_operation(|| {
            // Reject the whole request before either store can repair/write.
            for id in ids {
                if global(id) {
                    self.global_user_memory.validate_entry_path(&internal(id))?;
                } else {
                    self.workspace_memory.validate_entry_path(id)?;
                }
            }

            let workspace = ids
                .iter()
                .filter(|id| !global(id))
                .cloned()
                .map(JsValue::String)
                .collect();
            let globals = ids
                .iter()
                .filter(|id| global(id))
                .map(|id| JsValue::String(internal(id)))
                .collect();
            let w = self
                .workspace_memory
                .execute(method, &ids_input(workspace, "relativePaths"))?;
            let g = self
                .global_user_memory
                .execute(method, &ids_input(globals, "relativePaths"))?;
            self.workspace_memory.repair_manifests()?;
            let mut mutated = array(field(&w, "mutatedIds").clone());
            mutated.extend(array(field(&g, "mutatedIds").clone()).iter().map(exposed));
            let mut deleted = array(field(&w, "deletedProjectIds").clone());
            deleted.extend(array(field(&g, "deletedProjectIds").clone()));
            Ok(JsValue::Object(object(&[
                ("mutatedIds", JsValue::Array(mutated)),
                ("deletedProjectIds", JsValue::Array(deleted)),
            ])))
        })
    }
    pub fn delete_memory_entries(&self, ids: &JsValue) -> Result<JsValue> {
        let ids = mutation_ids(ids)?;
        self.mutate_entries(&ids, "deleteEntries")
    }
    pub fn deprecate_memory_entries(&self, ids: &JsValue) -> Result<JsValue> {
        let ids = mutation_ids(ids)?;
        self.mutate_entries(&ids, "markEntriesDeprecated")
    }
    pub fn restore_memory_entries(&self, ids: &JsValue) -> Result<JsValue> {
        let ids = mutation_ids(ids)?;
        self.mutate_entries(&ids, "restoreEntries")
    }
    pub fn archive_tmp_entries(&self, input: &JsObject) -> Result<JsValue> {
        let mut next = input.clone();
        next.insert(
            "relativePaths".into(),
            input.get("ids").cloned().unwrap_or(JsValue::Undefined),
        );
        self.workspace_memory.archive_tmp_entries(&next)
    }
    pub fn get_snapshot_version(&self) -> Result<String> {
        self.snapshot_operation(|| self.snapshot_version_locked())
    }
    fn snapshot_version_locked(&self) -> Result<String> {
        let last = self
            .storage
            .get_pipeline_state("lastDreamAt")?
            .filter(|v| !matches!(v, JsValue::Null | JsValue::Undefined))
            .unwrap_or(s(""));
        // exportMemoryBundle evaluates trace reads before exporting workspace files.
        self.storage.list_recent_case_traces(200)?;
        self.storage.list_recent_index_traces(200)?;
        self.storage.list_recent_dream_traces(200)?;
        let files = array(self.workspace_memory.export_snapshot_files()?)
            .into_iter()
            .filter(|f| field(f, "relativePath") != &s("MEMORY.md"))
            .collect();
        let text = stringify_json(&JsValue::Object(object(&[
            ("lastDreamAt", last),
            ("files", JsValue::Array(files)),
        ])))
        .map_err(|e| FileMemoryError::Invalid(e.to_string()))?
        .expect("object JSON");
        Ok(format!("{:x}", Sha1::digest(text.as_bytes()))[..10].into())
    }
    pub fn clear_all_memory_data(&self) -> Result<JsValue> {
        self.clear_memory(false)
    }
    pub fn clear_current_workspace_memory_data(&self) -> Result<JsValue> {
        self.clear_memory(true)
    }
    fn clear_memory(&self, current: bool) -> Result<JsValue> {
        self.snapshot_operation(|| {
            let (l0, pipeline) = {
                let conn = self.storage.conn()?;
                (
                    conn.query_row("SELECT COUNT(*) FROM l0_sessions", [], |r| {
                        r.get::<_, i64>(0)
                    })?,
                    conn.query_row("SELECT COUNT(*) FROM pipeline_state", [], |r| {
                        r.get::<_, i64>(0)
                    })?,
                )
            };
            let w = self.workspace_memory.export_bundle_records()?;
            let g = if current {
                JsValue::Undefined
            } else {
                self.global_user_memory.export_bundle_records()?
            };
            let settings = if current {
                self.storage.get_pipeline_state("indexingSettings")?
            } else {
                None
            };
            let workspace = if current {
                self.storage.get_pipeline_state("workspaceDir")?
            } else {
                None
            };
            // Deliberately auto-commit each statement, as DatabaseSync.exec does.
            self.storage
                .conn()?
                .execute_batch("DELETE FROM l0_sessions; DELETE FROM pipeline_state;")?;
            if let Some(v) = settings {
                self.storage.set_pipeline_state("indexingSettings", &v)?;
            }
            if let Some(v) = workspace {
                self.storage.set_pipeline_state("workspaceDir", &v)?;
            }
            let options = object(&[("rebuildManifest", JsValue::Bool(false))]);
            (self.repository.snapshot_io)(
                super::snapshots::SnapshotIoOperation::Remove,
                self.workspace_memory.get_root_dir(),
                None,
            )?;
            self.workspace_memory.execute("clearAllData", &options)?;
            if !current {
                self.global_user_memory.execute("clearAllData", &options)?;
            }
            self.clear_last_dream_snapshot()?;
            Ok(self.clear_result(
                if current {
                    "current_project"
                } else {
                    "all_memory"
                },
                l0 as usize,
                pipeline as usize,
                array(field(&w, "memoryFiles").clone()).len()
                    + array(field(&g, "memoryFiles").clone()).len(),
                array(field(&w, "projectMetas").clone()).len(),
            ))
        })
    }
    pub fn clear_session_memory_data(&self, session: &str) -> Result<JsValue> {
        self.snapshot_operation(|| {
            let session = js_trim(session);
            if session.is_empty() {
                return Err(FileMemoryError::Invalid("sessionKey is required.".into()));
            }
            let l0 = self.storage.conn()?.query_row(
                "SELECT COUNT(*) FROM l0_sessions WHERE session_key = ?",
                [session],
                |r| r.get::<_, i64>(0),
            )?;
            let options = object(&[
                ("includeDeprecated", JsValue::Bool(true)),
                ("includeTmp", JsValue::Bool(true)),
            ]);
            let mut entries = self.entry_query(&options)?;
            super::accessors::sort_entry_values(&mut entries)?;
            entries.truncate(5000);
            let ids = JsValue::Array(
                entries
                    .iter()
                    .filter(|e| field(e, "sourceSessionKey") == &s(session))
                    .filter_map(|e| field(e, "relativePath").as_string().cloned())
                    .map(JsValue::String)
                    .collect(),
            );
            let deleted = self.delete_memory_entries(&ids)?;
            self.storage
                .conn()?
                .execute("DELETE FROM l0_sessions WHERE session_key = ?", [session])?;
            Ok(self.clear_result(
                "session",
                l0 as usize,
                0,
                array(field(&deleted, "mutatedIds").clone()).len(),
                array(field(&deleted, "deletedProjectIds").clone()).len(),
            ))
        })
    }
    fn clear_result(
        &self,
        scope: &str,
        l0: usize,
        pipeline: usize,
        files: usize,
        metas: usize,
    ) -> JsValue {
        JsValue::Object(object(&[
            ("scope", s(scope)),
            (
                "cleared",
                JsValue::Object(object(&[
                    ("l0Sessions", JsValue::Number(l0 as f64)),
                    ("pipelineState", JsValue::Number(pipeline as f64)),
                    ("memoryFiles", JsValue::Number(files as f64)),
                    ("projectMetas", JsValue::Number(metas as f64)),
                ])),
            ),
            ("clearedAt", s(&(self.repository.clock)())),
        ]))
    }
}
// Repository SQL failures keep the existing typed storage error.
impl From<rusqlite::Error> for FileMemoryError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Storage(super::StorageError::Sqlite(e))
    }
}

#[cfg(test)]
mod round6_tests {
    use super::*;
    #[test]
    fn malformed_global_record_returns_internal_error() {
        for value in [JsValue::Null, JsValue::Undefined, JsValue::Array(vec![])] {
            assert!(
                matches!(map_record(value), Err(FileMemoryError::Invalid(message))
                if message == "Internal error: file record is not an object")
            );
        }
    }
}
