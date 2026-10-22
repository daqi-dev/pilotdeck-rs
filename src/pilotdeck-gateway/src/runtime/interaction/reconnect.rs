use super::*;

/// UTF-16 identity, independently constructible without issuing a WS connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InteractionBinding {
    pub connection_id: JsString,
    pub generation: u64,
}
impl InteractionBinding {
    pub fn from_value(value: &JsValue) -> Result<Self, JsString> {
        let id = value
            .get("connectionId")
            .and_then(JsValue::as_string)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| {
                JsString::from("Interaction connectionId must be a non-empty string.")
            })?;
        let generation = value
            .get("generation")
            .and_then(JsValue::as_number)
            .filter(|n| {
                n.is_finite() && *n >= 0.0 && n.fract() == 0.0 && *n <= 9_007_199_254_740_991.0
            })
            .ok_or_else(|| {
                JsString::from(
                    "Interaction connection generation must be a non-negative safe integer.",
                )
            })?;
        Ok(Self {
            connection_id: id.clone(),
            generation: generation as u64,
        })
    }
    fn validate(&self) -> Result<(), JsString> {
        Self::from_value(&self.to_value()).map(|_| ())
    }
    pub fn to_value(&self) -> JsValue {
        object([
            ("connectionId", JsValue::String(self.connection_id.clone())),
            ("generation", JsValue::Number(self.generation as f64)),
        ])
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InteractionKind {
    Permission,
    Question,
}
impl InteractionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Permission => "permission",
            Self::Question => "question",
        }
    }
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Permission => "Permission",
            Self::Question => "Elicitation",
        }
    }
}

/// Full replay provider for these buses. It shares their lock, so checking a
/// binding and consuming an entry cannot race with disconnect/reconnect.
#[derive(Clone, Default)]
pub struct InteractionReconnectPort {
    pub(super) state: Shared,
}
impl InteractionReconnectPort {
    pub fn current_binding(&self, owner: &JsString) -> Option<InteractionBinding> {
        current(&self.state.lock().unwrap(), owner)
    }
    pub fn reconnect(
        &self,
        owner: &JsString,
        next: InteractionBinding,
        previous: Option<&InteractionBinding>,
    ) -> Result<JsValue, JsString> {
        next.validate()?;
        let mut state = self.state.lock().unwrap();
        let binding = current(&state, owner);
        let last = binding.clone().or_else(|| lookup(&state.last, owner));
        if binding.as_ref() == Some(&next) && previous.is_none_or(|p| Some(p) == binding.as_ref()) {
            return Ok(result(
                "reconnected",
                binding.as_ref(),
                snapshot(&state, owner, None),
            ));
        }
        if last.as_ref().is_some_and(|last| previous != Some(last))
            || (last.is_none() && previous.is_some())
            || last
                .as_ref()
                .is_some_and(|last| next.generation <= last.generation)
        {
            return Ok(result("stale_binding", last.as_ref(), vec![]));
        }
        assign(&mut state.bindings, owner, next.clone());
        assign(&mut state.last, owner, next.clone());
        for request in &mut state.requests {
            if &request.owner == owner {
                request.binding = Some(next.clone());
            }
        }
        let requests = snapshot(&state, owner, None);
        let outcome = if last.is_some() {
            "reconnected"
        } else if requests.is_empty() {
            "no_pending"
        } else {
            "initial"
        };
        Ok(result(outcome, Some(&next), requests))
    }
    pub fn disconnect(
        &self,
        owner: &JsString,
        binding: &InteractionBinding,
    ) -> Result<bool, JsString> {
        binding.validate()?;
        let mut state = self.state.lock().unwrap();
        if current(&state, owner).as_ref() != Some(binding) {
            return Ok(false);
        }
        state.bindings.retain(|(key, _)| key != owner);
        for request in &mut state.requests {
            if &request.owner == owner {
                request.binding = None;
            }
        }
        if !state.requests.iter().any(|request| &request.owner == owner) {
            state.last.retain(|(key, _)| key != owner);
        }
        Ok(true)
    }
    pub fn snapshot(&self, owner: &JsString) -> Vec<JsValue> {
        snapshot(&self.state.lock().unwrap(), owner, None)
    }
    pub fn dispose_owner(&self, owner: &JsString) {
        let mut state = self.state.lock().unwrap();
        state.requests.retain(|request| &request.owner != owner);
        state.bindings.retain(|(key, _)| key != owner);
        state.last.retain(|(key, _)| key != owner);
    }
    /// Like TS this clears replay state, not the buses' pending callbacks.
    pub fn dispose(&self) {
        let mut state = self.state.lock().unwrap();
        state.requests.clear();
        state.bindings.clear();
        state.last.clear();
    }
}
fn lookup(
    bindings: &[(JsString, InteractionBinding)],
    owner: &JsString,
) -> Option<InteractionBinding> {
    bindings
        .iter()
        .find(|(key, _)| key == owner)
        .map(|(_, binding)| binding.clone())
}
pub(super) fn current(state: &State, owner: &JsString) -> Option<InteractionBinding> {
    lookup(&state.bindings, owner)
}
fn assign(
    bindings: &mut Vec<(JsString, InteractionBinding)>,
    owner: &JsString,
    binding: InteractionBinding,
) {
    if let Some((_, value)) = bindings.iter_mut().find(|(key, _)| key == owner) {
        *value = binding;
    } else {
        bindings.push((owner.clone(), binding));
    }
}
pub(super) fn result(
    outcome: &str,
    binding: Option<&InteractionBinding>,
    requests: Vec<JsValue>,
) -> JsValue {
    let mut value = object([("outcome", outcome.into())]);
    if let Some(binding) = binding {
        set(&mut value, "binding", binding.to_value());
    }
    set(&mut value, "requests", JsValue::Array(requests));
    value
}
pub(super) fn snapshot(
    state: &State,
    owner: &JsString,
    kind: Option<InteractionKind>,
) -> Vec<JsValue> {
    state
        .requests
        .iter()
        .filter(|r| &r.owner == owner && kind.is_none_or(|kind| r.kind == kind))
        .map(|r| {
            let mut value = object([
                ("ownerId", JsValue::String(r.owner.clone())),
                ("requestId", JsValue::String(r.entry.request_id.clone())),
                ("kind", r.kind.as_str().into()),
                ("toolCallId", JsValue::String(r.entry.tool_call_id.clone())),
                ("toolName", JsValue::String(r.entry.tool_name.clone())),
            ]);
            if let Some(payload) = &r.payload {
                set(&mut value, "payload", payload.clone());
            }
            if let Some(binding) = &r.binding {
                set(&mut value, "binding", binding.to_value());
            }
            value
        })
        .collect()
}
pub(super) fn remove(state: &mut State, token: u64, owner: &JsString) {
    state.requests.retain(|r| r.token != token);
    if current(state, owner).is_none() && !state.requests.iter().any(|r| &r.owner == owner) {
        state.last.retain(|(key, _)| key != owner);
    }
}
pub(super) fn is_current(
    state: &State,
    owner: &JsString,
    kind: InteractionKind,
    request: &JsString,
    binding: &InteractionBinding,
) -> bool {
    current(state, owner).as_ref() == Some(binding)
        && state.requests.iter().any(|r| {
            &r.owner == owner
                && r.kind == kind
                && &r.entry.request_id == request
                && r.binding.as_ref() == Some(binding)
        })
}

