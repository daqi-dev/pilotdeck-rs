//! #2294: port of TS `src/cli/ProjectAutomationBundle.ts` — the
//! application-owned bundle that composes the project automation providers
//! (Always-On + cron) and owns their composition generations and lifecycle
//! order. Native managers retain scheduler, storage, active-run and drain
//! ownership; this bundle only decides **when** each provider is
//! constructed, bound, started and stopped:
//!
//! - `createAlwaysOn` conditions (:287-288): no `alwaysOn` config, or no
//!   project with `enabled: true`, means no provider — the factory only
//!   ever sees a live config.
//! - startup order `startGeneration` (:328-331): Always-On first, then cron;
//!   stop order `stopGeneration` (:333-356) is its reverse — cron drains
//!   before Always-On "so producer dependencies drain in reverse startup
//!   order".
//! - reload (:191-257) touches only the changed families, and its failure
//!   paths are pinned by the b-series golden records: stop failure refuses
//!   the replacement, a candidate start failure restores the previous
//!   generation, and a restore failure publishes a provider-less generation.
//!
//! One deliberate deviation, asserted by the b-series replay: TS publishes a
//! **standalone fallback control** when no Always-On provider exists
//! (`createFallbackAlwaysOnControl`, :378-391), whose `applyCycle` submits
//! turns through the agent gateway (`createApplyHandler`). That apply path
//! is the fire pipeline this port leaves behind its seam
//! (`runtime/always_on/fire.rs`), so the port publishes `None` instead — the
//! gateway keeps answering the #1313 `not_configured` envelopes when no
//! manager is configured, which is the honest state of the embedding. The
//! golden "fallback-factory" log lines are filtered by the replay and the
//! b9/b11 records assert the deviation directly.
//!
//! Concurrency: TS's bundle is single-threaded by construction (one CLI
//! owner). The port's `Mutex` only guards the generation/attachment/started
//! fields across the `async` lifecycle calls — no guard is held across an
//! await — and the native assembly drives exactly one `attach` / `start` /
//! `stop` sequence from one owner.
use std::{
    fmt,
    future::Future,
    io,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, Mutex},
};

use pilotdeck_protocol::{JsString, JsValue};

use crate::runtime::{
    always_on::{AlwaysOnAgentGateway, AlwaysOnConfig, AlwaysOnManager},
    cron::{CronAgentGateway, CronManager},
};

/// One provider lifecycle call: `Ok` or the thrown `Error.message`
/// (`always_on start boom` in the b-series records).
pub(crate) type AutomationFuture = Pin<Box<dyn Future<Output = Result<(), String>> + Send>>;

/// `attachment.isProjectBusy` (ProjectAutomationBundle.ts:64): feeds the
/// always-on `agent_busy` gate with real session data. The always-on module
/// owns the hook type (its `bindAgentGateway` signature reads it the same
/// way, AlwaysOnManager.ts:147-160); this re-export keeps the bundle's own
/// name for it.
pub(crate) use crate::runtime::always_on::IsProjectBusy;

/// One lifecycle failure, shaped the way the b-series golden records name
/// it: a single provider rejection crosses as `Error` with its message; the
/// bundle's combination sites wrap several as `AggregateError` with the
/// messages of everything that failed.
#[derive(Clone, Debug)]
pub(crate) struct AutomationError {
    /// `Error` or `AggregateError` — the TS constructor name. The b-series
    /// replays assert it; the shipping crossing flattens to one message
    /// string, so no non-test reader needs it.
    #[cfg_attr(not(test), allow(dead_code))]
    pub name: &'static str,
    pub message: String,
    /// The wrapped failure messages (`AggregateError` only).
    pub errors: Vec<String>,
}

impl AutomationError {
    fn provider(message: String) -> Self {
        Self {
            name: "Error",
            message,
            errors: Vec::new(),
        }
    }

    /// `combineFailure` (ProjectAutomationBundle.ts:414-417): an
    /// `AggregateError` over every defined failure message — one wrapped
    /// error stays an aggregate (b4), unlike `stopGeneration`'s pass-through.
    fn combine(message: &str, errors: Vec<Option<String>>) -> Self {
        Self {
            name: "AggregateError",
            message: message.into(),
            errors: errors.into_iter().flatten().collect(),
        }
    }
}

impl fmt::Display for AutomationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The aggregate carries its causes in the message — TS prints them
        // as the AggregateError's `errors`; this embedding's failures cross
        // as one string, and a cause that does not surface would be a
        // swallowed start/stop failure.
        if self.errors.is_empty() {
            write!(formatter, "{}", self.message)
        } else {
            write!(formatter, "{}: {}", self.message, self.errors.join("; "))
        }
    }
}

/// The turn facade both families bind — TS's one
/// `ProjectAutomationAgentGateway = AlwaysOnAgentGatewayPort &
/// CronAgentGatewayPort` object. The port keeps the two faces as separate
/// trait objects: the always-on half only ever calls `abortTurn`
/// (`AlwaysOnRuntime.ts:387-392`), and the cron face is the
/// `SessionAdmissionCronGateway` the #1326 wiring already constructs.
type NativeCronBind =
    Arc<dyn Fn(Arc<CronManager>) -> io::Result<Arc<crate::switch::CronStopHandle>> + Send + Sync>;

#[derive(Clone)]
pub(crate) struct ProjectAutomationAgentGateway {
    /// The always-on `abortTurn` face.
    pub always_on: Arc<dyn AlwaysOnAgentGateway>,
    /// The cron `submitTurn` / `abortTurn` / `closeSession` face.
    pub cron: Arc<dyn CronAgentGateway>,
    /// Native provider generations construct their own cancellation and drain
    /// handle while sharing this host's session owner and turn submitter.
    pub native_cron_bind: Option<NativeCronBind>,
}

/// The hooks record the always-on bind signature takes
/// (`bindAgentGateway(gateway, hooks?)`, AlwaysOnManager.ts:147-160). TS
/// always passes the `{isProjectBusy}` object — its member may be undefined
/// — so the record itself is what marks an always-on bind; the cron face
/// takes no hooks at all.
pub(crate) struct AlwaysOnBindHooks<'a> {
    pub is_project_busy: Option<&'a IsProjectBusy>,
}

/// One provider (TS `ProjectAutomationAlwaysOnProvider` /
/// `ProjectAutomationCronProvider`): the two TS types share this lifecycle
/// surface and differ only in whether bind receives the hooks record.
pub(crate) trait ProjectAutomationProvider: Send + Sync {
    /// `bindAgentGateway`. Always called before `start` (the bind gate the
    /// managers themselves enforce); the hooks record is present for the
    /// always-on family only.
    fn bind_agent_gateway(
        &self,
        gateway: &ProjectAutomationAgentGateway,
        hooks: Option<AlwaysOnBindHooks<'_>>,
    ) -> Result<(), io::Error>;
    fn start(self: Arc<Self>) -> AutomationFuture;
    fn stop(self: Arc<Self>) -> AutomationFuture;
    /// `getTools()`: the tool **names** the publish consumer composes into
    /// `extraTools`. The TS definitions carry schemas the native process
    /// never composes (the discovery tool is the fire seam's), so the ported
    /// surface is the name list.
    fn tools(&self) -> Vec<JsString>;
    /// The always-on control manager behind the provider. TS's provider
    /// **is** the control (`ProjectAutomationAlwaysOnProvider extends
    /// AlwaysOnControlPort`); the Rust manager sits behind the native
    /// provider wrapper, and this accessor is where that identity lands.
    /// `None` on the cron provider and the test fakes.
    fn always_on_manager(&self) -> Option<Arc<AlwaysOnManager>> {
        None
    }
    /// The cron control manager behind the provider — same story, the cron
    /// half.
    fn cron_manager(&self) -> Option<Arc<CronManager>> {
        None
    }
}

/// `SubsystemUpdate` (createLocalGateway.ts:548-553) as the port publishes
/// it. `always_on_control` is `None` where TS publishes the standalone
/// fallback — see the module docs for that deviation.
// The shipping consumer is the assembly's documented no-op
// `update_subsystems` (this embedding has no hot reload); the b-series
// replays read all three fields.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct SubsystemUpdate {
    pub extra_tools: Vec<JsString>,
    pub cron: Option<Arc<dyn ProjectAutomationProvider>>,
    pub always_on_control: Option<Arc<dyn ProjectAutomationProvider>>,
}

