//! `DreamRewriteRunner` sub-flows (`dream-review.ts:532-1025`).
//!
//! Three single-threaded, staged operations over one repository/store handle:
//! [`DreamRewriteRunner::run_category_dream`],
//! [`DreamRewriteRunner::merge_general_project_metas`] and
//! [`DreamRewriteRunner::run_project_meta_review`]. The surrounding `run()`
//! orchestration (snapshot, user-profile rewrite, final trace) belongs to the
//! later `dream/run.rs` package.
//!
//! All persisted shapes stay the shared camelCase [`JsObject`] codec, exactly
//! like the helpers in [`super::helpers`]. Nondeterminism is injected through
//! [`DreamEnvironment`] (`Date.now()`, `Math.random()`, `nowIso()`); the LLM
//! surface is [`DreamExtractor`], implemented for the real
//! [`LlmMemoryExtractor`] and scriptable from tests.

use super::helpers::{
    mutation, push_step, sort_entries, sort_project_metas, to_dream_meta_input,
    to_dream_record_input, to_header_input, to_refined_candidate, validate_exclusive_clusters,
    validate_general_project_merge_groups,
};
use crate::{
    js_value::{number_string, to_string_units},
    project_files::{node_collator, node_compare_utf16, node_locale},
    runtime::memory::edgeclaw::{
        EdgeClawFileStores,
        file_memory::FileMemoryError,
        llm::transport::{LlmError, LlmMemoryExtractor},
        storage::js_truthy,
        text::{hash_text, trace_i18n},
    },
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{
    collections::{HashMap, HashSet},
    fmt,
    future::Future,
    pin::Pin,
};

const DREAM_HEADER_SCAN_LIMIT: usize = 200;
const DREAM_CLUSTER_MAX_FILES: f64 = 8.0;
const DREAM_META_PROJECT_CONTEXT_LIMIT: usize = 5;
const DREAM_META_FEEDBACK_CONTEXT_LIMIT: usize = 5;
type Result<T> = std::result::Result<T, DreamRunnerError>;
type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Union of the store/LLM failures the three sub-flows can surface. `merge`
/// catches both because TS wraps planning *and* validation in one `try`.
#[derive(Debug)]
pub enum DreamRunnerError {
    Memory(FileMemoryError),
    Llm(LlmError),
}
impl fmt::Display for DreamRunnerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Memory(error) => write!(f, "{error}"),
            Self::Llm(error) => write!(f, "{error}"),
        }
    }
}
impl std::error::Error for DreamRunnerError {}
impl DreamRunnerError {
    /// `String(error)` as Node renders an `Error`: `<name>: <message>`.
    /// `planSummary` uses `error.message` instead (`dream-review.ts:836-838`).
    fn js_string(&self) -> String {
        let (name, message) = match self {
            Self::Llm(error) => (error.name, error.message.clone()),
            Self::Memory(error) => (error.js_name(), error.js_message()),
        };
        format!("{name}: {}", String::from_utf16_lossy(message.units()))
    }
}
impl From<FileMemoryError> for DreamRunnerError {
    fn from(error: FileMemoryError) -> Self {
        Self::Memory(error)
    }
}
impl From<LlmError> for DreamRunnerError {
    fn from(error: LlmError) -> Self {
        Self::Llm(error)
    }
}

/// The four Dream-side LLM calls the runner consumes. Implemented for the real
/// [`LlmMemoryExtractor`]; tests script the same trait without HTTP.
pub trait DreamExtractor: Send + Sync {
    fn plan_dream_clusters<'a>(
        &'a self,
        input: &'a JsObject,
        debug: &'a mut (dyn FnMut(JsValue) + Send),
    ) -> BoxFuture<'a, std::result::Result<JsObject, LlmError>>;
    fn refine_dream_cluster<'a>(
        &'a self,
        input: &'a JsObject,
        debug: &'a mut (dyn FnMut(JsValue) + Send),
    ) -> BoxFuture<'a, std::result::Result<JsObject, LlmError>>;
    fn plan_general_project_meta_merges<'a>(
        &'a self,
        input: &'a JsObject,
        debug: &'a mut (dyn FnMut(JsValue) + Send),
    ) -> BoxFuture<'a, std::result::Result<JsObject, LlmError>>;
    fn review_dream_project_meta<'a>(
        &'a self,
        input: &'a JsObject,
        debug: &'a mut (dyn FnMut(JsValue) + Send),
    ) -> BoxFuture<'a, std::result::Result<JsObject, LlmError>>;
}

impl DreamExtractor for LlmMemoryExtractor {
    fn plan_dream_clusters<'a>(
        &'a self,
        input: &'a JsObject,
        debug: &'a mut (dyn FnMut(JsValue) + Send),
    ) -> BoxFuture<'a, std::result::Result<JsObject, LlmError>> {
        Box::pin(self.plan_dream_clusters_with_debug(input, debug))
    }
    fn refine_dream_cluster<'a>(
        &'a self,
        input: &'a JsObject,
        debug: &'a mut (dyn FnMut(JsValue) + Send),
    ) -> BoxFuture<'a, std::result::Result<JsObject, LlmError>> {
        Box::pin(self.refine_dream_cluster_with_debug(input, debug))
    }
    fn plan_general_project_meta_merges<'a>(
        &'a self,
        input: &'a JsObject,
        debug: &'a mut (dyn FnMut(JsValue) + Send),
    ) -> BoxFuture<'a, std::result::Result<JsObject, LlmError>> {
        Box::pin(self.plan_general_project_meta_merges_with_debug(input, debug))
    }
    fn review_dream_project_meta<'a>(
        &'a self,
        input: &'a JsObject,
        debug: &'a mut (dyn FnMut(JsValue) + Send),
    ) -> BoxFuture<'a, std::result::Result<JsObject, LlmError>> {
        Box::pin(self.review_dream_project_meta_with_debug(input, debug))
    }
}

/// `Date.now()`, `Math.random()` and `nowIso()` are host inputs in TS. The
/// runner receives them through this seam so the oracle can freeze both.
pub trait DreamEnvironment: Send + Sync {
    fn now_ms(&self) -> f64;
    fn random(&self) -> f64;
    fn now_iso(&self) -> JsString;
}

/// TS `LoggerLike` (`dream-review.ts:28-34`). The runner only calls `warn`;
/// the other levels stay available so the later `run()` package can reuse the
/// same seam. Mirrors `DreamReviewRunnerOptions.logger`, which is optional.
pub trait DreamLogger: Send + Sync {
    fn info(&self, _message: &str) {}
    fn warn(&self, _message: &str) {}
    fn error(&self, _message: &str) {}
}

