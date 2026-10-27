//! `DreamRewriteRunner.run` orchestration (`dream-review.ts:1026-1492`).
//!
//! One complete staged Dream pass over a repository: snapshot capture, General
//! metadata merge, category sub-flows, project-meta review, user-profile
//! rewrite (including note absorption), manifest repair, no-op accounting and
//! the final `DreamTraceRecord`.
//!
//! This module owns no new storage or LLM code. It composes the already-ported
//! [`super::runner`] sub-flows and [`super::helpers`] pure helpers, keeping the
//! same camelCase [`JsObject`] shapes on the trace boundary.

use super::{
    helpers::{
        create_dream_trace, is_dream_user_note_path, is_dream_user_profile_path, mutation,
        push_step, select_user_note_window, sort_entries, sort_project_metas,
    },
    runner::{
        CategoryDreamResult, DreamRewriteRunner, DreamRunnerError, concat, json_detail, kv_detail,
        list_detail, num, push_mutation, step_title, string_field, type_is, value_array,
    },
};
use crate::runtime::memory::{
    edgeclaw::{file_memory::FileMemoryStore, storage::js_truthy, text::hash_text},
    js_trim_units,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};

type Result<T> = std::result::Result<T, DreamRunnerError>;

/// `DreamExecutionResult` (`dream-review.ts:56-61`): the staged runner result,
/// including the full trace and the TS no-op flag.
#[derive(Debug, Clone, PartialEq)]
pub struct DreamExecutionResult {
    pub reviewed_files: f64,
    pub rewritten_projects: f64,
    pub deleted_projects: f64,
    pub deleted_files: f64,
    pub profile_updated: bool,
    pub duplicate_topic_count: f64,
    pub conflict_topic_count: f64,
    pub summary: JsString,
    pub finished_at: JsString,
    pub is_no_op: bool,
    pub trace: JsObject,
}

const DEFAULT_SUMMARY: &str = "Dream completed without requiring any memory changes.";
const NO_MEMORY_SUMMARY: &str =
    "No file-based memory exists yet, so Dream had nothing to organize.";
const FALLBACK_PROFILE_PATH: &str = "global/UserIdentity/user-profile.md";