/// One atomic publication of both control families. Requests pin their
/// current manager before awaiting; retired generations keep owning their runs.
#[derive(Default)]
pub(crate) struct PublishedAutomation {
    state: Mutex<PublishedState>,
}
#[derive(Default)]
struct PublishedState {
    cron: Option<Arc<CronManager>>,
    control: Option<Arc<dyn crate::runtime::always_on::AlwaysOnControl>>,
}
impl PublishedAutomation {
    pub(crate) fn publish(
        &self,
        update: SubsystemUpdate,
        standalone: Arc<dyn crate::runtime::always_on::StandaloneApply>,
    ) {
        let manager = update
            .always_on_control
            .as_ref()
            .and_then(|p| p.always_on_manager());
        let control = crate::runtime::always_on::create_control(manager, standalone);
        *self.state.lock().unwrap_or_else(|e| e.into_inner()) = PublishedState {
            cron: update.cron.as_ref().and_then(|p| p.cron_manager()),
            control: Some(control),
        };
    }
    #[cfg(test)]
    pub(crate) fn set_control_for_test(
        &self,
        control: Arc<dyn crate::runtime::always_on::AlwaysOnControl>,
    ) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).control = Some(control);
    }
    pub(crate) fn clear(&self) {
        *self.state.lock().unwrap_or_else(|e| e.into_inner()) = PublishedState::default();
    }
    pub(crate) fn cron(&self) -> Option<Arc<CronManager>> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .cron
            .clone()
    }
    pub(crate) fn control(&self) -> Option<Arc<dyn crate::runtime::always_on::AlwaysOnControl>> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .control
            .clone()
    }
}

/// `ProjectAutomationAttachment` (:63-67): the one turn facade plus the
/// subsystem publication the bundle drives through it.
#[derive(Clone)]
pub(crate) struct ProjectAutomationAttachment {
    pub agent_gateway: ProjectAutomationAgentGateway,
    pub is_project_busy: Option<IsProjectBusy>,
    pub update_subsystems: Arc<dyn Fn(SubsystemUpdate) + Send + Sync>,
}

/// `ProjectAutomationConfig` (:30-34): the two normalized config sections.
/// `None` is the section's absence — the exact create-condition TS reads.
#[derive(Clone)]
pub(crate) struct ProjectAutomationConfig {
    pub always_on: Option<AlwaysOnConfig>,
    pub cron: Option<JsValue>,
}

/// `ProjectAutomationReloadInput` (:75-79). Constructed by the reload
/// tests; no production caller exists until this embedding grows a
/// config-change trigger.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct ProjectAutomationReloadInput {
    pub config: ProjectAutomationConfig,
    pub always_on_changed: bool,
    pub cron_changed: bool,
}

/// `ProjectAutomationStatus` (:81-84): which families have a provider.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ProjectAutomationStatus {
    pub always_on: bool,
    pub cron: bool,
}

/// The factory inputs. TS passes one large options object per family
/// (`CreateAlwaysOnManagerOptions` / `CreateCronManagerOptions`); the port
/// carries the two fields the native constructors consume.
pub(crate) struct AlwaysOnFactoryInput {
    pub config: AlwaysOnConfig,
    pub pilot_home: PathBuf,
}

pub(crate) struct CronFactoryInput {
    /// The normalized `cron:` section — presence was the bundle's gate, the
    /// fields are `cron::build`'s to read.
    pub section: JsValue,
    pub pilot_home: PathBuf,
}

pub(crate) type AlwaysOnFactory = Arc<
    dyn Fn(AlwaysOnFactoryInput) -> Result<Arc<dyn ProjectAutomationProvider>, io::Error>
        + Send
        + Sync,
>;
pub(crate) type CronFactory = Arc<
    dyn Fn(CronFactoryInput) -> Result<Arc<dyn ProjectAutomationProvider>, io::Error> + Send + Sync,
>;

/// One staged generation (:116-120): the config it was built from and the
/// two optional providers.
#[derive(Clone)]
struct Generation {
    // Written by every stage; read only by the reload restore path
    // (`stage(&previous.config, ...)`), which no production caller drives.
    #[cfg_attr(not(test), allow(dead_code))]
    config: ProjectAutomationConfig,
    always_on: Option<Arc<dyn ProjectAutomationProvider>>,
    cron: Option<Arc<dyn ProjectAutomationProvider>>,
}

#[derive(Clone, Copy)]
struct Changes {
    always_on_changed: bool,
    cron_changed: bool,
}
const ALL_CHANGED: Changes = Changes {
    always_on_changed: true,
    cron_changed: true,
};

struct State {
    generation: Generation,
    attachment: Option<ProjectAutomationAttachment>,
    started: bool,
    /// #2486: a start that has begun and not verifiably finished — set before
    /// `start`'s first await, cleared when the start (or its own failure
    /// cleanup) completes. A caller may drop the start future at any await
    /// point (the assembly's single-poll builder does), leaving providers
    /// half-started with `started` still clear; `stop` reads this to still
    /// drain that generation.
    attempted: bool,
}

pub(crate) struct ProjectAutomationBundleOptions {
    pub config: ProjectAutomationConfig,
    pub pilot_home: PathBuf,
    pub make_always_on: AlwaysOnFactory,
    pub make_cron: CronFactory,
}

/// `ProjectAutomationBundle` (:131-412). The three factory fields feed the
/// reload ladder's re-staging (`stage`); `new` stages its first generation
/// from its own options instead, so outside the reload tests nothing reads
/// them.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) struct ProjectAutomationBundle {
    make_always_on: AlwaysOnFactory,
    make_cron: CronFactory,
    pilot_home: PathBuf,
    state: Mutex<State>,
}

fn status_of(generation: &Generation) -> ProjectAutomationStatus {
    ProjectAutomationStatus {
        always_on: generation.always_on.is_some(),
        cron: generation.cron.is_some(),
    }
}

impl ProjectAutomationBundle {
    /// The constructor (:142-151) is `stage` and nothing else — construction
    /// is intentionally effect-free apart from the factories themselves.
    pub(crate) fn new(options: ProjectAutomationBundleOptions) -> Result<Self, io::Error> {
        let generation = Self::stage_with(
            &options.config,
            None,
            ALL_CHANGED,
            &options.pilot_home,
            &options.make_always_on,
            &options.make_cron,
        )?;
        Ok(Self {
            make_always_on: options.make_always_on,
            make_cron: options.make_cron,
            pilot_home: options.pilot_home,
            state: Mutex::new(State {
                generation,
                attachment: None,
                started: false,
                attempted: false,
            }),
        })
    }

    /// `createAlwaysOn` (:287-288): no config, or no project with
    /// `enabled: true`, means no provider.
    fn create_always_on(
        config: Option<&AlwaysOnConfig>,
        pilot_home: &Path,
        factory: &AlwaysOnFactory,
    ) -> Result<Option<Arc<dyn ProjectAutomationProvider>>, io::Error> {
        let Some(config) = config else {
            return Ok(None);
        };
        if !config.projects.iter().any(|(_, enabled)| *enabled) {
            return Ok(None);
        }
        Ok(Some(factory(AlwaysOnFactoryInput {
            config: config.clone(),
            pilot_home: pilot_home.to_path_buf(),
        })?))
    }

    /// `createCron` (:304-306): the section's presence is the whole gate.
    fn create_cron(
        section: Option<&JsValue>,
        pilot_home: &Path,
        factory: &CronFactory,
    ) -> Result<Option<Arc<dyn ProjectAutomationProvider>>, io::Error> {
        let Some(section) = section else {
            return Ok(None);
        };
        Ok(Some(factory(CronFactoryInput {
            section: section.clone(),
            pilot_home: pilot_home.to_path_buf(),
        })?))
    }

    /// `stage` (:271-285): an unchanged family reuses the previous
    /// generation's provider — the reuse b6/b10 pin.
    fn stage_with(
        config: &ProjectAutomationConfig,
        previous: Option<&Generation>,
        changes: Changes,
        pilot_home: &Path,
        make_always_on: &AlwaysOnFactory,
        make_cron: &CronFactory,
    ) -> Result<Generation, io::Error> {
        let always_on = match previous {
            Some(previous) if !changes.always_on_changed => previous.always_on.clone(),
            _ => Self::create_always_on(config.always_on.as_ref(), pilot_home, make_always_on)?,
        };
        let cron = match previous {
            Some(previous) if !changes.cron_changed => previous.cron.clone(),
            _ => Self::create_cron(config.cron.as_ref(), pilot_home, make_cron)?,
        };
        Ok(Generation {
            config: config.clone(),
            always_on,
            cron,
        })
    }

