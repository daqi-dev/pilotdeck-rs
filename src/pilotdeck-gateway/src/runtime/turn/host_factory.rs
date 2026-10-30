//! Per-turn host assembly from a real project lease, then the stdio sidecar.
//!
//! This is the Rust landing point for `AgentLoopDeploymentProfile.ts:20-110`,
//! `AgentSessionRuntimeBundle.ts:242-290`, `sidecarHostModulePorts.ts:240-322`
//! and `stdioAgentLoopSidecarConnection.ts:31-60`: the deployment selects one
//! stdio sidecar, every actual turn acquires the current project generation,
//! builds a fresh `ModuleRouter` from that lease, and only then spawns the
//! child. The child never supplies model, budget, context or capability ports.
//!
//! `capability` and `turn` are session-owned in TS (`dependencies.tools`,
//! session steer/persistence). They arrive through [`SessionPorts`]; model,
//! budget and context are rebuilt from the lease for every turn. No handler is
//! reused across turns or generations: `model_port::compose`, the lease's
//! token accounting and `ContextStorageResources::create_session` all return
//! turn-owned state.
//!
//! The lease is acquired during assembly; the returned connection is lazy.
//! LiveSession binds the handler and commits start/accepted input before polling
//! that connection. Failed gates therefore create no child; cancellation/drop
//! still retains the lease until any outstanding recorder write exits.
use crate::{
    config::Env,
    host::{
        HostClient, HostError, HostErrorKind, Limits,
        modules::{ModuleRouter, budget, capability, context, model::TurnContext, model_port},
    },
    runtime::{
        project::{
            ProjectResources, Resource, RuntimeLease,
            context_storage::ContextStorageResources,
            model::{RouterResources, TokenAccountingResources},
        },
        turn::{
            interaction::InteractionTurnInput,
            live::{InteractionHost, LiveSessionAdapter},
        },
    },
    storage::recorder::Recorder,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, admission::ExecutionIdentity};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::process::Command;

/// `AgentLoopDeploymentProfile` (TS lines 11-38). TCP is parsed for exact
/// error/selection parity, but the native host factory has no TCP transport.
#[derive(Clone, Debug, PartialEq)]
pub enum Deployment {
    Native,
    Stdio {
        command: String,
        args: Vec<String>,
        cwd: PathBuf,
        env: Env,
    },
    Tcp {
        host: String,
        port: u16,
        connect_timeout_ms: Option<u64>,
    },
}

