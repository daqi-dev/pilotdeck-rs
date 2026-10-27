//! EdgeClaw `ReasoningRetriever` (A6.6-edgeclaw-10).
//!
//! Ports the fixed TS baseline `core/retrieval/reasoning-loop.ts:364-799`
//! and the service wrappers `service.ts:889-913,971-979`:
//! the retrieve pipeline (auto/explicit, memory gate, general project
//! selection, manifest selection with fallback, record loading, context
//! rendering and trace/debug), the 30 s `recallCache`, `resetTransientState`
//! and `getRuntimeStats`.
//!
//! Ambient inputs stay injected so callers can freeze or advance them:
//! `Date.now()` is [`ReasoningRetriever::with_dependencies`]' clock and the
//! `Math.random().toString(36).slice(2, 10)` fragment is its token source.
//! Every string remains [`JsString`] (UTF-16 code units) end to end, and the
//! result object is built in the TS literal/spread order so trace, debug and
//! stats compare byte-for-byte with the real TS oracle.
//!
//! The cache deliberately mirrors the TS `Map` semantics, including the
//! lifetime edge measured in `_tmp/20261026-issue-767/probe-m10.txt`: the
//! `set` happens after the LLM await, so a `resetTransientState()` during an
//! in-flight retrieve does not stop the pre-reset result from being cached,
//! and two same-key concurrent misses both call the model (last write wins).

use super::retrieval::{
    MemoryMessage, ProjectMetaRecord, ProjectShortlistCandidate, ReadableProjectCatalogEntry,
    RecallHeaderEntry, StepOptions, build_project_shortlist, create_trace, fallback_selection,
    has_user_summary, normalize_query_key, push_step, recent_user_messages, render_context,
};
use super::{
    EdgeClawFileStores,
    file_memory::FileMemoryError,
    llm::{
        envelope::MemoryRoute,
        recall::{
            DecideFileMemoryRouteInput, MemoryMessage as PromptMessage,
            ProjectMetaRecord as PromptProjectMeta, ProjectShortlistCandidate as PromptCandidate,
            RecallHeaderEntry as PromptManifestEntry, SelectFileManifestEntriesInput,
            SelectRecallProjectInput,
        },
        transport::LlmMemoryExtractor,
    },
    text::{
        build_memory_recall_system_context, decode_escaped_unicode_text,
        decode_escaped_unicode_value, now_iso_at,
    },
};
use crate::{
    js_value::to_string_units,
    project_files::{node_collator, node_locale},
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, stringify_json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
};

/// `RECALL_CACHE_TTL_MS` (`reasoning-loop.ts:23`).
pub const RECALL_CACHE_TTL_MS: i64 = 30_000;
/// `MANIFEST_LIMIT` (`reasoning-loop.ts:24`).
pub const MANIFEST_LIMIT: usize = 200;
/// `DEFAULT_SELECTION_LIMIT` (`reasoning-loop.ts:25`).
pub const DEFAULT_SELECTION_LIMIT: f64 = 5.0;
/// `RECALL_FILE_MAX_LINES` (`reasoning-loop.ts:26`).
pub const RECALL_FILE_MAX_LINES: f64 = 200.0;

type Result<T> = std::result::Result<T, FileMemoryError>;

/// `RetrievalOptions` (`reasoning-loop.ts:28-32`).
#[derive(Clone, Debug, Default)]
pub struct RetrievalOptions {
    /// `retrievalMode ?? "auto"`.
    pub retrieval_mode: Option<JsString>,
    pub recent_messages: Vec<MemoryMessage>,
    /// `workspaceHint ?? ""`.
    pub workspace_hint: JsString,
}

impl RetrievalOptions {
    pub fn auto() -> Self {
        Self::default()
    }
    pub fn explicit() -> Self {
        Self {
            retrieval_mode: Some(JsString::from("explicit")),
            ..Self::default()
        }
    }
}

/// `RetrievalRuntimeStats` (`reasoning-loop.ts:40-47`).
#[derive(Clone, Debug)]
pub struct RetrievalRuntimeStats {
    pub last_recall_ms: f64,
    pub recall_timeouts: f64,
    pub last_recall_mode: JsString,
    pub last_recall_path: JsString,
    pub last_recall_injected: bool,
    pub last_recall_cache_hit: bool,
}

impl Default for RetrievalRuntimeStats {
    fn default() -> Self {
        Self {
            last_recall_ms: 0.0,
            recall_timeouts: 0.0,
            last_recall_mode: JsString::from("none"),
            last_recall_path: JsString::from("explicit"),
            last_recall_injected: false,
            last_recall_cache_hit: false,
        }
    }
}

impl RetrievalRuntimeStats {
    /// `getRuntimeStats()` spreads the record, so key order is the field order.
    pub fn to_object(&self) -> JsObject {
        object(vec![
            ("lastRecallMs", JsValue::Number(self.last_recall_ms)),
            ("recallTimeouts", JsValue::Number(self.recall_timeouts)),
            (
                "lastRecallMode",
                JsValue::String(self.last_recall_mode.clone()),
            ),
            (
                "lastRecallPath",
                JsValue::String(self.last_recall_path.clone()),
            ),
            (
                "lastRecallInjected",
                JsValue::Bool(self.last_recall_injected),
            ),
            (
                "lastRecallCacheHit",
                JsValue::Bool(self.last_recall_cache_hit),
            ),
        ])
    }
}

