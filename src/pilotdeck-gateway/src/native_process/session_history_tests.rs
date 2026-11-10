use super::*;
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("pdrs-session-history-{}", uuid()));
        fs::create_dir(&root).unwrap();
        Self(root)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn json(s: &str) -> JsValue {
    parse_json(s).unwrap()
}
fn transaction(
    root: &Path,
    original: &[u8],
    current: &[u8],
    owner: Option<JsValue>,
) -> (PathBuf, PathBuf, PathBuf) {
    let path = root.join("session.jsonl");
    fs::write(&path, current).unwrap();
    let id = uuid();
    let (backup, journal) = transaction_paths(root, &path, &id);
    fs::write(&backup, original).unwrap();
    let mut record = object([
        (
            "version",
            JsValue::Number(if owner.is_some() { 2. } else { 1. }),
        ),
        ("transactionId", string(id)),
        ("sessionKey", string("session")),
        ("replacementTurnId", string("new-turn")),
        ("preparedAt", string("2026-11-10T00:00:00.000Z")),
    ]);
    if let Some(owner) = owner {
        set(&mut record, "owner", owner);
    }
    fs::write(&journal, body(&[record]).unwrap()).unwrap();
    (path, backup, journal)
}
#[test]
fn recovery_restores_exact_backup_when_replacement_never_was_accepted() {
    let scratch = Scratch::new();
    let original = b"{\"type\":\"accepted_input\",\"turnId\":\"original\"}\n\n";
    let (path, backup, journal) = transaction(&scratch.0, original, b"", None);
    let report = recover_chats(std::slice::from_ref(&scratch.0));
    assert_eq!(report.rolled_back, 1);
    assert!(report.failures.is_empty());
    assert_eq!(fs::read(path).unwrap(), original);
    assert!(!backup.exists());
    assert!(!journal.exists());
}
#[test]
fn recovery_commits_existing_new_turn_without_replacing_its_data() {
    let scratch = Scratch::new();
    let current = b"{\"type\":\"accepted_input\",\"turnId\":\"new-turn\"}\n";
    let (path, backup, journal) = transaction(&scratch.0, b"original", current, None);
    let report = recover_chats(std::slice::from_ref(&scratch.0));
    assert_eq!(report.committed, 1);
    assert_eq!(fs::read(path).unwrap(), current);
    assert!(!backup.exists());
    assert!(!journal.exists());
}
#[test]
fn recovery_preserves_live_owner_and_reports_missing_backup() {
    let scratch = Scratch::new();
    let owner = object([
        ("instanceId", string("live")),
        ("pid", JsValue::Number(std::process::id() as f64)),
    ]);
    let (path, backup, journal) = transaction(&scratch.0, b"original", b"current", Some(owner));
    let report = recover_chats(std::slice::from_ref(&scratch.0));
    assert_eq!(report.skipped, 1);
    assert!(backup.exists() && journal.exists());
    let mut record = read_journal(
        &journal,
        &text(
            &parse_json(std::str::from_utf8(&fs::read(&journal).unwrap()).unwrap()).unwrap(),
            "transactionId",
        ),
    )
    .unwrap()
    .unwrap();
    set(&mut record, "version", JsValue::Number(1.));
    // TS honors an owner field even on a v1 journal. Remove that live owner
    // explicitly before exercising the separate missing-backup failure.
    record = omit(&record, &["owner"]);
    fs::write(&journal, body(&[record]).unwrap()).unwrap();
    fs::remove_file(&backup).unwrap();
    let report = recover_chats(std::slice::from_ref(&scratch.0));
    assert_eq!(report.failures.len(), 1);
    assert!(journal.exists());
    assert_eq!(fs::read(path).unwrap(), b"current");
}
#[test]
fn finalize_rollback_restores_original_raw_bytes_and_commit_keeps_new_bytes() {
    let scratch = Scratch::new();
    let (path, backup, journal) = transaction(&scratch.0, b"original\n\n", b"rewritten\n", None);
    let id = read_journal(
        &journal,
        journal
            .file_name()
            .unwrap()
            .to_string_lossy()
            .split('.')
            .nth(2)
            .unwrap(),
    )
    .unwrap()
    .unwrap()
    .get("transactionId")
    .unwrap()
    .clone();
    let input = object([
        ("sessionKey", string("session")),
        ("transactionId", id),
        ("action", string("rollback")),
    ]);
    finalize(&input, &scratch.0, &path).unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"original\n\n");
    assert!(!backup.exists() && !journal.exists());
    let (path, backup, journal) = transaction(&scratch.0, b"original", b"new-data", None);
    let id = journal
        .file_name()
        .unwrap()
        .to_string_lossy()
        .split('.')
        .nth(2)
        .unwrap()
        .to_owned();
    let input = object([
        ("sessionKey", string("session")),
        ("transactionId", string(id)),
        ("action", string("commit")),
    ]);
    finalize(&input, &scratch.0, &path).unwrap();
    assert_eq!(fs::read(path).unwrap(), b"new-data");
    assert!(!backup.exists() && !journal.exists());
}
#[test]
fn replace_first_input_removes_generated_title_but_preserves_user_title() {
    let all = vec![
        json(
            r#"{"type":"session_metadata","sessionId":"s","sequence":1,"entryId":"meta","metadata":{"aiTitle":"generated","title":"manual","firstPrompt":"old","lastPrompt":"old","isSnapshot":true}}"#,
        ),
        json(
            r#"{"type":"accepted_input","sessionId":"s","sequence":2,"entryId":"input","turnId":"old-turn","messages":[]}"#,
        ),
        json(
            r#"{"type":"session_metadata","sessionId":"s","sequence":3,"metadata":{"title":null,"lastPrompt":"latest"}}"#,
        ),
    ];
    let input = json(r#"{"expectedTurnId":"old-turn"}"#);
    let (_, removed, prefix) = replacement_plan(&all, &input, "2026-11-10T00:00:00.000Z").unwrap();
    assert_eq!(removed, 2);
    assert_eq!(
        text(field(prefix.last().unwrap(), "metadata"), "title"),
        "manual"
    );
    for e in &prefix {
        assert!(matches!(
            field(field(e, "metadata"), "aiTitle"),
            JsValue::Undefined
        ));
    }
    assert_eq!(text(prefix.last().unwrap(), "parentEntryId"), "meta");
    assert!(matches!(
        field(field(prefix.last().unwrap(), "metadata"), "lastPrompt"),
        JsValue::Undefined
    ));
}
#[test]
fn default_fork_carryover_preserves_reference_paths_like_ts_storage_provider() {
    let entry = json(
        r#"{"type":"accepted_input","sessionId":"old","turnId":"turn","messages":[{"role":"user","content":[{"type":"media_reference","path":"/project/tools/old/image.png"},{"type":"tool_result_reference","path":"/outside/result"}]}]}"#,
    );
    let target = retarget(&entry, "new");
    let JsValue::Array(messages) = field(&target, "messages") else {
        panic!()
    };
    let JsValue::Array(blocks) = field(&messages[0], "content") else {
        panic!()
    };
    assert_eq!(text(&blocks[0], "path"), "/project/tools/old/image.png");
    assert_eq!(text(&blocks[1], "path"), "/outside/result");
    assert_eq!(
        text(
            field(field(&messages[0], "metadata"), "forkCarryover"),
            "sourceSessionId"
        ),
        "old"
    );
}
#[test]
fn fork_title_matches_js_utf16_slice_and_prefill_retains_lone_units() {
    let mut units = vec![b'a' as u16; 47];
    units.extend([0xd83d, 0xde00]);
    let prompt = JsString::from_utf16(units);
    let title = fork_title(&prompt, 0, None);
    let mut expected = "⑂ ".encode_utf16().collect::<Vec<_>>();
    expected.extend(vec![b'a' as u16; 47]);
    expected.extend([0xd83d, 0x2026]);
    assert_eq!(title.as_string().unwrap().units(), expected);
    let entry = json(
        r#"{"messages":[{"content":[{"type":"text","text":"\ufeff \ud800 \ufeff"},{"type":"text","text":"tail"}]}]}"#,
    );
    assert_eq!(
        accepted_text(&entry).units(),
        [0xd800, 10, 10, 116, 97, 105, 108]
    );
}
#[test]
fn compact_retarget_requires_valid_versioned_nonempty_messages() {
    let malformed = json(
        r#"{"type":"control_boundary","sessionId":"old","turnId":"turn","boundary":{"kind":"compact","subtype":"compact_boundary","snapshot":{"version":1,"messages":[{"role":"user","content":[{"type":"media_reference","path":"/tools/old/a"}]}]}}}"#,
    );
    let mapped = retarget(&malformed, "new");
    assert_eq!(field(&mapped, "boundary"), field(&malformed, "boundary"));
    let valid = json(
        r#"{"type":"control_boundary","sessionId":"old","turnId":"turn","boundary":{"kind":"compact","subtype":"compact_boundary","snapshot":{"version":1,"messages":[{"role":"user","content":[{"type":"text","text":"summary"}]}]}}}"#,
    );
    let mapped = retarget(&valid, "new");
    let JsValue::Array(messages) = field(field(field(&mapped, "boundary"), "snapshot"), "messages")
    else {
        panic!()
    };
    assert_eq!(
        text(
            field(field(&messages[0], "metadata"), "forkCarryover"),
            "sourceTurnId"
        ),
        "turn"
    );
}
#[test]
fn recovery_does_not_wrap_an_out_of_range_owner_pid_into_a_live_pid() {
    let scratch = Scratch::new();
    let owner = object([
        ("instanceId", string("old")),
        ("pid", JsValue::Number(4_294_967_297.)),
    ]);
    let (path, _, _) = transaction(&scratch.0, b"original", b"prepared", Some(owner));
    let report = recover_chats(std::slice::from_ref(&scratch.0));
    assert_eq!(report.rolled_back, 1);
    assert_eq!(fs::read(path).unwrap(), b"original");
}
#[test]
fn recovery_accepts_selected_home_alias_but_does_not_follow_child_aliases() {
    use std::os::unix::fs::symlink;
    let scratch = Scratch::new();
    let home = scratch.0.join("home");
    let chat = home.join("projects/registered/chats");
    fs::create_dir_all(&chat).unwrap();
    let (path, _, _) = transaction(&chat, b"original", b"prepared", None);
    let alias = scratch.0.join("selected-home");
    symlink(&home, &alias).unwrap();
    let report = recover(&alias).unwrap();
    assert_eq!(report.rolled_back, 1);
    assert_eq!(fs::read(path).unwrap(), b"original");
    let outside = Scratch::new();
    let outside_chat = outside.0.join("chats");
    fs::create_dir(&outside_chat).unwrap();
    let (path, backup, journal) =
        transaction(&outside_chat, b"outside-original", b"outside-current", None);
    symlink(&outside.0, home.join("projects/child-link")).unwrap();
    let report = recover(&alias).unwrap();
    assert_eq!(report.rolled_back, 0);
    assert_eq!(fs::read(path).unwrap(), b"outside-current");
    assert!(backup.exists() && journal.exists());
}
#[test]
fn provider_retargets_only_chat_session_paths_and_primary_subagent_relative_path() {
    let input = json(
        r#"{"type":"assistant_message","message":{"role":"assistant","content":[{"type":"tool_result_reference","path":"/chat/old/tool-results/a"},{"type":"media_reference","path":"/project/.pilotdeck/tool-results/old/a"},{"type":"media_reference","path":"/chat/old/..odd/a"}]}}"#,
    );
    let mapped =
        auxiliary_entry(&input, Path::new("/chat/old"), Path::new("/chat/new"), true).unwrap();
    let JsValue::Array(blocks) = field(field(&mapped, "message"), "content") else {
        panic!()
    };
    assert_eq!(text(&blocks[0], "path"), "/chat/new/tool-results/a");
    assert_eq!(
        text(&blocks[1], "path"),
        "/project/.pilotdeck/tool-results/old/a"
    );
    assert_eq!(text(&blocks[2], "path"), "/chat/old/..odd/a");
    let entry = json(
        r#"{"type":"subagent_started","transcriptRelativePath":"old\\subagents\\nested.jsonl"}"#,
    );
    assert_eq!(
        text(
            &auxiliary_entry(&entry, Path::new("/chat/old"), Path::new("/chat/new"), true).unwrap(),
            "transcriptRelativePath"
        ),
        "new/subagents/nested.jsonl"
    );
    assert_eq!(
        auxiliary_entry(
            &entry,
            Path::new("/chat/old"),
            Path::new("/chat/new"),
            false
        )
        .unwrap(),
        entry
    );
}
#[test]
fn copied_subagent_jsonl_rewrites_internal_paths_without_changing_invalid_or_blank_lines() {
    let scratch = Scratch::new();
    let root = scratch.0.join("subagents/nested");
    fs::create_dir_all(&root).unwrap();
    let path = root.join("child.jsonl");
    let original=b"{\"type\":\"assistant_message\",\"sessionId\":\"child\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"tool_result_reference\",\"path\":\"/chat/old/tool-results/a\"}]}}\r\n  \r\n{bad}\r\n";
    fs::write(&path, original).unwrap();
    rewrite_subagents(
        &scratch.0.join("subagents"),
        Path::new("/chat/old"),
        Path::new("/chat/new"),
        0,
    )
    .unwrap();
    let expected = String::from_utf8(original.to_vec())
        .unwrap()
        .replace("/chat/old/tool-results/a", "/chat/new/tool-results/a")
        .replace("\r\n", "\n");
    assert_eq!(fs::read(&path).unwrap(), expected.as_bytes());
}
#[test]
fn recovery_ignores_malformed_unicode_artifact_names_without_slicing_inside_a_codepoint() {
    let scratch = Scratch::new();
    let name = format!(".session汉{}.replace.json", "a".repeat(34));
    let path = scratch.0.join(name);
    fs::write(&path, b"{}").unwrap();
    let report = recover_chats(std::slice::from_ref(&scratch.0));
    assert!(report.failures.is_empty());
    assert_eq!(report.cleaned, 0);
    assert_eq!(fs::read(path).unwrap(), b"{}");
}
#[test]
fn recovery_preserves_new_current_and_all_sidecars_when_native_read_bound_is_exceeded() {
    let scratch = Scratch::new();
    let first = b"{\"type\":\"accepted_input\",\"turnId\":\"new-turn\"}\n";
    let (path, backup, journal) = transaction(&scratch.0, b"old-backup", first, None);
    File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(MAX_BYTES + 1)
        .unwrap();
    let before = fs::metadata(&path).unwrap();
    let backup_bytes = fs::read(&backup).unwrap();
    let journal_bytes = fs::read(&journal).unwrap();
    let report = recover_chats(std::slice::from_ref(&scratch.0));
    assert_eq!(report.failures.len(), 1);
    assert_eq!(report.rolled_back, 0);
    assert_eq!(report.committed, 0);
    assert_eq!(report.cleaned, 0);
    assert_eq!(fs::metadata(&path).unwrap().len(), before.len());
    let mut observed = vec![0; first.len()];
    File::open(&path)
        .unwrap()
        .read_exact(&mut observed)
        .unwrap();
    assert_eq!(observed, first);
    assert_eq!(fs::read(&backup).unwrap(), backup_bytes);
    assert_eq!(fs::read(&journal).unwrap(), journal_bytes);
}
#[test]
fn recovery_does_not_resurrect_a_missing_current_transcript() {
    let scratch = Scratch::new();
    let (path, backup, journal) =
        transaction(&scratch.0, b"deleted-conversation", b"current", None);
    fs::remove_file(&path).unwrap();
    let backup_bytes = fs::read(&backup).unwrap();
    let journal_bytes = fs::read(&journal).unwrap();
    let report = recover_chats(std::slice::from_ref(&scratch.0));
    assert_eq!(report.failures.len(), 1);
    assert_eq!(report.rolled_back, 0);
    assert!(!path.exists());
    assert_eq!(fs::read(backup).unwrap(), backup_bytes);
    assert_eq!(fs::read(journal).unwrap(), journal_bytes);
}
#[test]
fn late_rollback_after_interrupted_delete_preserves_backup_without_resurrection() {
    let scratch = Scratch::new();
    let (path, backup, journal) =
        transaction(&scratch.0, b"deleted-conversation", b"current", None);
    let record = parse_json(fs::read_to_string(&journal).unwrap().trim()).unwrap();
    let input = object([
        ("sessionKey", string("session")),
        ("transactionId", field(&record, "transactionId").clone()),
        ("action", string("rollback")),
    ]);
    let backup_bytes = fs::read(&backup).unwrap();
    let journal_bytes = fs::read(&journal).unwrap();
    // Persisted state at unlink-success / sidecar-cleanup-not-yet-started.
    fs::remove_file(&path).unwrap();
    let report = recover_chats(std::slice::from_ref(&scratch.0));
    assert_eq!(report.failures.len(), 1);
    assert_eq!(report.rolled_back, 0);
    let error = finalize(&input, &scratch.0, &path).unwrap_err();
    assert_eq!(error.code.as_deref(), Some("replace_transaction_missing"));
    assert!(!path.exists());
    assert_eq!(fs::read(&backup).unwrap(), backup_bytes);
    assert_eq!(fs::read(&journal).unwrap(), journal_bytes);
}
#[test]
fn explicit_rollback_accepts_a_current_transcript_that_grew_past_read_limit() {
    let scratch = Scratch::new();
    let (path, backup, journal) = transaction(&scratch.0, b"original-data", b"current", None);
    let record = parse_json(fs::read_to_string(&journal).unwrap().trim()).unwrap();
    File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(MAX_BYTES + 1)
        .unwrap();
    let input = object([
        ("sessionKey", string("session")),
        ("transactionId", field(&record, "transactionId").clone()),
        ("action", string("rollback")),
    ]);
    finalize(&input, &scratch.0, &path).unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"original-data");
    assert!(!backup.exists() && !journal.exists());
}