/// `resolveAgentLoopDeploymentProfile` (`AgentLoopDeploymentProfile.ts:44-81`).
/// Error text is byte-for-byte the TS baseline; the tcp selection is kept so
/// `create` can reject it explicitly (#130) instead of inventing a fallback.
pub fn resolve_deployment(env: &Env, cwd: &Path) -> Result<Deployment, String> {
    let transport = env
        .get("PILOTDECK_AGENT_LOOP_TRANSPORT")
        .map(|value| value.trim().to_lowercase())
        .unwrap_or_default();
    match transport.as_str() {
        "" | "native" => Ok(Deployment::Native),
        "tcp" => Ok(Deployment::Tcp {
            host: required_text(
                env.get("PILOTDECK_AGENT_LOOP_TCP_HOST").map(String::as_str),
                "127.0.0.1",
                "PILOTDECK_AGENT_LOOP_TCP_HOST",
            )?,
            port: required_port(env.get("PILOTDECK_AGENT_LOOP_TCP_PORT").map(String::as_str))?,
            connect_timeout_ms: optional_positive_integer(
                env.get("PILOTDECK_AGENT_LOOP_CONNECT_TIMEOUT_MS")
                    .map(String::as_str),
            )?,
        }),
        "stdio" => {
            let command = required_text(
                env.get("PILOTDECK_AGENT_LOOP_SIDECAR_COMMAND")
                    .map(String::as_str),
                &std::env::current_exe()
                    .map(|path| path.to_string_lossy().into_owned())
                    .unwrap_or_else(|_| "pilotdeck".into()),
                "PILOTDECK_AGENT_LOOP_SIDECAR_COMMAND",
            )?;
            let sidecar_path = match env
                .get("PILOTDECK_AGENT_LOOP_SIDECAR_PATH")
                .map(String::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                Some(value) => crate::metadata::resolve(cwd, value),
                // TS resolves `new URL("./pilotdeck-agent-loop-sidecar.js",
                // import.meta.url)`. The native sibling is the sidecar binary
                // next to the gateway executable.
                None => std::env::current_exe()
                    .ok()
                    .and_then(|path| path.parent().map(Path::to_path_buf))
                    .unwrap_or_else(|| cwd.to_path_buf())
                    .join("pilotdeck-agent-loop-sidecar.js"),
            };
            // A stdio child is per-turn; it must not inherit the parent's TCP
            // deployment config (`AgentLoopDeploymentProfile.ts:70-73`).
            let mut child_env = env.clone();
            child_env.remove("PILOTDECK_AGENT_LOOP_TCP_HOST");
            child_env.remove("PILOTDECK_AGENT_LOOP_TCP_PORT");
            Ok(Deployment::Stdio {
                command,
                args: vec![sidecar_path.to_string_lossy().into_owned()],
                cwd: cwd.to_path_buf(),
                env: child_env,
            })
        }
        _ => Err("PILOTDECK_AGENT_LOOP_TRANSPORT must be native, stdio, or tcp.".into()),
    }
}

fn required_text(value: Option<&str>, default: &str, name: &str) -> Result<String, String> {
    let value = value.unwrap_or(default).trim();
    if value.is_empty() {
        return Err(format!("{name} must not be empty."));
    }
    Ok(value.to_owned())
}

/// JS `Number(value)` restricted to the integer forms the deployment profile
/// consumes. TS parses both numbers with `Number()` + `Number.isInteger`
/// (`AgentLoopDeploymentProfile.ts:127-141`), so `+80`, `80.0`, `1e3` and
/// `0x50` are accepted while `+0x50`, `0x+50` and `80.5` are not.
///
/// The grammar (JS whitespace, radix-prefix recognition and the digit gate that
/// rejects `0x+50`) lives in [`crate::js_value::string_to_number`]; keeping a
/// second copy here is what let the two drift apart before #744 fix-r2. Only
/// `Number.isInteger` remains: finite and without a fractional part.
fn js_integral_number(value: &str) -> Option<f64> {
    let number = crate::js_value::string_to_number(value);
    (number.is_finite() && number.fract() == 0.0).then_some(number)
}

fn required_port(value: Option<&str>) -> Result<u16, String> {
    let Some(value) = value.filter(|value| !value.trim().is_empty()) else {
        return Err("PILOTDECK_AGENT_LOOP_TCP_PORT is required when transport is tcp.".into());
    };
    match js_integral_number(value) {
        Some(port) if (1.0..=65_535.0).contains(&port) => Ok(port as u16),
        _ => Err("PILOTDECK_AGENT_LOOP_TCP_PORT must be an integer between 1 and 65535.".into()),
    }
}

fn optional_positive_integer(value: Option<&str>) -> Result<Option<u64>, String> {
    let Some(value) = value.filter(|value| !value.trim().is_empty()) else {
        return Ok(None);
    };
    match js_integral_number(value) {
        // TS accepts any positive integer `number`; the deployment field is a
        // `u64` millisecond count, so values above its range stay rejected.
        Some(number) if number > 0.0 && number <= u64::MAX as f64 => Ok(Some(number as u64)),
        _ => Err("PILOTDECK_AGENT_LOOP_CONNECT_TIMEOUT_MS must be a positive integer.".into()),
    }
}