impl<'a> DreamRewriteRunner<'a> {
    /// `run(trigger = "manual")` (`dream-review.ts:1026-1492`).
    ///
    /// Store and extractor failures propagate exactly like the TS `await`s;
    /// the two user-profile failure shapes TS catches become trace warnings
    /// instead (`Dream skipped user profile rewrite ...`).
    pub async fn run(&self, trigger: &JsString) -> Result<DreamExecutionResult> {
        // `createDreamTrace(trigger)` reads `nowIso()` then `Math.random()`.
        let started_at = self.environment.now_iso();
        let suffix = self.environment.random_suffix();
        let mut trace = create_dream_trace(trigger, &started_at, &suffix);

        let workspace_store = self.repository.get_file_memory_store();
        let global_store = self.repository.get_global_user_store();
        let user_profile_relative_path = global_store
            .get_user_profile_relative_path()
            .map(JsString::from);

        let workspace_entries_value = self.repository.list_memory_entries(&entry_options(
            &["project", "feedback"],
            Some("project"),
            None,
        ))?;
        let workspace_entries = objects(&workspace_entries_value);
        let project_entries: Vec<JsObject> = workspace_entries
            .iter()
            .filter(|entry| type_is(&JsValue::Object((*entry).clone()), "project"))
            .cloned()
            .collect();
        let feedback_entries: Vec<JsObject> = workspace_entries
            .iter()
            .filter(|entry| type_is(&JsValue::Object((*entry).clone()), "feedback"))
            .cloned()
            .collect();

        let is_general_mode = workspace_store.is_general_mode();
        let project_meta = if is_general_mode {
            None
        } else {
            let meta = workspace_store.get_project_meta(&JsObject::new())?;
            Some(match meta.as_object() {
                Some(meta) => meta.clone(),
                None => workspace_store
                    .ensure_project_meta(&JsObject::new())?
                    .as_object()
                    .cloned()
                    .unwrap_or_default(),
            })
        };
        let general_project_metas = if is_general_mode {
            sort_project_metas(&workspace_store.list_project_metas()?)?
        } else {
            Vec::new()
        };

        let user_summary = self.repository.get_user_summary()?;
        let user_summary_files = array_field(&user_summary, "files");

        let raw_global_user_entries =
            objects(&self.repository.list_memory_entries(&entry_options(
                &["user"],
                Some("global"),
                None,
            ))?);
        let mut skipped_protected_user_entries = Vec::new();
        for entry in &raw_global_user_entries {
            let path = string_field(entry, "relativePath").unwrap_or_default();
            if is_dream_user_profile_path(&path) || !is_dream_user_note_path(&path) {
                skipped_protected_user_entries.push(concat(&[
                    &path,
                    &" | ".into(),
                    &string_field(entry, "updatedAt").unwrap_or_default(),
                ]));
            }
        }
        let user_note_entries: Vec<JsObject> = raw_global_user_entries
            .iter()
            .filter(|entry| {
                let path = string_field(entry, "relativePath").unwrap_or_default();
                is_dream_user_note_path(&path) && !is_dream_user_profile_path(&path)
            })
            .cloned()
            .collect();

        let workspace_records = if workspace_entries.is_empty() {
            Vec::new()
        } else {
            records_by_ids(self, workspace_entries.iter(), 5000.0)?
        };
        let user_note_records = if user_note_entries.is_empty() {
            Vec::new()
        } else {
            records_by_ids(self, user_note_entries.iter(), 5000.0)?
        };

        let mut snapshot = JsObject::new();
        snapshot.insert(
            "projectMetaPresent".into(),
            JsValue::Bool(if is_general_mode {
                !general_project_metas.is_empty()
            } else {
                project_meta.is_some()
            }),
        );
        snapshot.insert(
            "projectFileCount".into(),
            JsValue::Number(project_entries.len() as f64),
        );
        snapshot.insert(
            "feedbackFileCount".into(),
            JsValue::Number(feedback_entries.len() as f64),
        );
        snapshot.insert(
            "hasUserProfile".into(),
            JsValue::Bool(!user_summary_files.is_empty()),
        );
        trace.insert("snapshotSummary".into(), JsValue::Object(snapshot));

        let title = || step_title(&"trace.step.dream_start".into(), &"Dream Start".into());
        let mut options = JsObject::new();
        options.insert("titleI18n".into(), title()?);
        push_step(
            &mut trace,
            &"dream_start".into(),
            &"Dream Start".into(),
            &"info".into(),
            &concat(&[trigger, &" dream run started.".into()]),
            &"Dream loaded the current memory snapshot and began staged refinement.".into(),
            &options,
        );

        let has_snapshot = !workspace_entries.is_empty()
            || !user_summary_files.is_empty()
            || !user_note_records.is_empty()
            || !general_project_metas.is_empty();
        let mut details = vec![JsValue::Object(with_i18n(
            kv_detail(
                &"snapshot-summary".into(),
                &"Snapshot Summary".into(),
                &snapshot_summary_entries(
                    is_general_mode,
                    if is_general_mode {
                        !general_project_metas.is_empty()
                    } else {
                        project_meta.is_some()
                    },
                    general_project_metas.len(),
                    project_entries.len(),
                    feedback_entries.len(),
                    !user_summary_files.is_empty(),
                    user_note_records.len(),
                ),
            ),
            Some(step_title(
                &"trace.detail.snapshot_summary".into(),
                &"Snapshot Summary".into(),
            )?),
        ))];
        if !is_general_mode {
            details.push(JsValue::Object(with_i18n(
                json_detail(
                    &"project-meta".into(),
                    &"Project Meta".into(),
                    project_meta
                        .as_ref()
                        .map(|meta| JsValue::Object(meta.clone()))
                        .unwrap_or(JsValue::Undefined),
                ),
                Some(step_title(
                    &"trace.detail.project_meta".into(),
                    &"Project Meta".into(),
                )?),
            )));
        }
        if is_general_mode && !general_project_metas.is_empty() {
            details.push(JsValue::Object(json_detail(
                &"general-project-meta".into(),
                &"General Project Meta".into(),
                JsValue::Array(
                    general_project_metas
                        .iter()
                        .map(|meta| {
                            let mut item = JsObject::new();
                            for key in ["projectId", "projectName"] {
                                item.insert(
                                    key.into(),
                                    meta.get(key).cloned().unwrap_or(JsValue::Undefined),
                                );
                            }
                            item.insert(
                                "sourceKind".into(),
                                meta.get("sourceKind")
                                    .cloned()
                                    .filter(|value| {
                                        !value.is_undefined() && !matches!(value, JsValue::Null)
                                    })
                                    .unwrap_or(JsValue::String("general_local".into())),
                            );
                            for key in ["sourceWorkspacePath", "sourceProjectId"] {
                                item.insert(
                                    key.into(),
                                    meta.get(key).cloned().unwrap_or(JsValue::Null),
                                );
                            }
                            item.insert(
                                "updatedAt".into(),
                                meta.get("updatedAt").cloned().unwrap_or(JsValue::Undefined),
                            );
                            JsValue::Object(item)
                        })
                        .collect(),
                ),
            )));
        }
        if !workspace_entries.is_empty() {
            let sorted = sort_entries(&JsValue::Array(
                workspace_entries
                    .iter()
                    .cloned()
                    .map(JsValue::Object)
                    .collect(),
            ))?;
            details.push(JsValue::Object(with_i18n(
                list_detail(
                    &"snapshot-files".into(),
                    &"Current Project Files".into(),
                    sorted
                        .iter()
                        .map(|entry| {
                            JsValue::String(concat(&[
                                &string_field(entry, "relativePath").unwrap_or_default(),
                                &" | ".into(),
                                &string_field(entry, "updatedAt").unwrap_or_default(),
                            ]))
                        })
                        .collect(),
                ),
                Some(step_title(
                    &"trace.detail.loaded_files".into(),
                    &"Loaded Files".into(),
                )?),
            )));
        }
        if !user_note_entries.is_empty() {
            let sorted = sort_entries(&JsValue::Array(
                user_note_entries
                    .iter()
                    .cloned()
                    .map(JsValue::Object)
                    .collect(),
            ))?;
            details.push(JsValue::Object(list_detail(
                &"snapshot-user-notes".into(),
                &"User Notes".into(),
                sorted
                    .iter()
                    .map(|entry| {
                        JsValue::String(concat(&[
                            &string_field(entry, "relativePath").unwrap_or_default(),
                            &" | ".into(),
                            &string_field(entry, "updatedAt").unwrap_or_default(),
                        ]))
                    })
                    .collect(),
            )));
        }
        if !skipped_protected_user_entries.is_empty() {
            details.push(JsValue::Object(list_detail(
                &"snapshot-protected-user-paths".into(),
                &"Protected User Paths Skipped".into(),
                skipped_protected_user_entries
                    .iter()
                    .map(|item| JsValue::String(item.clone()))
                    .collect(),
            )));
        }
        let mut options = JsObject::new();
        options.insert(
            "titleI18n".into(),
            step_title(
                &"trace.step.snapshot_loaded".into(),
                &"Snapshot Loaded".into(),
            )?,
        );
        options.insert("details".into(), JsValue::Array(details));
        push_step(
            &mut trace,
            &"snapshot_loaded".into(),
            &"Snapshot Loaded".into(),
            &(if has_snapshot {
                "success".into()
            } else {
                "warning".into()
            }),
            &concat(&[
                &num(workspace_entries.len() as f64),
                &" project files, ".into(),
                &num(user_note_records.len() as f64),
                &" user notes".into(),
            ]),
            &(if has_snapshot {
                "Current project memory is ready for staged Dream processing.".into()
            } else {
                "No file-based memory exists yet, so Dream had nothing to organize.".into()
            }),
            &options,
        );

        if workspace_entries.is_empty()
            && user_summary_files.is_empty()
            && user_note_records.is_empty()
            && (!is_general_mode || general_project_metas.is_empty())
        {
            let finished_at = self.environment.now_iso();
            let summary = JsString::from(NO_MEMORY_SUMMARY);
            trace.insert("finishedAt".into(), JsValue::String(finished_at.clone()));
            trace.insert("status".into(), "completed".into());
            trace.insert("isNoOp".into(), JsValue::Bool(true));
            trace.insert("displayStatus".into(), "No-op".into());
            trace.insert(
                "outcome".into(),
                JsValue::Object(outcome(0.0, 0.0, 0.0, false, &summary)),
            );
            let mut options = JsObject::new();
            options.insert(
                "titleI18n".into(),
                step_title(
                    &"trace.step.dream_finished".into(),
                    &"Dream Finished".into(),
                )?,
            );
            push_step(
                &mut trace,
                &"dream_finished".into(),
                &"Dream Finished".into(),
                &"success".into(),
                &(if is_general_mode {
                    "No memory files or General project nodes".into()
                } else {
                    "No memory files".into()
                }),
                &summary,
                &options,
            );
            return Ok(DreamExecutionResult {
                reviewed_files: 0.0,
                rewritten_projects: 0.0,
                deleted_projects: 0.0,
                deleted_files: 0.0,
                profile_updated: false,
                duplicate_topic_count: 0.0,
                conflict_topic_count: 0.0,
                summary,
                finished_at,
                is_no_op: true,
                trace,
            });
        }

        let mut project_dream = empty_category();
        let mut feedback_dream = empty_category();
        let mut meta_updated_count = 0.0;
        let mut deleted_projects = 0.0;

        if is_general_mode {
            let merge = self.merge_general_project_metas(&mut trace).await?;
            deleted_projects = merge.deleted_projects;
            let current_project_metas = sort_project_metas(&workspace_store.list_project_metas()?)?;
            for current_meta in &current_project_metas {
                let current_entries_value = self.repository.list_memory_entries(&entry_options(
                    &["project", "feedback"],
                    Some("project"),
                    string_field(current_meta, "projectId"),
                ))?;
                let current_entries = objects(&current_entries_value);
                let current_project_entries: Vec<JsObject> = current_entries
                    .iter()
                    .filter(|entry| type_is(&JsValue::Object((*entry).clone()), "project"))
                    .cloned()
                    .collect();
                let current_feedback_entries: Vec<JsObject> = current_entries
                    .iter()
                    .filter(|entry| type_is(&JsValue::Object((*entry).clone()), "feedback"))
                    .cloned()
                    .collect();
                let current_records = if current_entries.is_empty() {
                    Vec::new()
                } else {
                    records_by_ids(self, current_entries.iter(), 5000.0)?
                };

                let next_project = self
                    .run_category_dream(
                        &mut trace,
                        &"project".into(),
                        &current_project_entries,
                        &current_records,
                        Some(current_meta),
                    )
                    .await?;
                let next_feedback = self
                    .run_category_dream(
                        &mut trace,
                        &"feedback".into(),
                        &current_feedback_entries,
                        &current_records,
                        Some(current_meta),
                    )
                    .await?;
                merge_category(&mut project_dream, &next_project);
                merge_category(&mut feedback_dream, &next_feedback);

                let review = self
                    .run_project_meta_review(&mut trace, current_meta)
                    .await?;
                if review.updated {
                    meta_updated_count += 1.0;
                }
            }
        } else {
            project_dream = self
                .run_category_dream(
                    &mut trace,
                    &"project".into(),
                    &project_entries,
                    &workspace_records,
                    None,
                )
                .await?;
            feedback_dream = self
                .run_category_dream(
                    &mut trace,
                    &"feedback".into(),
                    &feedback_entries,
                    &workspace_records,
                    None,
                )
                .await?;
            let Some(project_meta) = project_meta.as_ref() else {
                return Err(DreamRunnerError::Memory(
                    crate::runtime::memory::edgeclaw::file_memory::FileMemoryError::Invalid(
                        "project metadata is required outside General mode".into(),
                    ),
                ));
            };
            let review = self
                .run_project_meta_review(&mut trace, project_meta)
                .await?;
            if review.updated {
                meta_updated_count += 1.0;
            }
        }

        let window = select_user_note_window(&JsValue::Array(
            user_note_records
                .iter()
                .cloned()
                .map(JsValue::Object)
                .collect(),
        ))?;
        let selected_records = objects(
            window
                .get("selectedRecords")
                .unwrap_or(&JsValue::Array(vec![])),
        );
        let kept_records = objects(window.get("keptRecords").unwrap_or(&JsValue::Array(vec![])));
        let selected_chars = window
            .get("selectedChars")
            .and_then(JsValue::as_number)
            .unwrap_or(0.0);
        let selected_candidates: Vec<JsObject> = selected_records
            .iter()
            .map(FileMemoryStore::to_candidate)
            .collect();

        let mut user_profile_updated = false;
        let mut user_rewrite_debug: Option<JsValue> = None;
        let mut absorbed_user_note_ids: Vec<JsString> = Vec::new();
        let mut protected_user_paths_skipped: Vec<JsString> = Vec::new();
        let mut user_rewrite_failure_message: Option<JsString> = None;
        let mut user_rewrite_kept_notes_message: Option<JsString> = None;

        if !selected_candidates.is_empty() {
            let mut input = JsObject::new();
            input.insert("existingProfile".into(), user_summary.clone());
            input.insert(
                "candidates".into(),
                JsValue::Array(
                    selected_candidates
                        .iter()
                        .cloned()
                        .map(JsValue::Object)
                        .collect(),
                ),
            );
            let extracted = self
                .extractor
                .rewrite_user_profile(&input, &mut |event| user_rewrite_debug = Some(event))
                .await;
            match extracted {
                Ok(Some(rewritten)) => {
                    let body = rewritten
                        .get("body")
                        .and_then(JsValue::as_string)
                        .map(|body| JsString::from_utf16(js_trim_units(body.units()).to_vec()))
                        .unwrap_or_default();
                    if !body.is_empty() {
                        let previous_profile_body = user_summary_files
                            .first()
                            .and_then(|file| file.get("content"))
                            .and_then(JsValue::as_string)
                            .map(|content| {
                                JsString::from_utf16(js_trim_units(content.units()).to_vec())
                            })
                            .unwrap_or_default();
                        if previous_profile_body != body {
                            global_store.upsert_user_profile(&rewritten)?;
                            user_profile_updated = true;

                            let seed_path = user_profile_relative_path
                                .clone()
                                .unwrap_or_else(|| "undefined".into());
                            let now = num(self.environment.now_ms());
                            let seed = concat(&[
                                &"rewrite_user_profile:".into(),
                                &seed_path,
                                &":".into(),
                                &now,
                            ]);
                            let mut rewrite_mutation = JsObject::new();
                            rewrite_mutation.insert(
                                "mutationId".into(),
                                JsValue::String(JsString::from(format!(
                                    "mutation_{}",
                                    hash_text(&seed)
                                ))),
                            );
                            rewrite_mutation.insert(
                                "action".into(),
                                JsValue::String("rewrite_user_profile".into()),
                            );
                            rewrite_mutation.insert(
                                "relativePath".into(),
                                JsValue::String(
                                    user_profile_relative_path
                                        .clone()
                                        .unwrap_or_else(|| FALLBACK_PROFILE_PATH.into()),
                                ),
                            );
                            let mut mutations = match trace.get("mutations") {
                                Some(JsValue::Array(items)) => items.clone(),
                                _ => Vec::new(),
                            };
                            mutations.push(JsValue::Object(rewrite_mutation));
                            trace.insert("mutations".into(), JsValue::Array(mutations));

                            let requested: Vec<JsString> = selected_records
                                .iter()
                                .filter_map(|record| string_field(record, "relativePath"))
                                .collect();
                            absorbed_user_note_ids = requested
                                .iter()
                                .filter(|path| {
                                    is_dream_user_note_path(path)
                                        && !is_dream_user_profile_path(path)
                                })
                                .cloned()
                                .collect();
                            protected_user_paths_skipped = requested
                                .iter()
                                .filter(|path| {
                                    !absorbed_user_note_ids
                                        .iter()
                                        .any(|absorbed| absorbed == *path)
                                })
                                .cloned()
                                .collect();
                            if !absorbed_user_note_ids.is_empty() {
                                self.repository.delete_memory_entries(&JsValue::Array(
                                    absorbed_user_note_ids
                                        .iter()
                                        .cloned()
                                        .map(JsValue::String)
                                        .collect(),
                                ))?;
                                for path in &absorbed_user_note_ids {
                                    push_mutation(
                                        &mut trace,
                                        mutation(
                                            &"delete".into(),
                                            path,
                                            0.0,
                                            0.0,
                                            &JsObject::new(),
                                        ),
                                        Some(self.environment),
                                    );
                                }
                            }
                        } else {
                            user_rewrite_kept_notes_message = Some(
                                "Dream kept the selected user notes because the rewritten profile matched the existing profile."
                                    .into(),
                            );
                        }
                    } else {
                        user_rewrite_failure_message = Some(
                            "Dream skipped user profile rewrite because the model returned no valid rewritten profile."
                                .into(),
                        );
                    }
                }
                Ok(None) => {
                    user_rewrite_failure_message = Some(
                        "Dream skipped user profile rewrite because the model returned no valid rewritten profile."
                            .into(),
                    );
                }
                Err(error) => {
                    let error = DreamRunnerError::Llm(error);
                    user_rewrite_failure_message = Some(concat(&[
                        &"Dream skipped user profile rewrite because the rewrite request failed: "
                            .into(),
                        &error.js_message(),
                    ]));
                    if let Some(logger) = self.logger {
                        logger.warn(&concat(&[
                            &"[clawxmemory] staged dream user-profile rewrite failed: ".into(),
                            &error.js_string(),
                        ]));
                    }
                }
            }
        }

        let rewritten_status = if user_profile_updated || !absorbed_user_note_ids.is_empty() {
            "success".into()
        } else if !selected_candidates.is_empty() {
            "warning".into()
        } else {
            "skipped".into()
        };
        let rewritten_output = if user_profile_updated {
            concat(&[
                &"Dream updated the global user profile and absorbed ".into(),
                &num(absorbed_user_note_ids.len() as f64),
                &" user notes.".into(),
            ])
        } else if !absorbed_user_note_ids.is_empty() {
            concat(&[
                &"Dream absorbed ".into(),
                &num(absorbed_user_note_ids.len() as f64),
                &" user notes without changing the current profile summary.".into(),
            ])
        } else if !selected_candidates.is_empty() {
            user_rewrite_failure_message
                .clone()
                .or_else(|| user_rewrite_kept_notes_message.clone())
                .unwrap_or_else(|| {
                    "Dream could not absorb the selected user notes into the global profile.".into()
                })
        } else {
            "Dream found no user notes within the current processing window.".into()
        };
        let mut details = vec![JsValue::Object(kv_detail(
            &"user-rewrite-summary".into(),
            &"User Rewrite Summary".into(),
            &[
                (
                    "existingProfileFiles".into(),
                    JsValue::Number(user_summary_files.len() as f64),
                ),
                (
                    "allUserNotes".into(),
                    JsValue::Number(user_note_records.len() as f64),
                ),
                (
                    "selectedUserNotes".into(),
                    JsValue::Number(selected_records.len() as f64),
                ),
                ("selectedChars".into(), JsValue::Number(selected_chars)),
                (
                    "keptUserNotes".into(),
                    JsValue::Number(kept_records.len() as f64),
                ),
                (
                    "profileUpdated".into(),
                    JsValue::String(if user_profile_updated {
                        "yes".into()
                    } else {
                        "no".into()
                    }),
                ),
                (
                    "notesAbsorbed".into(),
                    JsValue::Number(absorbed_user_note_ids.len() as f64),
                ),
            ],
        ))];
        if !selected_records.is_empty() {
            details.push(JsValue::Object(list_detail(
                &"selected-user-notes".into(),
                &"Selected User Notes".into(),
                selected_records
                    .iter()
                    .map(|record| {
                        JsValue::String(concat(&[
                            &string_field(record, "relativePath").unwrap_or_default(),
                            &" | ".into(),
                            &string_field(record, "updatedAt").unwrap_or_default(),
                        ]))
                    })
                    .collect(),
            )));
        }
        if !kept_records.is_empty() {
            details.push(JsValue::Object(list_detail(
                &"kept-user-notes".into(),
                &"Kept User Notes".into(),
                kept_records
                    .iter()
                    .map(|record| {
                        JsValue::String(concat(&[
                            &string_field(record, "relativePath").unwrap_or_default(),
                            &" | ".into(),
                            &string_field(record, "updatedAt").unwrap_or_default(),
                        ]))
                    })
                    .collect(),
            )));
        }
        if !protected_user_paths_skipped.is_empty() {
            details.push(JsValue::Object(list_detail(
                &"protected-user-paths-skipped".into(),
                &"Protected User Paths Skipped".into(),
                protected_user_paths_skipped
                    .iter()
                    .cloned()
                    .map(JsValue::String)
                    .collect(),
            )));
        }
        if let Some(message) = &user_rewrite_failure_message {
            details.push(JsValue::Object(kv_detail(
                &"user-rewrite-warning".into(),
                &"User Rewrite Warning".into(),
                &[("message".into(), JsValue::String(message.clone()))],
            )));
        }
        if let Some(message) = &user_rewrite_kept_notes_message {
            details.push(JsValue::Object(kv_detail(
                &"user-rewrite-kept-notes".into(),
                &"User Notes Kept".into(),
                &[("message".into(), JsValue::String(message.clone()))],
            )));
        }
        let mut options = JsObject::new();
        options.insert(
            "titleI18n".into(),
            step_title(
                &"trace.step.user_profile_rewritten".into(),
                &"User Profile Rewritten".into(),
            )?,
        );
        options.insert("details".into(), JsValue::Array(details));
        if let Some(debug) = user_rewrite_debug.filter(js_truthy) {
            options.insert("promptDebug".into(), debug);
        }
        push_step(
            &mut trace,
            &"user_profile_rewritten".into(),
            &"User Profile Rewritten".into(),
            &rewritten_status,
            &concat(&[
                &num(user_summary_files.len() as f64),
                &" profile files, ".into(),
                &num(user_note_records.len() as f64),
                &" user notes".into(),
            ]),
            &rewritten_output,
            &options,
        );

        let repaired = workspace_store.repair_manifests()?;
        let repaired_summary = repaired
            .get("summary")
            .and_then(JsValue::as_string)
            .cloned()
            .unwrap_or_default();
        let mut options = JsObject::new();
        options.insert(
            "titleI18n".into(),
            step_title(
                &"trace.step.manifests_repaired".into(),
                &"Manifests Repaired".into(),
            )?,
        );
        options.insert(
            "details".into(),
            JsValue::Array(vec![JsValue::Object(with_i18n(
                kv_detail(
                    &"manifest-repair".into(),
                    &"Manifest Repair".into(),
                    &[
                        (
                            "changed".into(),
                            repaired
                                .get("changed")
                                .cloned()
                                .unwrap_or(JsValue::Undefined),
                        ),
                        (
                            "memoryFileCount".into(),
                            repaired
                                .get("memoryFileCount")
                                .cloned()
                                .unwrap_or(JsValue::Undefined),
                        ),
                    ],
                ),
                Some(step_title(
                    &"trace.detail.manifest_scan".into(),
                    &"Manifest Scan".into(),
                )?),
            ))]),
        );
        push_step(
            &mut trace,
            &"manifests_repaired".into(),
            &"Manifests Repaired".into(),
            &"success".into(),
            &"workspace manifest rebuild".into(),
            &repaired_summary,
            &options,
        );

        let finished_at = self.environment.now_iso();
        let deleted_files = project_dream.deleted_files
            + feedback_dream.deleted_files
            + absorbed_user_note_ids.len() as f64;
        let rewritten_projects = project_dream.refined_clusters + feedback_dream.refined_clusters;
        let conflict_topic_count = project_dream.dropped_warnings.len() as f64
            + feedback_dream.dropped_warnings.len() as f64;
        let duplicate_topic_count =
            project_dream.planned_clusters + feedback_dream.planned_clusters;
        let meta_updated = meta_updated_count > 0.0;
        let mut parts: Vec<JsString> = Vec::new();
        if deleted_projects > 0.0 {
            parts.push(concat(&[
                &"Merged ".into(),
                &num(deleted_projects),
                &" duplicate General project nodes.".into(),
            ]));
        }
        if rewritten_projects > 0.0 {
            parts.push(concat(&[
                &"Refined ".into(),
                &num(rewritten_projects),
                &" dream clusters.".into(),
            ]));
        }
        if deleted_files > 0.0 {
            parts.push(concat(&[
                &"Deleted ".into(),
                &num(deleted_files),
                &" absorbed source files.".into(),
            ]));
        }
        if user_profile_updated {
            parts.push("Updated the global user profile.".into());
        }
        if meta_updated {
            parts.push(concat(&[
                &"Reviewed and updated ".into(),
                &num(meta_updated_count),
                &" project metadata records.".into(),
            ]));
        }
        let summary = if parts.is_empty() {
            JsString::from(DEFAULT_SUMMARY)
        } else {
            let mut units = Vec::new();
            for (index, part) in parts.iter().enumerate() {
                if index > 0 {
                    units.push(u16::from(b' '));
                }
                units.extend(part.units());
            }
            JsString::from_utf16(units)
        };
        let is_no_op = rewritten_projects == 0.0
            && deleted_files == 0.0
            && deleted_projects == 0.0
            && !user_profile_updated
            && !meta_updated;

        trace.insert("finishedAt".into(), JsValue::String(finished_at.clone()));
        trace.insert("status".into(), "completed".into());
        trace.insert("isNoOp".into(), JsValue::Bool(is_no_op));
        trace.insert(
            "displayStatus".into(),
            if is_no_op {
                "No-op".into()
            } else {
                "Completed".into()
            },
        );
        trace.insert(
            "outcome".into(),
            JsValue::Object(outcome(
                rewritten_projects,
                deleted_projects,
                deleted_files,
                user_profile_updated,
                &summary,
            )),
        );

        let mut category_summary = JsObject::new();
        category_summary.insert(
            "projectDream".into(),
            JsValue::Object(category_json(&project_dream)),
        );
        category_summary.insert(
            "feedbackDream".into(),
            JsValue::Object(category_json(&feedback_dream)),
        );
        category_summary.insert(
            "absorbedUserNoteIds".into(),
            JsValue::Array(
                absorbed_user_note_ids
                    .iter()
                    .cloned()
                    .map(JsValue::String)
                    .collect(),
            ),
        );
        category_summary.insert("deletedProjects".into(), JsValue::Number(deleted_projects));
        category_summary.insert(
            "metaUpdatedCount".into(),
            JsValue::Number(meta_updated_count),
        );
        let mut options = JsObject::new();
        options.insert(
            "titleI18n".into(),
            step_title(
                &"trace.step.dream_finished".into(),
                &"Dream Finished".into(),
            )?,
        );
        options.insert(
            "details".into(),
            JsValue::Array(vec![
                JsValue::Object(kv_detail(
                    &"dream-outcome".into(),
                    &"Dream Outcome".into(),
                    &[
                        (
                            "rewrittenClusters".into(),
                            JsValue::Number(rewritten_projects),
                        ),
                        ("deletedProjects".into(), JsValue::Number(deleted_projects)),
                        ("deletedFiles".into(), JsValue::Number(deleted_files)),
                        (
                            "profileUpdated".into(),
                            JsValue::String(if user_profile_updated {
                                "yes".into()
                            } else {
                                "no".into()
                            }),
                        ),
                        (
                            "metaUpdated".into(),
                            JsValue::String(if meta_updated {
                                "yes".into()
                            } else {
                                "no".into()
                            }),
                        ),
                        (
                            "updatedProjectMetas".into(),
                            JsValue::Number(meta_updated_count),
                        ),
                        (
                            "noOp".into(),
                            JsValue::String(if is_no_op { "yes".into() } else { "no".into() }),
                        ),
                    ],
                )),
                JsValue::Object(json_detail(
                    &"dream-category-summary".into(),
                    &"Dream Category Summary".into(),
                    JsValue::Object(category_summary),
                )),
            ]),
        );
        push_step(
            &mut trace,
            &"dream_finished".into(),
            &"Dream Finished".into(),
            &(if is_no_op {
                "warning".into()
            } else {
                "success".into()
            }),
            &concat(&[
                &num(workspace_entries.len() as f64),
                &" project files, ".into(),
                &num(user_note_records.len() as f64),
                &" user notes".into(),
            ]),
            &summary,
            &options,
        );

        Ok(DreamExecutionResult {
            reviewed_files: workspace_entries.len() as f64
                + user_note_records.len() as f64
                + general_project_metas.len() as f64,
            rewritten_projects,
            deleted_projects,
            deleted_files,
            profile_updated: user_profile_updated,
            duplicate_topic_count,
            conflict_topic_count,
            summary,
            finished_at,
            is_no_op,
            trace,
        })
    }
}

