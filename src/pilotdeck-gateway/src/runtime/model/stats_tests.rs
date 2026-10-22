use super::*;

/// `RouterRuntime.ts:936` / `:972` write `endedAt` with
/// `(deps.now?.() ?? new Date()).toISOString()`. The canonical output is
/// `YYYY-MM-DDTHH:mm:ss.sssZ` for years 0000-9999 and
/// `±YYYYYY-MM-DDTHH:mm:ss.sssZ` outside that range; both are interpreted.
/// The expected millisecond values below are the baseline's `Date.parse`
/// output (also pinned end to end by the `retention-expanded-years` oracle).
#[test]
fn parses_the_canonical_to_iso_string_shapes() {
    assert_eq!(
        parse_date_ms("2026-03-01T10:00:00.000Z"),
        Some(1_772_359_200_000.0)
    );
    assert_eq!(
        parse_date_ms("2026-03-01T10:00:00.123Z"),
        Some(1_772_359_200_123.0)
    );
    // Millisecond precision and real leap days, including the `%400` branch
    // (`1600`) that a `%4`-only rule would get wrong.
    assert_eq!(
        parse_date_ms("2024-02-29T23:59:59.999Z"),
        Some(1_709_251_199_999.0)
    );
    assert_eq!(
        parse_date_ms("1600-02-29T00:00:00.000Z"),
        Some(-11_670_998_400_000.0)
    );
    // The whole 4-digit-year range `toISOString()` emits, end to end.
    assert_eq!(
        parse_date_ms("0000-01-01T00:00:00.000Z"),
        Some(-62_167_219_200_000.0)
    );
    assert_eq!(
        parse_date_ms("9999-12-31T23:59:59.999Z"),
        Some(253_402_300_799_999.0)
    );
    // The expanded `±YYYYYY` form, including both ends of the `Date` range.
    assert_eq!(
        parse_date_ms("-000001-12-31T23:59:59.999Z"),
        Some(-62_167_219_200_001.0)
    );
    assert_eq!(
        parse_date_ms("-010000-01-01T00:00:00.000Z"),
        Some(-377_736_739_200_000.0)
    );
    assert_eq!(
        parse_date_ms("+010000-01-01T00:00:00.000Z"),
        Some(253_402_300_800_000.0)
    );
    assert_eq!(
        parse_date_ms("+275760-09-13T00:00:00.000Z"),
        Some(8_640_000_000_000_000.0)
    );
    assert_eq!(
        parse_date_ms("-271821-04-20T00:00:00.000Z"),
        Some(-8_640_000_000_000_000.0)
    );
    assert_eq!(
        parse_date_ms("+275760-09-12T23:59:59.999Z"),
        Some(8_639_999_999_999_999.0)
    );
    assert_eq!(
        parse_date_ms("-271821-04-20T00:00:00.001Z"),
        Some(-8_639_999_999_999_999.0)
    );
    for value in [
        // V8 accepts all of these; retention keeps the row instead (divergence).
        "2026-03-01T10:00:00Z",          // no milliseconds
        "2026-03-01t10:00:00.000z",      // lower-case t/z
        "2026-03-01 10:00:00.000Z",      // space instead of `T`
        "2026-03-01T18:00:00.000+08:00", // numeric offset
        "2026-03-01",                    // date only
        "2026-3-1",                      // single-digit fields
        "Mon, 01 Jan 2024 00:00:00 GMT", // RFC 2822
        // V8 also accepts these (rolling the calendar over); the port keeps.
        "2023-02-29T00:00:00.000Z",
        "2026-02-30T00:00:00.000Z",
        "1900-02-29T00:00:00.000Z", // century non-leap: V8 rolls to March 1
        "2100-02-29T00:00:00.000Z",
        "2026-01-01T24:00:00.000Z", // hour 24: V8 rolls to the next midnight
        // Expanded years are only canonical with six digits and the exact
        // millisecond form; V8 parses several of these, the port keeps them.
        "+000000-01-01T00:00:00.000Z",
        "-000000-01-01T00:00:00.000Z",
        "+000001-01-01T00:00:00.000Z",
        "+010000-01-01T00:00:00Z",
        "+010000-01-01T00:00:00.000z",
        "+010000-01-01T00:00:00.000+00:00",
        "+275760-09-13T00:00:00.0000Z",
        "010000-01-01T00:00:00.000Z",
        // Outside the `Date` range: `Date.parse` is NaN, so keep the row.
        "+275760-09-13T00:00:00.001Z",
        "-271821-04-19T23:59:59.999Z",
        "+999999-12-31T23:59:59.999Z",
        "-999999-12-31T23:59:59.999Z",
        // Valid shape, impossible calendar/time.
        "+010000-02-30T00:00:00.000Z",
        "+010000-13-01T00:00:00.000Z",
        "+010000-01-01T24:00:00.000Z",
        "+010000-01-01T00:00:00.000",
        // Rejected by both.
        "2026-13-01T00:00:00.000Z",
        "2026-01-32T00:00:00.000Z",
        "not-a-date",
        "",
    ] {
        assert_eq!(parse_date_ms(value), None, "{value}");
    }
}

