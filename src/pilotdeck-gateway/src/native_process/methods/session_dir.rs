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
    // #2068: the TS read is `input.projectKey ?? fallbackProjectRoot`
    // (`ProjectRuntimeRegistry.resolve` / `resolveProjectKey`), so the shared
    // nullish read replaces the old `to_utf8().ok()` chain — a non-string key
    // answers the bare `path.resolve` TypeError instead of silently listing
    // the configured project, and a lone-surrogate key is a real (unserved)
    // key instead of the fallback.
    let project = super::resolve_project_key(params, &requests.options.project)?;
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

/// The `input.projectKey ? \`project=${input.projectKey}:\` : ""` read
/// (InProcessGateway.ts:2029): plain truthiness, not a trim gate — a blank or
/// BOM-only key keeps its prefix, a non-string truthy key is stringified into
/// it, and only the nullish spellings and the empty string drop it (#2068
/// probe: `project=7:` / `project= :` / `project=﻿:` all measured). The
/// rendering is the shared JS `String(value)` walk, and the prefix keeps the
/// key's raw UTF-16 units, so a lone surrogate survives into the sessionKey
/// the way the TS template literal keeps it. A rendering that throws (an own
/// non-callable `toString`, a too-deep array) is the boundary's
/// `gateway_request_failed` with the **bare** V8 text — the TS boundary
/// serializes `error.message` only, no `name:` prefix
/// (GatewayWsConnection.ts:252; probe `probe-2068-r2.out`; `runs.rs`
/// `template_text` gives its run-key template the same mapping).
fn project_prefix_units(params: &JsValue) -> Result<Vec<u16>, RequestError> {
    match super::input_property(params, "projectKey")?.as_ref() {
        None | Some(JsValue::Undefined) | Some(JsValue::Null) => Ok(Vec::new()),
        Some(JsValue::String(key)) if key.units().is_empty() => Ok(Vec::new()),
        Some(JsValue::Bool(false)) => Ok(Vec::new()),
        Some(JsValue::Number(n)) if *n == 0.0 || n.is_nan() => Ok(Vec::new()),
        Some(other) => {
            let rendered = crate::js_value::to_string_units_checked(other)
                .map_err(|error| RequestError::new(error.message()))?;
            let mut units: Vec<u16> = "project=".encode_utf16().collect();
            units.extend(rendered);
            units.push(u16::from(b':'));
            Ok(units)
        }
    }
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
        let project_prefix = project_prefix_units(params)?;
        // UTF-16 assembly: the prefix may carry the projectKey's lone
        // surrogates, and the sessionKey is a JsString on the wire either way.
        // `to_utf8` stays the channelKey's registered gate (A7.8); once it
        // passes, `units()` is that same text's UTF-16 encoding.
        let mut session_units = channel_key
            .to_utf8()
            .map_err(|_| RequestError::new("channelKey must be a valid UTF-8 string."))?
            .encode_utf16()
            .collect::<Vec<u16>>();
        session_units.push(u16::from(b':'));
        session_units.extend_from_slice(&project_prefix);
        session_units.extend(format!("s_{suffix}").encode_utf16());
        Ok(object([(
            "sessionKey",
            JsValue::String(JsString::from_utf16(session_units)),
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
    /// file history + tool results + subagents directories, then drop the
    /// session's SDK registry rows (`clearSdkSessionState`,
    /// createLocalGateway.ts:1160).
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
        // `registry.clearSdkSessionState(input.sessionKey)` — after every
        // storage delete succeeded, exactly like the baseline. The native
        // split (#2392): the MCP fields (server table, permission-mode
        // overrides, generated permission rules, the shared session→project
        // entry) live in `SdkMcpConfig`; `sdkThinkingOverrides` and
        // `sdkSessionConfigs.outputStyle` (the TS clear deletes the whole
        // config row, ProjectRuntimeRegistry.ts:776-786) live in
        // `session_overrides`. The registry half of the TS clear (task-budget
        // ledger, plugins, handler registrations) has no production instance
        // in this process yet — see `clear_session_state`'s contract
        // (runtime/sdk_session/registry.rs): it must be called **before**
        // the MCP clear, because the MCP clear drops the shared projects
        // entry its budget step reads. When that half is wired (#1839), it
        // belongs above this line, never below it.
        self.mcp
            .config()
            .clear_sdk_session_state(&JsValue::String(key.clone()));
        self.session_overrides.clear_thinking(&key);
        self.session_overrides.clear_output_style(&key);
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
            // The shared three-segment protocol (#1874): this closure is both
            // the first lookup and the busy-lease re-query, so the one checked
            // append below covers both paths.
            let live = || {
                let Some((owner, recorder)) =
                    super::super::assembly::live_recorder(&recorders, &key_clone)
                else {
                    return Ok(None);
                };
                recorder
                    .writer()
                    .record_session_metadata_checked(&key_clone, &turn_id, entry.clone())
                    .map_err(|e| RequestError::new(e.to_string()))?;
                Ok(Some(owner))
            };
            transcript_locks::append_metadata_with_lease(
                &home,
                &project_root,
                &key_clone,
                live,
                |storage| {
                    storage
                        .transcript
                        .record_session_metadata(&key_clone, &turn_id, entry.clone())
                        .map_err(|e| RequestError::new(e.to_string()))
                },
            )?;
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
        // #2083: the TS baseline runs a lone-surrogate `projectKey` to
        // completion (`closeProjectSessions` answers `{sessionKeys: []}`;
        // real-entry probe `_tmp/20261107-issue-2083/worker-r1/probe.out`), so the only
        // gate is the `?.trim()` shape above. The key converts lossily for the
        // configured-project comparison, like TS's own path handling; the raw
        // UTF-16 key still reaches the owner, so a session created under the
        // same spelling closes like any other project. Residual, accepted with
        // #2083: a key whose lossy form names the configured project (a real
        // path containing U+FFFD) is served.
        let project_key_str = String::from_utf16_lossy(project_key.units());
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
        let recorders = self.recorders.clone();
        let home = self.options.home.clone();
        let project_root = self.options.project.clone();
        // The shared three-segment protocol (#1874), with #1873's error shape:
        // the live closure below is both the first lookup and the busy-lease
        // re-query, so its one checked append — a writer that closed under us
        // is an error, not a `recorded: true` with nothing on disk — covers
        // both paths; a contended lease reports the retryable busy answer
        // like the rename and config paths; storage failures propagate instead
        // of collapsing into `recorded: false` (TS reserves that for "not live
        // and no cold provider", which never holds in the native process).
        let recorded = tokio::task::spawn_blocking(move || -> Result<bool, RequestError> {
            // The fence must outlive a cancelled request, so it moves into the
            // blocking closure instead of dropping with this future.
            let _write = write_fence;
            let live = || {
                let Some((owner, recorder)) =
                    super::super::assembly::live_recorder(&recorders, &session_key)
                else {
                    return Ok(None);
                };
                recorder
                    .writer()
                    .record_agent_status_message_checked(&session_key, &turn_id, &status)
                    .map_err(|e| RequestError::new(e.to_string()))?;
                Ok(Some(owner))
            };
            transcript_locks::append_metadata_with_lease(
                &home,
                &project_root,
                &session_key,
                live,
                |storage| {
                    storage
                        .transcript
                        .record_agent_status_message(&session_key, &turn_id, &status)
                        .map_err(|e| RequestError::new(e.to_string()))
                },
            )?;
            Ok(true)
        })
        .await
        .map_err(|e| RequestError::new(e.to_string()))??;
        Ok(object([("recorded", JsValue::Bool(recorded))]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::recorder::Recorder;
    use crate::{negative_assert, negative_assert_eq};

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
            .expect("#1873 the transcript lease probe itself must not fail")
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

    /// #1873: the status cold path answers the same retryable busy error the
    /// rename and config paths give when a session holds the writer lease.
    /// The pre-fix shape returned `recorded: false` — TS's "not live and no
    /// cold provider" answer, which never holds in the native process.
    #[tokio::test]
    async fn status_cold_write_reports_busy_while_a_session_holds_the_lease() {
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
                session_id: "d3/session".into(),
                now: None,
            })
            .expect("#1873 the transcript storage opens");
        // Stand in for `factory`: the lease is taken before restore and held
        // while the recorder is not yet in the map.
        let lease = transcript_locks::cold_write_lease(&storage.transcript_path)
            .expect("#1873 the transcript lease probe itself must not fail")
            .expect("#1873 the transcript lease is free before the session loads");
        let error = crate::runtime::turn::gateway::GatewayRequests::request(
            &requests,
            "record_agent_status_message".into(),
            crate::metadata::object([
                ("sessionKey", "d3-session".into()),
                ("turnId", "must-not-fork".into()),
                ("status", JsValue::Undefined),
            ]),
        )
        .await
        .expect_err(
            "#1873 a cold status append must not run beside a session that owns the writer",
        );
        negative_assert_eq!(
            error.code.as_str(),
            "SESSION_WRITER_BUSY",
            "#1873 the racing cold status append reports the retryable busy error"
        );
        drop(lease);
        negative_assert_eq!(
            std::fs::read_to_string(&storage.transcript_path)
                .map(|text| text.contains("must-not-fork"))
                .unwrap_or(false),
            false,
            "#1873 the refused status append wrote nothing"
        );
    }

    /// #1873 P2: a `<transcript>.lock` that cannot be opened is a storage
    /// failure. The pre-fix `try_writer_lease` folded every error into the
    /// busy answer, so a broken chat directory masqueraded as a session that
    /// would eventually drain; the request must fail with the underlying
    /// error, never the retryable busy code.
    #[tokio::test]
    async fn status_cold_write_surfaces_a_broken_lock_file_as_an_error() {
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
                session_id: "d5/session".into(),
                now: None,
            })
            .expect("#1873 the transcript storage opens");
        let mut lock = std::ffi::OsString::from(storage.transcript_path.as_os_str());
        lock.push(".lock");
        std::fs::create_dir(std::path::PathBuf::from(&lock))
            .expect("#1873 the broken ledger lock is in place");
        let error = crate::runtime::turn::gateway::GatewayRequests::request(
            &requests,
            "record_agent_status_message".into(),
            crate::metadata::object([
                ("sessionKey", "d5-session".into()),
                ("turnId", "broken-lock".into()),
                ("status", JsValue::Undefined),
            ]),
        )
        .await
        .expect_err("#1873 the status append surfaces the storage failure");
        negative_assert!(
            error.code.as_str() != "SESSION_WRITER_BUSY",
            "#1873 a broken lock file is a storage error, not the retryable busy answer"
        );
        negative_assert!(
            String::from_utf16_lossy(error.message.units())
                .contains("Could not open session ledger lock"),
            "#1873 the storage error names the ledger lock, got {}",
            String::from_utf16_lossy(error.message.units())
        );
    }

    /// #1873: the rename cold path answers the same storage-error separation
    /// the status caller got — a broken ledger lock is never the retryable
    /// busy answer for a rename either. The `#1873` `try_writer_lease`
    /// `Result` split reaches this caller through the shared
    /// `append_metadata_with_lease` protocol.
    #[tokio::test]
    async fn rename_cold_write_surfaces_a_broken_lock_file_as_an_error() {
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
                session_id: "d6/session".into(),
                now: None,
            })
            .expect("#1873 the rename transcript storage opens");
        let mut lock = std::ffi::OsString::from(storage.transcript_path.as_os_str());
        lock.push(".lock");
        std::fs::create_dir(std::path::PathBuf::from(&lock))
            .expect("#1873 the rename broken ledger lock is in place");
        let error = crate::runtime::turn::gateway::GatewayRequests::request(
            &requests,
            "rename_session".into(),
            crate::metadata::object([
                ("sessionKey", "d6-session".into()),
                ("value", "broken-lock-rename".into()),
            ]),
        )
        .await
        .expect_err("#1873 the rename append surfaces the storage failure");
        negative_assert!(
            error.code.as_str() != "SESSION_WRITER_BUSY",
            "#1873 the rename storage failure is never the retryable busy code"
        );
        negative_assert!(
            String::from_utf16_lossy(error.message.units())
                .contains("Could not open session ledger lock"),
            "#1873 the rename storage error names the ledger lock, got {}",
            String::from_utf16_lossy(error.message.units())
        );
    }

    /// #1874: a recorder whose writer already closed must surface the close
    /// as an error, not answer `updated: true` with nothing on disk. The live
    /// closure's checked append is what makes the closed writer visible; the
    /// unchecked form the cold path uses would silently no-op.
    #[tokio::test]
    async fn rename_reports_a_closed_writer_as_an_error() {
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
                project_root: project,
                pilot_home: requests.options.home.clone(),
                session_id: "d7/session".into(),
                now: None,
            })
            .expect("#1874 the transcript storage opens");
        let recorder = Arc::new(Recorder::from_writer(Arc::new(storage.transcript)));
        requests
            .recorders
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert("d7/session".into(), Arc::downgrade(&recorder));
        // A close that raced the request — the exact window fix-r2's checked
        // append exists for.
        recorder.writer().close();
        let error = crate::runtime::turn::gateway::GatewayRequests::request(
            &requests,
            "rename_session".into(),
            crate::metadata::object([
                ("sessionKey", "d7-session".into()),
                ("value", "must-not-vanish".into()),
            ]),
        )
        .await
        .expect_err("#1874 a closed session writer is an error, not a silent rename");
        negative_assert!(
            String::from_utf16_lossy(error.message.units()).contains("the session writer closed"),
            "#1874 the rename error names the closed session writer, got {}",
            String::from_utf16_lossy(error.message.units())
        );
    }

    /// #1874: the status twin of the closed-writer gate — a closed writer
    /// must be an error, not a `recorded: true` with nothing on disk.
    #[tokio::test]
    async fn status_reports_a_closed_writer_as_an_error() {
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
                project_root: project,
                pilot_home: requests.options.home.clone(),
                session_id: "d8/session".into(),
                now: None,
            })
            .expect("#1874 the transcript storage opens");
        let recorder = Arc::new(Recorder::from_writer(Arc::new(storage.transcript)));
        requests
            .recorders
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert("d8/session".into(), Arc::downgrade(&recorder));
        // A close that raced the request — the exact window fix-r2's checked
        // append exists for.
        recorder.writer().close();
        let error = crate::runtime::turn::gateway::GatewayRequests::request(
            &requests,
            "record_agent_status_message".into(),
            crate::metadata::object([
                ("sessionKey", "d8-session".into()),
                ("turnId", "must-not-vanish".into()),
                ("status", JsValue::Undefined),
            ]),
        )
        .await
        .expect_err("#1874 a closed session writer is an error, not a recorded status");
        negative_assert!(
            String::from_utf16_lossy(error.message.units()).contains("the session writer closed"),
            "#1874 the status error names the closed session writer, got {}",
            String::from_utf16_lossy(error.message.units())
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

    // #2090: request-level gate order for the session_dir family, measured on
    // both sides (`_tmp/20261107-issue-2090/`). Two gates can answer a bad or
    // absent key: the request-level isolated-project admission
    // (`native_process/requests.rs`) and this family's
    // `require_session_key` (`:180` / `:215` / `:232` / `:301`). Admission runs
    // before the method dispatch, so a `projectKey` the process does not serve
    // is answered as `gateway_request_failed` even when `sessionKey` is blank;
    // with the project admitted (or omitted, which falls back to the configured
    // one) the missing or blank key answers `INVALID_SESSION_KEY`. #2272: an
    // explicit `projectKey: null` is "not provided" for that admission (the
    // same nullish read TS gives it), so it falls back to the configured
    // project exactly like an omitted key; the resume/close blank-key
    // divergence stays registered in
    // `specs/gateway-parity/diffs/A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001-R9.md`.
    const BLANK_KEY_MESSAGE: &str = "sessionKey is required.";
    const ISOLATED_PROJECT_MESSAGE: &str =
        "The isolated native process serves only its configured project.";

    async fn blank_key_gate_order(method: &str) {
        let (_s, r) = std::panic::catch_unwind(crate::native_process::management_tests::setup)
            .expect("#2090 fixture");
        // A path no `createLocalGateway` registry can resolve, so the two
        // mechanisms that reject it (TS registry vs this process's admission)
        // are both forced to answer.
        const UNSERVED_PROJECT: &str = "/nonexistent/pdrs-2090";
        // (1) No `projectKey`: the fallback is the configured project, so the
        // family's own sessionKey gate answers — for an absent key and for a
        // blank one alike.
        for (label, params) in [
            ("missing", object([])),
            ("blank", object([("sessionKey", "   ".into())])),
        ] {
            let expected = format!("#2090 a {label} sessionKey is rejected");
            let error =
                crate::runtime::turn::gateway::GatewayRequests::request(&r, method.into(), params)
                    .await
                    .expect_err(&expected);
            negative_assert_eq!(
                error.code.as_str(),
                "INVALID_SESSION_KEY",
                "#2090 the sessionKey gate answers a missing or blank key under the configured project"
            );
            negative_assert_eq!(
                key_message(&error),
                BLANK_KEY_MESSAGE,
                "#2090 the gate message matches the TS provider gate"
            );
        }

        // (1b) #2272: an explicit `projectKey: null` is "not provided" for the
        // admission too, so the fallback is the configured project and the
        // family's sessionKey gate answers the blank key exactly as (1).
        {
            let params = object([("sessionKey", "   ".into()), ("projectKey", JsValue::Null)]);
            let error =
                crate::runtime::turn::gateway::GatewayRequests::request(&r, method.into(), params)
                    .await
                    .expect_err("#2272 the blank sessionKey is rejected under a null projectKey");
            negative_assert_eq!(
                error.code.as_str(),
                "INVALID_SESSION_KEY",
                "#2272 a null projectKey falls back like a missing one, so the sessionKey gate answers the blank key"
            );
            negative_assert_eq!(
                key_message(&error),
                BLANK_KEY_MESSAGE,
                "#2272 the gate message matches the TS provider gate under a null projectKey"
            );
        }

        // (2) Project outside the configured one: request admission answers
        // before the method dispatch, so the first code is the admission one.
        // The valid-key control answering identically is what proves admission
        // — not the key — decides here.
        for (label, params) in [
            (
                "blank key + unserved project",
                object([
                    ("sessionKey", "   ".into()),
                    ("projectKey", UNSERVED_PROJECT.into()),
                ]),
            ),
            (
                "valid key + unserved project",
                object([
                    ("sessionKey", "pdrs-2090-control".into()),
                    ("projectKey", UNSERVED_PROJECT.into()),
                ]),
            ),
        ] {
            let expected = format!("#2090 admission refuses {label}");
            let error =
                crate::runtime::turn::gateway::GatewayRequests::request(&r, method.into(), params)
                    .await
                    .expect_err(&expected);
            negative_assert_eq!(
                error.code.as_str(),
                "gateway_request_failed",
                "#2090 admission answers an unserved project before the sessionKey gate"
            );
            negative_assert_eq!(
                key_message(&error),
                ISOLATED_PROJECT_MESSAGE,
                "#2090 the admission message is the isolated-project one"
            );
        }

        // (3) #2272: a valid sessionKey with `projectKey: null` runs the
        // method on the configured project — the fallback the handler's
        // `resolve_project` gives a nullish key. Rust has no
        // unresolvable-fallback shape (the fallback is always the configured
        // project, R9 difference 4), so this is TS scenario A only; resume's
        // echo missing `projectKey` and close answering `undefined` are the
        // registered R9 divergences, kept as they are.
        {
            let params = object([
                ("sessionKey", "pdrs-2090-control".into()),
                ("projectKey", JsValue::Null),
            ]);
            let result =
                crate::runtime::turn::gateway::GatewayRequests::request(&r, method.into(), params)
                    .await;
            match method {
                "resume_session" => {
                    let value = result
                        .expect("#2272 resume_session answers a valid key under a null projectKey");
                    let echoed = value
                        .get("sessionKey")
                        .and_then(JsValue::as_string)
                        .cloned()
                        .expect(
                            "#2272 resume_session echoes the sessionKey under a null projectKey",
                        );
                    negative_assert_eq!(
                        String::from_utf16_lossy(echoed.units()),
                        "pdrs-2090-control",
                        "#2272 resume_session echoes the key it was given under a null projectKey"
                    );
                    negative_assert!(
                        value.get("projectKey").is_none(),
                        "#2272 resume_session keeps the registered R9 echo shape with no projectKey"
                    );
                }
                "close_session" => {
                    negative_assert!(
                        matches!(result, Ok(JsValue::Undefined)),
                        "#2272 close_session answers undefined for a valid key under a null projectKey"
                    );
                }
                "delete_session" => {
                    let error = result.expect_err(
                        "#2272 delete_session looks for a valid key in the configured project under a null projectKey",
                    );
                    negative_assert_eq!(
                        error.code.as_str(),
                        "SESSION_NOT_FOUND",
                        "#2272 delete_session answers SESSION_NOT_FOUND, not an admission refusal"
                    );
                }
                "rename_session" | "tag_session" => {
                    let value = result
                        .expect("#2272 rename/tag answers a valid key under a null projectKey");
                    negative_assert_eq!(
                        value.get("updated").and_then(JsValue::as_bool),
                        Some(true),
                        "#2272 rename/tag reports updated:true on the configured project"
                    );
                }
                _ => unreachable!("blank_key_gate_order called for an unknown method"),
            }
        }

        r.owner.shutdown().await.expect("#2090 shutdown");
        r.project.dispose().await.expect("#2090 dispose");
    }

    #[tokio::test]
    async fn resume_session_blank_key_gate_order() {
        blank_key_gate_order("resume_session").await;
    }

    #[tokio::test]
    async fn close_session_blank_key_gate_order() {
        blank_key_gate_order("close_session").await;
    }

    #[tokio::test]
    async fn delete_session_blank_key_gate_order() {
        blank_key_gate_order("delete_session").await;
    }

    /// The session keys one `snapshot_state` table holds, as UTF-8 strings.
    fn snapshot_table_keys(
        config: &crate::runtime::mcp::sdk::SdkMcpConfig,
        field: &str,
    ) -> Vec<String> {
        let rows = config
            .snapshot_state()
            .get(field)
            .cloned()
            .unwrap_or(JsValue::Undefined);
        let JsValue::Array(ref rows) = rows else {
            return Vec::new();
        };
        rows.iter()
            .map(|row| match row {
                JsValue::Array(parts) => parts
                    .first()
                    .and_then(JsValue::as_string)
                    .map(|key| String::from_utf16_lossy(key.units()))
                    .unwrap_or_default(),
                _ => String::new(),
            })
            .collect()
    }

    /// Seed the `clearSdkSessionState` halves this process owns for one key:
    /// the MCP permission tables (an override row plus its generated rule,
    /// exactly what `setMcpPermissionModeOverrideForSdk` leaves), the thinking
    /// override, and the output-style selection (`sdkSessionConfigs`'s
    /// `outputStyle` row, the other `session_overrides` half the TS clear
    /// drops with the config row).
    async fn seed_sdk_session_state(requests: &Requests, session: &str) {
        let input = object([
            ("sessionKey", session.into()),
            ("serverName", "pdrs-2392-server".into()),
            ("mode", "auto".into()),
        ]);
        requests
            .mcp
            .config()
            .set_mcp_permission_mode_override(&input)
            .await
            .expect("#2392 the seeded permission override commits");
        // The lease-acquired sync shape (the wiring layer calls this with
        // `lease.rules().ask`, ProjectRuntimeRegistry.ts:305-308): generates
        // the session's permission-rule row for the override just seeded.
        requests
            .mcp
            .config()
            .sync_permission_rules(&session.into(), &mut Vec::new());
        requests
            .session_overrides
            .set_thinking(&session.into(), object([("enabled", JsValue::Bool(true))]));
        requests
            .session_overrides
            .set_output_style(&session.into(), "pdrs-2392-style".into());
        requests
            .session_overrides
            .set_permission_mode(&session.into(), "acceptEdits".into());
    }

    /// #2392: `deleteSession` ends with `clearSdkSessionState`
    /// (createLocalGateway.ts:1160), so the deleted session's registry rows
    /// must not outlive its files — the MCP tables, the thinking override,
    /// and the output-style selection (the config row the TS clear drops
    /// carries it, ProjectRuntimeRegistry.ts:779). The permission-mode
    /// selection is the counter-scope: the TS clear never touches
    /// `sessionOverrides`, so the native delete must keep it.
    #[tokio::test]
    async fn session_delete_clears_the_sdk_session_state() {
        let (_scratch, requests) = crate::native_process::management_tests::setup();
        let session: JsString = "a8-session".into();
        seed_sdk_session_state(&requests, "a8-session").await;
        let config = requests.mcp.config();
        negative_assert!(
            snapshot_table_keys(config, "overrides").contains(&"a8-session".to_string()),
            "#2392 delete starts with a seeded MCP permission override"
        );
        negative_assert!(
            snapshot_table_keys(config, "rules").contains(&"a8-session".to_string()),
            "#2392 delete starts with a seeded generated permission rule"
        );
        negative_assert!(
            requests.session_overrides.thinking(&session).is_some(),
            "#2392 delete starts with a seeded thinking override"
        );
        negative_assert!(
            requests.session_overrides.output_style(&session).is_some(),
            "#2392 delete starts with a seeded output-style selection"
        );

        let result = crate::runtime::turn::gateway::GatewayRequests::request(
            &requests,
            "delete_session".into(),
            object([("sessionKey", "a8-session".into())]),
        )
        .await;
        negative_assert!(
            matches!(result, Ok(JsValue::Undefined)),
            "#2392 delete_session answers undefined after deleting the transcript"
        );
        negative_assert!(
            !snapshot_table_keys(config, "overrides").contains(&"a8-session".to_string()),
            "#2392 delete drops the session's MCP permission overrides"
        );
        negative_assert!(
            !snapshot_table_keys(config, "rules").contains(&"a8-session".to_string()),
            "#2392 delete drops the session's generated permission rules"
        );
        negative_assert!(
            requests.session_overrides.thinking(&session).is_none(),
            "#2392 delete drops the session's thinking override"
        );
        negative_assert!(
            requests.session_overrides.output_style(&session).is_none(),
            "#2392 delete drops the session's output-style selection"
        );
        negative_assert_eq!(
            requests
                .session_overrides
                .permission_mode(&session)
                .map(|mode| String::from_utf16_lossy(mode.units())),
            Some("acceptEdits".to_string()),
            "#2392 delete keeps the permission-mode selection, which the TS clear does not touch"
        );

        requests.owner.shutdown().await.expect("#2392 shutdown");
        requests.project.dispose().await.expect("#2392 dispose");
    }

    /// #2392: `clearSdkSessionState` runs only after every storage delete
    /// succeeded (createLocalGateway.ts:1150-1160); `SESSION_NOT_FOUND`
    /// leaves before any clear, so the seeded rows survive the refusal.
    #[tokio::test]
    async fn session_delete_not_found_keeps_the_sdk_session_state() {
        let (_scratch, requests) = crate::native_process::management_tests::setup();
        let session: JsString = "pdrs-2392-ghost".into();
        seed_sdk_session_state(&requests, "pdrs-2392-ghost").await;
        let error = crate::runtime::turn::gateway::GatewayRequests::request(
            &requests,
            "delete_session".into(),
            object([("sessionKey", "pdrs-2392-ghost".into())]),
        )
        .await
        .expect_err("#2392 delete_session refuses the session without a transcript");
        negative_assert_eq!(
            error.code.as_str(),
            "SESSION_NOT_FOUND",
            "#2392 the missing transcript answers SESSION_NOT_FOUND"
        );
        let config = requests.mcp.config();
        negative_assert!(
            snapshot_table_keys(config, "overrides").contains(&"pdrs-2392-ghost".to_string()),
            "#2392 the refusal keeps the session's MCP permission overrides"
        );
        negative_assert!(
            snapshot_table_keys(config, "rules").contains(&"pdrs-2392-ghost".to_string()),
            "#2392 the refusal keeps the session's generated permission rules"
        );
        negative_assert!(
            requests.session_overrides.thinking(&session).is_some(),
            "#2392 the refusal keeps the session's thinking override"
        );
        negative_assert!(
            requests.session_overrides.output_style(&session).is_some(),
            "#2392 the refusal keeps the session's output-style selection"
        );

        requests.owner.shutdown().await.expect("#2392 shutdown");
        requests.project.dispose().await.expect("#2392 dispose");
    }

    #[tokio::test]
    async fn rename_session_blank_key_gate_order() {
        blank_key_gate_order("rename_session").await;
    }

    #[tokio::test]
    async fn tag_session_blank_key_gate_order() {
        blank_key_gate_order("tag_session").await;
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

    // #1604: the other half of the same gate. Missing and blank (JS trim,
    // U+FEFF included) keys keep the plain `Error` shape TS produces —
    // `gateway_request_failed` and `projectKey is required.` — and neither
    // reaches an owner call.
    #[tokio::test]
    async fn close_project_sessions_blank_project_key_is_the_domain_error() {
        let (_s, r) = std::panic::catch_unwind(crate::native_process::management_tests::setup)
            .expect("#1310 fixture");
        for (label, value) in [
            ("blank", JsValue::from("  ")),
            ("bom-only", JsValue::from("\u{feff}")),
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

    // #2083: a lone-surrogate `projectKey` is a non-blank key, and the TS
    // baseline runs it to completion (`closeProjectSessions` answers
    // `{sessionKeys: []}` on both branches — real-entry probe
    // `_tmp/20261107-issue-2083/worker-r1/probe.out`). It therefore passes the trim gate
    // and reaches the same configured-project check every other foreign key
    // fails, on both the close and the resume branch, instead of being
    // answered the missing-key domain error.
    #[tokio::test]
    async fn close_project_sessions_surrogate_project_key_reaches_the_capability_gate() {
        let (_s, r) = std::panic::catch_unwind(crate::native_process::management_tests::setup)
            .expect("#1310 fixture");
        for (label, resume) in [("close", false), ("resume", true)] {
            let error = r
                .session_dir_request(
                    "close_project_sessions".into(),
                    object([
                        (
                            "projectKey",
                            JsValue::String(JsString::from_utf16(vec![0xd800])),
                        ),
                        ("resume", JsValue::Bool(resume)),
                    ]),
                )
                .await
                .expect_err("#2083 the surrogate projectKey is not a missing key");
            assert_eq!(
                "CAPABILITY_UNAVAILABLE", error.code,
                "#2083 the surrogate projectKey reaches the capability gate ({label} branch)"
            );
            assert_eq!(
                "The isolated native process serves only its configured project.",
                key_message(&error),
                "#2083 the surrogate projectKey keeps the capability message ({label} branch)"
            );
        }
        r.owner.shutdown().await.expect("#1310 shutdown");
        r.project.dispose().await.expect("#1310 dispose");
    }

    // #2083 r1 (DS sibling#1): the admission gate in `GatewayRequests::request`
    // is the same source as the handler's configured-project check above, so
    // it converts the key lossily too. Through the real entry an unserved
    // surrogate is answered by the admission gate itself (`gateway_request_failed`
    // + the isolation message, like every foreign key), and the registered
    // residual — a key whose lossy form names a configured project path
    // containing U+FFFD — is admitted and served on both layers alike
    // (`resume` arm: the gate sits before the branch).
    #[tokio::test]
    async fn close_project_sessions_surrogate_project_key_meets_the_same_lossy_gate_at_the_entry() {
        let (_s, mut r) = std::panic::catch_unwind(crate::native_process::management_tests::setup)
            .expect("#1310 fixture");
        let error = crate::runtime::turn::gateway::GatewayRequests::request(
            &r,
            "close_project_sessions".into(),
            object([
                (
                    "projectKey",
                    JsValue::String(JsString::from_utf16(vec![0xd800])),
                ),
                ("resume", JsValue::Bool(true)),
            ]),
        )
        .await
        .expect_err("#2083 an unserved surrogate projectKey is refused at the entry");
        assert_eq!(
            "gateway_request_failed", error.code,
            "#2083 the entry answers the admission refusal for the surrogate projectKey"
        );
        assert_eq!(
            "The isolated native process serves only its configured project.",
            key_message(&error),
            "#2083 the entry keeps the isolation message for the surrogate projectKey"
        );
        let residual = format!("{}-pd-\u{FFFD}", r.options.project.to_string_lossy());
        r.options.project = std::path::PathBuf::from(&residual);
        let residual_units: Vec<u16> = residual
            .encode_utf16()
            .map(|unit| if unit == 0xFFFD { 0xD800 } else { unit })
            .collect();
        let served = crate::runtime::turn::gateway::GatewayRequests::request(
            &r,
            "close_project_sessions".into(),
            object([
                (
                    "projectKey",
                    JsValue::String(JsString::from_utf16(residual_units)),
                ),
                ("resume", JsValue::Bool(true)),
            ]),
        )
        .await
        .expect("#2083 the residual lossy form of the configured project is served at the entry");
        match served.get("sessionKeys") {
            Some(JsValue::Array(keys)) => assert!(
                keys.is_empty(),
                "#2083 the residual project has no sessions to close"
            ),
            other => {
                panic!("#2083 the resume arm must answer the sessionKeys array, got {other:?}")
            }
        }
        r.owner.shutdown().await.expect("#1310 shutdown");
        r.project.dispose().await.expect("#1310 dispose");
    }

    // #2564: request-level `projectKey: null` for the three family members
    // whose subject key is the project itself. The admission gate reads the
    // key nullish (#2272), so a null reaches each method's own read, and the
    // TS baselines differ per method: `closeProjectSessions` gates with
    // `!input.projectKey?.trim()` (InProcessGateway.ts:2037) so a null is the
    // missing-key domain error; `listSessions` resolves
    // `input.projectKey ?? fallbackProjectRoot` (ProjectRuntimeRegistry.ts:1542)
    // so it lists the configured project; `newSession` reads
    // `input.projectKey ? \`project=${…}:\` : ""` (InProcessGateway.ts:2029)
    // so the null omits the prefix. Node v22.22.0 end-to-end probe:
    // `_tmp/20261026-issue-2562/ts-probe/probe-submit.out`.
    #[tokio::test]
    async fn close_project_sessions_null_project_key_is_the_domain_error() {
        let (_s, r) = std::panic::catch_unwind(crate::native_process::management_tests::setup)
            .expect("#1310 fixture");
        let error = crate::runtime::turn::gateway::GatewayRequests::request(
            &r,
            "close_project_sessions".into(),
            object([("projectKey", JsValue::Null)]),
        )
        .await
        .expect_err("#2564 a null projectKey is refused by the close_project_sessions gate");
        negative_assert_eq!(
            error.code.as_str(),
            "gateway_request_failed",
            "#2564 a null projectKey keeps the domain code"
        );
        negative_assert_eq!(
            key_message(&error),
            "projectKey is required.",
            "#2564 a null projectKey keeps the domain message"
        );
        r.owner.shutdown().await.expect("#1310 shutdown");
        r.project.dispose().await.expect("#1310 dispose");
    }

    #[tokio::test]
    async fn list_sessions_null_project_key_lists_the_configured_project() {
        let (_s, r) = std::panic::catch_unwind(crate::native_process::management_tests::setup)
            .expect("#1310 fixture");
        let value = crate::runtime::turn::gateway::GatewayRequests::request(
            &r,
            "list_sessions".into(),
            object([("projectKey", JsValue::Null)]),
        )
        .await
        .expect("#2564 a null projectKey lists the configured project");
        negative_assert!(
            matches!(value.get("sessions"), Some(JsValue::Array(_))),
            "#2564 a null projectKey falls back to the configured project's catalog"
        );
        r.owner.shutdown().await.expect("#1310 shutdown");
        r.project.dispose().await.expect("#1310 dispose");
    }

    #[tokio::test]
    async fn new_session_null_project_key_omits_the_project_prefix() {
        let (_s, r) = std::panic::catch_unwind(crate::native_process::management_tests::setup)
            .expect("#1310 fixture");
        let key_of = |params| {
            crate::runtime::turn::gateway::GatewayRequests::request(
                &r,
                "new_session".into(),
                params,
            )
        };
        let null_key = key_of(object([
            ("channelKey", "pdrs-2564".into()),
            ("projectKey", JsValue::Null),
        ]))
        .await
        .expect("#2564 a null projectKey generates a session key")
        .get("sessionKey")
        .and_then(JsValue::as_string)
        .cloned()
        .expect("#2564 new_session answers a sessionKey");
        negative_assert!(
            String::from_utf16_lossy(null_key.units()).starts_with("pdrs-2564:s_"),
            "#2564 a null projectKey omits the project= prefix"
        );
        // Control: a served string is truthy on both sides, so the prefix
        // appears and the null above is proven to be the nullish read.
        let served_key = key_of(object([
            ("channelKey", "pdrs-2564".into()),
            (
                "projectKey",
                JsValue::String(r.options.project.to_string_lossy().into_owned().into()),
            ),
        ]))
        .await
        .expect("#2564 the configured project string generates a session key")
        .get("sessionKey")
        .and_then(JsValue::as_string)
        .cloned()
        .expect("#2564 new_session answers a sessionKey for the served project");
        negative_assert!(
            String::from_utf16_lossy(served_key.units()).starts_with("pdrs-2564:project="),
            "#2564 a served project string keeps the project= prefix"
        );
        r.owner.shutdown().await.expect("#1310 shutdown");
        r.project.dispose().await.expect("#1310 dispose");
    }

    // #2068: the family's projectKey read is the shared `?? fallback` shape —
    // every TS chain that consumes it (`ProjectRuntimeRegistry.resolve` for
    // listSessions, `dialog.projects.resolveProjectKey` for delete/rename/tag)
    // starts at Node's `path.resolve`, so a non-string key answers the
    // `TypeError [ERR_INVALID_ARG_TYPE]` whose `code` the boundary preserves
    // (`hasStructuredErrorCode`; the frozen oracle's `project-number` WS
    // original is `ERR_INVALID_ARG_TYPE`), never the configured-project
    // fallback the old `to_utf8().ok()` chain silently gave. The received
    // descriptions come from the shared `argument_received` port: the `-0` and
    // constructor spellings below are the shapes `runtime/manager/tests.rs`
    // and the `probe-2068-r2.out` cells pinned (a hand-written frame keeps
    // `-0`'s sign through JSON), and the last two cells are Node's
    // description phase throwing its own bare TypeError — no `code`, so the
    // boundary answers `gateway_request_failed` with the bare message.
    #[tokio::test]
    async fn list_sessions_non_string_project_key_answers_the_path_resolve_type_error() {
        let (_s, r) = std::panic::catch_unwind(crate::native_process::management_tests::setup)
            .expect("#1310 fixture");
        for (label, value, received) in [
            ("number", JsValue::Number(7.0), "type number (7)"),
            ("negative-zero", JsValue::Number(-0.0), "type number (-0)"),
            ("boolean", JsValue::Bool(true), "type boolean (true)"),
            ("array", JsValue::Array(Vec::new()), "an instance of Array"),
            ("object", object([]), "an instance of Object"),
            (
                "constructor-null",
                object([("constructor", JsValue::Null)]),
                "[Object]",
            ),
            (
                "constructor-name-null",
                object([("constructor", object([("name", JsValue::Null)]))]),
                "an instance of null",
            ),
            (
                "constructor-name-array",
                object([(
                    "constructor",
                    object([("name", JsValue::Array(Vec::new()))]),
                )]),
                "an instance of ",
            ),
        ] {
            let error = r
                .session_dir_request("list_sessions".into(), object([("projectKey", value)]))
                .await
                .expect_err("#2068 a non-string projectKey is the path.resolve TypeError");
            negative_assert_eq!(
                error.code.as_str(),
                "ERR_INVALID_ARG_TYPE",
                "#2068 the {label} projectKey answers the Node TypeError code"
            );
            negative_assert_eq!(
                key_message(&error),
                format!("The \"paths[0]\" argument must be of type string. Received {received}"),
                "#2068 the {label} projectKey keeps the path.resolve TypeError message"
            );
        }
        for (label, value, message) in [
            (
                "constructor-number",
                object([("constructor", JsValue::Number(5.0))]),
                "Cannot use 'in' operator to search for 'name' in 5",
            ),
            (
                "constructor-name-tostring-shadow",
                object([(
                    "constructor",
                    object([("name", object([("toString", "x".into())]))]),
                )]),
                "Cannot convert object to primitive value",
            ),
        ] {
            let error = r
                .session_dir_request("list_sessions".into(), object([("projectKey", value)]))
                .await
                .expect_err("#2068 a non-string projectKey is the path.resolve TypeError");
            negative_assert_eq!(
                error.code.as_str(),
                "gateway_request_failed",
                "#2068 the {label} projectKey's description throw answers the boundary code"
            );
            negative_assert_eq!(
                key_message(&error),
                message,
                "#2068 the {label} projectKey's description throw keeps the bare message"
            );
        }
        r.owner.shutdown().await.expect("#1310 shutdown");
        r.project.dispose().await.expect("#1310 dispose");
    }

    // #2068: strings the old `to_utf8().ok()` filter folded onto the
    // configured project are real keys on the TS side (`path.resolve` takes
    // any string; the probe's blank / BOM-only / lone-surrogate projectKeys
    // all reached the catalog as project roots), so each answers the
    // unserved-project refusal instead of listing the configured project.
    // The null cell is the fallback control #2564 pinned at the entry.
    #[tokio::test]
    async fn list_sessions_string_project_keys_are_real_keys_not_the_fallback() {
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
                .session_dir_request("list_sessions".into(), object([("projectKey", value)]))
                .await
                .expect_err("#2068 a string projectKey is a real key, never the fallback");
            negative_assert_eq!(
                error.code.as_str(),
                "CAPABILITY_UNAVAILABLE",
                "#2068 the {label} projectKey reaches the capability gate"
            );
            negative_assert_eq!(
                key_message(&error),
                ISOLATED_PROJECT_MESSAGE,
                "#2068 the {label} projectKey keeps the isolation message"
            );
        }
        let value = r
            .session_dir_request(
                "list_sessions".into(),
                object([("projectKey", JsValue::Null)]),
            )
            .await
            .expect("#2068 a null projectKey still falls back to the configured project");
        negative_assert!(
            matches!(value.get("sessions"), Some(JsValue::Array(_))),
            "#2068 the null fallback lists the configured project"
        );
        r.owner.shutdown().await.expect("#1310 shutdown");
        r.project.dispose().await.expect("#1310 dispose");
    }

    // #2068: `newSession`'s projectKey read is plain truthiness
    // (InProcessGateway.ts:2029, probe `probe-2068.out`): the prefix keeps a
    // blank, BOM-only, lone-surrogate and non-string truthy key, and only the
    // nullish spellings and the falsy scalars drop it. The lone surrogate is
    // asserted on raw UTF-16 units because it must survive into the
    // sessionKey the way the TS template literal keeps it.
    #[tokio::test]
    async fn new_session_project_key_prefix_follows_ts_truthiness() {
        let (_s, r) = std::panic::catch_unwind(crate::native_process::management_tests::setup)
            .expect("#1310 fixture");
        let key_of = |project_key: JsValue| {
            r.session_dir_request(
                "new_session".into(),
                object([
                    ("channelKey", "pdrs-2068".into()),
                    ("projectKey", project_key),
                ]),
            )
        };
        let key_units = |result: Result<JsValue, RequestError>, gate: &str| -> Vec<u16> {
            result
                .expect(gate)
                .get("sessionKey")
                .and_then(JsValue::as_string)
                .cloned()
                .expect("#2068 new_session answers a sessionKey")
                .units()
                .to_vec()
        };
        for (label, value, prefix) in [
            ("number", JsValue::Number(7.0), "project=7:"),
            ("true", JsValue::Bool(true), "project=true:"),
            ("blank", JsValue::from(" "), "project= :"),
            ("bom-only", JsValue::from("\u{feff}"), "project=\u{feff}:"),
        ] {
            let units = key_units(
                key_of(value).await,
                "#2068 a truthy projectKey generates a session key",
            );
            let expected: Vec<u16> = format!("pdrs-2068:{prefix}s_").encode_utf16().collect();
            negative_assert!(
                units.starts_with(&expected),
                "#2068 the {label} projectKey keeps its truthiness prefix"
            );
        }
        // The lone surrogate: raw unit between `project=` and `:`, not a
        // dropped prefix and not a lossy replacement.
        let units = key_units(
            key_of(JsValue::String(JsString::from_utf16(vec![0xd800]))).await,
            "#2068 a surrogate projectKey generates a session key",
        );
        let expected: Vec<u16> = "pdrs-2068:project=".encode_utf16().collect();
        negative_assert_eq!(
            units.get(..expected.len() + 2),
            Some(
                expected
                    .iter()
                    .copied()
                    .chain([0xd800, u16::from(b':')])
                    .collect::<Vec<u16>>()
                    .as_slice()
            ),
            "#2068 the surrogate projectKey keeps its raw unit inside the prefix"
        );
        // Falsy spellings: the nullish pair, the empty string, 0, NaN and false.
        for (label, value) in [
            ("null", JsValue::Null),
            ("empty", JsValue::from("")),
            ("zero", JsValue::Number(0.0)),
            ("nan", JsValue::Number(f64::NAN)),
            ("false", JsValue::Bool(false)),
        ] {
            let units = key_units(
                key_of(value).await,
                "#2068 a falsy projectKey generates a session key",
            );
            let expected: Vec<u16> = "pdrs-2068:s_".encode_utf16().collect();
            negative_assert!(
                units.starts_with(&expected),
                "#2068 the {label} projectKey drops the prefix"
            );
        }
        r.owner.shutdown().await.expect("#1310 shutdown");
        r.project.dispose().await.expect("#1310 dispose");
    }

    // #2068 fix-r1: a truthy projectKey whose JS `String()` rendering throws
    // (an own non-callable `toString` shadowing the inherited one — JSON
    // cannot carry a callable, so any wire-visible one is the shadow) fails
    // `new_session` with the boundary's `gateway_request_failed` and the
    // **bare** V8 text: the TS boundary serializes `error.message` only, no
    // `name:` prefix (GatewayWsConnection.ts:252; probe `probe-2068-r2.out`;
    // the #1604 q3/q7 probes pinned the same mapping for the run-key
    // template).
    #[tokio::test]
    async fn new_session_throwing_project_key_prefix_answers_the_boundary_typeerror() {
        let (_s, r) = std::panic::catch_unwind(crate::native_process::management_tests::setup)
            .expect("#1310 fixture");
        let error = r
            .session_dir_request(
                "new_session".into(),
                object([
                    ("channelKey", "pdrs-2068".into()),
                    ("projectKey", object([("toString", JsValue::Null)])),
                ]),
            )
            .await
            .expect_err("#2068 a throwing projectKey rendering fails new_session");
        negative_assert_eq!(
            error.code.as_str(),
            "gateway_request_failed",
            "#2068 the ToPrimitive failure answers the boundary code"
        );
        negative_assert_eq!(
            key_message(&error),
            "Cannot convert object to primitive value",
            "#2068 the ToPrimitive failure keeps the bare V8 text"
        );
        r.owner.shutdown().await.expect("#1310 shutdown");
        r.project.dispose().await.expect("#1310 dispose");
    }
}