    // `new` stages its first generation from its own options; `stage` itself
    // is the reload ladder's re-staging entry, which only the reload tests
    // drive until this embedding grows a config-change trigger.
    #[cfg_attr(not(test), allow(dead_code))]
    fn stage(
        &self,
        config: &ProjectAutomationConfig,
        previous: Option<&Generation>,
        changes: Changes,
    ) -> Result<Generation, io::Error> {
        Self::stage_with(
            config,
            previous,
            changes,
            &self.pilot_home,
            &self.make_always_on,
            &self.make_cron,
        )
    }

    /// `attach` (:163-169): the once-only lifecycle hand-off; the initial
    /// bind covers both families.
    pub(crate) fn attach(&self, attachment: ProjectAutomationAttachment) -> Result<(), io::Error> {
        let generation = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.attachment.is_some() {
                return Err(io::Error::other(
                    "ProjectAutomationBundle.attach may only be called once.",
                ));
            }
            state.attachment = Some(attachment.clone());
            state.generation.clone()
        };
        Self::bind_generation(&generation, ALL_CHANGED, &attachment)
    }

    fn require_attachment(
        attachment: Option<ProjectAutomationAttachment>,
    ) -> Result<ProjectAutomationAttachment, AutomationError> {
        attachment.ok_or_else(|| {
            AutomationError::provider(
                "ProjectAutomationBundle must attach an agent gateway before lifecycle operations."
                    .into(),
            )
        })
    }

    /// `bindGeneration` (:318-326): the always-on face receives the hooks
    /// record, the cron face none.
    fn bind_generation(
        generation: &Generation,
        changes: Changes,
        attachment: &ProjectAutomationAttachment,
    ) -> Result<(), io::Error> {
        if changes.always_on_changed
            && let Some(always_on) = &generation.always_on
        {
            always_on.bind_agent_gateway(
                &attachment.agent_gateway,
                Some(AlwaysOnBindHooks {
                    is_project_busy: attachment.is_project_busy.as_ref(),
                }),
            )?;
        }
        if changes.cron_changed
            && let Some(cron) = &generation.cron
        {
            cron.bind_agent_gateway(&attachment.agent_gateway, None)?;
        }
        Ok(())
    }

    /// `startGeneration` (:328-331): Always-On first, then cron.
    async fn start_generation(generation: &Generation, changes: Changes) -> Result<(), String> {
        if changes.always_on_changed
            && let Some(always_on) = &generation.always_on
        {
            always_on.clone().start().await?;
        }
        if changes.cron_changed
            && let Some(cron) = &generation.cron
        {
            cron.clone().start().await?;
        }
        Ok(())
    }

    /// `stopGeneration` (:333-356): **cron first, then Always-On**, so
    /// producer dependencies drain in reverse startup order. One failure
    /// passes through raw; two or more become the cleanup `AggregateError`.
    async fn stop_generation(generation: &Generation, changes: Changes) -> Option<AutomationError> {
        let mut failures = Vec::new();
        if changes.cron_changed
            && let Some(cron) = &generation.cron
            && let Err(error) = cron.clone().stop().await
        {
            failures.push(error);
        }
        if changes.always_on_changed
            && let Some(always_on) = &generation.always_on
            && let Err(error) = always_on.clone().stop().await
        {
            failures.push(error);
        }
        match failures.len() {
            0 => None,
            1 => Some(AutomationError::provider(
                failures.pop().expect("#2294 one failure recorded"),
            )),
            _ => Some(AutomationError {
                name: "AggregateError",
                message: "Project automation provider cleanup failed.".into(),
                errors: failures,
            }),
        }
    }

    /// `publishGeneration` (:358-365) — minus the TS fallback control (see
    /// the module docs).
    fn publish_generation(generation: &Generation, attachment: &ProjectAutomationAttachment) {
        let mut extra_tools = Vec::new();
        if let Some(always_on) = &generation.always_on {
            extra_tools.extend(always_on.tools());
        }
        if let Some(cron) = &generation.cron {
            extra_tools.extend(cron.tools());
        }
        (attachment.update_subsystems)(SubsystemUpdate {
            extra_tools,
            cron: generation.cron.clone(),
            always_on_control: generation.always_on.clone(),
        });
    }

    /// `unpublish` (:368-375): the stop-time update removes every consumer.
    fn unpublish(attachment: &ProjectAutomationAttachment) {
        (attachment.update_subsystems)(SubsystemUpdate {
            extra_tools: Vec::new(),
            cron: None,
            always_on_control: None,
        });
    }

    /// `start` (:172-184): start both, and a failure tears the generation
    /// back down before it crosses as the startup `AggregateError` (b4).
    pub(crate) async fn start(&self) -> Result<ProjectAutomationStatus, AutomationError> {
        let (generation, attachment) = {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.started {
                return Ok(status_of(&state.generation));
            }
            (
                state.generation.clone(),
                Self::require_attachment(state.attachment.clone())?,
            )
        };
        // #2486: the first await below can be the last time this future is
        // polled — the assembly's synchronous builder drives `start` with a
        // single `now_or_never` poll, and the failure cleanup's always-on
        // stop joins the ticker, which only a runtime pass can complete.
        // `attempted` is what survives the drop, so the teardown's `stop`
        // still drains this half-started generation.
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .attempted = true;
        if let Err(error) = Self::start_generation(&generation, ALL_CHANGED).await {
            let cleanup = Self::stop_generation(&generation, ALL_CHANGED).await;
            // A completed cleanup leaves nothing for a later stop to drain;
            // a failed one may, so `attempted` stays set for the teardown's
            // compensating `stop` to retry.
            if cleanup.is_none() {
                self.state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .attempted = false;
            }
            return Err(AutomationError::combine(
                "Project automation startup failed.",
                vec![Some(error), cleanup.map(|failure| failure.message)],
            ));
        }
        {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.started = true;
            state.attempted = false;
        }
        Self::publish_generation(&generation, &attachment);
        Ok(status_of(&generation))
    }

    /// `reload` (:191-257). See the module docs for the failure ladder.
    /// No production caller exists yet — this embedding has no config
    /// hot-reload (the assembly's `update_subsystems` is a documented
    /// no-op) — so the b-series reload replays are the only driver.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) async fn reload(
        &self,
        input: ProjectAutomationReloadInput,
    ) -> Result<ProjectAutomationStatus, AutomationError> {
        let changes = Changes {
            always_on_changed: input.always_on_changed,
            cron_changed: input.cron_changed,
        };
        if !changes.always_on_changed && !changes.cron_changed {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            return Ok(status_of(&state.generation));
        }
        let (previous, started, attachment) = {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            (
                state.generation.clone(),
                state.started,
                state.attachment.clone(),
            )
        };
        // Construction is intentionally effect-free (:200-203): a malformed
        // new config cannot tear down a working provider generation, and a
        // factory failure here propagates without touching the current one.
        let config = input.config.clone();
        let staging_previous = previous.clone();
        let home = self.pilot_home.clone();
        let make_always_on = self.make_always_on.clone();
        let make_cron = self.make_cron.clone();
        let candidate = tokio::task::spawn_blocking(move || {
            Self::stage_with(
                &config,
                Some(&staging_previous),
                changes,
                &home,
                &make_always_on,
                &make_cron,
            )
        })
        .await
        .map_err(|error| AutomationError::provider(error.to_string()))?
        .map_err(|error| AutomationError::provider(error.to_string()))?;
        if !started {
            let generation = candidate;
            let attachment = match attachment {
                Some(attachment) => attachment,
                None => return Ok(status_of(&generation)),
            };
            Self::bind_generation(&generation, changes, &attachment)
                .map_err(|error| AutomationError::provider(error.to_string()))?;
            self.state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .generation = generation.clone();
            return Ok(status_of(&generation));
        }
        let attachment = Self::require_attachment(attachment)?;
        // Stop forwarding Gateway controls to a provider that is about to
        // drain (:212-217): the transitional generation carries only the
        // families that survive the reload.
        let transitional = Generation {
            always_on: (!changes.always_on_changed)
                .then(|| previous.always_on.clone())
                .flatten(),
            cron: (!changes.cron_changed)
                .then(|| previous.cron.clone())
                .flatten(),
            config: input.config.clone(),
        };
        Self::publish_generation(&transitional, &attachment);
        let previous_stop = Self::stop_generation(&previous, changes).await;
        if let Some(error) = previous_stop {
            return Err(AutomationError {
                name: "AggregateError",
                message: "Project automation reload could not stop the previous generation; no replacement was started."
                    .into(),
                errors: vec![error.message],
            });
        }
        let candidate_failure = async {
            Self::bind_generation(&candidate, changes, &attachment)
                .map_err(|error| error.to_string())?;
            Self::start_generation(&candidate, changes).await?;
            Ok::<(), String>(())
        }
        .await;
        if let Err(error) = candidate_failure {
            let candidate_stop = Self::stop_generation(&candidate, changes).await;
            // The restore (:236-240): re-stage the previous config and start
            // it. Its own failure — stage, bind or start — is `restoreError`.
            let restored = async {
                let restored = self
                    .stage(&previous.config, Some(&previous), changes)
                    .map_err(|error| error.to_string())?;
                Self::bind_generation(&restored, changes, &attachment)
                    .map_err(|error| error.to_string())?;
                Self::start_generation(&restored, changes).await?;
                Ok::<Generation, String>(restored)
            }
            .await;
            match restored {
                Ok(restored) => {
                    self.state
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .generation = restored.clone();
                    Self::publish_generation(&restored, &attachment);
                    Err(AutomationError::combine(
                        "Project automation reload failed; the previous generation was restored.",
                        vec![Some(error), candidate_stop.map(|failure| failure.message)],
                    ))
                }
                Err(restore_error) => {
                    self.state
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .generation = transitional.clone();
                    Self::publish_generation(&transitional, &attachment);
                    Err(AutomationError::combine(
                        "Project automation reload failed and the previous generation could not be restored.",
                        vec![
                            Some(error),
                            candidate_stop.map(|failure| failure.message),
                            Some(restore_error),
                        ],
                    ))
                }
            }
        } else {
            self.state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .generation = candidate.clone();
            Self::publish_generation(&candidate, &attachment);
            Ok(status_of(&candidate))
        }
    }

    /// `stop` (:260-269): reverse dependency order; the started flag and the
    /// unpublish happen even when a provider stop fails (the TS `finally`).
    /// #2486: a start whose future was dropped mid-flight leaves `attempted`
    /// set with `started` clear — that generation still gets its drain here,
    /// which is the compensating stop the host's failed-initialization
    /// teardown performs after the builder's single poll read `None`.
    pub(crate) async fn stop(&self) -> Result<(), AutomationError> {
        let generation = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if !state.started && !state.attempted {
                return Ok(());
            }
            state.started = false;
            state.attempted = false;
            state.generation.clone()
        };
        let failure = Self::stop_generation(&generation, ALL_CHANGED).await;
        if let Some(attachment) = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .attachment
            .clone()
        {
            Self::unpublish(&attachment);
        }
        match failure {
            None => Ok(()),
            Some(error) => Err(error),
        }
    }

    /// The staged generation's always-on control manager — the face TS's
    /// `getInitialGatewayOptions` / `publishGeneration` hand the gateway
    /// (where the provider *is* the manager; see the trait docs).
    #[cfg(test)]
    pub(crate) fn always_on_manager(&self) -> Option<Arc<AlwaysOnManager>> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state
            .generation
            .always_on
            .as_ref()
            .and_then(|provider| provider.always_on_manager())
    }

    /// The staged generation's cron control manager — the cron half of the
    /// same composition face (`getInitialGatewayOptions().cron`).
    pub(crate) fn cron_manager(&self) -> Option<Arc<CronManager>> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state
            .generation
            .cron
            .as_ref()
            .and_then(|provider| provider.cron_manager())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::negative_assert_eq;
    use crate::runtime::always_on::GatewayAbortInput;
    use crate::runtime::cron::CronError;
    use std::future::Future;

    const GOLDEN: &str = include_str!("../../../tests/fixtures/always-on-2294/oracle.json");

    fn golden(id: &str) -> serde_json::Value {
        let records: Vec<serde_json::Value> =
            serde_json::from_str(GOLDEN).expect("#2294 golden parses");
        records
            .iter()
            .find(|record| record["id"].as_str() == Some(id))
            .map(|record| record["value"].clone())
            .unwrap_or_else(|| panic!("#2294 golden record {id} missing"))
    }

    /// The golden logs carry the TS fallback factory's lines; the port has
    /// no fallback (module docs), so the replay filters them and the b9/b11
    /// tests assert the deviation directly.
    fn expected_log(id: &str) -> Vec<String> {
        lines_of(&golden(id)["log"])
    }

    fn lines_of(value: &serde_json::Value) -> Vec<String> {
        value
            .as_array()
            .unwrap_or_else(|| panic!("#2294 log array"))
            .iter()
            .map(|line| line.as_str().expect("#2294 log line").to_string())
            .filter(|line| line != "fallback-factory")
            .collect()
    }

    fn actual_log(log: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
        log.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn push(log: &Arc<Mutex<Vec<String>>>, line: String) {
        log.lock().unwrap_or_else(|e| e.into_inner()).push(line);
    }

    /// One b-series fake (`makeProvider`, generate.mts:627-635): every
    /// lifecycle call is logged, start/stop can throw, and the bind line
    /// records whether the hooks record was present.
    struct FakeProvider {
        name: &'static str,
        log: Arc<Mutex<Vec<String>>>,
        start_throws: Option<String>,
        stop_throws: Option<String>,
    }

    fn fake(
        name: &'static str,
        log: &Arc<Mutex<Vec<String>>>,
    ) -> Arc<dyn ProjectAutomationProvider> {
        provider(name, log, None, None)
    }

    fn provider(
        name: &'static str,
        log: &Arc<Mutex<Vec<String>>>,
        start_throws: Option<&str>,
        stop_throws: Option<&str>,
    ) -> Arc<dyn ProjectAutomationProvider> {
        Arc::new(FakeProvider {
            name,
            log: log.clone(),
            start_throws: start_throws.map(str::to_string),
            stop_throws: stop_throws.map(str::to_string),
        })
    }

    impl ProjectAutomationProvider for FakeProvider {
        fn bind_agent_gateway(
            &self,
            _gateway: &ProjectAutomationAgentGateway,
            hooks: Option<AlwaysOnBindHooks<'_>>,
        ) -> Result<(), io::Error> {
            push(
                &self.log,
                format!(
                    "{}.bind:{}",
                    self.name,
                    if hooks.is_some() { "hooks" } else { "none" }
                ),
            );
            Ok(())
        }
        fn start(self: Arc<Self>) -> AutomationFuture {
            Box::pin(async move {
                push(&self.log, format!("{}.start", self.name));
                match self.start_throws.clone() {
                    Some(message) => Err(message),
                    None => Ok(()),
                }
            })
        }
        fn stop(self: Arc<Self>) -> AutomationFuture {
            Box::pin(async move {
                push(&self.log, format!("{}.stop", self.name));
                match self.stop_throws.clone() {
                    Some(message) => Err(message),
                    None => Ok(()),
                }
            })
        }
        fn tools(&self) -> Vec<JsString> {
            vec![format!("{}-tool", self.name).into()]
        }
    }

    /// #2486: the always-on provider in the shape the assembly's single
    /// poll sees in production — `start` spawns a task
    /// (`AlwaysOnScheduler::start`'s `tokio::spawn`), `stop` flags, wakes,
    /// and joins it (the `task.await` in `AlwaysOnScheduler::stop`), so a
    /// failing start's cleanup yields exactly where the production one does:
    /// the join cannot complete without a runtime pass.
    struct TickerProvider {
        stopped: Arc<std::sync::atomic::AtomicBool>,
        notify: Arc<tokio::sync::Notify>,
        task: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
        /// Set by the ticker when it observes the stop flag and returns —
        /// read after a stop to prove nothing was left running.
        finished: Arc<std::sync::atomic::AtomicBool>,
    }

    impl TickerProvider {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                stopped: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                notify: Arc::new(tokio::sync::Notify::new()),
                task: Arc::new(Mutex::new(None)),
                finished: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            })
        }

        fn finished(&self) -> bool {
            self.finished.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    impl ProjectAutomationProvider for TickerProvider {
        fn bind_agent_gateway(
            &self,
            _gateway: &ProjectAutomationAgentGateway,
            _hooks: Option<AlwaysOnBindHooks<'_>>,
        ) -> Result<(), io::Error> {
            Ok(())
        }
        fn start(self: Arc<Self>) -> AutomationFuture {
            let (stopped, notify, task, finished) = (
                self.stopped.clone(),
                self.notify.clone(),
                self.task.clone(),
                self.finished.clone(),
            );
            Box::pin(async move {
                let handle = tokio::spawn(async move {
                    loop {
                        notify.notified().await;
                        if stopped.load(std::sync::atomic::Ordering::SeqCst) {
                            break;
                        }
                    }
                    finished.store(true, std::sync::atomic::Ordering::SeqCst);
                });
                *task.lock().unwrap_or_else(|e| e.into_inner()) = Some(handle);
                Ok(())
            })
        }
        fn stop(self: Arc<Self>) -> AutomationFuture {
            let (stopped, notify, task) =
                (self.stopped.clone(), self.notify.clone(), self.task.clone());
            Box::pin(async move {
                stopped.store(true, std::sync::atomic::Ordering::SeqCst);
                notify.notify_one();
                let handle = task.lock().unwrap_or_else(|e| e.into_inner()).take();
                if let Some(handle) = handle {
                    let _ = handle.await;
                }
                Ok(())
            })
        }
        fn tools(&self) -> Vec<JsString> {
            Vec::new()
        }
    }

    struct DummyAlwaysOnGateway;
    impl AlwaysOnAgentGateway for DummyAlwaysOnGateway {
        fn run_phase<'a>(
            &'a self,
            _: crate::runtime::always_on::PhaseRequest,
        ) -> Pin<
            Box<
                dyn Future<
                        Output = Result<
                            crate::runtime::always_on::PhaseResult,
                            crate::runtime::always_on::ControlError,
                        >,
                    > + Send
                    + 'a,
            >,
        > {
            Box::pin(async {
                Err(crate::runtime::always_on::ControlError {
                    code: "unexpected_phase".into(),
                    message: "Lifecycle test cannot execute a phase.".into(),
                })
            })
        }
        fn abort_turn<'a>(
            &'a self,
            _input: GatewayAbortInput<'a>,
        ) -> Pin<Box<dyn Future<Output = Result<(), JsString>> + Send + 'a>> {
            Box::pin(async { Ok(()) })
        }
    }

    struct DummyCronGateway;
    impl CronAgentGateway for DummyCronGateway {
        fn submit_turn(&self, _: &crate::runtime::cron::CronTurnSubmit) -> Result<(), CronError> {
            Ok(())
        }
        fn abort_turn(
            &self,
            _: &JsString,
            _: Option<&JsString>,
            _: Option<&JsString>,
        ) -> Result<(), CronError> {
            Ok(())
        }
        fn close_session(&self, _: &JsString, _: Option<&JsString>) -> Result<(), CronError> {
            Ok(())
        }
    }

    /// The recorded publish (`makeBundle`'s `opts`, generate.mts:636-647).
    #[derive(Default)]
    struct Opts {
        ao_count: u32,
        cron_count: u32,
    }

    fn make_bundle(
        log: Arc<Mutex<Vec<String>>>,
        config: ProjectAutomationConfig,
        ao_providers: Vec<Arc<dyn ProjectAutomationProvider>>,
        cron_providers: Vec<Arc<dyn ProjectAutomationProvider>>,
    ) -> (ProjectAutomationBundle, Arc<Mutex<Opts>>) {
        let opts = Arc::new(Mutex::new(Opts::default()));
        let ao_opts = opts.clone();
        let ao_log = log.clone();
        let make_always_on: AlwaysOnFactory = Arc::new(move |_input| {
            let mut opts = ao_opts.lock().unwrap_or_else(|e| e.into_inner());
            opts.ao_count += 1;
            let count = opts.ao_count;
            drop(opts);
            push(&ao_log, format!("ao-factory:{count}"));
            Ok(ao_providers
                .get(count as usize - 1)
                .cloned()
                .unwrap_or_else(|| {
                    provider(
                        Box::leak(format!("ao#{count}").into_boxed_str()),
                        &ao_log,
                        None,
                        None,
                    )
                }))
        });
        let cron_opts = opts.clone();
        let cron_log = log.clone();
        let make_cron: CronFactory = Arc::new(move |_input| {
            let mut opts = cron_opts.lock().unwrap_or_else(|e| e.into_inner());
            opts.cron_count += 1;
            let count = opts.cron_count;
            drop(opts);
            push(&cron_log, format!("cron-factory:{count}"));
            Ok(cron_providers
                .get(count as usize - 1)
                .cloned()
                .unwrap_or_else(|| {
                    provider(
                        Box::leak(format!("cron#{count}").into_boxed_str()),
                        &cron_log,
                        None,
                        None,
                    )
                }))
        });
        (
            ProjectAutomationBundle::new(ProjectAutomationBundleOptions {
                config,
                pilot_home: PathBuf::from("/tmp/probe-home"),
                make_always_on,
                make_cron,
            })
            .expect("#2294 bundle stages"),
            opts,
        )
    }

    fn enabled_ao_config() -> AlwaysOnConfig {
        let mut config = AlwaysOnConfig::default_config();
        config.projects = vec![("/p".into(), true)];
        config
    }

    #[tokio::test]
    async fn native_reload_queue_orders_generations_and_shutdown_is_idempotent() {
        struct PausedStop {
            log: Arc<Mutex<Vec<String>>>,
            entered: Arc<tokio::sync::Notify>,
            release: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
        }
        impl ProjectAutomationProvider for PausedStop {
            fn bind_agent_gateway(
                &self,
                _: &ProjectAutomationAgentGateway,
                _: Option<AlwaysOnBindHooks<'_>>,
            ) -> io::Result<()> {
                Ok(())
            }
            fn start(self: Arc<Self>) -> AutomationFuture {
                Box::pin(async move {
                    push(&self.log, "first.start".into());
                    Ok(())
                })
            }
            fn stop(self: Arc<Self>) -> AutomationFuture {
                Box::pin(async move {
                    let release = self.release.lock().unwrap().take();
                    self.entered.notify_one();
                    if let Some(release) = release {
                        release.await.map_err(|e| e.to_string())?;
                    }
                    push(&self.log, "first.stop".into());
                    Ok(())
                })
            }
            fn tools(&self) -> Vec<JsString> {
                vec![]
            }
        }
        let log: Arc<Mutex<Vec<String>>> = Arc::default();
        let entered = Arc::new(tokio::sync::Notify::new());
        let (release, receiver) = tokio::sync::oneshot::channel();
        let first = Arc::new(PausedStop {
            log: log.clone(),
            entered: entered.clone(),
            release: Mutex::new(Some(receiver)),
        });
        let config = ProjectAutomationConfig {
            always_on: None,
            cron: empty_cron_section(),
        };
        let (bundle, opts) = make_bundle(
            log.clone(),
            config.clone(),
            vec![],
            vec![first, fake("second", &log), fake("third", &log)],
        );
        let published = attach_bundle(&bundle, false);
        let lifetime =
            super::super::automation_lifetime::NativeAutomationLifetime::new(Arc::new(bundle));
        lifetime
            .bind_runtime(tokio::runtime::Handle::current())
            .unwrap();
        lifetime.start().await.unwrap();
        let reload = || ProjectAutomationReloadInput {
            config: config.clone(),
            always_on_changed: false,
            cron_changed: true,
        };
        lifetime.request_reload(reload());
        tokio::time::timeout(std::time::Duration::from_secs(2), entered.notified())
            .await
            .unwrap();
        lifetime.request_reload(reload());
        // The second requested generation cannot bind/start through the first
        // generation's still-owned stop, even though both callers returned.
        assert!(!actual_log(&log).iter().any(|line| line == "second.start"));
        release.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while published.lock().unwrap().len() < 3 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        lifetime.stop().await.unwrap();
        lifetime.stop().await.unwrap();
        lifetime.request_reload(reload());
        tokio::task::yield_now().await;
        assert!(lifetime.shutdown.is_cancelled());
        assert_eq!(opts.lock().unwrap().cron_count, 3);
        let events = actual_log(&log);
        let lifecycle: Vec<_> = events
            .iter()
            .filter(|line| line.ends_with(".start") || line.ends_with(".stop"))
            .map(String::as_str)
            .collect();
        assert_eq!(
            lifecycle,
            [
                "first.start",
                "first.stop",
                "second.start",
                "second.stop",
                "third.start",
                "third.stop"
            ]
        );
        assert!(published.lock().unwrap().last().unwrap().cron.is_none());
    }

    fn disabled_ao_config() -> AlwaysOnConfig {
        let mut config = AlwaysOnConfig::default_config();
        config.projects = vec![("/p".into(), false)];
        config
    }

    fn empty_cron_section() -> Option<JsValue> {
        Some(JsValue::Object(pilotdeck_protocol::JsObject::new()))
    }

    /// The attach the probe performs; `published` records every
    /// `updateSubsystems` call for the publish assertions.
    fn attach_bundle(
        bundle: &ProjectAutomationBundle,
        with_busy_hook: bool,
    ) -> Arc<Mutex<Vec<SubsystemUpdate>>> {
        let published: Arc<Mutex<Vec<SubsystemUpdate>>> = Arc::default();
        let sink = published.clone();
        bundle
            .attach(ProjectAutomationAttachment {
                agent_gateway: ProjectAutomationAgentGateway {
                    always_on: Arc::new(DummyAlwaysOnGateway),
                    cron: Arc::new(DummyCronGateway),
                    native_cron_bind: None,
                },
                is_project_busy: with_busy_hook.then(|| Arc::new(|_: &str| false) as IsProjectBusy),
                update_subsystems: Arc::new(move |update| {
                    sink.lock().unwrap_or_else(|e| e.into_inner()).push(update);
                }),
            })
            .expect("#2294 attach");
        published
    }

    fn error_json(error: &AutomationError) -> serde_json::Value {
        if error.errors.is_empty() {
            serde_json::json!({"name": error.name, "message": error.message})
        } else {
            serde_json::json!({"name": error.name, "message": error.message, "errors": error.errors})
        }
    }

    fn opts_json(opts: &Opts) -> serde_json::Value {
        // The TS `opts` also carries `fallbackCount`; the port has no
        // fallback factory at all (module docs), so the field is asserted
        // against the golden's zero instead of being replayed.
        serde_json::json!({"aoCount": opts.ao_count, "cronCount": opts.cron_count})
    }

    fn assert_golden_opts(golden_opts: &serde_json::Value, opts: &Opts) {
        // The TS `opts` also carries `fallbackCount`; the port has no
        // fallback factory at all (module docs), so that field has no
        // counterpart to compare — the deviation itself is what b9/b11
        // assert directly against the last publish.
        negative_assert_eq!(
            opts_json(opts),
            serde_json::json!({
                "aoCount": golden_opts["aoCount"],
                "cronCount": golden_opts["cronCount"],
            }),
            "#2294 factory counts"
        );
    }

    // b1: `createAlwaysOn` conditions — no config / no enabled project
    // construct nothing; an enabled project constructs exactly one provider
    // (and one cron under a present section).
    #[tokio::test]
    async fn b1_create_conditions() {
        let record = golden("b1-create-conditions");
        for (config, state) in [
            (
                ProjectAutomationConfig {
                    always_on: None,
                    cron: None,
                },
                "noConfig",
            ),
            (
                ProjectAutomationConfig {
                    always_on: Some(disabled_ao_config()),
                    cron: None,
                },
                "noEnabled",
            ),
            (
                ProjectAutomationConfig {
                    always_on: Some(enabled_ao_config()),
                    cron: empty_cron_section(),
                },
                "enabled",
            ),
        ] {
            let (_bundle, opts) = make_bundle(Arc::default(), config, Vec::new(), Vec::new());
            let opts = opts.lock().unwrap_or_else(|e| e.into_inner());
            assert_golden_opts(&record[state], &opts);
        }
    }

    #[test]
    fn enabled_generation_exposes_manager_disabled_generation_does_not() {
        struct ManagerProvider(Arc<AlwaysOnManager>);
        impl ProjectAutomationProvider for ManagerProvider {
            fn bind_agent_gateway(
                &self,
                _: &ProjectAutomationAgentGateway,
                _: Option<AlwaysOnBindHooks<'_>>,
            ) -> Result<(), io::Error> {
                Ok(())
            }
            fn start(self: Arc<Self>) -> AutomationFuture {
                Box::pin(async { Ok(()) })
            }
            fn stop(self: Arc<Self>) -> AutomationFuture {
                Box::pin(async { Ok(()) })
            }
            fn tools(&self) -> Vec<JsString> {
                vec![]
            }
            fn always_on_manager(&self) -> Option<Arc<AlwaysOnManager>> {
                Some(self.0.clone())
            }
        }
        let manager = Arc::new(
            AlwaysOnManager::new(crate::runtime::always_on::AlwaysOnManagerOptions {
                config: AlwaysOnConfig::default_config(),
                pilot_home: PathBuf::from("/tmp/probe-home"),
                now: None,
                uuid: None,
                logger: None,
            })
            .expect("manager"),
        );
        let provider: Arc<dyn ProjectAutomationProvider> =
            Arc::new(ManagerProvider(manager.clone()));
        let (enabled, _) = make_bundle(
            Arc::default(),
            ProjectAutomationConfig {
                always_on: Some(enabled_ao_config()),
                cron: None,
            },
            vec![provider],
            vec![],
        );
        assert!(Arc::ptr_eq(
            &enabled.always_on_manager().expect("enabled manager"),
            &manager
        ));
        let (disabled, _) = make_bundle(
            Arc::default(),
            ProjectAutomationConfig {
                always_on: Some(disabled_ao_config()),
                cron: None,
            },
            vec![],
            vec![],
        );
        assert!(disabled.always_on_manager().is_none());
    }

    // b2: start then stop — the lifecycle order (ao binds with hooks, cron
    // without; ao starts first, cron stops first) and the two publishes
    // (the started generation, then the empty unpublish).
    #[tokio::test]
    async fn b2_start_stop_order() {
        let log: Arc<Mutex<Vec<String>>> = Arc::default();
        let ao = fake("ao", &log);
        let cron = fake("cron", &log);
        let (bundle, _opts) = make_bundle(
            log.clone(),
            ProjectAutomationConfig {
                always_on: Some(enabled_ao_config()),
                cron: empty_cron_section(),
            },
            vec![ao.clone()],
            vec![cron.clone()],
        );
        let published = attach_bundle(&bundle, true);
        let record = golden("b2-start-stop-order");
        let started = bundle.start().await.expect("#2294 start");
        bundle.stop().await.expect("#2294 stop");
        negative_assert_eq!(
            serde_json::json!({"alwaysOn": started.always_on, "cron": started.cron}),
            record["started"],
            "#2294 the started status"
        );
        negative_assert_eq!(
            actual_log(&log),
            expected_log("b2-start-stop-order"),
            "#2294 lifecycle order"
        );
        let published = published.lock().unwrap_or_else(|e| e.into_inner());
        negative_assert_eq!(
            published.len(),
            record["published"]
                .as_array()
                .expect("#2294 published")
                .len(),
            "#2294 start publishes, stop unpublishes"
        );
        for (update, expected) in published.iter().zip(
            record["published"]
                .as_array()
                .expect("#2294 published")
                .iter(),
        ) {
            negative_assert_eq!(
                update
                    .extra_tools
                    .iter()
                    .map(|tool| tool.to_utf8().expect("#2294 tool UTF-8"))
                    .collect::<Vec<_>>(),
                expected["tools"]
                    .as_array()
                    .expect("#2294 tools")
                    .iter()
                    .map(|tool| tool.as_str().expect("#2294 tool").to_string())
                    .collect::<Vec<_>>(),
                "#2294 published tools"
            );
            negative_assert_eq!(
                update
                    .cron
                    .as_ref()
                    .is_some_and(|bound| Arc::ptr_eq(bound, &cron)),
                expected["cronIsProvider"]
                    .as_bool()
                    .expect("#2294 cronIsProvider"),
                "#2294 the published cron is the provider"
            );
            negative_assert_eq!(
                update
                    .always_on_control
                    .as_ref()
                    .is_some_and(|bound| Arc::ptr_eq(bound, &ao)),
                expected["alwaysOnIsProvider"]
                    .as_bool()
                    .expect("#2294 alwaysOnIsProvider"),
                "#2294 the published control is the provider"
            );
        }
    }

    // b3: stop failures — one passes through raw, two aggregate as the
    // cleanup error; the order is unchanged by the failures. The golden
    // keeps one log per scenario under `single` / `both` and the thrown
    // messages under `singleErr` / `bothErr`.
    #[tokio::test]
    async fn b3_stop_failures() {
        let record = golden("b3-stop-failures");
        for (state, error_key, ao_throws, cron_throws) in [
            ("single", "singleErr", Some("ao stop boom"), None),
            (
                "both",
                "bothErr",
                Some("ao stop boom"),
                Some("cron stop boom"),
            ),
        ] {
            let log: Arc<Mutex<Vec<String>>> = Arc::default();
            let (bundle, _opts) = make_bundle(
                log.clone(),
                ProjectAutomationConfig {
                    always_on: Some(enabled_ao_config()),
                    cron: empty_cron_section(),
                },
                vec![provider("ao", &log, None, ao_throws)],
                vec![provider("cron", &log, None, cron_throws)],
            );
            attach_bundle(&bundle, false);
            bundle.start().await.expect("#2294 start");
            let error = bundle
                .stop()
                .await
                .expect_err("#2294 the stop failure crosses");
            negative_assert_eq!(
                error_json(&error),
                record[error_key],
                "#2294 stop failure shape"
            );
            negative_assert_eq!(
                actual_log(&log),
                lines_of(&record[state]),
                "#2294 stop order is unchanged by the failures"
            );
        }
    }

    // b4: a start failure tears the generation back down (the ao stop in
    // the log) and crosses as the startup AggregateError.
    #[tokio::test]
    async fn b4_start_failure() {
        let log: Arc<Mutex<Vec<String>>> = Arc::default();
        let (bundle, _opts) = make_bundle(
            log.clone(),
            ProjectAutomationConfig {
                always_on: Some(enabled_ao_config()),
                cron: None,
            },
            vec![provider("ao", &log, Some("ao start boom"), None)],
            Vec::new(),
        );
        attach_bundle(&bundle, false);
        let error = bundle
            .start()
            .await
            .expect_err("#2294 the start failure crosses");
        negative_assert_eq!(
            error_json(&error),
            golden("b4-start-failure")["err"],
            "#2294 startup failure shape"
        );
        negative_assert_eq!(
            actual_log(&log),
            expected_log("b4-start-failure"),
            "#2294 the failed generation is torn down"
        );
    }

    // #2486 测试 A（成功形）: the assembly's synchronous builder drives
    // `start` with one `now_or_never` poll inside `runtime.block_on`, so a
    // successful start chain must complete inside that single poll — no
    // await point on the success path may depend on a runtime pass.
    #[tokio::test(flavor = "current_thread")]
    async fn a_successful_start_completes_under_the_builders_single_poll() {
        use futures_util::FutureExt;
        let log: Arc<Mutex<Vec<String>>> = Arc::default();
        let (bundle, _opts) = make_bundle(
            log.clone(),
            ProjectAutomationConfig {
                always_on: Some(enabled_ao_config()),
                cron: empty_cron_section(),
            },
            vec![TickerProvider::new()],
            vec![fake("cron", &log)],
        );
        attach_bundle(&bundle, true);
        let started = std::pin::pin!(bundle.start())
            .now_or_never()
            .expect("#2486 a successful start never yields")
            .expect("#2486 start");
        negative_assert_eq!(started.always_on, true, "#2486 the ticker provider started");
        negative_assert_eq!(started.cron, true, "#2486 the cron provider started");
        bundle.stop().await.expect("#2486 stop");
    }

    // #2486 测试 A（失败形）: a failing start's cleanup stops the always-on
    // half by joining its ticker, which yields — the single poll reads
    // `None` and drops the whole start chain with the real cron failure
    // still inside it. The compensating stop the host teardown performs
    // must still drain the half-started generation and leave no ticker
    // behind (before #2486, `stop` early-returned on `!started` and the
    // generation stayed live).
    #[tokio::test(flavor = "current_thread")]
    async fn a_yielding_start_cleanup_is_drained_by_the_compensating_stop() {
        use futures_util::FutureExt;
        let log: Arc<Mutex<Vec<String>>> = Arc::default();
        let ticker = TickerProvider::new();
        let (bundle, _opts) = make_bundle(
            log.clone(),
            ProjectAutomationConfig {
                always_on: Some(enabled_ao_config()),
                cron: empty_cron_section(),
            },
            vec![ticker.clone()],
            vec![provider("cron", &log, Some("cron start boom"), None)],
        );
        attach_bundle(&bundle, false);
        negative_assert_eq!(
            std::pin::pin!(bundle.start()).now_or_never().is_none(),
            true,
            "#2486 the failure cleanup's ticker join yields under the single poll"
        );
        bundle
            .stop()
            .await
            .expect("#2486 the compensating stop drains the half-started generation");
        let log = actual_log(&log);
        negative_assert_eq!(
            log.iter().any(|line| line == "cron.start"),
            true,
            "#2486 the cron start was attempted"
        );
        negative_assert_eq!(
            log.iter().any(|line| line == "cron.stop"),
            true,
            "#2486 the compensating stop tore the cron generation down"
        );
        // The dropped first stop detached the ticker with the stop flag set
        // and a wake permit stored; one scheduler pass retires it.
        tokio::task::yield_now().await;
        negative_assert_eq!(
            ticker.finished(),
            true,
            "#2486 the ticker observed the stop and returned"
        );
    }

    // #2486 测试 B（awaited 形）: driven to completion instead of dropped,
    // the same failing start crosses the real provider failure in the
    // combined error's Display — the yield must not be able to swallow it.
    #[tokio::test]
    async fn an_awaited_start_failure_carries_the_cron_error() {
        let log: Arc<Mutex<Vec<String>>> = Arc::default();
        let (bundle, _opts) = make_bundle(
            log.clone(),
            ProjectAutomationConfig {
                always_on: Some(enabled_ao_config()),
                cron: empty_cron_section(),
            },
            vec![TickerProvider::new()],
            vec![provider("cron", &log, Some("cron start boom"), None)],
        );
        attach_bundle(&bundle, false);
        let error = bundle
            .start()
            .await
            .expect_err("#2486 the start failure crosses");
        negative_assert_eq!(
            error.to_string().contains("cron start boom"),
            true,
            "#2486 the real cron failure surfaces in the crossing error"
        );
    }

    // b5: a reload with nothing changed touches no provider.
    #[tokio::test]
    async fn b5_reload_no_change() {
        let log: Arc<Mutex<Vec<String>>> = Arc::default();
        let (bundle, opts) = make_bundle(
            log.clone(),
            ProjectAutomationConfig {
                always_on: Some(enabled_ao_config()),
                cron: empty_cron_section(),
            },
            vec![fake("ao", &log)],
            vec![fake("cron", &log)],
        );
        attach_bundle(&bundle, false);
        bundle.start().await.expect("#2294 start");
        let log_before = actual_log(&log).len();
        let opts_before = {
            let opts = opts.lock().unwrap_or_else(|e| e.into_inner());
            opts_json(&opts)
        };
        let status = bundle
            .reload(ProjectAutomationReloadInput {
                config: ProjectAutomationConfig {
                    always_on: Some(enabled_ao_config()),
                    cron: empty_cron_section(),
                },
                always_on_changed: false,
                cron_changed: false,
            })
            .await
            .expect("#2294 no-change reload");
        let record = golden("b5-reload-no-change");
        negative_assert_eq!(
            serde_json::json!({"alwaysOn": status.always_on, "cron": status.cron}),
            record["status"],
            "#2294 no-change reload status"
        );
        negative_assert_eq!(
            opts_before,
            serde_json::json!({
                "aoCount": record["optsBefore"]["aoCount"],
                "cronCount": record["optsBefore"]["cronCount"],
            }),
            "#2294 opts before"
        );
        negative_assert_eq!(
            opts_json(&opts.lock().unwrap_or_else(|e| e.into_inner())),
            serde_json::json!({
                "aoCount": record["optsAfter"]["aoCount"],
                "cronCount": record["optsAfter"]["cronCount"],
            }),
            "#2294 opts after"
        );
        negative_assert_eq!(
            actual_log(&log).len() - log_before,
            record["logAdded"].as_array().expect("#2294 logAdded").len(),
            "#2294 a no-change reload adds no lifecycle calls"
        );
    }

    // b6: a cron-only reload reuses `previous.alwaysOn` — the last publish's
    // control is the first generation's provider.
    #[tokio::test]
    async fn b6_reload_cron_only() {
        let log: Arc<Mutex<Vec<String>>> = Arc::default();
        let ao = fake("ao", &log);
        let (bundle, opts) = make_bundle(
            log.clone(),
            ProjectAutomationConfig {
                always_on: Some(enabled_ao_config()),
                cron: empty_cron_section(),
            },
            vec![ao.clone()],
            vec![fake("cron#1", &log), fake("cron#2", &log)],
        );
        let published = attach_bundle(&bundle, false);
        bundle.start().await.expect("#2294 start");
        let status = bundle
            .reload(ProjectAutomationReloadInput {
                config: ProjectAutomationConfig {
                    always_on: Some(enabled_ao_config()),
                    cron: Some(crate::metadata::object([("enabled", JsValue::Bool(true))])),
                },
                always_on_changed: false,
                cron_changed: true,
            })
            .await
            .expect("#2294 cron-only reload");
        let record = golden("b6-reload-cron-only");
        negative_assert_eq!(
            serde_json::json!({"alwaysOn": status.always_on, "cron": status.cron}),
            record["status"],
            "#2294 cron-only reload status"
        );
        assert_golden_opts(
            &record["opts"],
            &opts.lock().unwrap_or_else(|e| e.into_inner()),
        );
        negative_assert_eq!(
            actual_log(&log),
            expected_log("b6-reload-cron-only"),
            "#2294 order"
        );
        let published = published.lock().unwrap_or_else(|e| e.into_inner());
        let last = published.last().expect("#2294 final publish");
        negative_assert_eq!(
            last.always_on_control
                .as_ref()
                .is_some_and(|bound| Arc::ptr_eq(bound, &ao)),
            record["lastPublishAlwaysOnIsPrevious"]
                .as_bool()
                .expect("#2294 pin"),
            "#2294 an unchanged family reuses the previous provider"
        );
    }

    // b7: a reload whose previous generation cannot stop refuses the
    // replacement — nothing further starts.
    #[tokio::test]
    async fn b7_reload_stop_failure() {
        let log: Arc<Mutex<Vec<String>>> = Arc::default();
        let (bundle, _opts) = make_bundle(
            log.clone(),
            ProjectAutomationConfig {
                always_on: Some(enabled_ao_config()),
                cron: None,
            },
            vec![provider("ao", &log, None, Some("ao stop boom"))],
            vec![fake("cron", &log)],
        );
        attach_bundle(&bundle, false);
        bundle.start().await.expect("#2294 start");
        let error = bundle
            .reload(ProjectAutomationReloadInput {
                config: ProjectAutomationConfig {
                    always_on: Some(enabled_ao_config()),
                    cron: None,
                },
                always_on_changed: true,
                cron_changed: true,
            })
            .await
            .expect_err("#2294 the stop failure refuses the reload");
        negative_assert_eq!(
            error_json(&error),
            golden("b7-reload-stop-failure")["err"],
            "#2294 reload stop-failure shape"
        );
        negative_assert_eq!(
            actual_log(&log),
            expected_log("b7-reload-stop-failure"),
            "#2294 no replacement was started"
        );
    }

    // b8: a candidate start failure restores the previous generation —
    // a third factory run, and the restored provider starts.
    #[tokio::test]
    async fn b8_reload_restore() {
        let log: Arc<Mutex<Vec<String>>> = Arc::default();
        let (bundle, opts) = make_bundle(
            log.clone(),
            ProjectAutomationConfig {
                always_on: Some(enabled_ao_config()),
                cron: None,
            },
            vec![
                fake("ao#1", &log),
                provider("ao#2", &log, Some("ao#2 start boom"), None),
                fake("ao#3", &log),
            ],
            vec![fake("cron#1", &log)],
        );
        attach_bundle(&bundle, false);
        bundle.start().await.expect("#2294 start");
        let error = bundle
            .reload(ProjectAutomationReloadInput {
                config: ProjectAutomationConfig {
                    always_on: Some(enabled_ao_config()),
                    cron: None,
                },
                always_on_changed: true,
                cron_changed: true,
            })
            .await
            .expect_err("#2294 the candidate failure crosses");
        let record = golden("b8-reload-restore");
        negative_assert_eq!(error_json(&error), record["err"], "#2294 restore shape");
        assert_golden_opts(
            &record["opts"],
            &opts.lock().unwrap_or_else(|e| e.into_inner()),
        );
        negative_assert_eq!(
            actual_log(&log),
            expected_log("b8-reload-restore"),
            "#2294 restore order"
        );
    }

    // b9: a restore failure publishes a provider-less generation — the
    // port's published control is `None` where TS publishes the fallback
    // (the module-docs deviation, asserted here), and a following no-change
    // reload reports the empty status.
    #[tokio::test]
    async fn b9_reload_restore_failure() {
        let log: Arc<Mutex<Vec<String>>> = Arc::default();
        let (bundle, opts) = make_bundle(
            log.clone(),
            ProjectAutomationConfig {
                always_on: Some(enabled_ao_config()),
                cron: None,
            },
            vec![
                fake("ao#1", &log),
                provider("ao#2", &log, Some("ao#2 start boom"), None),
                provider("ao#3", &log, Some("ao#3 start boom"), None),
            ],
            vec![
                fake("cron#1", &log),
                provider("cron#2", &log, Some("cron#2 start boom"), None),
                provider("cron#3", &log, Some("cron#3 start boom"), None),
            ],
        );
        let published = attach_bundle(&bundle, false);
        bundle.start().await.expect("#2294 start");
        let error = bundle
            .reload(ProjectAutomationReloadInput {
                config: ProjectAutomationConfig {
                    always_on: Some(enabled_ao_config()),
                    cron: None,
                },
                always_on_changed: true,
                cron_changed: true,
            })
            .await
            .expect_err("#2294 the restore failure crosses");
        let record = golden("b9-reload-restore-failure");
        negative_assert_eq!(
            error_json(&error),
            record["err"],
            "#2294 restore-failure shape"
        );
        assert_golden_opts(
            &record["opts"],
            &opts.lock().unwrap_or_else(|e| e.into_inner()),
        );
        negative_assert_eq!(
            actual_log(&log),
            expected_log("b9-reload-restore-failure"),
            "#2294 restore-failure order"
        );
        let last = published
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .last()
            .expect("#2294 final publish")
            .always_on_control
            .clone();
        negative_assert_eq!(
            last.is_none(),
            true,
            "#2294 deviation: the port publishes no control where TS publishes the standalone fallback"
        );
        let status = bundle
            .reload(ProjectAutomationReloadInput {
                config: ProjectAutomationConfig {
                    always_on: None,
                    cron: None,
                },
                always_on_changed: false,
                cron_changed: false,
            })
            .await
            .expect("#2294 no-change reload");
        negative_assert_eq!(
            serde_json::json!({"alwaysOn": status.always_on, "cron": status.cron}),
            record["statusAfter"],
            "#2294 the provider-less generation's status"
        );
    }

    // b10: a reload before start only stages — no provider lifecycle call.
    #[tokio::test]
    async fn b10_reload_before_start() {
        let log: Arc<Mutex<Vec<String>>> = Arc::default();
        let (bundle, opts) = make_bundle(
            log.clone(),
            ProjectAutomationConfig {
                always_on: Some(enabled_ao_config()),
                cron: None,
            },
            vec![fake("ao#1", &log), fake("ao#2", &log)],
            vec![fake("cron#1", &log), fake("cron#2", &log)],
        );
        attach_bundle(&bundle, false);
        let status = bundle
            .reload(ProjectAutomationReloadInput {
                config: ProjectAutomationConfig {
                    always_on: Some(enabled_ao_config()),
                    cron: None,
                },
                always_on_changed: true,
                cron_changed: false,
            })
            .await
            .expect("#2294 pre-start reload");
        let record = golden("b10-reload-before-start");
        negative_assert_eq!(
            serde_json::json!({"alwaysOn": status.always_on, "cron": status.cron}),
            record["status"],
            "#2294 pre-start reload status"
        );
        assert_golden_opts(
            &record["opts"],
            &opts.lock().unwrap_or_else(|e| e.into_inner()),
        );
        negative_assert_eq!(
            actual_log(&log),
            expected_log("b10-reload-before-start"),
            "#2294 staging only — no start or stop"
        );
    }

    // b11: without an Always-On provider the published control is the TS
    // standalone fallback — the deviation this port documents: `None`, so
    // the three wire methods keep the #1313 not_configured envelopes.
    #[tokio::test]
    async fn b11_fallback_publish() {
        let log: Arc<Mutex<Vec<String>>> = Arc::default();
        let (bundle, opts) = make_bundle(
            log.clone(),
            ProjectAutomationConfig {
                always_on: None,
                cron: empty_cron_section(),
            },
            Vec::new(),
            vec![fake("cron", &log)],
        );
        let published = attach_bundle(&bundle, false);
        bundle.start().await.expect("#2294 start");
        let status = bundle
            .reload(ProjectAutomationReloadInput {
                config: ProjectAutomationConfig {
                    always_on: None,
                    cron: empty_cron_section(),
                },
                always_on_changed: false,
                cron_changed: false,
            })
            .await
            .expect("#2294 no-change reload");
        let record = golden("b11-fallback-publish");
        negative_assert_eq!(
            serde_json::json!({"alwaysOn": status.always_on, "cron": status.cron}),
            record["status"],
            "#2294 fallback status"
        );
        assert_golden_opts(
            &record["opts"],
            &opts.lock().unwrap_or_else(|e| e.into_inner()),
        );
        negative_assert_eq!(
            published
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .last()
                .expect("#2294 final publish")
                .always_on_control
                .is_none(),
            true,
            "#2294 deviation: no standalone fallback exists; the control is None, not kind 'standalone-fallback'"
        );
        negative_assert_eq!(
            golden("b11-fallback-publish")["alwaysOnControl"]["kind"].as_str(),
            Some("standalone-fallback"),
            "#2294 the golden still pins the TS side of the deviation"
        );
        negative_assert_eq!(
            actual_log(&log),
            expected_log("b11-fallback-publish"),
            "#2294 fallback-publish order"
        );
    }
}