#[cfg(unix)]
#[test]
fn explicit_rollback_rejects_nonregular_current_without_consuming_backups() {
    for kind in ["directory", "symlink", "fifo"] {
        let scratch = Scratch::new();
        let (path, backup, journal) = transaction(&scratch.0, b"original-data", b"current", None);
        let record = parse_json(fs::read_to_string(&journal).unwrap().trim()).unwrap();
        let journal_bytes = fs::read(&journal).unwrap();
        fs::remove_file(&path).unwrap();
        let outside = scratch.0.join("outside");
        fs::write(&outside, b"outside-data").unwrap();
        match kind {
            "symlink" => std::os::unix::fs::symlink(&outside, &path).unwrap(),
            "directory" => fs::create_dir(&path).unwrap(),
            "fifo" => assert!(
                std::process::Command::new("mkfifo")
                    .arg(&path)
                    .status()
                    .unwrap()
                    .success()
            ),
            _ => unreachable!(),
        }
        let input = object([
            ("sessionKey", string("session")),
            ("transactionId", field(&record, "transactionId").clone()),
            ("action", string("rollback")),
        ]);
        let error = finalize(&input, &scratch.0, &path)
            .expect_err("nonregular current must not be replaced");
        assert_eq!(error.code, Some("replace_transaction_missing".into()));
        assert_eq!(fs::read(&backup).unwrap(), b"original-data");
        assert_eq!(fs::read(&journal).unwrap(), journal_bytes);
        assert_eq!(fs::read(&outside).unwrap(), b"outside-data");
        use std::os::unix::fs::FileTypeExt;
        let actual = fs::symlink_metadata(&path).unwrap().file_type();
        match kind {
            "symlink" => assert!(actual.is_symlink()),
            "directory" => assert!(actual.is_dir()),
            "fifo" => assert!(actual.is_fifo()),
            _ => unreachable!(),
        }
    }
}