#[derive(Clone)]
struct RecallCacheEntry {
    expires_at: i64,
    result: JsObject,
}

/// The injected `Math.random().toString(36).slice(2, 10)` fragment.
pub type RandomToken = Arc<dyn Fn() -> JsString + Send + Sync>;
/// The injected `Date.now()`.
pub type RetrieverClock = Arc<dyn Fn() -> i64 + Send + Sync>;

pub struct ReasoningRetriever {
    repository: Arc<EdgeClawFileStores>,
    extractor: Arc<LlmMemoryExtractor>,
    clock: RetrieverClock,
    random_token: RandomToken,
    recall_cache: Mutex<HashMap<JsString, RecallCacheEntry>>,
    runtime_stats: Mutex<RetrievalRuntimeStats>,
}

impl ReasoningRetriever {
    pub fn new(repository: Arc<EdgeClawFileStores>, extractor: Arc<LlmMemoryExtractor>) -> Self {
        Self::with_dependencies(
            repository,
            extractor,
            Arc::new(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis()
                    .min(i64::MAX as u128) as i64
            }),
            Arc::new(default_random_token),
        )
    }

    /// Deterministic dependencies used by the fixture replay and cache tests.
    pub fn with_dependencies(
        repository: Arc<EdgeClawFileStores>,
        extractor: Arc<LlmMemoryExtractor>,
        clock: RetrieverClock,
        random_token: RandomToken,
    ) -> Self {
        Self {
            repository,
            extractor,
            clock,
            random_token,
            recall_cache: Mutex::new(HashMap::new()),
            runtime_stats: Mutex::new(RetrievalRuntimeStats::default()),
        }
    }

    pub fn get_runtime_stats(&self) -> JsObject {
        self.runtime_stats
            .lock()
            .expect("#767 runtime stats lock")
            .to_object()
    }

    /// `resetTransientState`: only the recall cache is transient.
    pub fn reset_transient_state(&self) {
        self.recall_cache
            .lock()
            .expect("#767 recall cache lock")
            .clear();
    }

    /// `service.search`: force `retrievalMode: "explicit"`.
    pub async fn search(&self, query: &JsString, options: &RetrievalOptions) -> Result<JsObject> {
        let mut options = options.clone();
        options.retrieval_mode = Some(JsString::from("explicit"));
        self.retrieve(query, &options).await
    }

    /// `service.retrieveContext`: append `systemContext` to the retrieve result.
    pub async fn retrieve_context(
        &self,
        query: &JsString,
        options: &RetrievalOptions,
    ) -> Result<JsObject> {
        let result = self.retrieve(query, options).await?;
        let context = string_field(&result, "context");
        let system_context = if context.is_empty() {
            JsString::default()
        } else {
            build_memory_recall_system_context(&context)
        };
        let mut out = result;
        out.insert(
            JsString::from("systemContext"),
            JsValue::String(system_context),
        );
        Ok(out)
    }

    /// `ReasoningRetriever.retrieve` (`reasoning-loop.ts:389-799`).
    pub async fn retrieve(&self, query: &JsString, options: &RetrievalOptions) -> Result<JsObject> {
        let started_at_ms = (self.clock)();
        let mode = options
            .retrieval_mode
            .clone()
            .unwrap_or_else(|| JsString::from("auto"));
        let recent = recent_user_messages(Some(&options.recent_messages));
        let snapshot = JsString::from(self.repository.get_snapshot_version()?);
        let cache_key = recall_cache_key(&mode, query, &recent, &snapshot);

        let cached = self
            .recall_cache
            .lock()
            .expect("#767 recall cache lock")
            .get(&cache_key)
            .cloned();
        if let Some(cached) = cached
            && cached.expires_at > (self.clock)()
        {
            let result = cache_hit_result(&cached.result, &mode);
            self.record_cache_hit_stats(&result, &mode);
            return Ok(result);
        }

        let mut trace = create_trace(query, &mode, (self.clock)(), &(self.random_token)());
        push_step(
            &mut trace,
            &s("recall_start"),
            &s("Recall Started"),
            &s("info"),
            query,
            &concat_js(&[&s("mode="), &mode]),
            &StepOptions {
                title_i18n: Some(trace_i18n("trace.step.recall_start", "Recall Started")),
                details: Some(JsValue::Array(vec![kv_detail(
                    "recall-start-inputs",
                    "Recall Inputs",
                    vec![
                        ("query", JsValue::String(query.clone())),
                        ("mode", JsValue::String(mode.clone())),
                        ("recentUserMessages", JsValue::Number(recent.len() as f64)),
                        (
                            "workspaceHint",
                            JsValue::String(options.workspace_hint.clone()),
                        ),
                    ],
                    Some(trace_i18n("trace.detail.recall_inputs", "Recall Inputs")),
                )])),
                ..StepOptions::default()
            },
        );

        let mut gate_debug: Option<JsValue> = None;
        let route = self
            .extractor
            .decide_file_memory_route(
                &DecideFileMemoryRouteInput {
                    query: query.clone(),
                    recent_messages: prompt_messages(&options.recent_messages),
                    agent_id: None,
                    timeout_ms: None,
                },
                &mut |debug| gate_debug = Some(debug),
            )
            .await;
        let route_text = s(route.as_str());
        push_step(
            &mut trace,
            &s("memory_gate"),
            &s("Memory Gate"),
            &s(if route == MemoryRoute::None {
                "warning"
            } else {
                "success"
            }),
            query,
            &concat_str(&["route=", route.as_str()]),
            &StepOptions {
                title_i18n: Some(trace_i18n("trace.step.memory_gate", "Memory Gate")),
                details: Some(JsValue::Array(vec![kv_detail(
                    "gate-route",
                    "Route",
                    vec![
                        ("route", JsValue::String(route_text.clone())),
                        ("recentUserMessages", JsValue::Number(recent.len() as f64)),
                        (
                            "workspaceHint",
                            JsValue::String(options.workspace_hint.clone()),
                        ),
                    ],
                    Some(trace_i18n("trace.detail.route", "Route")),
                )])),
                prompt_debug: gate_debug,
                ..StepOptions::default()
            },
        );

        let needs_user_summary = matches!(route, MemoryRoute::User | MemoryRoute::Mix);
        let needs_project_memory = matches!(route, MemoryRoute::Project | MemoryRoute::Mix);
        let is_general_workspace = self.repository.get_workspace_mode() == "general";
        let user_summary = if needs_user_summary {
            object_or_default(self.repository.get_user_summary()?)
        } else {
            empty_user_summary()
        };
        let mut project_meta = if needs_project_memory {
            project_meta_value(
                self.repository
                    .get_file_memory_store()
                    .get_project_meta(&JsObject::new())?,
            )
        } else {
            None
        };
        let mut selected_project: Option<ReadableProjectCatalogEntry> = None;

        let user_base_required = needs_user_summary;
        let has_user = has_user_summary(&user_summary);
        push_step(
            &mut trace,
            &s("user_base_loaded"),
            &s("User Base Loaded"),
            &s(if !user_base_required {
                "skipped"
            } else if has_user {
                "success"
            } else {
                "warning"
            }),
            &if !user_base_required {
                concat_str(&["route=", route.as_str()])
            } else {
                s("global user profile")
            },
            &if !user_base_required {
                s("Current route does not require user identity background.")
            } else if has_user {
                s("Attached compact global user profile.")
            } else {
                s("No stable user profile was available.")
            },
            &StepOptions {
                title_i18n: Some(trace_i18n(
                    "trace.step.user_base_loaded",
                    "User Base Loaded",
                )),
                details: Some(JsValue::Array(user_base_details(
                    &route_text,
                    needs_user_summary,
                    &user_summary,
                ))),
                ..StepOptions::default()
            },
        );

        if needs_project_memory && is_general_workspace {
            let catalog = catalog_entries(self.repository.list_readable_project_catalog()?);
            let project_shortlist = build_project_shortlist(&catalog, query, &recent, collator());
            push_step(
                &mut trace,
                &s("project_shortlist_built"),
                &s("Project Shortlist Built"),
                &s(if project_shortlist.is_empty() {
                    "warning"
                } else {
                    "success"
                }),
                &concat_str(&[&catalog.len().to_string(), " readable projects"]),
                &if project_shortlist.is_empty() {
                    s("No readable projects were available for General recall.")
                } else {
                    concat_str(&[
                        &project_shortlist.len().to_string(),
                        " candidate projects are available for General recall.",
                    ])
                },
                &StepOptions {
                    details: Some(JsValue::Array(vec![
                        kv_detail(
                            "project-shortlist-summary",
                            "Project Shortlist Summary",
                            vec![
                                ("catalog", JsValue::Number(catalog.len() as f64)),
                                ("shortlist", JsValue::Number(project_shortlist.len() as f64)),
                            ],
                            None,
                        ),
                        list_detail(
                            "project-shortlist-items",
                            "Shortlist",
                            project_shortlist.iter().map(shortlist_text).collect(),
                            None,
                        ),
                    ])),
                    ..StepOptions::default()
                },
            );
            if !project_shortlist.is_empty() {
                let mut project_selection_debug: Option<JsValue> = None;
                let selection = self
                    .extractor
                    .select_recall_project(
                        &SelectRecallProjectInput {
                            query: query.clone(),
                            recent_user_messages: prompt_messages(&recent),
                            shortlist: prompt_shortlist(&project_shortlist),
                            allow_empty: true,
                            agent_id: None,
                            timeout_ms: None,
                        },
                        &mut |debug| project_selection_debug = Some(debug),
                    )
                    .await;
                let selected_project_id = string_field(&selection, "projectId");
                if !selected_project_id.is_empty() {
                    selected_project = self.readable_project(&selected_project_id)?;
                }
                let selected_project_reason = string_field(&selection, "reason");
                if let Some(project) = &selected_project {
                    project_meta = Some(project.clone());
                }
                push_step(
                    &mut trace,
                    &s("project_selected"),
                    &s("Project Selected"),
                    &s(if selected_project.is_some() {
                        "success"
                    } else {
                        "warning"
                    }),
                    &concat_str(&[&project_shortlist.len().to_string(), " candidates"]),
                    &match &selected_project {
                        Some(project) => concat_js(&[
                            &string_field(project, "projectName"),
                            &s(" selected for General recall."),
                        ]),
                        None => s("General recall could not resolve a readable project."),
                    },
                    &StepOptions {
                        details: Some(JsValue::Array(vec![json_detail(
                            "project-selected",
                            "Project Selection",
                            JsValue::Object(object(vec![
                                (
                                    "selectedProjectId",
                                    if selected_project_id.is_empty() {
                                        JsValue::Null
                                    } else {
                                        JsValue::String(selected_project_id.clone())
                                    },
                                ),
                                (
                                    "selectedProjectName",
                                    selected_project
                                        .as_ref()
                                        .map(|project| {
                                            JsValue::String(string_field(project, "projectName"))
                                        })
                                        .unwrap_or(JsValue::Null),
                                ),
                                (
                                    "selectedProjectSource",
                                    selected_project
                                        .as_ref()
                                        .map(|project| {
                                            JsValue::from(
                                                if string_field(project, "sourceType")
                                                    .equals_str("workspace_external")
                                                {
                                                    "workspace_external"
                                                } else {
                                                    "general_local"
                                                },
                                            )
                                        })
                                        .unwrap_or(JsValue::Null),
                                ),
                                (
                                    "reason",
                                    if selected_project_reason.is_empty() {
                                        JsValue::Null
                                    } else {
                                        JsValue::String(selected_project_reason.clone())
                                    },
                                ),
                            ])),
                        )])),
                        prompt_debug: project_selection_debug,
                        ..StepOptions::default()
                    },
                );
            }
        }

        let manifest: Vec<RecallHeaderEntry> = if route == MemoryRoute::User {
            entry_objects(self.repository.list_memory_entries(&object(vec![
                ("kinds", JsValue::Array(vec![JsValue::String(s("user"))])),
                ("scope", JsValue::String(s("global"))),
                ("limit", JsValue::Number(MANIFEST_LIMIT as f64)),
                ("includeDeprecated", JsValue::Bool(false)),
            ]))?)
        } else if needs_project_memory && is_general_workspace {
            match &selected_project {
                Some(project) => {
                    let logical = string_field(project, "logicalProjectId");
                    if logical.is_empty() {
                        Vec::new()
                    } else {
                        let mut entries =
                            entry_objects(self.repository.list_readable_project_entries(
                                &logical.to_utf8().map_err(|_| {
                                    FileMemoryError::Invalid("#767 project id is not UTF-8".into())
                                })?,
                                &object(vec![
                                    (
                                        "kinds",
                                        JsValue::Array(vec![
                                            JsValue::String(s("project")),
                                            JsValue::String(s("feedback")),
                                        ]),
                                    ),
                                    ("includeDeprecated", JsValue::Bool(false)),
                                ]),
                            )?);
                        entries.truncate(MANIFEST_LIMIT);
                        entries
                    }
                }
                None => Vec::new(),
            }
        } else if needs_project_memory {
            entry_objects(self.repository.list_memory_entries(&object(vec![
                (
                    "kinds",
                    JsValue::Array(vec![
                        JsValue::String(s("project")),
                        JsValue::String(s("feedback")),
                    ]),
                ),
                ("scope", JsValue::String(s("project"))),
                ("limit", JsValue::Number(MANIFEST_LIMIT as f64)),
                ("includeDeprecated", JsValue::Bool(false)),
            ]))?)
        } else {
            Vec::new()
        };

        push_step(
            &mut trace,
            &s("manifest_scanned"),
            &s("Manifest Scanned"),
            &s(if !manifest.is_empty() {
                "success"
            } else if route == MemoryRoute::None {
                "skipped"
            } else {
                "warning"
            }),
            &if needs_project_memory {
                if is_general_workspace {
                    concat_js(&[
                        &s("general project="),
                        &selected_project
                            .as_ref()
                            .map(|project| string_field(project, "projectName"))
                            .unwrap_or_else(|| s("none")),
                    ])
                } else {
                    s("current workspace project memory")
                }
            } else {
                concat_str(&["route=", route.as_str()])
            },
            &if manifest.is_empty() {
                s("No matching workspace memory files were available.")
            } else {
                concat_str(&[&manifest.len().to_string(), " recall header entries ready."])
            },
            &StepOptions {
                title_i18n: Some(trace_i18n(
                    "trace.step.manifest_scanned",
                    "Manifest Scanned",
                )),
                details: Some(JsValue::Array(vec![
                    kv_detail(
                        "manifest-scan-summary",
                        "Manifest Scan",
                        vec![
                            ("count", JsValue::Number(manifest.len() as f64)),
                            ("route", JsValue::String(route_text.clone())),
                            (
                                "scope",
                                JsValue::String(s(if route == MemoryRoute::User {
                                    "global"
                                } else if needs_project_memory {
                                    if is_general_workspace {
                                        "general_selected_project"
                                    } else {
                                        "workspace_project"
                                    }
                                } else {
                                    "none"
                                })),
                            ),
                            ("limit", JsValue::Number(MANIFEST_LIMIT as f64)),
                            (
                                "workspaceHint",
                                JsValue::String(options.workspace_hint.clone()),
                            ),
                        ],
                        Some(trace_i18n("trace.detail.manifest_scan", "Manifest Scan")),
                    ),
                    list_detail(
                        "manifest-scan-preview",
                        "Sorted Candidates",
                        manifest
                            .iter()
                            .map(|entry| {
                                concat_js(&[
                                    &string_field(entry, "updatedAt"),
                                    &s(" | "),
                                    &string_field(entry, "type"),
                                    &s(" | "),
                                    &string_field(entry, "relativePath"),
                                    &s(" | "),
                                    &string_field(entry, "description"),
                                ])
                            })
                            .collect(),
                        Some(trace_i18n(
                            "trace.detail.sorted_candidates",
                            "Sorted Candidates",
                        )),
                    ),
                ])),
                ..StepOptions::default()
            },
        );

        let mut selection_debug: Option<JsValue> = None;
        let mut selected_ids: Vec<JsString> = if manifest.is_empty() {
            Vec::new()
        } else {
            self.extractor
                .select_file_manifest_entries(
                    &SelectFileManifestEntriesInput {
                        query: query.clone(),
                        route,
                        recent_user_messages: prompt_messages(&recent),
                        project_meta: prompt_project_meta(&project_meta),
                        manifest: prompt_manifest(&manifest),
                        limit: Some(if route == MemoryRoute::User {
                            1.0
                        } else {
                            DEFAULT_SELECTION_LIMIT
                        }),
                        agent_id: None,
                        timeout_ms: None,
                    },
                    &mut |debug| selection_debug = Some(debug),
                )
                .await
        };
        if selected_ids.is_empty() {
            selected_ids = fallback_selection(&route_text, &manifest);
        }

        push_step(
            &mut trace,
            &s("manifest_selected"),
            &s("Manifest Selected"),
            &s(if !selected_ids.is_empty() {
                "success"
            } else if !manifest.is_empty() {
                "warning"
            } else {
                "skipped"
            }),
            &concat_str(&[&manifest.len().to_string(), " entries"]),
            &concat_str(&[&selected_ids.len().to_string(), " file ids selected."]),
            &StepOptions {
                title_i18n: Some(trace_i18n(
                    "trace.step.manifest_selected",
                    "Manifest Selected",
                )),
                details: Some(JsValue::Array(vec![
                    list_detail(
                        "manifest-selection-input",
                        "Manifest Candidate IDs",
                        manifest
                            .iter()
                            .map(|entry| string_field(entry, "relativePath"))
                            .collect(),
                        Some(trace_i18n(
                            "trace.detail.manifest_candidate_ids",
                            "Manifest Candidate IDs",
                        )),
                    ),
                    list_detail(
                        "selected-files",
                        "Selected File IDs",
                        selected_ids.clone(),
                        Some(trace_i18n(
                            "trace.detail.selected_file_ids",
                            "Selected File IDs",
                        )),
                    ),
                ])),
                prompt_debug: selection_debug,
                ..StepOptions::default()
            },
        );

        let records = if selected_ids.is_empty() {
            Vec::new()
        } else {
            entry_objects(self.repository.get_memory_records_by_ids(
                &JsValue::Array(selected_ids.iter().cloned().map(JsValue::String).collect()),
                RECALL_FILE_MAX_LINES,
            )?)
        };

        push_step(
            &mut trace,
            &s("files_loaded"),
            &s("Files Loaded"),
            &s(if !records.is_empty() {
                "success"
            } else if !selected_ids.is_empty() {
                "warning"
            } else {
                "skipped"
            }),
            &concat_str(&[&selected_ids.len().to_string(), " requested"]),
            &concat_str(&[&records.len().to_string(), " files loaded."]),
            &StepOptions {
                title_i18n: Some(trace_i18n("trace.step.files_loaded", "Files Loaded")),
                details: Some(JsValue::Array(vec![
                    list_detail(
                        "requested-files",
                        "Requested IDs",
                        selected_ids.clone(),
                        Some(trace_i18n("trace.detail.requested_ids", "Requested IDs")),
                    ),
                    list_detail(
                        "loaded-files",
                        "Loaded Files",
                        records
                            .iter()
                            .map(|record| {
                                concat_js(&[
                                    &string_field(record, "relativePath"),
                                    &s(" | "),
                                    &string_field(record, "updatedAt"),
                                ])
                            })
                            .collect(),
                        Some(trace_i18n("trace.detail.loaded_files", "Loaded Files")),
                    ),
                ])),
                ..StepOptions::default()
            },
        );

        let context = if route == MemoryRoute::None {
            JsString::default()
        } else {
            render_context(&route_text, &user_summary, project_meta.as_ref(), &records)
        };
        let context_nonempty = !context.is_empty();
        push_step(
            &mut trace,
            &s("context_rendered"),
            &s("Context Rendered"),
            &s(if context_nonempty {
                "success"
            } else if route == MemoryRoute::None {
                "skipped"
            } else {
                "warning"
            }),
            &concat_str(&[
                &records.len().to_string(),
                if has_user {
                    " files + user base"
                } else {
                    " files"
                },
            ]),
            &if context_nonempty {
                s("Memory context prepared.")
            } else {
                s("No recall context was prepared.")
            },
            &StepOptions {
                title_i18n: Some(trace_i18n(
                    "trace.step.context_rendered",
                    "Context Rendered",
                )),
                details: Some(JsValue::Array(context_details(
                    &route_text,
                    has_user,
                    project_meta.as_ref(),
                    &selected_project,
                    is_general_workspace && needs_project_memory,
                    &records,
                    &user_summary,
                    &context,
                    context_nonempty,
                ))),
                ..StepOptions::default()
            },
        );

        trace.insert(
            JsString::from("finishedAt"),
            JsValue::String(JsString::from(now_iso_at((self.clock)()))),
        );
        let elapsed_ms = (self.clock)() - started_at_ms;
        let result = if route == MemoryRoute::None && !context_nonempty {
            build_empty_result(query, &trace, elapsed_ms)
        } else {
            let mut debug = object(vec![
                (
                    "mode",
                    JsValue::from(if context_nonempty {
                        "llm"
                    } else {
                        "local_fallback"
                    }),
                ),
                ("elapsedMs", JsValue::Number(elapsed_ms as f64)),
                ("cacheHit", JsValue::Bool(false)),
                ("path", JsValue::String(mode.clone())),
            ]);
            if let Some(project) = &selected_project {
                debug.insert(
                    JsString::from("resolvedProjectId"),
                    JsValue::String(string_field(project, "logicalProjectId")),
                );
            }
            debug.insert(JsString::from("route"), JsValue::String(route_text.clone()));
            debug.insert(
                JsString::from("manifestCount"),
                JsValue::Number(manifest.len() as f64),
            );
            debug.insert(
                JsString::from("selectedFileIds"),
                JsValue::Array(selected_ids.iter().cloned().map(JsValue::String).collect()),
            );
            object(vec![
                ("query", JsValue::String(query.clone())),
                ("intent", JsValue::String(route_text.clone())),
                ("context", JsValue::String(context)),
                ("trace", JsValue::Object(trace)),
                ("debug", JsValue::Object(debug)),
            ])
        };

        self.recall_cache
            .lock()
            .expect("#767 recall cache lock")
            .insert(
                cache_key,
                RecallCacheEntry {
                    expires_at: (self.clock)().saturating_add(RECALL_CACHE_TTL_MS),
                    result: result.clone(),
                },
            );
        {
            let previous = self
                .runtime_stats
                .lock()
                .expect("#767 runtime stats lock")
                .clone();
            *self.runtime_stats.lock().expect("#767 runtime stats lock") = RetrievalRuntimeStats {
                last_recall_ms: elapsed_ms as f64,
                recall_timeouts: previous.recall_timeouts,
                last_recall_mode: debug_field(&result, "mode").unwrap_or_else(|| s("none")),
                last_recall_path: debug_field(&result, "path").unwrap_or_else(|| mode.clone()),
                last_recall_injected: !string_field(&result, "context").is_empty(),
                last_recall_cache_hit: false,
            };
        }
        Ok(result)
    }

    fn record_cache_hit_stats(&self, result: &JsObject, mode: &JsString) {
        let mut stats = self.runtime_stats.lock().expect("#767 runtime stats lock");
        stats.last_recall_ms = 0.0;
        stats.last_recall_mode = debug_field(result, "mode").unwrap_or_else(|| s("none"));
        stats.last_recall_path = debug_field(result, "path").unwrap_or_else(|| mode.clone());
        stats.last_recall_injected = !string_field(result, "context").is_empty();
        stats.last_recall_cache_hit = true;
    }

    fn readable_project(&self, logical: &JsString) -> Result<Option<ReadableProjectCatalogEntry>> {
        let logical = logical
            .to_utf8()
            .map_err(|_| FileMemoryError::Invalid("#767 project id is not UTF-8".into()))?;
        Ok(self
            .repository
            .get_readable_project(&logical)?
            .as_object()
            .cloned())
    }
}

