use super::{Options, assembly::Recorders};
use crate::{
    config::Env,
    metadata::object,
    runtime::{
        project::{ProjectResources, ProjectRuntimeRegistry},
        turn::{
            submit::{
                ChannelAttachmentOrigin, NativeSubmitSnapshots, SubmitError, SubmitFuture,
                SubmitSnapshots,
            },
            submit_host::{SubmitAttachmentLease, SubmitCwdService, SubmitHostServices},
        },
    },
    storage::{ContentAddressedWorkspaceSnapshotRecorder, RunRef},
    upload::UploadStore,
};
use pilotdeck_protocol::{JsString, JsValue};
use std::io::Write as IoWrite;
use std::path::{Path, PathBuf};
use std::sync::Arc;

mod attachments;

pub(super) use attachments::{compose_attachment_turn, compose_attachment_turn_with_cache};
// Preserve the native_process helper paths even when only tests use the exports.
#[cfg(test)]
pub(super) use attachments::swap_test_hook;
#[allow(unused_imports)]
pub(super) use attachments::{
    attachment_identity_matches, canonical_upload_store, handle_bound_to_upload_areas,
};

#[derive(Clone)]
pub struct Services {
    pub options: Options,
    pub project: Arc<ProjectRuntimeRegistry>,
    pub env: Env,
    pub recorders: Recorders,
    pub owner: crate::runtime::session::SessionOwner,
    /// Extracted once at startup from `PersistenceResources`; `None` when
    /// `--storage-root` was not supplied and snapshots are disabled.
    pub snapshot_recorder: Option<Arc<ContentAddressedWorkspaceSnapshotRecorder>>,
    pub upload_store: Arc<UploadStore>,
}

fn failure(message: impl Into<String>) -> SubmitError {
    SubmitError::new("gateway_submit_failed", message)
}
fn unavailable(message: &str) -> SubmitError {
    SubmitError::new("CAPABILITY_UNAVAILABLE", message)
}
struct Cwd(std::path::PathBuf);
impl SubmitCwdService for Cwd {
    fn set(&self, _: &JsString, cwd: &JsString) -> Result<(), SubmitError> {
        if cwd
            .to_utf8()
            .is_ok_and(|cwd| crate::metadata::resolve(&self.0, &cwd) == self.0)
        {
            Ok(())
        } else {
            Err(unavailable(
                "The isolated native process cannot change its configured project.",
            ))
        }
    }
}
impl Services {
    fn compose_native_attachments<'a>(
        &'a self,
        message: &'a JsValue,
        attachments: Vec<JsValue>,
        origin: Option<ChannelAttachmentOrigin>,
    ) -> SubmitFuture<'a, Result<JsValue, SubmitError>> {
        Box::pin(async move {
            super::assembly::validate_profile(&self.options, &self.env)
                .await
                .map_err(|error| unavailable(&error.to_string()))?;
            if !matches!(message, JsValue::String(_)) {
                return Err(failure("message must be a string."));
            }
            if attachments.is_empty() {
                return Ok(object([("type", "text".into()), ("text", message.clone())]));
            }
            let project_dir = self.options.project.clone();
            let cache = origin.map(|origin| match origin {
                ChannelAttachmentOrigin::Feishu => (
                    self.options.home.clone(),
                    self.options.home.join("im-attachments/feishu"),
                ),
                ChannelAttachmentOrigin::Weixin => (
                    self.options.home.clone(),
                    self.options.home.join("im-attachments/weixin"),
                ),
                ChannelAttachmentOrigin::Wecom => (
                    std::env::temp_dir(),
                    std::env::temp_dir().join("pilotdeck-wecom-media"),
                ),
            });
            let message = message.clone();
            // P1-3/concurrency#5: the whole composition — the once-per-prepare
            // lease-root canonicalize (simplify#2), the per-attachment stat and
            // read — runs on a blocking thread so the async executor is not
            // blocked.
            tokio::task::spawn_blocking(move || match cache {
                Some((base, root)) => compose_attachment_turn_with_cache(
                    &project_dir,
                    &message,
                    &attachments,
                    Some((&base, &root)),
                ),
                None => compose_attachment_turn(&project_dir, &message, &attachments),
            })
            .await
            .map_err(|e| failure(e.to_string()))?
        })
    }
}

