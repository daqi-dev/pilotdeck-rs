//! Default factory composition: authentic bound host + Rust owning turn policy.
//! Missing domain profiles produce explicit failures, never a fake successful loop.
use crate::execute::{FactoryPayloadDecoder, OriginalFactoryPayloadDecoder};
use crate::notifications::InstructionBridge;
use crate::server::{
    ExecuteRequest, ExecutionClock, ExecutionContext, ExecutionFactory, ExecutionTerminal,
};
use crate::{AdvertisedMethods, RemoteHost};
use pilotdeck_agent::ports::*;
use pilotdeck_agent::request::{
    OriginalRequestAssembler, RequestAssembler, RequestAssemblyInput, RequestPreparationObserver,
    RequestPreparationState, RouteTokenCaps,
};
use pilotdeck_agent::session_state::{
    parse_file_state, parse_model_state, snapshot_file_state, snapshot_model_state,
};
use pilotdeck_agent::turn::{
    OutputCapUpdate, TurnClock, TurnEventPort, TurnInput, TurnPreparationPort, TurnRuntimeConfig,
    run_admitted_turn,
};
use pilotdeck_protocol::model::BoundarySnapshot;
use pilotdeck_protocol::module::ModuleOutcome;
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::sync::{Arc, Mutex};

pub struct OriginalAgentFactory {
    pub clock: Arc<dyn ExecutionClock>,
    pub process_cwd: JsString,
}
/// IO composition only: final context capture cannot run while an accepted
/// advisory notice is still uncertain. Domain policy and the host remain owned
/// by their original ports; this wrapper supplies no model or Session state.
struct NotificationFencedHost<H> {
    host: H,
    notifications: Arc<Mutex<InstructionBridge>>,
}
impl<H: HostPort> HostPort for NotificationFencedHost<H> {
    fn advertised(&self, module: &str, operation: &str) -> bool {
        self.host.advertised(module, operation)
    }
    fn call(&mut self, control: &CallControl, call: HostCall) -> PortResult<HostReply> {
        if matches!(&call, HostCall::CaptureTurn(_)) {
            self.notifications
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .finish()?;
        }
        self.host.call(control, call)
    }
}
struct ProductionTurnClock(Arc<dyn ExecutionClock>);
impl TurnClock for ProductionTurnClock {
    fn now_iso(&self) -> JsString {
        self.0.now_iso()
    }
    fn uuid(&self) -> JsString {
        uuid::Uuid::new_v4().to_string().into()
    }
}
struct Preparation {
    state: RequestPreparationState,
    model_state: pilotdeck_agent::session_state::AgentModelSessionState,
    hints: ModelHints,
    methods: AdvertisedMethods,
    notifications: Arc<Mutex<InstructionBridge>>,
}
fn route_key(route: &Route) -> Vec<u16> {
    route
        .provider
        .units()
        .iter()
        .copied()
        .chain(std::iter::once(0))
        .chain(route.model.units().iter().copied())
        .collect()
}
impl Preparation {
    fn output_cap_options(&self, config: &TurnRuntimeConfig, route: &Route) -> (Option<f64>, bool) {
        let key = route_key(route);
        let cap = self
            .state
            .token_caps
            .iter()
            .rev()
            .find(|cap| route_key(&cap.route) == key);
        let baseline = config
            .subagent_model
            .as_ref()
            .filter(|baseline| config.is_subagent == Some(true) && baseline.route == *route);
        let config_active = !(config.is_subagent == Some(true) && config.subagent_model.is_some());
        let requested = cap
            .and_then(|cap| {
                cap.attempt_max_output_tokens
                    .or(cap.requested_max_output_tokens)
                    .or(cap.session_max_output_tokens)
            })
            .or(baseline.and_then(|baseline| baseline.max_output_tokens))
            .or(if config_active {
                config.max_output_tokens
            } else {
                None
            });
        let has_candidate = [requested, cap.and_then(|cap| cap.hard_max_output_tokens)]
            .into_iter()
            .flatten()
            .any(|value| value.is_finite() && value > 0.0);
        let limit = has_candidate.then(|| {
            OriginalRequestAssembler
                .compaction_limits(&self.state, config, route, false)
                .1
        });
        let attempt_override = cap.is_some_and(|cap| {
            cap.attempt_max_output_tokens
                .or(cap.requested_max_output_tokens)
                .is_some()
        });
        (limit, attempt_override)
    }
    fn apply_request_caps(
        &self,
        config: &TurnRuntimeConfig,
        request: &CapturedRequest,
        route: &Route,
    ) -> PortResult<CapturedRequest> {
        let (current_max_output_tokens, attempt_override_present) =
            self.output_cap_options(config, route);
        let boundary = BoundarySnapshot::capture(request.wire.request());
        let capped = pilotdeck_agent::request_materialization::OriginalRequestMaterialization
            .apply_caps(pilotdeck_agent::request_materialization::RequestCapsInput {
                request: &boundary,
                route,
                current_max_output_tokens,
                attempt_override_present,
            })?;
        crate::admit_model_request(capped.value())
    }
    fn materialize_window(
        &mut self,
        host: &mut dyn HostPort,
        control: &CallControl,
        prepared: &PreparedInvocation,
        candidate: &BoundarySnapshot,
    ) -> PortResult<CapturedRequest> {
        let Some(mut routed) = candidate.value().as_object().cloned() else {
            return Err(invalid(
                "Materialization candidate must be a canonical request object.",
            ));
        };
        routed.insert(
            "provider".into(),
            JsValue::String(prepared.route.provider.clone()),
        );
        routed.insert(
            "model".into(),
            JsValue::String(prepared.route.model.clone()),
        );
        if host.advertised("model", "materialize_prepared_request") {
            let request = crate::admit_model_request(&JsValue::Object(routed))?;
            let HostReply::ModelMaterialized(request) = host.call(
                control,
                HostCall::ModelMaterialize {
                    handle: prepared.handle.clone(),
                    request: Box::new(request),
                },
            )?
            else {
                return Err(invalid(
                    "Materialization returned a reply for another operation.",
                ));
            };
            return Ok(*request);
        }
        let prepared_request = BoundarySnapshot::capture(prepared.request.wire.request());
        let candidate_request = BoundarySnapshot::capture(&JsValue::Object(routed));
        let merged = pilotdeck_agent::request_materialization::OriginalRequestMaterialization
            .default_prepared_window(
                pilotdeck_agent::request_materialization::PreparedWindowInput {
                    prepared_request: &prepared_request,
                    candidate_request: &candidate_request,
                    route: &prepared.route,
                },
            )?;
        crate::admit_model_request(merged.value())
    }
}
// Source budgetCalibrationFor uses the restored Map's joined UTF-16 key,
// including collisions across the separator. Empty route labels suppress lookup.
fn compaction_calibration(
    state: &pilotdeck_agent::session_state::AgentModelSessionState,
    route: &Route,
) -> Option<TokenCalibration> {
    if route.provider.is_empty() || route.model.is_empty() {
        return None;
    }
    let key = |route: &Route| {
        route
            .provider
            .units()
            .iter()
            .copied()
            .chain(std::iter::once(0))
            .chain(route.model.units().iter().copied())
            .collect::<Vec<u16>>()
    };
    let requested = key(route);
    state
        .token_calibration
        .iter()
        .rev()
        .find(|baseline| key(&baseline.route) == requested)
        .cloned()
}
struct InstructionObserver<'a> {
    notifications: &'a Arc<Mutex<InstructionBridge>>,
    input: &'a TurnInput,
}
impl RequestPreparationObserver for InstructionObserver<'_> {
    fn instructions_loaded(&mut self, has_system_prompt: bool) -> PortResult<()> {
        self.notifications
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .notice(self.input, has_system_prompt)
    }
}
impl TurnPreparationPort for Preparation {
    fn finish_pending_effects(&mut self) -> PortResult<()> {
        self.notifications
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .finish()
    }
    fn current_output_cap(
        &self,
        config: &TurnRuntimeConfig,
        route: &Route,
    ) -> PortResult<Option<f64>> {
        Ok(self.output_cap_options(config, route).0)
    }
    fn model_output_limit(&self, route: &Route) -> Option<f64> {
        let key = route_key(route);
        self.state
            .metadata
            .iter()
            .rev()
            .find(|metadata| route_key(&metadata.view.route) == key)
            .and_then(|metadata| metadata.view.token_limits.as_ref())
            .and_then(|limits| limits.max_output_tokens)
    }
    fn set_output_cap(&mut self, route: &Route, update: OutputCapUpdate) -> PortResult<()> {
        let key = route_key(route);
        let index = self
            .state
            .token_caps
            .iter()
            .position(|cap| route_key(&cap.route) == key);
        let index = match index {
            Some(index) => index,
            None => {
                self.state.token_caps.push(RouteTokenCaps {
                    route: route.clone(),
                    max_context_tokens: None,
                    attempt_max_output_tokens: None,
                    requested_max_output_tokens: None,
                    session_max_output_tokens: None,
                    hard_max_output_tokens: None,
                });
                self.state.token_caps.len() - 1
            }
        };
        match update {
            OutputCapUpdate::Requested(value) => {
                self.state.token_caps[index].requested_max_output_tokens = Some(value)
            }
            OutputCapUpdate::Attempt(value) => {
                self.state.token_caps[index].attempt_max_output_tokens = Some(value)
            }
            OutputCapUpdate::Session(value) => {
                self.state.token_caps[index].session_max_output_tokens = Some(value);
                if let Some(cap) = self
                    .model_state
                    .token_caps
                    .iter_mut()
                    .find(|cap| route_key(&cap.route) == key)
                {
                    cap.session_max_output_tokens = Some(value);
                } else {
                    self.model_state.token_caps.push(
                        pilotdeck_agent::session_state::PersistentTokenCap {
                            route: route.clone(),
                            max_context_tokens: None,
                            session_max_output_tokens: Some(value),
                            hard_max_output_tokens: None,
                        },
                    );
                }
            }
            OutputCapUpdate::HardCap(value) => {
                // AgentLoop.ts:2631-2635 merges `hardMaxOutputTokens` into the
                // same per-route record; AgentLoop.ts:360 never clears it, so it
                // is mirrored into the persistent modelState like Session caps.
                self.state.token_caps[index].hard_max_output_tokens = Some(value);
                if let Some(cap) = self
                    .model_state
                    .token_caps
                    .iter_mut()
                    .find(|cap| route_key(&cap.route) == key)
                {
                    cap.hard_max_output_tokens = Some(value);
                } else {
                    self.model_state.token_caps.push(
                        pilotdeck_agent::session_state::PersistentTokenCap {
                            route: route.clone(),
                            max_context_tokens: None,
                            session_max_output_tokens: None,
                            hard_max_output_tokens: Some(value),
                        },
                    );
                }
            }
        }
        Ok(())
    }

