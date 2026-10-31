//! Stateful heartbeat indexing pipeline (A6.6-edgeclaw-13).
//!
//! Ports `core/pipeline/heartbeat.ts:447-1017` on top of the already-ported
//! storage, file-memory store and index-side LLM calls:
//!
//! * `routeGeneralCandidate` (:472-548) — General-mode catalog shortlist,
//!   `selectIndexProject`, attach-or-create routing and project metadata;
//! * `captureL0Session` (:550-575) — duplicate suppression against the newest
//!   L0 row plus deterministic L0 id construction;
//! * `runHeartbeat` (:577-1015) — per-session batch flatten/focus derivation,
//!   classification, note creation, candidate routing/persistence, pipeline
//!   state, trace steps and final `markL0Indexed`.
//!
//! The module intentionally keeps the TS control flow, including the edges the
//! concurrency matrix measured:
//!
//! * `runHeartbeat` snapshots pending keys and each session's unindexed rows,
//!   so a capture during an awaited classify/create is *not* marked indexed;
//! * a `clearSession` during an awaited classify/create is re-checked before
//!   the persist (#917, human decision 2026-10-27 #914 A, diff ledger
//!   `A6.6-EDGECLAW-HEARTBEAT-CLEAR-RESURRECT-001`): the cleared session's
//!   turn is discarded instead of writing the file the TS baseline
//!   "resurrects"; the same clear before `markL0Indexed` still removes the
//!   already-written file instead;
//! * `markL0Indexed` and the final `saveIndexTrace` sit outside the per-session
//!   `try`, so a close at those points rejects `runHeartbeat` with
//!   `Error: database is not open`; a close inside the copied per-session body
//!   is caught, warned and turned into a session error first.

use super::helpers::{
    build_candidate_memory_preview, build_candidate_routing_query, build_general_project_shortlist,
    create_batch_trace, create_step, derive_focus_turns, expose_stored_relative_path,
    flatten_batch_messages, has_new_content, infer_storage_kind, json_detail, json_detail_key,
    kv_detail, messages_from_value, normalize_trigger, note_detail, preview_text, text_detail,
};
use crate::runtime::memory::edgeclaw::{
    file_memory::{EdgeClawFileStores, FileMemoryClock, FileMemoryError, MemoryFileRecord},
    llm::{
        index::{
            ClassifyTurnInput, IndexError, MemoryClassificationLabel, MemoryCreateKind,
            MemoryNoteInput, SelectIndexProjectInput,
        },
        transport::LlmMemoryExtractor,
    },
    messages::{MemoryMessage, MemoryRole},
    storage::{IndexingSettings, L0SessionRecord, StorageError},
    text,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, stringify_json};
use std::sync::Arc;

const LAST_INDEXED_AT_STATE_KEY: &str = "lastIndexedAt";
const LAST_INDEXED_CURSOR_PREFIX: &str = "lastIndexedCursor:";
const INDEX_TRACE_LIMIT: usize = 30;

/// `HeartbeatStats` (`heartbeat.ts:33-41`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HeartbeatStats {
    pub captured_sessions: usize,
    pub written_files: usize,
    pub written_user_files: usize,
    pub written_project_files: usize,
    pub written_feedback_files: usize,
    pub user_profiles_updated: usize,
    pub failed_sessions: usize,
}

impl HeartbeatStats {
    pub fn to_value(self) -> JsValue {
        let mut object = JsObject::new();
        object.insert(
            "capturedSessions".into(),
            JsValue::Number(self.captured_sessions as f64),
        );
        object.insert(
            "writtenFiles".into(),
            JsValue::Number(self.written_files as f64),
        );
        object.insert(
            "writtenUserFiles".into(),
            JsValue::Number(self.written_user_files as f64),
        );
        object.insert(
            "writtenProjectFiles".into(),
            JsValue::Number(self.written_project_files as f64),
        );
        object.insert(
            "writtenFeedbackFiles".into(),
            JsValue::Number(self.written_feedback_files as f64),
        );
        object.insert(
            "userProfilesUpdated".into(),
            JsValue::Number(self.user_profiles_updated as f64),
        );
        object.insert(
            "failedSessions".into(),
            JsValue::Number(self.failed_sessions as f64),
        );
        JsValue::Object(object)
    }
}

/// `HeartbeatRunOptions` (`heartbeat.ts:26-30`).
#[derive(Clone, Debug, Default)]
pub struct HeartbeatRunOptions {
    pub batch_size: Option<f64>,
    pub session_keys: Option<Vec<String>>,
    pub reason: Option<JsString>,
}

/// The optional `logger` seam (`heartbeat.ts:17-23`). Nothing is emitted unless
/// a logger is installed, exactly like the TS optional calls.
pub trait HeartbeatLogger: Send + Sync {
    fn info(&self, _message: &JsString) {}
    fn warn(&self, _message: &JsString) {}
}
impl HeartbeatLogger for () {}

/// Deterministic seams used by the concurrency matrix. Production callers do
/// not install a hook; the callback runs synchronously at the named point and
/// may mutate the repository (capture/clear/close) the way the matrix's real-TS
/// probe does while the pipeline is awaiting an LLM call.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeartbeatStage {
    AfterListUnindexed,
    BeforeClassify,
    BeforeCreateNote,
    BeforeSelectIndexProject,
    BeforeMarkL0Indexed,
}

#[doc(hidden)]
pub type HeartbeatHook = Arc<dyn Fn(HeartbeatStage, &str) + Send + Sync>;

/// `HeartbeatOptions` (`heartbeat.ts:16-23`).
pub struct HeartbeatOptions {
    pub batch_size: Option<f64>,
    pub source: Option<String>,
    pub settings: IndexingSettings,
    pub logger: Option<Arc<dyn HeartbeatLogger>>,
    /// Injected `nowIso()`; production uses [text::now_iso].
    pub clock: Option<FileMemoryClock>,
}

impl HeartbeatOptions {
    pub fn new(settings: IndexingSettings) -> Self {
        Self {
            batch_size: None,
            source: None,
            settings,
            logger: None,
            clock: None,
        }
    }
}

/// `HeartbeatIndexer` (`heartbeat.ts:447-470`).
#[derive(Clone)]
pub struct HeartbeatIndexer {
    repository: Arc<EdgeClawFileStores>,
    extractor: Arc<LlmMemoryExtractor>,
    batch_size: f64,
    source: String,
    settings: IndexingSettings,
    logger: Option<Arc<dyn HeartbeatLogger>>,
    clock: FileMemoryClock,
    hook: Option<HeartbeatHook>,
}

impl HeartbeatIndexer {
    pub(crate) fn configured_batch_size(&self) -> f64 {
        self.batch_size
    }

    pub fn new(
        repository: Arc<EdgeClawFileStores>,
        extractor: Arc<LlmMemoryExtractor>,
        options: HeartbeatOptions,
    ) -> Self {
        Self {
            repository,
            extractor,
            batch_size: options.batch_size.unwrap_or(30.0),
            source: options.source.unwrap_or_else(|| "openclaw".into()),
            settings: options.settings,
            logger: options.logger,
            clock: options.clock.unwrap_or_else(|| Arc::new(text::now_iso)),
            hook: None,
        }
    }

    #[doc(hidden)]
    pub fn with_hook(mut self, hook: HeartbeatHook) -> Self {
        self.hook = Some(hook);
        self
    }

