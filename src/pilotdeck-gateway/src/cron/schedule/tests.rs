use super::*;

fn ms(value: &str) -> i64 {
    parse_date(value).unwrap().unwrap()
}

#[test]
fn original_day_syntax_cases() {
    for expression in ["0 9 1 * 1", "0 9 1 * 0-6", "0 9 1-31 * 1", "0 9 1,* * 1"] {
        assert!(cron_day_fields_use_or(expression), "{expression}");
    }
    for expression in [
        "0 9 * * 1",
        "0 9 1 * *",
        "0 9 */2 * 1",
        "0 9 1 * */2",
        "0 9 *,1 * 1",
    ] {
        assert!(!cron_day_fields_use_or(expression), "{expression}");
    }
}

#[test]
fn original_sixteen_schedule_oracles() {
    // Exact expected instants from tests/cron/cron-schedule.test.ts:15-112.
    for (expression, after, timezone, expected) in [
        (
            "0 9 1 * 1",
            "2026-06-02T00:00:00Z",
            "UTC",
            "2026-06-08T09:00:00Z",
        ),
        (
            "0 9 1 * 1",
            "2026-06-29T09:00:00Z",
            "UTC",
            "2026-07-01T09:00:00Z",
        ),
        (
            "15 10 1 7 1",
            "2026-06-02T00:00:00Z",
            "UTC",
            "2026-07-01T10:15:00Z",
        ),
        (
            "0 9 1 * 7",
            "2026-06-02T00:00:00Z",
            "UTC",
            "2026-06-07T09:00:00Z",
        ),
        (
            "0 9 1,15 * 1-5",
            "2026-07-31T09:00:00Z",
            "UTC",
            "2026-08-01T09:00:00Z",
        ),
        (
            "0 9 * * 1",
            "2026-06-02T00:00:00Z",
            "UTC",
            "2026-06-08T09:00:00Z",
        ),
        (
            "0 9 1 * *",
            "2026-06-02T00:00:00Z",
            "UTC",
            "2026-07-01T09:00:00Z",
        ),
        (
            "0 9 * * *",
            "2026-06-02T00:00:00Z",
            "UTC",
            "2026-06-02T09:00:00Z",
        ),
        (
            "0 9 */2 * 1",
            "2026-06-02T00:00:00Z",
            "UTC",
            "2026-06-15T09:00:00Z",
        ),
        (
            "0 9 1 * */2",
            "2026-06-02T00:00:00Z",
            "UTC",
            "2026-08-01T09:00:00Z",
        ),
        (
            "0 9 1 * 0-6",
            "2026-06-02T00:00:00Z",
            "UTC",
            "2026-06-02T09:00:00Z",
        ),
        (
            "0 9 1-31 * 1",
            "2026-06-02T00:00:00Z",
            "UTC",
            "2026-06-02T09:00:00Z",
        ),
        (
            "0 9 29 2 *",
            "2026-01-30T00:00:00Z",
            "UTC",
            "2028-02-29T09:00:00Z",
        ),
        (
            "0 9 29 2 0-6",
            "2026-01-30T00:00:00Z",
            "UTC",
            "2026-02-01T09:00:00Z",
        ),
        (
            "0 9 29 2 1",
            "2026-01-30T00:00:00Z",
            "UTC",
            "2026-02-02T09:00:00Z",
        ),
        (
            "0 9 1 * 1",
            "2026-06-07T20:00:00Z",
            "Asia/Shanghai",
            "2026-06-08T01:00:00Z",
        ),
    ] {
        assert_eq!(
            compute_next_cron_run_at(expression, ms(after), timezone).unwrap(),
            Some(ms(expected)),
            "{expression} after {after}"
        );
    }
}

#[test]
fn original_shanghai_leap_day_oracle() {
    assert_eq!(
        compute_next_cron_run_at("30 8 29 2 *", ms("2026-01-01T00:00:00Z"), "Asia/Shanghai")
            .unwrap(),
        Some(ms("2028-02-29T00:30:00Z"))
    );
}