impl SubmitHostServices for Services {
    fn bind_project<'a>(
        &'a self,
        input: &'a mut JsValue,
    ) -> SubmitFuture<'a, Result<Option<Arc<dyn SubmitHostServices>>, SubmitError>> {
        Box::pin(async move {
            let selected = super::project_scope::select(&self.owner, &self.options, input, false)
                .map_err(|error| {
                SubmitError::new(&error.code, String::from_utf16_lossy(error.message.units()))
            })?;
            let mut services = self.clone();
            services.options = selected.options;
            // Persistence snapshots use only the shared captured storage-root
            // environment. The project identity is resolved by workspace_id
            // against this selected view, while the one provider stays shared.
            Ok(Some(Arc::new(ScopedServices {
                services,
                _reservation: selected.reservation,
            }) as Arc<dyn SubmitHostServices>))
        })
    }

    fn workspace_id(&self, input: &JsValue) -> Result<Option<JsString>, SubmitError> {
        // #2562: the filter skips `undefined` *and* `null`, the same nullish
        // read the request admission gate got in #2272 (requests.rs). The
        // submit path's TS source is `workspaceIdResolver(input.projectKey)`
        // (InProcessGateway.ts:954) and the local resolver is
        // `resolveProjectStorageId(requestedProjectKey ?? fallbackProjectRoot,
        // pilotHome)` (createLocalGateway.ts:1026-1028), so an explicit null is
        // "not provided" and takes the configured-project fallback instead of
        // the type error. Node v22.22.0 probes:
        // `_tmp/20261026-issue-2562/ts-probe/probe-{resolver,submit}.out`.
        // A non-string keeps the type error (the TS resolver chain throws on
        // it too — `resolve(7)` is ERR_INVALID_ARG_TYPE).
        if let Some(project) = input
            .get("projectKey")
            .filter(|v| !matches!(v, JsValue::Undefined | JsValue::Null))
        {
            let key = project
                .as_string()
                .ok_or_else(|| failure("projectKey must be a valid path string."))?;
            let project = crate::metadata::resolve_process_identity(Path::new(""), key)
                .map_err(|error| failure(error.to_string()))?
                .ok_or_else(|| failure("projectKey must be a valid path string."))?;
            if !self.options.serves_project(&project) {
                return Err(unavailable(
                    "The isolated native process serves only its configured project.",
                ));
            }
        }
        // sdkConfig is not a wired native input; sdkSessionConfig is handled
        // by the separately validated SDK admission port.
        if input.get("sdkConfig").is_some_and(|v| !v.is_undefined()) {
            return Err(unavailable(
                "Per-turn model/SDK overrides are not installed in this isolated process.",
            ));
        }
        // When snapshots are enabled (--storage-root was supplied), resolve the
        // id through the same bounded helper as the management snapshot route:
        // TS `resolveProjectStorageId` prefers a registered `.cwd` marker, else
        // the legacy path slug. Sharing it keeps captured snapshots visible to
        // `snapshot_list`.
        if self.snapshot_recorder.is_some() {
            let metadata = crate::metadata::Metadata {
                home: self.options.home.clone(),
                cwd: PathBuf::new(),
                files: super::methods::session_dir::BoundedSessionMarkers::new(),
            };
            let project = self.options.project.to_string_lossy();
            let workspace_id =
                super::management::bounded_snapshot_workspace_id(&metadata, &project)
                    .map_err(failure)?;
            Ok(Some(JsString::from(workspace_id.as_str())))
        } else {
            Ok(None)
        }
    }
    fn snapshots(&self) -> Option<Arc<dyn SubmitSnapshots>> {
        self.snapshot_recorder.as_ref().map(|recorder| {
            Arc::new(NativeSubmitSnapshots(recorder.clone())) as Arc<dyn SubmitSnapshots>
        })
    }
    fn snapshots_required(&self) -> bool {
        false
    }
    fn cwd(&self) -> Option<Arc<dyn SubmitCwdService>> {
        Some(Arc::new(Cwd(self.options.project.clone())))
    }
    fn refresh(&self) -> SubmitFuture<'_, Result<(), SubmitError>> {
        Box::pin(async {
            let options = self.options.clone();
            let env = self.env.clone();
            let project = self.project.clone();
            let permit = super::methods::session_dir::session_io_permit()
                .await
                .map_err(|error| failure(String::from_utf16_lossy(error.message.units())))?;
            let changed = tokio::task::spawn_blocking(move || {
                let _permit = permit;
                use crate::runtime::project::ConfigSource;
                let fresh = crate::runtime::project::NativeConfigSource::new(
                    options.project.clone(),
                    env,
                    Some(options.home),
                    None,
                )
                .bounded()
                .load(&options.project)
                .map_err(|e| failure(e.to_string()))?;
                let lease = project
                    .acquire(Some(&options.project))
                    .map_err(|e| failure(e.to_string()))?;
                let staged = lease
                    .runtime()
                    .resources()
                    .get::<ProjectResources>()
                    .ok_or_else(|| failure("Native project resources are missing."))?
                    .context()
                    .snapshot();
                Ok::<_, SubmitError>(fresh.get("config") != staged.get("config"))
            })
            .await
            .map_err(|error| failure(error.to_string()))??;
            if changed {
                self.project.invalidate(Some(&self.options.project));
                use crate::runtime::refresh::RefreshRouter;
                self.owner.mark_project_dirty(
                    &self.options.project.to_string_lossy().into_owned().into(),
                    "config_changed",
                );
            }
            Ok(())
        })
    }
    fn run_policy(&self) -> Option<JsValue> {
        let lease = self.project.acquire(Some(&self.options.project)).ok()?;
        lease
            .runtime()
            .resources()
            .get::<ProjectResources>()?
            .context()
            .snapshot()
            .get("config")?
            .get("runPolicy")
            .cloned()
    }
    fn resolve_attachments<'a>(
        &'a self,
        input: &'a JsValue,
    ) -> SubmitFuture<'a, Result<SubmitAttachmentLease, SubmitError>> {
        let store = self.upload_store.clone();
        let project = self.options.project.clone();
        Box::pin(async move {
            // `uploadedAttachments` is a non-empty array at this call site
            // (GatewaySubmitHost::prepare checks that before calling).
            let refs = match input.get("uploadedAttachments") {
                Some(JsValue::Array(refs)) => refs.clone(),
                _ => {
                    return Err(SubmitError::new(
                        "CAPABILITY_UNAVAILABLE",
                        "uploadedAttachments must be a non-empty array.",
                    ));
                }
            };
            let project_key = project.to_string_lossy().into_owned();
            // Acquire leases sequentially, mirroring TS GatewayUploadedAttachmentBundle.
            // Disk I/O runs on a blocking thread so the async executor is not blocked.
            let mut acquired: Vec<crate::upload::UploadArtifactLease> = Vec::new();
            let result: Result<Vec<crate::upload::UploadArtifactLease>, SubmitError> =
                tokio::task::spawn_blocking(move || {
                    // P1-2: On error mid-loop, release every lease acquired so far
                    // (reverse order, best-effort) before propagating. Without this
                    // the partially-acquired set is dropped without release.
                    let outcome: Result<(), SubmitError> = (|| {
                        for r in &refs {
                            let upload_id = r
                                .get("uploadId")
                                .and_then(JsValue::as_string)
                                .and_then(|s| s.to_utf8().ok())
                                .ok_or_else(|| {
                                    SubmitError::new(
                                        "CAPABILITY_UNAVAILABLE",
                                        "uploadedAttachments[].uploadId must be a string.",
                                    )
                                })?;
                            // attachmentIds: optional string array
                            let attachment_ids: Option<Vec<String>> = match r.get("attachmentIds") {
                                Some(JsValue::Array(ids)) => Some(
                                    ids.iter()
                                        .map(|v| {
                                            v.as_string().and_then(|s| s.to_utf8().ok()).ok_or_else(
                                                || {
                                                    SubmitError::new(
                                                        "CAPABILITY_UNAVAILABLE",
                                                        "attachmentIds must be strings.",
                                                    )
                                                },
                                            )
                                        })
                                        .collect::<Result<Vec<_>, _>>()?,
                                ),
                                _ => None,
                            };
                            let lease = store
                                .acquire_attachment_lease(
                                    &upload_id,
                                    &project_key,
                                    attachment_ids.as_deref(),
                                )
                                .map_err(|e| {
                                    SubmitError::new(
                                        e.code.as_str(),
                                        String::from_utf16_lossy(e.message.units()),
                                    )
                                })?;
                            acquired.push(lease);
                        }
                        Ok(())
                    })();
                    match outcome {
                        Ok(()) => Ok(acquired),
                        Err(e) => {
                            for lease in acquired.iter().rev() {
                                if let Err(rel) = lease.release() {
                                    // #1850: `eprintln!` panics when stderr is
                                    // unwritable, which would abort this
                                    // blocking task mid-release. The whole
                                    // loop is best-effort, so a dropped line
                                    // must not cost the remaining leases.
                                    let _ = writeln!(
                                        std::io::stderr(),
                                        "native upload lease release on error: {rel}"
                                    );
                                }
                            }
                            Err(e)
                        }
                    }
                })
                .await
                .map_err(|e| SubmitError::new("CAPABILITY_UNAVAILABLE", e.to_string()))?;
            let leases = result?;
            // Merge attachments from all leases (TS: flatMap over acquired).
            let attachments: Vec<JsValue> = leases
                .iter()
                .flat_map(|lease| lease.attachments.iter().cloned())
                .collect();
            // Release is idempotent on all acquired leases (reverse order like TS).
            let release: crate::runtime::turn::submit_host::SubmitLeaseRelease =
                Box::new(move || {
                    Box::pin(async move {
                        // Concurrency P2a (gate): `lease.release()` removes the
                        // whole lease directory; like the acquire path it runs
                        // on a blocking thread so the serial host event loop is
                        // never blocked on unlink churn.
                        tokio::task::spawn_blocking(move || {
                            for lease in leases.iter().rev() {
                                // Best-effort: log failures but continue.
                                if let Err(e) = lease.release() {
                                    // #1850: same as the error path above —
                                    // this runs detached from the submit
                                    // call, where an `eprintln!` panic would
                                    // silently kill the remaining releases.
                                    let _ = writeln!(
                                        std::io::stderr(),
                                        "native upload lease release: {e}"
                                    );
                                }
                            }
                        })
                        .await
                        .map_err(|e| SubmitError::new("CAPABILITY_UNAVAILABLE", e.to_string()))
                    })
                });
            Ok(SubmitAttachmentLease {
                attachments,
                release,
            })
        })
    }
    fn prepare_channel_attachment_turn<'a>(
        &'a self,
        origin: ChannelAttachmentOrigin,
        message: &'a JsValue,
        attachments: Vec<JsValue>,
        _project: Option<&JsString>,
        _session: Option<&JsString>,
    ) -> SubmitFuture<'a, Result<JsValue, SubmitError>> {
        self.compose_native_attachments(message, attachments, Some(origin))
    }
    fn prepare_attachment_turn<'a>(
        &'a self,
        message: &'a JsValue,
        attachments: Vec<JsValue>,
        _project: Option<&JsString>,
        _session: Option<&JsString>,
    ) -> SubmitFuture<'a, Result<JsValue, SubmitError>> {
        self.compose_native_attachments(message, attachments, None)
    }
    fn resolve_model_selection<'a>(
        &'a self,
        input: &'a JsValue,
    ) -> SubmitFuture<'a, Result<JsValue, SubmitError>> {
        let home = self.options.home.clone();
        let project = self.options.project.clone();
        let env = self.env.clone();
        let input = input.clone();
        Box::pin(async move {
            static SLOTS: std::sync::LazyLock<Arc<tokio::sync::Semaphore>> =
                std::sync::LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(4)));
            let permit = SLOTS
                .clone()
                .acquire_owned()
                .await
                .map_err(|e| failure(e.to_string()))?;
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                let marker_files =
                    crate::runtime::history::HistoryMarkerFiles::for_model_selection();
                let lookup = crate::metadata::Metadata {
                    home: home.clone(),
                    cwd: PathBuf::new(),
                    files: marker_files,
                };
                let fallback = JsValue::from(project.to_string_lossy().as_ref());
                let project_key = input
                    .get("projectKey")
                    .filter(|value| !matches!(value, JsValue::Null | JsValue::Undefined))
                    .unwrap_or(&fallback);
                let resolved = crate::session_model::resolve_project(&lookup, project_key);
                if lookup.files.was_refused() {
                    return Err(SubmitError::new(
                        "gateway_request_failed",
                        "Unsafe project marker.",
                    ));
                }
                let resolved = resolved.map_err(|error| {
                    SubmitError::new(&error.code, String::from_utf16_lossy(error.message.units()))
                })?;
                // This native host owns one project registry lease. Validate
                // registration first, then refuse a different project rather
                // than selecting its model against this host's configuration.
                if std::path::Path::new(&resolved) != project {
                    return Err(unavailable(
                        "The isolated native process serves only its configured project.",
                    ));
                }
                let defined = |value: Option<&JsValue>| value.is_some_and(|v| !v.is_undefined());
                let selected = input.get("modelSelection");
                let override_model = input.get("modelOverride");
                if defined(selected) && defined(override_model) {
                    return Err(SubmitError::new(
                        "INVALID_MODEL_OVERRIDE",
                        "Specify modelSelection or modelOverride, not both.",
                    ));
                }
                let requested = if defined(selected) {
                    let value = selected.unwrap();
                    if !matches!(value, JsValue::Object(_) | JsValue::Array(_)) {
                        return Err(SubmitError::new(
                            "INVALID_MODEL_OVERRIDE",
                            "modelSelection must be an object.",
                        ));
                    }
                    Some(value)
                } else {
                    override_model.filter(|v| match v {
                        JsValue::Null | JsValue::Undefined => false,
                        JsValue::Bool(b) => *b,
                        JsValue::Number(n) => *n != 0.0 && !n.is_nan(),
                        JsValue::String(s) => !s.is_empty(),
                        _ => true,
                    })
                };
                let error = |e: crate::runtime::connection::RequestError| {
                    SubmitError::new(&e.code, String::from_utf16_lossy(e.message.units()))
                };
                let validate = |selection: &JsValue| -> Result<(), SubmitError> {
                    super::methods::validate_selection_shape(selection).map_err(error)?;
                    // The served project's snapshot, rather than the process
                    // working directory or an independent model registry.
                    let config = crate::config::load_project_bounded(
                        &home,
                        &project,
                        &env,
                        Some(project.to_string_lossy().as_ref()),
                    )
                    .map_err(|e| {
                        SubmitError::new(&e.code, String::from_utf16_lossy(e.message.units()))
                    })?;
                    let catalog = crate::model_catalog::execute_loaded(config, &object([]), &env)
                        .map_err(|e| {
                        SubmitError::new(&e.code, String::from_utf16_lossy(e.message.units()))
                    })?;
                    super::methods::validate_selection_catalog(selection, &catalog).map_err(error)
                };
                if let Some(requested) = requested {
                    if !defined(selected) && requested.get("mode") != Some(&JsValue::from("model"))
                    {
                        return Err(SubmitError::new(
                            "INVALID_MODEL_OVERRIDE",
                            "A model selection requires mode, provider, and model.",
                        ));
                    }
                    validate(requested)?;
                    let normalized = super::methods::normalize_selection(requested);
                    if normalized.get("mode") == Some(&JsValue::from("auto")) {
                        return Ok(object([("source", "router".into())]));
                    }
                    return Ok(object([
                        ("source", "turn".into()),
                        ("selection", normalized),
                    ]));
                }
                let params = object([
                    (
                        "projectKey",
                        JsValue::from(project.to_string_lossy().as_ref()),
                    ),
                    (
                        "sessionKey",
                        input
                            .get("sessionKey")
                            .cloned()
                            .unwrap_or(JsValue::Undefined),
                    ),
                ]);
                let state =
                    crate::session_model::execute_with_config(&lookup, &params, &env, |selected| {
                        crate::config::load_project_bounded_lazy(&home, &lookup.cwd, &env, selected)
                    });
                // Both the second marker scan and transcript open use the same
                // bounded, nonblocking owner. A refused marker must not silently
                // become a lexical fallback or a default model selection.
                if lookup.files.was_refused() {
                    return Err(SubmitError::new(
                        "gateway_request_failed",
                        "Unsafe project marker or session transcript.",
                    ));
                }
                let state = state.map_err(|e| {
                    SubmitError::new(&e.code, String::from_utf16_lossy(e.message.units()))
                })?;
                let saved = state.get("saved").cloned().unwrap_or(JsValue::Undefined);
                if saved.get("mode") == Some(&JsValue::from("model")) {
                    validate(&saved)?;
                    Ok(object([("source", "session".into()), ("selection", saved)]))
                } else {
                    Ok(object([(
                        "source",
                        state
                            .get("effective")
                            .and_then(|v| v.get("source"))
                            .cloned()
                            .unwrap_or("default".into()),
                    )]))
                }
            })
            .await
            .map_err(|e| failure(e.to_string()))?
        })
    }
    fn record_failure<'a>(
        &'a self,
        run: &'a RunRef,
        status: &'a JsValue,
    ) -> SubmitFuture<'a, Result<(), SubmitError>> {
        let recorder = self
            .recorders
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&run.session_key)
            .and_then(std::sync::Weak::upgrade);
        let run = run.clone();
        let status = status.clone();
        Box::pin(async move {
            let recorder =
                recorder.ok_or_else(|| failure("Native session recorder is unavailable."))?;
            tokio::task::spawn_blocking(move || {
                recorder.writer().record_agent_status_message(
                    &run.session_key,
                    &run.run_id,
                    &status,
                )
            })
            .await
            .map_err(|e| failure(e.to_string()))?
            .map_err(|e| failure(e.to_string()))
        })
    }
    fn after_turn(&self, _: &RunRef) -> Result<(), SubmitError> {
        self.recorders
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|_, recorder| recorder.strong_count() > 0);
        Ok(())
    }
    fn now_ms(&self) -> f64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs_f64()
            * 1000.0
    }
    fn abort_turn_timeout_ms(&self) -> u64 {
        self.options.drain_budget.as_millis() as u64
    }
}