    fn set_context_cap(&mut self, route: &Route, max_context_tokens: f64) -> PortResult<()> {
        // AgentLoop.ts:1480 merges `maxContextTokens` into the same per-route
        // record (A:2633-2637); A:2648-2660 keeps it across turns, so it is
        // mirrored into the persistent modelState like Session/HardCap caps.
        let key = route_key(route);
        if let Some(cap) = self
            .state
            .token_caps
            .iter_mut()
            .find(|cap| route_key(&cap.route) == key)
        {
            cap.max_context_tokens = Some(max_context_tokens);
        } else {
            self.state.token_caps.push(RouteTokenCaps {
                route: route.clone(),
                max_context_tokens: Some(max_context_tokens),
                attempt_max_output_tokens: None,
                requested_max_output_tokens: None,
                session_max_output_tokens: None,
                hard_max_output_tokens: None,
            });
        }
        if let Some(cap) = self
            .model_state
            .token_caps
            .iter_mut()
            .find(|cap| route_key(&cap.route) == key)
        {
            cap.max_context_tokens = Some(max_context_tokens);
        } else {
            self.model_state
                .token_caps
                .push(pilotdeck_agent::session_state::PersistentTokenCap {
                    route: route.clone(),
                    max_context_tokens: Some(max_context_tokens),
                    session_max_output_tokens: None,
                    hard_max_output_tokens: None,
                });
        }
        Ok(())
    }