    pub fn get_settings(&self) -> IndexingSettings {
        self.settings.clone()
    }

    pub fn set_settings(&mut self, settings: IndexingSettings) {
        self.settings = settings;
    }

    pub fn repository(&self) -> &Arc<EdgeClawFileStores> {
        &self.repository
    }

    fn fire(&self, stage: HeartbeatStage, session_key: &str) {
        if let Some(hook) = &self.hook {
            hook(stage, session_key);
        }
    }

    fn warn(&self, message: JsString) {
        if let Some(logger) = &self.logger {
            logger.warn(&message);
        }
    }

    fn info(&self, message: JsString) {
        if let Some(logger) = &self.logger {
            logger.info(&message);
        }
    }

    // -- captureL0Session (`heartbeat.ts:550-575`) -------------------------

    pub fn capture_l0_session(
        &self,
        input: CaptureL0SessionInput,
    ) -> Result<Option<L0SessionRecord>, HeartbeatError> {
        let timestamp = input.timestamp.unwrap_or_else(|| (self.clock)());
        let payload = stringify_json(&JsValue::Array(
            input.messages.iter().map(MemoryMessage::to_value).collect(),
        ))
        .map_err(|error| HeartbeatError::Json(error.to_string()))?
        .ok_or_else(|| HeartbeatError::Json("messages are not serializable".into()))?;
        let timestamp_js = JsString::from(timestamp.as_str());
        let payload_js = JsString::from(payload);
        let l0_index_id = text::build_l0_index_id(
            &JsString::from(input.session_key.as_str()),
            &timestamp_js,
            &payload_js,
        );
        let record = L0SessionRecord {
            l0_index_id: l0_index_id.to_utf8().unwrap_or_default(),
            session_key: input.session_key,
            timestamp,
            messages: JsValue::Array(input.messages.iter().map(MemoryMessage::to_value).collect()),
            source: input.source.unwrap_or_else(|| self.source.clone()),
            indexed: false,
            created_at: (self.clock)(),
        };
        // The read-check-insert pair must share one connection guard: TS is
        // safe because `captureL0Session` has no `await`, but this indexer is
        // shared across threads (review P1-1).
        let inserted = self
            .repository
            .storage
            .insert_l0_session_if_new(&record, |recent| {
                recent.session_key == record.session_key
                    && !has_new_content(&messages_from_value(&recent.messages), &input.messages)
            })?;
        if !inserted {
            self.info(JsString::from(format!(
                "[clawxmemory] skip duplicate l0 capture for session={}",
                record.session_key
            )));
            return Ok(None);
        }
        Ok(Some(record))
    }

    // -- runHeartbeat (`heartbeat.ts:577-1015`) ----------------------------

