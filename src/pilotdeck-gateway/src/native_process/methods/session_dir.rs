//! S1 session directory / lifecycle wiring.
//!
//! TS references:
//!  - `InProcessGateway.ts:2022-2078` (list/resume/new/close/closeProject/record)
//!  - `InProcessGateway.ts:2211-2239` (rename/tag/delete)
//!  - `createLocalGateway.ts:1116-1152` (rename/tag/delete implementations)
//!  - `SessionRouter.ts:368-420` (list, recordAgentStatusMessage)
//!  - `GatewaySessionCatalog.ts:20-35` (cursor protocol over SessionCatalogPort)
use super::super::requests::Requests;
use crate::{
    metadata::object,
    runtime::{
        connection::{Call, RequestError},
        manager::{
            GatewaySessionCatalogConsumer, SessionCatalogListInput, TranscriptSessionCatalog,
        },
        session::transcript_locks,
    },
    storage::{AsyncTranscriptAdapter, JsonlTranscriptStore, ProjectKey, SessionStorageInput},
};
use pilotdeck_protocol::{JsString, JsValue};
use std::sync::Arc;

pub(super) const METHODS: &[&str] = &[
    "list_sessions",
    "resume_session",
    "new_session",
    "close_session",
    "delete_session",
    "rename_session",
    "tag_session",
    "close_project_sessions",
    "record_agent_status_message",
];
pub(super) const HANDLER: super::Handler =
    super::Handler::AfterAdmission(Requests::session_dir_request);

fn gateway_error(code: &'static str, message: impl Into<String>) -> RequestError {
    RequestError {
        code: code.into(),
        message: message.into().into(),
        details: None,
        validation: None,
    }
}

/// Resolves `projectKey` from params, falling back to the configured project.
/// Returns the resolved path string.
fn resolve_project(requests: &Requests, params: &JsValue) -> Result<String, RequestError> {
    let project = params
        .get("projectKey")
        .and_then(JsValue::as_string)
        .and_then(|s| s.to_utf8().ok())
        .unwrap_or_else(|| requests.options.project.to_string_lossy().into_owned());
    if !requests.options.serves_project(&project) {
        return Err(gateway_error(
            "CAPABILITY_UNAVAILABLE",
            "The isolated native process serves only its configured project.",
        ));
    }
    Ok(project)
}

/// Validates that `sessionKey` is present and non-blank (TS `?.trim()` check).
/// Delegates to the family-shared guard (#1604) so the blank test is JS trim
/// (U+FEFF and the other WhiteSpace code points count as blank) and a
/// non-string produces the bare `input.sessionKey?.trim is not a function`
/// TypeError instead of the `INVALID_SESSION_KEY` domain error this file used
/// to answer. The blank check stays
/// lossy: TS accepts a lone surrogate in the key (`?.trim()` leaves it intact,
/// #1455 probe4), so a `to_utf8` filter here would reject a key TS accepts.
/// Callers that need UTF-8 bytes convert lossily at the point of use, like
/// TS's own path encoding.
fn require_session_key(params: &JsValue) -> Result<JsString, RequestError> {
    super::require_text(
        params,
        "sessionKey",
        "INVALID_SESSION_KEY",
        "sessionKey is required.",
    )
}

/// Build a transcript adapter for the configured project.
fn _make_adapter(requests: &Requests) -> AsyncTranscriptAdapter {
    let home = requests.options.home.clone();
    AsyncTranscriptAdapter::new(
        Arc::new(JsonlTranscriptStore::new()),
        Arc::new(move |key: &ProjectKey| {
            crate::session_chat_dir(
                &home,
                &key.project_root,
                &key.project_root.to_string_lossy(),
            )
        }),
    )
}

