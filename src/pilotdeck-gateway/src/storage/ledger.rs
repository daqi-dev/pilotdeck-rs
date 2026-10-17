use super::{
    Result, invalid,
    projection::{normalize_seed, serialize_seed},
};
use crate::metadata::object;
use pilotdeck_protocol::{
    JsObject, JsString, JsValue,
    module::{ModuleBinding, ModuleOutcome},
    stringify_json,
};
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OperationIdentity {
    pub run_id: JsString,
    pub operation_id: JsString,
    pub request_id: JsString,
    pub session_id: JsString,
    pub turn_id: JsString,
    pub binding: ModuleBinding,
    pub idempotency_key: Option<JsString>,
}
impl OperationIdentity {
    fn durable_eq(&self, other: &Self) -> bool {
        self.run_id == other.run_id
            && self.operation_id == other.operation_id
            && self.session_id == other.session_id
            && self.turn_id == other.turn_id
            && self.idempotency_key == other.idempotency_key
    }
    fn event(&self, kind: &str) -> JsObject {
        let mut event = JsObject::new();
        event.insert("type".into(), kind.into());
        for (key, value) in [
            ("runId", &self.run_id),
            ("operationId", &self.operation_id),
            ("requestId", &self.request_id),
        ] {
            event.insert(key.into(), JsValue::String(value.clone()));
        }
        event.insert(
            "binding".into(),
            object([
                (
                    "moduleInstanceId",
                    JsValue::String(self.binding.module_instance_id.clone()),
                ),
                (
                    "connectionGeneration",
                    JsValue::String(self.binding.connection_generation.clone()),
                ),
            ]),
        );
        event
    }
    fn append_key(&self, event: &mut JsObject) {
        optional_string(event, "idempotencyKey", self.idempotency_key.as_ref());
    }
    fn from_entry(entry: &JsValue) -> Result<Self> {
        Ok(Self {
            run_id: string(entry, "runId")?,
            operation_id: string(entry, "operationId")?,
            request_id: string(entry, "requestId")?,
            session_id: string(entry, "sessionId")?,
            turn_id: string(entry, "turnId")?,
            binding: ModuleBinding {
                module_instance_id: string(field(entry, "binding"), "moduleInstanceId")?,
                connection_generation: string(field(entry, "binding"), "connectionGeneration")?,
            },
            idempotency_key: nonempty_string(entry, "idempotencyKey"),
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct OperationResolution {
    pub outcome: ModuleOutcome,
    pub result: JsValue,
    pub messages: Vec<JsValue>,
    /// Host-neutral file-state projection; map fields are plain JSON objects.
    pub seed_state: Option<JsValue>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct KnownTerminal {
    pub identity: OperationIdentity,
    pub stream_id: Option<JsString>,
    pub last_applied_sequence: f64,
    pub resolution: OperationResolution,
    pub code: Option<JsString>,
    /// None is absent; Some(Null) is an explicit JSON null.
    pub error: Option<JsValue>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct UnknownTerminal {
    pub identity: OperationIdentity,
    pub stream_id: JsString,
    /// TS does not validate unknown sequences. None preserves JSON null after
    /// a TS non-finite number was persisted by JSON.stringify.
    pub last_applied_sequence: Option<f64>,
    pub code: Option<JsString>,
    pub error: Option<JsValue>,
}
#[derive(Clone, Debug, PartialEq)]
pub enum OperationRecovery {
    Incomplete,
    Terminal(OperationResolution),
    ResultUnknown(UnknownTerminal),
}
#[derive(Clone, Debug)]
enum Terminal {
    Known(Box<KnownTerminal>),
    Unknown(Box<UnknownTerminal>),
}
#[derive(Clone, Debug)]
struct Record {
    identity: OperationIdentity,
    stream_id: Option<JsString>,
    terminal: Option<Terminal>,
}

/// A synchronous commit boundary. Returning Err must not report a committed event.
/// Mutable access serializes writes; callers using async code should own the ledger
/// on a blocking worker rather than perform filesystem IO on the executor thread.
pub trait TranscriptSink {
    fn append(&mut self, session_id: &JsString, turn_id: &JsString, event: JsObject) -> Result<()>;
}

pub struct SessionOperationLedger<S> {
    session_id: JsString,
    sink: S,
    records: HashMap<JsString, Record>,
}
impl<S: TranscriptSink> SessionOperationLedger<S> {
    pub fn new(session_id: JsString, sink: S, restored_entries: &[JsValue]) -> Result<Self> {
        let mut ledger = Self {
            session_id,
            sink,
            records: HashMap::new(),
        };
        for entry in restored_entries {
            ledger.restore(entry)?;
        }
        Ok(ledger)
    }
    pub fn sink(&self) -> &S {
        &self.sink
    }
    #[cfg(test)]
    pub(super) fn sink_mut(&mut self) -> &mut S {
        &mut self.sink
    }
    pub fn recover(&self, input: &OperationIdentity) -> Result<Option<OperationRecovery>> {
        self.assert_session(input)?;
        Ok(self
            .records
            .get(&input.operation_id)
            .filter(|r| r.identity.durable_eq(input))
            .map(|r| match &r.terminal {
                None => OperationRecovery::Incomplete,
                Some(Terminal::Known(t)) => OperationRecovery::Terminal(t.resolution.clone()),
                Some(Terminal::Unknown(t)) => OperationRecovery::ResultUnknown(t.as_ref().clone()),
            }))
    }
    pub fn start(&mut self, input: &OperationIdentity) -> Result<()> {
        self.assert_session(input)?;
        if let Some(record) = self.records.get(&input.operation_id) {
            return if record.identity == *input {
                Ok(())
            } else {
                Err(invalid(
                    "AgentLoop operation conflicts with its durable identity.",
                ))
            };
        }
        let mut event = input.event("agent_loop_operation_started");
        input.append_key(&mut event);
        self.sink.append(&input.session_id, &input.turn_id, event)?;
        self.records.insert(
            input.operation_id.clone(),
            Record {
                identity: input.clone(),
                stream_id: None,
                terminal: None,
            },
        );
        Ok(())
    }
    pub fn accept(&mut self, input: &OperationIdentity, stream_id: &JsString) -> Result<()> {
        self.assert_session(input)?;
        let record = self.require_active(input, None)?;
        if let Some(stream) = record.stream_id.as_ref().filter(|s| !s.is_empty()) {
            return if stream == stream_id {
                Ok(())
            } else {
                Err(invalid("AgentLoop operation changed its stream identity."))
            };
        }
        let mut event = input.event("agent_loop_operation_accepted");
        event.insert("streamId".into(), JsValue::String(stream_id.clone()));
        input.append_key(&mut event);
        self.sink.append(&input.session_id, &input.turn_id, event)?;
        self.records.get_mut(&input.operation_id).unwrap().stream_id = Some(stream_id.clone());
        Ok(())
    }
    pub fn terminal(&mut self, input: &KnownTerminal) -> Result<()> {
        let id = &input.identity;
        self.assert_session(id)?;
        if !input.last_applied_sequence.is_finite()
            || input.last_applied_sequence.fract() != 0.0
            || input.last_applied_sequence < -1.0
        {
            return Err(invalid("AgentLoop operation terminal sequence is invalid."));
        }
        if field(&input.resolution.result, "sessionId").as_string() != Some(&id.session_id)
            || field(&input.resolution.result, "turnId").as_string() != Some(&id.turn_id)
        {
            return Err(invalid(
                "AgentLoop operation terminal result does not match its session or turn.",
            ));
        }
        if input.resolution.outcome == ModuleOutcome::ResultUnknown {
            return Err(invalid("Known terminal requires a known outcome."));
        }
        let record = self.require_active(id, input.stream_id.as_ref())?;
        if let Some(Terminal::Known(existing)) = &record.terminal {
            return if same_known(existing, input)? {
                Ok(())
            } else {
                Err(invalid(
                    "AgentLoop operation already has a different durable terminal.",
                ))
            };
        }
        let mut stable = input.clone();
        stable.resolution.seed_state = normalize_seed(input.resolution.seed_state.as_ref())?;
        let mut event = id.event("agent_loop_operation_terminal");
        optional_string(&mut event, "streamId", input.stream_id.as_ref());
        event.insert(
            "lastAppliedSequence".into(),
            JsValue::Number(input.last_applied_sequence),
        );
        event.insert(
            "outcome".into(),
            outcome_name(input.resolution.outcome).into(),
        );
        event.insert("result".into(), input.resolution.result.clone());
        event.insert(
            "messages".into(),
            JsValue::Array(input.resolution.messages.clone()),
        );
        if let Some(seed) = serialize_seed(input.resolution.seed_state.as_ref())? {
            event.insert("seedState".into(), seed);
        }
        id.append_key(&mut event);
        details(&mut event, input.code.as_ref(), input.error.as_ref());
        self.sink.append(&id.session_id, &id.turn_id, event)?;
        let record = self.records.get_mut(&id.operation_id).unwrap();
        if record.stream_id.is_none() {
            record.stream_id = input.stream_id.clone();
        }
        record.terminal = Some(Terminal::Known(Box::new(stable)));
        Ok(())
    }
    pub fn result_unknown(&mut self, input: &UnknownTerminal) -> Result<()> {
        let id = &input.identity;
        self.assert_session(id)?;
        let record = self.require_active(id, Some(&input.stream_id))?;
        if let Some(terminal) = &record.terminal {
            return match terminal {
                Terminal::Known(_) => Ok(()),
                Terminal::Unknown(existing) if same_unknown(existing, input)? => Ok(()),
                _ => Err(invalid(
                    "AgentLoop operation already has a different unresolved terminal.",
                )),
            };
        }
        let mut event = id.event("agent_loop_operation_terminal");
        event.insert("streamId".into(), JsValue::String(input.stream_id.clone()));
        event.insert(
            "lastAppliedSequence".into(),
            input
                .last_applied_sequence
                .map(JsValue::Number)
                .unwrap_or(JsValue::Null),
        );
        event.insert("outcome".into(), "result_unknown".into());
        id.append_key(&mut event);
        details(&mut event, input.code.as_ref(), input.error.as_ref());
        self.sink.append(&id.session_id, &id.turn_id, event)?;
        // TS deliberately does not set record.streamId in the online unknown path.
        self.records.get_mut(&id.operation_id).unwrap().terminal =
            Some(Terminal::Unknown(Box::new(input.clone())));
        Ok(())
    }
    pub fn reconcile(&self, input: &UnknownTerminal) -> Result<Option<OperationResolution>> {
        self.assert_session(&input.identity)?;
        Ok(self
            .records
            .get(&input.identity.operation_id)
            .filter(|r| {
                r.identity == input.identity && r.stream_id.as_ref() == Some(&input.stream_id)
            })
            .and_then(|r| match &r.terminal {
                Some(Terminal::Known(t)) => Some(t.resolution.clone()),
                _ => None,
            }))
    }
    fn assert_session(&self, input: &OperationIdentity) -> Result<()> {
        if input.session_id == self.session_id {
            Ok(())
        } else {
            Err(invalid(
                "AgentLoop operation ledger cannot write another session.",
            ))
        }
    }
    fn require_active(
        &self,
        input: &OperationIdentity,
        stream: Option<&JsString>,
    ) -> Result<&Record> {
        let record = self
            .records
            .get(&input.operation_id)
            .filter(|r| r.identity == *input)
            .ok_or_else(|| invalid("AgentLoop operation is not durably started."))?;
        if let (Some(a), Some(b)) = (
            stream.filter(|s| !s.is_empty()),
            record.stream_id.as_ref().filter(|s| !s.is_empty()),
        ) && a != b
        {
            return Err(invalid(
                "AgentLoop operation stream does not match its durable binding.",
            ));
        }
        Ok(record)
    }
    fn restore(&mut self, entry: &JsValue) -> Result<()> {
        if field(entry, "sessionId").as_string() != Some(&self.session_id) {
            return Ok(());
        }
        let kind = field(entry, "type").as_string();
        if ![
            "agent_loop_operation_started",
            "agent_loop_operation_accepted",
            "agent_loop_operation_terminal",
        ]
        .iter()
        .any(|s| kind.is_some_and(|v| v.equals_str(s)))
        {
            return Ok(());
        }
        let identity = OperationIdentity::from_entry(entry)?;
        if kind.unwrap().equals_str("agent_loop_operation_started") {
            if let Some(existing) = self.records.get(&identity.operation_id)
                && existing.identity != identity
            {
                return Err(invalid(
                    "Restored AgentLoop operation has conflicting identity.",
                ));
            }
            self.records
                .entry(identity.operation_id.clone())
                .or_insert(Record {
                    identity,
                    stream_id: None,
                    terminal: None,
                });
            return Ok(());
        }
        let record = self
            .records
            .get_mut(&identity.operation_id)
            .filter(|r| r.identity == identity)
            .ok_or_else(|| {
                invalid(
                    "Restored AgentLoop operation was accepted or terminated before it started.",
                )
            })?;
        let stream = field(entry, "streamId").as_string().cloned();
        let accepted = kind.unwrap().equals_str("agent_loop_operation_accepted");
        if let Some(old) = record.stream_id.as_ref().filter(|s| !s.is_empty())
            && (accepted || stream.as_ref().is_some_and(|s| !s.is_empty()))
            && stream.as_ref() != Some(old)
        {
            return Err(invalid(
                "Restored AgentLoop operation has conflicting stream ids.",
            ));
        }
        if accepted {
            record.stream_id = Some(string(entry, "streamId")?);
            return Ok(());
        }
        let sequence = field(entry, "lastAppliedSequence");
        let code = nonempty_string(entry, "code");
        let error = entry.get("error").cloned();
        let terminal = if field(entry, "outcome")
            .as_string()
            .is_some_and(|s| s.equals_str("result_unknown"))
        {
            Terminal::Unknown(Box::new(UnknownTerminal {
                identity,
                stream_id: stream.clone().filter(|s| !s.is_empty()).ok_or_else(|| {
                    invalid("Restored AgentLoop operation result_unknown lacks a stream id.")
                })?,
                last_applied_sequence: match sequence {
                    JsValue::Number(n) => Some(*n),
                    JsValue::Null => None,
                    _ => return Err(invalid("Invalid unknown terminal sequence.")),
                },
                code,
                error,
            }))
        } else {
            let result = field(entry, "result");
            let JsValue::Array(messages) = field(entry, "messages") else {
                return Err(invalid(
                    "Restored AgentLoop operation terminal lacks a result projection.",
                ));
            };
            if !truthy(result) {
                return Err(invalid(
                    "Restored AgentLoop operation terminal lacks a result projection.",
                ));
            }
            Terminal::Known(Box::new(KnownTerminal {
                identity,
                stream_id: stream.clone().filter(|s| !s.is_empty()),
                last_applied_sequence: sequence
                    .as_number()
                    .ok_or_else(|| invalid("Missing terminal sequence."))?,
                resolution: OperationResolution {
                    outcome: parse_outcome(field(entry, "outcome"))?,
                    result: result.clone(),
                    messages: messages.clone(),
                    seed_state: normalize_seed(entry.get("seedState"))?,
                },
                code,
                error,
            }))
        };
        if let (Some(Terminal::Known(a)), Terminal::Known(b)) = (&record.terminal, &terminal)
            && !same_known(a, b)?
        {
            return Err(invalid(
                "Restored AgentLoop operation has conflicting durable terminals.",
            ));
        }
        if record.stream_id.is_none() {
            record.stream_id = stream;
        }
        // Restore is intentionally not routed through the online terminal methods:
        // historical unknown after known, and differing unknowns, overwrite in TS.
        record.terminal = Some(terminal);
        Ok(())
    }
}
fn same_known(a: &KnownTerminal, b: &KnownTerminal) -> Result<bool> {
    Ok(a.resolution.outcome == b.resolution.outcome
        && a.stream_id == b.stream_id
        && a.last_applied_sequence == b.last_applied_sequence
        && stringify_json(&a.resolution.result)? == stringify_json(&b.resolution.result)?
        && stringify_json(&JsValue::Array(a.resolution.messages.clone()))?
            == stringify_json(&JsValue::Array(b.resolution.messages.clone()))?
        && seed_json(a.resolution.seed_state.as_ref())?
            == seed_json(b.resolution.seed_state.as_ref())?)
}
fn seed_json(seed: Option<&JsValue>) -> Result<Option<String>> {
    serialize_seed(seed)?
        .as_ref()
        .map(stringify_json)
        .transpose()
        .map(|v| v.flatten())
        .map_err(Into::into)
}
fn same_unknown(a: &UnknownTerminal, b: &UnknownTerminal) -> Result<bool> {
    Ok(a.stream_id == b.stream_id
        && a.last_applied_sequence == b.last_applied_sequence
        && a.code == b.code
        && stringify_json(a.error.as_ref().unwrap_or(&JsValue::Undefined))?
            == stringify_json(b.error.as_ref().unwrap_or(&JsValue::Undefined))?)
}
fn details(event: &mut JsObject, code: Option<&JsString>, error: Option<&JsValue>) {
    optional_string(event, "code", code);
    if let Some(error) = error {
        event.insert("error".into(), error.clone());
    }
}
fn optional_string(event: &mut JsObject, key: &str, value: Option<&JsString>) {
    if let Some(value) = value.filter(|v| !v.is_empty()) {
        event.insert(key.into(), JsValue::String(value.clone()));
    }
}
fn outcome_name(outcome: ModuleOutcome) -> &'static str {
    match outcome {
        ModuleOutcome::Completed => "completed",
        ModuleOutcome::Failed => "failed",
        ModuleOutcome::Cancelled => "cancelled",
        ModuleOutcome::ResultUnknown => "result_unknown",
    }
}
fn parse_outcome(value: &JsValue) -> Result<ModuleOutcome> {
    [
        ModuleOutcome::Completed,
        ModuleOutcome::Failed,
        ModuleOutcome::Cancelled,
    ]
    .into_iter()
    .find(|o| {
        value
            .as_string()
            .is_some_and(|s| s.equals_str(outcome_name(*o)))
    })
    .ok_or_else(|| invalid("Invalid terminal outcome."))
}
pub(super) fn field<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    static UNDEFINED: JsValue = JsValue::Undefined;
    value.get(key).unwrap_or(&UNDEFINED)
}
fn string(value: &JsValue, key: &str) -> Result<JsString> {
    field(value, key)
        .as_string()
        .cloned()
        .ok_or_else(|| invalid(format!("Invalid operation field: {key}.")))
}
fn nonempty_string(value: &JsValue, key: &str) -> Option<JsString> {
    field(value, key)
        .as_string()
        .filter(|s| !s.is_empty())
        .cloned()
}
fn truthy(v: &JsValue) -> bool {
    match v {
        JsValue::Undefined | JsValue::Null => false,
        JsValue::Bool(b) => *b,
        JsValue::Number(n) => *n != 0.0 && !n.is_nan(),
        JsValue::String(s) => !s.is_empty(),
        _ => true,
    }
}