    pub async fn run_heartbeat(
        &self,
        options: HeartbeatRunOptions,
    ) -> Result<HeartbeatStats, HeartbeatError> {
        let mut stats = HeartbeatStats::default();
        let session_keys = self.repository.storage.list_pending_session_keys(
            if crate::mutants::on("native-indexer-batch-truncated") {
                pilotdeck_protocol::js_math::max(1.0, options.batch_size.unwrap_or(self.batch_size))
                    .trunc()
            } else if crate::mutants::on("native-indexer-override-ignored") {
                pilotdeck_protocol::js_math::max(1.0, self.batch_size)
            } else {
                pilotdeck_protocol::js_math::max(1.0, options.batch_size.unwrap_or(self.batch_size))
            },
            options.session_keys.as_deref(),
        )?;
        if session_keys.is_empty() {
            return Ok(stats);
        }

        for session_key in session_keys {
            let sessions = self
                .repository
                .storage
                .list_unindexed_l0_by_session(&session_key)?;
            self.fire(HeartbeatStage::AfterListUnindexed, &session_key);
            if sessions.is_empty() {
                continue;
            }

            let first = &sessions[0];
            let previous_indexed = self.repository.storage.get_latest_l0_before(
                &session_key,
                &first.timestamp,
                &first.created_at,
            )?;
            let previous_messages = previous_indexed
                .as_ref()
                .map(|session| messages_from_value(&session.messages))
                .unwrap_or_default();
            let focus_turns_by_session = derive_focus_turns(&previous_messages, &sessions);
            let batch_context_messages = flatten_batch_messages(&sessions, &previous_messages);
            let focus_user_turn_count = focus_turns_by_session
                .iter()
                .map(|(_, turns)| turns.len())
                .sum::<usize>();
            let mut trace = create_batch_trace(
                &JsString::from(session_key.as_str()),
                &sessions,
                normalize_trigger(options.reason.as_ref()),
                focus_user_turn_count,
                &JsString::from((self.clock)()),
            );
            let trigger = field_str(&trace, "trigger");
            create_step(
                &mut trace,
                "index_start",
                &"Index Started".into(),
                "info",
                &JsString::from(format!("trigger={trigger}")),
                &JsString::from(format!("Preparing batch indexing for {session_key}.")),
                &object(vec![
                    (
                        "titleI18n",
                        trace_i18n("trace.step.index_start", "Index Started", &[]),
                    ),
                    (
                        "outputSummaryI18n",
                        trace_i18n(
                            "trace.text.index_start.output.preparing_batch",
                            "Preparing batch indexing for {0}.",
                            &[JsValue::String(JsString::from(session_key.as_str()))],
                        ),
                    ),
                ]),
            );
            let batch_summary = trace
                .get("batchSummary")
                .and_then(JsValue::as_object)
                .cloned()
                .unwrap_or_default();
            let segment_count = batch_summary
                .get("segmentCount")
                .and_then(JsValue::as_number)
                .unwrap_or(0.0) as usize;
            let from_timestamp = field_str_value(&batch_summary, "fromTimestamp");
            let to_timestamp = field_str_value(&batch_summary, "toTimestamp");
            let from_display = if from_timestamp.is_empty() {
                "n/a".to_string()
            } else {
                utf8_lossy(&from_timestamp)
            };
            let to_display = if to_timestamp.is_empty() {
                "n/a".to_string()
            } else {
                utf8_lossy(&to_timestamp)
            };
            let l0_ids = batch_summary
                .get("l0Ids")
                .and_then(as_items)
                .map(|ids| {
                    ids.iter()
                        .filter_map(JsValue::as_string)
                        .cloned()
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let l0_ids_joined = text::join_text_refs(l0_ids.iter(), ", ");
            create_step(
                &mut trace,
                "batch_loaded",
                &"Batch Loaded".into(),
                "info",
                &JsString::from(format!(
                    "{segment_count} segments from {from_display} to {to_display}"
                )),
                &JsString::from(format!(
                    "{} messages loaded into batch context.",
                    batch_context_messages.len()
                )),
                &object(vec![
                    (
                        "titleI18n",
                        trace_i18n("trace.step.batch_loaded", "Batch Loaded", &[]),
                    ),
                    (
                        "inputSummaryI18n",
                        trace_i18n(
                            "trace.text.batch_loaded.input",
                            "{0} segments from {1} to {2}",
                            &[
                                JsValue::Number(segment_count as f64),
                                JsValue::String(JsString::from(from_display.as_str())),
                                JsValue::String(JsString::from(to_display.as_str())),
                            ],
                        ),
                    ),
                    (
                        "outputSummaryI18n",
                        trace_i18n(
                            "trace.text.batch_loaded.output",
                            "{0} messages loaded into batch context.",
                            &[JsValue::Number(batch_context_messages.len() as f64)],
                        ),
                    ),
                    (
                        "metrics",
                        value(vec![
                            ("segmentCount", JsValue::Number(segment_count as f64)),
                            (
                                "focusUserTurnCount",
                                JsValue::Number(focus_user_turn_count as f64),
                            ),
                        ]),
                    ),
                    (
                        "details",
                        JsValue::Array(vec![
                            kv_detail(
                                "batch-summary",
                                "Batch Summary",
                                &[
                                    value(vec![
                                        ("label", JsValue::String("sessionKey".into())),
                                        (
                                            "value",
                                            JsValue::String(JsString::from(session_key.as_str())),
                                        ),
                                    ]),
                                    value(vec![
                                        ("label", JsValue::String("from".into())),
                                        ("value", JsValue::String(from_timestamp.clone())),
                                    ]),
                                    value(vec![
                                        ("label", JsValue::String("to".into())),
                                        ("value", JsValue::String(to_timestamp.clone())),
                                    ]),
                                    value(vec![
                                        ("label", JsValue::String("l0Ids".into())),
                                        (
                                            "value",
                                            JsValue::String(if l0_ids_joined.is_empty() {
                                                "none".into()
                                            } else {
                                                l0_ids_joined.clone()
                                            }),
                                        ),
                                    ]),
                                ],
                                Some(&trace_i18n(
                                    "trace.detail.batch_summary",
                                    "Batch Summary",
                                    &[],
                                )),
                            )
                            .expect("#770 batch summary detail"),
                            json_detail(
                                "batch-context",
                                "Batch Context",
                                &JsValue::Array(
                                    batch_context_messages
                                        .iter()
                                        .enumerate()
                                        .map(|(index, message)| {
                                            value(vec![
                                                ("index", JsValue::Number(index as f64)),
                                                ("role", JsValue::String(message.role.as_string())),
                                                (
                                                    "content",
                                                    JsValue::String(message.content.clone()),
                                                ),
                                            ])
                                        })
                                        .collect(),
                                ),
                                Some(&trace_i18n(
                                    "trace.detail.batch_context",
                                    "Batch Context",
                                    &[],
                                )),
                            ),
                        ]),
                    ),
                ]),
            );
            let assistant_count = batch_context_messages
                .iter()
                .filter(|message| message.role == MemoryRole::Assistant)
                .count();
            let focus_status = if focus_user_turn_count > 0 {
                "success"
            } else {
                "warning"
            };
            let mut focus_details = vec![
                kv_detail(
                    "focus-turn-selection-summary",
                    "Focus Selection Summary",
                    &[
                        value(vec![
                            ("label", JsValue::String("userTurns".into())),
                            (
                                "value",
                                JsValue::String(JsString::from(
                                    focus_user_turn_count.to_string().as_str(),
                                )),
                            ),
                        ]),
                        value(vec![
                            (
                                "label",
                                JsValue::String("assistantMessagesInContext".into()),
                            ),
                            (
                                "value",
                                JsValue::String(JsString::from(
                                    assistant_count.to_string().as_str(),
                                )),
                            ),
                        ]),
                        value(vec![
                            (
                                "label",
                                JsValue::String("assistantUsedAsContextOnly".into()),
                            ),
                            ("value", JsValue::String("yes".into())),
                        ]),
                    ],
                    Some(&trace_i18n(
                        "trace.detail.focus_selection_summary",
                        "Focus Selection Summary",
                        &[],
                    )),
                )
                .expect("#770 focus selection detail"),
            ];
            for (index, message) in sessions
                .iter()
                .flat_map(|session| {
                    focus_turns_by_session
                        .iter()
                        .find(|(id, _)| id == &session.l0_index_id)
                        .map(|(_, turns)| turns.iter())
                        .into_iter()
                        .flatten()
                })
                .enumerate()
            {
                focus_details.push(text_detail(
                    &format!("focus-turn-{}", index + 1),
                    &format!("Focus Turn {}", index + 1),
                    &message.content,
                    Some(&trace_i18n(
                        "trace.detail.focus_turn",
                        "Focus Turn {0}",
                        &[JsValue::Number((index + 1) as f64)],
                    )),
                ));
            }
            create_step(
                &mut trace,
                "focus_turns_selected",
                &"Focus Turns Selected".into(),
                focus_status,
                &JsString::from(format!("{focus_user_turn_count} user turns in this batch.")),
                &JsString::from(if focus_user_turn_count > 0 {
                    "User turns will be classified one by one."
                } else {
                    "No user turns found; this batch will be marked indexed without storing memory."
                }),
                &object(vec![
                    (
                        "titleI18n",
                        trace_i18n(
                            "trace.step.focus_turns_selected",
                            "Focus Turns Selected",
                            &[],
                        ),
                    ),
                    (
                        "inputSummaryI18n",
                        trace_i18n(
                            "trace.text.focus_turns_selected.input",
                            "{0} user turns in this batch.",
                            &[JsValue::Number(focus_user_turn_count as f64)],
                        ),
                    ),
                    (
                        "outputSummaryI18n",
                        if focus_user_turn_count > 0 {
                            trace_i18n(
                                "trace.text.focus_turns_selected.output.classifying",
                                "User turns will be classified one by one.",
                                &[],
                            )
                        } else {
                            trace_i18n(
                                "trace.text.focus_turns_selected.output.no_user_turns",
                                "No user turns found; this batch will be marked indexed without storing memory.",
                                &[],
                            )
                        },
                    ),
                    ("details", JsValue::Array(focus_details)),
                ]),
            );
            self.repository
                .storage
                .save_index_trace(&JsValue::Object(trace.clone()), INDEX_TRACE_LIMIT)?;

            let mut processed_ids: Vec<String> = Vec::new();
            let mut session_had_error = false;
            for session in &sessions {
                let focus_user_turns = focus_turns_by_session
                    .iter()
                    .find(|(id, _)| id == &session.l0_index_id)
                    .map(|(_, turns)| turns.clone())
                    .unwrap_or_default();
                let outcome = self
                    .process_session(
                        session,
                        &focus_user_turns,
                        &batch_context_messages,
                        &mut trace,
                        &mut stats,
                        &mut processed_ids,
                    )
                    .await;
                if let Err(error) = outcome {
                    stats.failed_sessions += 1;
                    session_had_error = true;
                    let message = error.message();
                    let options = object(vec![
                        (
                            "titleI18n",
                            trace_i18n("trace.text.index_error.title", "Index Error", &[]),
                        ),
                        (
                            "details",
                            JsValue::Array(vec![note_detail(
                                &format!("index-error-{}", session.l0_index_id),
                                "Index Error",
                                &message,
                                Some(&trace_i18n("trace.detail.index_error", "Index Error", &[])),
                            )]),
                        ),
                    ]);
                    create_step(
                        &mut trace,
                        "index_finished",
                        &"Index Error".into(),
                        "error",
                        &JsString::from(session.l0_index_id.as_str()),
                        &message,
                        &options,
                    );
                    self.warn(JsString::from(format!(
                        "[clawxmemory] heartbeat file-memory extraction failed for {}: {}",
                        session.l0_index_id,
                        utf8_lossy(&error.js_string())
                    )));
                }
            }

            if !processed_ids.is_empty() {
                self.fire(HeartbeatStage::BeforeMarkL0Indexed, &session_key);
                self.repository.storage.mark_l0_indexed(&processed_ids)?;
            }
            let finished_at = (self.clock)();
            trace.insert(
                "finishedAt".into(),
                JsValue::String(JsString::from(finished_at)),
            );
            trace.insert(
                "status".into(),
                JsValue::String(if session_had_error {
                    "error".into()
                } else {
                    "completed".into()
                }),
            );
            let stored_results = trace
                .get("storedResults")
                .and_then(as_items)
                .map_or(0, <[JsValue]>::len);
            trace.insert("isNoOp".into(), JsValue::Bool(stored_results == 0));
            trace.insert(
                "displayStatus".into(),
                JsValue::String(
                    if session_had_error {
                        "Error"
                    } else if stored_results == 0 {
                        "No-op"
                    } else {
                        "Completed"
                    }
                    .into(),
                ),
            );
            let stored_results_value = trace
                .get("storedResults")
                .cloned()
                .unwrap_or(JsValue::Array(Vec::new()));
            create_step(
                &mut trace,
                "index_finished",
                &"Index Finished".into(),
                if session_had_error {
                    "warning"
                } else {
                    "success"
                },
                &JsString::from(format!("segments={segment_count}")),
                &JsString::from(format!(
                    "stored={}, failed={}",
                    stored_results,
                    usize::from(session_had_error)
                )),
                &object(vec![
                    (
                        "titleI18n",
                        trace_i18n("trace.step.index_finished", "Index Finished", &[]),
                    ),
                    (
                        "metrics",
                        value(vec![
                            ("storedResults", JsValue::Number(stored_results as f64)),
                            (
                                "failed",
                                JsValue::Number(f64::from(u8::from(session_had_error))),
                            ),
                        ]),
                    ),
                    (
                        "details",
                        JsValue::Array(vec![json_detail(
                            "stored-results",
                            "Stored Results",
                            &stored_results_value,
                            Some(&trace_i18n(
                                "trace.detail.stored_results",
                                "Stored Results",
                                &[],
                            )),
                        )]),
                    ),
                ]),
            );
            self.repository
                .storage
                .save_index_trace(&JsValue::Object(trace), INDEX_TRACE_LIMIT)?;
        }
        Ok(stats)
    }

    #[allow(clippy::too_many_arguments)]
    async fn process_session(
        &self,
        session: &L0SessionRecord,
        focus_user_turns: &[MemoryMessage],
        batch_context_messages: &[MemoryMessage],
        trace: &mut JsObject,
        stats: &mut HeartbeatStats,
        processed_ids: &mut Vec<String>,
    ) -> Result<(), HeartbeatError> {
        let store = self.repository.get_file_memory_store();
        if focus_user_turns.is_empty() {
            processed_ids.push(session.l0_index_id.clone());
            stats.captured_sessions += 1;
            return Ok(());
        }

        for focus_turn in focus_user_turns {
            let current_project_meta = match self
                .repository
                .get_project_meta()
                .map_err(HeartbeatError::File)?
            {
                JsValue::Undefined | JsValue::Null => {
                    match store
                        .get_project_meta(&JsObject::new())
                        .map_err(HeartbeatError::File)?
                    {
                        JsValue::Undefined | JsValue::Null => None,
                        value => Some(value),
                    }
                }
                value => Some(value),
            };
            self.fire(HeartbeatStage::BeforeClassify, &session.session_key);
            let mut classification_prompt_debug: Option<JsValue> = None;
            let classification = self
                .extractor
                .classify_memory_turn(
                    &ClassifyTurnInput {
                        timestamp: JsString::from(session.timestamp.as_str()),
                        session_key: Some(JsString::from(session.session_key.as_str())),
                        focus_user_turn: focus_turn,
                        batch_context_messages,
                        current_project_meta: current_project_meta.as_ref(),
                        agent_id: None,
                        timeout_ms: None,
                    },
                    |debug| classification_prompt_debug = Some(debug),
                )
                .await;
            let labels = if classification.should_store {
                classification.labels
            } else {
                Vec::new()
            };
            let mut classification_options = object(vec![
                (
                    "refs",
                    value(vec![(
                        "classification",
                        JsValue::Array(if labels.is_empty() {
                            vec![JsValue::String("none".into())]
                        } else {
                            labels
                                .iter()
                                .map(|label| JsValue::String(JsString::from(label.kind.as_str())))
                                .collect()
                        }),
                    )]),
                ),
                (
                    "details",
                    JsValue::Array(vec![
                        text_detail(
                            &format!("focus-turn-text-{}", session.l0_index_id),
                            "Focus User Turn",
                            &focus_turn.content,
                            Some(&trace_i18n(
                                "trace.detail.focus_user_turn",
                                "Focus User Turn",
                                &[],
                            )),
                        ),
                        kv_detail(
                            &format!("classification-result-{}", session.l0_index_id),
                            "Classification Result",
                            &[
                                value(vec![
                                    ("label", JsValue::String("sessionKey".into())),
                                    (
                                        "value",
                                        JsValue::String(JsString::from(
                                            session.session_key.as_str(),
                                        )),
                                    ),
                                ]),
                                value(vec![
                                    ("label", JsValue::String("timestamp".into())),
                                    (
                                        "value",
                                        JsValue::String(JsString::from(session.timestamp.as_str())),
                                    ),
                                ]),
                                value(vec![
                                    ("label", JsValue::String("result".into())),
                                    (
                                        "value",
                                        JsValue::String(if labels.is_empty() {
                                            "none".into()
                                        } else {
                                            JsString::from(
                                                labels
                                                    .iter()
                                                    .map(|label| label.kind.as_str())
                                                    .collect::<Vec<_>>()
                                                    .join(", "),
                                            )
                                        }),
                                    ),
                                ]),
                            ],
                            Some(&trace_i18n(
                                "trace.detail.classification_result",
                                "Classification Result",
                                &[],
                            )),
                        )
                        .expect("#770 classification result detail"),
                        json_detail(
                            &format!("classification-labels-{}", session.l0_index_id),
                            "Classification Labels",
                            &JsValue::Array(labels.iter().map(label_value).collect()),
                            Some(&trace_i18n(
                                "trace.detail.classifier_candidates",
                                "Classifier Candidates",
                                &[],
                            )),
                        ),
                    ]),
                ),
            ]);
            if let Some(debug) = classification_prompt_debug {
                classification_options.insert("promptDebug".into(), debug);
            }
            create_step(
                trace,
                "classification",
                &"Classification".into(),
                if labels.is_empty() {
                    "skipped"
                } else {
                    "success"
                },
                &preview_text(&focus_turn.content, 220),
                &JsString::from(if labels.is_empty() {
                    "classified=none".to_string()
                } else {
                    format!(
                        "classified={}",
                        labels
                            .iter()
                            .map(|label| label.kind.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                }),
                &classification_options,
            );

            let mut created_candidates: Vec<(MemoryClassificationLabel, Option<JsObject>)> =
                Vec::new();
            for label in &labels {
                self.fire(HeartbeatStage::BeforeCreateNote, &session.session_key);
                let mut create_prompt_debug: Option<JsValue> = None;
                let candidate = match label.kind {
                    MemoryCreateKind::User => {
                        self.extractor
                            .create_user_memory_note(
                                &MemoryNoteInput {
                                    timestamp: JsString::from(session.timestamp.as_str()),
                                    session_key: Some(JsString::from(session.session_key.as_str())),
                                    focus_user_turn: focus_turn,
                                    batch_context_messages,
                                    current_project_meta: current_project_meta.as_ref(),
                                    classification: label,
                                    agent_id: None,
                                    timeout_ms: None,
                                },
                                |debug| create_prompt_debug = Some(debug),
                            )
                            .await?
                    }
                    MemoryCreateKind::Project => {
                        self.extractor
                            .create_project_memory_note(
                                &MemoryNoteInput {
                                    timestamp: JsString::from(session.timestamp.as_str()),
                                    session_key: Some(JsString::from(session.session_key.as_str())),
                                    focus_user_turn: focus_turn,
                                    batch_context_messages,
                                    current_project_meta: current_project_meta.as_ref(),
                                    classification: label,
                                    agent_id: None,
                                    timeout_ms: None,
                                },
                                |debug| create_prompt_debug = Some(debug),
                            )
                            .await?
                    }
                    MemoryCreateKind::Feedback => {
                        self.extractor
                            .create_feedback_memory_note(
                                &MemoryNoteInput {
                                    timestamp: JsString::from(session.timestamp.as_str()),
                                    session_key: Some(JsString::from(session.session_key.as_str())),
                                    focus_user_turn: focus_turn,
                                    batch_context_messages,
                                    current_project_meta: current_project_meta.as_ref(),
                                    classification: label,
                                    agent_id: None,
                                    timeout_ms: None,
                                },
                                |debug| create_prompt_debug = Some(debug),
                            )
                            .await?
                    }
                };
                let kind = label.kind;
                let candidate_detail = match &candidate {
                    Some(candidate) => value(vec![
                        (
                            "type",
                            JsValue::String(
                                candidate
                                    .get("type")
                                    .and_then(JsValue::as_string)
                                    .cloned()
                                    .unwrap_or_else(|| kind.as_str().into()),
                            ),
                        ),
                        (
                            "name",
                            JsValue::String(
                                candidate
                                    .get("name")
                                    .and_then(JsValue::as_string)
                                    .cloned()
                                    .unwrap_or_default(),
                            ),
                        ),
                        (
                            "description",
                            JsValue::String(
                                candidate
                                    .get("description")
                                    .and_then(JsValue::as_string)
                                    .cloned()
                                    .unwrap_or_default(),
                            ),
                        ),
                        (
                            "body",
                            JsValue::String(
                                candidate
                                    .get("body")
                                    .and_then(JsValue::as_string)
                                    .cloned()
                                    .unwrap_or_default(),
                            ),
                        ),
                    ]),
                    None => JsValue::Null,
                };
                let mut options = object(vec![
                    (
                        "refs",
                        value(vec![(
                            "candidateType",
                            JsValue::String(JsString::from(kind.as_str())),
                        )]),
                    ),
                    (
                        "details",
                        JsValue::Array(vec![json_detail(
                            &format!("create-{}-{}", kind.as_str(), session.l0_index_id),
                            &format!("{} Create Result", kind.as_str()),
                            &value(vec![
                                ("classification", label_value(label)),
                                ("candidate", candidate_detail),
                            ]),
                            None,
                        )]),
                    ),
                ]);
                if let Some(debug) = create_prompt_debug {
                    options.insert("promptDebug".into(), debug);
                }
                let candidate_label = candidate.as_ref().map(|candidate| {
                    candidate
                        .get("name")
                        .and_then(JsValue::as_string)
                        .cloned()
                        .unwrap_or_default()
                });
                let reason = if label.reason.is_empty() {
                    JsString::from("no explicit reason")
                } else {
                    label.reason.clone()
                };
                let input_summary = text::join_text_refs(
                    [
                        &JsString::from(kind.as_str()),
                        &JsString::from(" | "),
                        &reason,
                    ],
                    "",
                );
                let output_summary = match &candidate_label {
                    Some(name) => text::join_text_refs([&JsString::from("created="), name], ""),
                    None => JsString::from(format!("skipped={}", kind.as_str())),
                };
                create_step(
                    trace,
                    match kind {
                        MemoryCreateKind::User => "user_create",
                        MemoryCreateKind::Project => "project_create",
                        MemoryCreateKind::Feedback => "feedback_create",
                    },
                    &JsString::from(format!("{} Create", kind.as_str())),
                    if candidate_label.is_some() {
                        "success"
                    } else {
                        "skipped"
                    },
                    &input_summary,
                    &output_summary,
                    &options,
                );
                created_candidates.push((label.clone(), candidate));
            }

            // #917 (human decision 2026-10-27, #914 A; diff ledger
            // `A6.6-EDGECLAW-HEARTBEAT-CLEAR-RESURRECT-001`): a `clearSession`
            // that lands while the classify/createNote awaits above are in
            // flight deletes this session's L0 rows before the persist below
            // runs. The TS baseline still writes the turn's file ("file
            // resurrection"); the registered divergence discards the turn's
            // candidates instead, so a cleared session's memory never reaches
            // the file stores. The lookup deliberately fails open: a close
            // during the same awaits must keep failing at the same calls TS
            // fails at, or the close matrix cells would drift.
            let session_cleared = match self
                .repository
                .storage
                .get_l0_by_ids(std::slice::from_ref(&session.l0_index_id))
            {
                Ok(rows) => rows.is_empty(),
                // Fail open, per the comment above.
                Err(_) => false,
            };

            let mut persisted_records: Vec<MemoryFileRecord> = Vec::new();
            let mut wrote_global_user_note = false;
            for (label, candidate) in &created_candidates {
                if session_cleared {
                    self.info(JsString::from(format!(
                        "[clawxmemory] skip persist for cleared session={}",
                        session.session_key
                    )));
                    break;
                }
                let Some(candidate) = candidate else {
                    continue;
                };
                let routed = self
                    .route_general_candidate(
                        candidate,
                        focus_turn,
                        batch_context_messages,
                        &session.session_key,
                    )
                    .await?;
                if let Some(meta) = &routed.created_project_meta {
                    self.push_stored_result(
                        trace,
                        value(vec![
                            (
                                "candidateType",
                                JsValue::String("general_project_meta".into()),
                            ),
                            (
                                "candidateName",
                                JsValue::String(
                                    meta.get("projectName")
                                        .and_then(JsValue::as_string)
                                        .cloned()
                                        .unwrap_or_default(),
                                ),
                            ),
                            ("scope", JsValue::String("project".into())),
                            (
                                "projectId",
                                JsValue::String(
                                    meta.get("projectId")
                                        .and_then(JsValue::as_string)
                                        .cloned()
                                        .unwrap_or_default(),
                                ),
                            ),
                            (
                                "sourceKind",
                                JsValue::String(
                                    routed
                                        .selected_project
                                        .as_ref()
                                        .and_then(|project| project.get("sourceType"))
                                        .and_then(JsValue::as_string)
                                        .cloned()
                                        .or_else(|| {
                                            meta.get("sourceKind")
                                                .and_then(JsValue::as_string)
                                                .cloned()
                                        })
                                        .unwrap_or_else(|| "general_local".into()),
                                ),
                            ),
                            (
                                "relativePath",
                                JsValue::String(
                                    meta.get("relativePath")
                                        .and_then(JsValue::as_string)
                                        .cloned()
                                        .unwrap_or_default(),
                                ),
                            ),
                            (
                                "storageKind",
                                JsValue::String("general_project_meta".into()),
                            ),
                        ]),
                    );
                    stats.written_files += 1;
                    let mut options = object(vec![(
                        "details",
                        JsValue::Array(vec![json_detail(
                            &format!(
                                "project-route-{}-{}",
                                session.l0_index_id,
                                stored_results_len(trace)
                            ),
                            "Project Route",
                            &value(vec![
                                (
                                    "decision",
                                    JsValue::String(
                                        routed.routing_decision.unwrap_or("create_new").into(),
                                    ),
                                ),
                                (
                                    "reason",
                                    routed
                                        .routing_reason
                                        .clone()
                                        .map(JsValue::String)
                                        .unwrap_or(JsValue::Null),
                                ),
                                ("candidateType", JsValue::String(label.kind.as_str().into())),
                                (
                                    "candidateName",
                                    JsValue::String(
                                        candidate
                                            .get("name")
                                            .and_then(JsValue::as_string)
                                            .cloned()
                                            .unwrap_or_default(),
                                    ),
                                ),
                                (
                                    "selectedProject",
                                    routed
                                        .selected_project
                                        .as_ref()
                                        .and_then(|project| project.get("projectName"))
                                        .and_then(JsValue::as_string)
                                        .cloned()
                                        .map(JsValue::String)
                                        .unwrap_or(JsValue::Null),
                                ),
                                (
                                    "assignedProjectId",
                                    JsValue::String(
                                        meta.get("projectId")
                                            .and_then(JsValue::as_string)
                                            .cloned()
                                            .unwrap_or_default(),
                                    ),
                                ),
                                (
                                    "createdProjectMeta",
                                    JsValue::String(
                                        meta.get("relativePath")
                                            .and_then(JsValue::as_string)
                                            .cloned()
                                            .unwrap_or_default(),
                                    ),
                                ),
                            ]),
                            None,
                        )]),
                    )]);
                    if let Some(debug) = routed.selection_debug.clone() {
                        options.insert("promptDebug".into(), debug);
                    }
                    create_step(
                        trace,
                        "project_routed",
                        &"Project Routed".into(),
                        "success",
                        &preview_text(&focus_turn.content, 180),
                        &text::join_text_refs(
                            [
                                &JsString::from("Created new General project "),
                                meta.get("projectName")
                                    .and_then(JsValue::as_string)
                                    .unwrap_or(&JsString::default()),
                                &JsString::from("."),
                            ],
                            "",
                        ),
                        &options,
                    );
                } else if let Some(project) = &routed.selected_project {
                    let route_key = text::join_text_refs(
                        [
                            &JsString::from(format!("project-route-{}-", session.l0_index_id)),
                            candidate
                                .get("name")
                                .and_then(JsValue::as_string)
                                .unwrap_or(&JsString::default()),
                        ],
                        "",
                    );
                    let mut options = object(vec![(
                        "details",
                        JsValue::Array(vec![json_detail_key(
                            &route_key,
                            "Project Route",
                            &value(vec![
                                (
                                    "decision",
                                    JsValue::String(
                                        routed.routing_decision.unwrap_or("attach_existing").into(),
                                    ),
                                ),
                                (
                                    "reason",
                                    routed
                                        .routing_reason
                                        .clone()
                                        .map(JsValue::String)
                                        .unwrap_or(JsValue::Null),
                                ),
                                ("candidateType", JsValue::String(label.kind.as_str().into())),
                                (
                                    "candidateName",
                                    JsValue::String(
                                        candidate
                                            .get("name")
                                            .and_then(JsValue::as_string)
                                            .cloned()
                                            .unwrap_or_default(),
                                    ),
                                ),
                                (
                                    "selectedProject",
                                    JsValue::String(
                                        project
                                            .get("projectName")
                                            .and_then(JsValue::as_string)
                                            .cloned()
                                            .unwrap_or_default(),
                                    ),
                                ),
                                (
                                    "selectedSource",
                                    JsValue::String(
                                        if project
                                            .get("sourceType")
                                            .and_then(JsValue::as_string)
                                            .is_some_and(|source| {
                                                source.equals_str("workspace_external")
                                            })
                                        {
                                            "workspace_external"
                                        } else {
                                            "general_local"
                                        }
                                        .into(),
                                    ),
                                ),
                                (
                                    "assignedProjectId",
                                    JsValue::String(
                                        project
                                            .get("projectId")
                                            .and_then(JsValue::as_string)
                                            .cloned()
                                            .unwrap_or_default(),
                                    ),
                                ),
                            ]),
                            None,
                        )]),
                    )]);
                    if let Some(debug) = routed.selection_debug.clone() {
                        options.insert("promptDebug".into(), debug);
                    }
                    create_step(
                        trace,
                        "project_routed",
                        &"Project Routed".into(),
                        "success",
                        &preview_text(&focus_turn.content, 180),
                        &text::join_text_refs(
                            [
                                &JsString::from("Assigned to "),
                                project
                                    .get("projectName")
                                    .and_then(JsValue::as_string)
                                    .unwrap_or(&JsString::default()),
                                &JsString::from("."),
                            ],
                            "",
                        ),
                        &options,
                    );
                }
                let target_store = if routed.candidate_type_is_user() {
                    self.repository.get_global_user_store()
                } else {
                    store
                };
                let record = target_store.upsert_candidate(&routed.candidate)?;
                if routed.candidate_type_is_user() {
                    wrote_global_user_note = true;
                }
                persisted_records.push(record.as_object().cloned().unwrap_or_default());
                let mut stored = object(vec![
                    (
                        "candidateType",
                        JsValue::String(
                            routed
                                .candidate
                                .get("type")
                                .and_then(JsValue::as_string)
                                .cloned()
                                .unwrap_or_default(),
                        ),
                    ),
                    (
                        "candidateName",
                        JsValue::String(
                            routed
                                .candidate
                                .get("name")
                                .and_then(JsValue::as_string)
                                .cloned()
                                .unwrap_or_default(),
                        ),
                    ),
                    (
                        "scope",
                        JsValue::String(
                            routed
                                .candidate
                                .get("scope")
                                .and_then(JsValue::as_string)
                                .cloned()
                                .unwrap_or_default(),
                        ),
                    ),
                ]);
                if let Some(project_id) = record
                    .get("projectId")
                    .and_then(JsValue::as_string)
                    .filter(|project_id| !project_id.is_empty())
                {
                    stored.insert("projectId".into(), JsValue::String(project_id.clone()));
                }
                stored.insert(
                    "relativePath".into(),
                    JsValue::String(expose_stored_relative_path(
                        record
                            .get("scope")
                            .and_then(JsValue::as_string)
                            .unwrap_or(&JsString::default()),
                        record
                            .get("relativePath")
                            .and_then(JsValue::as_string)
                            .unwrap_or(&JsString::default()),
                    )),
                );
                stored.insert(
                    "storageKind".into(),
                    JsValue::String(
                        infer_storage_kind(
                            record
                                .get("type")
                                .and_then(JsValue::as_string)
                                .unwrap_or(&JsString::default()),
                            record
                                .get("relativePath")
                                .and_then(JsValue::as_string)
                                .unwrap_or(&JsString::default()),
                        )
                        .into(),
                    ),
                );
                push_stored_result(trace, JsValue::Object(stored));
                stats.written_files += 1;
                match routed.candidate.get("type").and_then(JsValue::as_string) {
                    Some(kind) if kind.equals_str("user") => stats.written_user_files += 1,
                    Some(kind) if kind.equals_str("project") => stats.written_project_files += 1,
                    Some(kind) if kind.equals_str("feedback") => {
                        stats.written_feedback_files += 1;
                    }
                    _ => {}
                }
            }
            if wrote_global_user_note {
                self.repository
                    .repair_workspace_manifest()
                    .map_err(HeartbeatError::File)?;
            }
            let prepared = created_candidates
                .iter()
                .filter(|(_, candidate)| candidate.is_some())
                .count();
            create_step(
                trace,
                "persist",
                &"Persist".into(),
                if persisted_records.is_empty() {
                    "skipped"
                } else {
                    "success"
                },
                &JsString::from(format!("{prepared} candidates ready to persist.")),
                &JsString::from(if persisted_records.is_empty() {
                    "No memory files were written for this turn.".to_string()
                } else {
                    format!("{} memory files written.", persisted_records.len())
                }),
                &object(vec![(
                    "details",
                    JsValue::Array(vec![json_detail(
                        &format!("persisted-files-{}", session.l0_index_id),
                        "Persisted Files",
                        &JsValue::Array(
                            persisted_records
                                .iter()
                                .map(|record| {
                                    value(vec![
                                        (
                                            "type",
                                            record
                                                .get("type")
                                                .and_then(JsValue::as_string)
                                                .cloned()
                                                .map(JsValue::String)
                                                .unwrap_or(JsValue::Null),
                                        ),
                                        (
                                            "name",
                                            record
                                                .get("name")
                                                .and_then(JsValue::as_string)
                                                .cloned()
                                                .map(JsValue::String)
                                                .unwrap_or(JsValue::Null),
                                        ),
                                        (
                                            "projectId",
                                            record
                                                .get("projectId")
                                                .and_then(JsValue::as_string)
                                                .cloned()
                                                .map(JsValue::String)
                                                .unwrap_or(JsValue::Null),
                                        ),
                                        (
                                            "relativePath",
                                            JsValue::String(expose_stored_relative_path(
                                                record
                                                    .get("scope")
                                                    .and_then(JsValue::as_string)
                                                    .unwrap_or(&JsString::default()),
                                                record
                                                    .get("relativePath")
                                                    .and_then(JsValue::as_string)
                                                    .unwrap_or(&JsString::default()),
                                            )),
                                        ),
                                        (
                                            "storageKind",
                                            JsValue::String(
                                                infer_storage_kind(
                                                    record
                                                        .get("type")
                                                        .and_then(JsValue::as_string)
                                                        .unwrap_or(&JsString::default()),
                                                    record
                                                        .get("relativePath")
                                                        .and_then(JsValue::as_string)
                                                        .unwrap_or(&JsString::default()),
                                                )
                                                .into(),
                                            ),
                                        ),
                                    ])
                                })
                                .collect(),
                        ),
                        Some(&trace_i18n(
                            "trace.detail.persisted_files",
                            "Persisted Files",
                            &[],
                        )),
                    )]),
                )]),
            );
        }

        processed_ids.push(session.l0_index_id.clone());
        stats.captured_sessions += 1;
        self.repository.storage.set_pipeline_state(
            LAST_INDEXED_AT_STATE_KEY,
            &JsValue::String(JsString::from(session.timestamp.as_str())),
        )?;
        self.repository.storage.set_pipeline_state(
            &format!("{LAST_INDEXED_CURSOR_PREFIX}{}", session.session_key),
            &JsValue::String(JsString::from(session.timestamp.as_str())),
        )?;
        Ok(())
    }

    fn push_stored_result(&self, trace: &mut JsObject, value: JsValue) {
        push_stored_result(trace, value);
    }

    // -- routeGeneralCandidate (`heartbeat.ts:472-548`) --------------------

    async fn route_general_candidate(
        &self,
        candidate: &JsObject,
        focus_turn: &MemoryMessage,
        batch_context_messages: &[MemoryMessage],
        session_key: &str,
    ) -> Result<RoutedCandidate, HeartbeatError> {
        let store = self.repository.get_file_memory_store();
        let candidate_value = JsValue::Object(candidate.clone());
        let candidate_type = candidate
            .get("type")
            .and_then(JsValue::as_string)
            .cloned()
            .unwrap_or_default();
        if !store.is_general_mode() || candidate_type.equals_str("user") {
            return Ok(RoutedCandidate {
                candidate: candidate.clone(),
                ..RoutedCandidate::default()
            });
        }

        let routing_text = build_candidate_routing_query(&candidate_value, focus_turn);
        let candidate_preview = build_candidate_memory_preview(&candidate_value);
        let catalog_value = self
            .repository
            .list_readable_project_catalog()
            .map_err(HeartbeatError::File)?;
        let catalog = as_items(&catalog_value)
            .map(|entries| {
                entries
                    .iter()
                    .filter(|entry| {
                        !entry
                            .get("sourceType")
                            .and_then(JsValue::as_string)
                            .is_some_and(|source| source.equals_str("workspace_external"))
                    })
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let shortlist = build_general_project_shortlist(&catalog, &routing_text);
        let mut selected_project: Option<JsValue> = None;
        let mut selection_debug: Option<JsValue> = None;
        let mut routing_decision = "create_new";
        let mut routing_reason = if shortlist.is_empty() {
            JsString::from("No existing General projects are available.")
        } else {
            JsString::from("No existing General project was clearly selected.")
        };
        if !shortlist.is_empty() {
            self.fire(HeartbeatStage::BeforeSelectIndexProject, session_key);
            let recent_user_messages: Vec<MemoryMessage> = batch_context_messages
                .iter()
                .filter(|message| message.role == MemoryRole::User)
                .cloned()
                .collect::<Vec<_>>();
            let recent_user_messages =
                recent_user_messages[recent_user_messages.len().saturating_sub(4)..].to_vec();
            let mut debug: Option<JsValue> = None;
            let selection = self
                .extractor
                .select_index_project(
                    &SelectIndexProjectInput {
                        candidate: &candidate_value,
                        candidate_preview: &candidate_preview,
                        focus_turn,
                        recent_user_messages: Some(&recent_user_messages),
                        shortlist: &shortlist,
                        agent_id: None,
                        timeout_ms: None,
                    },
                    |event| debug = Some(event),
                )
                .await;
            selection_debug = debug;
            routing_decision = if selection.attach_existing {
                "attach_existing"
            } else {
                "create_new"
            };
            if let Some(reason) = &selection.reason {
                routing_reason = reason.clone();
            }
            if selection.attach_existing
                && let Some(project_id) = selection
                    .project_id
                    .as_ref()
                    .filter(|project_id| !project_id.is_empty())
            {
                let project = self
                    .repository
                    .get_readable_project(&String::from_utf16_lossy(project_id.units()))
                    .map_err(HeartbeatError::File)?;
                if project
                    .get("sourceType")
                    .and_then(JsValue::as_string)
                    .is_some_and(|source| source.equals_str("workspace_external"))
                {
                    selected_project = None;
                    routing_decision = "create_new";
                    routing_reason =
                        JsString::from("Selected project was outside General-local index scope.");
                } else if matches!(project, JsValue::Object(_)) {
                    selected_project = Some(project);
                }
            }
        }

        if let Some(project) = &selected_project {
            let project_id = project
                .get("projectId")
                .and_then(JsValue::as_string)
                .cloned()
                .unwrap_or_default();
            let mut routed = candidate.clone();
            routed.insert("projectId".into(), JsValue::String(project_id));
            return Ok(RoutedCandidate {
                candidate: routed,
                created_project_meta: None,
                selected_project: Some(project.clone()),
                selection_debug,
                routing_decision: Some(routing_decision),
                routing_reason: Some(routing_reason),
            });
        }

        let project_name = if candidate_type.equals_str("project") {
            candidate
                .get("name")
                .and_then(JsValue::as_string)
                .cloned()
                .unwrap_or_default()
        } else {
            candidate
                .get("description")
                .and_then(JsValue::as_string)
                .filter(|description| !description.is_empty())
                .cloned()
                .or_else(|| candidate.get("name").and_then(JsValue::as_string).cloned())
                .unwrap_or_default()
        };
        let meta = store
            .upsert_project_meta(&object(vec![
                ("projectName", JsValue::String(project_name)),
                (
                    "description",
                    JsValue::String(
                        candidate
                            .get("description")
                            .and_then(JsValue::as_string)
                            .cloned()
                            .unwrap_or_default(),
                    ),
                ),
                ("status", JsValue::String("in_progress".into())),
                ("sourceKind", JsValue::String("general_local".into())),
            ]))
            .map_err(HeartbeatError::File)?;
        let project_id = meta
            .get("projectId")
            .and_then(JsValue::as_string)
            .cloned()
            .unwrap_or_default();
        let mut routed = candidate.clone();
        routed.insert("projectId".into(), JsValue::String(project_id));
        let meta_object = meta.as_object().cloned().unwrap_or_default();
        Ok(RoutedCandidate {
            candidate: routed,
            created_project_meta: Some(meta_object),
            selected_project: None,
            selection_debug,
            routing_decision: Some("create_new"),
            routing_reason: Some(routing_reason),
        })
    }
}

/// `captureL0Session` input (`heartbeat.ts:550-555`).
#[derive(Clone, Debug)]
pub struct CaptureL0SessionInput {
    pub session_key: String,
    pub timestamp: Option<String>,
    pub messages: Vec<MemoryMessage>,
    pub source: Option<String>,
}

#[derive(Default)]
struct RoutedCandidate {
    candidate: JsObject,
    created_project_meta: Option<JsObject>,
    selected_project: Option<JsValue>,
    selection_debug: Option<JsValue>,
    routing_decision: Option<&'static str>,
    routing_reason: Option<JsString>,
}

impl RoutedCandidate {
    fn candidate_type_is_user(&self) -> bool {
        self.candidate
            .get("type")
            .and_then(JsValue::as_string)
            .is_some_and(|kind| kind.equals_str("user"))
    }
}

/// Error union for the indexer. `message()` is the TS catch-block
/// `error.message`; `js_string()` is `String(error)` for the warning log.
#[derive(Debug)]
pub enum HeartbeatError {
    Storage(StorageError),
    File(FileMemoryError),
    Index(IndexError),
    Json(String),
}
impl std::fmt::Display for HeartbeatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Storage(error) => write!(f, "{error}"),
            Self::File(error) => write!(f, "{error}"),
            Self::Index(error) => write!(f, "{}", utf8_lossy(&error.message())),
            Self::Json(error) => write!(f, "{error}"),
        }
    }
}
impl std::error::Error for HeartbeatError {}
impl From<StorageError> for HeartbeatError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}
impl From<FileMemoryError> for HeartbeatError {
    fn from(error: FileMemoryError) -> Self {
        Self::File(error)
    }
}
impl From<IndexError> for HeartbeatError {
    fn from(error: IndexError) -> Self {
        Self::Index(error)
    }
}
impl HeartbeatError {
    /// TS catch-block `error.message`; the service facade delegates here so a
    /// new variant cannot drift from the indexer's own mapping.
    pub(crate) fn message(&self) -> JsString {
        match self {
            Self::Storage(error) => error.js_message(),
            Self::File(error) => error.js_message(),
            Self::Index(error) => error.message(),
            Self::Json(error) => JsString::from(error.as_str()),
        }
    }

    fn js_string(&self) -> JsString {
        match self {
            Self::Index(error) => index_js_string(error),
            _ => JsString::from(format!("Error: {}", utf8_lossy(&self.message()))),
        }
    }
}

fn index_js_string(error: &IndexError) -> JsString {
    match error {
        IndexError::Llm(error) => error.js_string(),
        IndexError::Envelope(error) => JsString::from(format!("Error: {}", error.message())),
        IndexError::Type(message) => JsString::from(format!(
            "TypeError: {}",
            String::from_utf16_lossy(message.units())
        )),
        IndexError::Range => JsString::from(format!(
            "RangeError: {}",
            crate::js_value::StackOverflow::MESSAGE
        )),
    }
}

fn label_value(label: &MemoryClassificationLabel) -> JsValue {
    value(vec![
        ("type", JsValue::String(label.kind.as_str().into())),
        ("reason", JsValue::String(label.reason.clone())),
        ("evidence", JsValue::String(label.evidence.clone())),
    ])
}

fn trace_i18n(key: &str, fallback: &str, args: &[JsValue]) -> JsValue {
    JsValue::Object(
        text::trace_i18n(&JsString::from(key), &JsString::from(fallback), args)
            .expect("#770 trace i18n arguments"),
    )
}

fn value(entries: Vec<(&str, JsValue)>) -> JsValue {
    JsValue::Object(object(entries))
}

fn utf8_lossy(value: &JsString) -> String {
    value
        .to_utf8()
        .unwrap_or_else(|_| String::from_utf16_lossy(value.units()))
}

fn as_items(value: &JsValue) -> Option<&[JsValue]> {
    match value {
        JsValue::Array(items) => Some(items),
        _ => None,
    }
}

fn object(entries: Vec<(&str, JsValue)>) -> JsObject {
    let mut object = JsObject::new();
    for (key, value) in entries {
        object.insert(key.into(), value);
    }
    object
}

fn field_str(trace: &JsObject, key: &str) -> String {
    field_str_value(trace, key)
        .to_utf8()
        .unwrap_or_else(|_| String::from_utf16_lossy(field_str_value(trace, key).units()))
}

fn field_str_value(object: &JsObject, key: &str) -> JsString {
    object
        .get(key)
        .and_then(JsValue::as_string)
        .cloned()
        .unwrap_or_default()
}

fn push_stored_result(trace: &mut JsObject, value: JsValue) {
    let mut results = trace
        .get("storedResults")
        .and_then(as_items)
        .map(|items| items.to_vec())
        .unwrap_or_default();
    results.push(value);
    trace.insert("storedResults".into(), JsValue::Array(results));
}

fn stored_results_len(trace: &JsObject) -> usize {
    trace
        .get("storedResults")
        .and_then(as_items)
        .map_or(0, <[JsValue]>::len)
}
