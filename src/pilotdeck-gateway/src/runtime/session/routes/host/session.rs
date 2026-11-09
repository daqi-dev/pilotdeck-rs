use super::super::live::{LiveBindings, LiveSessionState};
use super::*;
use crate::{
    metadata::object,
    runtime::{
        project::{DisposeFuture, RuntimeError, RuntimeResult},
        session::{
            LiveSession, LiveSessionHandle, LiveTurn, SessionNext, SessionQueuedTurn,
            module_host::with_live_state,
        },
    },
};
use std::{collections::VecDeque, sync::Mutex};

pub(crate) fn bind(
    live: Arc<LiveBindings>,
    context: SessionContext,
    cwd: JsString,
    inner: Arc<dyn LiveSession>,
    storage: SessionStorage,
    previous: Option<LiveSessionHandle>,
    execution_error: Option<RuntimeError>,
) -> Result<LiveSessionHandle, RuntimeError> {
    let restored = storage.read_transcript(None).map_err(runtime_error)?;
    let queued = SessionQueuedTurn::restore(&restored.entries)?;
    let input_template = inner.queued_input_template();
    let state = LiveSessionState::new(context.session_key, cwd, storage)?;
    if let Some(previous) = previous {
        let snapshot = previous.snapshot();
        if let Some(seed) = snapshot.get("seedState") {
            let seed = crate::host::modules::tool_context::parse_seed_state(seed)
                .map_err(runtime_error)?;
            if let Some(seed) = seed {
                *state.checkpoint.read_file_state.lock().unwrap() =
                    seed.read_file_state.unwrap_or_default();
                *state.checkpoint.write_snapshots.lock().unwrap() =
                    seed.write_snapshots.unwrap_or_default();
                state
                    .checkpoint
                    .allow_read_files(&seed.allowed_read_files.unwrap_or_default());
            }
        }
    }
    live.bind(&state);
    Ok(LiveSessionHandle::new(
        Arc::new(BoundSession {
            inner,
            state,
            live,
            execution_error,
            queued: Mutex::new(queued.into()),
            input_template: Mutex::new(input_template),
        }),
        None,
    ))
}

