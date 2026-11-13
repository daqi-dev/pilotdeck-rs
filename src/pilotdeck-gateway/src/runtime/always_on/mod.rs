//! Port of the TS always-on background runtime's control half
//! (`src/always-on/`, baseline cc7c4e6): storage paths, the discovery state
//! and cycle/plan stores, channel leases, the pure gate chain, the discovery
//! lock, the dormancy signal watcher and the periodic scheduler
//! (`DiscoveryScheduler.ts`), plus the per-project runtime and multi-project
//! manager (`AlwaysOnRuntime.ts` / `AlwaysOnManager.ts`) that own them.
//!
//! The deep fire pipeline — the five-phase discovery run, the workspace
//! strategies, the report tools and the model turns behind
//! `DiscoveryFire.run` / `runApplyPhase` / `rerunPlan` — is deliberately NOT
//! ported: those halves return an explicit `internal` error ("not implemented
//! in this embedding") instead of a fake success. Everything before them is
//! real: gate reads, state bytes, the `wx` lock, session-key derivation and
//! the control envelopes.
//!
//! Evidence: `tests/fixtures/always-on-2294/` (58 TS-baseline golden records,
//! Node v22.22.0) drives the tests in this module and the assembly half.
// The deep fire seam above means most of this port's surface — the fire
// pipeline's inputs, the lease registry's mutators, the report paths — is
// driven by those replay tests rather than the shipping build, until the
// fire half itself is ported. The port's wire-shape items keep the reason
// inline with their own `#[allow(dead_code)]`; this umbrella covers the
// rest of the unported surface (paths, mutators, tick entries).
#![allow(dead_code)]
mod cycles;
mod fire;
mod gates;
mod leases;
mod manager;
mod paths;
mod scheduler;
mod state;
mod watcher;
// #2488: the closed-set pin over the always-on-2294 negative-control
// fixture's row table — test-only, like the replay modules' own `tests`.
#[cfg(test)]
mod negative_controls_manifest;

pub(crate) use cycles::{DiscoveryPlanRecord, DiscoveryPlanStore, WorkCycleRecord, WorkCycleStore};
pub(crate) use fire::{
    acquire_discovery_lock, derive_execution_session_key, derive_report_session_key,
    derive_workspace_session_key, release_discovery_lock,
};
pub(crate) use gates::{GateBlockReason, GateInput, evaluate_always_on_discovery_gates};
pub(crate) use leases::{ChannelLease, ChannelLeaseRegistry};
pub(crate) use manager::{
    AbortInput, AlwaysOnAgentGateway, AlwaysOnManagerOptions, ApplyInput, ControlError,
    GatewayAbortInput, RerunInput,
};
// The one face that crosses the crate boundary: `native_process::requests`
// carries it in a public field (`Requests::always_on`), the same `pub`
// pattern `runtime::cron::CronManager` follows. Its methods stay
// `pub(crate)`.
pub use manager::AlwaysOnManager;
pub(crate) use paths::{AlwaysOnPaths, resolve_always_on_paths};
pub(crate) use scheduler::{ALWAYS_ON_METHODS, DiscoveryScheduler, SchedulerDeps};
pub(crate) use state::DiscoveryStateStore;
pub(crate) use watcher::{SignalWatcher, SignalWatcherOptions};

use std::sync::Arc;

use pilotdeck_protocol::{JsObject, JsString, JsValue};

