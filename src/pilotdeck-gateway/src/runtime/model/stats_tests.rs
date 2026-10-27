use super::*;

#[test]
fn retention_keeps_malformed_rows_and_uses_the_boundary() {
    let record = |ended: &str| {
        let mut object = JsObject::new();
        object.insert("endedAt".into(), JsValue::String(ended.into()));
        JsValue::Object(object)
    };
    let now = 1_772_359_200_000.0;
    assert!(is_retained_record(
        None,
        now,
        &record("2000-01-01T00:00:00.000Z")
    ));
    assert!(!is_retained_record(
        Some(1_000.0),
        now,
        &record("2026-03-01T09:59:58.999Z")
    ));
    assert!(is_retained_record(
        Some(1_000.0),
        now,
        &record("2026-03-01T09:59:59.000Z")
    ));
    assert!(is_retained_record(
        Some(1_000.0),
        now,
        &record("not-a-date")
    ));
    assert!(is_retained_record(
        Some(1_000.0),
        now,
        &record("2999-01-01T00:00:00.000Z")
    ));
}

/// The TS gold (`retention-boundary`) pins this with a real `Date.now`; this is
/// the same boundary one millisecond at a time.
#[test]
fn retention_boundary_is_inclusive_and_millisecond_exact() {
    let record = |ended: &str| {
        let mut object = JsObject::new();
        object.insert("endedAt".into(), JsValue::String(ended.into()));
        JsValue::Object(object)
    };
    let now = 1_772_366_400_500.0; // 2026-03-01T12:00:00.500Z
    assert!(is_retained_record(
        Some(60_000.0),
        now,
        &record("2026-03-01T11:59:00.500Z")
    ));
    assert!(is_retained_record(
        Some(60_000.0),
        now,
        &record("2026-03-01T11:59:00.501Z")
    ));
    assert!(!is_retained_record(
        Some(60_000.0),
        now,
        &record("2026-03-01T11:59:00.499Z")
    ));
}

fn retaining(file: &Path) -> StatsConfig {
    StatsConfig {
        enabled: true,
        model_pricing: None,
        file_path: Some(file.to_path_buf()),
        retention_ms: Some(60_000.0),
        baseline_model: None,
    }
}