/// Session-owned ports that are not rebuilt from the project generation.
/// Every field except `model`/`recorder` is turn state and must be fresh.
pub struct SessionPorts {
    /// `options` for the router model adapter (`RouterModelPortOptions`).
    pub model: crate::host::modules::model_port::RouterModelPortOptions,
    /// Durable model recorder; `None` keeps the model port non-durable.
    pub recorder: Option<crate::host::modules::model_port::DurableRecorder>,
    /// Session tool-results directory (`ProjectSessionRuntimeBundle.ts:520`).
    pub tool_results_dir: PathBuf,
    /// A caller-supplied context runtime; production leaves this `None` and the
    /// lease's `ContextStorageResources` composes the session runtime.
    pub context_runtime: Option<Arc<dyn context::ContextRuntime>>,
    /// `modules.context.requestIdentity`; turn-bound by the session.
    pub context_identity: crate::host::modules::tool_context::ContextRequestIdentity,
    pub context_config: JsObject,
    pub tool_execution: Arc<dyn capability::ToolExecution>,
    pub tool_context: Arc<dyn capability::ToolContext>,
    pub tool_observer: Arc<dyn capability::CapabilityResultObserver>,
    /// `modules.planTodo` resolved handle for this session.
    pub plan_todo: Option<Arc<dyn capability::PlanTodoHandler>>,
    /// Session steer/persistence callbacks (`sidecarTurnComposition.ts:83-87`).
    pub turn: crate::host::modules::turn::Ports,
    /// Trusted per-turn interaction input; `InteractionTurn::bind` still
    /// replaces the profile and the four interaction ports from the lease.
    pub interaction: InteractionTurnInput,
}

type PortsFactory = dyn Fn(&ExecutionIdentity, Arc<Recorder>, crate::runtime::hooks::Cancellation) -> SessionPorts
    + Send
    + Sync;

pub struct TurnHostFactory {
    registry: Arc<crate::runtime::project::ProjectRuntimeRegistry>,
    deployment: Deployment,
    limits: Limits,
    expected_module_id: Option<JsString>,
    ports: Arc<PortsFactory>,
    agent_config: Option<JsObject>,
}

impl TurnHostFactory {
    pub fn new(
        registry: Arc<crate::runtime::project::ProjectRuntimeRegistry>,
        deployment: Deployment,
        limits: Limits,
        expected_module_id: Option<JsString>,
        ports: impl Fn(&ExecutionIdentity, Arc<Recorder>) -> SessionPorts + Send + Sync + 'static,
    ) -> Self {
        Self::new_with_signal(
            registry,
            deployment,
            limits,
            expected_module_id,
            move |identity, recorder, _signal| ports(identity, recorder),
        )
    }

    /// Production entry for ports sharing the LiveSession cancellation source.
    pub fn new_with_signal(
        registry: Arc<crate::runtime::project::ProjectRuntimeRegistry>,
        deployment: Deployment,
        limits: Limits,
        expected_module_id: Option<JsString>,
        ports: impl Fn(
            &ExecutionIdentity,
            Arc<Recorder>,
            crate::runtime::hooks::Cancellation,
        ) -> SessionPorts
        + Send
        + Sync
        + 'static,
    ) -> Self {
        Self {
            registry,
            deployment,
            limits,
            expected_module_id,
            ports: Arc::new(ports),
            agent_config: None,
        }
    }

    /// Bind the session's explicit serialized AgentRuntimeConfig.
    pub fn with_agent_config(mut self, config: JsObject) -> Self {
        self.agent_config = Some(config);
        self
    }

