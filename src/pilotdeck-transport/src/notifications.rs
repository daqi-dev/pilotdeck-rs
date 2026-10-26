//! Original advisory InstructionsLoaded lifecycle and ordered event bridge.
//! The connection retains accepted call records; neither queue nor waiter owns
//! permission to replay an uncertain effect. This worker never runs domain hooks.
use crate::AdvertisedMethods;
use crate::stdio::{BoundExchange, PendingExchange};
use pilotdeck_agent::ports::{CallControl, CancelCause, Cancellation, PortError, PortResult};
use pilotdeck_agent::turn::TurnInput;
use pilotdeck_protocol::admission::CallEffect;
use pilotdeck_protocol::{JsObject, JsValue};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

struct EventNotice {
    payload: JsObject,
    admitted: Option<mpsc::Sender<()>>,
    control: CallControl,
}
struct EventQueue {
    sender: SyncSender<EventNotice>,
    outstanding: Arc<AtomicUsize>,
}
pub(crate) struct InstructionBridge {
    lifecycle: BoundExchange,
    lifecycle_pending: Vec<PendingExchange>,
    methods: AdvertisedMethods,
    control: CallControl,
    events: Option<EventQueue>,
    event_worker: Option<JoinHandle<PortResult<()>>>,
    event_finish: Cancellation,
    event_finish_deadline: Arc<Mutex<Option<std::time::Instant>>>,
}
impl InstructionBridge {
    pub(crate) fn new(
        lifecycle: BoundExchange,
        mut event_exchange: BoundExchange,
        methods: AdvertisedMethods,
        control: CallControl,
    ) -> PortResult<Self> {
        let event_finish = Cancellation::default();
        let event_finish_deadline = Arc::new(Mutex::new(None));
        let (events, event_worker) = if methods.supports("event", "emit") {
            let (sender, receiver) = mpsc::sync_channel::<EventNotice>(64);
            let outstanding = Arc::new(AtomicUsize::new(0));
            let event_outstanding = outstanding.clone();
            let finish_signal = event_finish.clone();
            let finish_deadline = event_finish_deadline.clone();
            let operation_deadline = control.deadline;
            let worker = thread::Builder::new()
                .name("pilotdeck-event-notices".into())
                .spawn(move || {
                    let mut unresolved = None;
                    for notice in receiver {
                        let terminal_deadline =
                            *finish_deadline.lock().unwrap_or_else(|p| p.into_inner());
                        let start_control = CallControl {
                            cancellation: notice.control.cancellation,
                            deadline: terminal_deadline.or(notice.control.deadline),
                        };
                        let event_control = CallControl {
                            cancellation: finish_signal.clone(),
                            deadline: terminal_deadline.or(operation_deadline),
                        };
                        // Original event emitter has no error channel. Known
                        // failure and uncertain delivery leave the next notice
                        // independent, while the connection retains uncertainty.
                        let pending = if unresolved.is_none() {
                            Some(event_exchange.start(
                                &start_control,
                                "event",
                                "emit",
                                CallEffect::SideEffect,
                                notice.payload,
                            ))
                        } else {
                            None
                        };
                        // An idle emitter starts its original microtask before
                        // createModelRequest resolves. A queued successor still
                        // waits behind the previous response without blocking
                        // model preparation on that response.
                        if let Some(admitted) = notice.admitted {
                            let _ = admitted.send(());
                        }
                        if let Some(pending) = pending
                            && let Err(
                                error @ (PortError::AcceptedUnknown { .. }
                                | PortError::Cancelled { .. }
                                | PortError::TransportBeforeDispatch { .. }),
                            ) = pending.and_then(|pending| pending.wait(&event_control))
                        {
                            // Keep the first unresolved identity or failed
                            // bounded admission. Do not hide an omitted notice
                            // behind a known completion. Drain later queued
                            // notices without dispatching new effects.
                            unresolved = Some(error);
                        }
                        event_outstanding.fetch_sub(1, Ordering::SeqCst);
                    }
                    unresolved.map_or(Ok(()), Err)
                })
                .map_err(|error| PortError::InvalidResponse {
                    message: format!("Event notice worker could not start: {error}").into(),
                })?;
            (
                Some(EventQueue {
                    sender,
                    outstanding,
                }),
                Some(worker),
            )
        } else {
            (None, None)
        };
        Ok(Self {
            lifecycle,
            lifecycle_pending: Vec::new(),
            methods,
            control,
            events,
            event_worker,
            event_finish,
            event_finish_deadline,
        })
    }
    pub(crate) fn notice(&mut self, input: &TurnInput, has_system_prompt: bool) -> PortResult<()> {
        let mut detail = JsObject::new();
        detail.insert("hasSystemPrompt".into(), JsValue::Bool(has_system_prompt));
        // This phase follows a known context-preparation ACK. Source still
        // dispatches InstructionsLoaded after user cancellation, before its
        // post-createModelRequest abort check. Only a bounded cleanup grant
        // permits these notices; retirement/transport loss retain their cause.
        if let Some(cause @ (CancelCause::OwnerRetired | CancelCause::TransportClosed)) =
            self.control.cancellation.cause()
        {
            return Err(PortError::Cancelled { cause });
        }
        let accepted = CallControl {
            cancellation: Cancellation::default(),
            deadline: Some(std::time::Instant::now() + std::time::Duration::from_secs(5)),
        };
        self.queue_lifecycle("InstructionsLoaded", detail, &accepted)?;
        if let Some(queue) = &self.events {
            let mut event = JsObject::new();
            event.insert("type".into(), "instructions_loaded".into());
            event.insert(
                "sessionId".into(),
                JsValue::String(input.session_id.clone()),
            );
            event.insert("turnId".into(), JsValue::String(input.turn_id.clone()));
            event.insert("hasSystemPrompt".into(), JsValue::Bool(has_system_prompt));
            let mut payload = JsObject::new();
            payload.insert("operation".into(), "emit".into());
            payload.insert("event".into(), JsValue::Object(event));
            let idle = queue.outstanding.fetch_add(1, Ordering::SeqCst) == 0;
            let (admitted, admission) = mpsc::channel();
            if let Err(error) = queue.sender.try_send(EventNotice {
                payload,
                admitted: idle.then_some(admitted),
                control: accepted,
            }) {
                queue.outstanding.fetch_sub(1, Ordering::SeqCst);
                return Err(PortError::InvalidResponse {
                    message: match error {
                        mpsc::TrySendError::Full(_) => {
                            "Advisory event pending-notice capacity exhausted."
                        }
                        mpsc::TrySendError::Disconnected(_) => {
                            "Event notice worker terminated before queue admission."
                        }
                    }
                    .into(),
                });
            }
            if idle {
                admission.recv().map_err(|_| PortError::InvalidResponse {
                    message: "Event notice worker terminated before dispatch admission.".into(),
                })?;
            }
        }
        Ok(())
    }
    pub(crate) fn before_model_request(
        &mut self,
        route: &pilotdeck_agent::ports::Route,
    ) -> PortResult<()> {
        let mut detail = JsObject::new();
        detail.insert("provider".into(), JsValue::String(route.provider.clone()));
        detail.insert("model".into(), JsValue::String(route.model.clone()));
        self.queue_lifecycle("PreModelRequest", detail, &self.control.clone())
    }
    fn queue_lifecycle(
        &mut self,
        event: &str,
        detail: JsObject,
        dispatch_control: &CallControl,
    ) -> PortResult<()> {
        // Reap already known advisory replies without waiting for a lifecycle
        // hook before advancing the model. Original results/errors are ignored.
        let mut waiting = Vec::new();
        let mut pending_calls = std::mem::take(&mut self.lifecycle_pending).into_iter();
        while let Some(pending) = pending_calls.next() {
            if pending.is_ready() {
                if let Err(error) = pending.wait(&self.control)
                    && notification_ownership_error(&error)
                {
                    waiting.extend(pending_calls);
                    self.lifecycle_pending = waiting;
                    return Err(error);
                }
            } else {
                waiting.push(pending);
            }
        }
        self.lifecycle_pending = waiting;
        if self.methods.supports("lifecycle", "dispatch") {
            if self.lifecycle_pending.len() >= 64 {
                return Err(PortError::InvalidResponse {
                    message: "Advisory lifecycle pending-call capacity exhausted.".into(),
                });
            }
            let mut payload = JsObject::new();
            payload.insert("operation".into(), "dispatch".into());
            payload.insert("event".into(), JsValue::from(event));
            payload.insert("payload".into(), JsValue::Object(detail));
            match self.lifecycle.start(
                dispatch_control,
                "lifecycle",
                "dispatch",
                CallEffect::SideEffect,
                payload,
            ) {
                Ok(pending) => self.lifecycle_pending.push(pending),
                Err(error) if notification_ownership_error(&error) => return Err(error),
                // Known pre-dispatch advisory failure has no accepted effect.
                Err(_) => {}
            }
        }
        Ok(())
    }
    /// Settle all event notices queued before publishing the Module terminal.
    /// The independent reader delivers replies or cancellation/EOF wakes waits.
    pub(crate) fn finish(&mut self) -> PortResult<()> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let terminal_control = CallControl {
            cancellation: self.control.cancellation.clone(),
            deadline: Some(
                self.control
                    .deadline
                    .map_or(deadline, |prior| prior.min(deadline)),
            ),
        };
        // Capture, Completed and exceptional factory settlement can call this
        // repeatedly. Only the first terminal observation owns a new bound.
        let terminal_control = {
            let mut saved = self
                .event_finish_deadline
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            let deadline = *saved.get_or_insert(terminal_control.deadline.expect("terminal bound"));
            CallControl {
                cancellation: terminal_control.cancellation,
                deadline: Some(deadline),
            }
        };
        // Wake an already-dispatched waiter before join, without replay or
        // marking the accepted call known. Its existing identity reconciles
        // within the same fixed terminal budget.
        self.event_finish.cancel_with_reconciliation_deadline(
            CancelCause::Requested("notification terminal reconciliation".into()),
            terminal_control
                .deadline
                .expect("terminal reconciliation always bounded"),
        );
        self.events.take();
        if let Some(worker) = self.event_worker.take() {
            worker.join().map_err(|_| PortError::InvalidResponse {
                message: "Event notice worker panicked.".into(),
            })??;
        }
        let mut pending_calls = std::mem::take(&mut self.lifecycle_pending).into_iter();
        while let Some(pending) = pending_calls.next() {
            if let Err(error) = pending.wait(&terminal_control)
                && notification_ownership_error(&error)
            {
                self.lifecycle_pending.extend(pending_calls);
                return Err(error);
            }
        }
        Ok(())
    }
}
impl Drop for InstructionBridge {
    fn drop(&mut self) {
        // Exceptional loop exit does not strand the actual owned event worker.
        // A known successful finish already joined it and leaves control intact.
        if self.event_worker.is_some() {
            self.control.cancellation.cancel(CancelCause::OwnerRetired);
            self.event_finish.cancel(CancelCause::OwnerRetired);
            self.events.take();
            if let Some(worker) = self.event_worker.take() {
                let _ = worker.join();
            }
        }
        // No additional hook is called during retirement. Pending side-effect
        // records remain with the connection, including accepted unknown replies.
        let retirement = CallControl {
            cancellation: Default::default(),
            deadline: Some(std::time::Instant::now()),
        };
        for pending in self.lifecycle_pending.drain(..) {
            let _ = pending.wait(&retirement);
        }
    }
}