/// `AlwaysOnError` (`src/always-on/protocol/errors.ts`): name `AlwaysOnError`
/// plus a machine-readable code. The codes are the TS union verbatim — the
/// scheduler preserves the code when a fire crash crosses it
/// (`DiscoveryScheduler.ts:158-167`).
#[derive(Clone, Debug)]
pub(crate) struct AlwaysOnError {
    code: String,
    message: JsString,
}
impl AlwaysOnError {
    pub(crate) fn new(code: impl Into<String>, message: impl Into<JsString>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
    /// The `code` the TS scheduler rethrows verbatim.
    pub(crate) fn code(&self) -> &str {
        &self.code
    }
    pub(crate) fn message(&self) -> JsString {
        self.message.clone()
    }
}

/// TS `AlwaysOnRuntimeLogger` (AlwaysOnRuntime.ts:64-67), both channels, with
/// the metadata object exactly as the TS call sites build it (an empty object
/// where TS omits the argument).
pub(crate) trait AlwaysOnLogger: Send + Sync {
    fn info(&self, message: &str, meta: &JsObject);
    fn warn(&self, message: &str, meta: &JsObject);
}

/// The always-on busy hook: `attachment.isProjectBusy`
/// (ProjectAutomationBundle.ts:64) is the one the bundle's attachment
/// carries, and the manager's `bindAgentGateway` signature reads it the
/// same way (AlwaysOnManager.ts:147-160) — one type for the same concept
/// on both sides.
pub(crate) type IsProjectBusy = Arc<dyn Fn(&str) -> bool + Send + Sync>;

/// TS `NOOP_LOGGER` (AlwaysOnRuntime.ts:60-63).
pub(crate) struct NoopAlwaysOnLogger;
impl AlwaysOnLogger for NoopAlwaysOnLogger {
    fn info(&self, _message: &str, _meta: &JsObject) {}
    fn warn(&self, _message: &str, _meta: &JsObject) {}
}

/// `DEFAULT_MAX_PLANS_PER_CYCLE` (parseAlwaysOnConfig.ts:36): the cycle-full
/// bound the scheduler checks after the gates and before the lock.
pub(crate) const DEFAULT_MAX_PLANS_PER_CYCLE: usize = 3;

/// `AlwaysOnConfig` (parseAlwaysOnConfig.ts:20-27) as the normalized config
/// snapshot section carries it (`config/snapshot.rs` `always_on()`): trigger
/// defaults already filled, project keys already absolute. `projects` keeps
/// the section's insertion order, which decides the manager's runtime order.
#[derive(Clone, Debug)]
pub(crate) struct AlwaysOnConfig {
    pub tick_interval_minutes: f64,
    pub cooldown_minutes: f64,
    pub daily_budget: f64,
    pub heartbeat_stale_seconds: f64,
    pub recent_user_msg_minutes: f64,
    pub prefer_channel: String,
    pub language: Option<String>,
    /// `(projectKey, enabled)` in section order.
    pub projects: Vec<(String, bool)>,
}
impl AlwaysOnConfig {
    /// `defaultAlwaysOnConfig()` (parseAlwaysOnConfig.ts:38-44): the trigger
    /// defaults a `pilotdeck.yaml` without an `alwaysOn:` section would get.
    pub fn default_config() -> Self {
        Self {
            tick_interval_minutes: 5.0,
            cooldown_minutes: 60.0,
            daily_budget: 4.0,
            heartbeat_stale_seconds: 90.0,
            recent_user_msg_minutes: 5.0,
            prefer_channel: "web".into(),
            language: None,
            projects: Vec::new(),
        }
    }

    /// Read a normalized snapshot section. The normalizer already rejects
    /// invalid values, so this is a field copy like the cron counterpart
    /// (`assembly/cron.rs` `manager_config`).
    pub fn from_section(section: &JsValue) -> Self {
        let mut config = Self::default_config();
        if let Some(trigger) = section.get("trigger").and_then(JsValue::as_object) {
            for (slot, key) in [
                (&mut config.tick_interval_minutes, "tickIntervalMinutes"),
                (&mut config.cooldown_minutes, "cooldownMinutes"),
                (&mut config.daily_budget, "dailyBudget"),
                (&mut config.heartbeat_stale_seconds, "heartbeatStaleSeconds"),
                (&mut config.recent_user_msg_minutes, "recentUserMsgMinutes"),
            ] {
                if let Some(value) = trigger.get(key).and_then(JsValue::as_number) {
                    *slot = value;
                }
            }
            if let Some(channel) = trigger
                .get("preferChannel")
                .and_then(JsValue::as_string)
                .and_then(|value| value.to_utf8().ok())
            {
                config.prefer_channel = channel;
            }
        }
        if let Some(projects) = section.get("projects").and_then(JsValue::as_object) {
            for (key, value) in projects.entries() {
                let Some(key) = key.to_utf8().ok() else {
                    continue;
                };
                let enabled = value.as_object().is_some_and(|project| {
                    project.get("enabled").and_then(JsValue::as_bool) == Some(true)
                });
                config.projects.push((key, enabled));
            }
        }
        if let Some(language) = section
            .get("language")
            .and_then(JsValue::as_string)
            .and_then(|value| value.to_utf8().ok())
        {
            config.language = Some(language);
        }
        config
    }

    /// `config.projects[projectKey]` — `None` is the not-listed key, which the
    /// first gate reads as disabled.
    pub fn project(&self, project_key: &str) -> Option<bool> {
        self.projects
            .iter()
            .find(|(key, _)| key == project_key)
            .map(|(_, enabled)| *enabled)
    }
}
