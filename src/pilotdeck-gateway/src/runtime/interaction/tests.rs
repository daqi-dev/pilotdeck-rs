use super::*;
use crate::config::permission::GatewaySessionPermissionRuleSetRegistry;
use crate::test_baseline as baseline;
use pilotdeck_protocol::stringify_json;
use std::sync::{
    Weak,
    atomic::{AtomicU64, Ordering},
};

fn field(value: &JsValue, key: &str) -> JsString {
    value
        .get(key)
        .and_then(JsValue::as_string)
        .cloned()
        .unwrap_or_else(|| panic!("missing string {key}"))
}
fn text(value: &JsValue, key: &str) -> String {
    field(value, key).to_utf8().unwrap()
}
fn array(value: &JsValue) -> &[JsValue] {
    if let JsValue::Array(values) = value {
        values
    } else {
        panic!("expected array")
    }
}
fn encoded(value: &JsValue) -> String {
    stringify_json(value).unwrap().unwrap()
}
fn binding(step: &JsValue, key: &str) -> Result<Option<InteractionBinding>, JsString> {
    step.get(key)
        .map(InteractionBinding::from_value)
        .transpose()
}
fn permission(step: &JsValue) -> bool {
    step.get("kind")
        .and_then(JsValue::as_string)
        .is_some_and(|s| s.equals_str("permission"))
}
struct Timer {
    deadline: u64,
    step: JsValue,
    registration: Registration,
}
struct Harness {
    coordinator: GatewayInteractionCoordinator,
    events: Mutex<Vec<JsValue>>,
    registrations: Mutex<Vec<(JsString, Registration)>>,
    timers: Mutex<Vec<Timer>>,
    now: AtomicU64,
}
impl Harness {
    fn new() -> Arc<Self> {
        Self::build(false)
    }
    /// Every `InteractionOptions` component is injected, mirroring the
    /// generator's `injectedOptions` scenarios.
    fn injected() -> Arc<Self> {
        Self::build(true)
    }
    fn build(injected: bool) -> Arc<Self> {
        Arc::new_cyclic(|weak: &Weak<Self>| {
            let weak = weak.clone();
            let delivered: DeliveredObserver = Arc::new(move |session, id| {
                weak.upgrade().unwrap().events.lock().unwrap().push(object([
                    ("event", "delivered".into()),
                    ("session", JsValue::String(session.clone())),
                    ("id", JsValue::String(id.clone())),
                ]));
            });
            let options = if injected {
                let port = InteractionReconnectPort::default();
                InteractionOptions {
                    reconnect_port: Some(port.clone()),
                    permission_bus: Some(GatewayPermissionBus::new(port.clone())),
                    elicitation_bus: Some(GatewayElicitationBus::new(port)),
                    permission_grants: Some(Arc::new(
                        GatewaySessionPermissionRuleSetRegistry::default(),
                    )),
                    on_elicitation_delivered: Some(delivered),
                }
            } else {
                InteractionOptions {
                    on_elicitation_delivered: Some(delivered),
                    ..Default::default()
                }
            };
            Self {
                coordinator: GatewayInteractionCoordinator::new(options),
                events: Mutex::new(vec![]),
                registrations: Mutex::new(vec![]),
                timers: Mutex::new(vec![]),
                now: AtomicU64::new(0),
            }
        })
    }
    fn register(self: &Arc<Self>, step: &JsValue) -> Result<JsValue, JsString> {
        let session = field(step, "session");
        let id = field(step, "id");
        let kind = field(step, "kind");
        let is_permission = permission(step);
        let reenter = step
            .get("reenter")
            .and_then(JsValue::as_bool)
            .unwrap_or(false);
        let reenter_steps = step.get("reenterSteps").cloned();
        let reentered = AtomicBool::new(false);
        let weak = Arc::downgrade(self);
        let callback = Arc::new(move |event: &str, value: JsValue| {
            let harness = weak.upgrade().unwrap();
            harness.events.lock().unwrap().push(object([
                ("event", event.into()),
                ("kind", JsValue::String(kind.clone())),
                ("session", JsValue::String(session.clone())),
                ("id", JsValue::String(id.clone())),
                ("value", value),
                (
                    "p",
                    JsValue::Number(
                        harness.coordinator.permission_bus().pending_count(&session) as f64
                    ),
                ),
                (
                    "q",
                    JsValue::Number(
                        harness
                            .coordinator
                            .elicitation_bus()
                            .pending_count(&session) as f64,
                    ),
                ),
                (
                    "replay",
                    JsValue::Number(
                        harness
                            .coordinator
                            .reconnect_port()
                            .snapshot(&session)
                            .len() as f64,
                    ),
                ),
            ]));
            if reenter {
                harness
                    .register(&object([
                        ("kind", JsValue::String(kind.clone())),
                        ("session", JsValue::String(session.clone())),
                        ("id", "reentered".into()),
                    ]))
                    .unwrap();
            }
            if let Some(actions) = &reenter_steps
                && !reentered.swap(true, Ordering::SeqCst)
            {
                for action in array(actions) {
                    let result = harness.run(action).unwrap();
                    harness
                        .events
                        .lock()
                        .unwrap()
                        .push(object([("event", "reentry".into()), ("result", result)]));
                }
            }
        });
        let resolve = callback.clone();
        let request_id = field(step, "id");
        let tool_call_id = step
            .get("toolCallId")
            .and_then(JsValue::as_string)
            .cloned()
            .unwrap_or_else(|| {
                JsString::from_utf16(
                    "tc-"
                        .encode_utf16()
                        .chain(request_id.units().iter().copied())
                        .collect(),
                )
            });
        let entry = PendingEntry {
            request_id,
            tool_call_id,
            tool_name: step
                .get("toolName")
                .and_then(JsValue::as_string)
                .cloned()
                .unwrap_or_else(|| "fixture".into()),
            resolve: Arc::new(move |value| resolve("resolve", value)),
            reject: Arc::new(move |error| callback("reject", JsValue::String(error))),
        };
        let session = field(step, "session");
        let payload = if step.get("specialPayload").and_then(JsValue::as_bool) == Some(true) {
            Some(object([
                ("keep", "text".into()),
                ("omit", JsValue::Undefined),
                ("nan", JsValue::Number(f64::NAN)),
                ("inf", JsValue::Number(f64::INFINITY)),
                (
                    "arr",
                    JsValue::Array(vec![JsValue::Undefined, JsValue::Number(f64::NAN)]),
                ),
                (
                    "nested",
                    object([
                        ("__proto__", object([("hidden", JsValue::Number(1.0))])),
                        ("visible", JsValue::Number(2.0)),
                    ]),
                ),
            ]))
        } else {
            step.get("payload").cloned()
        };
        let peer = step.get("peer").and_then(JsValue::as_bool) == Some(true);
        let registration = if peer && is_permission {
            GatewayPermissionBus::new(self.coordinator.reconnect_port().clone())
                .register(&session, entry, payload)
        } else if peer {
            GatewayElicitationBus::new(self.coordinator.reconnect_port().clone())
                .register(&session, entry, payload)
        } else if is_permission {
            self.coordinator
                .permission_bus()
                .register(&session, entry, payload)
        } else {
            self.coordinator
                .elicitation_bus()
                .register(&session, entry, payload)
        }?;
        let tag = step
            .get("tag")
            .and_then(JsValue::as_string)
            .cloned()
            .unwrap_or_else(|| field(step, "id"));
        let mut registrations = self.registrations.lock().unwrap();
        if let Some((_, current)) = registrations.iter_mut().find(|(key, _)| key == &tag) {
            *current = registration;
        } else {
            registrations.push((tag, registration));
        }
        Ok(object([("registered", JsValue::Bool(true))]))
    }
    fn registration(&self, tag: &JsString) -> Registration {
        self.registrations
            .lock()
            .unwrap()
            .iter()
            .find(|(key, _)| key == tag)
            .unwrap()
            .1
            .clone()
    }
    fn consume(
        &self,
        session: &JsString,
        id: &JsString,
        p: bool,
        binding: Option<&InteractionBinding>,
    ) -> Option<PendingEntry> {
        if p {
            self.coordinator
                .permission_bus()
                .consume(session, id, binding)
        } else {
            self.coordinator
                .elicitation_bus()
                .consume(session, id, binding)
        }
    }
    fn run(self: &Arc<Self>, step: &JsValue) -> Result<JsValue, JsString> {
        let session = step
            .get("session")
            .and_then(JsValue::as_string)
            .cloned()
            .unwrap_or_else(|| "s".into());
        let p = self.coordinator.permission_bus();
        let q = self.coordinator.elicitation_bus();
        let reason = || field(step, "reason");
        match text(step, "op").as_str() {
            "register" => return self.register(step),
            "registration_dispose" => self.registration(&field(step, "tag")).dispose(),
            "answer" => {
                return Ok(object([(
                    "delivered",
                    JsValue::Bool(self.coordinator.respond_elicitation(
                        &session,
                        &field(step, "id"),
                        step.get("answer").cloned().unwrap(),
                        binding(step, "binding")?.as_ref(),
                    )),
                )]));
            }
            "decide" => {
                return Ok(object([(
                    "delivered",
                    JsValue::Bool(self.coordinator.decide_permission(
                        &session,
                        PermissionDecision {
                            request_id: field(step, "id"),
                            decision: field(step, "decision"),
                            remember: step.get("remember").cloned(),
                            reason: step.get("reason").cloned(),
                        },
                        binding(step, "binding")?.as_ref(),
                    )),
                )]));
            }
            "consume" => {
                let entry = self.consume(
                    &session,
                    &field(step, "id"),
                    permission(step),
                    binding(step, "binding")?.as_ref(),
                );
                let consumed = entry.is_some();
                if let Some(entry) = entry {
                    (entry.resolve)(step.get("value").cloned().unwrap());
                }
                return Ok(object([("consumed", JsValue::Bool(consumed))]));
            }
            "reject" => {
                if permission(step) {
                    p.reject_session(&session, &reason());
                } else {
                    q.reject_session(&session, &reason());
                }
            }
            "deny" => p.deny_session(&session, &reason()),
            "bus_dispose" => {
                let reason = step.get("reason").and_then(JsValue::as_string);
                if permission(step) {
                    p.dispose(reason);
                } else {
                    q.dispose(reason);
                }
            }
            "reconnect" => {
                return self.coordinator.reconnect(
                    &session,
                    binding(step, "next")?,
                    binding(step, "previous")?.as_ref(),
                );
            }
            "reconnect_turn" => {
                return self
                    .coordinator
                    .reconnect_for_turn(&session, binding(step, "next")?.unwrap());
            }
            "disconnect" => {
                return self
                    .coordinator
                    .disconnect(&session, &binding(step, "binding")?.unwrap());
            }
            "reject_turn" => self.coordinator.reject_pending_turn(&session, &reason()),
            "close" => self.coordinator.close_session(&session, &reason()),
            "dispose" => self.coordinator.dispose(&reason()),
            "port_dispose" => self.coordinator.reconnect_port().dispose_owner(&session),
            "grant" => {
                return Ok(self
                    .coordinator
                    .grant_session_permission(&session, &field(step, "entry")));
            }
            "schedule" => {
                let deadline = self.now.load(Ordering::SeqCst)
                    + step.get("delay").and_then(JsValue::as_number).unwrap() as u64;
                self.timers.lock().unwrap().push(Timer {
                    deadline,
                    step: step.clone(),
                    registration: self.registration(&field(step, "id")),
                });
            }
            "tick" => {
                let now = self.now.fetch_add(
                    step.get("ms").and_then(JsValue::as_number).unwrap() as u64,
                    Ordering::SeqCst,
                ) + step.get("ms").and_then(JsValue::as_number).unwrap() as u64;
                loop {
                    let timer = {
                        let mut timers = self.timers.lock().unwrap();
                        timers
                            .iter()
                            .enumerate()
                            .filter(|(_, t)| t.deadline <= now)
                            .min_by_key(|(_, t)| t.deadline)
                            .map(|(i, _)| i)
                            .map(|i| timers.remove(i))
                    };
                    let Some(timer) = timer else {
                        break;
                    };
                    let session = field(&timer.step, "session");
                    let id = field(&timer.step, "id");
                    if text(&timer.step, "event") == "timeout" {
                        if timer.registration.active()
                            && let Some(entry) =
                                self.consume(&session, &id, permission(&timer.step), None)
                        {
                            (entry.reject)("request_timeout".into());
                        }
                    } else if permission(&timer.step) {
                        self.coordinator.decide_permission(
                            &session,
                            PermissionDecision {
                                request_id: id,
                                decision: "allow".into(),
                                remember: None,
                                reason: None,
                            },
                            None,
                        );
                    } else {
                        self.coordinator.respond_elicitation(
                            &session,
                            &id,
                            object([
                                ("behavior", "answered".into()),
                                ("value", object([("text", "yes".into())])),
                            ]),
                            None,
                        );
                    }
                }
            }
            op => panic!("unknown step {op}"),
        }
        Ok(JsValue::Null)
    }
    fn snapshot(&self, result: JsValue, owners: &[JsString], step: &JsValue) -> JsValue {
        let p = self.coordinator.permission_bus();
        let q = self.coordinator.elicitation_bus();
        let id = step
            .get("id")
            .and_then(JsValue::as_string)
            .cloned()
            .unwrap_or_else(|| "r".into());
        object([
            ("result", result),
            (
                "events",
                JsValue::Array(self.events.lock().unwrap().clone()),
            ),
            (
                "registrations",
                JsValue::Array(
                    self.registrations
                        .lock()
                        .unwrap()
                        .iter()
                        .map(|(tag, r)| {
                            object([
                                ("tag", JsValue::String(tag.clone())),
                                ("active", JsValue::Bool(r.active())),
                            ])
                        })
                        .collect(),
                ),
            ),
            (
                "owners",
                JsValue::Array(
                    owners
                        .iter()
                        .map(|owner| {
                            object([
                                ("session", JsValue::String(owner.clone())),
                                ("p", JsValue::Number(p.pending_count(owner) as f64)),
                                ("q", JsValue::Number(q.pending_count(owner) as f64)),
                                ("hasP", JsValue::Bool(p.has_pending_session(owner))),
                                ("hasQ", JsValue::Bool(q.has_pending_session(owner))),
                                ("pendingP", JsValue::Bool(p.has_pending(owner, &id))),
                                ("pendingQ", JsValue::Bool(q.has_pending(owner, &id))),
                                ("pSnapshot", JsValue::Array(p.snapshot(owner))),
                                ("qSnapshot", JsValue::Array(q.snapshot(owner))),
                                (
                                    "snapshot",
                                    JsValue::Array(
                                        self.coordinator.reconnect_port().snapshot(owner),
                                    ),
                                ),
                                (
                                    "binding",
                                    self.coordinator
                                        .get_binding(owner)
                                        .map_or(JsValue::Null, |b| b.to_value()),
                                ),
                                (
                                    "rules",
                                    self.coordinator.session_allow_rules(owner).to_value(),
                                ),
                            ])
                        })
                        .collect(),
                ),
            ),
        ])
    }
}
#[test]
fn interaction_real_ts_stepwise_oracle() {
    let oracle = baseline::oracle(include_str!(
        "../../../tests/fixtures/interaction-bus/oracle.json"
    ));
    assert_eq!(
        text(&oracle, "sourceCommit"),
        "cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85"
    );
    let scenarios = array(oracle.get("scenarios").unwrap());
    assert_eq!(scenarios.len(), 48, "recorded TS scenario cardinality");
    assert_eq!(
        scenarios
            .iter()
            .map(|s| array(s.get("steps").unwrap()).len())
            .sum::<usize>(),
        281,
        "recorded TS step cardinality"
    );
    for scenario in scenarios {
        let injected = scenario.get("injectedOptions").and_then(JsValue::as_bool) == Some(true);
        let harness = if injected {
            Harness::injected()
        } else {
            Harness::new()
        };
        let steps = array(scenario.get("steps").unwrap());
        assert_eq!(
            steps.len(),
            array(scenario.get("results").unwrap()).len(),
            "each step requires a result"
        );
        let mut owners = vec![JsString::from("s")];
        for step in steps {
            if let Some(owner) = step.get("session").and_then(JsValue::as_string)
                && !owners.contains(owner)
            {
                owners.push(owner.clone());
            }
        }
        for (index, (step, expected)) in steps
            .iter()
            .zip(array(scenario.get("results").unwrap()))
            .enumerate()
        {
            let result = harness
                .run(step)
                .unwrap_or_else(|error| object([("error", JsValue::String(error))]));
            let actual = harness.snapshot(result, &owners, step);
            assert_eq!(
                encoded(&actual),
                encoded(expected),
                "{} step {index}: {}",
                text(scenario, "name"),
                encoded(step)
            );
        }
        harness.coordinator.dispose(&"fixture_end".into());
    }
}

