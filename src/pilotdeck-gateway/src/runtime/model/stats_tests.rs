use super::*;

#[test]
fn parses_the_iso_subset_written_by_js() {
    assert_eq!(
        parse_date_ms("2026-03-01T10:00:00.000Z"),
        Some(1_772_359_200_000.0)
    );
    assert_eq!(
        parse_date_ms("2026-03-01T10:00:00Z"),
        Some(1_772_359_200_000.0)
    );
    assert_eq!(
        parse_date_ms("2026-03-01T10:00:00.5Z"),
        Some(1_772_359_200_500.0)
    );
    assert_eq!(
        parse_date_ms("2026-03-01T18:00:00+08:00"),
        Some(1_772_359_200_000.0)
    );
    assert_eq!(parse_date_ms("2026-03-01"), Some(1_772_323_200_000.0));
    assert_eq!(parse_date_ms("not-a-date"), None);
    assert_eq!(parse_date_ms("2026-13-01T00:00:00Z"), None);
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
        &record("2000-01-01T00:00:00Z")
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
        &record("2999-01-01T00:00:00Z")
    ));
}

/// Values and accept/reject decisions are `node -e 'Date.parse(v)'` output on
/// Node 22.22 (`TZ=UTC` unless noted); see RESULT.md「修复轮 1」for the raw probe.
#[test]
fn parse_date_matches_v8_acceptance() {
    // Out-of-range numeric offsets are NaN; the old port accepted them and
    // retention then deleted the row.
    assert_eq!(parse_date_ms("2000-01-01T00:00:00+99:99"), None);
    assert_eq!(parse_date_ms("2000-01-01T00:00:00+01:99"), None);
    assert_eq!(parse_date_ms("2000-01-01T00:00:00+24:00"), None);
    assert_eq!(parse_date_ms("2000-01-01T00:00:00-24:00"), None);
    // `+15:00` and `+23:59` are finite in V8 even though they exceed ISO-8601.
    assert_eq!(
        parse_date_ms("2026-03-01T10:00:00+15:00"),
        Some(1_772_305_200_000.0)
    );
    assert_eq!(
        parse_date_ms("2026-03-01T10:00:00+23:59"),
        Some(1_772_272_860_000.0)
    );
    // `±HHMM` without a colon.
    assert_eq!(
        parse_date_ms("2026-03-01T10:00:00+0800"),
        Some(1_772_330_400_000.0)
    );
    // 24:00 rolls to the next day only when min/sec/ms are zero.
    assert_eq!(
        parse_date_ms("2026-03-01T24:00:00.000Z"),
        Some(1_772_409_600_000.0)
    );
    assert_eq!(
        parse_date_ms("2026-03-01T24:00Z"),
        Some(1_772_409_600_000.0)
    );
    assert_eq!(parse_date_ms("2026-03-01T24:00:00.001Z"), None);
    // Lower case `t`/`z`, optional seconds, and long fractions.
    assert_eq!(
        parse_date_ms("2026-03-01t10:00:00.000z"),
        Some(1_772_359_200_000.0)
    );
    assert_eq!(
        parse_date_ms("2026-03-01T10:00Z"),
        Some(1_772_359_200_000.0)
    );
    assert_eq!(
        parse_date_ms("2026-03-01T10:00:00.1234567Z"),
        Some(1_772_359_200_123.0)
    );
    // Single-digit month/day only for the date-only form.
    assert_eq!(parse_date_ms("2026-3-1"), Some(1_772_323_200_000.0));
    assert_eq!(parse_date_ms("2026-3-1T10:00:00Z"), None);
    // V8 rolls day 30/31 over instead of rejecting; day 32 is NaN.
    assert_eq!(
        parse_date_ms("2026-02-30T00:00:00Z"),
        parse_date_ms("2026-03-02T00:00:00Z")
    );
    assert_eq!(parse_date_ms("2026-01-32T00:00:00Z"), None);
    // RFC 2822 forms.
    assert_eq!(
        parse_date_ms("Mon, 01 Jan 2024 00:00:00 GMT"),
        Some(1_704_067_200_000.0)
    );
    assert_eq!(
        parse_date_ms("Mon Jan 01 2024 00:00:00 GMT+0800"),
        Some(1_704_038_400_000.0)
    );
    assert_eq!(
        parse_date_ms("Mon, 01 Jan 2024 00:00:00 -0800"),
        Some(1_704_096_000_000.0)
    );
    // Named zones other than GMT/UT/UTC stay unparsed (retained).
    assert_eq!(parse_date_ms("Mon, 01 Jan 2024 00:00:00 EST"), None);
    // Zone-less timestamps are local time. `TZ=Asia/Shanghai` probe:
    // Date.parse("2026-03-01T10:00:00") === 1772330400000.
    assert_eq!(
        parse_date_ms_with_offset("2026-03-01T10:00:00", 8 * 3_600_000),
        Some(1_772_330_400_000.0)
    );
}

#[test]
fn retention_keeps_out_of_range_offsets() {
    let record = |ended: &str| {
        let mut object = JsObject::new();
        object.insert("endedAt".into(), JsValue::String(ended.into()));
        JsValue::Object(object)
    };
    let now = 1_772_359_200_000.0;
    for ended in [
        "2000-01-01T00:00:00+99:99",
        "2000-01-01T00:00:00+01:99",
        "2000-01-01T00:00:00+24:00",
    ] {
        assert!(
            is_retained_record(Some(1_000.0), now, &record(ended)),
            "{ended} must be kept (Date.parse is NaN)"
        );
    }
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
