use super::*;
use crate::{
    runtime::replacement::{ReplacementSession, SubmitClaim},
    storage::{JsonlTranscriptStore, LedgerError, StoreResult, TranscriptKey, TranscriptStore},
};
use pilotdeck_protocol::{parse_json, stringify_json};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

// Same UUID-scoped temporary-directory pattern as the existing storage tests.
struct Fixture {
    root: PathBuf,
    history: Arc<HistoryStorage>,
    store: Arc<FaultStore>,
}
impl Fixture {
    fn new() -> Self {
        let root =
            std::env::temp_dir().join(format!("pdrs-session-history-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let store = Arc::new(FaultStore::default());
        let adapter = Arc::new(AsyncTranscriptAdapter::new(
            store.clone(),
            Arc::new(|p| p.pilot_home.join("chats")),
        ));
        let history = Arc::new(HistoryStorage::new(
            adapter,
            root.join("home"),
            root.join("project"),
            Arc::new(|| "2026-10-27T00:00:00.000Z".into()),
            Some(1000.0),
            Some(100.0),
        ));
        Self {
            root,
            history,
            store,
        }
    }
    fn input(&self) -> JsValue {
        object([("sessionKey", "s".into())])
    }
    fn storage(&self) -> SessionStorage {
        self.history.storage(&self.input()).unwrap()
    }
    fn write(&self, entries: &[JsValue]) {
        self.storage().replace_transcript(entries).unwrap().unwrap();
    }
    fn raw(&self) -> Vec<u8> {
        fs::read(self.storage().transcript_path).unwrap()
    }
    fn entries(&self) -> Vec<JsValue> {
        self.storage().read_transcript(None).unwrap().entries
    }
    fn prepare(&self) -> JsValue {
        self.history
            .prepare(&request(
                r#"{"sessionKey":"s","expectedTurnId":"b","replacementTurnId":"new"}"#,
            ))
            .unwrap()
    }
    fn finalize(&self, prepared: &JsValue, action: &str) -> JsValue {
        object([
            ("sessionKey", "s".into()),
            ("transactionId", field(prepared, "transactionId").clone()),
            ("action", action.into()),
        ])
    }
    fn routes(&self, session: Arc<Session>) -> (HistoryRoutes, ReplacementCoordinator) {
        let replacements =
            ReplacementCoordinator::new(self.history.replacement_storage(), session, 60_000, None);
        (
            HistoryRoutes::new(
                self.history.clone(),
                Arc::new(TurnEventCoordinator::new()),
                replacements.clone(),
            ),
            replacements,
        )
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[derive(Default)]
struct FaultStore {
    inner: JsonlTranscriptStore,
    fail: AtomicBool,
    uncertain: AtomicBool,
}
impl TranscriptStore for FaultStore {
    fn append(&self, key: &TranscriptKey, entry: &JsValue) -> StoreResult<()> {
        self.inner.append(key, entry)
    }
    fn read(&self, key: &TranscriptKey, max: Option<u64>) -> StoreResult<TranscriptRead> {
        self.inner.read(key, max)
    }
    fn replace(&self, key: &TranscriptKey, entries: &[JsValue]) -> Option<StoreResult<()>> {
        if self.fail.swap(false, Ordering::SeqCst) {
            return Some(Err(LedgerError::Io(io::Error::other(
                "injected replacement failure",
            ))));
        }
        let result = self.inner.replace(key, entries);
        if self.uncertain.swap(false, Ordering::SeqCst) {
            return Some(Err(LedgerError::Io(io::Error::other(
                "injected uncertain publication",
            ))));
        }
        result
    }
}
#[derive(Default)]
struct Session {
    active: Mutex<Option<JsString>>,
    fail_close: AtomicBool,
    closed: AtomicUsize,
    aborted: AtomicUsize,
}

struct ForkTransaction {
    commits: Arc<AtomicUsize>,
    rollbacks: Arc<AtomicUsize>,
}
impl ExecutionStorageFork for ForkTransaction {
    fn retarget_entry(
        &self,
        entry: JsValue,
        _source_session_id: &JsString,
        _target_session_id: &JsString,
    ) -> JsValue {
        let mut fields = entry.as_object().cloned().unwrap_or_default();
        fields.insert("executionRetargeted".into(), JsValue::Bool(true));
        JsValue::Object(fields)
    }
    fn commit(&self) -> ForkFuture<'_, ()> {
        Box::pin(async move {
            self.commits.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }
    fn rollback(&self) -> ForkFuture<'_, ()> {
        Box::pin(async move {
            self.rollbacks.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }
}

struct ForkTransactionPort {
    commits: Arc<AtomicUsize>,
    rollbacks: Arc<AtomicUsize>,
    targets: Arc<Mutex<Vec<JsString>>>,
}
impl ExecutionStorageForkPort for ForkTransactionPort {
    fn begin(
        &self,
        _source_session_id: JsString,
        target_session_id: JsString,
    ) -> ForkFuture<'_, Arc<dyn ExecutionStorageFork>> {
        self.targets.lock().unwrap().push(target_session_id);
        let transaction: Arc<dyn ExecutionStorageFork> = Arc::new(ForkTransaction {
            commits: self.commits.clone(),
            rollbacks: self.rollbacks.clone(),
        });
        Box::pin(async move { Ok(transaction) })
    }
}

impl ReplacementSession for Session {
    fn active_turn_run_id(&self, _: &JsString) -> Option<JsString> {
        self.active.lock().unwrap().clone()
    }
    fn has_active_turn(&self, _: &JsString) -> bool {
        self.active.lock().unwrap().is_some()
    }
    fn abort_expected_turn(
        &self,
        _: &JsString,
        _: &JsString,
    ) -> BoxFuture<Result<(), ReplacementError>> {
        self.aborted.fetch_add(1, Ordering::SeqCst);
        self.active.lock().unwrap().take();
        Box::pin(async { Ok(()) })
    }
    fn close_session(&self, _: &JsString) -> BoxFuture<Result<(), ReplacementError>> {
        self.closed.fetch_add(1, Ordering::SeqCst);
        let fail = self.fail_close.swap(false, Ordering::SeqCst);
        Box::pin(async move {
            if fail {
                Err(failure("injected close failure"))
            } else {
                Ok(())
            }
        })
    }
}
fn request(json: &str) -> JsValue {
    parse_json(json).unwrap()
}
fn entry(kind: &str, turn: &str, sequence: f64, body: JsValue) -> JsValue {
    let mut o = body.as_object().cloned().unwrap_or_default();
    for (key, value) in [
        ("type", kind.into()),
        ("sessionId", "s".into()),
        ("turnId", turn.into()),
        ("sequence", JsValue::Number(sequence)),
        (
            "createdAt",
            js(format!("2026-10-27T00:00:{:02}.000Z", sequence as u32)),
        ),
        ("entryId", js(format!("e{sequence}"))),
        (
            "parentEntryId",
            if sequence == 1.0 {
                JsValue::Null
            } else {
                js(format!("e{}", sequence - 1.0))
            },
        ),
    ] {
        o.insert(key.into(), value);
    }
    JsValue::Object(o)
}
fn accepted(turn: &str, sequence: f64, text: &str) -> JsValue {
    entry(
        "accepted_input",
        turn,
        sequence,
        object([(
            "messages",
            JsValue::Array(vec![object([
                ("role", "user".into()),
                (
                    "content",
                    JsValue::Array(vec![object([
                        ("type", "text".into()),
                        ("text", text.into()),
                    ])]),
                ),
            ])]),
        )]),
    )
}
fn assistant(turn: &str, sequence: f64, text: &str) -> JsValue {
    entry(
        "assistant_message",
        turn,
        sequence,
        object([(
            "message",
            object([
                ("role", "assistant".into()),
                (
                    "content",
                    JsValue::Array(vec![object([
                        ("type", "text".into()),
                        ("text", text.into()),
                    ])]),
                ),
            ]),
        )]),
    )
}
fn result(turn: &str, sequence: f64) -> JsValue {
    entry(
        "turn_result",
        turn,
        sequence,
        request(r#"{"result":{"type":"success","stopReason":"end_turn"}}"#),
    )
}
fn conversation() -> Vec<JsValue> {
    vec![
        accepted("a", 1.0, "first"),
        assistant("a", 2.0, "answer"),
        result("a", 3.0),
        accepted("b", 4.0, "second"),
        assistant("b", 5.0, "last"),
        result("b", 6.0),
    ]
}
fn code(result: ReplacementResult) -> Option<&'static str> {
    result.unwrap_err().code
}

#[test]
fn disk_messages_page_at_web_boundaries_and_ignore_direction() {
    let f = Fixture::new();
    f.write(&conversation());
    let page = f
        .history
        .read_session_messages(&request(
            r#"{"sessionKey":"s","cursor":"1tail","limit":2,"direction":"backward"}"#,
        ))
        .unwrap();
    assert_eq!(number(&page, "total"), 4.0);
    assert_eq!(text(&page, "nextCursor"), "3");
    assert_eq!(text(&array(field(&page, "messages"))[0], "text"), "answer");
    let all = f
        .history
        .read_session_messages(&request(r#"{"sessionKey":"s","cursor":"-9","limit":0}"#))
        .unwrap();
    assert_eq!(array(field(&all, "messages")).len(), 4);
    assert!(field(&all, "nextCursor").is_undefined());
    assert_eq!(text(field(&all, "session"), "sessionKey"), "s");
}

#[test]
fn list_reads_cold_disk_orders_pages_and_returns_full_page_cursor() {
    let f = Fixture::new();
    f.write(&conversation());
    let other = f
        .history
        .storage(&request(r#"{"sessionKey":"other"}"#))
        .unwrap();
    other
        .replace_transcript(&[accepted("z", 1.0, "other prompt")])
        .unwrap()
        .unwrap();
    let all = f.history.list_sessions(&request(r#"{"limit":0}"#)).unwrap();
    assert_eq!(array(field(&all, "sessions")).len(), 2);
    let first = f.history.list_sessions(&request(r#"{"limit":1}"#)).unwrap();
    assert_eq!(text(&first, "nextCursor"), "1");
    let second = f
        .history
        .list_sessions(&request(r#"{"limit":1,"cursor":"1"}"#))
        .unwrap();
    assert_eq!(text(&second, "nextCursor"), "2");
    assert_ne!(
        field(&array(field(&first, "sessions"))[0], "sessionId"),
        field(&array(field(&second, "sessions"))[0], "sessionId")
    );
}

#[test]
fn input_paths_and_project_types_are_checked_before_io() {
    let f = Fixture::new();
    for json in [
        r#"{}"#,
        r#"{"sessionKey":".."}"#,
        r#"{"sessionKey":"s","projectKey":false}"#,
        r#"{"sessionKey":"s","limit":"2"}"#,
    ] {
        assert!(f.history.read_session_messages(&request(json)).is_err());
    }
    for path in [
        "../escape.jsonl",
        "/outside.jsonl",
        "notes.txt",
        "x/../../escape.jsonl",
    ] {
        let mut input = f.input().as_object().cloned().unwrap();
        input.insert("sessionKind".into(), "background_task".into());
        input.insert("relativeTranscriptPath".into(), path.into());
        assert!(
            f.history
                .read_session_messages(&JsValue::Object(input))
                .is_err()
        );
    }
}

#[cfg(unix)]
#[test]
fn symlink_subagent_and_background_paths_cannot_escape() {
    let f = Fixture::new();
    f.write(&conversation());
    let outside = f.root.join("outside.jsonl");
    fs::write(&outside, b"secret").unwrap();
    std::os::unix::fs::symlink(outside, f.storage().chat_dir.join("link.jsonl")).unwrap();
    assert!(f.history.read_session_messages(&request(r#"{"sessionKey":"s","sessionKind":"background_task","relativeTranscriptPath":"link.jsonl"}"#)).is_err());
}

#[test]
fn subagent_requires_durable_reference_and_hides_input_prelude() {
    let f = Fixture::new();
    let mut entries = conversation();
    entries.push(entry(
        "subagent_started",
        "b",
        7.0,
        request(r#"{"subagentId":"child","transcriptRelativePath":"s/subagents/child.jsonl"}"#),
    ));
    f.write(&entries);
    let storage = f.storage();
    fs::create_dir_all(&storage.subagents_dir).unwrap();
    let child = storage.subagent_transcript_path(&"child".into());
    fs::write(
        child,
        [
            accepted("c", 1.0, "hidden fork prelude"),
            assistant("c", 2.0, "child output"),
            result("c", 3.0),
        ]
        .iter()
        .map(|v| format!("{}\n", stringify_json(v).unwrap().unwrap()))
        .collect::<String>(),
    )
    .unwrap();
    let read = f
        .history
        .read_subagent_messages(&request(r#"{"sessionKey":"s","subagentId":"child"}"#))
        .unwrap();
    assert_eq!(number(&read, "total"), 1.0);
    let message = &array(field(&read, "messages"))[0];
    assert_eq!(text(message, "text"), "child output");
    assert_eq!(text(message, "sessionKey"), "s::sub::child");
    let absent = f
        .history
        .read_subagent_messages(&request(r#"{"sessionKey":"s","subagentId":"unknown"}"#))
        .unwrap();
    assert_eq!(number(&absent, "total"), 0.0);
}

#[test]
fn subagent_session_id_reads_through_the_transcript_provider() {
    let f = Fixture::new();
    let mut parent = conversation();
    parent.push(entry(
        "subagent_started",
        "b",
        7.0,
        request(r#"{"subagentId":"child","subagentSessionId":"child-session"}"#),
    ));
    f.write(&parent);
    let child = f
        .history
        .storage(&request(r#"{"sessionKey":"child-session"}"#))
        .unwrap();
    child
        .replace_transcript(&[
            accepted("c", 1.0, "hidden fork prelude"),
            assistant("c", 2.0, "provider child output"),
            result("c", 3.0),
        ])
        .unwrap()
        .unwrap();

    let read = f
        .history
        .read_subagent_messages(&request(r#"{"sessionKey":"s","subagentId":"child"}"#))
        .unwrap();
    assert_eq!(number(&read, "total"), 1.0);
    let message = &array(field(&read, "messages"))[0];
    assert_eq!(text(message, "text"), "provider child output");
    assert_eq!(text(message, "sessionKey"), "s::sub::child");
}

#[test]
fn background_history_uses_scoped_relative_path() {
    let f = Fixture::new();
    f.write(&conversation());
    let relative = "s/background/task.jsonl";
    let path = f.storage().chat_dir.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        path,
        format!(
            "{}\n",
            stringify_json(&assistant("task", 1.0, "background output"))
                .unwrap()
                .unwrap()
        ),
    )
    .unwrap();
    let read=f.history.read_session_messages(&request(r#"{"sessionKey":"task","sessionKind":"background_task","relativeTranscriptPath":"s/background/task.jsonl","parentSessionId":"s"}"#)).unwrap();
    assert_eq!(
        text(&array(field(&read, "messages"))[0], "text"),
        "background output"
    );
    assert_eq!(text(field(&read, "session"), "parentSessionId"), "s");
}

#[tokio::test]
async fn fork_user_prefill_is_persistent_and_records_parent_and_turn() {
    let f = Fixture::new();
    f.write(&conversation());
    let original = f.raw();
    let fork = f
        .history
        .fork(&request(r#"{"sessionKey":"s","fromEntryId":"e4"}"#))
        .await
        .unwrap();
    assert_eq!(text(&fork, "prefillText"), "second");
    assert_eq!(number(&fork, "carriedMessageCount"), 2.0);
    let key = field(&fork, "newSessionKey").clone();
    let storage = f
        .history
        .storage(&object([("sessionKey", key.clone())]))
        .unwrap();
    let entries = storage.read_transcript(None).unwrap().entries;
    assert_eq!(entries.len(), 4);
    assert_eq!(
        text(
            field(entries.last().unwrap(), "metadata"),
            "parentSessionId"
        ),
        "s"
    );
    assert_eq!(
        text(
            field(entries.last().unwrap(), "metadata"),
            "forkedFromTurnId"
        ),
        "b"
    );
    assert!(entries.iter().all(|e| field(e, "sessionId") == &key));
    assert_eq!(
        text(
            field(
                field(&array(field(&entries[0], "messages"))[0], "metadata"),
                "forkCarryover"
            ),
            "sourceSessionId"
        ),
        "s"
    );
    assert_eq!(f.raw(), original);
}

#[tokio::test]
async fn fork_assistant_keeps_completion_without_later_messages() {
    let f = Fixture::new();
    let mut entries = conversation();
    entries.insert(2, assistant("a", 2.5, "not selected"));
    f.write(&entries);
    let fork = f
        .history
        .fork(&request(r#"{"sessionKey":"s","fromEntryId":"e2"}"#))
        .await
        .unwrap();
    assert_eq!(text(&fork, "prefillText"), "");
    let dest = f
        .history
        .storage(&object([(
            "sessionKey",
            field(&fork, "newSessionKey").clone(),
        )]))
        .unwrap();
    let entries = dest.read_transcript(None).unwrap().entries;
    assert!(entries.iter().any(|e| is(e, "type", "turn_result")));
    assert!(!entries.iter().any(|e| is(e, "type", "assistant_message")
        && text(field(e, "message"), "content") == "not selected"));
    assert!(!entries.iter().any(|e| number(e, "sequence") == 2.5));
}

#[tokio::test]
async fn nested_fork_carryover_uses_immediate_parent_session() {
    let f = Fixture::new();
    f.write(&conversation());
    let first = f
        .history
        .fork(&request(r#"{"sessionKey":"s","fromEntryId":"e4"}"#))
        .await
        .unwrap();
    let parent = field(&first, "newSessionKey").clone();
    let parent_entries = f
        .history
        .storage(&object([("sessionKey", parent.clone())]))
        .unwrap()
        .read_transcript(None)
        .unwrap()
        .entries;
    let fork_from = text(
        parent_entries
            .iter()
            .find(|entry| is(entry, "type", "assistant_message"))
            .expect("forked parent must retain assistant context"),
        "entryId",
    );
    let second = f
        .history
        .fork(&object([
            ("sessionKey", parent.clone()),
            ("fromEntryId", fork_from.as_str().into()),
        ]))
        .await
        .unwrap();
    let child = field(&second, "newSessionKey").clone();
    let entries = f
        .history
        .storage(&object([("sessionKey", child.clone())]))
        .unwrap()
        .read_transcript(None)
        .unwrap()
        .entries;
    let carryover = field(
        field(&array(field(&entries[0], "messages"))[0], "metadata"),
        "forkCarryover",
    );
    assert_eq!(field(carryover, "sourceSessionId"), &parent);
    assert_ne!(text(carryover, "sourceSessionId"), "s");
}

#[tokio::test]
async fn fork_marks_compact_snapshot_messages_as_carryover() {
    let f = Fixture::new();
    let compact = entry(
        "control_boundary",
        "a",
        2.0,
        object([(
            "boundary",
            object([
                ("kind", "compact".into()),
                ("subtype", "compact_boundary".into()),
                (
                    "snapshot",
                    object([
                        ("version", JsValue::Number(1.0)),
                        (
                            "messages",
                            JsValue::Array(vec![object([
                                ("role", "assistant".into()),
                                (
                                    "content",
                                    JsValue::Array(vec![object([
                                        ("type", "text".into()),
                                        ("text", "retained".into()),
                                    ])]),
                                ),
                            ])]),
                        ),
                    ]),
                ),
            ]),
        )]),
    );
    f.write(&[accepted("a", 1.0, "hello"), compact]);
    let fork = f
        .history
        .fork(&request(r#"{"sessionKey":"s","fromEntryId":"e2"}"#))
        .await
        .unwrap();
    let dest = f
        .history
        .storage(&object([(
            "sessionKey",
            field(&fork, "newSessionKey").clone(),
        )]))
        .unwrap();
    let entries = dest.read_transcript(None).unwrap().entries;
    let compact = entries
        .iter()
        .find(|entry| is(entry, "type", "control_boundary"))
        .unwrap();
    let message = &array(field(
        field(field(compact, "boundary"), "snapshot"),
        "messages",
    ))[0];
    let carryover = field(field(message, "metadata"), "forkCarryover");
    assert_eq!(text(carryover, "sourceSessionId"), "s");
    assert_eq!(text(carryover, "sourceTurnId"), "a");
}

#[tokio::test]
async fn production_fork_port_persists_retargeted_entries_and_commits_execution_storage() {
    let f = Fixture::new();
    f.write(&conversation());
    let source = f.storage();
    fs::create_dir_all(&source.subagents_dir).unwrap();
    fs::write(
        source.subagent_transcript_path(&"child".into()),
        format!(
            "{}\n",
            stringify_json(&assistant("child-turn", 1.0, "child output"))
                .unwrap()
                .unwrap()
        ),
    )
    .unwrap();
    assert!(f.history.session_fork_port.is_some());
    let commits = Arc::new(AtomicUsize::new(0));
    let rollbacks = Arc::new(AtomicUsize::new(0));
    let targets = Arc::new(Mutex::new(Vec::new()));
    let history = HistoryStorage::new(
        f.history.adapter.clone(),
        f.history.pilot_home.clone(),
        f.history.fallback_project_root.clone(),
        f.history.now.clone(),
        f.history.max_context_tokens,
        f.history.max_output_tokens,
    )
    .with_execution_fork_port(Arc::new(ForkTransactionPort {
        commits: commits.clone(),
        rollbacks: rollbacks.clone(),
        targets: targets.clone(),
    }));

    let result = history
        .fork(&request(r#"{"sessionKey":"s","fromEntryId":"e2"}"#))
        .await
        .unwrap();
    let target = field(&result, "newSessionKey").as_string().unwrap();
    assert_eq!(
        targets.lock().unwrap().as_slice(),
        std::slice::from_ref(target)
    );
    let storage = history
        .storage(&object([("sessionKey", JsValue::String(target.clone()))]))
        .unwrap();
    let entries = storage.read_transcript(None).unwrap().entries;
    assert!(
        entries[..entries.len() - 1]
            .iter()
            .all(|entry| field(entry, "executionRetargeted") == &JsValue::Bool(true))
    );
    assert!(field(entries.last().unwrap(), "executionRetargeted").is_undefined());
    let child = storage
        .read_transcript_at_path(&storage.subagent_transcript_path(&"child".into()), None)
        .unwrap()
        .entries;
    assert_eq!(
        field(&child[0], "executionRetargeted"),
        &JsValue::Bool(true)
    );
    assert_eq!(commits.load(Ordering::SeqCst), 1);
    assert_eq!(rollbacks.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn production_fork_failure_rolls_back_execution_storage() {
    let f = Fixture::new();
    f.write(&conversation());
    let commits = Arc::new(AtomicUsize::new(0));
    let rollbacks = Arc::new(AtomicUsize::new(0));
    let targets = Arc::new(Mutex::new(Vec::new()));
    let history = HistoryStorage::new(
        f.history.adapter.clone(),
        f.history.pilot_home.clone(),
        f.history.fallback_project_root.clone(),
        f.history.now.clone(),
        f.history.max_context_tokens,
        f.history.max_output_tokens,
    )
    .with_execution_fork_port(Arc::new(ForkTransactionPort {
        commits: commits.clone(),
        rollbacks: rollbacks.clone(),
        targets: targets.clone(),
    }));
    f.store.fail.store(true, Ordering::SeqCst);

    assert!(
        history
            .fork(&request(r#"{"sessionKey":"s","fromEntryId":"e2"}"#))
            .await
            .is_err()
    );
    assert_eq!(commits.load(Ordering::SeqCst), 0);
    assert_eq!(rollbacks.load(Ordering::SeqCst), 1);
    let target = targets.lock().unwrap()[0].clone();
    let target = history
        .storage(&object([("sessionKey", JsValue::String(target))]))
        .unwrap();
    assert!(!target.transcript_path.exists());
}

#[tokio::test]
async fn fork_guard_and_attachment_errors_do_not_write_targets() {
    let f = Fixture::new();
    f.write(&conversation());
    let original = f.raw();
    assert_eq!(
        code(
            f.history
                .fork(&request(r#"{"sessionKey":"s","fromEntryId":"missing"}"#))
                .await
        ),
        Some("fork_entry_not_found")
    );
    assert_eq!(
        code(
            f.history
                .fork(&request(
                    r#"{"sessionKey":"s","fromEntryId":"e2","resumeDropsTurn":"e1"}"#,
                ))
                .await,
        ),
        Some("resume_drops_turn_invalid")
    );
    assert_eq!(
        code(
            f.history
                .fork(&request(
                    r#"{"sessionKey":"s","fromEntryId":"e1","resumeDropsTurn":"e4"}"#,
                ))
                .await,
        ),
        Some("resume_drops_turn_mismatch")
    );
    assert_eq!(f.raw(), original);
    let input = entry(
        "accepted_input",
        "a",
        1.0,
        request(
            r#"{"messages":[{"role":"user","content":[{"type":"image","mimeType":"image/png","data":"AA"}]}]}"#,
        ),
    );
    f.write(&[input]);
    assert_eq!(
        code(
            f.history
                .fork(&request(r#"{"sessionKey":"s","fromEntryId":"e1"}"#))
                .await
        ),
        Some("fork_unsupported_content")
    );
    assert!(
        f.history
            .fork(&request(
                r#"{"sessionKey":"s","fromEntryId":"e1","resumeAt":true}"#
            ))
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn fork_copies_auxiliary_files_and_retargets_references() {
    let f = Fixture::new();
    let source = f.storage();
    fs::create_dir_all(&source.tool_results_dir).unwrap();
    fs::write(source.tool_results_dir.join("result.txt"), b"tool bytes").unwrap();
    fs::create_dir_all(&source.file_history_dir).unwrap();
    fs::write(source.file_history_dir.join("checkpoint"), b"backup bytes").unwrap();
    let path = source.tool_results_dir.join("result.txt");
    let block = object([
        ("type", "tool_result_reference".into()),
        ("path", path.to_string_lossy().as_ref().into()),
        ("preview", "tool".into()),
    ]);
    let output = entry(
        "assistant_message",
        "a",
        2.0,
        object([(
            "message",
            object([
                ("role", "assistant".into()),
                ("content", JsValue::Array(vec![block])),
            ]),
        )]),
    );
    f.write(&[accepted("a", 1.0, "hello"), output, result("a", 3.0)]);
    let fork = f
        .history
        .fork(&request(r#"{"sessionKey":"s","fromEntryId":"e2"}"#))
        .await
        .unwrap();
    let dest = f
        .history
        .storage(&object([(
            "sessionKey",
            field(&fork, "newSessionKey").clone(),
        )]))
        .unwrap();
    assert_eq!(
        fs::read(dest.tool_results_dir.join("result.txt")).unwrap(),
        b"tool bytes"
    );
    assert_eq!(
        fs::read(dest.file_history_dir.join("checkpoint")).unwrap(),
        b"backup bytes"
    );
    let read = dest.read_transcript(None).unwrap();
    let copied = &array(field(field(&read.entries[1], "message"), "content"))[0];
    assert_eq!(
        PathBuf::from(text(copied, "path")),
        dest.tool_results_dir.join("result.txt")
    );
}

#[test]
fn replacement_rollback_restores_exact_bytes_and_second_rollback_fails() {
    let f = Fixture::new();
    f.write(&conversation());
    let mut raw = f.raw();
    raw.extend_from_slice(b"  \n");
    fs::write(f.storage().transcript_path, &raw).unwrap();
    let prepared = f.prepare();
    assert_eq!(number(&prepared, "removedEntryCount"), 3.0);
    assert_eq!(f.entries().len(), 3);
    let finalize = f.finalize(&prepared, "rollback");
    f.history.finalize(&finalize).unwrap();
    assert_eq!(f.raw(), raw);
    assert_eq!(
        code(f.history.finalize(&finalize)),
        Some("replace_transaction_missing")
    );
    assert_eq!(f.raw(), raw);
}

#[test]
fn replacement_commit_is_storage_idempotent_and_does_not_restore_old_turn() {
    let f = Fixture::new();
    f.write(&conversation());
    let prepared = f.prepare();
    let truncated = f.raw();
    let finalize = f.finalize(&prepared, "commit");
    f.history.finalize(&finalize).unwrap();
    f.history.finalize(&finalize).unwrap();
    assert_eq!(f.raw(), truncated);
    assert_eq!(
        code(f.history.finalize(&f.finalize(&prepared, "rollback"))),
        Some("replace_transaction_missing")
    );
}

#[test]
fn replacement_rejects_conflicts_corruption_and_invalid_transaction() {
    let f = Fixture::new();
    f.write(&conversation());
    let original = f.raw();
    assert_eq!(
        code(f.history.prepare(&request(
            r#"{"sessionKey":"s","expectedTurnId":"old","replacementTurnId":"new"}"#
        ))),
        Some("replace_turn_conflict")
    );
    assert_eq!(f.raw(), original);
    assert_eq!(
        code(f.history.finalize(&request(
            r#"{"sessionKey":"s","transactionId":"../../escape","action":"rollback"}"#
        ))),
        Some("replace_invalid_transaction")
    );
    let mut corrupt = original.clone();
    corrupt.extend_from_slice(b"{broken}\n");
    fs::write(f.storage().transcript_path, &corrupt).unwrap();
    assert_eq!(
        code(f.history.prepare(&request(
            r#"{"sessionKey":"s","expectedTurnId":"b","replacementTurnId":"new"}"#
        ))),
        Some("replace_invalid_transcript")
    );
    assert_eq!(f.raw(), corrupt);
}

#[test]
fn first_turn_replacement_keeps_user_metadata_and_discards_generated_title() {
    let f = Fixture::new();
    let meta = entry(
        "session_metadata",
        "metadata",
        0.5,
        request(
            r#"{"metadata":{"title":"pinned","aiTitle":"generated","tag":"keep","lastPrompt":"old","firstPrompt":"old"}}"#,
        ),
    );
    f.write(&[meta, accepted("a", 1.0, "old"), result("a", 2.0)]);
    f.history
        .prepare(&request(
            r#"{"sessionKey":"s","expectedTurnId":"a","replacementTurnId":"new"}"#,
        ))
        .unwrap();
    let metadata = projection::metadata(&f.entries());
    assert_eq!(text(&metadata, "title"), "pinned");
    assert_eq!(text(&metadata, "tag"), "keep");
    assert!(field(&metadata, "aiTitle").is_undefined());
    let entries = f.entries();
    let snapshot = field(entries.last().unwrap(), "metadata");
    assert!(field(snapshot, "lastPrompt").is_undefined());
    assert!(field(snapshot, "firstPrompt").is_undefined());
}

#[test]
fn failed_replace_preserves_original_and_uncertain_write_retains_backup() {
    let f = Fixture::new();
    f.write(&conversation());
    let original = f.raw();
    f.store.fail.store(true, Ordering::SeqCst);
    assert!(
        f.history
            .prepare(&request(
                r#"{"sessionKey":"s","expectedTurnId":"b","replacementTurnId":"new"}"#
            ))
            .is_err()
    );
    assert_eq!(f.raw(), original);
    f.store.uncertain.store(true, Ordering::SeqCst);
    assert!(
        f.history
            .prepare(&request(
                r#"{"sessionKey":"s","expectedTurnId":"b","replacementTurnId":"new"}"#
            ))
            .is_err()
    );
    let artifacts = fs::read_dir(f.storage().chat_dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect::<Vec<_>>();
    let backup = artifacts
        .iter()
        .find(|p| p.extension().is_some_and(|e| e == "bak"))
        .unwrap();
    assert_eq!(fs::read(backup).unwrap(), original);
}

#[test]
fn accepted_replacement_cannot_be_rolled_back() {
    let f = Fixture::new();
    f.write(&conversation());
    let prepared = f.prepare();
    let mut entries = f.entries();
    entries.push(accepted("new", 4.0, "accepted replacement"));
    f.write(&entries);
    let accepted = f.raw();
    assert_eq!(
        code(f.history.finalize(&f.finalize(&prepared, "rollback"))),
        Some("replace_transaction_active")
    );
    assert_eq!(f.raw(), accepted);
}

fn mark_owner_dead(f: &Fixture, prepared: &JsValue) {
    let safe = "s";
    let id = text(prepared, "transactionId");
    let journal = f
        .storage()
        .chat_dir
        .join(format!(".{safe}.{id}.replace.json"));
    let mut value = parse_json(&fs::read_to_string(&journal).unwrap())
        .unwrap()
        .as_object()
        .cloned()
        .unwrap();
    value.insert("version".into(), JsValue::Number(1.0));
    value.insert("owner".into(), JsValue::Undefined);
    fs::write(
        journal,
        stringify_json(&JsValue::Object(value)).unwrap().unwrap(),
    )
    .unwrap();
}
#[test]
fn recovery_rolls_back_or_commits_from_durable_accepted_input() {
    let f = Fixture::new();
    f.write(&conversation());
    let original = f.raw();
    let prepared = f.prepare();
    f.history.recover_replacements(&f.input()).unwrap();
    assert_ne!(f.raw(), original);
    mark_owner_dead(&f, &prepared);
    f.history.recover_replacements(&f.input()).unwrap();
    assert_eq!(f.raw(), original);
    let prepared = f.prepare();
    let mut entries = f.entries();
    entries.push(accepted("new", 4.0, "new"));
    f.write(&entries);
    let committed = f.raw();
    mark_owner_dead(&f, &prepared);
    f.history.recover_replacements(&f.input()).unwrap();
    assert_eq!(f.raw(), committed);
    assert!(f.history.finalize(&f.finalize(&prepared, "commit")).is_ok());
}

fn recovery_artifact(f: &Fixture, original: &[u8], journal: &str) -> (PathBuf, PathBuf) {
    let id = uuid::Uuid::new_v4().to_string();
    let backup = f.storage().chat_dir.join(format!(".s.{id}.replace.bak"));
    let path = f.storage().chat_dir.join(format!(".s.{id}.replace.json"));
    fs::write(&backup, original).unwrap();
    fs::write(&path, journal.replace("TRANSACTION_ID", &id)).unwrap();
    (backup, path)
}

fn artifact_modified(path: &Path, seconds: u64) {
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds))
        .unwrap();
}

#[test]
fn recovery_orders_real_backups_by_prepared_at_and_falls_back_to_artifact_mtime() {
    // TS NodeProjectSessionReplacementPort.ts:167-180 chooses max(Date.parse,
    // backup mtime, journal mtime), rather than only the latest file write.
    for (date, selected) in [
        ("2040-03-01T00:00:00.000Z", "prepared"),
        ("not a date", "mtime"),
    ] {
        let f = Fixture::new();
        f.write(&conversation());
        let prepared_body = b"prepared transaction's original transcript\r\n";
        let mtime_body = b"mtime transaction's original transcript\n";
        let (prepared_backup, prepared_journal) = recovery_artifact(
            &f,
            prepared_body,
            &format!(
                r#"{{"version":1,"transactionId":"TRANSACTION_ID","sessionKey":"s","replacementTurnId":"new","preparedAt":"{date}"}}"#
            ),
        );
        let (mtime_backup, mtime_journal) = recovery_artifact(
            &f,
            mtime_body,
            r#"{"version":1,"transactionId":"TRANSACTION_ID","sessionKey":"s","replacementTurnId":"new","preparedAt":"2020-01-01T00:00:00.000Z"}"#,
        );
        for path in [&prepared_backup, &prepared_journal] {
            artifact_modified(path, 1_600_000_000);
        }
        for path in [&mtime_backup, &mtime_journal] {
            artifact_modified(path, 1_700_000_000);
        }
        f.history.recover_replacements(&f.input()).unwrap();
        assert_eq!(
            f.raw(),
            if selected == "prepared" {
                prepared_body.as_slice()
            } else {
                mtime_body.as_slice()
            },
            "preparedAt={date}"
        );
        for path in [
            prepared_backup,
            prepared_journal,
            mtime_backup,
            mtime_journal,
        ] {
            assert!(!path.exists());
        }
    }
}

#[test]
fn recovery_invalid_journals_use_real_backup_and_current_accepted_inputs() {
    // Every invalid journal must use the backup-difference fallback, including
    // an owner that would otherwise look alive. TS:85-118,257-267.
    for journal in [
        "{broken",
        "null",
        r#"{"version":3,"transactionId":"TRANSACTION_ID","sessionKey":"s","replacementTurnId":"a","preparedAt":"2040-01-01"}"#,
        r#"{"version":1,"transactionId":"wrong","sessionKey":"s","replacementTurnId":"a","preparedAt":"2040-01-01"}"#,
        r#"{"version":1,"transactionId":"TRANSACTION_ID","sessionKey":1,"replacementTurnId":"a","preparedAt":"2040-01-01"}"#,
        r#"{"version":1,"transactionId":"TRANSACTION_ID","sessionKey":"\ufeff","replacementTurnId":"a","preparedAt":"2040-01-01"}"#,
        r#"{"version":1,"transactionId":"TRANSACTION_ID","sessionKey":"s","replacementTurnId":1,"preparedAt":"2040-01-01"}"#,
        r#"{"version":1,"transactionId":"TRANSACTION_ID","sessionKey":"s","replacementTurnId":"a"}"#,
        r#"{"version":1,"transactionId":"TRANSACTION_ID","sessionKey":"s","replacementTurnId":"a","preparedAt":123}"#,
        r#"{"version":2,"transactionId":"TRANSACTION_ID","sessionKey":"s","replacementTurnId":"a","preparedAt":"2040-01-01","owner":null}"#,
        r#"{"version":2,"transactionId":"TRANSACTION_ID","sessionKey":"s","replacementTurnId":"a","preparedAt":"2040-01-01","owner":{"instanceId":"x","pid":1.5}}"#,
        r#"{"version":2,"transactionId":"TRANSACTION_ID","sessionKey":"s","replacementTurnId":"a","preparedAt":"2040-01-01","owner":{"instanceId":"x","pid":0}}"#,
        r#"{"version":2,"transactionId":"TRANSACTION_ID","sessionKey":"s","replacementTurnId":"a","preparedAt":"2040-01-01","owner":{"instanceId":"x","pid":9007199254740992}}"#,
    ] {
        for committed in [false, true] {
            let f = Fixture::new();
            f.write(&conversation());
            let original = f.raw();
            let (backup, path) = recovery_artifact(&f, &original, journal);
            f.write(&[accepted("a", 1.0, "first")]);
            if committed {
                let mut entries = f.entries();
                entries.push(accepted("different-from-journal", 2.0, "durable new input"));
                f.write(&entries);
            }
            let current = f.raw();
            f.history.recover_replacements(&f.input()).unwrap();
            assert_eq!(f.raw(), if committed { current } else { original });
            assert!(!backup.exists());
            assert!(!path.exists());
        }
    }
    let f = Fixture::new();
    f.write(&conversation());
    let original = f.raw();
    let (backup, journal) = recovery_artifact(
        &f,
        &original,
        &format!(
            r#"{{"version":2,"transactionId":"TRANSACTION_ID","sessionKey":"s","replacementTurnId":"a","preparedAt":"2040-01-01","owner":{{"instanceId":" ","pid":{}}}}}"#,
            std::process::id()
        ),
    );
    f.write(&[accepted("a", 1.0, "first")]);
    f.history.recover_replacements(&f.input()).unwrap();
    assert_eq!(f.raw(), original, "invalid owner must not skip recovery");
    assert!(!backup.exists());
    assert!(!journal.exists());
}

#[test]
fn recovery_uses_ts_raw_line_evidence_and_decodes_backup_as_utf8() {
    for (body, committed) in [
        (
            "{broken}\n{\"type\":\"accepted_input\",\"turnId\":\"new\"}\r\n",
            true,
        ),
        ("{\"type\":\"accepted_input\",\"turnId\":\"new\"\n", false),
        ("{\"type\": \"accepted_input\",\"turnId\":\"new\"}\n", false),
        ("{\"type\":\"accepted_input\",\"turnId\":7}\n", false),
        (
            "{\"type\":\"accepted_input\",\"turnId\":\"\\ud800\"}\n",
            false,
        ),
    ] {
        let f = Fixture::new();
        f.write(&conversation());
        let original = b"original transcript\r\ninvalid UTF8: \xff\n";
        let (backup, journal) = recovery_artifact(
            &f,
            original,
            r#"{"version":1,"transactionId":"TRANSACTION_ID","sessionKey":"s","replacementTurnId":"new","preparedAt":"invalid date"}"#,
        );
        fs::write(f.storage().transcript_path, body).unwrap();
        f.history.recover_replacements(&f.input()).unwrap();
        assert_eq!(
            f.raw(),
            if committed {
                body.as_bytes().to_vec()
            } else {
                String::from_utf8_lossy(original).as_bytes().to_vec()
            }
        );
        assert!(!backup.exists());
        assert!(!journal.exists());
    }
}

#[test]
fn recovery_journal_read_io_failure_still_restores_before_cleanup_failure() {
    let f = Fixture::new();
    f.write(&conversation());
    let original = f.raw();
    let id = uuid::Uuid::new_v4();
    let backup = f.storage().chat_dir.join(format!(".s.{id}.replace.bak"));
    let journal = f.storage().chat_dir.join(format!(".s.{id}.replace.json"));
    fs::write(&backup, &original).unwrap();
    // Reading a directory as UTF8 is a real IO error. Like TS, the reader
    // ignores it and restores first; later rm of that directory still fails.
    fs::create_dir(&journal).unwrap();
    f.write(&[accepted("a", 1.0, "first")]);
    assert!(f.history.recover_replacements(&f.input()).is_err());
    assert_eq!(f.raw(), original);
    assert!(journal.is_dir());
}

#[test]
fn projection_preserves_incomplete_output_tools_and_token_usage() {
    let mut entries = vec![accepted("a", 1.0, "q"), assistant("a", 2.0, "partial")];
    let messages = projection::messages(&entries, "s", None, "now", false, true);
    assert_eq!(messages.len(), 3);
    assert_eq!(text(messages.last().unwrap(), "kind"), "status");
    entries.push(entry("turn_result","a",3.0,request(r#"{"result":{"type":"success","usage":{"inputTokens":10,"cacheReadTokens":2,"outputTokens":3}}}"#)));
    let usage = projection::token_usage(&entries, Some(1000.0), Some(100.0));
    assert_eq!(number(&usage, "used"), 12.0);
    assert_eq!(number(&usage, "effectiveTotal"), 900.0);
    assert_eq!(
        field(&usage, "breakdown").get("cacheWrite"),
        Some(&JsValue::Number(0.0))
    );
}

#[tokio::test]
async fn routes_share_live_read_and_replacement_coordinator() {
    let f = Fixture::new();
    f.write(&conversation());
    let session = Arc::new(Session::default());
    let (routes, replacements) = f.routes(session.clone());
    assert!(routes.handle("rename_session", &f.input()).await.is_none());
    let history = routes
        .handle("read_session_messages", &f.input())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(number(&history, "total"), 4.0);
    assert!(history.get("stream").is_some());
    let replace = request(r#"{"sessionKey":"s","expectedTurnId":"b","replacementTurnId":"new"}"#);
    let original = f.raw();
    let prepared = routes
        .handle("replace_last_turn", &replace)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(session.closed.load(Ordering::SeqCst), 1);
    assert_eq!(
        code(routes.handle("replace_last_turn", &replace).await.unwrap()),
        Some("replace_turn_pending")
    );
    let rollback = f.finalize(&prepared, "rollback");
    routes
        .handle("finalize_last_turn_replacement", &rollback)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(f.raw(), original);
    assert_eq!(
        code(
            routes
                .handle("finalize_last_turn_replacement", &rollback)
                .await
                .unwrap()
        ),
        Some("replace_transaction_conflict")
    );
    replacements.dispose();
}

#[tokio::test]
async fn close_failure_active_conflict_and_submit_claim_do_not_lose_data() {
    let f = Fixture::new();
    f.write(&conversation());
    let original = f.raw();
    let session = Arc::new(Session::default());
    let (routes, replacements) = f.routes(session.clone());
    let replace = request(r#"{"sessionKey":"s","expectedTurnId":"b","replacementTurnId":"new"}"#);
    session.fail_close.store(true, Ordering::SeqCst);
    assert!(
        routes
            .handle("replace_last_turn", &replace)
            .await
            .unwrap()
            .is_err()
    );
    assert_eq!(f.raw(), original);
    *session.active.lock().unwrap() = Some("other".into());
    assert_eq!(
        code(routes.handle("replace_last_turn", &replace).await.unwrap()),
        Some("replace_turn_conflict")
    );
    assert_eq!(f.raw(), original);
    *session.active.lock().unwrap() = Some("b".into());
    let prepared = routes
        .handle("replace_last_turn", &replace)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(session.aborted.load(Ordering::SeqCst), 1);
    assert_eq!(
        replacements.claim_for_submit(&"s".into(), &"wrong".into()),
        SubmitClaim::Conflict
    );
    assert_eq!(
        replacements.claim_for_submit(&"s".into(), &"new".into()),
        SubmitClaim::Claimed
    );
    assert_eq!(
        code(
            routes
                .handle(
                    "finalize_last_turn_replacement",
                    &f.finalize(&prepared, "rollback")
                )
                .await
                .unwrap()
        ),
        Some("replace_transaction_pending")
    );
    let mut entries = f.entries();
    entries.push(accepted("new", 4.0, "replacement"));
    f.write(&entries);
    let accepted = f.raw();
    replacements
        .commit_accepted_input(&"s".into(), &"new".into())
        .await;
    assert_eq!(f.raw(), accepted);
    replacements.dispose();
}
