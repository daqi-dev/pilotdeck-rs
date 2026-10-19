//! Session-scoped pending async hooks (TS `extension/hooks/execution/AsyncHookRegistry.ts`).
//!
//! This is deliberately distinct from [`crate::runtime::async_hooks`]: that
//! registry owns the Gateway's deferred-result protocol (run fences, steer
//! delivery, duplicate/expired outcomes). This registry only owns hooks the
//! session runtime awaits after a `command` hook emitted `{async:true}`; the
//! `invocationId` it may carry can later be registered with the Gateway.
use crate::hooks::protocol::HookOutput;
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::sync::{
    Arc, Mutex, Weak,
    atomic::{AtomicBool, Ordering},
};

#[derive(Clone, Debug, PartialEq)]
pub struct PendingAsyncHookInput {
    pub id: JsString,
    /// `Date#toISOString()` text for the dispatch start time.
    pub started_at: JsString,
    pub hook_name: JsString,
    pub hook_event: JsString,
    pub stdout: JsString,
    pub stderr: JsString,
    pub async_rewake: Option<bool>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AsyncHookCompletion {
    pub stdout: Option<JsString>,
    pub stderr: Option<JsString>,
    /// A completed async hook must resolve to a terminal sync output.
    pub output: Option<HookOutput>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PendingAsyncHook {
    pub id: JsString,
    pub generation: u64,
    pub started_at: JsString,
    pub hook_name: JsString,
    pub hook_event: JsString,
    pub stdout: JsString,
    pub stderr: JsString,
    pub response_delivered: bool,
    pub async_rewake: Option<bool>,
    pub completion: Option<Completed>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Completed {
    pub stdout: JsString,
    pub stderr: JsString,
    pub output: HookOutput,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PendingAsyncHookDescriptor {
    pub id: JsString,
    pub generation: u64,
    pub hook_name: JsString,
    pub hook_event: JsString,
    pub started_at: JsString,
}
impl PendingAsyncHookDescriptor {
    pub fn to_value(&self) -> JsValue {
        let mut object = JsObject::new();
        object.insert("id".into(), JsValue::String(self.id.clone()));
        object.insert("generation".into(), JsValue::Number(self.generation as f64));
        object.insert("hookName".into(), JsValue::String(self.hook_name.clone()));
        object.insert("hookEvent".into(), JsValue::String(self.hook_event.clone()));
        object.insert("startedAt".into(), JsValue::String(self.started_at.clone()));
        JsValue::Object(object)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct AsyncHookResponse {
    pub id: JsString,
    pub hook_name: JsString,
    pub hook_event: JsString,
    pub stdout: JsString,
    pub stderr: JsString,
    pub output: HookOutput,
    pub rewake: bool,
}
impl AsyncHookResponse {
    pub fn to_value(&self) -> JsValue {
        let mut object = JsObject::new();
        object.insert("id".into(), JsValue::String(self.id.clone()));
        object.insert("hookName".into(), JsValue::String(self.hook_name.clone()));
        object.insert("hookEvent".into(), JsValue::String(self.hook_event.clone()));
        object.insert("stdout".into(), JsValue::String(self.stdout.clone()));
        object.insert("stderr".into(), JsValue::String(self.stderr.clone()));
        object.insert(
            "output".into(),
            super::exec::hook_output_to_value(&self.output),
        );
        object.insert("rewake".into(), JsValue::Bool(self.rewake));
        JsValue::Object(object)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PendingState {
    Active,
    Draining,
    Disposed,
}
impl PendingState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Draining => "draining",
            Self::Disposed => "disposed",
        }
    }
}

struct State {
    // Vec preserves Map insertion order for list/collectResponses; Map.set on
    // an existing id replaces the value but keeps its position.
    hooks: Vec<PendingAsyncHook>,
    state: PendingState,
    generation: u64,
}
impl Default for State {
    fn default() -> Self {
        Self {
            hooks: Vec::new(),
            state: PendingState::Active,
            generation: 0,
        }
    }
}
#[derive(Default)]
struct Inner {
    state: Mutex<State>,
}

/// Registration handle returned by [`PendingAsyncHooks::register`].
#[derive(Clone)]
pub struct Registration {
    inner: Weak<Inner>,
    id: JsString,
    generation: u64,
    active: Arc<AtomicBool>,
}
impl Registration {
    pub fn id(&self) -> &JsString {
        &self.id
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    /// TS: `active && registry.hooks.get(hook.id) === hook`. The generation is
    /// the identity: it advances on every registration.
    pub fn active(&self) -> bool {
        if !self.active.load(Ordering::SeqCst) {
            return false;
        }
        self.inner.upgrade().is_some_and(|inner| {
            let state = inner.state.lock().unwrap();
            state
                .hooks
                .iter()
                .any(|hook| hook.id == self.id && hook.generation == self.generation)
        })
    }
    pub fn dispose(&self) {
        if !self.active.swap(false, Ordering::SeqCst) {
            return;
        }
        if let Some(inner) = self.inner.upgrade() {
            inner
                .state
                .lock()
                .unwrap()
                .hooks
                .retain(|hook| !(hook.id == self.id && hook.generation == self.generation));
        }
    }
}

#[derive(Clone, Default)]
pub struct PendingAsyncHooks(Arc<Inner>);
impl PendingAsyncHooks {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn state(&self) -> PendingState {
        self.0.state.lock().unwrap().state
    }
    /// TS throws while the registry is not active; the caller decides whether
    /// that aborts the whole dispatch (it does inside `HookRuntime.run`).
    pub fn register(&self, input: PendingAsyncHookInput) -> Result<Registration, JsString> {
        let mut state = self.0.state.lock().unwrap();
        if state.state != PendingState::Active {
            return Err(JsString::from(format!(
                "Cannot register an async hook; registry is {}.",
                state.state.as_str()
            )));
        }
        state.generation += 1;
        let hook = PendingAsyncHook {
            id: input.id,
            generation: state.generation,
            started_at: input.started_at,
            hook_name: input.hook_name,
            hook_event: input.hook_event,
            stdout: input.stdout,
            stderr: input.stderr,
            response_delivered: false,
            async_rewake: input.async_rewake,
            completion: None,
        };
        let generation = hook.generation;
        let id = hook.id.clone();
        match state.hooks.iter().position(|existing| existing.id == id) {
            Some(index) => state.hooks[index] = hook,
            None => state.hooks.push(hook),
        }
        Ok(Registration {
            inner: Arc::downgrade(&self.0),
            id,
            generation,
            active: Arc::new(AtomicBool::new(true)),
        })
    }
    /// `list()` returns copies, like TS's shallow Map values in a fixture.
    pub fn list(&self) -> Vec<PendingAsyncHook> {
        self.0.state.lock().unwrap().hooks.clone()
    }
    pub fn describe(&self, id: &JsString) -> Option<PendingAsyncHookDescriptor> {
        self.0
            .state
            .lock()
            .unwrap()
            .hooks
            .iter()
            .find(|hook| &hook.id == id)
            .map(|hook| PendingAsyncHookDescriptor {
                id: hook.id.clone(),
                generation: hook.generation,
                hook_name: hook.hook_name.clone(),
                hook_event: hook.hook_event.clone(),
                started_at: hook.started_at.clone(),
            })
    }
    pub fn complete(&self, id: &JsString, completion: AsyncHookCompletion) -> bool {
        let mut state = self.0.state.lock().unwrap();
        if state.state != PendingState::Active {
            return false;
        }
        let Some(hook) = state.hooks.iter_mut().find(|hook| &hook.id == id) else {
            return false;
        };
        if hook.response_delivered || hook.completion.is_some() {
            return false;
        }
        let stdout = completion.stdout.unwrap_or_else(|| hook.stdout.clone());
        let output = completion.output.unwrap_or_else(|| {
            super::exec::parse_hook_output(&String::from_utf16_lossy(stdout.units()))
        });
        if matches!(output, HookOutput::Async(_)) {
            return false;
        }
        hook.completion = Some(Completed {
            stdout,
            stderr: completion.stderr.unwrap_or_else(|| hook.stderr.clone()),
            output,
        });
        true
    }
    pub fn cancel(&self, id: &JsString) -> bool {
        let mut state = self.0.state.lock().unwrap();
        if state.state != PendingState::Active {
            return false;
        }
        let before = state.hooks.len();
        state.hooks.retain(|hook| &hook.id != id);
        state.hooks.len() != before
    }
    pub fn collect_responses(&self) -> Vec<AsyncHookResponse> {
        let mut state = self.0.state.lock().unwrap();
        let mut responses = Vec::new();
        for hook in &mut state.hooks {
            if hook.response_delivered {
                continue;
            }
            let Some(completion) = &hook.completion else {
                continue;
            };
            hook.response_delivered = true;
            responses.push(AsyncHookResponse {
                id: hook.id.clone(),
                hook_name: hook.hook_name.clone(),
                hook_event: hook.hook_event.clone(),
                stdout: completion.stdout.clone(),
                stderr: completion.stderr.clone(),
                output: completion.output.clone(),
                rewake: hook.async_rewake == Some(true) && is_blocking_output(&completion.output),
            });
        }
        responses
    }
    pub fn remove_delivered(&self) {
        let mut state = self.0.state.lock().unwrap();
        state.hooks.retain(|hook| !hook.response_delivered);
    }
    pub fn clear(&self) {
        self.0.state.lock().unwrap().hooks.clear();
    }
    pub async fn dispose(&self) {
        let mut state = self.0.state.lock().unwrap();
        if state.state == PendingState::Disposed {
            return;
        }
        state.state = PendingState::Draining;
        state.hooks.clear();
        state.state = PendingState::Disposed;
    }
}

fn is_blocking_output(output: &HookOutput) -> bool {
    matches!(
        output,
        HookOutput::Sync(sync)
            if sync.continue_ == Some(false) || sync.decision == Some(crate::hooks::protocol::HookDecision::Block)
    )
}