#[derive(Debug, Clone, PartialEq)]
pub struct CategoryDreamResult {
    pub planned_clusters: f64,
    pub refined_clusters: f64,
    pub deleted_files: f64,
    pub dropped_warnings: Vec<JsValue>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GeneralProjectMergeResult {
    pub merged_projects: f64,
    pub deleted_projects: f64,
    pub relinked_files: f64,
    pub plan_summary: Option<JsString>,
    pub dropped_warnings: Vec<JsValue>,
    pub groups: Vec<JsValue>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MetaReviewResult {
    pub updated: bool,
    pub reason: JsString,
}

pub struct DreamRewriteRunner<'a> {
    repository: &'a EdgeClawFileStores,
    extractor: &'a dyn DreamExtractor,
    environment: &'a dyn DreamEnvironment,
    logger: Option<&'a dyn DreamLogger>,
}

impl<'a> DreamRewriteRunner<'a> {
    pub fn new(
        repository: &'a EdgeClawFileStores,
        extractor: &'a dyn DreamExtractor,
        environment: &'a dyn DreamEnvironment,
    ) -> Self {
        Self {
            repository,
            extractor,
            environment,
            logger: None,
        }
    }

    /// `new DreamRewriteRunner(repository, extractor, {logger})`
    /// (`dream-review.ts:532-541`). Optional; without it the merge planning
    /// failure warning is simply not emitted, like `this.logger?.warn?.()`.
    pub fn with_logger(mut self, logger: &'a dyn DreamLogger) -> Self {
        self.logger = Some(logger);
        self
    }

