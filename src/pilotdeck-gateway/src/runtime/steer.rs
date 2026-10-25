//! Turn-scoped durable-first guidance mailbox (TS SteerMailbox / TurnRunner).
use crate::host::{HostError, HostFuture, ModuleError};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{
    collections::HashSet,
    future::Future,
    sync::{Arc, Mutex},
};
use tokio::sync::oneshot;

#[derive(Clone, Debug, PartialEq)]
pub struct SteerMessage {
    pub item_id: JsString,
    pub message: JsValue,
    pub allowed_read_files: Option<Vec<JsString>>,
}
impl SteerMessage {
    pub fn to_object(&self) -> JsObject {
        let mut result = JsObject::new();
        result.insert("itemId".into(), JsValue::String(self.item_id.clone()));
        result.insert("message".into(), self.message.clone());
        if let Some(paths) = &self.allowed_read_files {
            result.insert(
                "allowedReadFiles".into(),
                JsValue::Array(paths.iter().cloned().map(JsValue::String).collect()),
            );
        }
        result
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SteerReason {
    NoActiveTurn,
    TurnMismatch,
    TurnClosing,
    Cancelled,
    TooLate,
}
impl SteerReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoActiveTurn => "no_active_turn",
            Self::TurnMismatch => "turn_mismatch",
            Self::TurnClosing => "turn_closing",
            Self::Cancelled => "cancelled",
            Self::TooLate => "too_late",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnqueueResult {
    pub accepted: bool,
    pub reason: Option<SteerReason>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CancelResult {
    pub cancelled: bool,
    pub reason: Option<SteerReason>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct DrainResult {
    pub messages: Vec<SteerMessage>,
    pub closed: bool,
}

/// A successful callback is the durability boundary. Failures leave the
/// corresponding in-memory transition unapplied and do not poison the queue.
pub trait MutationRecorder: Send + Sync {
    fn record(&self, turn_id: JsString, mutation: JsObject) -> HostFuture<()>;
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum DeliveryState {
    Pending,
    Offered,
    Claimed,
}
struct PendingSteer {
    message: SteerMessage,
    state: DeliveryState,
}
#[derive(Default)]
struct State {
    turn_id: Option<JsString>,
    open: bool,
    // Stable entry identity matters if start/finish runs while claim is awaiting
    // storage: its completion must not claim a new entry with the same item ID.
    pending: Vec<Arc<Mutex<PendingSteer>>>,
    seen: HashSet<JsString>,
    cancelled: HashSet<JsString>,
}
impl State {
    fn find(&self, item_id: &JsString) -> Option<Arc<Mutex<PendingSteer>>> {
        self.pending
            .iter()
            .find(|entry| entry.lock().unwrap().message.item_id == *item_id)
            .cloned()
    }
    fn remove(&mut self, item_id: &JsString) {
        if let Some(index) = self
            .pending
            .iter()
            .position(|entry| entry.lock().unwrap().message.item_id == *item_id)
        {
            self.pending.remove(index);
        }
    }
    fn offer(&mut self) -> Vec<SteerMessage> {
        self.pending
            .iter()
            .filter_map(|entry| {
                let mut entry = entry.lock().unwrap();
                if entry.state != DeliveryState::Pending {
                    return None;
                }
                entry.state = DeliveryState::Offered;
                Some(entry.message.clone())
            })
            .collect()
    }
    fn active_turn(&self, turn_id: &JsString) -> Option<SteerReason> {
        match &self.turn_id {
            None => Some(SteerReason::NoActiveTurn),
            Some(id) if id.is_empty() => Some(SteerReason::NoActiveTurn),
            Some(id) if id != turn_id => Some(SteerReason::TurnMismatch),
            _ => None,
        }
    }
}
#[derive(Default)]
struct Inner {
    state: Mutex<State>,
    tail: Mutex<Option<oneshot::Receiver<()>>>,
    recorder: Option<Arc<dyn MutationRecorder>>,
}

/// Mutations are queued at method invocation, in order, and continue if their
/// returned future is dropped (like TS promises). Invoke async mutations from a
/// Tokio runtime. Synchronous boundaries never wait for storage or the queue.
#[derive(Clone, Default)]
pub struct SteerMailbox(Arc<Inner>);
impl SteerMailbox {
    #[cfg(test)]
    pub(crate) fn poison_state_for_test(&self) {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _state = self.0.state.lock().unwrap();
            panic!("injected mailbox mutex poison");
        }));
    }
    pub fn new(recorder: Option<Arc<dyn MutationRecorder>>) -> Self {
        Self(Arc::new(Inner {
            recorder,
            ..Inner::default()
        }))
    }
    pub fn start(&self, turn_id: JsString) {
        *self.0.state.lock().unwrap() = State {
            turn_id: Some(turn_id),
            open: true,
            ..State::default()
        };
    }
    pub fn enqueue(&self, turn_id: JsString, input: SteerMessage) -> HostFuture<EnqueueResult> {
        let mailbox = self.clone();
        self.serialize(async move {
            let reject = |reason| EnqueueResult {
                accepted: false,
                reason: Some(reason),
            };
            {
                let state = mailbox.0.state.lock().unwrap();
                if let Some(reason) = state.active_turn(&turn_id) {
                    return Ok(reject(reason));
                }
                if !state.open {
                    return Ok(reject(SteerReason::TurnClosing));
                }
                if state.cancelled.contains(&input.item_id) {
                    return Ok(reject(SteerReason::Cancelled));
                }
                if state.seen.contains(&input.item_id) {
                    return Ok(EnqueueResult {
                        accepted: true,
                        reason: None,
                    });
                }
            }
            let mut insert = JsObject::new();
            insert.insert("mutation".into(), "insert".into());
            for (key, value) in input.to_object().entries() {
                insert.insert(key.clone(), value.clone());
            }
            mailbox.record(&turn_id, insert).await?;
            let rejected = {
                let mut state = mailbox.0.state.lock().unwrap();
                if !state.open || state.turn_id.as_ref() != Some(&turn_id) {
                    true
                } else {
                    state.seen.insert(input.item_id.clone());
                    state.pending.push(Arc::new(Mutex::new(PendingSteer {
                        message: input.clone(),
                        state: DeliveryState::Pending,
                    })));
                    false
                }
            };
            if rejected {
                mailbox
                    .record(
                        &turn_id,
                        mutation("discard", &input.item_id, Some("turn_closing")),
                    )
                    .await?;
                // TS checks the turn again after the discard callback.
                let state = mailbox.0.state.lock().unwrap();
                return Ok(reject(if state.turn_id.as_ref() == Some(&turn_id) {
                    SteerReason::TurnClosing
                } else {
                    SteerReason::NoActiveTurn
                }));
            }
            Ok(EnqueueResult {
                accepted: true,
                reason: None,
            })
        })
    }
    pub fn cancel(&self, turn_id: JsString, item_id: JsString) -> HostFuture<CancelResult> {
        let mailbox = self.clone();
        self.serialize(async move {
            let rejected = |reason| CancelResult {
                cancelled: false,
                reason: Some(reason),
            };
            let pending = {
                let state = mailbox.0.state.lock().unwrap();
                if let Some(reason) = state.active_turn(&turn_id) {
                    return Ok(rejected(reason));
                }
                if state.cancelled.contains(&item_id) {
                    return Ok(CancelResult {
                        cancelled: true,
                        reason: None,
                    });
                }
                let pending = state.find(&item_id);
                if pending
                    .as_ref()
                    .is_some_and(|entry| entry.lock().unwrap().state != DeliveryState::Pending)
                    || (pending.is_none() && state.seen.contains(&item_id))
                {
                    return Ok(rejected(SteerReason::TooLate));
                }
                pending
            };
            mailbox
                .record(&turn_id, mutation("cancel", &item_id, None))
                .await?;
            let mut state = mailbox.0.state.lock().unwrap();
            // A drain may have offered the captured entry during the write.
            if pending.is_some() {
                state.remove(&item_id);
            }
            state.cancelled.insert(item_id);
            Ok(CancelResult {
                cancelled: true,
                reason: None,
            })
        })
    }
    pub fn drain(&self, turn_id: &JsString) -> Vec<SteerMessage> {
        let mut state = self.0.state.lock().unwrap();
        if !state.open || state.turn_id.as_ref() != Some(turn_id) {
            return Vec::new();
        }
        state.offer()
    }
    pub fn drain_or_close(&self, turn_id: &JsString) -> DrainResult {
        let mut state = self.0.state.lock().unwrap();
        if !state.open || state.turn_id.as_ref() != Some(turn_id) {
            return DrainResult {
                messages: Vec::new(),
                closed: true,
            };
        }
        let messages = state.offer();
        let closed = messages.is_empty();
        if closed {
            state.open = false;
        }
        DrainResult { messages, closed }
    }
    pub fn claim(&self, turn_id: JsString, item_id: JsString) -> HostFuture<()> {
        let mailbox = self.clone();
        self.serialize(async move {
            let pending = {
                let state = mailbox.0.state.lock().unwrap();
                let Some(pending) = state.find(&item_id) else {
                    return Ok(());
                };
                let delivery = pending.lock().unwrap().state;
                if delivery == DeliveryState::Claimed {
                    return Ok(());
                }
                if state.turn_id.as_ref() != Some(&turn_id) || delivery != DeliveryState::Offered {
                    let message = "Steer "
                        .encode_utf16()
                        .chain(item_id.units().iter().copied())
                        .chain(" is not offered for turn ".encode_utf16())
                        .chain(turn_id.units().iter().copied())
                        .chain(".".encode_utf16())
                        .collect();
                    return Err(ModuleError::new(JsString::from_utf16(message)).into());
                }
                pending
            };
            mailbox
                .record(&turn_id, mutation("claim", &item_id, None))
                .await?;
            pending.lock().unwrap().state = DeliveryState::Claimed;
            Ok(())
        })
    }
    pub fn ack(&self, turn_id: &JsString, item_id: &JsString) {
        let mut state = self.0.state.lock().unwrap();
        if state.turn_id.as_ref() != Some(turn_id) {
            return;
        }
        if state
            .find(item_id)
            .is_some_and(|entry| entry.lock().unwrap().state == DeliveryState::Claimed)
        {
            state.remove(item_id);
        }
    }
    pub fn close(&self, turn_id: JsString) -> HostFuture<Vec<SteerMessage>> {
        {
            let mut state = self.0.state.lock().unwrap();
            if state.turn_id.as_ref() != Some(&turn_id) {
                return Box::pin(async { Ok(Vec::new()) });
            }
            state.open = false;
        }
        let mailbox = self.clone();
        self.serialize(async move {
            let mut discarded = Vec::new();
            loop {
                let entry = mailbox.0.state.lock().unwrap().pending.first().cloned();
                let Some(entry) = entry else { break };
                let message = entry.lock().unwrap().message.clone();
                mailbox
                    .record(
                        &turn_id,
                        mutation("discard", &message.item_id, Some("turn_ended")),
                    )
                    .await?;
                discarded.push(message);
                let mut state = mailbox.0.state.lock().unwrap();
                if !state.pending.is_empty() {
                    state.pending.remove(0);
                }
            }
            Ok(discarded)
        })
    }
    pub fn finish(&self, turn_id: &JsString) -> Vec<SteerMessage> {
        let mut state = self.0.state.lock().unwrap();
        if state.turn_id.as_ref() != Some(turn_id) {
            return Vec::new();
        }
        let previous = std::mem::take(&mut *state);
        previous
            .pending
            .into_iter()
            .map(|entry| entry.lock().unwrap().message.clone())
            .collect()
    }
    async fn record(&self, turn_id: &JsString, mutation: JsObject) -> Result<(), HostError> {
        if let Some(recorder) = &self.0.recorder {
            recorder.record(turn_id.clone(), mutation).await?;
        }
        Ok(())
    }
    fn serialize<T: Send + 'static>(
        &self,
        operation: impl Future<Output = Result<T, HostError>> + Send + 'static,
    ) -> HostFuture<T> {
        let (complete, next) = oneshot::channel();
        let previous = self.0.tail.lock().unwrap().replace(next);
        let task = tokio::spawn(async move {
            if let Some(previous) = previous {
                let _ = previous.await;
            }
            let result = operation.await;
            let _ = complete.send(());
            result
        });
        Box::pin(async move {
            task.await.map_err(|error| {
                ModuleError::new(format!("Steer mailbox mutation failed: {error}"))
            })?
        })
    }
}
fn mutation(kind: &str, item_id: &JsString, reason: Option<&str>) -> JsObject {
    let mut result = JsObject::new();
    result.insert("mutation".into(), kind.into());
    result.insert("itemId".into(), JsValue::String(item_id.clone()));
    if let Some(reason) = reason {
        result.insert("reason".into(), reason.into());
    }
    result
}

/// TurnRunner's drain tracking and durable/applied callbacks. Authorization is
/// applied by the host dispatcher after on_durable_message succeeds.
#[derive(Clone)]
pub struct TurnSteers {
    pub mailbox: SteerMailbox,
    turn_id: JsString,
    tracked: Arc<Mutex<Tracked>>,
}
#[derive(Default)]
struct Tracked {
    unacknowledged: Vec<SteerMessage>,
    messages: Vec<JsValue>,
}
impl TurnSteers {
    pub fn new(mailbox: SteerMailbox, turn_id: JsString, messages: Vec<JsValue>) -> Self {
        Self {
            mailbox,
            turn_id,
            tracked: Arc::new(Mutex::new(Tracked {
                messages,
                ..Tracked::default()
            })),
        }
    }
    pub fn drain(&self) -> Vec<SteerMessage> {
        self.track(self.mailbox.drain(&self.turn_id))
    }
    pub fn drain_or_close(&self) -> DrainResult {
        let result = self.mailbox.drain_or_close(&self.turn_id);
        DrainResult {
            messages: self.track(result.messages),
            closed: result.closed,
        }
    }
    fn track(&self, messages: Vec<SteerMessage>) -> Vec<SteerMessage> {
        let mut tracked = self.tracked.lock().unwrap();
        for message in &messages {
            if let Some(old) = tracked
                .unacknowledged
                .iter_mut()
                .find(|entry| entry.item_id == message.item_id)
            {
                *old = message.clone();
            } else {
                tracked.unacknowledged.push(message.clone());
            }
        }
        messages
    }
    pub fn on_durable_message(
        &self,
        message: JsValue,
        record: impl FnOnce(JsValue) -> HostFuture<()> + Send + 'static,
    ) -> HostFuture<()> {
        let turn = self.clone();
        Box::pin(async move {
            if let Some(item_id) = message
                .get("metadata")
                .and_then(|v| v.get("queueItemId"))
                .and_then(JsValue::as_string)
                .filter(|id| !id.is_empty())
            {
                turn.mailbox
                    .claim(turn.turn_id.clone(), item_id.clone())
                    .await?;
            }
            record(message).await
        })
    }
    pub fn on_steer_applied(&self, item_id: &JsString) {
        let mut tracked = self.tracked.lock().unwrap();
        if let Some(index) = tracked
            .unacknowledged
            .iter()
            .position(|entry| &entry.item_id == item_id)
        {
            let applied = tracked.unacknowledged.remove(index);
            tracked.messages.push(applied.message);
        }
        self.mailbox.ack(&self.turn_id, item_id);
    }
    pub fn messages(&self) -> Vec<JsValue> {
        self.tracked.lock().unwrap().messages.clone()
    }
    pub fn unacknowledged(&self) -> Vec<SteerMessage> {
        self.tracked.lock().unwrap().unacknowledged.clone()
    }
}