// -- local shape helpers ----------------------------------------------------

fn entry_options(kinds: &[&str], scope: Option<&str>, project_id: Option<JsString>) -> JsObject {
    let mut options = JsObject::new();
    options.insert(
        "kinds".into(),
        JsValue::Array(
            kinds
                .iter()
                .map(|kind| JsValue::String((*kind).into()))
                .collect(),
        ),
    );
    if let Some(scope) = scope {
        options.insert("scope".into(), JsValue::String(scope.into()));
    }
    if let Some(project_id) = project_id {
        options.insert("projectId".into(), JsValue::String(project_id));
    }
    options.insert("includeDeprecated".into(), JsValue::Bool(false));
    options.insert("limit".into(), JsValue::Number(5000.0));
    options
}

fn records_by_ids<'b>(
    runner: &DreamRewriteRunner<'_>,
    entries: impl Iterator<Item = &'b JsObject>,
    max_lines: f64,
) -> Result<Vec<JsObject>> {
    let ids: Vec<JsValue> = entries
        .filter_map(|entry| string_field(entry, "relativePath"))
        .map(JsValue::String)
        .collect();
    let value = runner
        .repository
        .get_memory_records_by_ids(&JsValue::Array(ids), max_lines)?;
    Ok(objects(&value))
}

fn objects(value: &JsValue) -> Vec<JsObject> {
    match value {
        JsValue::Array(items) => items
            .iter()
            .filter_map(JsValue::as_object)
            .cloned()
            .collect(),
        _ => Vec::new(),
    }
}