#[test]
fn interaction_register_racing_reject_window_matches_ts_map_semantics() {
    use std::sync::mpsc;
    // Real TS runs rejectSession's exhausted loop and bySession.delete as
    // adjacent statements (GatewayElicitationBus.ts:127-138; probe evidence in
    // this issue's RESULT). A register the live bucket still holds is settled by
    // the loop; a register landing after the delete keeps a fresh bucket; a
    // register into a bucket the loop does not hold is dropped as an unsettled
    // zombie whose replay record survives — the pinned TS baseline
    // reject-deny-register-original-map.
    for p in [false, true] {
        for replacement in [false, true] {
            let events: Arc<Mutex<Vec<(&'static str, &'static str)>>> =
                Arc::new(Mutex::new(Vec::new()));
            let coordinator = Arc::new(GatewayInteractionCoordinator::new(
                InteractionOptions::default(),
            ));
            let register = move |coordinator: &GatewayInteractionCoordinator,
                                 entry: PendingEntry| {
                if p {
                    coordinator
                        .permission_bus()
                        .register(&"s".into(), entry, None)
                } else {
                    coordinator
                        .elicitation_bus()
                        .register(&"s".into(), entry, None)
                }
                .unwrap()
            };
            let reject = move |coordinator: &GatewayInteractionCoordinator, reason: &str| {
                let reason = reason.into();
                if p {
                    coordinator
                        .permission_bus()
                        .reject_session(&"s".into(), &reason);
                } else {
                    coordinator
                        .elicitation_bus()
                        .reject_session(&"s".into(), &reason);
                }
            };
            let pending_count = move |coordinator: &GatewayInteractionCoordinator| {
                if p {
                    coordinator.permission_bus().pending_count(&"s".into())
                } else {
                    coordinator.elicitation_bus().pending_count(&"s".into())
                }
            };
            let (entered_tx, entered_rx) = mpsc::channel();
            let (resume_tx, resume_rx) = mpsc::channel();
            let resume_rx = Arc::new(Mutex::new(resume_rx));
            let paused = Arc::new(AtomicBool::new(false));
            for id in ["first", "last"] {
                let (resolved, rejected) = (events.clone(), events.clone());
                let (entered_tx, resume_rx, paused) =
                    (entered_tx.clone(), resume_rx.clone(), paused.clone());
                let entry = PendingEntry {
                    request_id: id.into(),
                    tool_call_id: id.into(),
                    tool_name: "fixture".into(),
                    resolve: Arc::new(move |_| resolved.lock().unwrap().push((id, "resolve"))),
                    reject: Arc::new(move |_| {
                        rejected.lock().unwrap().push((id, "reject"));
                        if id == "first" && !paused.swap(true, Ordering::SeqCst) {
                            entered_tx.send(()).unwrap();
                            resume_rx.lock().unwrap().recv().unwrap();
                        }
                    }),
                };
                register(&coordinator, entry);
            }
            let rejecting = coordinator.clone();
            let worker = std::thread::spawn(move || {
                reject(&rejecting, "closed");
            });
            // The worker is parked inside first's reject callback, holding no
            // lock: a cross-thread register can land around its final scan.
            entered_rx.recv().unwrap();
            let (settled, dropped) = (events.clone(), events.clone());
            let entry = PendingEntry {
                request_id: "new".into(),
                tool_call_id: "new".into(),
                tool_name: "fixture".into(),
                resolve: Arc::new(move |_| settled.lock().unwrap().push(("new", "resolve"))),
                reject: Arc::new(move |_| dropped.lock().unwrap().push(("new", "reject"))),
            };
            if replacement {
                // A second bulk finishes first: it settles "last" ("first" is
                // claimed cross-thread) and deletes the shared bucket, so
                // "new" registers into a bucket the parked loop never held.
                reject(&coordinator, "other");
            }
            let registration = register(&coordinator, entry);
            resume_tx.send(()).unwrap();
            worker.join().unwrap();
            let events = events.lock().unwrap();
            let settle_count =
                |id: &'static str| events.iter().filter(|(key, _)| *key == id).count();
            for id in ["first", "last"] {
                assert_eq!(
                    settle_count(id),
                    1,
                    "{id}: p={p}, {replacement:?}, {events:?}"
                );
            }
            if replacement {
                // The zombie matches the TS replacement-Map baseline: no
                // callback, active handle, no pending, replay residue.
                assert_eq!(settle_count("new"), 0, "p={p}, {events:?}");
                assert!(registration.active(), "p={p}");
                assert_eq!(pending_count(&coordinator), 0, "p={p}");
                assert_eq!(
                    coordinator.reconnect_port().snapshot(&"s".into()).len(),
                    1,
                    "p={p}"
                );
                let delivered = if p {
                    coordinator.decide_permission(
                        &"s".into(),
                        PermissionDecision {
                            request_id: "new".into(),
                            decision: "allow".into(),
                            remember: None,
                            reason: None,
                        },
                        None,
                    )
                } else {
                    coordinator.respond_elicitation(
                        &"s".into(),
                        &"new".into(),
                        "answer".into(),
                        None,
                    )
                };
                assert!(!delivered, "p={p}");
            } else {
                // The register landed in the bucket the loop still drains, so
                // TS settles it like any same-Map reentrant registration.
                assert_eq!(settle_count("new"), 1, "p={p}, {events:?}");
                assert!(!registration.active(), "p={p}");
                assert_eq!(pending_count(&coordinator), 0, "p={p}");
                assert_eq!(
                    coordinator.reconnect_port().snapshot(&"s".into()).len(),
                    0,
                    "p={p}"
                );
            }
        }
    }
}