    /// Build the production `LiveSession` over this factory. The registry must
    /// outlive the session and be disposed by its owner after the session.
    /// The ports factory receives the same recorder on every turn; use it for
    /// any durable model/tool ports. `history` reads the current session view.
    pub fn into_live_session(
        mut self,
        recorder: Arc<Recorder>,
        history: impl Fn() -> Vec<JsValue> + Send + Sync + 'static,
    ) -> LiveSessionAdapter {
        let ports = self.ports.clone();
        self.ports = Arc::new(move |identity, recorder, signal| {
            let mut ports = ports(identity, recorder.clone(), signal);
            ports.recorder = Some(model_port::DurableRecorder {
                recorder,
                recording_session_id: ports.recorder.and_then(|r| r.recording_session_id),
            });
            ports
        });
        let close_timeout = self.limits.close_timeout;
        let agent_config = self.agent_config.clone();
        let factory = Arc::new(self);
        let session = LiveSessionAdapter::with_interaction_signal(
            recorder,
            history,
            move |identity, recorder, signal| {
                let factory = factory.clone();
                Box::pin(async move {
                    factory
                        .create_with_signal(&identity, recorder, signal)
                        .await
                })
            },
        )
        .with_close_timeout(if crate::mutants::on("gates-factory-close-timeout") {
            Limits::default().close_timeout
        } else {
            close_timeout
        });
        match agent_config {
            Some(config) => session.with_agent_config(config),
            None => session,
        }
    }

    /// Acquire one generation lease and prepare a lazy sidecar connection. Unsupported
    /// transports fail before any lease exists, so they cannot leak.
    pub async fn create(
        &self,
        identity: &ExecutionIdentity,
        recorder: Arc<Recorder>,
    ) -> Result<InteractionHost, HostError> {
        self.create_with_signal(
            identity,
            recorder,
            crate::runtime::hooks::Cancellation::new(),
        )
        .await
    }

    pub async fn create_with_signal(
        &self,
        identity: &ExecutionIdentity,
        recorder: Arc<Recorder>,
        signal: crate::runtime::hooks::Cancellation,
    ) -> Result<InteractionHost, HostError> {
        let (command, args, cwd, env) = match &self.deployment {
            Deployment::Stdio {
                command,
                args,
                cwd,
                env,
            } => (command, args, cwd, env),
            Deployment::Native => {
                return Err(HostError::new(
                    HostErrorKind::Protocol,
                    "Agent loop transport 'native' has no sidecar host factory.",
                ));
            }
            Deployment::Tcp { .. } => {
                return Err(HostError::new(
                    HostErrorKind::Protocol,
                    "Agent loop transport 'tcp' is not supported; see #130.",
                ));
            }
        };
        let lease = self
            .registry
            .acquire(None)
            .map_err(|error| HostError::new(HostErrorKind::Protocol, error.message))?;
        let ports = (self.ports)(identity, recorder, signal);
        let (router, input, lease) = match Self::assemble(lease, identity, ports).await {
            Ok(value) => value,
            // `assemble` owns (and therefore drops) the lease on error.
            Err(error) => return Err(error),
        };
        let mut command = Command::new(command);
        command.args(args).current_dir(cwd).env_clear().envs(env);
        let expected = self.expected_module_id.clone();
        let limits = self.limits;
        Ok(InteractionHost {
            client: Box::pin(async move {
                HostClient::spawn(&mut command, expected.as_ref(), limits).await
            }),
            lease,
            router,
            input,
        })
    }