    /// `runCategoryDream` (`dream-review.ts:543-798`).
    pub async fn run_category_dream(
        &self,
        trace: &mut JsObject,
        kind: &JsString,
        entries: &[JsObject],
        records: &[JsObject],
        project_meta: Option<&JsObject>,
    ) -> Result<CategoryDreamResult> {
        let entry_values = JsValue::Array(entries.iter().cloned().map(JsValue::Object).collect());
        let sorted_all = sort_entries(&entry_values)?;
        let sorted_entries: Vec<JsObject> = sorted_all
            .into_iter()
            .take(DREAM_HEADER_SCAN_LIMIT)
            .collect();
        let headers: Vec<JsValue> = sorted_entries
            .iter()
            .map(|entry| JsValue::Object(to_header_input(entry)))
            .collect();

        let is_project = kind.equals_str("project");
        let header_step_kind = if is_project {
            "project_header_scan"
        } else {
            "feedback_header_scan"
        };
        let cluster_plan_kind = if is_project {
            "project_cluster_plan"
        } else {
            "feedback_cluster_plan"
        };
        let refine_kind = if is_project {
            "project_cluster_refine"
        } else {
            "feedback_cluster_refine"
        };
        let category_label: JsString = if is_project {
            "Project".into()
        } else {
            "Feedback".into()
        };
        let scoped_label = match project_meta {
            Some(meta) => concat(&[
                &category_label,
                &" · ".into(),
                &string_field(meta, "projectName").unwrap_or_default(),
            ]),
            None => category_label.clone(),
        };
        let kind_label = kind.clone();

        let mut header_options = JsObject::new();
        header_options.insert(
            "titleI18n".into(),
            JsValue::Object(
                trace_i18n(
                    &concat(&[&"trace.step.".into(), &header_step_kind.into()]),
                    &concat(&[&category_label, &" Header Scan".into()]),
                    &[],
                )
                .map_err(|_| {
                    DreamRunnerError::Memory(FileMemoryError::Range(
                        crate::js_value::StackOverflow::MESSAGE.into(),
                    ))
                })?,
            ),
        );
        let mut header_details = vec![JsValue::Object(kv_detail(
            &concat(&[&kind_label, &"-header-summary".into()]),
            &concat(&[&category_label, &" Header Summary".into()]),
            &header_summary_entries(
                entries.len() as f64,
                sorted_entries.len() as f64,
                project_meta,
            ),
        ))];
        if !sorted_entries.is_empty() {
            let items: Vec<JsValue> = sorted_entries
                .iter()
                .map(|entry| {
                    JsValue::String(concat(&[
                        &string_field(entry, "relativePath").unwrap_or_default(),
                        &" | ".into(),
                        &string_field(entry, "name").unwrap_or_default(),
                        &" | ".into(),
                        &string_field(entry, "updatedAt").unwrap_or_default(),
                    ]))
                })
                .collect();
            header_details.push(JsValue::Object(list_detail(
                &concat(&[&kind_label, &"-headers".into()]),
                &concat(&[&category_label, &" Headers".into()]),
                items,
            )));
        }
        header_options.insert("details".into(), JsValue::Array(header_details));
        push_step(
            trace,
            &header_step_kind.into(),
            &concat(&[&scoped_label, &" Header Scan".into()]),
            &(if !sorted_entries.is_empty() {
                "success"
            } else {
                "skipped"
            })
            .into(),
            &concat(&[
                &num(entries.len() as f64),
                &" active ".into(),
                &kind_label,
                &" files".into(),
            ]),
            &if !sorted_entries.is_empty() {
                concat(&[
                    &"Loaded ".into(),
                    &num(sorted_entries.len() as f64),
                    &" ".into(),
                    &kind_label,
                    &" headers for cluster planning.".into(),
                ])
            } else {
                concat(&[
                    &"No active ".into(),
                    &kind_label,
                    &" files were available for cluster planning.".into(),
                ])
            },
            &header_options,
        );

        if sorted_entries.len() < 2 {
            let mut options = JsObject::new();
            options.insert(
                "titleI18n".into(),
                JsValue::Object(
                    trace_i18n(
                        &concat(&[&"trace.step.".into(), &cluster_plan_kind.into()]),
                        &concat(&[&category_label, &" Cluster Plan".into()]),
                        &[],
                    )
                    .map_err(|_| {
                        DreamRunnerError::Memory(FileMemoryError::Range(
                            crate::js_value::StackOverflow::MESSAGE.into(),
                        ))
                    })?,
                ),
            );
            push_step(
                trace,
                &cluster_plan_kind.into(),
                &concat(&[&scoped_label, &" Cluster Plan".into()]),
                &"skipped".into(),
                &concat(&[
                    &num(sorted_entries.len() as f64),
                    &" ".into(),
                    &kind_label,
                    &" headers".into(),
                ]),
                &concat(&[
                    &"Dream skipped ".into(),
                    &kind_label,
                    &" cluster planning because fewer than 2 files were available.".into(),
                ]),
                &options,
            );
            return Ok(CategoryDreamResult {
                planned_clusters: 0.0,
                refined_clusters: 0.0,
                deleted_files: 0.0,
                dropped_warnings: vec![],
            });
        }

        let mut plan_input = JsObject::new();
        plan_input.insert("kind".into(), JsValue::String(kind.clone()));
        plan_input.insert("headers".into(), JsValue::Array(headers));
        let mut plan_debug = Vec::new();
        let raw_plan = self
            .extractor
            .plan_dream_clusters(&plan_input, &mut |event| plan_debug.push(event))
            .await?;
        let cluster_plan_debug = plan_debug.pop();
        let raw_clusters = raw_plan
            .get("clusters")
            .cloned()
            .unwrap_or(JsValue::Undefined);
        let raw_summary = string_field(&raw_plan, "summary");
        let allowed: HashSet<JsString> = sorted_entries
            .iter()
            .filter_map(|entry| string_field(entry, "relativePath"))
            .collect();
        let validated =
            validate_exclusive_clusters(&raw_clusters, &allowed, DREAM_CLUSTER_MAX_FILES)?;
        let clusters = array_field(&validated, "clusters");
        let mut dropped_warnings = array_field(&validated, "droppedWarnings");

        let planner_count = match &raw_clusters {
            JsValue::Array(items) => items.len() as f64,
            _ => 0.0,
        };
        let mut plan_options = JsObject::new();
        plan_options.insert(
            "titleI18n".into(),
            JsValue::Object(
                trace_i18n(
                    &concat(&[&"trace.step.".into(), &cluster_plan_kind.into()]),
                    &concat(&[&category_label, &" Cluster Plan".into()]),
                    &[],
                )
                .map_err(|_| {
                    DreamRunnerError::Memory(FileMemoryError::Range(
                        crate::js_value::StackOverflow::MESSAGE.into(),
                    ))
                })?,
            ),
        );
        let status: JsString = if !clusters.is_empty() {
            "success".into()
        } else if !dropped_warnings.is_empty() {
            "warning".into()
        } else {
            "skipped".into()
        };
        let output = if !clusters.is_empty() {
            raw_summary.clone().unwrap_or_default()
        } else if !dropped_warnings.is_empty() {
            concat(&[
                &"Dream produced no valid ".into(),
                &kind_label,
                &" clusters after validation.".into(),
            ])
        } else {
            concat(&[
                &"Dream found no ".into(),
                &kind_label,
                &" files that should be refined together.".into(),
            ])
        };
        let summary_entries = {
            let mut items = project_meta_entries(project_meta);
            items.push(("plannerClusters".into(), JsValue::Number(planner_count)));
            items.push((
                "acceptedClusters".into(),
                JsValue::Number(clusters.len() as f64),
            ));
            items.push((
                "droppedClusters".into(),
                JsValue::Number(dropped_warnings.len() as f64),
            ));
            items.push((
                "clusterMaxFiles".into(),
                JsValue::Number(DREAM_CLUSTER_MAX_FILES),
            ));
            items
        };
        let mut plan_details = vec![JsValue::Object(kv_detail(
            &concat(&[&kind_label, &"-cluster-summary".into()]),
            &concat(&[&category_label, &" Cluster Summary".into()]),
            &summary_entries,
        ))];
        let mut plan_json = JsObject::new();
        if let Some(summary) = raw_summary.clone() {
            plan_json.insert("summary".into(), JsValue::String(summary));
        }
        plan_json.insert("plannerClusters".into(), raw_clusters.clone());
        plan_json.insert("acceptedClusters".into(), JsValue::Array(clusters.clone()));
        plan_json.insert(
            "droppedWarnings".into(),
            JsValue::Array(dropped_warnings.clone()),
        );
        plan_details.push(JsValue::Object(json_detail(
            &concat(&[&kind_label, &"-cluster-plan-output".into()]),
            &concat(&[&category_label, &" Cluster Plan Output".into()]),
            JsValue::Object(plan_json),
        )));
        plan_options.insert("details".into(), JsValue::Array(plan_details));
        if let Some(debug) = cluster_plan_debug.filter(js_truthy) {
            plan_options.insert("promptDebug".into(), debug);
        }
        push_step(
            trace,
            &cluster_plan_kind.into(),
            &concat(&[&scoped_label, &" Cluster Plan".into()]),
            &status,
            &concat(&[
                &num(sorted_entries.len() as f64),
                &" ".into(),
                &kind_label,
                &" headers".into(),
            ]),
            &output,
            &plan_options,
        );

        let record_by_path: HashMap<JsString, JsObject> = records
            .iter()
            .filter_map(|record| {
                string_field(record, "relativePath").map(|path| (path, record.clone()))
            })
            .collect();

        let mut refined_clusters = 0.0;
        let mut deleted_files = 0.0;
        for (index, cluster) in clusters.iter().enumerate() {
            let members = cluster_members(cluster);
            let cluster_records: Vec<JsObject> = members
                .iter()
                .filter_map(|path| record_by_path.get(path).cloned())
                .collect();
            if cluster_records.len() < 2 {
                dropped_warnings.push(JsValue::String(concat(&[
                    &"Skipped ".into(),
                    &kind_label,
                    &" cluster ".into(),
                    &num((index + 1) as f64),
                    &" because fewer than 2 files could be loaded.".into(),
                ])));
                continue;
            }

            let mut refine_input = JsObject::new();
            refine_input.insert("kind".into(), JsValue::String(kind.clone()));
            refine_input.insert(
                "records".into(),
                JsValue::Array(
                    cluster_records
                        .iter()
                        .map(|record| JsValue::Object(to_dream_record_input(record)))
                        .collect(),
                ),
            );
            let mut refine_debug = Vec::new();
            let refined = self
                .extractor
                .refine_dream_cluster(&refine_input, &mut |event| refine_debug.push(event))
                .await?;
            let refine_debug = refine_debug.pop();
            let refined_file = refined.get("file").and_then(JsValue::as_object).cloned();

            let members_value =
                JsValue::Array(members.iter().cloned().map(JsValue::String).collect());
            let cluster_json = {
                let mut object = JsObject::new();
                if let Some(meta) = project_meta {
                    put_field(&mut object, "projectId", meta.get("projectId"));
                    put_field(&mut object, "projectName", meta.get("projectName"));
                }
                object.insert("members".into(), members_value.clone());
                put_field(
                    &mut object,
                    "reason",
                    cluster.get("reason").cloned().as_ref(),
                );
                object
            };

            if refined_file.is_none() {
                let mut options = JsObject::new();
                let mut details = vec![JsValue::Object(json_detail(
                    &concat(&[&kind_label, &"-cluster-".into(), &num((index + 1) as f64)]),
                    &concat(&[&category_label, &" Cluster".into()]),
                    JsValue::Object(cluster_json),
                ))];
                options.insert(
                    "titleI18n".into(),
                    step_title(
                        &concat(&[&"trace.step.".into(), &refine_kind.into()]),
                        &concat(&[&category_label, &" Cluster Refine".into()]),
                    )?,
                );
                if let Some(debug) = refine_debug.clone().filter(js_truthy) {
                    options.insert("promptDebug".into(), debug);
                }
                options.insert(
                    "details".into(),
                    JsValue::Array(std::mem::take(&mut details)),
                );
                push_step(
                    trace,
                    &refine_kind.into(),
                    &concat(&[
                        &scoped_label,
                        &" Cluster Refine #".into(),
                        &num((index + 1) as f64),
                    ]),
                    &"warning".into(),
                    &concat(&[
                        &num(cluster_records.len() as f64),
                        &" ".into(),
                        &kind_label,
                        &" files".into(),
                    ]),
                    &concat(&[
                        &"Dream could not synthesize a refined ".into(),
                        &kind_label,
                        &" file for this cluster.".into(),
                    ]),
                    &options,
                );
                continue;
            }
            let file = refined_file.expect("checked above");
            let source_record = latest_record(&cluster_records).expect("cluster records");
            let mut candidate_input = JsObject::new();
            candidate_input.insert("kind".into(), JsValue::String(kind.clone()));
            put_field(&mut candidate_input, "name", file.get("name"));
            put_field(&mut candidate_input, "description", file.get("description"));
            put_field(&mut candidate_input, "markdown", file.get("markdown"));
            candidate_input.insert(
                "sourceRecord".into(),
                JsValue::Object(source_record.clone()),
            );
            let mut candidate = to_refined_candidate(&candidate_input);
            if let Some(meta) = project_meta {
                put_field(&mut candidate, "projectId", meta.get("projectId"));
            }

            let member_ids = JsValue::Array(members.iter().cloned().map(JsValue::String).collect());
            self.repository.delete_memory_entries(&member_ids)?;
            for path in &members {
                push_mutation(
                    trace,
                    mutation(&JsString::from("delete"), path, 0.0, 0.0, &JsObject::new()),
                    Some(self.environment),
                );
            }
            let written = self
                .repository
                .get_file_memory_store()
                .upsert_candidate(&candidate)?;
            let written = written.as_object().cloned().unwrap_or_default();
            let written_path = string_field(&written, "relativePath").unwrap_or_default();
            let content = string_field(&written, "content").unwrap_or_default();
            let mut write_options = JsObject::new();
            put_field(
                &mut write_options,
                "candidateType",
                Some(&JsValue::String(kind.clone())),
            );
            put_field(&mut write_options, "name", written.get("name"));
            put_field(
                &mut write_options,
                "description",
                written.get("description"),
            );
            write_options.insert(
                "preview".into(),
                JsValue::String(preview_markdown(&content, 220.0)),
            );
            push_mutation(
                trace,
                mutation(
                    &JsString::from("write"),
                    &written_path,
                    0.0,
                    0.0,
                    &write_options,
                ),
                Some(self.environment),
            );

            refined_clusters += 1.0;
            deleted_files += members.len() as f64;

            let mut new_json = cluster_json.clone();
            new_json.insert("newFile".into(), JsValue::String(written_path.clone()));
            new_json.insert("deletedFiles".into(), members_value.clone());
            let mut options = JsObject::new();
            options.insert(
                "details".into(),
                JsValue::Array(vec![JsValue::Object(json_detail(
                    &concat(&[&kind_label, &"-cluster-".into(), &num((index + 1) as f64)]),
                    &concat(&[&category_label, &" Cluster".into()]),
                    JsValue::Object(new_json),
                ))]),
            );
            options.insert(
                "titleI18n".into(),
                step_title(
                    &concat(&[&"trace.step.".into(), &refine_kind.into()]),
                    &concat(&[&category_label, &" Cluster Refine".into()]),
                )?,
            );
            if let Some(debug) = refine_debug.filter(js_truthy) {
                options.insert("promptDebug".into(), debug);
            }
            push_step(
                trace,
                &refine_kind.into(),
                &concat(&[
                    &scoped_label,
                    &" Cluster Refine #".into(),
                    &num((index + 1) as f64),
                ]),
                &"success".into(),
                &concat(&[
                    &num(cluster_records.len() as f64),
                    &" ".into(),
                    &kind_label,
                    &" files".into(),
                ]),
                &string_field(&refined, "summary").unwrap_or_default(),
                &options,
            );
        }

        Ok(CategoryDreamResult {
            planned_clusters: clusters.len() as f64,
            refined_clusters,
            deleted_files,
            dropped_warnings,
        })
    }