#[test]
fn interaction_injected_options_share_the_supplied_components() {
    // The InteractionOptions Some-branches must wire the caller's components,
    // not silent defaults: a default port, bus or registry behaves identically
    // through the coordinator while leaving every external handle empty.
    let port = InteractionReconnectPort::default();
    let permission_bus = GatewayPermissionBus::new(port.clone());
    let elicitation_bus = GatewayElicitationBus::new(port.clone());
    let grants = Arc::new(GatewaySessionPermissionRuleSetRegistry::default());
    let coordinator = GatewayInteractionCoordinator::new(InteractionOptions {
        reconnect_port: Some(port.clone()),
        permission_bus: Some(permission_bus.clone()),
        elicitation_bus: Some(elicitation_bus.clone()),
        permission_grants: Some(grants.clone()),
        on_elicitation_delivered: None,
    });
    let entry = |id: &str| PendingEntry {
        request_id: id.into(),
        tool_call_id: id.into(),
        tool_name: "fixture".into(),
        resolve: Arc::new(|_| ()),
        reject: Arc::new(|_| ()),
    };
    permission_bus
        .register(&"s".into(), entry("p"), None)
        .unwrap();
    elicitation_bus
        .register(&"s".into(), entry("q"), None)
        .unwrap();
    assert_eq!(port.snapshot(&"s".into()).len(), 2);
    assert_eq!(coordinator.reconnect_port().snapshot(&"s".into()).len(), 2);
    let binding = InteractionBinding {
        connection_id: "a".into(),
        generation: 1,
    };
    coordinator
        .reconnect(&"s".into(), Some(binding.clone()), None)
        .unwrap();
    assert_eq!(port.current_binding(&"s".into()), Some(binding));
    let decided = coordinator.decide_permission(
        &"s".into(),
        PermissionDecision {
            request_id: "p".into(),
            decision: "allow".into(),
            remember: None,
            reason: None,
        },
        None,
    );
    assert_eq!(
        permission_bus.pending_count(&"s".into()),
        0,
        "decide delivered"
    );
    assert!(decided);
    let answered = coordinator.respond_elicitation(&"s".into(), &"q".into(), "answer".into(), None);
    assert_eq!(
        elicitation_bus.pending_count(&"s".into()),
        0,
        "answer delivered"
    );
    assert!(answered);
    let granted = coordinator.grant_session_permission(&"s".into(), &"Read".into());
    assert_eq!(
        granted.get("granted").and_then(JsValue::as_bool),
        Some(true)
    );
    assert_eq!(grants.allow_rules(&"s".into()).len(), 1);
    assert_eq!(coordinator.session_allow_rules(&"s".into()).len(), 1);
    coordinator.dispose(&"gone".into());
    assert_eq!(grants.allow_rules(&"s".into()).len(), 0);
    assert_eq!(port.snapshot(&"s".into()).len(), 0);
}