// ------------------------------------------------ U7 native settings (#1320)
use crate::config::store::{ConfigStore, StoreOptions};
use crate::runtime::refresh::{
    ConsoleLog, NoMemoryMaintenance, RefreshBundle, RefreshLog, RefreshOptions, RefreshRegistry,
    RefreshRouter, RegistryRefresh,
};
use crate::runtime::session::SessionOwner;
use crate::runtime::settings::{ResolvedSettingsPort, SettingsGateway, SettingsGatewayOptions};

/// The native U7 settings composition inputs. `router` and `registry` are the
/// bundle's collaborators as ports: production binds the session owner and
/// `RegistryRefresh` over the project runtime registry, tests substitute
/// recording collaborators to observe the dirty/broadcast order.
pub struct NativeSettingsOptions {
    /// The gateway's admission/close port: `hasActiveTurn` drives the
    /// SESSION_BUSY gate and `close` is the post-success session close.
    pub owner: SessionOwner,
    /// `getRouter`: the bundle's dirty router.
    pub router: Arc<dyn RefreshRouter>,
    /// The bundle's runtime registry.
    pub registry: Arc<dyn RefreshRegistry>,
    /// `createPilotConfigStoreSync({ projectRoot, env })`: the caller loads
    /// the store (a load failure is a boot failure, like the TS
    /// constructor's throw).
    pub store: ConfigStore,
    /// `projectRoot` for the bundle's log lines and diagnostics.
    pub project_root: JsString,
    /// The bundle's `log` (createLocalGateway.ts:806: `getConsoleLog()`).
    /// Production passes [`ConsoleLog`]; the #1320 replay passes a recorder
    /// because the golden pins the bundle's exact log lines.
    pub log: Arc<dyn RefreshLog>,
}