    /// `mergeGeneralProjectMetas` (`dream-review.ts:800-913`).
    pub async fn merge_general_project_metas(
        &self,
        trace: &mut JsObject,
    ) -> Result<GeneralProjectMergeResult> {
        let metas_value = self
            .repository
            .get_file_memory_store()
            .list_project_metas()?;
        let metas = sort_project_metas(&metas_value)?;
        let mut result = GeneralProjectMergeResult {
            merged_projects: 0.0,
            deleted_projects: 0.0,
            relinked_files: 0.0,
            plan_summary: if metas.len() < 2 {
                Some(
                    "Fewer than two General project nodes were available for merge planning."
                        .into(),
                )
            } else {
                None
            },
            dropped_warnings: vec![],
            groups: vec![],
        };

        let mut planner_groups: Vec<JsValue> = vec![];
        let mut accepted_groups: Vec<JsValue> = vec![];
        let mut merge_plan_debug = None;
        if metas.len() >= 2 {
            let mut plan_input = JsObject::new();
            plan_input.insert(
                "projectMetas".into(),
                JsValue::Array(
                    metas
                        .iter()
                        .map(|meta| JsValue::Object(to_dream_meta_input(meta)))
                        .collect(),
                ),
            );
            let mut debug = Vec::new();
            let planned = self
                .extractor
                .plan_general_project_meta_merges(&plan_input, &mut |event| debug.push(event))
                .await;
            merge_plan_debug = debug.pop();
            let planned = planned.map_err(DreamRunnerError::Llm).and_then(|plan| {
                result.plan_summary = string_field(&plan, "summary");
                planner_groups = plan
                    .get("mergeGroups")
                    .and_then(value_array)
                    .cloned()
                    .unwrap_or_default();
                let mut validation_input = JsObject::new();
                validation_input.insert(
                    "metas".into(),
                    JsValue::Array(metas.iter().cloned().map(JsValue::Object).collect()),
                );
                validation_input.insert(
                    "groups".into(),
                    plan.get("mergeGroups")
                        .cloned()
                        .unwrap_or(JsValue::Undefined),
                );
                validate_general_project_merge_groups(&validation_input)
                    .map_err(DreamRunnerError::Memory)
            });
            match planned {
                Ok(validated) => {
                    result.dropped_warnings = array_field(&validated, "droppedWarnings");
                    accepted_groups = array_field(&validated, "acceptedGroups");
                }
                Err(error) => {
                    let message = concat(&[
                        &"Dream skipped General project merge planning because the model request failed: ".into(),
                        &error.to_string().into(),
                    ]);
                    result.plan_summary = Some(message.clone());
                    result.dropped_warnings.push(JsValue::String(message));
                    if let Some(logger) = self.logger {
                        logger.warn(&format!(
                            "[clawxmemory] General project meta merge planning failed: {}",
                            error.js_string()
                        ));
                    }
                }
            }
        }

        let project_by_id: HashMap<JsString, JsObject> = metas
            .iter()
            .filter_map(|meta| string_field(meta, "projectId").map(|id| (id, meta.clone())))
            .collect();

        for group in accepted_groups {
            let group = group.as_object().cloned().unwrap_or_default();
            let keeper_id = string_field(&group, "keeperProjectId").unwrap_or_default();
            let Some(keeper) = project_by_id.get(&keeper_id).cloned() else {
                continue;
            };
            let duplicates: Vec<JsObject> = array_field(&group, "duplicateProjectIds")
                .iter()
                .filter_map(JsValue::as_string)
                .filter_map(|id| project_by_id.get(id).cloned())
                .collect();
            if duplicates.is_empty() {
                continue;
            }
            let mut relinked_files: Vec<JsString> = vec![];
            for duplicate in &duplicates {
                let mut reassign = JsObject::new();
                put_field(&mut reassign, "fromProjectId", duplicate.get("projectId"));
                put_field(&mut reassign, "toProjectId", keeper.get("projectId"));
                let relinked = self
                    .repository
                    .get_file_memory_store()
                    .reassign_project_entries(&reassign)?;
                if let Some(ids) = relinked.get("mutatedIds") {
                    relinked_files.extend(
                        value_array(ids)
                            .into_iter()
                            .flatten()
                            .filter_map(JsValue::as_string)
                            .cloned(),
                    );
                }
                let mut delete_input = JsObject::new();
                delete_input.insert(
                    "relativePaths".into(),
                    JsValue::Array(vec![JsValue::String(
                        string_field(duplicate, "relativePath").unwrap_or_default(),
                    )]),
                );
                self.repository
                    .get_file_memory_store()
                    .delete_entries(&delete_input)?;
                let project_id = string_field(duplicate, "projectId").unwrap_or_default();
                let project_name = string_field(duplicate, "projectName").unwrap_or_default();
                let relative_path = string_field(duplicate, "relativePath").unwrap_or_default();
                let mut seed = "delete_project:".encode_utf16().collect::<Vec<_>>();
                seed.extend(project_id.units());
                seed.push(u16::from(b':'));
                seed.extend(number_string(self.environment.now_ms()).encode_utf16());
                let mut mutation_object = JsObject::new();
                mutation_object.insert(
                    "mutationId".into(),
                    JsValue::String(JsString::from(format!(
                        "mutation_{}",
                        hash_text(&JsString::from_utf16(seed))
                    ))),
                );
                mutation_object.insert("action".into(), "delete_project".into());
                mutation_object.insert("projectId".into(), JsValue::String(project_id));
                mutation_object.insert("projectName".into(), JsValue::String(project_name));
                mutation_object.insert("relativePath".into(), JsValue::String(relative_path));
                push_mutation(trace, mutation_object, None);
                result.merged_projects += 1.0;
                result.deleted_projects += 1.0;
            }
            for relative_path in &relinked_files {
                push_mutation(
                    trace,
                    mutation(&"write".into(), relative_path, 0.0, 0.0, &JsObject::new()),
                    Some(self.environment),
                );
            }
            result.relinked_files += relinked_files.len() as f64;
            let mut group_out = JsObject::new();
            put_field(&mut group_out, "keeperProjectId", keeper.get("projectId"));
            put_field(
                &mut group_out,
                "keeperProjectName",
                keeper.get("projectName"),
            );
            group_out.insert(
                "mergedProjectIds".into(),
                JsValue::Array(
                    duplicates
                        .iter()
                        .filter_map(|duplicate| duplicate.get("projectId").cloned())
                        .collect(),
                ),
            );
            put_field(&mut group_out, "reason", group.get("reason"));
            group_out.insert(
                "relinkedFiles".into(),
                JsValue::Array(relinked_files.into_iter().map(JsValue::String).collect()),
            );
            result.groups.push(JsValue::Object(group_out));
        }

        let mut options = JsObject::new();
        options.insert(
            "titleI18n".into(),
            JsValue::Object(
                trace_i18n(
                    &"trace.step.general_project_merge".into(),
                    &"General Project Merge".into(),
                    &[],
                )
                .map_err(|_| {
                    DreamRunnerError::Memory(FileMemoryError::Range(
                        crate::js_value::StackOverflow::MESSAGE.into(),
                    ))
                })?,
            ),
        );
        let status: JsString = if !result.groups.is_empty() {
            "success".into()
        } else if !result.dropped_warnings.is_empty() {
            "warning".into()
        } else {
            "skipped".into()
        };
        let output = if !result.groups.is_empty() {
            concat(&[
                &"Merged ".into(),
                &num(result.deleted_projects),
                &" duplicate General project nodes and relinked ".into(),
                &num(result.relinked_files),
                &" memory files.".into(),
            ])
        } else {
            result
                .plan_summary
                .clone()
                .filter(|summary| !summary.is_empty())
                .unwrap_or_else(|| "No duplicate General project nodes required merging.".into())
        };
        let mut details = vec![JsValue::Object(kv_detail(
            &"general-project-merge-summary".into(),
            &"General Project Merge Summary".into(),
            &[
                ("projectNodes".into(), JsValue::Number(metas.len() as f64)),
                (
                    "plannerGroups".into(),
                    JsValue::Number(planner_groups.len() as f64),
                ),
                (
                    "droppedGroups".into(),
                    JsValue::Number(result.dropped_warnings.len() as f64),
                ),
                (
                    "mergedProjects".into(),
                    JsValue::Number(result.merged_projects),
                ),
                (
                    "deletedProjects".into(),
                    JsValue::Number(result.deleted_projects),
                ),
                (
                    "relinkedFiles".into(),
                    JsValue::Number(result.relinked_files),
                ),
            ],
        ))];
        let mut json = JsObject::new();
        if let Some(summary) = result.plan_summary.clone() {
            json.insert("summary".into(), JsValue::String(summary));
        }
        json.insert("plannerGroups".into(), JsValue::Array(planner_groups));
        json.insert(
            "acceptedGroups".into(),
            JsValue::Array(result.groups.clone()),
        );
        json.insert(
            "droppedWarnings".into(),
            JsValue::Array(result.dropped_warnings.clone()),
        );
        details.push(JsValue::Object(json_detail(
            &"general-project-merge-plan".into(),
            &"General Project Merge Plan".into(),
            JsValue::Object(json),
        )));
        options.insert("details".into(), JsValue::Array(details));
        if let Some(debug) = merge_plan_debug.filter(js_truthy) {
            options.insert("promptDebug".into(), debug);
        }
        push_step(
            trace,
            &"general_project_merge".into(),
            &"General Project Merge".into(),
            &status,
            &concat(&[&num(metas.len() as f64), &" local general projects".into()]),
            &output,
            &options,
        );

        Ok(result)
    }