#[test]
fn interaction_concurrent_response_timeout_has_one_winner() {
    use std::sync::Barrier;
    for p in [false, true] {
        for _ in 0..32 {
            let harness = Harness::new();
            harness
                .register(&object([
                    ("session", "s".into()),
                    ("id", "r".into()),
                    ("kind", if p { "permission" } else { "question" }.into()),
                ]))
                .unwrap();
            let barrier = Arc::new(Barrier::new(3));
            std::thread::scope(|scope| {
                for event in ["resolve", "reject"] {
                    let harness = harness.clone();
                    let barrier = barrier.clone();
                    scope.spawn(move || {
                        barrier.wait();
                        if let Some(entry) = harness.consume(&"s".into(), &"r".into(), p, None) {
                            if event == "resolve" {
                                (entry.resolve)("answer".into());
                            } else {
                                (entry.reject)("request_timeout".into());
                            }
                        }
                    });
                }
                barrier.wait();
            });
            assert_eq!(harness.events.lock().unwrap().len(), 1);
            assert!(!harness.registration(&"r".into()).active());
            assert!(
                harness
                    .coordinator
                    .reconnect_port()
                    .snapshot(&"s".into())
                    .is_empty()
            );
        }
    }
}

#[test]
#[ignore = "requires Node 22 and PDRS_BASELINE; generates the real TS oracle twice"]
fn interaction_regenerate_twice_identically() {
    baseline::regenerates_identically("interaction-bus");
    baseline::regenerates_identically("interaction-bus");
}