/// The `retention-expanded-years` oracle pins these rows against the real TS
/// collector; `now` is just past the last valid `Date` instant so a parsed
/// canonical timestamp is expired while an unparseable one is kept.
#[test]
fn retention_expands_years_and_rejects_out_of_range_instants() {
    let record = |ended: &str| {
        let mut object = JsObject::new();
        object.insert("endedAt".into(), JsValue::String(ended.into()));
        JsValue::Object(object)
    };
    let now = 8_640_000_000_000_002.0;
    for expired in [
        "-000001-12-31T23:59:59.999Z",
        "-010000-01-01T00:00:00.000Z",
        "+010000-01-01T00:00:00.000Z",
        "+275760-09-13T00:00:00.000Z",
        "-271821-04-20T00:00:00.000Z",
    ] {
        assert!(
            !is_retained_record(Some(0.0), now, &record(expired)),
            "{expired} must expire"
        );
    }
    for kept in [
        // `Date.parse` is NaN outside the valid range.
        "+275760-09-13T00:00:00.001Z",
        "-271821-04-19T23:59:59.999Z",
        "+999999-12-31T23:59:59.999Z",
        // Non-canonical expanded years are kept by design (divergence).
        "+000000-01-01T00:00:00.000Z",
        "-000000-01-01T00:00:00.000Z",
        "+010000-01-01T00:00:00Z",
    ] {
        assert!(
            is_retained_record(Some(0.0), now, &record(kept)),
            "{kept} must be kept"
        );
    }
}

/// codex r2's three P0 inputs plus the other shapes its grammar mis-accepted.
/// All are non-canonical, so retention must keep the accounting row instead of
/// producing a finite old instant.
#[test]
fn retention_keeps_every_reviewed_non_canonical_timestamp() {
    let record = |ended: &str| {
        let mut object = JsObject::new();
        object.insert("endedAt".into(), JsValue::String(ended.into()));
        JsValue::Object(object)
    };
    let now = 1_772_359_200_000.0;
    for ended in [
        // P0: fraction after the minute, with no seconds field.
        "2000-01-01T00:00.500Z",
        // P0: 24:00 with a non-zero sub-millisecond fraction.
        "2000-01-01T24:00:00.0001Z",
        // P0: unsigned six-digit year.
        "002000-01-01T00:00:00Z",
        "2000-01-01T00:00:00+99:99",
        "2000-01-01T00:00:00+01:99",
        "2000-01-01T00:00:00+24:00",
        "2001-02-30T00:00:00Z",
        "2026-02-30T00:00:00.000Z",
        "not-a-date",
        // P0: multi-byte offset used to panic the byte-slicing parser; keep it
        // last so this loop reaches the assertions above first.
        "2000-01-01T00:00:00+汉a",
    ] {
        assert!(
            is_retained_record(Some(1_000.0), now, &record(ended)),
            "{ended} must be kept (not the canonical toISOString shape)"
        );
    }
}

#[test]
fn non_ascii_offset_is_unparseable_without_panicking() {
    assert_eq!(parse_date_ms("2000-01-01T00:00:00+汉a"), None);
}

/// The DST-gap input from codex r2 P0-3: a zone-less local time. V8 resolves
/// `2026-03-08T02:30:00` in `America/New_York` to `07:30Z`; the old two-pass
/// guess produced `06:30Z` and deleted a row TS kept. The port now keeps it.
#[test]
fn retention_keeps_zone_less_timestamps_regardless_of_dst() {
    let mut object = JsObject::new();
    object.insert(
        "endedAt".into(),
        JsValue::String("2026-03-08T02:30:00".into()),
    );
    let record = JsValue::Object(object);
    // `now = 2026-03-08T08:00:00Z`, retention = 1h; TS keeps (07:30Z >= 07:00Z).
    let now = 1_772_956_800_000.0;
    assert!(is_retained_record(Some(3_600_000.0), now, &record));
}

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
        TokenStatsCollector::with_clock(Some(&config), Some(&home), Arc::new(move || now));
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