// -- pure helpers -----------------------------------------------------------

fn s(value: &str) -> JsString {
    JsString::from(value)
}

fn object(entries: Vec<(&str, JsValue)>) -> JsObject {
    let mut out = JsObject::new();
    for (key, value) in entries {
        out.insert(JsString::from(key), value);
    }
    out
}

fn concat_str(parts: &[&str]) -> JsString {
    JsString::from(parts.concat())
}

/// JS template literals concatenate UTF-16 code units, including lone
/// surrogates; going through `&str` here would silently drop them.
fn concat_js(parts: &[&JsString]) -> JsString {
    let mut units = Vec::new();
    for part in parts {
        units.extend_from_slice(part.units());
    }
    JsString::from_utf16(units)
}

fn string_field(object: &JsObject, key: &str) -> JsString {
    object
        .get(key)
        .and_then(JsValue::as_string)
        .cloned()
        .unwrap_or_default()
}

fn object_or_default(value: JsValue) -> JsObject {
    value.as_object().cloned().unwrap_or_default()
}

fn project_meta_value(value: JsValue) -> Option<ProjectMetaRecord> {
    match &value {
        JsValue::Object(object) => Some(object.clone()),
        _ => None,
    }
}

fn empty_user_summary() -> JsObject {
    object(vec![
        ("identityBackground", JsValue::Array(Vec::new())),
        ("files", JsValue::Array(Vec::new())),
    ])
}