#[test]
fn error_message_and_requested_ids_preserve_raw_utf16_units() {
    let value = JsValue::String(JsString::from_utf16(vec![0xd800]));
    let message = prefixed_value("Transcript entry not found: ", &value).unwrap();
    let error = error("fork_entry_not_found", message);
    let mut expected = "Transcript entry not found: "
        .encode_utf16()
        .collect::<Vec<_>>();
    expected.push(0xd800);
    assert_eq!(error.message.units(), expected);
    let other = JsValue::String(JsString::from_utf16(vec![0xdc00]));
    assert!(!strict_id(&value, &other));
    assert_eq!(
        text(&object([("id", value.clone())]), "id"),
        text(&object([("id", other)]), "id"),
        "lossy display collides but identity must not"
    );
    let request = crate::runtime::connection::RequestError {
        code: "PROJECT_NOT_FOUND".into(),
        message: JsString::from_utf16(vec![0xd800, 0x0041]),
        details: None,
        validation: None,
    };
    let converted = request_error(request);
    assert_eq!(converted.message.units(), [0xd800, 0x0041]);
}
#[cfg(unix)]
#[test]
fn write_and_read_refuse_symlink_ancestor_without_touching_outside() {
    use std::os::unix::fs::symlink;
    let scratch = Scratch::new();
    let outside = Scratch::new();
    symlink(&outside.0, scratch.0.join("alias")).unwrap();
    assert!(write_new(&scratch.0.join("alias/payload"), b"escape").is_err());
    assert!(!outside.0.join("payload").exists());
    fs::write(outside.0.join("original"), b"keep").unwrap();
    assert!(read_bytes(&scratch.0.join("alias/original")).is_err());
}