    /// `runProjectMetaReview` (`dream-review.ts:915-1024`).
    pub async fn run_project_meta_review(
        &self,
        trace: &mut JsObject,
        project_meta: &JsObject,
    ) -> Result<MetaReviewResult> {
        let project_id = string_field(project_meta, "projectId").unwrap_or_default();
        let source_kind = string_field(project_meta, "sourceKind").unwrap_or_default();
        let mut entry_options = JsObject::new();
        entry_options.insert(
            "kinds".into(),
            JsValue::Array(vec!["project".into(), "feedback".into()]),
        );
        entry_options.insert("includeDeprecated".into(), JsValue::Bool(false));
        entry_options.insert("scope".into(), "project".into());
        entry_options.insert("projectId".into(), JsValue::String(project_id.clone()));
        entry_options.insert("limit".into(), JsValue::Number(5000.0));
        let entries_value = if source_kind.equals_str("workspace_external_mirror") {
            self.repository.list_readable_project_entries(
                &String::from_utf16_lossy(project_id.units()),
                &entry_options,
            )?
        } else {
            self.repository.list_memory_entries(&entry_options)?
        };
        let entries = value_array(&entries_value).cloned().unwrap_or_default();
        let project_entries: Vec<JsObject> = entries
            .iter()
            .filter(|entry| type_is(entry, "project"))
            .filter_map(JsValue::as_object)
            .cloned()
            .collect();
        let feedback_entries: Vec<JsObject> = entries
            .iter()
            .filter(|entry| type_is(entry, "feedback"))
            .filter_map(JsValue::as_object)
            .cloned()
            .collect();
        let recent_project_entries: Vec<JsObject> = sort_entries(&JsValue::Array(
            project_entries.into_iter().map(JsValue::Object).collect(),
        ))?
        .into_iter()
        .take(DREAM_META_PROJECT_CONTEXT_LIMIT)
        .collect();
        let recent_feedback_entries: Vec<JsObject> = sort_entries(&JsValue::Array(
            feedback_entries.into_iter().map(JsValue::Object).collect(),
        ))?
        .into_iter()
        .take(DREAM_META_FEEDBACK_CONTEXT_LIMIT)
        .collect();
        let mut review_ids: Vec<JsValue> = recent_project_entries
            .iter()
            .filter_map(|entry| string_field(entry, "relativePath"))
            .map(JsValue::String)
            .collect();
        review_ids.extend(
            recent_feedback_entries
                .iter()
                .filter_map(|entry| string_field(entry, "relativePath"))
                .map(JsValue::String),
        );
        let records = if review_ids.is_empty() {
            vec![]
        } else {
            let value = self
                .repository
                .get_memory_records_by_ids(&JsValue::Array(review_ids), 5000.0)?;
            value_array(&value)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(JsValue::as_object)
                        .cloned()
                        .collect()
                })
                .unwrap_or_default()
        };
        let record_by_path: HashMap<JsString, JsObject> = records
            .iter()
            .filter_map(|record| {
                string_field(record, "relativePath").map(|path| (path, record.clone()))
            })
            .collect();
        let recent_project_records: Vec<JsObject> = recent_project_entries
            .iter()
            .filter_map(|entry| {
                string_field(entry, "relativePath")
                    .and_then(|path| record_by_path.get(&path).cloned())
            })
            .collect();
        let recent_feedback_records: Vec<JsObject> = recent_feedback_entries
            .iter()
            .filter_map(|entry| {
                string_field(entry, "relativePath")
                    .and_then(|path| record_by_path.get(&path).cloned())
            })
            .collect();

