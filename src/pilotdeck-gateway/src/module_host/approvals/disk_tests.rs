#![cfg(unix)]
use super::super::{ApprovalSubject, ResumeSource, StoreAuthority};
use super::*;
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("pdrs-sop-state-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        Self(root)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn text(value: &JsValue) -> JsString {
    value.as_string().expect("oracle string").clone()
}
fn array(value: &JsValue) -> &[JsValue] {
    let JsValue::Array(v) = value else {
        panic!("oracle array")
    };
    v
}
fn result(value: Result<JsValue, SopFailure>) -> Result<Option<JsValue>, SopFailure> {
    value.map(Some)
}
fn resume_input(value: &JsValue) -> StoreResumeInput {
    StoreResumeInput {
        session_key: text(prop(value, "sessionId")),
        project_key: None,
        request_id: text(prop(value, "requestId")),
        wait_id: text(prop(value, "waitId")),
        message: text(prop(value, "message")),
        expected_revision: value.get("expectedRevision").and_then(JsValue::as_number),
        slot_updates: value.get("slotUpdates").cloned(),
        source: if prop(value, "source") == &JsValue::from("human") {
            ResumeSource::Human
        } else {
            ResumeSource::ExternalTask
        },
        authority: value.get("authority").map(|authority| {
            let subject = prop(authority, "subject");
            StoreAuthority {
                tenant_id: text(prop(authority, "tenantId")),
                session_id: text(prop(authority, "sessionId")),
                subject: ApprovalSubject {
                    tenant_id: text(prop(subject, "tenantId")),
                    source: text(prop(subject, "source")),
                    disabled: prop(subject, "disabled") == &JsValue::Bool(true),
                    user_id: text(prop(subject, "userId")),
                    role: text(prop(subject, "role")),
                },
            }
        }),
    }
}
fn same(actual: &JsValue, expected: &JsValue, label: &str) {
    assert_eq!(
        stringify_json(actual).unwrap(),
        stringify_json(expected).unwrap(),
        "{label}"
    );
}
#[tokio::test]
async fn disk_state_matches_fifteen_real_ts_sequences_thirty_five_steps() {
    let oracle = parse_json(include_str!(
        "../../../tests/fixtures/native-sop/oracle.json"
    ))
    .unwrap();
    assert_eq!(
        prop(&oracle, "baseline"),
        &JsValue::from("cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85")
    );
    assert_eq!(prop(&oracle, "forbidden"), &JsValue::Number(0.0));
    let rows = array(prop(&oracle, "cases"));
    assert_eq!(rows.len(), 15);
    let mut count = 0;
    for row in rows {
        let scratch = Scratch::new();
        let clock = text(prop(&oracle, "fixed")).to_utf8().unwrap();
        let store = DiskSopStore::with_clock(scratch.0.clone(), Arc::new(move || clock.clone()));
        let session: JsString = "s1".into();
        if let Some(initial) = row.get("initial") {
            std::fs::write(
                store.path(&session),
                format!("{}\n", stringify_json(initial).unwrap().unwrap()),
            )
            .unwrap();
        }
        for step in array(prop(row, "steps")) {
            count += 1;
            let operation = prop(step, "operation");
            let args = array(prop(operation, "args"));
            let method = text(prop(operation, "method")).to_utf8().unwrap();
            let called = match method.as_str() {
                "loadOrCreate" => result(
                    store
                        .load_or_create(&text(&args[0]), args[1].clone(), text(&args[2]))
                        .await,
                ),
                "replace" => result(
                    store
                        .replace(
                            &text(&args[0]),
                            args[1].clone(),
                            args[2].clone(),
                            args.get(3).and_then(JsValue::as_number),
                        )
                        .await,
                ),
                "status" => store.status(&text(&args[0])).await,
                "resume" => result(store.resume(resume_input(&args[0])).await),
                "commitSubmission" => result(
                    store
                        .commit_submission(
                            &text(&args[0]),
                            args[1].clone(),
                            args[2].clone(),
                            args[3].as_number().unwrap(),
                            text(&args[4]),
                            args[5].clone(),
                        )
                        .await,
                ),
                "replyDelivery" => store.reply_delivery(&text(&args[0])).await,
                "markReplyDurable" => result(
                    store
                        .mark_reply_durable(
                            &text(&args[0]),
                            text(&args[1]),
                            args.get(2).map(text).unwrap_or_else(|| text(&args[1])),
                        )
                        .await,
                ),
                "clearReplyDelivery" => store
                    .clear_reply_delivery(&text(&args[0]), text(&args[1]))
                    .await
                    .map(|()| None),
                "authority" => result(
                    store
                        .authority(&text(&args[0]), args[1].as_number().unwrap())
                        .await,
                ),
                "recordSuccessfulTools" => result(
                    store
                        .record_successful_tools(
                            &text(&args[0]),
                            args[1].clone(),
                            text(&args[2]),
                            array(&args[3]).iter().map(text).collect(),
                        )
                        .await,
                ),
                _ => panic!("unrecognized oracle operation {method}"),
            };
            let mut outcome = JsObject::new();
            match called {
                Ok(value) => {
                    outcome.insert("ok".into(), JsValue::Bool(true));
                    if let Some(value) = value {
                        outcome.insert("result".into(), value);
                    }
                }
                Err(error) => {
                    outcome.insert("ok".into(), JsValue::Bool(false));
                    outcome.insert("name".into(), "Error".into());
                    if error.code != "gateway_request_failed" {
                        outcome.insert("code".into(), error.code.as_str().into());
                    }
                    outcome.insert("message".into(), error.message.as_str().into());
                }
            }
            let label = format!(
                "{} step {count} {method}",
                String::from_utf16_lossy(text(prop(row, "name")).units())
            );
            same(&JsValue::Object(outcome), prop(step, "outcome"), &label);
            let after = match std::fs::read_to_string(store.path(&session)) {
                Ok(raw) => Some(parse_json(&raw).unwrap()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => panic!("{error}"),
            };
            assert_eq!(
                after.is_some(),
                step.get("after").is_some(),
                "{label} file presence"
            );
            if let Some(after) = after {
                same(
                    &after,
                    prop(step, "after"),
                    &format!("{label} durable record"),
                );
            }
        }
    }
    assert_eq!(count, 35);
}
#[cfg(unix)]
#[tokio::test]
async fn state_refuses_fifo_symlink_and_sparse_without_changing_original_bytes() {
    use rustix::fs::{CWD, Mode, mkfifoat};
    let scratch = Scratch::new();
    let store = DiskSopStore::new(scratch.0.clone());
    let session: JsString = "s1".into();
    let path = store.path(&session);
    mkfifoat(CWD, &path, Mode::from_raw_mode(0o600)).unwrap();
    let error = tokio::time::timeout(std::time::Duration::from_secs(2), store.load(&session))
        .await
        .expect("FIFO read is bounded")
        .unwrap_err();
    assert!(error.message.contains("must be a regular file"));
    assert!(
        std::fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_fifo()
    );
    std::fs::remove_file(&path).unwrap();
    let victim = scratch.0.join("victim");
    std::fs::write(&victim, b"untouched").unwrap();
    std::os::unix::fs::symlink(&victim, &path).unwrap();
    assert!(store.load(&session).await.is_err());
    assert_eq!(std::fs::read(&victim).unwrap(), b"untouched");
    assert!(
        std::fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    std::fs::remove_file(&path).unwrap();
    let file = std::fs::File::create(&path).unwrap();
    file.set_len((STATE_LIMIT + 1) as u64).unwrap();
    assert!(
        store
            .load(&session)
            .await
            .unwrap_err()
            .message
            .contains("8 MiB")
    );
    assert_eq!(file.metadata().unwrap().len(), (STATE_LIMIT + 1) as u64);
}
#[cfg(unix)]
use std::os::unix::fs::FileTypeExt;
#[tokio::test]
async fn concurrent_duplicate_commands_publish_once_and_reopen_from_disk() {
    let scratch = Scratch::new();
    let store = DiskSopStore::new(scratch.0.clone());
    let session: JsString = "s1".into();
    let bundle = parse_json(r#"{"sops":[]}"#).unwrap();
    store.replace(&session,bundle,parse_json(r#"{"status":"waiting_external_task","awaiting_input_json":{"wait_id":"wait-external"}}"#).unwrap(),None).await.unwrap();
    let input=parse_json(r#"{"sessionId":"s1","requestId":"r","waitId":"wait-external","message":"fixture","source":"external_task","expectedRevision":1}"#).unwrap();
    assert_eq!(
        store.model_context(&session).await.unwrap(),
        Some(JsValue::Object(JsObject::new()))
    );
    let other = DiskSopStore::new(scratch.0.clone());
    let (a, b) = tokio::join!(
        store.resume(resume_input(&input)),
        other.resume(resume_input(&input))
    );
    let a = a.unwrap();
    let b = b.unwrap();
    assert_ne!(a.get("duplicate"), b.get("duplicate"));
    assert_eq!(a.get("revision"), Some(&JsValue::Number(2.0)));
    assert_eq!(b.get("revision"), a.get("revision"));
    assert_eq!(
        other.load(&session).await.unwrap().unwrap().get("revision"),
        Some(&JsValue::Number(2.0))
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(store.path(&session))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}
