//! Native usage RPCs over the project's existing Router statistics.
//!
//! `ProjectRuntimeRegistry.ts:1310-1373` resolves the project before reading its
//! collector. The resolver here must return that same cached collector; inject
//! its Arc into `RouterDeps.stats` when building the sidecar model host. Router
//! `observe_request` then persists actual model attempts to `stats.jsonl`, with
//! provider/model, session/turn, role, usage, timestamps and cost provenance.
//! A `turn_completed` event is not a model-request record and is never counted
//! here. `UsageRegistry` owns collector construction and journal recovery.
//! Composition binds its initial snapshot with `resolve_with_snapshot(None,
//! &snapshot)`; session factories do the same for `SessionContext.project_key`
//! before calling `SidecarSession::with_usage_stats`.

use crate::{
    metadata,
    runtime::{
        connection::RequestError,
        model::{
            router::{RouterConfig, RouterModelRef, StatsConfig},
            stats::TokenStatsCollector,
        },
    },
};
use pilotdeck_protocol::{JsString, JsValue};
use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

pub type SharedUsageStats = Arc<Mutex<TokenStatsCollector>>;
pub type UsageProjectStats<'a> =
    dyn Fn(Option<&JsString>) -> Result<SharedUsageStats, RequestError> + Send + Sync + 'a;

/// Project collectors with the baseline's existing journal path.
/// TS rebuilds every project from `<pilot_home>/router/stats.jsonl` without
/// filtering `projectPath`; this preserves that historical shared-journal scope.
pub struct UsageRegistry {
    pilot_home: PathBuf,
    cwd: JsString,
    config: StatsConfig,
    projects: Mutex<HashMap<JsString, UsageProject>>,
}

struct UsageProject {
    stats: SharedUsageStats,
    config: StatsConfig,
}

impl UsageRegistry {
    /// Gateway statistics are enabled by default (registry TS:1769).
    /// No ambient home or configuration containing provider secrets is read.
    pub fn new(pilot_home: PathBuf, cwd: JsString) -> Self {
        Self::with_config(
            pilot_home,
            cwd,
            StatsConfig {
                enabled: true,
                ..Default::default()
            },
        )
    }

    /// The composition owner can supply already-resolved pricing/retention.
    pub fn with_config(pilot_home: PathBuf, cwd: JsString, config: StatsConfig) -> Self {
        Self {
            pilot_home,
            cwd,
            config,
            projects: Mutex::new(HashMap::new()),
        }
    }

    pub fn resolve(&self, project: Option<&JsString>) -> Result<SharedUsageStats, RequestError> {
        self.resolve_configured(project, None)
    }

    /// Call before binding the sidecar. The same Arc survives config changes,
    /// so both existing RPC routes and model observers keep seeing one owner.
    pub fn resolve_with_snapshot(
        &self,
        project: Option<&JsString>,
        snapshot: &JsValue,
    ) -> Result<SharedUsageStats, RequestError> {
        let router = RouterConfig::from_snapshot(snapshot);
        let mut config = router.stats;
        let raw_config = snapshot.get("config");
        let raw_router = raw_config.and_then(|config| config.get("router"));
        // ensureRouterConfig: missing/null optional routing is off; an opted-in
        // router enables stats unless stats.enabled explicitly disables them.
        config.enabled = raw_router.is_some_and(|value| {
            !matches!(value, JsValue::Null | JsValue::Undefined)
                && value.get("enabled") != Some(&JsValue::Bool(false))
        }) && raw_router
            .and_then(|router| router.get("stats"))
            .and_then(|stats| stats.get("enabled"))
            != Some(&JsValue::Bool(false));
        if config.baseline_model.is_none() {
            config.baseline_model = raw_config
                .and_then(|config| config.get("agent"))
                .and_then(|agent| agent.get("model"))
                .and_then(RouterModelRef::from_value)
                .or(router.scenarios_default);
        }
        config.file_path = self.config.file_path.clone();
        self.resolve_configured(project, Some(config))
    }