fn catalog_entries(value: JsValue) -> Vec<ReadableProjectCatalogEntry> {
    entry_objects(value)
}

fn entry_objects(value: JsValue) -> Vec<JsObject> {
    match &value {
        JsValue::Array(items) => items
            .iter()
            .filter_map(|item| item.as_object().cloned())
            .collect(),
        _ => Vec::new(),
    }
}

fn prompt_messages(messages: &[JsObject]) -> Vec<PromptMessage> {
    messages
        .iter()
        .map(|message| PromptMessage::from_value(&JsValue::Object(message.clone())))
        .collect()
}

fn prompt_shortlist(items: &[JsObject]) -> Vec<PromptCandidate> {
    items
        .iter()
        .map(|item| PromptCandidate::from_value(&JsValue::Object(item.clone())))
        .collect()
}

fn prompt_project_meta(meta: &Option<JsObject>) -> Option<PromptProjectMeta> {
    meta.as_ref()
        .map(|meta| PromptProjectMeta::from_value(&JsValue::Object(meta.clone())))
}

fn prompt_manifest(entries: &[JsObject]) -> Vec<PromptManifestEntry> {
    entries
        .iter()
        .map(|entry| PromptManifestEntry::from_value(&JsValue::Object(entry.clone())))
        .collect()
}