fn temp_home(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("pdrs-stats-{tag}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    root
}

/// Rebuild reaches `parse_date_ms` through `is_retained`; the old parser
/// panicked on this row and took the whole collector constructor down. The row
/// must survive compaction instead.
#[test]
fn rebuild_keeps_non_canonical_rows_without_panicking() {
    let home = temp_home("rebuild");
    let journal = home.join("stats.jsonl");
    let row = |session: &str, ended: &str| {
        format!(
            r#"{{"sessionId":"{session}","startedAt":"2026-03-01T00:00:00.000Z","endedAt":"{ended}","usage":{{}},"provider":"anthropic","model":"claude-sonnet-4-5-20250929"}}"#
        )
    };
    fs::write(
        &journal,
        format!(
            "{}\n{}\n",
            row("multi-byte", "2000-01-01T00:00:00+汉a"),
            row("expired", "2000-01-01T00:00:01.000Z")
        ),
    )
    .unwrap();
    let config = retaining(&journal);
    let now = 1_772_366_400_500.0;
    let mut stats =
        TokenStatsCollector::with_clock(Some(&config), Some(&home), Arc::new(move || now))
            .expect("#879 stats retention collector");
    let snapshot = stats.snapshot();
    assert_eq!(
        snapshot.get("totalRequests").and_then(JsValue::as_number),
        Some(1.0)
    );
    let content = fs::read_to_string(&journal).unwrap();
    assert!(content.contains("multi-byte"), "{content}");
    assert!(!content.contains("expired"), "{content}");
    fs::remove_dir_all(&home).unwrap();
}

/// Lock timeout publishes a sidecar without exposing the row to main-file replacement.
#[test]
fn append_falls_back_to_sidecar_without_losing_visibility() {
    let home = temp_home("lock-append");
    let journal = home.join("stats.jsonl");
    fs::write(&journal, "").unwrap();
    let config = retaining(&journal);
    let now = 1_772_366_400_500.0;
    let mut stats =
        TokenStatsCollector::with_clock(Some(&config), Some(&home), Arc::new(move || now))
            .expect("#879 stats lock collector");
    let lock = home.join("stats.jsonl.retention.lock");
    fs::write(&lock, b"").unwrap();
    let mut record = parse_json(
        r#"{"sessionId":"locked","startedAt":"2026-03-01T11:59:30.000Z","endedAt":"2026-03-01T11:59:59.500Z","usage":{}}"#,
    )
    .unwrap();
    stats.observe(&mut record);
    assert_eq!(fs::read_to_string(&journal).unwrap(), "");
    assert_eq!(read_stats_records(&journal), vec![record]);
    let mut reader =
        TokenStatsCollector::with_clock(Some(&config), Some(&home), Arc::new(move || now))
            .expect("#879 stats reader collector");
    assert_eq!(
        reader
            .snapshot()
            .get("totalRequests")
            .and_then(JsValue::as_number),
        Some(1.0)
    );
    fs::remove_file(&lock).unwrap();
    fs::remove_dir_all(&home).unwrap();
}

/// TS `clear()` (`TokenStatsCollector.ts:230-243`) also retries the lock and
/// truncates on timeout; pin that fallback so a refactor does not silently
/// change lock-timeout semantics.
#[test]
fn clear_truncates_on_lock_timeout_like_ts() {
    let home = temp_home("lock-clear");
    let journal = home.join("stats.jsonl");
    fs::write(&journal, "stale\n").unwrap();
    let config = retaining(&journal);
    let now = 1_772_366_400_500.0;
    let mut stats =
        TokenStatsCollector::with_clock(Some(&config), Some(&home), Arc::new(move || now))
            .expect("#879 stats append collector");
    let lock = home.join("stats.jsonl.retention.lock");
    fs::write(&lock, b"").unwrap();
    stats.clear();
    assert_eq!(fs::read_to_string(&journal).unwrap(), "");
    fs::remove_file(&lock).unwrap();
    fs::remove_dir_all(&home).unwrap();
}

/// Node `readFileSync(path, "utf8")` decodes invalid bytes to U+FFFD instead of
/// throwing (`node -e` probe: `"a\uFFFDb\n"`); a corrupt byte must not empty
/// the whole journal (and, with retention, let compaction rewrite it empty).
#[test]
fn journal_reader_replaces_invalid_utf8_like_node() {
    let home = temp_home("lossy");
    let journal = home.join("stats.jsonl");
    fs::write(&journal, [b'a', 0xff, b'b', b'\n']).unwrap();
    assert_eq!(
        read_bounded_text(&journal, 100).as_deref(),
        Some("a\u{fffd}b\n")
    );
    let valid = r#"{"sessionId":"ok","startedAt":"2026-03-01T00:00:00.000Z"}"#;
    let mut bytes = vec![b'{', 0xff, b'}', b'\n'];
    bytes.extend_from_slice(valid.as_bytes());
    bytes.push(b'\n');
    fs::write(&journal, &bytes).unwrap();
    let records = read_stats_records(&journal);
    assert_eq!(records.len(), 1);
    assert_eq!(
        text(&records[0], "sessionId").unwrap().to_utf8().unwrap(),
        "ok"
    );
    fs::remove_dir_all(&home).unwrap();
}

#[test]
fn migration_write_failure_keeps_source_and_tries_next_candidate() {
    use std::fs;
    let root = std::env::temp_dir().join(format!("pdrs-stats-migration-{}", uuid::Uuid::new_v4()));
    let router_dir = root.join("state").join("router");
    let parent = root.join("state");
    fs::create_dir_all(&router_dir).unwrap();
    let record = |session: &str, started: &str| {
        format!(
            r#"{{"sessionId":"{session}","startedAt":"{started}","endedAt":"{started}","usage":{{}}}}"#
        )
    };
    let stats_json = router_dir.join("stats.json");
    let legacy_json = parent.join("router-stats.json");
    let jsonl = router_dir.join("stats.jsonl");
    fs::write(
        &stats_json,
        format!(
            r#"{{"sessions":{{"a":{{"requestLog":[{}]}}}}}}"#,
            record("first", "2026-01-01T00:00:00.000Z")
        ),
    )
    .unwrap();
    fs::write(
        &legacy_json,
        format!(
            r#"{{"sessions":{{"b":{{"requestLog":[{}]}}}}}}"#,
            record("second", "2026-01-02T00:00:00.000Z")
        ),
    )
    .unwrap();

    let mut attempts = 0;
    let target = jsonl.clone();
    let mut write = |_path: &Path, text: &str| {
        attempts += 1;
        if attempts == 1 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "injected write failure",
            ));
        }
        fs::write(&target, text)
    };
    migrate_json_to_jsonl_with(&router_dir, &jsonl, &mut write);

    assert_eq!(attempts, 2, "the second candidate must still be attempted");
    assert!(stats_json.exists(), "failed candidate must not be renamed");
    assert!(!router_dir.join("stats.json.bak").exists());
    assert!(parent.join("router-stats.json.bak").exists());
    let written = fs::read_to_string(&jsonl).unwrap();
    assert!(written.contains("\"second\""), "{written}");
    assert!(!written.contains("\"first\""), "{written}");
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn journal_reader_stops_at_v8_string_length() {
    use std::fs;
    let root = std::env::temp_dir().join(format!("pdrs-stats-limit-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    let path = root.join("stats.jsonl");
    fs::write(&path, "0123456789").unwrap();
    assert_eq!(read_bounded_text(&path, 10).as_deref(), Some("0123456789"));
    assert_eq!(read_bounded_text(&path, 9), None);
    // UTF-16 units, not bytes: four CJK scalars are four units and 12 bytes.
    fs::write(&path, "汉字测试").unwrap();
    assert_eq!(read_bounded_text(&path, 4).as_deref(), Some("汉字测试"));
    assert_eq!(read_bounded_text(&path, 3), None);
    fs::remove_dir_all(&root).unwrap();
}

/// `os.homedir()` in TS falls back to the passwd database when HOME is
/// missing; Rust uses HOME only, so `~` stays literal. Pin that existing
/// source difference (A6.4-STATS-CWD-001) instead of inventing a home.
#[test]
fn missing_home_keeps_tilde_literal() {
    assert_eq!(
        expand_pilot_home(Some("~"), None),
        PathBuf::from("~"),
        "#879 missing HOME must keep ~ literal"
    );
    assert_eq!(
        expand_pilot_home(Some("~/data"), None),
        PathBuf::from("~/data"),
        "#879 missing HOME must keep ~/child literal"
    );
}

#[test]
fn resolve_pilot_home_matches_paths_ts() {
    use std::ffi::OsStr;
    use std::path::Path;
    let cwd = Path::new("/work");
    let home = OsStr::new("/home/me");
    assert_eq!(
        resolve_pilot_home(None, Some(home), cwd),
        Path::new("/home/me/.pilotdeck")
    );
    // `PILOT_HOME=""` is set, so `path.resolve("")` is the cwd (paths.rs:70).
    assert_eq!(resolve_pilot_home(Some(""), Some(home), cwd), cwd);
    assert_eq!(resolve_pilot_home(Some("~"), Some(home), cwd), home);
    // `normalizeHomePath("~")` returns `homedir()` verbatim, so an empty or
    // relative HOME never resolves through the cwd.
    assert_eq!(
        resolve_pilot_home(Some("~"), Some(OsStr::new("")), cwd),
        PathBuf::new(),
        "#879 ~ with empty HOME stays verbatim"
    );
    assert_eq!(
        resolve_pilot_home(Some("~"), Some(OsStr::new("rel-home")), cwd),
        PathBuf::from("rel-home"),
        "#879 ~ with relative HOME stays verbatim"
    );
    assert_eq!(
        resolve_pilot_home(Some("~/data"), Some(home), cwd),
        Path::new("/home/me/data")
    );
    assert_eq!(
        resolve_pilot_home(Some("relative/./dir"), Some(home), cwd),
        Path::new("/work/relative/dir")
    );
    assert_eq!(
        resolve_pilot_home(Some("~/a/../b"), Some(home), cwd),
        Path::new("/home/me/b")
    );
    assert_eq!(
        resolve_pilot_home(Some("/abs"), Some(home), cwd),
        Path::new("/abs")
    );
}