impl Requests {
    pub(in crate::native_process) fn session_dir_request(
        &self,
        method: String,
        params: JsValue,
    ) -> Call<'_, Result<JsValue, RequestError>> {
        Box::pin(async move {
            match method.as_str() {
                "list_sessions" => self.session_list(&params).await,
                "resume_session" => self.session_resume(&params),
                "new_session" => self.session_new(&params),
                "close_session" => self.session_close(&params).await,
                "delete_session" => self.session_delete(&params).await,
                "rename_session" => self.session_rename_or_tag(&params, false).await,
                "tag_session" => self.session_rename_or_tag(&params, true).await,
                "close_project_sessions" => self.session_close_project(&params).await,
                "record_agent_status_message" => self.session_record_status(&params).await,
                _ => unreachable!("session_dir_request called for unknown method"),
            }
        })
    }

    /// `InProcessGateway.listSessions` → `SessionRouter.list` → catalog path.
    ///
    /// Cursor is a base-10 integer offset string (`GatewaySessionCatalog.ts:24-25`).
    async fn session_list(&self, params: &JsValue) -> Result<JsValue, RequestError> {
        let project_root = resolve_project(self, params)?;
        let limit = params
            .get("limit")
            .and_then(JsValue::as_number)
            .filter(|n| n.is_finite() && *n > 0.0);
        let offset: f64 = params
            .get("cursor")
            .and_then(JsValue::as_string)
            .and_then(|s| s.to_utf8().ok())
            .filter(|s| !s.is_empty())
            .and_then(|s| s.parse::<f64>().ok())
            .filter(|n| n.is_finite())
            .unwrap_or(0.0);
        let home = self.options.home.clone();
        let input = SessionCatalogListInput {
            project_root: project_root.clone().into(),
            pilot_home: home.clone(),
            limit,
            offset: Some(offset),
            include_internal: false,
        };
        let sessions = tokio::task::spawn_blocking(move || {
            let adapter = AsyncTranscriptAdapter::new(
                Arc::new(JsonlTranscriptStore::new()),
                Arc::new(move |key: &ProjectKey| {
                    crate::session_chat_dir(
                        &home,
                        &key.project_root,
                        &key.project_root.to_string_lossy(),
                    )
                }),
            );
            let catalog = Arc::new(TranscriptSessionCatalog::new(Arc::new(adapter)));
            GatewaySessionCatalogConsumer::new(catalog).list(&input)
        })
        .await
        .map_err(|e| RequestError::new(e.to_string()))?
        .map_err(|e| RequestError {
            code: e.code().unwrap_or("gateway_request_failed").into(),
            message: e.message().clone(),
            details: None,
            validation: None,
        })?;
        let next_cursor = limit
            .filter(|lim| sessions.len() as f64 >= *lim)
            .map(|lim| (offset + lim).to_string());
        let mut result = pilotdeck_protocol::JsObject::new();
        result.insert("sessions".into(), JsValue::Array(sessions));
        if let Some(cursor) = next_cursor {
            result.insert("nextCursor".into(), JsValue::String(cursor.into()));
        }
        Ok(JsValue::Object(result))
    }

    /// `InProcessGateway.resumeSession` → identity (returns the key unchanged).
    fn session_resume(&self, params: &JsValue) -> Result<JsValue, RequestError> {
        let key = require_session_key(params)?;
        Ok(object([("sessionKey", JsValue::String(key))]))
    }

    /// `InProcessGateway.newSession` → generate key from channelKey + optional projectKey.
    fn session_new(&self, params: &JsValue) -> Result<JsValue, RequestError> {
        let channel_key = params
            .get("channelKey")
            .and_then(JsValue::as_string)
            .cloned()
            .ok_or_else(|| RequestError::new("channelKey must be a string."))?;
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let project_prefix = params
            .get("projectKey")
            .and_then(JsValue::as_string)
            .and_then(|s| s.to_utf8().ok())
            .filter(|s| !s.is_empty())
            .map(|p| format!("project={}:", p))
            .unwrap_or_default();
        let session_key = format!(
            "{}:{}s_{}",
            channel_key
                .to_utf8()
                .map_err(|_| RequestError::new("channelKey must be a valid UTF-8 string."))?,
            project_prefix,
            suffix,
        );
        Ok(object([(
            "sessionKey",
            JsValue::String(session_key.into()),
        )]))
    }

    /// `InProcessGateway.closeSession` → owner.close + interaction coordinator cleanup.
    async fn session_close(&self, params: &JsValue) -> Result<JsValue, RequestError> {
        let key = require_session_key(params)?;
        let reason: JsString = params
            .get("reason")
            .and_then(JsValue::as_string)
            .cloned()
            .unwrap_or_else(|| "session_closed".into());
        self.owner
            .close(&key)
            .await
            .map_err(|e| RequestError::new(e.to_string()))?;
        self.interactions.close_session(&key, &reason);
        Ok(JsValue::Undefined)
    }

    /// `createLocalGateway.deleteSession` → close live owner, delete transcript +
    /// file history + tool results + subagents directories.
    async fn session_delete(&self, params: &JsValue) -> Result<JsValue, RequestError> {
        let key = require_session_key(params)?;
        let project_root: std::path::PathBuf = resolve_project(self, params)?.into();
        // Close the live session first so the writer drains before we delete.
        self.owner
            .close(&key)
            .await
            .map_err(|e| RequestError::new(e.to_string()))?;
        let home = self.options.home.clone();
        let key_clone = key.clone();
        let project_clone = project_root.clone();
        tokio::task::spawn_blocking(move || {
            let home2 = home.clone();
            let adapter = AsyncTranscriptAdapter::new(
                Arc::new(JsonlTranscriptStore::new()),
                Arc::new(move |k: &ProjectKey| {
                    crate::session_chat_dir(
                        &home2,
                        &k.project_root,
                        &k.project_root.to_string_lossy(),
                    )
                }),
            );
            let storage = adapter
                .create_session_storage(SessionStorageInput {
                    project_root: project_clone.clone(),
                    pilot_home: home.clone(),
                    session_id: key_clone.clone(),
                    now: None,
                })
                .map_err(|e| RequestError::new(e.to_string()))?;
            // Verify the transcript exists before deleting.
            let exists = storage
                .transcript_exists()
                .transpose()
                .map_err(|e| RequestError::new(e.to_string()))?
                .unwrap_or(false);
            if !exists {
                let msg = format!(
                    "Session not found: {}",
                    String::from_utf16_lossy(key_clone.units())
                );
                return Err(gateway_error("SESSION_NOT_FOUND", msg));
            }
            // Delete transcript files.
            if let Some(result) = storage.delete_session_transcripts() {
                result.map_err(|e| RequestError::new(e.to_string()))?;
            }
            // Remove file history and tool results directories.
            for dir in [
                &storage.file_history_dir,
                &storage.tool_results_dir,
                &storage.subagents_dir,
            ] {
                let _ = std::fs::remove_dir_all(dir);
            }
            Ok(())
        })
        .await
        .map_err(|e| RequestError::new(e.to_string()))??;
        Ok(JsValue::Undefined)
    }

    /// `createLocalGateway.renameSession` / `tagSession` → busy check + metadata record.
    /// `is_tag` selects between title (rename) and tag metadata fields.
    async fn session_rename_or_tag(
        &self,
        params: &JsValue,
        is_tag: bool,
    ) -> Result<JsValue, RequestError> {
        let key = require_session_key(params)?;
        if self.owner.has_active_turn(&key) {
            let verb = if is_tag { "tag" } else { "rename" };
            return Err(gateway_error(
                "SESSION_BUSY",
                format!("Cannot {verb} an active session."),
            ));
        }
        let project_root: std::path::PathBuf = resolve_project(self, params)?.into();
        // #1457 fix-r1: same single-writer fence the config family uses; two
        // fresh writers that both restore the tail and append `sequence + 1`
        // leave the transcript unloadable.
        let fence_path =
            transcript_locks::session_transcript_path(&self.options.home, &project_root, &key);
        let write_fence = transcript_locks::acquire(&fence_path).await?;
        // #1457 fix-r2: a loaded session owns the transcript writer (and the
        // writer lease) with an in-memory sequence, so the metadata entry goes
        // *through* its recorder. Closing it first retired an idle session the
        // user did not ask to close and could abort a turn admitted between the
        // busy check and the close.
        // #1457 fix-r3: with no loaded session the cold append takes the same
        // writer lease a session holds, so it cannot interleave with the
        // factory's restore→publish window; a busy lease means a session owns
        // the transcript after all (use its writer or report busy).
        let live = super::super::assembly::live_recorder(&self.recorders, &key);
        let recorders = self.recorders.clone();
        let home = self.options.home.clone();
        let value_field: JsValue = params.get("value").cloned().unwrap_or(JsValue::Undefined);
        let value_str: Option<String> = match &value_field {
            JsValue::Null | JsValue::Undefined => None,
            other => other
                .as_string()
                .and_then(|s| s.to_utf8().ok())
                .or_else(|| {
                    // TS: `String(input.value)` — convert number/bool to string
                    match other {
                        JsValue::Number(n) => Some(n.to_string()),
                        JsValue::Bool(b) => Some(b.to_string()),
                        _ => None,
                    }
                }),
        };
        let key_clone = key.clone();
        tokio::task::spawn_blocking(move || {
            // Held inside the closure: a cancelled request drops the future but
            // not the blocking append, so the fence has to travel with the write.
            let _write = write_fence;
            let updated_at = pilotdeck_transport::system_clock::format_epoch_iso(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as i64,
            );
            let mut meta = pilotdeck_protocol::JsObject::new();
            let field_name = if is_tag { "tag" } else { "title" };
            meta.insert(
                field_name.into(),
                value_str
                    .map(|s| JsValue::String(s.into()))
                    .unwrap_or(JsValue::Undefined),
            );
            meta.insert("updatedAt".into(), JsValue::String(updated_at.into()));
            let turn_id = JsString::from(if is_tag { "sdk-tag" } else { "sdk-rename" });
            let entry = JsValue::Object(meta);
            if let Some((_owner, recorder)) = live {
                recorder
                    .writer()
                    .record_session_metadata_checked(&key_clone, &turn_id, entry)
                    .map_err(|e| RequestError::new(e.to_string()))?;
                return Ok(());
            }
            let home2 = home.clone();
            let adapter = AsyncTranscriptAdapter::new(
                Arc::new(JsonlTranscriptStore::new()),
                Arc::new(move |k: &ProjectKey| {
                    crate::session_chat_dir(
                        &home2,
                        &k.project_root,
                        &k.project_root.to_string_lossy(),
                    )
                }),
            );
            let storage = adapter
                .create_session_storage(SessionStorageInput {
                    project_root: project_root.clone(),
                    pilot_home: home.clone(),
                    session_id: key_clone.clone(),
                    now: None,
                })
                .map_err(|e| RequestError::new(e.to_string()))?;
            let Some(_lease) = transcript_locks::cold_write_lease(&storage.transcript_path) else {
                // #1457 fix-r3: a session claimed the transcript after the
                // lookup. Append through its writer if it is published;
                // otherwise report busy instead of forking the chain.
                if let Some((_owner, recorder)) =
                    super::super::assembly::live_recorder(&recorders, &key_clone)
                {
                    recorder
                        .writer()
                        .record_session_metadata_checked(&key_clone, &turn_id, entry)
                        .map_err(|e| RequestError::new(e.to_string()))?;
                    return Ok(());
                }
                return Err(transcript_locks::writer_busy_error());
            };
            // Seed the writer's sequence from the existing transcript so the
            // metadata entry continues the chain rather than restarting at 1.
            if matches!(storage.transcript_exists(), Some(Ok(true))) {
                storage
                    .restore()
                    .map_err(|e| RequestError::new(e.to_string()))?;
            }
            storage
                .transcript
                .record_session_metadata(&key_clone, &turn_id, entry)
                .map_err(|e| RequestError::new(e.to_string()))?;
            Ok(())
        })
        .await
        .map_err(|e| RequestError::new(e.to_string()))??;
        Ok(object([("updated", JsValue::Bool(true))]))
    }

    /// `InProcessGateway.closeProjectSessions` → resume or close all project sessions.
    async fn session_close_project(&self, params: &JsValue) -> Result<JsValue, RequestError> {
        // #1604: `!input.projectKey?.trim()` — same guard family as
        // `require_session_key`, so a non-string projectKey throws the bare
        // optional-chain TypeError instead of folding into the domain error.
        // The missing/blank branch keeps the plain `Error` shape TS produces
        // (`gateway_request_failed` + "projectKey is required.").
        let project_key = super::require_text(
            params,
            "projectKey",
            "gateway_request_failed",
            "projectKey is required.",
        )?;
        // A key that is not valid UTF-8 is still a missing key here: it can
        // never name a project directory (`from_utf16_lossy` would silently
        // rename it). Same answer as before #1604.
        let project_key_str = project_key
            .to_utf8()
            .map_err(|_| RequestError::new("projectKey is required."))?;
        if !self.options.serves_project(&project_key_str) {
            return Err(gateway_error(
                "CAPABILITY_UNAVAILABLE",
                "The isolated native process serves only its configured project.",
            ));
        }
        if params.get("resume") == Some(&JsValue::Bool(true)) {
            self.owner.resume_project(&project_key);
            return Ok(object([("sessionKeys", JsValue::Array(vec![]))]));
        }
        let keys = self
            .owner
            .close_project(&project_key)
            .await
            .map_err(|e| RequestError::new(e.to_string()))?;
        for key in &keys {
            self.interactions
                .close_session(key, &"project_closed".into());
        }
        let session_keys: Vec<JsValue> = keys.into_iter().map(JsValue::String).collect();
        Ok(object([("sessionKeys", JsValue::Array(session_keys))]))
    }

    /// `InProcessGateway.recordAgentStatusMessage` → live owner first, cold fallback.
    ///
    /// Live path: look up the session's recorder in the shared recorders map.
    /// Cold path: append via a fresh transcript writer (like `options.recordAgentStatusMessage`).
    async fn session_record_status(&self, params: &JsValue) -> Result<JsValue, RequestError> {
        let session_key = params
            .get("sessionKey")
            .and_then(JsValue::as_string)
            .cloned()
            .ok_or_else(|| RequestError::new("sessionKey must be a string."))?;
        let turn_id = params
            .get("turnId")
            .and_then(JsValue::as_string)
            .cloned()
            .ok_or_else(|| RequestError::new("turnId must be a string."))?;
        let status = params.get("status").cloned().unwrap_or(JsValue::Undefined);

        // #1457 fix-r2: take the fence before deciding who writes. The lookup
        // and the append then see one state — either a live writer exists and
        // the entry goes through it, or it does not and this path is the only
        // writer for the tail it restores. Looking the recorder up first left a
        // window where a concurrent close retired it between lookup and append,
        // and the retired writer's no-op append reported `recorded: true`.
        let fence_path = transcript_locks::session_transcript_path(
            &self.options.home,
            &self.options.project,
            &session_key,
        );
        let write_fence = transcript_locks::acquire(&fence_path).await?;
        let live_recorder = super::super::assembly::live_recorder(&self.recorders, &session_key);
        let recorders = self.recorders.clone();
        if let Some((_owner, recorder)) = live_recorder {
            // The live writer numbers its append from its own in-memory
            // sequence, so it appends under the same fence the fresh writers
            // hold — otherwise it can append while `record_model_selection`
            // holds a tail both of them read as `N`. fix-r3: the checked
            // append turns a writer that closed under us into an error instead
            // of a `recorded: true` with nothing on disk.
            let session_key2 = session_key.clone();
            let turn_id2 = turn_id.clone();
            let status2 = status.clone();
            let recorded = tokio::task::spawn_blocking(move || {
                let _write = write_fence;
                recorder
                    .writer()
                    .record_agent_status_message_checked(&session_key2, &turn_id2, &status2)
                    .map(|()| true)
            })
            .await
            .map_err(|e| RequestError::new(e.to_string()))?
            .map_err(|e| RequestError::new(e.to_string()))?;
            return Ok(object([("recorded", JsValue::Bool(recorded))]));
        }

        // Cold path: write to the durable transcript directly, holding the
        // session writer lease so a concurrent hydrate cannot restore the same
        // tail and publish a recorder beside this append.
        let home = self.options.home.clone();
        let project_root = self.options.project.clone();
        let recorded = tokio::task::spawn_blocking(move || {
            // The fence must outlive a cancelled request, so it moves into the
            // blocking closure instead of dropping with this future.
            let _write = write_fence;
            let home2 = home.clone();
            let adapter = AsyncTranscriptAdapter::new(
                Arc::new(JsonlTranscriptStore::new()),
                Arc::new(move |k: &ProjectKey| {
                    crate::session_chat_dir(
                        &home2,
                        &k.project_root,
                        &k.project_root.to_string_lossy(),
                    )
                }),
            );
            match adapter.create_session_storage(SessionStorageInput {
                project_root: project_root.clone(),
                pilot_home: home.clone(),
                session_id: session_key.clone(),
                now: None,
            }) {
                Ok(storage) => {
                    let Some(_lease) = transcript_locks::cold_write_lease(&storage.transcript_path)
                    else {
                        // A session claimed the transcript after the lookup:
                        // append through its writer, or report that nothing was
                        // recorded instead of forking the chain.
                        return super::super::assembly::live_recorder(&recorders, &session_key)
                            .is_some_and(|(_owner, recorder)| {
                                recorder
                                    .writer()
                                    .record_agent_status_message_checked(
                                        &session_key,
                                        &turn_id,
                                        &status,
                                    )
                                    .is_ok()
                            });
                    };
                    // Seed the writer's sequence from the existing transcript so
                    // the status entry continues the chain rather than restarting
                    // at 1.
                    if matches!(storage.transcript_exists(), Some(Ok(true)))
                        && storage.restore().is_err()
                    {
                        return false;
                    }
                    storage
                        .transcript
                        .record_agent_status_message(&session_key, &turn_id, &status)
                        .map(|()| true)
                        .unwrap_or(false)
                }
                Err(_) => false,
            }
        })
        .await
        .unwrap_or(false);
        Ok(object([("recorded", JsValue::Bool(recorded))]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::negative_assert_eq;
    use crate::storage::recorder::Recorder;

    /// #1457 fix-r2: a loaded session owns the transcript writer, so the rename
    /// append goes through its recorder instead of a second writer restored
    /// from the tail. The request spells the key as an alias (`a8/session` and
    /// `a8-session` are one file after `sanitize_session_id`), which is exactly
    /// the case a raw-key lookup missed: the recorder would keep a stale
    /// sequence and its next append would duplicate `sequence + 1`.
    #[tokio::test]
    async fn rename_appends_through_the_loaded_writer() {
        let (_scratch, requests) = crate::native_process::management_tests::setup();
        let home = requests.options.home.clone();
        let project = requests.options.project.clone();
        let adapter = AsyncTranscriptAdapter::new(
            Arc::new(JsonlTranscriptStore::new()),
            Arc::new(move |k: &ProjectKey| {
                crate::session_chat_dir(&home, &k.project_root, &k.project_root.to_string_lossy())
            }),
        );
        let open = |session_id: &str| {
            adapter
                .create_session_storage(SessionStorageInput {
                    project_root: project.clone(),
                    pilot_home: requests.options.home.clone(),
                    session_id: session_id.into(),
                    now: None,
                })
                .expect("#1457 the transcript storage opens")
        };
        let loaded = open("a8/session");
        loaded
            .restore()
            .expect("#1457 the seeded transcript restores");
        let writer = Arc::new(loaded.transcript);
        let recorder = Arc::new(Recorder::from_writer(writer.clone()));
        requests
            .recorders
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert("a8/session".into(), Arc::downgrade(&recorder));

        let updated = crate::runtime::turn::gateway::GatewayRequests::request(
            &requests,
            "rename_session".into(),
            crate::metadata::object([
                ("sessionKey", "a8-session".into()),
                ("value", "renamed-through-the-recorder".into()),
            ]),
        )
        .await
        .expect("#1457 the rename resolves over a loaded session");
        negative_assert_eq!(
            updated
                .get("updated")
                .and_then(pilotdeck_protocol::JsValue::as_bool),
            Some(true),
            "#1457 the rename reports the metadata write"
        );

        // The loaded writer still holds the only sequence: this append continues
        // the chain the rename extended. A second writer on the same tail would
        // have duplicated `sequence + 1` and this call would be rejected.
        writer
            .record_agent_status_message(
                &"a8/session".into(),
                &"after-rename".into(),
                &JsValue::Undefined,
            )
            .expect("#1457 the loaded writer keeps numbering the chain");
        let reread = open("a8-session");
        reread.restore().expect("#1457 the transcript still loads");
        let text = std::fs::read_to_string(&reread.transcript_path)
            .expect("#1457 the written transcript is readable");
        negative_assert_eq!(
            text.contains("renamed-through-the-recorder"),
            true,
            "#1457 the rename entry reached the loaded session's transcript"
        );
        negative_assert_eq!(
            text.contains("after-rename"),
            true,
            "#1457 the loaded writer kept appending after the rename"
        );
    }

    /// #1457 fix-r3: `record_agent_status_message` has the same live branch as
    /// rename. Two things have to hold at once: the entry goes through the
    /// loaded writer (so the chain keeps one sequence), and the request reports
    /// `recorded: true` only because the writer actually took it.
    #[tokio::test]
    async fn status_appends_through_the_loaded_writer() {
        let (_scratch, requests) = crate::native_process::management_tests::setup();
        let home = requests.options.home.clone();
        let project = requests.options.project.clone();
        let adapter = AsyncTranscriptAdapter::new(
            Arc::new(JsonlTranscriptStore::new()),
            Arc::new(move |k: &ProjectKey| {
                crate::session_chat_dir(&home, &k.project_root, &k.project_root.to_string_lossy())
            }),
        );
        let open = |session_id: &str| {
            adapter
                .create_session_storage(SessionStorageInput {
                    project_root: project.clone(),
                    pilot_home: requests.options.home.clone(),
                    session_id: session_id.into(),
                    now: None,
                })
                .expect("#1457 the transcript storage opens")
        };
        let loaded = open("c4/session");
        loaded
            .restore()
            .expect("#1457 the seeded transcript restores");
        let writer = Arc::new(loaded.transcript);
        let recorder = Arc::new(Recorder::from_writer(writer.clone()));
        requests
            .recorders
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert("c4/session".into(), Arc::downgrade(&recorder));

        let recorded = crate::runtime::turn::gateway::GatewayRequests::request(
            &requests,
            "record_agent_status_message".into(),
            crate::metadata::object([
                ("sessionKey", "c4-session".into()),
                ("turnId", "status-through-the-recorder".into()),
                ("status", JsValue::Undefined),
            ]),
        )
        .await
        .expect("#1457 the status write resolves over a loaded session");
        negative_assert_eq!(
            recorded
                .get("recorded")
                .and_then(pilotdeck_protocol::JsValue::as_bool),
            Some(true),
            "#1457 the status write reports the metadata write"
        );

        writer
            .record_agent_status_message(
                &"c4/session".into(),
                &"after-status".into(),
                &JsValue::Undefined,
            )
            .expect("#1457 the loaded writer keeps numbering the chain");
        let reread = open("c4-session");
        reread.restore().expect("#1457 the transcript still loads");
        let text = std::fs::read_to_string(&reread.transcript_path)
            .expect("#1457 the written transcript is readable");
        negative_assert_eq!(
            text.contains("status-through-the-recorder"),
            true,
            "#1457 the status entry reached the loaded session's transcript"
        );
    }

    /// #1457 fix-r3: a cold metadata append must not run while a session holds
    /// the writer lease. The lease is the same object the factory takes before
    /// it restores the tail and publishes the recorder, so holding it here is
    /// exactly the hydrate window; the request has to report the retryable busy
    /// error instead of forking the sequence chain.
    #[tokio::test]
    async fn cold_metadata_write_reports_busy_while_a_session_holds_the_lease() {
        let (_scratch, requests) = crate::native_process::management_tests::setup();
        let home = requests.options.home.clone();
        let project = requests.options.project.clone();
        let adapter = AsyncTranscriptAdapter::new(
            Arc::new(JsonlTranscriptStore::new()),
            Arc::new(move |k: &ProjectKey| {
                crate::session_chat_dir(&home, &k.project_root, &k.project_root.to_string_lossy())
            }),
        );
        let storage = adapter
            .create_session_storage(SessionStorageInput {
                project_root: project.clone(),
                pilot_home: requests.options.home.clone(),
                session_id: "d2/session".into(),
                now: None,
            })
            .expect("#1457 the transcript storage opens");
        // Stand in for `factory`: the lease is taken before restore and held
        // while the recorder is not yet in the map.
        let lease = transcript_locks::cold_write_lease(&storage.transcript_path)
            .expect("#1457 the transcript lease is free before the session loads");

        let error = crate::runtime::turn::gateway::GatewayRequests::request(
            &requests,
            "rename_session".into(),
            crate::metadata::object([
                ("sessionKey", "d2-session".into()),
                ("value", "must-not-fork".into()),
            ]),
        )
        .await
        .expect_err("#1457 a cold append must not run beside a session that owns the writer");
        negative_assert_eq!(
            error.code.as_str(),
            "SESSION_WRITER_BUSY",
            "#1457 the racing cold append reports the retryable busy error"
        );
        drop(lease);
        negative_assert_eq!(
            std::fs::read_to_string(&storage.transcript_path)
                .map(|text| text.contains("must-not-fork"))
                .unwrap_or(false),
            false,
            "#1457 the refused append wrote nothing"
        );
    }

    // #1455 r1: the same gate serves resume / close / delete / rename / tag;    // a guard-level case pins the predicate once for all five callers instead
    // of relying on the single resume-level request test.
    #[test]
    fn require_session_key_keeps_lone_surrogates_and_rejects_blank() {
        let surrogate = JsString::from_utf16(vec![0xd800]);
        let accepted = object([("sessionKey", JsValue::String(surrogate))]);
        negative_assert_eq!(
            require_session_key(&accepted).is_ok(),
            true,
            "#1455 require_session_key keeps the lone-surrogate key for every caller"
        );
        let blank = object([("sessionKey", "  ".into())]);
        let err = require_session_key(&blank)
            .expect_err("#1455 require_session_key rejects the blank key for every caller");
        negative_assert_eq!(
            err.code.as_str(),
            "INVALID_SESSION_KEY",
            "#1455 require_session_key rejects the blank key for every caller"
        );
    }

    // #1455 G7: the shared `?.trim()` gate accepts a lone-surrogate key the
    // way TS does (probe4: `{"label":"thinking-lone-surrogate-key",
    // "value":{"applied":"callback"}}`); the previous `to_utf8` filter
    // rejected it with INVALID_SESSION_KEY, diverging from TS.
    #[tokio::test]
    async fn resume_session_accepts_lone_surrogate_session_key() {
        let (_s, r) = std::panic::catch_unwind(crate::native_process::management_tests::setup)
            .expect("#1310 fixture");
        let key = JsString::from_utf16(vec![0xd800]);
        let params = object([("sessionKey", JsValue::String(key.clone()))]);
        let result = r.session_dir_request("resume_session".into(), params).await;
        let value = result.expect("#1455 resume_session accepts the surrogate key");
        let echoed = value
            .get("sessionKey")
            .and_then(JsValue::as_string)
            .cloned()
            .expect("#1455 resume_session echoes the sessionKey");
        negative_assert_eq!(
            echoed.units(),
            key.units(),
            "#1455 resume_session passes the lone-surrogate sessionKey through the trim gate"
        );
        r.owner.shutdown().await.expect("#1310 shutdown");
        r.project.dispose().await.expect("#1310 dispose");
    }

    #[tokio::test]
    async fn resume_session_rejects_blank_session_key() {
        let (_s, r) = std::panic::catch_unwind(crate::native_process::management_tests::setup)
            .expect("#1310 fixture");
        let params = object([("sessionKey", "  ".into())]);
        let result = r.session_dir_request("resume_session".into(), params).await;
        let err = result.expect_err("#1455 resume_session rejects the blank key");
        negative_assert_eq!(
            err.code.as_str(),
            "INVALID_SESSION_KEY",
            "#1455 resume_session blank sessionKey is rejected"
        );
        r.owner.shutdown().await.expect("#1310 shutdown");
        r.project.dispose().await.expect("#1310 dispose");
    }

    // #1604: the sessionKey gate is the family-shared `?.trim()` guard, so
    // every method that goes through `require_session_key` answers a
    // non-string the way the TS optional chain does, and treats JS WhiteSpace
    // (U+FEFF included) as blank. `session_record_status` /
    // `session_new` keep their own `must be a string.` gate on purpose (TS has
    // no `?.trim()` there — A7.8-INTERACTION-NONSTRING-OWNER-001-R1).
    fn require_key(value: JsValue) -> Result<JsString, RequestError> {
        require_session_key(&object([("sessionKey", value)]))
    }

    fn key_message(error: &RequestError) -> String {
        error
            .message
            .to_utf8()
            .expect("#1604 the gate's message is UTF-8")
    }

    #[test]
    fn a_non_string_session_key_throws_the_optional_chain_type_error() {
        let error = require_key(JsValue::Number(7.0))
            .expect_err("#1604 a number is not a trimmable string");
        assert_eq!("gateway_request_failed", error.code);
        assert_eq!(
            "input.sessionKey?.trim is not a function",
            key_message(&error)
        );
    }

    // #1604: with a null params object TS throws reading `sessionKey` off
    // null, so the guard must not fold that into the domain error either.
    #[test]
    fn a_null_params_object_reports_the_optional_chain_read_error() {
        let error =
            require_session_key(&JsValue::Null).expect_err("#1604 null params cannot be read");
        assert_eq!("gateway_request_failed", error.code);
        assert_eq!(
            "Cannot read properties of null (reading 'sessionKey')",
            key_message(&error)
        );
    }

    #[test]
    fn a_bom_only_session_key_is_blank() {
        let error = require_key(JsValue::from("\u{feff}"))
            .expect_err("#1604 a BOM-only key is blank after JS trim");
        assert_eq!("INVALID_SESSION_KEY", error.code);
        assert_eq!("sessionKey is required.", key_message(&error));
    }

    #[test]
    fn a_non_blank_session_key_passes_the_gate() {
        let key = require_key(JsValue::from("a8/session"))
            .expect("#1604 a non-blank string passes the gate");
        assert_eq!("a8/session", key.to_utf8().expect("#1604 key is UTF-8"));
    }

    // #1604: `close_project_sessions` gates `projectKey` with the same
    // `?.trim()` shape (A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001-R2 lists
    // it as this issue's second item), so a non-string must surface the
    // optional-chain TypeError instead of the `projectKey is required.`
    // domain error. The gate runs before any owner call, so the fixture is
    // only there to reach the request entry point.
    #[tokio::test]
    async fn close_project_sessions_non_string_project_key_throws_the_type_error() {
        let (_s, r) = std::panic::catch_unwind(crate::native_process::management_tests::setup)
            .expect("#1310 fixture");
        let params = object([("projectKey", JsValue::Number(7.0))]);
        let error = r
            .session_dir_request("close_project_sessions".into(), params)
            .await
            .expect_err("#1604 a number is not a trimmable projectKey");
        assert_eq!("gateway_request_failed", error.code);
        assert_eq!(
            "input.projectKey?.trim is not a function",
            key_message(&error)
        );
        r.owner.shutdown().await.expect("#1310 shutdown");
        r.project.dispose().await.expect("#1310 dispose");
    }

    // #1604: the other half of the same gate. Missing, blank (JS trim,
    // U+FEFF included) and non-UTF-8 keys all keep the plain `Error` shape TS
    // produces — `gateway_request_failed` and `projectKey is required.` — and
    // none of them reaches an owner call.
    #[tokio::test]
    async fn close_project_sessions_blank_or_surrogate_project_key_is_the_domain_error() {
        let (_s, r) = std::panic::catch_unwind(crate::native_process::management_tests::setup)
            .expect("#1310 fixture");
        for (label, value) in [
            ("blank", JsValue::from("  ")),
            ("bom-only", JsValue::from("\u{feff}")),
            (
                "lone-surrogate",
                JsValue::String(JsString::from_utf16(vec![0xd800])),
            ),
        ] {
            let error = r
                .session_dir_request(
                    "close_project_sessions".into(),
                    object([("projectKey", value)]),
                )
                .await
                .expect_err("#1604 a projectKey that cannot name a directory is refused");
            assert_eq!(
                "gateway_request_failed", error.code,
                "#1604 the {label} projectKey keeps the domain code"
            );
            assert_eq!(
                "projectKey is required.",
                key_message(&error),
                "#1604 the {label} projectKey keeps the domain message"
            );
        }
        r.owner.shutdown().await.expect("#1310 shutdown");
        r.project.dispose().await.expect("#1310 dispose");
    }
}
