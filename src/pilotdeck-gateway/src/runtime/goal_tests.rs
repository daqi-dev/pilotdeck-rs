use super::*;
use crate::{
    runtime::session::factory::FactoryStorage,
    storage::{JsonlTranscriptStore, LedgerError, TranscriptKey, TranscriptRead, TranscriptStore},
};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
};
struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
struct Store {
    inner: JsonlTranscriptStore,
    fail: AtomicBool,
}
impl TranscriptStore for Store {
    fn append(&self, key: &TranscriptKey, entry: &JsValue) -> crate::storage::StoreResult<()> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(LedgerError::Io(std::io::Error::other(
                "injected durable append failure",
            )));
        }
        self.inner.append(key, entry)
    }
    fn read(
        &self,
        key: &TranscriptKey,
        limit: Option<u64>,
    ) -> crate::storage::StoreResult<TranscriptRead> {
        self.inner.read(key, limit)
    }
}
fn fixture() -> (Scratch, Arc<Store>, FactoryStorage, Arc<GoalSession>) {
    let root = std::env::temp_dir().join(format!("pdrs-native-goal-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let key = TranscriptKey {
        project_root: root.clone(),
        pilot_home: root.clone(),
        session_id: "session".into(),
        transcript_path: root.join("session.jsonl"),
    };
    let store = Arc::new(Store {
        inner: JsonlTranscriptStore::new(),
        fail: AtomicBool::new(false),
    });
    let storage = FactoryStorage::new(store.clone(), key);
    let mut started = JsObject::new();
    started.insert("type".into(), "turn_started".into());
    storage
        .recorder
        .writer()
        .record_session_event_checked(&"session".into(), &"turn".into(), started)
        .unwrap();
    let mut runtime = GoalRuntime::new(
        "session".into(),
        storage.recorder.clone(),
        storage.projection.clone(),
    );
    Arc::get_mut(&mut runtime.inner).unwrap().uuid = Arc::new(|| "fixed-uuid".into());
    assert!(runtime.for_session(&"other".into()).is_none());
    (Scratch(root), store, storage, runtime.inner)
}
fn wire(value: &JsValue) -> serde_json::Value {
    serde_json::from_str(&pilotdeck_protocol::stringify_json(value).unwrap().unwrap()).unwrap()
}
fn snapshot(value: GoalState) -> serde_json::Value {
    wire(&crate::metadata::object([
        ("goal", value.goal),
        ("revision", JsValue::Number(value.revision)),
    ]))
}
#[tokio::test]
async fn fixed_ts_goal_cases_use_actual_durable_writer_and_projection() {
    let (_scratch, store, storage, session) = fixture();
    let oracle: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/native-goal/oracle.json"
    )))
    .unwrap();
    for row in oracle["rows"].as_array().unwrap() {
        let name = row["name"].as_str().unwrap();
        let input = pilotdeck_protocol::parse_json(&row["input"].to_string()).unwrap();
        store.fail.store(
            row["result"]["error"]["message"] == "injected durable append failure",
            Ordering::SeqCst,
        );
        let result = if name == "get_goal" {
            session.snapshot().map(|state| state.goal)
        } else {
            session
                .clone()
                .mutate(name == "create_goal", input, "turn".into())
                .await
        };
        store.fail.store(false, Ordering::SeqCst);
        if let Some(error) = row["result"].get("error") {
            let actual = result.expect_err(name);
            assert_eq!(
                actual.message.to_utf8().unwrap(),
                error["message"],
                "{name}"
            );
            if let Some(details) = error.get("details") {
                assert_eq!(
                    actual.details.map(|v| wire(&JsValue::Object(v))),
                    Some(details.clone())
                );
            }
        } else {
            assert_eq!(
                wire(&result.unwrap()),
                row["result"]["value"]["data"],
                "{name}"
            );
        }
        assert_eq!(
            snapshot(session.snapshot().unwrap()),
            row["snapshot"],
            "{name} committed snapshot"
        );
    }
    storage.recorder.writer().close();
}
#[tokio::test]
async fn goal_compare_and_swap_serializes_and_closed_writer_cannot_report_success() {
    let (_scratch, _store, storage, session) = fixture();
    session
        .clone()
        .mutate(
            true,
            pilotdeck_protocol::parse_json(r#"{"objective":"first"}"#).unwrap(),
            "turn".into(),
        )
        .await
        .unwrap();
    let input = pilotdeck_protocol::parse_json(
        r#"{"action":"edit","expected_revision":1,"objective":"next"}"#,
    )
    .unwrap();
    let (a, b) = tokio::join!(
        session.clone().mutate(false, input.clone(), "turn".into()),
        session.clone().mutate(false, input, "turn".into())
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    let error = a.err().or_else(|| b.err()).unwrap();
    assert_eq!(
        error.details.unwrap().get("actualRevision"),
        Some(&JsValue::Number(2.0))
    );
    let before = session.snapshot().unwrap();
    storage.recorder.writer().close();
    let error = session
        .clone()
        .mutate(
            false,
            pilotdeck_protocol::parse_json(r#"{"action":"clear"}"#).unwrap(),
            "closed".into(),
        )
        .await
        .unwrap_err();
    assert!(error.message.to_utf8().unwrap().contains("closed"));
    assert_eq!(session.snapshot().unwrap(), before);
}
#[test]
fn goal_trim_preserves_utf16_identity_and_rejects_unsafe_round_count() {
    let current = GoalState::default();
    let input =
        pilotdeck_protocol::parse_json(r#"{"objective":" \ud800 ","id":" \ud801 "}"#).unwrap();
    let (goal, _) = transition(&current, true, &input, || "unused".into()).unwrap();
    assert_eq!(
        goal.get("objective").unwrap().as_string().unwrap().units(),
        &[0xd800]
    );
    assert_eq!(
        goal.get("id").unwrap().as_string().unwrap().units(),
        &[0xd801]
    );
    let invalid =
        pilotdeck_protocol::parse_json(r#"{"objective":"x","max_goal_rounds":9007199254740992}"#)
            .unwrap();
    assert!(transition(&current, true, &invalid, || "unused".into()).is_err());
}

#[test]
fn goal_projection_omits_unknown_fields_and_blank_reason_without_rewriting_history() {
    let (scratch, _store, storage, session) = fixture();
    let mut event = JsObject::new();
    event.insert("type".into(), "goal_changed".into());
    event.insert("revision".into(), JsValue::Number(1.0));
    event.insert("goal".into(), pilotdeck_protocol::parse_json(r#"{"id":"fixture","revision":1,"objective":"retain raw history","phase":"active","blockedReason":"  ","unknownField":{"keep":"original"}}"#).unwrap());
    storage
        .recorder
        .writer()
        .record_session_event_checked(&"session".into(), &"turn".into(), event)
        .unwrap();
    let state = session.snapshot().unwrap();
    assert_eq!(
        wire(&state.goal),
        serde_json::json!({"id":"fixture","revision":1,"objective":"retain raw history","phase":"active"})
    );
    let bytes = std::fs::read_to_string(scratch.0.join("session.jsonl")).unwrap();
    assert!(bytes.contains("unknownField"));
    assert!(bytes.contains("blockedReason"));
    assert!(bytes.contains("original"));
}
