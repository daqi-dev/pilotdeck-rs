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
    // Zone-less timestamps are intentionally treated as malformed (kept).
    assert_eq!(parse_date_ms("2026-03-01T10:00:00"), None);
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