/// The disclosed divergence, end to end: the real TS collector's `Date.parse`
/// yields a finite past instant for all of these shapes and deletes the rows;
/// the port keeps them. Decision: `state/decisions-20261022.md`「A6.4-pr9b」
/// (option B); disclosure and real-TS values:
/// `specs/gateway-parity/accepted-diffs.md` A6.4-STATS-RETENTION-001.
/// Reproduced with `now = 2026-03-03T00:00:00.000Z`. Pinning the keep side
/// means a future parser widening cannot start deleting rows by accident.
#[test]
fn retention_keeps_non_canonical_rows_the_baseline_deletes() {
    let home = temp_home("divergence");
    let journal = home.join("stats.jsonl");
    let kept = [
        "2026-03-01",                    // date only
        "2026-03-01T10:00:00Z",          // no milliseconds
        "2026-03-01T18:00:00.000+08:00", // numeric offset
        "Sun, 01 Mar 2026 10:00:00 GMT", // RFC 2822
        "2026-02-30T10:00:00.000Z",      // calendar rollover (V8: March 2)
        "2026-03-01T24:00:00.000Z",      // hour 24 (V8: next day)
        "2026-03-01T10:00:00.0000Z",     // four fractional digits
        "+000000-01-01T00:00:00.000Z",   // non-canonical expanded zero year
    ];
    let mut lines = String::new();
    for (index, ended) in kept.iter().enumerate() {
        lines.push_str(&format!(
            r#"{{"sessionId":"keep-{index}","startedAt":"2026-03-01T00:00:00.000Z","endedAt":"{ended}","usage":{{}}}}"#
        ));
        lines.push('\n');
    }
    lines.push_str(
        r#"{"sessionId":"expired","startedAt":"2026-03-01T00:00:00.000Z","endedAt":"2026-03-01T09:00:00.000Z","usage":{}}"#,
    );
    lines.push('\n');
    fs::write(&journal, lines).unwrap();
    let config = StatsConfig {
        enabled: true,
        model_pricing: None,
        file_path: Some(journal.clone()),
        retention_ms: Some(1_000.0),
        baseline_model: None,
    };
    let now = 1_772_496_000_000.0; // 2026-03-03T00:00:00.000Z
    let mut stats =
        TokenStatsCollector::with_clock(Some(&config), Some(&home), Arc::new(move || now));
    let content = fs::read_to_string(&journal).unwrap();
    for index in 0..kept.len() {
        assert!(content.contains(&format!("\"keep-{index}\"")), "{content}");
    }
    assert!(!content.contains("\"expired\""), "{content}");
    assert_eq!(
        stats
            .snapshot()
            .get("totalRequests")
            .and_then(JsValue::as_number),
        Some(kept.len() as f64)
    );
    fs::remove_dir_all(&home).unwrap();
}

/// TS `appendRecord` (`TokenStatsCollector.ts:257-261`) falls back to an
/// unlocked append when the 25×1ms lock attempt fails; a lock timeout must not
/// drop a fresh accounting row.
#[test]
fn append_falls_back_to_unlocked_write_like_ts() {
    let home = temp_home("lock-append");
    let journal = home.join("stats.jsonl");
    fs::write(&journal, "").unwrap();
    let config = retaining(&journal);
    let now = 1_772_366_400_500.0;
    let mut stats =
        TokenStatsCollector::with_clock(Some(&config), Some(&home), Arc::new(move || now));
    let lock = home.join("stats.jsonl.retention.lock");
    fs::write(&lock, b"").unwrap();
    let mut record = parse_json(
        r#"{"sessionId":"locked","startedAt":"2026-03-01T11:59:30.000Z","endedAt":"2026-03-01T11:59:59.500Z","usage":{}}"#,
    )
    .unwrap();
    stats.observe(&mut record);
    let content = fs::read_to_string(&journal).unwrap();
    assert!(content.contains("\"locked\""), "{content}");
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
        TokenStatsCollector::with_clock(Some(&config), Some(&home), Arc::new(move || now));
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