fn notification_ownership_error(error: &PortError) -> bool {
    matches!(
        error,
        PortError::AcceptedUnknown { .. }
            | PortError::TransportBeforeDispatch { .. }
            | PortError::Cancelled {
                cause: CancelCause::OwnerRetired | CancelCause::TransportClosed
            }
    )
}

#[cfg(test)]
mod payload_tests {
    use super::*;
    use crate::stdio::{ConnectionOwner, ResponseDisposition};
    use pilotdeck_agent::ports::ExecutionIdentity;
    use pilotdeck_protocol::{admission::SidecarBinding, parse_json};
    use std::{
        io::{self, Write},
        time::{Duration, Instant},
    };

    struct Writer(Vec<u8>, mpsc::Sender<JsValue>);
    impl Write for Writer {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            let bytes = std::mem::take(&mut self.0);
            let value = parse_json(std::str::from_utf8(&bytes).map_err(io::Error::other)?)
                .map_err(io::Error::other)?;
            self.1.send(value).map_err(io::Error::other)
        }
    }
    struct RunningWriter(ConnectionOwner, Option<JoinHandle<io::Result<()>>>);
    impl Drop for RunningWriter {
        fn drop(&mut self) {
            self.0.close();
            if let Some(worker) = self.1.take() {
                worker.join().unwrap().unwrap();
            }
        }
    }

    #[test]
    fn sidecar_payload_bridge_continues_after_known_failure() {
        let oracle = parse_json(include_str!(
            "../../pilotdeck-gateway/tests/fixtures/sidecar-payload/oracle.json"
        ))
        .unwrap();
        let JsValue::Array(cases) = oracle.get("cases").unwrap() else {
            panic!("cases")
        };
        let case = cases
            .iter()
            .find(|row| {
                row.get("route") == Some(&"host-bridge".into())
                    && row.get("name") == Some(&"null".into())
            })
            .unwrap();
        let JsValue::Array(replies) = case.get("replies").unwrap() else {
            panic!("replies")
        };
        let events = [
            case.get("payload").unwrap().clone(),
            parse_json(
                r#"{"type":"text_delta","sessionId":"session","turnId":"turn","text":"after"}"#,
            )
            .unwrap(),
        ];
        let owner = ConnectionOwner::new(
            SidecarBinding {
                module_id: "pilotdeck-agent-loop".into(),
                module_instance_id: "instance".into(),
                connection_generation: "generation".into(),
                capabilities_version: "1".into(),
            },
            8,
        );
        let identity = ExecutionIdentity {
            session_id: "session".into(),
            turn_id: "turn".into(),
            run_id: "run".into(),
            operation_id: "operation".into(),
            request_id: "request".into(),
            idempotency_key: None,
            module_instance_id: "instance".into(),
            connection_generation: "generation".into(),
        };
        let control = CallControl {
            cancellation: Cancellation::default(),
            deadline: Some(Instant::now() + Duration::from_secs(10)),
        };
        let (sent, received) = mpsc::channel();
        let writer_owner = owner.clone();
        let _writer = RunningWriter(
            owner.clone(),
            Some(thread::spawn(move || {
                writer_owner.drive_writer(Writer(Vec::new(), sent))
            })),
        );
        let mut bridge = InstructionBridge::new(
            owner.bound_exchange(identity.clone()).unwrap(),
            owner.bound_exchange(identity).unwrap(),
            AdvertisedMethods {
                modules: vec![("event".into(), vec!["emit".into()])],
            },
            control.clone(),
        )
        .unwrap();
        let queue = bridge.events.as_ref().unwrap();
        for event in &events {
            let mut payload = JsObject::new();
            payload.insert("operation".into(), "emit".into());
            payload.insert("event".into(), event.clone());
            queue.outstanding.fetch_add(1, Ordering::SeqCst);
            queue
                .sender
                .send(EventNotice {
                    payload,
                    admitted: None,
                    control: control.clone(),
                })
                .unwrap();
        }
        for (event, golden) in events.iter().zip(replies) {
            let call = received
                .recv_timeout(Duration::from_secs(3))
                .expect("known failure must not suppress the next event");
            assert_eq!(call.get("module"), Some(&"event".into()));
            assert_eq!(call.get("payload").unwrap().get("event"), Some(event));
            let mut response = JsObject::new();
            response.insert("kind".into(), "response".into());
            response.insert("messageId".into(), "reply".into());
            response.insert("inReplyTo".into(), call.get("messageId").unwrap().clone());
            response.insert("ok".into(), golden.get("ok").unwrap().clone());
            if let Some(message) = golden.get("error") {
                let mut error = JsObject::new();
                error.insert("message".into(), message.clone());
                response.insert("error".into(), JsValue::Object(error));
            }
            assert_eq!(
                owner.receive_response(&JsValue::Object(response)),
                ResponseDisposition::Completed
            );
        }
        bridge.finish().unwrap();
        assert!(received.try_recv().is_err());
    }
}
