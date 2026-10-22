use super::*;
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
        Arc::new_cyclic(|weak: &Weak<Self>| {
            let weak = weak.clone();
            Self {
                coordinator: GatewayInteractionCoordinator::new(InteractionOptions {
                    on_elicitation_delivered: Some(Arc::new(move |session, id| {
                        weak.upgrade().unwrap().events.lock().unwrap().push(object([
                            ("event", "delivered".into()),
                            ("session", JsValue::String(session.clone())),
                            ("id", JsValue::String(id.clone())),
                        ]));
                    })),
                    ..Default::default()
                }),
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
        let registration = if is_permission {
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
    assert_eq!(scenarios.len(), 35, "recorded TS scenario cardinality");
    assert_eq!(
        scenarios
            .iter()
            .map(|s| array(s.get("steps").unwrap()).len())
            .sum::<usize>(),
        227,
        "recorded TS step cardinality"
    );
    for scenario in scenarios {
        let harness = Harness::new();
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
