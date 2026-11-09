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
    let report = recover_chats(&[scratch.0.clone()]);
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
    let report = recover_chats(&[scratch.0.clone()]);
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
    let report = recover_chats(&[scratch.0.clone()]);
    assert_eq!(report.skipped, 1);
    assert!(backup.exists() && journal.exists());
    let mut record = read_journal(
        &journal,
        &text(
            &parse_json(std::str::from_utf8(&fs::read(&journal).unwrap()).unwrap()).unwrap(),
            "transactionId",
        ),
    )
    .unwrap();
    set(&mut record, "version", JsValue::Number(1.));
    // TS honors an owner field even on a v1 journal. Remove that live owner
    // explicitly before exercising the separate missing-backup failure.
    record = omit(&record, &["owner"]);
    fs::write(&journal, body(&[record]).unwrap()).unwrap();
    fs::remove_file(&backup).unwrap();
    let report = recover_chats(&[scratch.0.clone()]);
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
        &journal
            .file_name()
            .unwrap()
            .to_string_lossy()
            .split('.')
            .nth(2)
            .unwrap(),
    )
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
    let report = recover_chats(&[scratch.0.clone()]);
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
    let report = recover_chats(&[scratch.0.clone()]);
    assert!(report.failures.is_empty());
    assert_eq!(report.cleaned, 0);
    assert_eq!(fs::read(path).unwrap(), b"{}");
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