struct BoundSession {
    execution_error: Option<RuntimeError>,
    inner: Arc<dyn LiveSession>,
    state: Arc<LiveSessionState>,
    live: Arc<LiveBindings>,
    queued: Mutex<VecDeque<SessionQueuedTurn>>,
    input_template: Mutex<Option<JsValue>>,
}
impl LiveSession for BoundSession {
    fn steer(
        &self,
        turn_id: JsString,
        message: crate::runtime::steer::SteerMessage,
    ) -> crate::host::HostFuture<crate::runtime::steer::EnqueueResult> {
        self.inner.steer(turn_id, message)
    }
    fn cancel_steer(
        &self,
        turn_id: JsString,
        item_id: JsString,
    ) -> crate::host::HostFuture<crate::runtime::steer::CancelResult> {
        self.inner.cancel_steer(turn_id, item_id)
    }
    fn submit(&self, input: JsValue, options: JsValue) -> Result<Box<dyn LiveTurn>, RuntimeError> {
        self.submit_inner(input, options, None)
    }
    fn pending_queued_turns(&self) -> Result<Vec<SessionQueuedTurn>, RuntimeError> {
        Ok(self.queued.lock().unwrap().iter().cloned().collect())
    }
    fn enqueue_queued_turn(&self, turn: &SessionQueuedTurn) -> RuntimeResult {
        let mut queued = self.queued.lock().unwrap();
        self.state
            .recorder
            .record_turn_enqueued(
                &self.state.session_key,
                &turn.turn_id,
                &turn.item_id,
                turn.input.clone(),
                turn.submit_options.clone(),
            )
            .map_err(runtime_error)?;
        queued.push_back(turn.clone());
        Ok(())
    }
    fn submit_queued_turn(
        &self,
        turn: &SessionQueuedTurn,
    ) -> Result<Box<dyn LiveTurn>, RuntimeError> {
        {
            let mut queued = self.queued.lock().unwrap();
            let next = queued
                .front()
                .filter(|next| next.item_id == turn.item_id && next.turn_id == turn.turn_id)
                .ok_or_else(|| RuntimeError::new("Queued turn is not the next FIFO admission."))?;
            self.state
                .recorder
                .start_queued_turn(&self.state.session_key, &next.turn_id, &next.item_id)
                .map_err(runtime_error)?;
            queued.pop_front();
        }
        let mut options = turn
            .submit_options
            .as_object()
            .cloned()
            .ok_or_else(|| RuntimeError::new("Queued turn options must be an object."))?;
        options.insert("turnId".into(), JsValue::String(turn.turn_id.clone()));
        let input =
            queued_execution_input(self.input_template.lock().unwrap().as_ref(), &turn.input)?;
        self.submit_inner(input, JsValue::Object(options), Some(&turn.item_id))
    }
    fn discard_queued_turns(&self, reason: &str) -> RuntimeResult {
        let mut queued = self.queued.lock().unwrap();
        while let Some(turn) = queued.front() {
            self.state
                .recorder
                .record_turn_discarded(
                    &self.state.session_key,
                    &turn.turn_id,
                    &turn.item_id,
                    reason,
                )
                .map_err(runtime_error)?;
            queued.pop_front();
        }
        Ok(())
    }
    fn snapshot(&self) -> JsValue {
        let mut snapshot = self
            .inner
            .snapshot()
            .as_object()
            .cloned()
            .unwrap_or_default();
        match self.state.snapshot() {
            Ok(state) => {
                if let Some(state) = state.as_object() {
                    for (key, value) in state.entries() {
                        snapshot.insert(key.clone(), value.clone());
                    }
                }
            }
            Err(error) => {
                snapshot.insert(
                    "snapshotError".into(),
                    JsValue::String(error.to_string().into()),
                );
            }
        }
        JsValue::Object(snapshot)
    }
    fn abort(&self, reason: &JsString) -> RuntimeResult {
        self.inner.abort(reason)
    }
    fn dispose_start(&self, reason: &JsString) -> DisposeFuture<'_> {
        self.inner.dispose_start(reason)
    }
    fn dispose(&self) -> DisposeFuture<'_> {
        Box::pin(async move {
            let _publication = self.live.publication.lock().await;
            self.inner.dispose().await?;
            self.state.dispose()
        })
    }
}

impl BoundSession {
    fn submit_inner(
        &self,
        mut input: JsValue,
        options: JsValue,
        inbox_item_id: Option<&JsString>,
    ) -> Result<Box<dyn LiveTurn>, RuntimeError> {
        if let Some(error) = &self.execution_error {
            return Err(error.clone());
        }
        let turn = options
            .get("turnId")
            .and_then(JsValue::as_string)
            .cloned()
            .filter(|id| !id.is_empty())
            .ok_or_else(|| RuntimeError::new("Turn identity is missing."))?;
        if inbox_item_id.is_none() {
            *self.input_template.lock().unwrap() = Some(input.clone());
        }
        let JsValue::Object(fields) = &mut input else {
            return Err(RuntimeError::new("Session input must be an object."));
        };
        fields.insert("seedState".into(), self.state.seed_state()?);
        fields.insert(
            "sessionId".into(),
            JsValue::String(self.state.session_key.clone()),
        );
        fields.insert("turnId".into(), JsValue::String(turn.clone()));
        if let Some(agent) = fields.get("agent").and_then(JsValue::as_object).cloned() {
            let mut agent = agent;
            agent.insert("cwd".into(), JsValue::String(self.state.cwd.clone()));
            fields.insert("agent".into(), JsValue::Object(agent));
        }
        let accepted = match fields.get("messages") {
            Some(JsValue::Array(messages)) => messages
                .last()
                .cloned()
                .ok_or_else(|| RuntimeError::new("Session input is empty."))?,
            _ => return Err(RuntimeError::new("Session messages must be an array.")),
        };
        let metadata = crate::runtime::turn::submit_host::accepted_input_metadata(&input);
        let mut accepted = object([
            ("type", "input_accepted".into()),
            ("sessionId", JsValue::String(self.state.session_key.clone())),
            ("turnId", JsValue::String(turn.clone())),
            ("messages", JsValue::Array(vec![accepted])),
        ]);
        if let (Some(metadata), JsValue::Object(fields)) = (metadata, &mut accepted) {
            fields.insert("metadata".into(), JsValue::Object(metadata));
        }
        let started = object([
            ("type", "turn_started".into()),
            ("sessionId", JsValue::String(self.state.session_key.clone())),
            ("turnId", JsValue::String(turn.clone())),
        ]);
        let mut started = started;
        if let (Some(item_id), JsValue::Object(fields)) = (inbox_item_id, &mut started) {
            fields.insert("inboxItemId".into(), JsValue::String(item_id.clone()));
        } else {
            self.state.record_event(&started)?;
        }
        self.state.record_event(&accepted)?;
        let inner = with_live_state(self.state.clone(), || self.inner.submit(input, options))?;
        Ok(Box::new(BoundTurn {
            inner,
            accepted: Some(accepted),
            started: Some(started),
            state: self.state.clone(),
        }))
    }
}