        let mut meta_updated = false;
        let mut meta_review_debug = None;
        let mut meta_review_reason: JsString =
            "Dream kept the current project metadata unchanged.".into();
        let mut meta_lookup = JsObject::new();
        if !project_id.is_empty() {
            meta_lookup.insert("projectId".into(), JsValue::String(project_id.clone()));
        }
        let current_project_meta = self
            .repository
            .get_file_memory_store()
            .get_project_meta(&meta_lookup)?
            .as_object()
            .cloned()
            .unwrap_or_else(|| project_meta.clone());
        if !recent_project_records.is_empty() || !recent_feedback_records.is_empty() {
            let mut review_input = JsObject::new();
            review_input.insert(
                "currentMeta".into(),
                JsValue::Object(to_dream_meta_input(&current_project_meta)),
            );
            review_input.insert(
                "recentProjectRecords".into(),
                JsValue::Array(
                    recent_project_records
                        .iter()
                        .map(|record| JsValue::Object(to_dream_record_input(record)))
                        .collect(),
                ),
            );
            review_input.insert(
                "recentFeedbackRecords".into(),
                JsValue::Array(
                    recent_feedback_records
                        .iter()
                        .map(|record| JsValue::Object(to_dream_record_input(record)))
                        .collect(),
                ),
            );
            let mut debug = Vec::new();
            let review = self
                .extractor
                .review_dream_project_meta(&review_input, &mut |event| debug.push(event))
                .await?;
            meta_review_debug = debug.pop();
            let proposed = review.get("projectMeta").and_then(JsValue::as_object);
            let should_update = review.get("shouldUpdate").is_some_and(js_truthy)
                && proposed.is_some_and(|proposed| {
                    field_changed(&current_project_meta, proposed, "projectName")
                        || field_changed(&current_project_meta, proposed, "description")
                        || field_changed(&current_project_meta, proposed, "status")
                });
            if should_update {
                let proposed = proposed.expect("checked above");
                let mut next_input = JsObject::new();
                put_field(
                    &mut next_input,
                    "projectId",
                    current_project_meta.get("projectId"),
                );
                put_field(&mut next_input, "projectName", proposed.get("projectName"));
                put_field(&mut next_input, "description", proposed.get("description"));
                put_field(&mut next_input, "status", proposed.get("status"));
                for key in ["sourceKind", "sourceWorkspacePath", "sourceProjectId"] {
                    if let Some(value) = current_project_meta
                        .get(key)
                        .filter(|value| js_truthy(value))
                    {
                        next_input.insert(key.into(), value.clone());
                    }
                }
                next_input.insert(
                    "dreamUpdatedAt".into(),
                    JsValue::String(self.environment.now_iso()),
                );
                let next_meta = self
                    .repository
                    .get_file_memory_store()
                    .upsert_project_meta(&next_input)?;
                let next_meta = next_meta.as_object().cloned().unwrap_or_default();
                let relative_path = string_field(&next_meta, "relativePath").unwrap_or_default();
                let preview = concat(&[
                    &string_field(&next_meta, "projectName").unwrap_or_default(),
                    &" | ".into(),
                    &string_field(&next_meta, "status").unwrap_or_default(),
                ]);
                let mut options = JsObject::new();
                options.insert("preview".into(), JsValue::String(preview));
                push_mutation(
                    trace,
                    mutation(&"write".into(), &relative_path, 0.0, 0.0, &options),
                    Some(self.environment),
                );
                meta_updated = true;
            }
            if let Some(reason) =
                string_field(&review, "reason").filter(|reason| !reason.is_empty())
            {
                meta_review_reason = reason;
            }
        }