fn value_string(value: &JsValue) -> JsString {
    match value {
        JsValue::Undefined | JsValue::Null => JsString::default(),
        other => JsString::from_utf16(to_string_units(other).unwrap_or_default()),
    }
}

fn trace_i18n(key: &str, fallback: &str) -> JsValue {
    JsValue::Object(object(vec![
        ("key", JsValue::String(s(key))),
        ("fallback", JsValue::String(s(fallback))),
    ]))
}

fn list_detail(
    key: &str,
    label: &str,
    items: Vec<JsString>,
    label_i18n: Option<JsValue>,
) -> JsValue {
    let mut detail = object(vec![
        ("key", JsValue::String(s(key))),
        ("label", JsValue::String(s(label))),
    ]);
    if let Some(label_i18n) = label_i18n {
        detail.insert(JsString::from("labelI18n"), label_i18n);
    }
    detail.insert(JsString::from("kind"), JsValue::String(s("list")));
    detail.insert(
        JsString::from("items"),
        JsValue::Array(
            items
                .iter()
                .map(|item| JsValue::String(decode_escaped_unicode_text(item, true)))
                .collect(),
        ),
    );
    JsValue::Object(detail)
}

fn kv_detail(
    key: &str,
    label: &str,
    entries: Vec<(&str, JsValue)>,
    label_i18n: Option<JsValue>,
) -> JsValue {
    let mut detail = object(vec![
        ("key", JsValue::String(s(key))),
        ("label", JsValue::String(s(label))),
    ]);
    if let Some(label_i18n) = label_i18n {
        detail.insert(JsString::from("labelI18n"), label_i18n);
    }
    detail.insert(JsString::from("kind"), JsValue::String(s("kv")));
    detail.insert(
        JsString::from("entries"),
        JsValue::Array(
            entries
                .into_iter()
                .map(|(label, value)| {
                    JsValue::Object(object(vec![
                        ("label", JsValue::String(s(label))),
                        (
                            "value",
                            JsValue::String(decode_escaped_unicode_text(
                                &value_string(&value),
                                true,
                            )),
                        ),
                    ]))
                })
                .collect(),
        ),
    );
    JsValue::Object(detail)
}