fn queued_execution_input(
    template: Option<&JsValue>,
    queued_input: &JsValue,
) -> Result<JsValue, RuntimeError> {
    if queued_input.get("messages").is_some() {
        return Ok(queued_input.clone());
    }
    let mut input = template
        .and_then(JsValue::as_object)
        .cloned()
        .ok_or_else(|| RuntimeError::new("Queued turn execution input is unavailable."))?;
    let kind = queued_input
        .get("type")
        .and_then(JsValue::as_string)
        .and_then(|kind| kind.to_utf8().ok())
        .unwrap_or_default();
    let content = match kind.as_str() {
        "text" => vec![object([
            ("type", "text".into()),
            (
                "text",
                queued_input
                    .get("text")
                    .cloned()
                    .ok_or_else(|| RuntimeError::new("Queued text input is missing text."))?,
            ),
        ])],
        "blocks" => match queued_input.get("content") {
            Some(JsValue::Array(content)) => content.clone(),
            _ => return Err(RuntimeError::new("Queued block input is missing content.")),
        },
        _ => return Err(RuntimeError::new("Queued turn input is invalid.")),
    };
    input.insert(
        "messages".into(),
        JsValue::Array(vec![object([
            ("role", "user".into()),
            ("content", JsValue::Array(content)),
        ])]),
    );
    Ok(JsValue::Object(input))
}