/// The composed U7 settings stack: the #1318 [`SettingsGateway`] over the
/// real refresh bundle and config store.
pub struct NativeSettings {
    pub gateway: Arc<SettingsGateway>,
    pub(crate) store: ConfigStore,
    // The Server assembly binds this bundle to its hosted notification registry
    // and owns its disposal. LocalHost and isolated compositions stay unbound.
    pub bundle: Arc<RefreshBundle>,
}

/// Compose the config store and runtime refresh bundle. Test compositions may
/// omit delegates; production supplies the atomic local writer and the shared
/// native session flag store. Output-style-only reload is a separate delegate.
#[cfg(test)]
pub fn native_settings(options: NativeSettingsOptions) -> NativeSettings {
    native_settings_with_writer(options, None, None, None, None)
}
fn native_settings_with_writer(
    options: NativeSettingsOptions,
    writer: Option<Arc<dyn crate::runtime::settings::LocalSettingsWriter>>,
    flags: Option<Arc<super::SessionOverrides>>,
    styles: Option<Arc<dyn crate::runtime::settings::ReloadOutputStyles>>,
    sdk: Option<Arc<dyn crate::runtime::refresh::SdkConfigChange>>,
) -> NativeSettings {
    let NativeSettingsOptions {
        owner,
        router,
        registry,
        store,
        project_root,
        log,
    } = options;
    let bundle = Arc::new(RefreshBundle::new(RefreshOptions {
        config_store: Arc::new(store.clone()),
        registry,
        memory_maintenance: Arc::new(NoMemoryMaintenance),
        router: Arc::new(move || Some(router.clone())),
        project_root,
        memory_diagnostics: None,
        sdk,
        log,
    }));
    // `runtimeRefresh.attach()` (createLocalGateway.ts:831): without the
    // subscription, an explicit `reload_config` still answers, but its
    // config change would never refresh runtimes or mark sessions dirty.
    bundle.attach();
    let gateway = Arc::new(SettingsGateway::new(SettingsGatewayOptions {
        owner,
        apply_flag_settings: flags
            .map(|port| port as Arc<dyn crate::runtime::settings::ApplyFlagSettings>),
        update_settings: writer.map(|writer| crate::runtime::settings::UpdateSettings {
            organization_denied: false,
            writer,
            store: store.clone(),
        }),
        resolve_settings: Some(Arc::new(ResolvedSettingsPort {
            store: store.clone(),
        })),
        reload_output_styles: styles,
        reload_config: Some(bundle.clone()),
        reload_extensions: Some(bundle.clone()),
    }));
    NativeSettings {
        gateway,
        bundle,
        store,
    }
}