fn json_detail(key: &str, label: &str, json: JsValue) -> JsValue {
    JsValue::Object(object(vec![
        ("key", JsValue::String(s(key))),
        ("label", JsValue::String(s(label))),
        ("kind", JsValue::String(s("json"))),
        ("json", decode_escaped_unicode_value(&json, true)),
    ]))
}

fn user_base_details(route: &JsString, required: bool, user_summary: &JsObject) -> Vec<JsValue> {
    let files = match user_summary.get("files") {
        Some(JsValue::Array(files)) => files.clone(),
        _ => Vec::new(),
    };
    let identity_count = match user_summary.get("identityBackground") {
        Some(JsValue::Array(items)) => items.len(),
        _ => 0,
    };
    let mut details = vec![kv_detail(
        "user-summary",
        "User Profile",
        vec![
            ("route", JsValue::String(route.clone())),
            (
                "required",
                JsValue::from(if required { "yes" } else { "no" }),
            ),
            ("identityBackground", JsValue::Number(identity_count as f64)),
        ],
        Some(trace_i18n("trace.detail.user_profile", "User Profile")),
    )];
    if !files.is_empty() {
        details.push(list_detail(
            "user-summary-files",
            "Source Files",
            files
                .iter()
                .filter_map(JsValue::as_object)
                .map(|file| {
                    concat_js(&[
                        &string_field(file, "relativePath"),
                        &s(" | "),
                        &string_field(file, "updatedAt"),
                    ])
                })
                .collect(),
            Some(trace_i18n("trace.detail.source_files", "Source Files")),
        ));
    }
    details
}