#[cfg(unix)]
#[test]
fn unreadable_journal_preserves_transcript_and_every_sidecar() {
    for kind in ["directory", "symlink", "fifo"] {
        bounded_nonregular_case(&format!("journal:{kind}"));
    }
}

fn accepted_line(session: &str) -> Vec<u8> {
    body(&[object([
        ("type", string("accepted_input")),
        ("sessionId", string(session)),
        ("sequence", JsValue::Number(1.)),
        ("entryId", string("input")),
        ("turnId", string("old")),
        ("createdAt", string("2026-01-01T00:00:00.000Z")),
        (
            "messages",
            json(r#"[{"role":"user","content":[{"type":"text","text":"original prompt"}]}]"#),
        ),
    ])])
    .unwrap()
}

#[tokio::test]
async fn public_delete_removes_replacement_artifacts_before_session_key_reuse() {
    use crate::runtime::turn::gateway::GatewayRequests;
    let (_scratch, requests) = super::super::management_tests::setup();
    let chat = super::super::methods::session_dir::bounded_chat_dir(
        &requests.options.home,
        &requests.options.project,
    )
    .unwrap();
    fs::create_dir_all(&chat).unwrap();
    let (path, backup, journal) = transaction(&chat, &accepted_line("session"), b"", None);
    let other = chat.join(".session.not-a-uuid.replace.bak");
    fs::write(&other, b"unrelated").unwrap();
    requests
        .request(
            "delete_session".into(),
            object([
                (
                    "projectKey",
                    string(requests.options.project.to_string_lossy().as_ref()),
                ),
                ("sessionKey", string("session")),
            ]),
        )
        .await
        .unwrap();
    assert!(!path.exists() && !backup.exists() && !journal.exists());
    assert_eq!(fs::read(&other).unwrap(), b"unrelated");
    let fresh = b"{\"type\":\"accepted_input\",\"turnId\":\"fresh-session\"}\n";
    fs::write(&path, fresh).unwrap();
    let report = recover_chats(&[chat]);
    assert!(report.failures.is_empty());
    assert_eq!(report.rolled_back, 0);
    assert_eq!(fs::read(path).unwrap(), fresh);
    requests.project.dispose().await.unwrap();
    requests.owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn concurrent_source_forks_publish_distinct_targets_and_keep_source() {
    let (_scratch, requests) = super::super::management_tests::setup();
    let chat = super::super::methods::session_dir::bounded_chat_dir(
        &requests.options.home,
        &requests.options.project,
    )
    .unwrap();
    fs::create_dir_all(&chat).unwrap();
    let original = accepted_line("session");
    fs::write(chat.join("session.jsonl"), &original).unwrap();
    let ports = storage(
        requests.options.home.clone(),
        requests.options.project.clone(),
        requests.owner.clone(),
    );
    let fork = ports.fork_session.unwrap();
    let input = object([
        ("sessionKey", string("session")),
        ("fromEntryId", string("input")),
    ]);
    let (left, right) = tokio::join!(fork(input.clone()), fork(input));
    let left = left.unwrap();
    let right = right.unwrap();
    let left = field(&left, "newSessionKey").as_string().unwrap();
    let right = field(&right, "newSessionKey").as_string().unwrap();
    assert_ne!(left, right);
    for key in [left, right] {
        assert!(
            chat.join(format!("{}.jsonl", sanitize_session_id(key)))
                .is_file()
        );
    }
    assert_eq!(fs::read(chat.join("session.jsonl")).unwrap(), original);
    requests.project.dispose().await.unwrap();
    requests.owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn aliased_transcript_replacements_cannot_both_rewrite_the_old_turn() {
    let (_scratch, requests) = super::super::management_tests::setup();
    let chat = super::super::methods::session_dir::bounded_chat_dir(
        &requests.options.home,
        &requests.options.project,
    )
    .unwrap();
    fs::create_dir_all(&chat).unwrap();
    fs::write(chat.join("a-b.jsonl"), accepted_line("a/b")).unwrap();
    let replace = storage(
        requests.options.home.clone(),
        requests.options.project.clone(),
        requests.owner.clone(),
    )
    .replace_last_turn
    .unwrap();
    let input = |key: &str, turn: &str| {
        object([
            ("sessionKey", string(key)),
            ("expectedTurnId", string("old")),
            ("replacementTurnId", string(turn)),
        ])
    };
    let (left, right) = tokio::join!(
        replace(input("a/b", "left")),
        replace(input("a-b", "right"))
    );
    assert_eq!(
        usize::from(left.is_ok()) + usize::from(right.is_ok()),
        1,
        "{left:?}; {right:?}"
    );
    assert_eq!(
        fs::read_dir(&chat)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".replace.bak"))
            .count(),
        1
    );
    requests.project.dispose().await.unwrap();
    requests.owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn real_storage_commit_during_active_turn_unlinks_only_its_sidecars() {
    let (_scratch, requests) = super::super::management_tests::setup();
    let chat = super::super::methods::session_dir::bounded_chat_dir(
        &requests.options.home,
        &requests.options.project,
    )
    .unwrap();
    fs::create_dir_all(&chat).unwrap();
    let (path, backup, journal) = transaction(&chat, b"backup", b"accepted new bytes", None);
    let record = parse_json(&fs::read_to_string(&journal).unwrap()).unwrap();
    let key = JsString::from("session");
    let run = JsString::from("active-run");
    let turn = requests.owner.begin_turn(&key, &run).unwrap();
    let writer = crate::runtime::session::transcript_locks::cold_write_lease(&path)
        .unwrap()
        .unwrap();
    let finalize = storage(
        requests.options.home.clone(),
        requests.options.project.clone(),
        requests.owner.clone(),
    )
    .finalize_last_turn_replacement
    .unwrap();
    finalize(object([
        ("sessionKey", string("session")),
        ("transactionId", field(&record, "transactionId").clone()),
        ("action", string("commit")),
    ]))
    .await
    .unwrap();
    assert_eq!(fs::read(path).unwrap(), b"accepted new bytes");
    assert!(!backup.exists() && !journal.exists());
    drop(writer);
    requests.owner.end_turn(&key, Some(&run), Some(&turn));
    requests.project.dispose().await.unwrap();
    requests.owner.shutdown().await.unwrap();
}

#[tokio::test]
async fn source_fork_waits_for_the_canonical_transcript_fence() {
    let (_scratch, requests) = super::super::management_tests::setup();
    let chat = super::super::methods::session_dir::bounded_chat_dir(
        &requests.options.home,
        &requests.options.project,
    )
    .unwrap();
    fs::create_dir_all(&chat).unwrap();
    let path = chat.join("session.jsonl");
    let original = accepted_line("session");
    fs::write(&path, &original).unwrap();
    let fence = crate::runtime::session::transcript_locks::acquire(&path)
        .await
        .unwrap();
    let fork = storage(
        requests.options.home.clone(),
        requests.options.project.clone(),
        requests.owner.clone(),
    )
    .fork_session
    .unwrap();
    let mut pending = fork(object([
        ("sessionKey", string("session")),
        ("fromEntryId", string("input")),
    ]));
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(100), &mut pending)
            .await
            .is_err(),
        "a held canonical fence must block the actual source read"
    );
    assert_eq!(fs::read(&path).unwrap(), original);
    drop(fence);
    tokio::time::timeout(std::time::Duration::from_secs(5), pending)
        .await
        .unwrap()
        .unwrap();
    requests.project.dispose().await.unwrap();
    requests.owner.shutdown().await.unwrap();
}