        let mut options = JsObject::new();
        options.insert(
            "titleI18n".into(),
            JsValue::Object(
                trace_i18n(
                    &"trace.step.project_meta_review".into(),
                    &"Project Meta Review".into(),
                    &[],
                )
                .map_err(|_| {
                    DreamRunnerError::Memory(FileMemoryError::Range(
                        crate::js_value::StackOverflow::MESSAGE.into(),
                    ))
                })?,
            ),
        );
        let mut details = vec![JsValue::Object(kv_detail(
            &"project-meta-review-summary".into(),
            &"Project Meta Review Summary".into(),
            &[
                (
                    "projectId".into(),
                    current_project_meta
                        .get("projectId")
                        .cloned()
                        .unwrap_or(JsValue::Undefined),
                ),
                (
                    "projectName".into(),
                    current_project_meta
                        .get("projectName")
                        .cloned()
                        .unwrap_or(JsValue::Undefined),
                ),
                (
                    "recentProjectFiles".into(),
                    JsValue::Number(recent_project_records.len() as f64),
                ),
                (
                    "recentFeedbackFiles".into(),
                    JsValue::Number(recent_feedback_records.len() as f64),
                ),
                (
                    "metaUpdated".into(),
                    JsValue::String(if meta_updated { "yes" } else { "no" }.into()),
                ),
            ],
        ))];
        let mut json = JsObject::new();
        json.insert(
            "currentMeta".into(),
            JsValue::Object(current_project_meta.clone()),
        );
        json.insert(
            "recentProjectFiles".into(),
            JsValue::Array(
                recent_project_entries
                    .iter()
                    .filter_map(|entry| string_field(entry, "relativePath"))
                    .map(JsValue::String)
                    .collect(),
            ),
        );
        json.insert(
            "recentFeedbackFiles".into(),
            JsValue::Array(
                recent_feedback_entries
                    .iter()
                    .filter_map(|entry| string_field(entry, "relativePath"))
                    .map(JsValue::String)
                    .collect(),
            ),
        );
        details.push(JsValue::Object(json_detail(
            &"project-meta-review-input".into(),
            &"Project Meta Review Input".into(),
            JsValue::Object(json),
        )));
        options.insert("details".into(), JsValue::Array(details));
        if let Some(debug) = meta_review_debug.filter(js_truthy) {
            options.insert("promptDebug".into(), debug);
        }
        push_step(
            trace,
            &"project_meta_review".into(),
            &concat(&[
                &"Project Meta Review · ".into(),
                &string_field(&current_project_meta, "projectName").unwrap_or_default(),
            ]),
            &(if meta_updated { "success" } else { "skipped" }).into(),
            &concat(&[
                &num(recent_project_records.len() as f64),
                &" project files + ".into(),
                &num(recent_feedback_records.len() as f64),
                &" feedback files".into(),
            ]),
            &meta_review_reason,
            &options,
        );

        Ok(MetaReviewResult {
            updated: meta_updated,
            reason: meta_review_reason,
        })
    }
}

// -- local projections -------------------------------------------------------

fn step_title(key: &JsString, fallback: &JsString) -> Result<JsValue> {
    trace_i18n(key, fallback, &[])
        .map(JsValue::Object)
        .map_err(|_| {
            DreamRunnerError::Memory(FileMemoryError::Range(
                crate::js_value::StackOverflow::MESSAGE.into(),
            ))
        })
}

fn num(value: f64) -> JsString {
    number_string(value).into()
}

fn value_array(value: &JsValue) -> Option<&Vec<JsValue>> {
    if let JsValue::Array(items) = value {
        Some(items)
    } else {
        None
    }
}

fn concat(parts: &[&JsString]) -> JsString {
    let mut units = Vec::new();
    for part in parts {
        units.extend(part.units());
    }
    JsString::from_utf16(units)
}

fn string_field(object: &JsObject, key: &str) -> Option<JsString> {
    object.get(key).and_then(JsValue::as_string).cloned()
}

fn put_field(object: &mut JsObject, key: &str, value: Option<&JsValue>) {
    if let Some(value) = value
        && !value.is_undefined()
    {
        object.insert(key.into(), value.clone());
    }
}

fn array_field(object: &JsObject, key: &str) -> Vec<JsValue> {
    object
        .get(key)
        .and_then(value_array)
        .cloned()
        .unwrap_or_default()
}

fn type_is(entry: &JsValue, wanted: &str) -> bool {
    entry
        .get("type")
        .and_then(JsValue::as_string)
        .is_some_and(|value| value.equals_str(wanted))
}