fn shortlist_text(item: &ProjectShortlistCandidate) -> JsString {
    // TS uses `item.sourceType ?? "unknown"`: an explicit empty string is
    // rendered as empty, only a missing value falls back.
    let source_text = item
        .get("sourceType")
        .and_then(JsValue::as_string)
        .cloned()
        .unwrap_or_else(|| s("unknown"));
    let matched = string_field(item, "matchedText");
    let matched_text = if matched.is_empty() {
        s("no-match")
    } else {
        matched
    };
    let score = pilotdeck_protocol::number::number_to_string(
        item.get("score")
            .and_then(JsValue::as_number)
            .unwrap_or(0.0),
    );
    let exact = pilotdeck_protocol::number::number_to_string(
        item.get("exact")
            .and_then(JsValue::as_number)
            .unwrap_or(0.0),
    );
    concat_js(&[
        &string_field(item, "projectName"),
        &s(" | "),
        &string_field(item, "projectId"),
        &s(" | "),
        &source_text,
        &s(" | score="),
        &JsString::from(score),
        &s(" | exact="),
        &JsString::from(exact),
        &s(" | "),
        &matched_text,
    ])
}

#[allow(clippy::too_many_arguments)]
fn context_details(
    route: &JsString,
    has_user: bool,
    project_meta: Option<&ProjectMetaRecord>,
    selected_project: &Option<ReadableProjectCatalogEntry>,
    disambiguation_required: bool,
    records: &[JsObject],
    user_summary: &JsObject,
    context: &JsString,
    context_nonempty: bool,
) -> Vec<JsValue> {
    let line_count = if context.is_empty() {
        0
    } else {
        context
            .units()
            .split(|unit| *unit == u16::from(b'\n'))
            .count()
    };
    let mut details = vec![kv_detail(
        "context-rendered-summary",
        "Context Summary",
        vec![
            ("route", JsValue::String(route.clone())),
            (
                "userBaseInjected",
                JsValue::from(if has_user { "yes" } else { "no" }),
            ),
            (
                "projectMetaInjected",
                JsValue::from(if project_meta.is_some() { "yes" } else { "no" }),
            ),
            (
                "selectedProject",
                JsValue::String(
                    selected_project
                        .as_ref()
                        .map(|project| string_field(project, "projectName"))
                        .unwrap_or_default(),
                ),
            ),
            (
                "disambiguationRequired",
                JsValue::from(if disambiguation_required { "yes" } else { "no" }),
            ),
            ("fileCount", JsValue::Number(records.len() as f64)),
            ("characters", JsValue::Number(context.units().len() as f64)),
            ("lines", JsValue::Number(line_count as f64)),
        ],
        Some(trace_i18n(
            "trace.detail.context_summary",
            "Context Summary",
        )),
    )];
    if context_nonempty {
        let user_paths: Vec<JsString> = match user_summary.get("files") {
            Some(JsValue::Array(files)) => files
                .iter()
                .filter_map(JsValue::as_object)
                .map(|file| string_field(file, "relativePath"))
                .collect(),
            _ => Vec::new(),
        };
        let mut blocks: Vec<JsString> = user_paths.clone();
        if project_meta.is_some() {
            blocks.push(s("project.meta.md"));
        }
        let mut seen = Vec::new();
        for record in records {
            let path = string_field(record, "relativePath");
            if !user_paths.contains(&path) && !seen.contains(&path) {
                seen.push(path.clone());
                blocks.push(path);
            }
        }
        details.push(list_detail(
            "context-rendered-blocks",
            "Injected Blocks",
            blocks,
            Some(trace_i18n(
                "trace.detail.injected_blocks",
                "Injected Blocks",
            )),
        ));
    }
    details
}