fn array_field(object: &JsValue, key: &str) -> Vec<JsObject> {
    object
        .get(key)
        .and_then(value_array)
        .map(|items| {
            items
                .iter()
                .filter_map(JsValue::as_object)
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

fn with_i18n(mut detail: JsObject, i18n: Option<JsValue>) -> JsObject {
    if let Some(i18n) = i18n {
        detail.insert("labelI18n".into(), i18n);
    }
    detail
}

fn snapshot_summary_entries(
    is_general_mode: bool,
    project_meta_present: bool,
    general_projects: usize,
    project_files: usize,
    feedback_files: usize,
    has_user_profile: bool,
    user_notes: usize,
) -> Vec<(JsString, JsValue)> {
    let mut entries = vec![(
        "projectMetaPresent".into(),
        JsValue::String(if project_meta_present {
            "yes".into()
        } else {
            "no".into()
        }),
    )];
    if is_general_mode {
        entries.push((
            "generalProjects".into(),
            JsValue::Number(general_projects as f64),
        ));
    }
    entries.push(("projectFiles".into(), JsValue::Number(project_files as f64)));
    entries.push((
        "feedbackFiles".into(),
        JsValue::Number(feedback_files as f64),
    ));
    entries.push((
        "hasUserProfile".into(),
        JsValue::String(if has_user_profile {
            "yes".into()
        } else {
            "no".into()
        }),
    ));
    entries.push(("userNotes".into(), JsValue::Number(user_notes as f64)));
    entries
}

fn empty_category() -> CategoryDreamResult {
    CategoryDreamResult {
        planned_clusters: 0.0,
        refined_clusters: 0.0,
        deleted_files: 0.0,
        dropped_warnings: vec![],
    }
}

fn merge_category(target: &mut CategoryDreamResult, next: &CategoryDreamResult) {
    target.planned_clusters += next.planned_clusters;
    target.refined_clusters += next.refined_clusters;
    target.deleted_files += next.deleted_files;
    target
        .dropped_warnings
        .extend(next.dropped_warnings.iter().cloned());
}

fn category_json(result: &CategoryDreamResult) -> JsObject {
    let mut out = JsObject::new();
    out.insert(
        "plannedClusters".into(),
        JsValue::Number(result.planned_clusters),
    );
    out.insert(
        "refinedClusters".into(),
        JsValue::Number(result.refined_clusters),
    );
    out.insert("deletedFiles".into(), JsValue::Number(result.deleted_files));
    out.insert(
        "droppedWarnings".into(),
        JsValue::Array(result.dropped_warnings.clone()),
    );
    out
}

fn outcome(
    rewritten_projects: f64,
    deleted_projects: f64,
    deleted_files: f64,
    profile_updated: bool,
    summary: &JsString,
) -> JsObject {
    let mut out = JsObject::new();
    out.insert(
        "rewrittenProjects".into(),
        JsValue::Number(rewritten_projects),
    );
    out.insert("deletedProjects".into(), JsValue::Number(deleted_projects));
    out.insert("deletedFiles".into(), JsValue::Number(deleted_files));
    out.insert("profileUpdated".into(), JsValue::Bool(profile_updated));
    out.insert("summary".into(), JsValue::String(summary.clone()));
    out
}