    fn compaction_token_limits(
        &self,
        config: &TurnRuntimeConfig,
        route: &Route,
        pre_route: bool,
    ) -> (f64, f64) {
        OriginalRequestAssembler.compaction_limits(&self.state, config, route, pre_route)
    }

    fn record_token_calibration(
        &mut self,
        route: &Route,
        usage: Option<&BoundarySnapshot>,
        estimated_input_tokens: Option<f64>,
    ) {
        pilotdeck_agent::session_state::record_token_calibration(
            &mut self.model_state,
            route,
            usage,
            estimated_input_tokens,
        );
    }

    fn clear_compaction_calibration(&mut self) {
        self.model_state.token_calibration.clear();
    }

    fn cap_prepared_request(
        &self,
        config: &TurnRuntimeConfig,
        prepared: &PreparedInvocation,
    ) -> PortResult<CapturedRequest> {
        self.apply_request_caps(config, &prepared.request, &prepared.route)
    }

    fn clear_attempt_output_cap(&mut self, route: &Route) {
        let key = route_key(route);
        if let Some(cap) = self
            .state
            .token_caps
            .iter_mut()
            .rev()
            .find(|cap| route_key(&cap.route) == key)
        {
            cap.attempt_max_output_tokens = None;
        }
    }

    fn before_model_request(&mut self, route: &Route) -> PortResult<()> {
        self.notifications
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .before_model_request(route)
    }
    fn observe_prepared_metadata(&mut self, prepared: &PreparedInvocation) {
        if let Some(metadata) = &prepared.metadata {
            let key = route_key(&metadata.view.route);
            if let Some(existing) = self
                .state
                .metadata
                .iter_mut()
                .find(|existing| route_key(&existing.view.route) == key)
            {
                *existing = metadata.clone();
            } else {
                self.state.metadata.push(metadata.clone());
            }
        }
    }