    /// Build the non-interaction handlers from the leased generation. Public
    /// to the crate so the manifest test can bind without spawning a child.
    pub(crate) async fn assemble(
        lease: RuntimeLease,
        identity: &ExecutionIdentity,
        ports: SessionPorts,
    ) -> Result<(ModuleRouter, InteractionTurnInput, RuntimeLease), HostError> {
        let SessionPorts {
            model,
            recorder,
            tool_results_dir,
            context_runtime,
            context_identity,
            context_config,
            tool_execution,
            tool_context,
            tool_observer,
            plan_todo,
            turn,
            mut interaction,
        } = ports;
        let context = lease
            .runtime()
            .resources()
            .get::<ProjectResources>()
            .ok_or_else(|| {
                HostError::new(
                    HostErrorKind::Protocol,
                    "Leased project resources are missing.",
                )
            })?
            .context();
        let signal = interaction.signal.clone();
        let turn_context = TurnContext {
            session_id: identity.session_id.clone(),
            turn_id: identity.turn_id.clone(),
            abort: Some(signal.clone()),
            ..Default::default()
        };
        let router = context.resource(Resource::Router).ok_or_else(|| {
            HostError::new(
                HostErrorKind::Protocol,
                "Leased project router resource is missing.",
            )
        })?;
        let router = router
            .as_any()
            .downcast_ref::<RouterResources>()
            .ok_or_else(|| {
                HostError::new(
                    HostErrorKind::Protocol,
                    "Leased project router resource is not a native router family.",
                )
            })?;
        let mut result = ModuleRouter {
            model: model_port::compose(
                router.router.clone(),
                router.model_config.clone(),
                recorder,
                model,
                turn_context,
            ),
            ..Default::default()
        };
        if let Some(resource) = context.resource(Resource::TokenAccounting) {
            let accounting = resource
                .as_any()
                .downcast_ref::<TokenAccountingResources>()
                .ok_or_else(|| {
                    HostError::new(
                        HostErrorKind::Protocol,
                        "Leased token accounting resource is not native.",
                    )
                })?;
            // TS `AgentTurnCapabilities.model.budget.estimateUsageCost` calls
            // `router.estimateUsageCost`, which reads the router's
            // `stats.modelPricing` (`RouterRuntime.ts:1065-1067`,
            // `TokenStatsCollector.ts:90-96,183-190`). The lease's router owns
            // that parsed table, so the budget port must use the same rates.
            let pricing = router.router.config().stats.model_pricing.clone();
            result.budget = budget::Handler {
                ports: Some(budget::Ports::for_accounting(
                    accounting.accounting.clone(),
                    pricing,
                )),
                signal: Some(signal.clone()),
            };
        }
        let context_runtime = match context_runtime {
            Some(runtime) => Some(runtime),
            None => {
                let storage = context
                    .resource(Resource::InstructionStorage)
                    .ok_or_else(|| {
                        HostError::new(
                            HostErrorKind::Protocol,
                            "Leased context storage resource is missing.",
                        )
                    })?;
                let storage = storage
                    .as_any()
                    .downcast_ref::<ContextStorageResources>()
                    .ok_or_else(|| {
                        HostError::new(
                            HostErrorKind::Protocol,
                            "Leased context storage resource is not native.",
                        )
                    })?;
                // A session-composition failure is not a host execution
                // failure: TS's factory rethrows the constructor error from
                // session creation unchanged (`ProjectSessionFactory.ts:160-163`),
                // so this boundary keeps the original message and kind.
                Some(Arc::new(
                    storage
                        .create_session(tool_results_dir)
                        .map_err(|error| {
                            HostError::new(
                                if crate::mutants::on("budget-session-kind") {
                                    HostErrorKind::Protocol
                                } else {
                                    HostErrorKind::Session
                                },
                                error.message,
                            )
                        })?
                        .context_runtime,
                ) as Arc<dyn context::ContextRuntime>)
            }
        };
        if let Some(runtime) = context_runtime {
            result.context = context::Handler::new(
                runtime,
                context_identity,
                context_config,
                Some(signal.clone()),
            );
        }
        let has_plan_todo = plan_todo.is_some();
        let mut capability = capability::Handler::new(tool_execution, tool_context, tool_observer);
        if let Some(plan) = plan_todo {
            capability = capability.with_plan_todo(plan);
        }
        result.capability = capability;
        // `InteractionTurn::bind` revokes stale authorizations and then installs
        // these session-owned callbacks for the current turn.
        interaction.turn_ports = turn;
        // `modules.planTodo` advertisement is independent of the handler
        // (#570), so record the same selection the session composed.
        interaction.switches.plan_todo = has_plan_todo;
        Ok((result, interaction, lease))
    }
}

#[cfg(test)]
#[path = "host_factory_tests.rs"]
mod tests;
