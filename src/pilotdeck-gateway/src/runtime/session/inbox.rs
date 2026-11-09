//! Durable ordinary follow-up inputs, using the existing AgentTurnInbox journal.
use crate::{runtime::project::RuntimeError, storage::recorder::Recorder};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::collections::VecDeque;

#[derive(Clone)]
pub(crate) struct QueuedTurn {
    pub item_id: JsString,
    pub turn_id: JsString,
    pub input: JsValue,
    pub submit_options: JsObject,
}
impl QueuedTurn {
    pub(crate) fn enqueue(
        &self,
        recorder: &Recorder,
        session: &JsString,
    ) -> Result<(), RuntimeError> {
        let mut event = JsObject::new();
        event.insert("type".into(), "agent_turn_enqueued".into());
        event.insert("itemId".into(), JsValue::String(self.item_id.clone()));
        event.insert("input".into(), self.input.clone());
        event.insert(
            "submitOptions".into(),
            JsValue::Object(self.submit_options.clone()),
        );
        recorder
            .writer()
            .record_session_event_checked(session, &self.turn_id, event)
            .map_err(|error| RuntimeError::new(error.to_string()))
    }
    pub(crate) fn discard(
        &self,
        recorder: &Recorder,
        session: &JsString,
    ) -> Result<(), RuntimeError> {
        let mut event = JsObject::new();
        event.insert("type".into(), "agent_turn_discarded".into());
        event.insert("itemId".into(), JsValue::String(self.item_id.clone()));
        event.insert("reason".into(), "agent_disposed".into());
        recorder
            .writer()
            .record_session_event_checked(session, &self.turn_id, event)
            .map_err(|error| RuntimeError::new(error.to_string()))
    }
}