/// The production composition for the native process
/// (createLocalGateway.ts:795-835): [`native_settings`] with the
/// collaborators the isolated process owns — `createPilotConfigStoreSync`
/// over the served home/project (a load failure is the TS boot failure),
/// `RegistryRefresh` over the project runtime registry and the session
/// owner as the dirty router. The store anchors exactly like the process's
/// other config reads (`NativeConfigSource`: PILOT_HOME from the
/// environment, the served project as cwd and `projectRoot`).
///
/// This isolated composition leaves notification binding to the embedding.
/// Server assembly binds the shared host registry; LocalHost stays unbound.
#[cfg(test)]
pub fn native_settings_for_process(
    owner: SessionOwner,
    project: Arc<crate::runtime::project::ProjectRuntimeRegistry>,
    home: &std::path::Path,
    served_project: &std::path::Path,
    env: &crate::config::Env,
) -> std::io::Result<NativeSettings> {
    native_settings_for_process_with_flags(owner, project, home, served_project, env, None)
}

#[cfg(test)]
pub fn native_settings_for_process_with_flags(
    owner: SessionOwner,
    project: Arc<crate::runtime::project::ProjectRuntimeRegistry>,
    home: &std::path::Path,
    served_project: &std::path::Path,
    env: &crate::config::Env,
    flags: Option<Arc<super::SessionOverrides>>,
) -> std::io::Result<NativeSettings> {
    native_settings_for_process_with_styles(
        owner,
        project,
        home,
        served_project,
        env,
        flags,
        (None, None),
    )
}
type NativeRefreshContributions = (
    Option<Arc<dyn crate::runtime::settings::ReloadOutputStyles>>,
    Option<Arc<dyn crate::runtime::refresh::SdkConfigChange>>,
);
pub(crate) fn native_settings_for_process_with_styles(
    owner: SessionOwner,
    project: Arc<crate::runtime::project::ProjectRuntimeRegistry>,
    home: &std::path::Path,
    served_project: &std::path::Path,
    env: &crate::config::Env,
    flags: Option<Arc<super::SessionOverrides>>,
    contributions: NativeRefreshContributions,
) -> std::io::Result<NativeSettings> {
    let (styles, sdk) = contributions;
    // #1320 fix-r1 F3: same anchor as every other config read of the process
    // (`NativeConfigSource`): PILOT_HOME from the environment resolved
    // against the served project, `--home` only as the fallback homedir for
    // `~` / `~/` prefixes. In production the env arrives from
    // `Options::environment`, which has already overwritten PILOT_HOME with
    // `--home`, so the resolved anchor is always `--home` there; the explicit
    // `Some(home)` keeps direct callers (tests and future non-CLI
    // assemblies) anchored exactly like the CLI path.
    let pilot_home = crate::runtime::project::resolve_pilot_home(env, served_project, Some(home));
    let mut store_options = StoreOptions::new(pilot_home.clone(), served_project, env.clone());
    store_options.project = Some(served_project.to_string_lossy().into_owned());
    let store = ConfigStore::new_bounded(store_options).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "pilot config store failed to load: {} {}",
                error.code,
                String::from_utf16_lossy(error.message.units())
            ),
        )
    })?;
    #[cfg(unix)]
    let writer: Option<Arc<dyn crate::runtime::settings::LocalSettingsWriter>> = Some(Arc::new(
        crate::runtime::settings::NativeLocalSettingsWriter {
            home: pilot_home,
            project: served_project.to_path_buf(),
            env: env.clone(),
        },
    ));
    #[cfg(not(unix))]
    let writer: Option<Arc<dyn crate::runtime::settings::LocalSettingsWriter>> = None;
    Ok(native_settings_with_writer(
        NativeSettingsOptions {
            owner: owner.clone(),
            router: Arc::new(owner),
            registry: Arc::new(RegistryRefresh::new(project)),
            store,
            project_root: JsString::from(served_project.to_string_lossy().into_owned()),
            log: Arc::new(ConsoleLog),
        },
        writer,
        flags,
        styles,
        sdk,
    ))
}