#[cfg(unix)]
fn bounded_nonregular_case(case: &str) {
    use std::{
        process::{Command, Stdio},
        time::{Duration, Instant},
    };
    let module = module_path!().split_once("::").unwrap().1;
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            &format!("{module}::nonregular_artifact_worker"),
            "--ignored",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("PDRS_HISTORY_NONREGULAR_CASE", case)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            child.kill().unwrap();
            let output = child.wait_with_output().unwrap();
            panic!(
                "nonregular case {case} exceeded5s: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "case {case}: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("PDRS_NONREGULAR_CASE_OK"),
        "worker must actually run, not zero selected tests"
    );
}
#[cfg(unix)]
#[test]
fn recovery_rejects_nonregular_current_without_consuming_sidecars() {
    for kind in ["directory", "symlink", "fifo"] {
        bounded_nonregular_case(&format!("recovery-current:{kind}"));
    }
}
#[cfg(unix)]
#[test]
fn replace_fifo_leaf_and_private_artifact_read_preserve_all_material() {
    bounded_nonregular_case("replace-fifo:fifo");
}
#[cfg(unix)]
#[test]
#[ignore = "only launched by bounded parent with a concrete case"]
fn nonregular_artifact_worker() {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let case = std::env::var("PDRS_HISTORY_NONREGULAR_CASE").unwrap();
    let (operation, kind) = case.split_once(':').unwrap();
    assert!(["recovery-current", "journal", "replace-fifo"].contains(&operation));
    assert!(["directory", "symlink", "fifo"].contains(&kind));
    let scratch = Scratch::new();
    let original = accepted_line("session");
    let current = b"{\"type\":\"accepted_input\",\"turnId\":\"new-turn\"}\n";
    let (path, backup, journal) = transaction(&scratch.0, &original, current, None);
    let journal_bytes = fs::read(&journal).unwrap();
    let leaf = if operation == "journal" {
        &journal
    } else {
        &path
    };
    fs::remove_file(leaf).unwrap();
    let sentinel = scratch.0.join("outside-sentinel");
    fs::write(
        &sentinel,
        if operation == "journal" {
            journal_bytes.as_slice()
        } else {
            original.as_slice()
        },
    )
    .unwrap();
    match kind {
        "directory" => {
            fs::create_dir(leaf).unwrap();
            fs::write(leaf.join("keep"), b"directory material").unwrap();
        }
        "symlink" => std::os::unix::fs::symlink(&sentinel, leaf).unwrap(),
        "fifo" => assert!(
            std::process::Command::new("/usr/bin/mkfifo")
                .args(["-m", "600"])
                .arg(leaf)
                .status()
                .unwrap()
                .success()
        ),
        _ => unreachable!(),
    }
    let identity = fs::symlink_metadata(leaf).unwrap();
    let names = || {
        let mut names: Vec<_> = fs::read_dir(&scratch.0)
            .unwrap()
            .map(|row| row.unwrap().file_name())
            .collect();
        names.sort();
        names
    };
    let before_names = names();
    if operation == "replace-fifo" {
        assert_eq!(kind, "fifo");
        // This calls the actual inner read_open_artifact guard, with no
        // require_current_transcript or storage::read_open_transcript gate.
        let inner = read_bytes(&path).expect_err("FIFO artifact is not regular");
        assert_eq!(
            inner.message,
            JsString::from("Transcript artifact must be a regular file.")
        );
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let (_unused, requests) = super::super::management_tests::setup();
            let state = State {
                home: scratch.0.clone(),
                fallback: scratch.0.clone(),
                owner: requests.owner.clone(),
                instance: uuid(),
            };
            let input = object([
                ("sessionKey", string("session")),
                ("expectedTurnId", string("old")),
                ("replacementTurnId", string("replacement-fifo")),
            ]);
            // The public replace implementation first reaches the separate
            // storage transcript gate. Do not claim this reaches line483.
            let error = replace(&state, &input, &scratch.0, &path)
                .expect_err("replace must reject leaf FIFO");
            assert!(
                error
                    .message
                    .to_utf8()
                    .unwrap()
                    .contains("not a regular file")
            );
            drop(state);
            requests.owner.shutdown().await.unwrap();
            requests.project.dispose().await.unwrap();
        });
    } else {
        let report = recover_chats(std::slice::from_ref(&scratch.0));
        assert_eq!(
            report.failures.len(),
            1,
            "read failure must not become absent data"
        );
        assert_eq!(report.rolled_back, 0);
        assert_eq!(report.committed, 0);
        assert_eq!(report.cleaned, 0);
        if kind == "fifo" {
            assert!(
                report.failures[0]
                    .message
                    .contains("Transcript artifact must be a regular file."),
                "FIFO must reach read_open_artifact, not a preceding unrelated gate"
            );
        }
    }
    assert_eq!(names(), before_names, "no backup/journal/publish pollution");
    assert_eq!(fs::read(&backup).unwrap(), original);
    if operation == "journal" {
        assert_eq!(fs::read(&path).unwrap(), current);
    } else {
        assert_eq!(fs::read(&journal).unwrap(), journal_bytes);
    }
    let after = fs::symlink_metadata(leaf).unwrap();
    assert_eq!((after.dev(), after.ino()), (identity.dev(), identity.ino()));
    match kind {
        "directory" => {
            assert!(after.is_dir());
            assert_eq!(fs::read(leaf.join("keep")).unwrap(), b"directory material");
        }
        "symlink" => {
            assert!(after.file_type().is_symlink());
            assert_eq!(fs::read_link(leaf).unwrap(), sentinel);
        }
        "fifo" => assert!(after.file_type().is_fifo()),
        _ => unreachable!(),
    }
    assert_eq!(
        fs::read(&sentinel).unwrap(),
        if operation == "journal" {
            journal_bytes
        } else {
            original
        }
    );
    println!("PDRS_NONREGULAR_CASE_OK");
}