    fn prepare_routed_compaction(
        &mut self,
        host: &mut dyn HostPort,
        control: &CallControl,
        parameters: pilotdeck_agent::turn::RoutedCompactionPreparation<'_>,
    ) -> PortResult<Option<Box<CompactionIntent>>> {
        let pilotdeck_agent::turn::RoutedCompactionPreparation {
            config,
            input,
            messages,
            prepared,
            base_request,
        } = parameters;
        let (_, reserved_output_tokens) =
            self.compaction_token_limits(config, &prepared.route, false);
        let max_context_tokens = prepared.max_context_tokens.unwrap_or_else(|| {
            self.compaction_token_limits(config, &prepared.route, false)
                .0
        });
        let mut observer = InstructionObserver {
            notifications: &self.notifications,
            input,
        };
        let preview = OriginalRequestAssembler.assemble_with_options(
            &mut self.state,
            host,
            RequestAssemblyInput {
                control,
                config,
                input,
                messages,
            },
            pilotdeck_agent::request::RequestAssemblyOptions {
                preview_only: Some(true),
                emit_instruction_events: false,
            },
            &mut observer,
        )?;
        let base = BoundarySnapshot::capture(base_request.wire.request());
        let candidate = pilotdeck_agent::request_materialization::OriginalRequestMaterialization
            .budget_candidate(
                pilotdeck_agent::request_materialization::BudgetCandidateInput {
                    base_request: &base,
                    preview_request: &preview.request,
                },
            )?;
        let budget_request = self.materialize_window(host, control, prepared, &candidate)?;
        let preparation = OriginalRequestAssembler.context_preparation(
            &self.state,
            RequestAssemblyInput {
                control,
                config,
                input,
                messages,
            },
        )?;
        let calibration = compaction_calibration(&self.model_state, &prepared.route);
        Ok(Some(Box::new(CompactionIntent {
            session_id: Some(input.session_id.clone()),
            turn_id: Some(input.turn_id.clone()),
            stage: BudgetStage::Routed,
            trigger: CompactionTrigger::Auto,
            messages: messages.to_vec(),
            max_context_tokens: Some(max_context_tokens),
            reserved_output_tokens: Some(reserved_output_tokens),
            manual_force: None,
            allow_fallback_on_failure: None,
            budget_request: Some(budget_request),
            preparation: Some(preparation),
            prepared_handle: Some(prepared.handle.clone()),
            calibration,
        })))
    }

    fn rebuild_routed_request(
        &mut self,
        host: &mut dyn HostPort,
        control: &CallControl,
        config: &TurnRuntimeConfig,
        input: &TurnInput,
        messages: &[pilotdeck_agent::ProjectedMessage],
        prepared: &PreparedInvocation,
    ) -> PortResult<CapturedRequest> {
        let (request, _) = self.prepare(host, control, config, input, messages)?;
        let request = self.apply_request_caps(config, &request, &prepared.route)?;
        self.materialize_window(
            host,
            control,
            prepared,
            &BoundarySnapshot::capture(request.wire.request()),
        )
    }