    fn resolve_configured(
        &self,
        project: Option<&JsString>,
        config: Option<StatsConfig>,
    ) -> Result<SharedUsageStats, RequestError> {
        let cwd = self.cwd.to_utf8().map_err(|_| {
            RequestError::new("Gateway working directory is not a valid filesystem path.")
        })?;
        let cwd = PathBuf::from(cwd);
        let base = if cwd.is_absolute() {
            metadata::resolve_path(Path::new("/"), &cwd)
        } else {
            let process_cwd =
                std::env::current_dir().map_err(|error| RequestError::new(error.to_string()))?;
            metadata::resolve_path(&process_cwd, &cwd)
        };
        let key = match project {
            Some(project) => metadata::resolve_units(&base.to_string_lossy(), project),
            None => JsString::from(base.to_string_lossy().as_ref()),
        };
        let mut projects = self
            .projects
            .lock()
            .map_err(|_| RequestError::new("Usage registry lock is poisoned."))?;
        if config.is_none()
            && let Some(project) = projects.get(&key)
        {
            return Ok(project.stats.clone());
        }
        let home = metadata::resolve_path(&base, &self.pilot_home);
        let mut config = config.unwrap_or_else(|| self.config.clone());
        config.file_path = config
            .file_path
            .as_ref()
            .map(|path| metadata::resolve_path(&base, path));
        if let Some(project) = projects.get(&key)
            && project.config == config
        {
            return Ok(project.stats.clone());
        }
        let directory = config
            .file_path
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .unwrap_or_else(|| home.join("router"));
        let journal = directory.join("stats.jsonl");
        if config.enabled {
            fs::create_dir_all(&directory).map_err(|error| RequestError::new(error.to_string()))?;
            match fs::metadata(&journal) {
                Ok(metadata) if !metadata.is_file() => {
                    return Err(RequestError::new(
                        "Router statistics journal is not a regular file.",
                    ));
                }
                Ok(_) => {
                    OpenOptions::new()
                        .read(true)
                        .append(true)
                        .open(&journal)
                        .map_err(|error| RequestError::new(error.to_string()))?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(RequestError::new(error.to_string())),
            }
        }
        // Block the shared writer while rebuilding a changed configuration,
        // otherwise a request finishing during replay could disappear in RAM.
        let existing = projects.get(&key).map(|project| project.stats.clone());
        let current = existing
            .as_ref()
            .map(|stats| {
                stats
                    .lock()
                    .map_err(|_| RequestError::new("Router statistics lock is poisoned."))
            })
            .transpose()?;
        let collector = TokenStatsCollector::new(Some(&config), Some(&home))
            .map_err(|error| RequestError::new(error.to_string()))?;
        if config.enabled {
            // Do not create the file before construction: that would suppress
            // the collector's migration of legacy stats.json. Verify I/O after.
            OpenOptions::new()
                .create(true)
                .read(true)
                .append(true)
                .open(&journal)
                .map_err(|error| RequestError::new(error.to_string()))?;
        }
        let stats = if let Some(mut current) = current {
            *current = collector;
            drop(current);
            existing.as_ref().expect("configured project").clone()
        } else {
            Arc::new(Mutex::new(collector))
        };
        projects.insert(
            key,
            UsageProject {
                stats: stats.clone(),
                config,
            },
        );
        Ok(stats)
    }
}

/// Borrow the project resolver; reuse existing collectors across RPCs.
pub struct UsageRoutes<'a> {
    resolve: &'a UsageProjectStats<'a>,
}

impl<'a> UsageRoutes<'a> {
    pub fn new(resolve: &'a UsageProjectStats<'a>) -> Self {
        Self { resolve }
    }

    pub fn handle(&self, method: &str, params: JsValue) -> Option<Result<JsValue, RequestError>> {
        match method {
            "usage_snapshot" => Some(self.usage_snapshot(params)),
            "model_usage_snapshot" => Some(self.model_usage_snapshot(params)),
            _ => None,
        }
    }

    pub fn usage_snapshot(&self, params: JsValue) -> Result<JsValue, RequestError> {
        let project = optional_key(&params, "projectKey")?;
        let stats = (self.resolve)(project)?;
        let session = optional_key(&params, "sessionKey")?.filter(|key| !key.is_empty());
        let mut stats = stats
            .lock()
            .map_err(|_| RequestError::new("Router statistics lock is poisoned."))?;
        Ok(stats.usage_snapshot_for_sdk(session))
    }

    pub fn model_usage_snapshot(&self, params: JsValue) -> Result<JsValue, RequestError> {
        let project = optional_key(&params, "projectKey")?;
        let stats = (self.resolve)(project)?;
        // TS treats an empty sessionKey as project scope in both projections.
        let session = optional_key(&params, "sessionKey")?.filter(|key| !key.is_empty());
        let mut stats = stats
            .lock()
            .map_err(|_| RequestError::new("Router statistics lock is poisoned."))?;
        Ok(stats.model_usage_snapshot_for_sdk(session))
    }
}

fn optional_key<'a>(params: &'a JsValue, name: &str) -> Result<Option<&'a JsString>, RequestError> {
    if matches!(params, JsValue::Null | JsValue::Undefined) {
        let kind = if matches!(params, JsValue::Null) {
            "null"
        } else {
            "undefined"
        };
        return Err(RequestError::new(format!(
            "Cannot read properties of {kind} (reading '{name}')"
        )));
    }
    match params.get(name) {
        None | Some(JsValue::Null | JsValue::Undefined) => Ok(None),
        Some(JsValue::String(key)) => Ok(Some(key)),
        Some(_) => Err(RequestError::new(format!(
            "{name} must be a string when provided."
        ))),
    }
}

#[cfg(test)]
#[path = "usage_tests.rs"]
mod tests;