#[derive(Default)]
pub(crate) struct InboxProjection {
    pub processed: usize,
    queues: std::collections::HashMap<JsString, VecDeque<QueuedTurn>>,
}
impl InboxProjection {
    pub(crate) fn apply(&mut self, event: &JsValue) -> Result<(), RuntimeError> {
        let kind = event.get("type").and_then(JsValue::as_string);
        let is_enqueue = kind.is_some_and(|kind| kind.equals_str("agent_turn_enqueued"));
        let is_discard = kind.is_some_and(|kind| kind.equals_str("agent_turn_discarded"));
        let is_claim = kind.is_some_and(|kind| kind.equals_str("turn_started"))
            && event.get("inboxItemId").is_some();
        if !(is_enqueue || is_discard || is_claim) {
            return Ok(());
        }
        let required = |key| {
            event
                .get(key)
                .and_then(JsValue::as_string)
                .cloned()
                .ok_or_else(|| RuntimeError::new("Durable queued turn identity is invalid."))
        };
        let session = required("sessionId")?;
        if is_enqueue {
            let entry = QueuedTurn {
                item_id: required("itemId")?,
                turn_id: required("turnId")?,
                input: event
                    .get("input")
                    .cloned()
                    .ok_or_else(|| RuntimeError::new("Durable queued turn input is missing."))?,
                submit_options: event
                    .get("submitOptions")
                    .and_then(JsValue::as_object)
                    .cloned()
                    .ok_or_else(|| RuntimeError::new("Durable queued turn options are invalid."))?,
            };
            self.queues.entry(session).or_default().push_back(entry);
        } else {
            let item = required(if is_claim { "inboxItemId" } else { "itemId" })?;
            if let Some(queue) = self.queues.get_mut(&session) {
                queue.retain(|entry| entry.item_id != item);
            }
        }
        Ok(())
    }
    pub(crate) fn pending(&self, session: &JsString) -> VecDeque<QueuedTurn> {
        self.queues.get(session).cloned().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::{StoreResult, TranscriptKey, TranscriptRead, TranscriptStore};
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    };
    #[derive(Default)]
    struct Store {
        entries: Mutex<Vec<JsValue>>,
        fail: AtomicBool,
    }
    impl TranscriptStore for Store {
        fn append(&self, _: &TranscriptKey, entry: &JsValue) -> StoreResult<()> {
            if self.fail.load(Ordering::SeqCst) {
                return Err(std::io::Error::other("injected append failure").into());
            }
            self.entries.lock().unwrap().push(entry.clone());
            Ok(())
        }
        fn read(&self, _: &TranscriptKey, _: Option<u64>) -> StoreResult<TranscriptRead> {
            Ok(TranscriptRead {
                entries: self.entries.lock().unwrap().clone(),
                diagnostics: vec![],
            })
        }
    }
    fn key() -> TranscriptKey {
        TranscriptKey {
            project_root: "/self-written".into(),
            pilot_home: "/self-written".into(),
            session_id: "child".into(),
            transcript_path: "/self-written/child.jsonl".into(),
        }
    }
    fn pending(entries: &[JsValue]) -> VecDeque<QueuedTurn> {
        let mut projection = InboxProjection::default();
        for entry in entries {
            projection.apply(entry).unwrap()
        }
        projection.pending(&"child".into())
    }
    fn value(item: &QueuedTurn) -> JsValue {
        let mut value = JsObject::new();
        value.insert("itemId".into(), JsValue::String(item.item_id.clone()));
        value.insert("turnId".into(), JsValue::String(item.turn_id.clone()));
        value.insert("input".into(), item.input.clone());
        value.insert(
            "submitOptions".into(),
            JsValue::Object(item.submit_options.clone()),
        );
        JsValue::Object(value)
    }
    #[test]
    fn actual_ts_durable_fifo_claim_restore_discard_and_failed_append() {
        let oracle = pilotdeck_protocol::parse_json(include_str!(
            "../../native_process/agent_continuation_oracle.json"
        ))
        .unwrap();
        let JsValue::Array(cases) = oracle.get("cases").unwrap() else {
            panic!("cases")
        };
        let first = cases[0].get("pending").unwrap();
        let JsValue::Array(items) = first else {
            panic!("pending")
        };
        let from = |value: &JsValue| QueuedTurn {
            item_id: value.get("itemId").unwrap().as_string().unwrap().clone(),
            turn_id: value.get("turnId").unwrap().as_string().unwrap().clone(),
            input: value.get("input").unwrap().clone(),
            submit_options: value
                .get("submitOptions")
                .unwrap()
                .as_object()
                .unwrap()
                .clone(),
        };
        let one = from(&items[0]);
        let two = from(&items[1]);
        let store = Arc::new(Store::default());
        let recorder = Recorder::new(store.clone(), key());
        one.enqueue(&recorder, &"child".into()).unwrap();
        two.enqueue(&recorder, &"child".into()).unwrap();
        assert_eq!(
            JsValue::Array(
                pending(&store.entries.lock().unwrap())
                    .iter()
                    .map(value)
                    .collect()
            ),
            *first
        );
        let before = store.entries.lock().unwrap().len();
        assert!(
            recorder
                .start_turn_with_inbox(&"child".into(), &two.turn_id, Some(&two.item_id))
                .is_err(),
            "a later queued input must not bypass FIFO"
        );
        assert_eq!(store.entries.lock().unwrap().len(), before);
        recorder
            .start_turn_with_inbox(&"child".into(), &one.turn_id, Some(&one.item_id))
            .unwrap();
        assert_eq!(
            JsValue::Array(
                pending(&store.entries.lock().unwrap())
                    .iter()
                    .map(value)
                    .collect()
            ),
            *cases[1].get("pending").unwrap()
        );
        let entries = store.entries.lock().unwrap().clone();
        assert_eq!(
            JsValue::Array(pending(&entries).iter().map(value).collect()),
            *cases[2].get("pending").unwrap()
        );
        two.discard(&recorder, &"child".into()).unwrap();
        assert!(pending(&store.entries.lock().unwrap()).is_empty());
        let store = Arc::new(Store::default());
        store.fail.store(true, Ordering::SeqCst);
        let recorder = Recorder::new(store.clone(), key());
        assert!(one.enqueue(&recorder, &"child".into()).is_err());
        assert!(store.entries.lock().unwrap().is_empty());
        assert!(pending(&store.entries.lock().unwrap()).is_empty());
    }
}