    fn prepare_compaction(
        &mut self,
        host: &mut dyn HostPort,
        control: &CallControl,
        config: &TurnRuntimeConfig,
        input: &TurnInput,
        messages: &[pilotdeck_agent::ProjectedMessage],
    ) -> PortResult<Option<Box<CompactionIntent>>> {
        if !host.advertised("context", "try_auto_compact") {
            return Ok(None);
        }
        let (max_context_tokens, reserved_output_tokens) =
            OriginalRequestAssembler.compaction_limits(&self.state, config, &config.route, true);
        let mut observer = InstructionObserver {
            notifications: &self.notifications,
            input,
        };
        let preview = OriginalRequestAssembler.assemble_with_options(
            &mut self.state,
            host,
            RequestAssemblyInput {
                control,
                config,
                input,
                messages,
            },
            pilotdeck_agent::request::RequestAssemblyOptions {
                preview_only: Some(true),
                emit_instruction_events: false,
            },
            &mut observer,
        )?;
        let preparation = OriginalRequestAssembler.context_preparation(
            &self.state,
            RequestAssemblyInput {
                control,
                config,
                input,
                messages,
            },
        )?;
        let budget_request = crate::admit_model_request(preview.request.value())?;
        let calibration = compaction_calibration(&self.model_state, &budget_request.route);
        Ok(Some(Box::new(CompactionIntent {
            session_id: Some(input.session_id.clone()),
            turn_id: Some(input.turn_id.clone()),
            stage: BudgetStage::PreRoute,
            trigger: CompactionTrigger::Auto,
            messages: messages.to_vec(),
            max_context_tokens: Some(max_context_tokens),
            reserved_output_tokens: Some(reserved_output_tokens),
            manual_force: None,
            allow_fallback_on_failure: None,
            budget_request: Some(budget_request),
            preparation: Some(preparation),
            prepared_handle: None,
            calibration,
        })))
    }
    fn prepare_recovery_compaction(
        &mut self,
        host: &mut dyn HostPort,
        control: &CallControl,
        parameters: pilotdeck_agent::turn::RecoveryCompactionPreparation<'_>,
    ) -> PortResult<Option<Box<CompactionIntent>>> {
        let pilotdeck_agent::turn::RecoveryCompactionPreparation {
            config,
            input,
            messages,
            target,
        } = parameters;
        // AgentLoop.ts:1512 `ctx?.tryAutoCompact` absent -> no call.
        if !host.advertised("context", "try_auto_compact") {
            return Ok(None);
        }
        // A:1514 currentMaxContextTokens(target) after the branch's cap merge;
        // A:1515 getReservedOutputTokens(target) = currentMaxOutputTokens ?? 0.
        let max_context_tokens = OriginalRequestAssembler
            .compaction_limits(&self.state, config, target, false)
            .0;
        let reserved_output_tokens = self.output_cap_options(config, target).0.unwrap_or(0.0);
        let mut observer = InstructionObserver {
            notifications: &self.notifications,
            input,
        };
        // A:2484-2487 createModelRequest(previewOnly, no instruction events).
        let preview = OriginalRequestAssembler.assemble_with_options(
            &mut self.state,
            host,
            RequestAssemblyInput {
                control,
                config,
                input,
                messages,
            },
            pilotdeck_agent::request::RequestAssemblyOptions {
                preview_only: Some(true),
                emit_instruction_events: false,
            },
            &mut observer,
        )?;
        let preparation = OriginalRequestAssembler.context_preparation(
            &self.state,
            RequestAssemblyInput {
                control,
                config,
                input,
                messages,
            },
        )?;
        // A:2497-2502 `decision` branch: no prepared invocation is supplied, so
        // the preview keeps its own controls and only provider/model become the
        // recovery target.
        let Some(mut candidate) = preview.request.value().as_object().cloned() else {
            return Err(invalid(
                "Recovery budget candidate must be a canonical request object.",
            ));
        };
        candidate.insert("provider".into(), JsValue::String(target.provider.clone()));
        candidate.insert("model".into(), JsValue::String(target.model.clone()));
        let budget_request = crate::admit_model_request(&JsValue::Object(candidate))?;
        // A:1536 budgetCalibrationFor(target.provider, target.model).
        let calibration = compaction_calibration(&self.model_state, target);
        Ok(Some(Box::new(CompactionIntent {
            session_id: Some(input.session_id.clone()),
            turn_id: Some(input.turn_id.clone()),
            stage: BudgetStage::Recovery,
            trigger: CompactionTrigger::Auto,
            messages: messages.to_vec(),
            max_context_tokens: Some(max_context_tokens),
            reserved_output_tokens: Some(reserved_output_tokens),
            manual_force: None,
            allow_fallback_on_failure: Some(true),
            budget_request: Some(budget_request),
            preparation: Some(preparation),
            prepared_handle: None,
            calibration,
        })))
    }
    fn prepare(
        &mut self,
        host: &mut dyn HostPort,
        control: &CallControl,
        config: &TurnRuntimeConfig,
        input: &TurnInput,
        messages: &[pilotdeck_agent::ProjectedMessage],
    ) -> PortResult<(CapturedRequest, ModelHints)> {
        let mut observer = InstructionObserver {
            notifications: &self.notifications,
            input,
        };
        let intent = OriginalRequestAssembler.assemble(
            &mut self.state,
            host,
            RequestAssemblyInput {
                control,
                config,
                input,
                messages,
            },
            &mut observer,
        )?;
        Ok((
            crate::admit_model_request(intent.request.value())?,
            self.hints.clone(),
        ))
    }
    fn project_assembly(
        &self,
        value: &BoundarySnapshot,
    ) -> PortResult<pilotdeck_agent::turn::TurnAssembly> {
        // The original default factory forwards raw tools. The host's bound
        // capability runtime owns permission; explicit permission-wrapped port
        // consumers are a separate composition, not a second decision here.
        crate::model_admission::turn_assembled(value.value())
    }
    fn project_tool_calls(
        &self,
        calls: &[BoundarySnapshot],
    ) -> PortResult<Vec<pilotdeck_protocol::model::ToolCall>> {
        calls
            .iter()
            .map(|call| crate::model_admission::tool_call(call.value()))
            .collect()
    }
    fn project_tool_result(
        &self,
        result: &BoundarySnapshot,
    ) -> PortResult<pilotdeck_agent::ToolExecutionResult> {
        crate::model_admission::tool_result(result.value())
    }
    fn observe_tool_results(
        &mut self,
        host: &mut dyn HostPort,
        control: &CallControl,
        input: &TurnInput,
        _results: &[pilotdeck_agent::ToolExecutionResult],
    ) -> PortResult<()> {
        if !self.methods.supports("capability", "plan_todo") {
            return Ok(());
        }
        let reply = host.call(
            control,
            HostCall::PlanTodo(PlanTodoRequest {
                session_id: input.session_id.clone(),
                turn_id: input.turn_id.clone(),
                mutation: PlanTodoMutation::Read,
            }),
        )?;
        match reply {
            HostReply::PlanTodo(snapshot) => {
                self.state.plan_todo_addendum = plan_addendum(&snapshot.view);
                Ok(())
            }
            HostReply::PlanTodoBoundaryOnly(_) => {
                Err(gap("open PlanTodo observer prompt/blocking projection"))
            }
            _ => Err(gap("PlanTodo observer operation reply")),
        }
    }
}
struct EventSink<'a> {
    context: &'a mut ExecutionContext,
    notifications: Arc<Mutex<InstructionBridge>>,
}
impl TurnEventPort for EventSink<'_> {
    fn emit(&mut self, boundary: BoundarySnapshot) -> PortResult<()> {
        self.publish(boundary, false)
    }
    fn emit_accepted(&mut self, boundary: BoundarySnapshot) -> PortResult<()> {
        self.publish(boundary, true)
    }
}
impl EventSink<'_> {
    fn publish(&mut self, boundary: BoundarySnapshot, accepted: bool) -> PortResult<()> {
        let payload = boundary
            .value()
            .as_object()
            .ok_or_else(|| gap("AgentEvent object admission"))?
            .clone();
        let kind = payload
            .get("type")
            .and_then(JsValue::as_string)
            .ok_or_else(|| gap("AgentEvent type admission"))?;
        // Source loop completion remains internal until accepted advisory calls
        // have a known outcome. This IO owner is shared only by the one turn
        // thread's preparation and event ports; the reader remains independent.
        if kind.equals_str("turn_completed") {
            self.context.check_accepted_uncertainty()?;
            self.notifications
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .finish()?;
        }
        let mut name: Vec<u16> = "agent.".encode_utf16().collect();
        name.extend_from_slice(kind.units());
        if accepted {
            self.context
                .emit_accepted(JsString::from_utf16(name), payload)
        } else {
            self.context.emit(JsString::from_utf16(name), payload)
        }
    }
}
impl ExecutionFactory for OriginalAgentFactory {
    fn execute(
        &self,
        request: &ExecuteRequest,
        context: &mut ExecutionContext,
    ) -> PortResult<ExecutionTerminal> {
        context.control.check()?;
        let mut decoded = OriginalFactoryPayloadDecoder
            .decode(&request.boundary, &self.process_cwd)
            .map_err(|error| {
                let message = match error {
                    crate::execute::FactoryPayloadError::NativeGraphRequired { message }
                    | crate::execute::FactoryPayloadError::SourceTypeError { message }
                    | crate::execute::FactoryPayloadError::InvalidSubagentModel { message }
                    | crate::execute::FactoryPayloadError::InvalidEnvelope { message } => message,
                };
                PortError::SourceException {
                    message,
                    boundary: BoundarySnapshot::capture(&request.boundary),
                }
            })?;
        let identity = context.identity(
            decoded.input.session_id.clone(),
            decoded.input.turn_id.clone(),
        );
        let exchange = context.exchange(identity.clone())?;
        let mut effective_methods = decoded.methods.clone();
        // Original model.stream and capability.execute are baseline operations;
        // advertisements select optional preparation, pull and batch protocols.
        for (module, operation) in [("model", "stream"), ("capability", "execute")] {
            if let Some((_, methods)) = effective_methods
                .modules
                .iter_mut()
                .find(|(m, _)| m.equals_str(module))
                && !methods.iter().any(|m| m.equals_str(operation))
            {
                methods.push(operation.into());
            }
        }
        let mut host = RemoteHost::new(exchange, effective_methods);
        // Order matches the original default factory: initialize plan state,
        // fetch initial model metadata, then parse file/model projections.
        let mut plan_todo_addendum = None;
        if host.advertised("capability", "plan_todo") {
            let reply = host.call(
                &context.control,
                HostCall::PlanTodo(PlanTodoRequest {
                    session_id: decoded.input.session_id.clone(),
                    turn_id: decoded.input.turn_id.clone(),
                    mutation: PlanTodoMutation::Read,
                }),
            )?;
            plan_todo_addendum = match reply {
                HostReply::PlanTodo(snapshot) => plan_addendum(&snapshot.view),
                HostReply::PlanTodoBoundaryOnly(_) => {
                    return Err(gap("open PlanTodo prompt/blocking projection"));
                }
                _ => return Err(gap("PlanTodo read operation reply")),
            };
        }
        let mut metadata = Vec::new();
        if host.advertised("model", "get_metadata") {
            let route = decoded
                .input
                .model_override
                .as_ref()
                .map_or_else(|| decoded.config.route.clone(), |m| m.route.clone());
            let HostReply::ModelMetadata(value) =
                host.call(&context.control, HostCall::ModelMetadata { route })?
            else {
                return Err(gap("metadata operation reply"));
            };
            metadata.push(value);
        }
        let file_state = parse_file_state(
            decoded
                .seed_state
                .as_ref()
                .map_or(&JsValue::Undefined, BoundarySnapshot::value),
        )?
        .unwrap_or_default();
        let model_state = parse_model_state(
            decoded
                .model_state
                .as_ref()
                .map_or(&JsValue::Undefined, BoundarySnapshot::value),
        )?
        .unwrap_or_default();
        // Host-owned sidecar tools do not receive native file maps. The volatile
        // projection is retained for terminal checkpoint round trips.
        let state = RequestPreparationState {
            tools: decoded.tools,
            metadata,
            plan_todo_addendum: plan_todo_addendum.take(),
            elicitation_available: decoded.elicitation_available,
            token_caps: model_state
                .token_caps
                .iter()
                .map(|cap| RouteTokenCaps {
                    route: cap.route.clone(),
                    max_context_tokens: cap.max_context_tokens,
                    attempt_max_output_tokens: None,
                    requested_max_output_tokens: None,
                    session_max_output_tokens: cap.session_max_output_tokens,
                    hard_max_output_tokens: cap.hard_max_output_tokens,
                })
                .collect(),
        };
        let execution = ExecutionHints {
            run_id: identity.run_id.clone(),
            operation_id: identity.operation_id.clone(),
            idempotency_key: identity.idempotency_key.clone(),
            operation_deadline: request
                .boundary
                .get("operationDeadline")
                .and_then(JsValue::as_string)
                .cloned(),
        };
        let notifications = Arc::new(Mutex::new(InstructionBridge::new(
            context.exchange(identity.clone())?,
            context.exchange(identity.clone())?,
            decoded.methods.clone(),
            context.control.clone(),
        )?));
        let mut preparation = Preparation {
            state,
            model_state,
            notifications: notifications.clone(),
            methods: decoded.methods,
            hints: ModelHints {
                session_id: decoded.input.session_id.clone(),
                turn_id: decoded.input.turn_id.clone(),
                execution: execution.clone(),
                workspace_id: None,
                storage_config_version: None,
                metadata: None,
                model_override: decoded
                    .input
                    .model_override
                    .as_ref()
                    .map(|m| m.route.clone()),
            },
        };
        let mut extensions = JsObject::new();
        extensions.insert(
            "permissionContext".into(),
            permission_context(&decoded.config),
        );
        let mut tools = ToolHints {
            session_id: decoded.input.session_id.clone(),
            turn_id: decoded.input.turn_id.clone(),
            cwd: decoded.config.cwd.clone(),
            permission_mode: decoded.config.permission.mode,
            run_mode: Some(run_mode(decoded.config.run_mode).into()),
            can_prompt: decoded.config.permission.can_prompt,
            can_elicit: Some(decoded.input.can_elicit),
            additional_working_directories: decoded
                .config
                .permission
                .additional_working_directories
                .clone(),
            context_extensions: extensions,
            execution,
        };
        let initial = decoded
            .input
            .messages
            .iter()
            .map(|m| crate::admit_message(m.value()))
            .collect::<PortResult<Vec<_>>>()?;
        let control = context.control.clone();
        let cleanup = CallControl {
            cancellation: Cancellation::default(),
            // The invocation owner starts the bounded close budget when cleanup
            // is needed, rather than expiring it during a long healthy stream.
            deadline: None,
        };
        let clock = ProductionTurnClock(self.clock.clone());
        let mut events = EventSink {
            context,
            notifications: notifications.clone(),
        };
        let mut host = NotificationFencedHost {
            host,
            notifications: notifications.clone(),
        };
        let output = run_admitted_turn(
            &mut decoded.config,
            &decoded.input,
            initial,
            &mut host,
            &mut preparation,
            &mut events,
            &clock,
            &control,
            &cleanup,
            &mut tools,
        );
        // Every owning exit explicitly settles notices. Preserve an existing
        // Unknown first; otherwise a newly unresolved accepted notice outranks
        // an ordinary primary failure. Drop is cleanup, not classification.
        let settled = notifications
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .finish();
        let responsibility = context.check_accepted_uncertainty();
        let output = match (output, settled, responsibility) {
            (Err(error @ PortError::AcceptedUnknown { .. }), _, _) => return Err(error),
            (_, Err(error @ PortError::AcceptedUnknown { .. }), _) => return Err(error),
            (_, _, Err(error @ PortError::AcceptedUnknown { .. })) => return Err(error),
            (Err(error), _, _) | (Ok(_), Err(error), _) | (Ok(_), Ok(()), Err(error)) => {
                return Err(error);
            }
            (Ok(output), Ok(()), Ok(())) => output,
        };
        let mut payload = JsObject::new();
        payload.insert(
            "result".into(),
            pilotdeck_agent::turn_wire::result(&decoded.input, &output.result)
                .value()
                .clone(),
        );
        payload.insert(
            "messages".into(),
            JsValue::Array(
                output
                    .messages
                    .iter()
                    .map(|m| m.boundary.value().clone())
                    .collect(),
            ),
        );
        payload.insert(
            "seedState".into(),
            snapshot_file_state(&file_state).value().clone(),
        );
        payload.insert(
            "modelState".into(),
            snapshot_model_state(&preparation.model_state)
                .value()
                .clone(),
        );
        Ok(ExecutionTerminal {
            outcome: match output.result.kind {
                pilotdeck_agent::turn::TurnResultKind::Success => ModuleOutcome::Completed,
                pilotdeck_agent::turn::TurnResultKind::Aborted => ModuleOutcome::Cancelled,
                _ => ModuleOutcome::Failed,
            },
            payload,
            error: output
                .result
                .errors
                .first()
                .and_then(|error| error.value().as_object())
                .cloned(),
        })
    }
}
fn invalid(message: &str) -> PortError {
    PortError::InvalidResponse {
        message: message.into(),
    }
}
fn gap(profile: &str) -> PortError {
    PortError::InvalidResponse {
        message: format!("Rust factory candidate has not implemented {profile}.").into(),
    }
}
fn run_mode(mode: Option<pilotdeck_agent::turn::RunMode>) -> &'static str {
    match mode {
        Some(pilotdeck_agent::turn::RunMode::Plan) => "plan",
        Some(pilotdeck_agent::turn::RunMode::Ask) => "ask",
        _ => "agent",
    }
}
fn permission_context(config: &TurnRuntimeConfig) -> JsValue {
    let mut object = JsObject::new();
    object.insert(
        "mode".into(),
        match config.permission.mode {
            PermissionMode::Default => "default",
            PermissionMode::Plan => "plan",
            PermissionMode::BypassPermissions => "bypassPermissions",
        }
        .into(),
    );
    object.insert(
        "canPrompt".into(),
        JsValue::Bool(config.permission.can_prompt),
    );
    object.insert(
        "bypassAvailable".into(),
        JsValue::Bool(config.permission.bypass_available),
    );
    object.insert(
        "additionalWorkingDirectories".into(),
        JsValue::Array(
            config
                .permission
                .additional_working_directories
                .iter()
                .cloned()
                .map(JsValue::String)
                .collect(),
        ),
    );
    if let Some(rules) = &config.permission.rules {
        let mut value = JsObject::new();
        for (key, rules) in [
            ("allow", &rules.allow),
            ("deny", &rules.deny),
            ("ask", &rules.ask),
        ] {
            value.insert(
                key.into(),
                JsValue::Array(rules.clone().unwrap_or_default()),
            );
        }
        object.insert("rules".into(), JsValue::Object(value));
    }
    JsValue::Object(object)
}
fn plan_addendum(snapshot: &PlanTodoSnapshot) -> Option<JsString> {
    if snapshot
        .approved_plan
        .as_ref()
        .is_none_or(JsString::is_empty)
    {
        return None;
    }
    if snapshot.requires_initialization {
        Some("You are executing an approved plan.\nBefore using any non-read-only tool, you MUST call `todo_write` with a markdown checklist derived from the approved plan.\nRepresent completed items as `- [x]` and remaining items as `- [ ]`.".into())
    } else if snapshot.tool_calls_since_last_todo_write >= 10.0 {
        Some(format!("You haven't updated the todo list in a while ({} tool calls since last update). Consider calling `todo_write` to reflect your current progress. This is a gentle reminder - ignore if not applicable.", snapshot.tool_calls_since_last_todo_write).into())
    } else {
        None
    }
}