/// Replay accepts boundary values rather than arbitrary host objects. Undefined
/// leaves are omitted in objects and become null in arrays; nonfinite numbers
/// become null. JsValue has no host references, Date, BigInt or cyclic objects.
fn replay(value: &JsValue) -> JsValue {
    // Iterative traversal avoids adding a Rust stack limit for wire payloads.
    enum Work<'a> {
        Visit(&'a JsValue),
        Array(usize),
        Object(Vec<JsString>),
    }
    let mut work = vec![Work::Visit(value)];
    let mut values = Vec::new();
    while let Some(task) = work.pop() {
        match task {
            Work::Visit(JsValue::Array(items)) => {
                work.push(Work::Array(items.len()));
                for item in items.iter().rev() {
                    work.push(Work::Visit(item));
                }
            }
            Work::Visit(JsValue::Object(object)) => {
                let entries: Vec<_> = object
                    .own_entries()
                    .into_iter()
                    .filter(|(key, v)| !key.equals_str("__proto__") && !v.is_undefined())
                    .collect();
                work.push(Work::Object(
                    entries.iter().map(|(key, _)| (*key).clone()).collect(),
                ));
                for (_, value) in entries.into_iter().rev() {
                    work.push(Work::Visit(value));
                }
            }
            Work::Visit(JsValue::Undefined) => values.push(JsValue::Null),
            Work::Visit(JsValue::Number(n)) if !n.is_finite() => values.push(JsValue::Null),
            Work::Visit(value) => values.push(value.clone()),
            Work::Array(count) => {
                let items = values.split_off(values.len() - count);
                values.push(JsValue::Array(items));
            }
            Work::Object(keys) => {
                let children = values.split_off(values.len() - keys.len());
                let mut object = JsObject::new();
                for (key, child) in keys.into_iter().zip(children) {
                    object.insert(key, child);
                }
                values.push(JsValue::Object(object));
            }
        }
    }
    values.pop().unwrap()
}
pub(super) fn replay_payload(value: Option<JsValue>) -> Option<JsValue> {
    value
        .filter(|v| !v.is_undefined())
        .map(|value| replay(&value))
}