#[test]
fn interaction_reject_cross_thread_response_and_bulk_have_one_winner() {
    use std::sync::{atomic::AtomicUsize, mpsc};
    for p in [false, true] {
        for challenger in ["response", "reject", "close"] {
            let events = Arc::new(Mutex::new(Vec::new()));
            let delivered = Arc::new(AtomicUsize::new(0));
            let observed = delivered.clone();
            let coordinator = Arc::new(GatewayInteractionCoordinator::new(InteractionOptions {
                on_elicitation_delivered: Some(Arc::new(move |_, _| {
                    observed.fetch_add(1, Ordering::SeqCst);
                })),
                ..Default::default()
            }));
            let (entered_tx, entered_rx) = mpsc::channel();
            let (resume_tx, resume_rx) = mpsc::channel();
            let resume_rx = Arc::new(Mutex::new(resume_rx));
            let mut registrations = Vec::new();
            let paused = Arc::new(AtomicBool::new(false));
            for id in ["first", "last"] {
                let resolved = events.clone();
                let rejected = events.clone();
                let entered_tx = entered_tx.clone();
                let resume_rx = resume_rx.clone();
                let paused = paused.clone();
                let entry = PendingEntry {
                    request_id: id.into(),
                    tool_call_id: id.into(),
                    tool_name: "fixture".into(),
                    resolve: Arc::new(move |_| resolved.lock().unwrap().push((id, "resolve"))),
                    reject: Arc::new(move |_| {
                        rejected.lock().unwrap().push((id, "reject"));
                        // Response case reproduces Grok exactly: first has been
                        // rejected and remains in the Map while last is paused.
                        // Bulk cases pause first so the other thread can also
                        // try to claim an as-yet unvisited entry.
                        let pause = if challenger == "response" {
                            "last"
                        } else {
                            "first"
                        };
                        if id == pause && !paused.swap(true, Ordering::SeqCst) {
                            entered_tx.send(()).unwrap();
                            resume_rx.lock().unwrap().recv().unwrap();
                        }
                    }),
                };
                registrations.push(
                    if p {
                        coordinator
                            .permission_bus()
                            .register(&"s".into(), entry, None)
                    } else {
                        coordinator
                            .elicitation_bus()
                            .register(&"s".into(), entry, None)
                    }
                    .unwrap(),
                );
            }
            let rejecting = coordinator.clone();
            let worker = std::thread::spawn(move || {
                if p {
                    rejecting
                        .permission_bus()
                        .reject_session(&"s".into(), &"closed".into());
                } else {
                    rejecting
                        .elicitation_bus()
                        .reject_session(&"s".into(), &"closed".into());
                }
            });
            entered_rx.recv().unwrap();
            let delivered_response = match challenger {
                "response" => {
                    if p {
                        coordinator.decide_permission(
                            &"s".into(),
                            PermissionDecision {
                                request_id: "first".into(),
                                decision: "allow".into(),
                                remember: None,
                                reason: None,
                            },
                            None,
                        )
                    } else {
                        coordinator.respond_elicitation(
                            &"s".into(),
                            &"first".into(),
                            "answer".into(),
                            None,
                        )
                    }
                }
                "reject" => {
                    if p {
                        coordinator
                            .permission_bus()
                            .reject_session(&"s".into(), &"other".into());
                    } else {
                        coordinator
                            .elicitation_bus()
                            .reject_session(&"s".into(), &"other".into());
                    }
                    false
                }
                "close" => {
                    coordinator.close_session(&"s".into(), &"other".into());
                    false
                }
                _ => unreachable!(),
            };
            resume_tx.send(()).unwrap();
            worker.join().unwrap();
            assert!(!delivered_response, "late response: p={p}, {challenger}");
            assert_eq!(delivered.load(Ordering::SeqCst), 0);
            let events = events.lock().unwrap();
            for id in ["first", "last"] {
                assert_eq!(
                    events.iter().filter(|(key, _)| *key == id).count(),
                    1,
                    "one settlement for {id}: p={p}, {challenger}, {events:?}"
                );
            }
            assert_eq!(events.len(), 2);
            assert!(registrations.iter().all(|r| !r.active()));
            assert!(
                coordinator
                    .reconnect_port()
                    .snapshot(&"s".into())
                    .is_empty()
            );
            assert_eq!(coordinator.permission_bus().pending_count(&"s".into()), 0);
            assert_eq!(coordinator.elicitation_bus().pending_count(&"s".into()), 0);
        }
    }
}