#[test]
fn minute_boundary_delay_units_and_time_clip() {
    let after = ms("2026-01-01T00:00:00Z");
    assert_eq!(
        compute_next_cron_run_at("* * * * *", after, "UTC").unwrap(),
        Some(after + 60_000)
    );
    assert_eq!(
        compute_next_cron_run_at("* * * * *", after + 59_999, "UTC").unwrap(),
        Some(after + 60_000)
    );
    for (unit, delta) in [
        ("second", 1000),
        ("minute", 60_000),
        ("hour", 3_600_000),
        ("day", 86_400_000),
    ] {
        assert_eq!(
            compute_next_run_at(
                &json!({"type":"delay","amount":0.5,"unit":unit}),
                after,
                "UTC"
            )
            .unwrap(),
            Some(after + delta / 2)
        );
    }
    for amount in [-1.0, 0.0] {
        assert_eq!(
            compute_next_run_at(
                &json!({"type":"delay","amount":amount,"unit":"minute"}),
                after,
                "UTC"
            )
            .unwrap(),
            None
        );
    }
    assert!(
        compute_next_run_at(
            &json!({"type":"delay","amount":1e100,"unit":"day"}),
            after,
            "UTC"
        )
        .is_err()
    );
    assert_eq!(
        compute_next_run_at(&json!({"type":"once","runAt":"not a date"}), after, "UTC").unwrap(),
        None
    );
}

#[test]
fn invalid_fields_and_baseline_parse_int_prefix_semantics() {
    for expression in [
        "* * * *",
        "* * * * * *",
        "60 * * * *",
        "* 24 * * *",
        "* * 0 * *",
        "* * * 13 *",
        "* * * * 8",
        "*/0 * * * *",
        "1,,2 * * * *",
        "2-1 * * * *",
        "*/2/3 * * * *",
        "@daily",
    ] {
        assert!(Parsed::parse(expression).is_none(), "{expression}");
    }
    assert_eq!(field("1junk", 0, 59), Some(BTreeSet::from([1])));
    assert_eq!(field("1-2-3", 0, 59), Some(BTreeSet::from([1, 2])));
    assert_eq!(field("5/2", 0, 59), Some(BTreeSet::from([5])));
    assert_eq!(field("*/20x", 0, 59), Some(BTreeSet::from([0, 20, 40])));
}

#[test]
fn timezone_validation_precedence_and_dst() {
    for value in [
        "UTC",
        "utc",
        "Asia/Shanghai",
        "america/new_york",
        "+05:30",
        "-0800",
    ] {
        assert!(valid_timezone(value), "{value}");
    }
    for value in ["", "Not/AZone", "+24:00", "../UTC", "+éxx"] {
        assert!(!valid_timezone(value), "{value}");
    }
    assert_eq!(
        resolve_timezone(Some("Not/AZone"), Some("Asia/Shanghai"), Some("UTC")),
        "Asia/Shanghai"
    );
    let normalized = normalize_schedule(
        &json!({"type":"cron","expression":"0 9 * * *","timezone":"Asia/Shanghai"}),
        Some(&json!("UTC")),
        "UTC",
        0,
        true,
    )
    .unwrap();
    assert_eq!(normalized["timezone"], "Asia/Shanghai");
    assert!(
        normalize_schedule(
            &json!({"type":"cron","expression":"0 9 * * *","timezone":"Not/AZone"}),
            None,
            "UTC",
            0,
            true
        )
        .is_err()
    );
    assert!(
        normalize_schedule(
            &json!({"type":"delay","amount":1,"unit":"minute"}),
            None,
            "UTC",
            0,
            false
        )
        .is_err()
    );
    assert_eq!(
        compute_next_cron_run_at("30 2 * * *", ms("2026-03-08T06:00:00Z"), "America/New_York")
            .unwrap(),
        Some(ms("2026-03-09T06:30:00Z"))
    );
    assert_eq!(
        compute_next_cron_run_at("30 1 * * *", ms("2026-11-01T05:30:00Z"), "America/New_York")
            .unwrap(),
        Some(ms("2026-11-01T06:30:00Z"))
    );
}