struct ScopedServices {
    services: Services,
    _reservation: Option<crate::runtime::session::SessionProjectScope>,
}
impl SubmitHostServices for ScopedServices {
    fn bound_project_key(&self) -> Option<JsString> {
        Some(
            self.services
                .options
                .project
                .to_string_lossy()
                .into_owned()
                .into(),
        )
    }
    fn workspace_id(&self, input: &JsValue) -> Result<Option<JsString>, SubmitError> {
        self.services.workspace_id(input)
    }
    fn snapshots(&self) -> Option<Arc<dyn SubmitSnapshots>> {
        self.services.snapshots()
    }
    fn snapshots_required(&self) -> bool {
        self.services.snapshots_required()
    }
    fn cwd(&self) -> Option<Arc<dyn SubmitCwdService>> {
        self.services.cwd()
    }
    fn refresh(&self) -> SubmitFuture<'_, Result<(), SubmitError>> {
        self.services.refresh()
    }
    fn run_policy(&self) -> Option<JsValue> {
        self.services.run_policy()
    }
    fn resolve_attachments<'a>(
        &'a self,
        input: &'a JsValue,
    ) -> SubmitFuture<'a, Result<SubmitAttachmentLease, SubmitError>> {
        self.services.resolve_attachments(input)
    }
    fn prepare_attachment_turn<'a>(
        &'a self,
        message: &'a JsValue,
        attachments: Vec<JsValue>,
        project: Option<&JsString>,
        session: Option<&JsString>,
    ) -> SubmitFuture<'a, Result<JsValue, SubmitError>> {
        self.services
            .prepare_attachment_turn(message, attachments, project, session)
    }
    fn prepare_channel_attachment_turn<'a>(
        &'a self,
        origin: ChannelAttachmentOrigin,
        message: &'a JsValue,
        attachments: Vec<JsValue>,
        project: Option<&JsString>,
        session: Option<&JsString>,
    ) -> SubmitFuture<'a, Result<JsValue, SubmitError>> {
        self.services.prepare_channel_attachment_turn(
            origin,
            message,
            attachments,
            project,
            session,
        )
    }
    fn resolve_model_selection<'a>(
        &'a self,
        input: &'a JsValue,
    ) -> SubmitFuture<'a, Result<JsValue, SubmitError>> {
        self.services.resolve_model_selection(input)
    }
    fn record_failure<'a>(
        &'a self,
        run: &'a RunRef,
        status: &'a JsValue,
    ) -> SubmitFuture<'a, Result<(), SubmitError>> {
        self.services.record_failure(run, status)
    }
    fn after_turn(&self, run: &RunRef) -> Result<(), SubmitError> {
        self.services.after_turn(run)
    }
    fn now_ms(&self) -> f64 {
        self.services.now_ms()
    }
    fn abort_turn_timeout_ms(&self) -> u64 {
        self.services.abort_turn_timeout_ms()
    }
}