fn cluster_members(cluster: &JsValue) -> Vec<JsString> {
    cluster
        .get("memberRelativePaths")
        .and_then(value_array)
        .map(|items| {
            items
                .iter()
                .filter_map(JsValue::as_string)
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

fn project_meta_entries(project_meta: Option<&JsObject>) -> Vec<(JsString, JsValue)> {
    let mut entries = vec![];
    if let Some(meta) = project_meta {
        for key in ["projectId", "projectName"] {
            entries.push((
                key.into(),
                meta.get(key).cloned().unwrap_or(JsValue::Undefined),
            ));
        }
    }
    entries
}

fn header_summary_entries(
    input_files: f64,
    scanned_headers: f64,
    project_meta: Option<&JsObject>,
) -> Vec<(JsString, JsValue)> {
    let mut entries = project_meta_entries(project_meta);
    entries.push(("inputFiles".into(), JsValue::Number(input_files)));
    entries.push(("scannedHeaders".into(), JsValue::Number(scanned_headers)));
    entries.push((
        "headerScanLimit".into(),
        JsValue::Number(DREAM_HEADER_SCAN_LIMIT as f64),
    ));
    entries
}

fn kv_detail(key: &JsString, label: &JsString, entries: &[(JsString, JsValue)]) -> JsObject {
    let mut out = JsObject::new();
    out.insert("key".into(), JsValue::String(key.clone()));
    out.insert("label".into(), JsValue::String(label.clone()));
    out.insert("kind".into(), "kv".into());
    out.insert(
        "entries".into(),
        JsValue::Array(
            entries
                .iter()
                .map(|(entry_label, value)| {
                    let mut entry = JsObject::new();
                    entry.insert("label".into(), JsValue::String(entry_label.clone()));
                    entry.insert("value".into(), JsValue::String(kv_value(value)));
                    JsValue::Object(entry)
                })
                .collect(),
        ),
    );
    out
}

fn kv_value(value: &JsValue) -> JsString {
    match value {
        JsValue::Null | JsValue::Undefined => JsString::default(),
        _ => to_string_units(value)
            .map(JsString::from_utf16)
            .unwrap_or_else(|_| crate::js_value::StackOverflow::MESSAGE.into()),
    }
}

fn list_detail(key: &JsString, label: &JsString, items: Vec<JsValue>) -> JsObject {
    let mut out = JsObject::new();
    out.insert("key".into(), JsValue::String(key.clone()));
    out.insert("label".into(), JsValue::String(label.clone()));
    out.insert("kind".into(), "list".into());
    out.insert("items".into(), JsValue::Array(items));
    out
}

fn json_detail(key: &JsString, label: &JsString, json: JsValue) -> JsObject {
    let mut out = JsObject::new();
    out.insert("key".into(), JsValue::String(key.clone()));
    out.insert("label".into(), JsValue::String(label.clone()));
    out.insert("kind".into(), "json".into());
    out.insert("json".into(), json);
    out
}

fn push_mutation(
    trace: &mut JsObject,
    mut object: JsObject,
    environment: Option<&dyn DreamEnvironment>,
) {
    if let Some(environment) = environment {
        let action = string_field(&object, "action").unwrap_or_default();
        let relative_path = string_field(&object, "relativePath").unwrap_or_default();
        let mut options = JsObject::new();
        for key in ["candidateType", "name", "description", "preview"] {
            put_field(&mut options, key, object.get(key));
        }
        object = mutation(
            &action,
            &relative_path,
            environment.now_ms(),
            environment.random(),
            &options,
        );
    }
    let mut mutations = match trace.get("mutations") {
        Some(JsValue::Array(items)) => items.clone(),
        _ => Vec::new(),
    };
    mutations.push(JsValue::Object(object));
    trace.insert("mutations".into(), JsValue::Array(mutations));
}

fn field_changed(current: &JsObject, proposed: &JsObject, key: &str) -> bool {
    match (current.get(key), proposed.get(key)) {
        (None | Some(JsValue::Undefined), None | Some(JsValue::Undefined)) => false,
        (Some(left), Some(right)) => left != right,
        _ => true,
    }
}

/// `[...records].sort((l, r) => r.updatedAt.localeCompare(l.updatedAt))[0]`:
/// descending `updatedAt` only, stable on ties (the first input record wins).
fn latest_record(records: &[JsObject]) -> Option<JsObject> {
    let mut best: Option<&JsObject> = None;
    if records.len() >= 2 {
        let collator = node_collator(&node_locale(|key| std::env::var(key).ok())).ok()?;
        for record in records {
            let Some(current) = best else {
                best = Some(record);
                continue;
            };
            let left = string_field(record, "updatedAt").unwrap_or_default();
            let right = string_field(current, "updatedAt").unwrap_or_default();
            if node_compare_utf16(&collator, &left, &right) == std::cmp::Ordering::Greater {
                best = Some(record);
            }
        }
    } else {
        best = records.first();
    }
    best.cloned()
}

/// `previewMarkdown` (`dream-review.ts:228-230`): strip heading markers and
/// collapse whitespace, then the local `truncate` that appends `...`.
///
/// Operates on UTF-16 units so an unpaired surrogate survives the rewrite
/// instead of being folded to U+FFFD by `String::from_utf16_lossy`.
fn preview_markdown(markdown: &JsString, max_length: f64) -> JsString {
    let units = markdown.units();
    let mut stripped: Vec<u16> = Vec::with_capacity(units.len());
    let mut index = 0;
    while index < units.len() {
        let line_start =
            index == 0 || matches!(units[index - 1], 0x000A | 0x000D | 0x2028 | 0x2029);
        if line_start {
            let mut hashes = index;
            while hashes < units.len() && units[hashes] == u16::from(b'#') {
                hashes += 1;
            }
            if hashes > index
                && hashes < units.len()
                && crate::runtime::memory::js_is_space_unit(units[hashes])
            {
                index = hashes;
                while index < units.len() && crate::runtime::memory::js_is_space_unit(units[index])
                {
                    index += 1;
                }
                continue;
            }
        }
        stripped.push(units[index]);
        index += 1;
    }
    let mut collapsed: Vec<u16> = Vec::with_capacity(stripped.len());
    let mut in_space = false;
    for unit in stripped {
        if crate::runtime::memory::js_is_space_unit(unit) {
            if !in_space {
                collapsed.push(u16::from(b' '));
                in_space = true;
            }
        } else {
            collapsed.push(unit);
            in_space = false;
        }
    }
    let normalized =
        JsString::from_utf16(crate::runtime::memory::js_trim_units(&collapsed).to_vec());
    if normalized.len_utf16() as f64 <= max_length {
        return normalized;
    }
    let end = if max_length.is_nan() {
        0
    } else if max_length.is_infinite() {
        normalized.len_utf16()
    } else {
        max_length.trunc() as usize
    };
    let truncated = JsString::from_utf16(
        crate::runtime::memory::js_trim_units(
            &normalized.units()[..end.min(normalized.len_utf16())],
        )
        .to_vec(),
    );
    concat(&[&truncated, &"...".into()])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_matches_ts_shape() {
        assert_eq!(
            preview_markdown(&"### Header\n\nBody   text".into(), 220.0),
            "Header Body text".into(),
            "#773 preview strips headings"
        );
        assert_eq!(
            preview_markdown(&"abcdef".into(), 3.0),
            "abc...".into(),
            "#773 preview truncates"
        );
        // `\u2028` is an ECMAScript line terminator for the `m` flag.
        assert_eq!(
            preview_markdown(&"# a\u{2028}# b".into(), 220.0),
            "a b".into(),
            "#773 preview strips U+2028 headings"
        );
    }

    #[test]
    fn preview_keeps_unpaired_surrogates() {
        // A lone high surrogate must survive heading stripping and whitespace
        // collapsing instead of becoming U+FFFD.
        let input = JsString::from_utf16(vec![0xD800, 0x0020, 0x0020, 0xDC00]);
        let output = preview_markdown(&input, 220.0);
        assert_eq!(
            output.units(),
            &[0xD800, 0x0020, 0xDC00],
            "#773 preview keeps lone surrogates"
        );
    }
}