struct BoundTurn {
    inner: Box<dyn LiveTurn>,
    accepted: Option<JsValue>,
    started: Option<JsValue>,
    state: Arc<LiveSessionState>,
}
impl LiveTurn for BoundTurn {
    fn next(&mut self) -> SessionNext<'_> {
        Box::pin(async move {
            if let Some(started) = self.started.take() {
                self.state.check_error()?;
                return Ok(Some(started));
            }
            if let Some(accepted) = self.accepted.take() {
                self.state.check_error()?;
                return Ok(Some(accepted));
            }
            let event = self.inner.next().await?;
            self.state.check_error()?;
            if let Some(event) = &event
                && event
                    .get("type")
                    .and_then(JsValue::as_string)
                    .is_some_and(|kind| kind.equals_str("turn_completed"))
                && let Some(result) = event.get("result")
            {
                self.state.complete_turn(result)?;
            }
            Ok(event)
        })
    }
}
fn runtime_error(error: impl std::fmt::Display) -> RuntimeError {
    RuntimeError::new(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct QueuedInner {
        submitted: Mutex<Vec<JsString>>,
        finished: tokio::sync::watch::Sender<usize>,
    }

    impl QueuedInner {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                submitted: Mutex::new(Vec::new()),
                finished: tokio::sync::watch::channel(0).0,
            })
        }
    }

    impl LiveSession for QueuedInner {
        fn queued_input_template(&self) -> Option<JsValue> {
            Some(pilotdeck_protocol::parse_json(r#"{"agent":{},"messages":[]}"#).unwrap())
        }

        fn submit(
            &self,
            input: JsValue,
            options: JsValue,
        ) -> Result<Box<dyn LiveTurn>, RuntimeError> {
            let turn = options
                .get("turnId")
                .and_then(JsValue::as_string)
                .cloned()
                .unwrap();
            assert_eq!(
                input
                    .get("messages")
                    .and_then(|messages| match messages {
                        JsValue::Array(messages) => messages.first(),
                        _ => None,
                    })
                    .and_then(|message| message.get("content"))
                    .and_then(|content| match content {
                        JsValue::Array(content) => content.first(),
                        _ => None,
                    })
                    .and_then(|block| block.get("text"))
                    .and_then(JsValue::as_string),
                Some(&JsString::from("follow up"))
            );
            self.submitted.lock().unwrap().push(turn);
            Ok(Box::new(QueuedInnerTurn {
                finished: self.finished.clone(),
                done: false,
            }))
        }

        fn snapshot(&self) -> JsValue {
            JsValue::Object(Default::default())
        }

        fn abort(&self, _reason: &JsString) -> RuntimeResult {
            Ok(())
        }

        fn dispose(&self) -> DisposeFuture<'_> {
            Box::pin(async { Ok(()) })
        }
    }

    struct QueuedInnerTurn {
        finished: tokio::sync::watch::Sender<usize>,
        done: bool,
    }

    impl LiveTurn for QueuedInnerTurn {
        fn next(&mut self) -> SessionNext<'_> {
            Box::pin(async move {
                if !self.done {
                    self.done = true;
                    self.finished.send_modify(|count| *count += 1);
                }
                Ok(None)
            })
        }
    }

    fn queued(item: &str, turn: &str) -> SessionQueuedTurn {
        SessionQueuedTurn {
            item_id: item.into(),
            turn_id: turn.into(),
            input: pilotdeck_protocol::parse_json(r#"{"type":"text","text":"follow up"}"#).unwrap(),
            submit_options: JsValue::Object(Default::default()),
        }
    }

    #[tokio::test]
    async fn bound_session_restores_and_claims_durable_queue_once_in_fifo_order() -> RuntimeResult {
        let root =
            std::env::temp_dir().join(format!("pdrs-bound-queued-turns-{}", uuid::Uuid::new_v4()));
        let chat = root.join("chat");
        let adapter = Arc::new(AsyncTranscriptAdapter::new(
            Arc::new(crate::storage::JsonlTranscriptStore::new()),
            Arc::new(move |_| chat.clone()),
        ));
        let storage = || {
            adapter
                .create_session_storage(SessionStorageInput {
                    project_root: root.clone(),
                    pilot_home: root.clone(),
                    session_id: "s".into(),
                    now: None,
                })
                .map_err(runtime_error)
        };
        let live = Arc::new(LiveBindings::default());
        let first_inner = QueuedInner::new();
        let first = bind(
            live.clone(),
            SessionContext {
                session_key: "s".into(),
                project_key: None,
                channel_key: "test".into(),
                allowed_tools: None,
                disallowed_tools: None,
            },
            root.to_string_lossy().as_ref().into(),
            first_inner.clone(),
            storage()?,
            None,
            None,
        )
        .unwrap();
        let foreground = first.begin_activity().unwrap();
        first.enqueue_queued_turn(queued("i1", "t1")).unwrap();
        first.enqueue_queued_turn(queued("i2", "t2")).unwrap();
        first.stop_new();
        drop(foreground);
        drop(first);
        assert!(first_inner.submitted.lock().unwrap().is_empty());

        let resumed_inner = QueuedInner::new();
        let mut finished = resumed_inner.finished.subscribe();
        let resumed = bind(
            live,
            SessionContext {
                session_key: "s".into(),
                project_key: None,
                channel_key: "test".into(),
                allowed_tools: None,
                disallowed_tools: None,
            },
            root.to_string_lossy().as_ref().into(),
            resumed_inner.clone(),
            storage()?,
            None,
            None,
        )
        .unwrap();
        resumed.resume_queued_turns();
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            finished.wait_for(|count| *count == 2),
        )
        .await
        .expect("restored durable queue did not drain")
        .unwrap();

        assert_eq!(
            *resumed_inner.submitted.lock().unwrap(),
            vec![JsString::from("t1"), JsString::from("t2")]
        );
        let entries = storage()?.read_transcript(None).unwrap().entries;
        let starts = entries
            .iter()
            .filter(|entry| entry.get("type") == Some(&JsValue::from("turn_started")))
            .collect::<Vec<_>>();
        assert_eq!(starts.len(), 2);
        assert_eq!(
            starts
                .iter()
                .map(|entry| entry.get("inboxItemId").cloned().unwrap())
                .collect::<Vec<_>>(),
            vec![JsValue::from("i1"), JsValue::from("i2")]
        );
        assert_eq!(
            entries
                .iter()
                .filter(|entry| entry.get("type") == Some(&JsValue::from("accepted_input")))
                .count(),
            2
        );
        resumed.dispose("test_done".into()).await.unwrap();
        if root.exists() {
            std::fs::remove_dir_all(root).unwrap();
        }
        Ok(())
    }

    #[tokio::test]
    async fn bound_session_forwards_live_steer_and_cancel() -> RuntimeResult {
        struct Active;
        impl LiveSession for Active {
            fn steer(
                &self,
                turn: JsString,
                message: crate::runtime::steer::SteerMessage,
            ) -> crate::host::HostFuture<crate::runtime::steer::EnqueueResult> {
                assert_eq!(turn, JsString::from("t"));
                assert_eq!(message.item_id, JsString::from("i"));
                Box::pin(async {
                    Ok(crate::runtime::steer::EnqueueResult {
                        accepted: true,
                        reason: None,
                    })
                })
            }
            fn cancel_steer(
                &self,
                turn: JsString,
                item: JsString,
            ) -> crate::host::HostFuture<crate::runtime::steer::CancelResult> {
                assert_eq!(turn, JsString::from("t"));
                assert_eq!(item, JsString::from("i"));
                Box::pin(async {
                    Ok(crate::runtime::steer::CancelResult {
                        cancelled: true,
                        reason: None,
                    })
                })
            }
            fn snapshot(&self) -> JsValue {
                JsValue::Null
            }
            fn abort(&self, _: &JsString) -> RuntimeResult {
                Ok(())
            }
            fn dispose(&self) -> DisposeFuture<'_> {
                Box::pin(async { Ok(()) })
            }
        }
        let root =
            std::env::temp_dir().join(format!("pdrs-bound-control-{}", uuid::Uuid::new_v4()));
        let adapter = AsyncTranscriptAdapter::new(
            Arc::new(crate::storage::JsonlTranscriptStore::new()),
            Arc::new(|key| key.pilot_home.join("chats")),
        );
        let storage = adapter
            .create_session_storage(SessionStorageInput {
                project_root: root.clone(),
                pilot_home: root.clone(),
                session_id: "s".into(),
                now: None,
            })
            .map_err(runtime_error)?;
        let session = BoundSession {
            inner: Arc::new(Active),
            state: LiveSessionState::new(
                "s".into(),
                root.to_string_lossy().as_ref().into(),
                storage,
            )
            .unwrap(),
            live: Arc::new(LiveBindings::default()),
            execution_error: None,
            queued: Mutex::new(VecDeque::new()),
            input_template: Mutex::new(None),
        };
        assert!(
            session
                .steer(
                    "t".into(),
                    crate::runtime::steer::SteerMessage {
                        item_id: "i".into(),
                        message: JsValue::Null,
                        allowed_read_files: None
                    }
                )
                .await
                .unwrap()
                .accepted
        );
        assert!(
            session
                .cancel_steer("t".into(), "i".into())
                .await
                .unwrap()
                .cancelled
        );
        if root.exists() {
            std::fs::remove_dir_all(root).unwrap();
        }
        Ok(())
    }
}