fn recall_cache_key(
    mode: &JsString,
    query: &JsString,
    recent: &[MemoryMessage],
    snapshot: &JsString,
) -> JsString {
    let key = JsValue::Object(object(vec![
        ("mode", JsValue::String(mode.clone())),
        ("query", JsValue::String(normalize_query_key(query))),
        (
            "recent",
            JsValue::Array(
                recent
                    .iter()
                    .map(|message| JsValue::String(string_field(message, "content")))
                    .collect(),
            ),
        ),
        ("snapshot", JsValue::String(snapshot.clone())),
    ]));
    JsString::from(
        stringify_json(&key)
            .ok()
            .flatten()
            .expect("#767 recall cache key is JSON-serializable"),
    )
}

fn cache_hit_result(cached: &JsObject, mode: &JsString) -> JsObject {
    let mut result = cached.clone();
    let mut debug = match cached.get("debug").and_then(JsValue::as_object) {
        Some(debug) => debug.clone(),
        None => object(vec![
            ("mode", JsValue::from("none")),
            ("elapsedMs", JsValue::Number(0.0)),
            ("path", JsValue::String(mode.clone())),
        ]),
    };
    debug.insert(JsString::from("cacheHit"), JsValue::Bool(true));
    debug.insert(JsString::from("elapsedMs"), JsValue::Number(0.0));
    result.insert(JsString::from("debug"), JsValue::Object(debug));
    result
}

fn build_empty_result(query: &JsString, trace: &JsObject, elapsed_ms: i64) -> JsObject {
    object(vec![
        ("query", JsValue::String(query.clone())),
        ("intent", JsValue::from("none")),
        ("context", JsValue::String(JsString::default())),
        ("trace", JsValue::Object(trace.clone())),
        (
            "debug",
            JsValue::Object(object(vec![
                ("mode", JsValue::from("none")),
                ("elapsedMs", JsValue::Number(elapsed_ms as f64)),
                ("cacheHit", JsValue::Bool(false)),
                ("path", JsValue::from("explicit")),
                ("route", JsValue::from("none")),
                ("manifestCount", JsValue::Number(0.0)),
                ("selectedFileIds", JsValue::Array(Vec::new())),
            ])),
        ),
    ])
}

fn debug_field(result: &JsObject, key: &str) -> Option<JsString> {
    result
        .get("debug")
        .and_then(JsValue::as_object)
        .and_then(|debug| debug.get(key))
        .and_then(JsValue::as_string)
        .cloned()
}

fn collator() -> &'static icu_collator::CollatorBorrowed<'static> {
    static COLLATOR: OnceLock<icu_collator::CollatorBorrowed<'static>> = OnceLock::new();
    COLLATOR.get_or_init(|| {
        let locale = node_locale(|key| std::env::var(key).ok());
        node_collator(&locale).expect("#767 bundled collation data")
    })
}

fn default_random_token() -> JsString {
    // V8 `Math.random().toString(36).slice(2, 10)`: eight base-36 characters.
    // The fragment only seeds a unique trace id; the fixture injects the exact
    // TS token so the golden trace bytes stay deterministic.
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let bytes = uuid::Uuid::new_v4();
    let mut state = u64::from_le_bytes(bytes.as_bytes()[..8].try_into().expect("#767 uuid bytes"));
    let mut out = String::with_capacity(8);
    for _ in 0..8 {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        out.push(DIGITS[((state >> 33) % 36) as usize] as char);
    }
    JsString::from(out)
}