#[cfg(test)]
mod calibration_lookup_tests {
    use super::*;
    use pilotdeck_agent::session_state::AgentModelSessionState;
    #[test]
    fn joined_key_collision_returns_the_original_baseline_route() {
        let baseline = TokenCalibration {
            route: Route {
                provider: "a\0b".into(),
                model: "c".into(),
            },
            actual_input_tokens: 19.0,
            estimated_input_tokens: 7.0,
        };
        let state = AgentModelSessionState {
            token_calibration: vec![baseline.clone()],
            token_caps: vec![],
        };
        let found = compaction_calibration(
            &state,
            &Route {
                provider: "a".into(),
                model: "b\0c".into(),
            },
        )
        .unwrap();
        assert_eq!(found.route, baseline.route);
        assert_eq!(found.actual_input_tokens, 19.0);
        assert_eq!(found.estimated_input_tokens, 7.0);
    }
    #[test]
    fn empty_route_labels_suppress_even_a_present_map_entry() {
        for route in [
            Route {
                provider: "".into(),
                model: "c".into(),
            },
            Route {
                provider: "a".into(),
                model: "".into(),
            },
        ] {
            let state = AgentModelSessionState {
                token_calibration: vec![TokenCalibration {
                    route: route.clone(),
                    actual_input_tokens: 19.0,
                    estimated_input_tokens: 7.0,
                }],
                token_caps: vec![],
            };
            assert!(compaction_calibration(&state, &route).is_none());
        }
    }
}
