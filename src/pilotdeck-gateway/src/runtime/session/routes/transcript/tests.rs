use super::*;
use crate::{
    runtime::{
        project::{DisposeFuture, RuntimeError, RuntimeResult},
        refresh::BoxFuture,
        session::{
            LiveSession, LiveSessionHandle, SessionContext, SessionFactory, SessionOwnerOptions,
        },
    },
    storage::{TranscriptWriter, read_transcript},
};
use pilotdeck_protocol::{parse_json, stringify_json};
use std::sync::Mutex;

fn js(text: &str) -> JsValue {
    parse_json(text).unwrap()
}
fn json(value: &JsValue) -> String {
    stringify_json(value).unwrap().unwrap()
}
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("pdrs-transcript-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        Self(root)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
struct Factory;
impl SessionFactory for Factory {
    fn create(
        &self,
        _: SessionContext,
        _: Option<LiveSessionHandle>,
    ) -> BoxFuture<Result<LiveSessionHandle, RuntimeError>> {
        Box::pin(async { Ok(LiveSessionHandle::new(Arc::new(Session), None)) })
    }
}
struct Session;
impl LiveSession for Session {
    fn snapshot(&self) -> JsValue {
        object([("status", "idle".into())])
    }
    fn abort(&self, _: &JsString) -> RuntimeResult {
        Ok(())
    }
    fn dispose(&self) -> DisposeFuture<'_> {
        Box::pin(async { Ok(()) })
    }
}
fn owner() -> SessionOwner {
    let mut options = SessionOwnerOptions::new(Arc::new(Factory));
    options.idle_sweep_interval_ms = 0;
    SessionOwner::new(options)
}
struct Host {
    root: PathBuf,
    adapter: AsyncTranscriptAdapter,
    checkpoint: HostToolCheckpoint,
    live_writer: Mutex<Option<Arc<TranscriptWriter>>>,
}
impl Host {
    fn new(root: &Path) -> Self {
        let cwd = root.to_path_buf();
        Self {
            root: root.to_path_buf(),
            adapter: AsyncTranscriptAdapter::new(
                Arc::new(JsonlTranscriptStore::new()),
                Arc::new(move |key| {
                    crate::session_chat_dir(
                        &key.pilot_home,
                        &cwd,
                        &key.project_root.to_string_lossy(),
                    )
                }),
            ),
            checkpoint: HostToolCheckpoint::default(),
            live_writer: Mutex::new(None),
        }
    }
    fn storage_for(&self, session: &str) -> SessionStorage {
        self.storage(&self.root, &session.into()).unwrap()
    }
}
impl TranscriptHost for Host {
    fn resolve_project(&self, params: &JsValue) -> Result<PathBuf, Error> {
        let requested = property(params, "projectKey")?;
        if !matches!(requested, JsValue::Null | JsValue::Undefined)
            && !requested
                .as_string()
                .is_some_and(|s| text(s) == self.root.to_string_lossy())
        {
            return Err(gateway("PROJECT_NOT_FOUND", "Unknown projectKey"));
        }
        Ok(self.root.clone())
    }
    fn storage(
        &self,
        project_root: &Path,
        session_key: &JsString,
    ) -> Result<SessionStorage, Error> {
        Ok(self.adapter.create_session_storage(SessionStorageInput {
            project_root: project_root.to_path_buf(),
            pilot_home: self.root.join("home"),
            session_id: session_key.clone(),
            now: Some(Arc::new(|| "2026-10-27T00:00:00.000Z".into())),
        }))
    }
    fn prepare_seed<'a>(&'a self, _: &'a JsValue) -> TranscriptFuture<'a, SeedReadTarget> {
        Box::pin(async {
            Ok(SeedReadTarget {
                cwd: self.root.to_string_lossy().as_ref().into(),
                additional_working_directories: vec![],
                bypass_permissions: false,
                checkpoint: self.checkpoint.clone(),
            })
        })
    }
    fn record_live_status<'a>(&'a self, params: &'a JsValue) -> TranscriptFuture<'a, Option<bool>> {
        Box::pin(async move {
            let writer = self.live_writer.lock().unwrap().clone();
            if let Some(writer) = writer {
                writer
                    .record_agent_status_message(
                        field(params, "sessionKey").as_string().unwrap(),
                        field(params, "turnId").as_string().unwrap(),
                        field(params, "status"),
                    )
                    .map_err(adapter_error)?;
                Ok(Some(true))
            } else {
                Ok(None)
            }
        })
    }
}
async fn call(routes: &TranscriptRoutes, method: &str, params: JsValue) -> Result<JsValue, Error> {
    routes.handle(method, &params).await.expect("owned method")
}
fn archive() -> JsValue {
    js(
        r#"{"sessionKey":"restored","archive":{"schemaVersion":1,"format":"portable_text_messages","messages":[{"role":"user","text":"hi"},{"role":"assistant","text":"hello"}],"title":"标题"}}"#,
    )
}
fn status(session: &str) -> JsValue {
    object([
        ("sessionKey", session.into()),
        ("turnId", "status-turn".into()),
        (
            "status",
            js(r#"{"event":"test_event","kind":"info","text":"saved","detail":{"key":1}}"#),
        ),
    ])
}

#[tokio::test]
async fn restore_export_survives_storage_and_routes_reconstruction() {
    let temp = Temp::new();
    let host = Arc::new(Host::new(&temp.0));
    let routes = TranscriptRoutes::new(owner(), host.clone());
    let response = call(&routes, "restore_session_transcript", archive())
        .await
        .unwrap();
    assert_eq!(
        json(&response),
        r#"{"sessionKey":"restored","importedMessages":2}"#
    );
    let path = host.storage_for("restored").transcript_path;
    let read = read_transcript(&path).unwrap();
    assert_eq!(read.entries.len(), 2);
    assert!(is(field(&read.entries[1], "type"), "accepted_input"));
    assert_eq!(
        field(
            field(&read.entries[1], "metadata"),
            "restoredFromGatewayArchive"
        )
        .as_bool(),
        Some(true)
    );
    drop(routes);
    drop(host);
    let fresh = TranscriptRoutes::new(owner(), Arc::new(Host::new(&temp.0)));
    let exported = call(
        &fresh,
        "export_session_transcript",
        js(r#"{"sessionKey":"restored"}"#),
    )
    .await
    .unwrap();
    assert_eq!(exported, field(&archive(), "archive").clone());
    assert_eq!(
        fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().contains("restore.tmp"))
            .count(),
        0
    );
}

#[tokio::test]
async fn conflicting_or_malformed_restore_preserves_existing_bytes() {
    let temp = Temp::new();
    let host = Arc::new(Host::new(&temp.0));
    let routes = TranscriptRoutes::new(owner(), host.clone());
    call(&routes, "restore_session_transcript", archive())
        .await
        .unwrap();
    let path = host.storage_for("restored").transcript_path;
    let before = fs::read(&path).unwrap();
    assert_eq!(
        call(&routes, "restore_session_transcript", archive())
            .await
            .unwrap_err()
            .code,
        "SESSION_RESTORE_CONFLICT"
    );
    let malformed = js(
        r#"{"sessionKey":"other","archive":{"schemaVersion":1,"format":"portable_text_messages","messages":[{"role":"assistant","text":null}]}}"#,
    );
    assert_eq!(
        call(&routes, "restore_session_transcript", malformed)
            .await
            .unwrap_err()
            .code,
        "INVALID_SESSION_ARCHIVE"
    );
    assert_eq!(fs::read(&path).unwrap(), before);
    assert!(!host.storage_for("other").transcript_path.exists());
}

#[tokio::test]
async fn malformed_durable_messages_reject_export_and_cold_status_without_appending() {
    let temp = Temp::new();
    let host = Arc::new(Host::new(&temp.0));
    let routes = TranscriptRoutes::new(owner(), host.clone());
    for kind in ["assistant_message", "tool_result_message"] {
        for message in [None, Some(JsValue::Null)] {
            let storage = host.storage_for(kind);
            fs::create_dir_all(&storage.chat_dir).unwrap();
            let mut entry = fields([
                ("type", kind.into()),
                ("sessionId", kind.into()),
                ("turnId", "incomplete".into()),
                ("sequence", JsValue::Number(1.0)),
                ("createdAt", "2026-10-27T00:00:00Z".into()),
            ]);
            if let Some(message) = message {
                entry.insert("message".into(), message);
            }
            let bytes = format!("{}\n", json(&JsValue::Object(entry)));
            fs::write(&storage.transcript_path, &bytes).unwrap();
            let error = call(
                &routes,
                "export_session_transcript",
                object([("sessionKey", kind.into())]),
            )
            .await
            .unwrap_err();
            assert_eq!(error.name, "TypeError");
            assert!(text(&error.message).contains("(reading 'metadata')"));
            let error = call(&routes, "record_agent_status_message", status(kind))
                .await
                .unwrap_err();
            assert_eq!(error.name, "TypeError");
            assert_eq!(fs::read_to_string(&storage.transcript_path).unwrap(), bytes);
        }
    }
}

#[tokio::test]
async fn cold_status_reopens_sequence_and_keeps_portable_history() {
    let temp = Temp::new();
    let host = Arc::new(Host::new(&temp.0));
    let routes = TranscriptRoutes::new(owner(), host.clone());
    call(&routes, "restore_session_transcript", archive())
        .await
        .unwrap();
    assert_eq!(
        call(&routes, "record_agent_status_message", status("restored"))
            .await
            .unwrap(),
        js(r#"{"recorded":true}"#)
    );
    drop(routes);
    let fresh_host = Arc::new(Host::new(&temp.0));
    let fresh = TranscriptRoutes::new(owner(), fresh_host.clone());
    call(&fresh, "record_agent_status_message", status("restored"))
        .await
        .unwrap();
    let read = read_transcript(fresh_host.storage_for("restored").transcript_path).unwrap();
    assert_eq!(read.entries.len(), 4);
    for index in 2..4 {
        assert_eq!(
            field(&read.entries[index], "sequence").as_number(),
            Some((index + 1) as f64)
        );
        assert_eq!(
            field(&read.entries[index], "parentEntryId"),
            field(&read.entries[index - 1], "entryId")
        );
        assert!(is(
            field(&read.entries[index], "type"),
            "agent_status_message"
        ));
        assert_eq!(
            field(&read.entries[index], "detail"),
            field(field(&status("restored"), "status"), "detail")
        );
    }
    let exported = call(
        &fresh,
        "export_session_transcript",
        js(r#"{"sessionKey":"restored"}"#),
    )
    .await
    .unwrap();
    assert_eq!(exported, field(&archive(), "archive").clone());
}

#[tokio::test]
async fn live_status_uses_the_injected_writer_without_cold_storage() {
    let temp = Temp::new();
    let host = Arc::new(Host::new(&temp.0));
    let storage = host.storage_for("live");
    let path = storage.transcript_path.clone();
    *host.live_writer.lock().unwrap() = Some(Arc::new(storage.transcript));
    let routes = TranscriptRoutes::new(owner(), host);
    let mut input = status("live");
    if let JsValue::Object(fields) = &mut input {
        fields.insert("projectKey".into(), "not-a-project".into());
    }
    call(&routes, "record_agent_status_message", input)
        .await
        .unwrap();
    let entries = read_transcript(path).unwrap().entries;
    assert_eq!(entries.len(), 1);
    assert!(is(field(&entries[0], "type"), "agent_status_message"));
}

#[tokio::test]
async fn seed_checks_mtime_and_updates_shared_read_state_and_write_snapshot() {
    let temp = Temp::new();
    let file = temp.0.join("source.txt");
    fs::write(&file, "\u{feff}hello\n").unwrap();
    let mtime = (fs::metadata(&file)
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
        * 1000.0)
        .floor();
    let host = Arc::new(Host::new(&temp.0));
    let routes = TranscriptRoutes::new(owner(), host.clone());
    let input = |mtime| {
        object([
            ("sessionKey", "s".into()),
            ("path", "source.txt".into()),
            ("mtime", JsValue::Number(mtime)),
        ])
    };
    assert_eq!(
        call(&routes, "seed_read_state", input(mtime + 1000.0))
            .await
            .unwrap(),
        js(r#"{"applied":false}"#)
    );
    assert!(host.checkpoint.read_file_state.lock().unwrap().is_empty());
    assert_eq!(
        call(&routes, "seed_read_state", input(mtime))
            .await
            .unwrap(),
        js(r#"{"applied":true}"#)
    );
    let seed = host.checkpoint.snapshot().unwrap();
    let absolute: JsString = file.to_string_lossy().as_ref().into();
    assert_eq!(
        seed.allowed_read_files.as_ref().unwrap(),
        &vec![absolute.clone()]
    );
    assert_eq!(
        seed.read_file_state
            .as_ref()
            .unwrap()
            .get_key(&format!("{}::text::1::all::", file.display()).into())
            .and_then(|v| v.get("mtimeMs"))
            .and_then(JsValue::as_number),
        Some(mtime)
    );
    let snapshot = seed
        .write_snapshots
        .as_ref()
        .unwrap()
        .get_key(&absolute)
        .unwrap();
    use sha2::{Digest, Sha256};
    assert_eq!(
        field(snapshot, "contentHash"),
        &JsValue::String(format!("{:x}", Sha256::digest("\u{feff}hello\n".as_bytes())).into())
    );
    let rebuilt = HostToolCheckpoint::new(Some(&seed), &[]).unwrap();
    assert_eq!(
        rebuilt.snapshot().unwrap().write_snapshots,
        seed.write_snapshots
    );
}

#[tokio::test]
async fn busy_seed_and_live_restore_are_rejected_by_the_shared_owner() {
    let temp = Temp::new();
    let host = Arc::new(Host::new(&temp.0));
    let owner = owner();
    let routes = TranscriptRoutes::new(owner.clone(), host);
    let reservation = owner.begin_turn(&"s".into(), &"run".into()).unwrap();
    let error = call(
        &routes,
        "seed_read_state",
        js(r#"{"sessionKey":"s","path":"a","mtime":0}"#),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "SESSION_BUSY");
    drop(reservation);
    owner
        .get_or_create(SessionContext {
            session_key: "restored".into(),
            project_key: None,
            channel_key: "api_server".into(),
            allowed_tools: None,
            disallowed_tools: None,
        })
        .await
        .unwrap();
    assert_eq!(
        call(&routes, "restore_session_transcript", archive())
            .await
            .unwrap_err()
            .code,
        "SESSION_BUSY"
    );
}

#[tokio::test]
async fn corrupt_json_io_failure_and_symlink_boundary_do_not_report_success() {
    let temp = Temp::new();
    let host = Arc::new(Host::new(&temp.0));
    let routes = TranscriptRoutes::new(owner(), host.clone());
    let storage = host.storage_for("corrupt");
    fs::create_dir_all(&storage.chat_dir).unwrap();
    fs::write(&storage.transcript_path, "{bad json\n").unwrap();
    assert_eq!(
        call(
            &routes,
            "export_session_transcript",
            js(r#"{"sessionKey":"corrupt"}"#)
        )
        .await
        .unwrap_err()
        .code,
        "SESSION_TRANSCRIPT_INVALID"
    );
    assert_eq!(
        call(&routes, "record_agent_status_message", status("corrupt"))
            .await
            .unwrap_err()
            .code,
        "SESSION_TRANSCRIPT_INVALID"
    );
    assert_eq!(
        fs::read_to_string(&storage.transcript_path).unwrap(),
        "{bad json\n"
    );
    fs::create_dir(host.storage_for("restored").transcript_path).unwrap();
    assert_eq!(
        call(&routes, "restore_session_transcript", archive())
            .await
            .unwrap_err()
            .code,
        "SESSION_RESTORE_CONFLICT"
    );
    let linked = host.storage_for("linked");
    let outside = temp.0.join("outside.txt");
    fs::write(&outside, "keep").unwrap();
    std::os::unix::fs::symlink(&outside, &linked.transcript_path).unwrap();
    assert_eq!(
        call(&routes, "record_agent_status_message", status("linked"))
            .await
            .unwrap_err()
            .code,
        "path_not_allowed"
    );
    assert_eq!(fs::read_to_string(outside).unwrap(), "keep");
    let file = temp.0.join("source.txt");
    fs::write(&file, "abc").unwrap();
    let io_host = Arc::new(Host::new(&file));
    let io_routes = TranscriptRoutes::new(owner(), io_host);
    assert_eq!(
        call(&io_routes, "restore_session_transcript", archive())
            .await
            .unwrap_err()
            .code,
        "ENOTDIR"
    );
}

#[tokio::test]
async fn seed_rejects_binary_and_outside_symlinks_without_state_changes() {
    let temp = Temp::new();
    let outside = Temp::new();
    fs::write(outside.0.join("secret.txt"), "private").unwrap();
    std::os::unix::fs::symlink(outside.0.join("secret.txt"), temp.0.join("link.txt")).unwrap();
    fs::write(temp.0.join("binary.txt"), b"a\0b").unwrap();
    let host = Arc::new(Host::new(&temp.0));
    let routes = TranscriptRoutes::new(owner(), host.clone());
    let error = call(
        &routes,
        "seed_read_state",
        js(r#"{"sessionKey":"s","path":"link.txt","mtime":0}"#),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "path_not_allowed");
    let mtime = (fs::metadata(temp.0.join("binary.txt"))
        .unwrap()
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
        * 1000.0)
        .floor();
    let error = call(
        &routes,
        "seed_read_state",
        object([
            ("sessionKey", "s".into()),
            ("path", "binary.txt".into()),
            ("mtime", JsValue::Number(mtime)),
        ]),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "invalid_tool_input");
    assert!(host.checkpoint.read_file_state.lock().unwrap().is_empty());
    assert!(host.checkpoint.write_snapshots.lock().unwrap().is_empty());
}

#[test]
fn archive_limits_utf16_title_unknown_fields_and_tool_text_projection() {
    let mut valid = field(&archive(), "archive").clone();
    if let JsValue::Object(fields) = &mut valid {
        fields.insert("title".into(), JsValue::String("🦀".repeat(257).into()));
    }
    assert_eq!(
        portable::validate(&valid).unwrap_err().code,
        "INVALID_SESSION_ARCHIVE"
    );
    if let JsValue::Object(fields) = &mut valid {
        fields.insert("title".into(), "ok".into());
        fields.insert(
            "extra".into(),
            JsValue::String("x".repeat(2 * 1024 * 1024).into()),
        );
    }
    assert!(text(&portable::validate(&valid).unwrap_err().message).contains("too large"));
    let entries = vec![js(
        r#"{"type":"accepted_input","turnId":"t","messages":[{"role":"assistant","content":[{"type":"tool_call","name":"read_file","input":{"file":"a"}},{"type":"tool_result","toolCallId":"x","content":[{"type":"text","text":"content"}]}]}]}"#,
    )];
    let archive = portable::project(&entries).unwrap();
    let messages = portable::validate(&archive).unwrap();
    assert_eq!(
        field(&messages[0], "text"),
        &JsValue::from("[Tool call: read_file]\n{\n  \"file\": \"a\"\n}\n\n[Tool result]\ncontent")
    );
}

#[test]
fn replay_rejects_malformed_tool_content_and_retains_complete_compact_snapshot() {
    let malformed = vec![
        js(
            r#"{"type":"assistant_message","turnId":"t","message":{"role":"assistant","content":[{"type":"tool_result","content":null}]}}"#,
        ),
        js(r#"{"type":"turn_result","turnId":"t","result":{"usage":{},"permissionDenials":[]}}"#),
    ];
    let error = portable::project(&malformed).unwrap_err();
    assert_eq!(error.name, "TypeError");
    assert_eq!(
        text(&error.message),
        "Cannot read properties of null (reading 'map')"
    );
    let compact = vec![
        js(
            r#"{"type":"accepted_input","turnId":"old","messages":[{"role":"user","content":[{"type":"text","text":"discard"}]}]}"#,
        ),
        js(
            r#"{"type":"control_boundary","turnId":"unfinished","boundary":{"kind":"compact","subtype":"compact_boundary","snapshot":{"version":1,"messages":[{"role":"assistant","content":[{"type":"text","text":"retained"}]}]}}}"#,
        ),
    ];
    let archive = portable::project(&compact).unwrap();
    let messages = portable::validate(&archive).unwrap();
    assert_eq!(messages.len(), 1);
    assert!(is(field(&messages[0], "text"), "retained"));
}

#[tokio::test]
async fn unknown_method_and_invalid_mtime_keep_dispatch_and_error_contracts() {
    let temp = Temp::new();
    let routes = TranscriptRoutes::new(owner(), Arc::new(Host::new(&temp.0)));
    assert!(routes.handle("other", &JsValue::Null).await.is_none());
    for value in ["null", "-1", "1.5", "\"1\""] {
        let params = js(&format!(
            r#"{{"sessionKey":"s","path":"a","mtime":{value}}}"#
        ));
        assert_eq!(
            call(&routes, "seed_read_state", params)
                .await
                .unwrap_err()
                .code,
            "INVALID_FILE_MTIME"
        );
    }
}
