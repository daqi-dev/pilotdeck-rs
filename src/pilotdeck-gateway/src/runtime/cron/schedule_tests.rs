//! Oracle replay for the #1324 cron schedule port. Every cell of
//! `tests/fixtures/cron-schedule/oracle.json` (nextRuns / dayOr /
//! validTimezones / resolveTimezones / delayMs) is replayed against the Rust
//! port by one named test, comparing each recorded TS result exactly —
//! including the Invalid Date the delay path returns beyond TimeClip and the
//! `RangeError: Invalid time value` the minute scan throws when the aligned
//! candidate leaves the Date range. The generator pins nothing: every input
//! is an explicit epoch and every `runAt` string is timezone-independent, so
//! this replay is host-timezone independent (the one deliberate divergence —
//! an ICU-valid legacy alias zone whose host TZif tree lacks the link, e.g.
//! `US/Pacific` on a split tzdata install — never reaches `computeNextRunAt`
//! cells; its validity and resolution cells only exercise the recorded-list
//! acceptance, which is host independent).
use std::sync::LazyLock;

use pilotdeck_protocol::{JsString, JsValue};

use super::{
    ComputedRunAt, CronCreateSchedule, DelayUnit, compute_next_run_at, cron_day_fields_use_or,
    delay_to_milliseconds, is_valid_cron_timezone, resolve_cron_timezone,
};

fn oracle() -> &'static JsValue {
    static ORACLE: LazyLock<JsValue> = LazyLock::new(|| {
        crate::test_baseline::oracle(include_str!(
            "../../../tests/fixtures/cron-schedule/oracle.json"
        ))
    });
    &ORACLE
}

fn cells(section: &str) -> &'static [JsValue] {
    match oracle().get(section) {
        Some(JsValue::Array(cells)) => cells,
        _ => panic!("#1324 oracle section {section} must be an array"),
    }
}

fn cell(section: &str, name: &str) -> &'static JsValue {
    cells(section)
        .iter()
        .find(|entry| text(entry, "name") == name)
        .unwrap_or_else(|| panic!("#1324 oracle section {section} has no cell {name}"))
}

fn text(value: &JsValue, name: &str) -> String {
    value
        .get(name)
        .and_then(JsValue::as_string)
        .unwrap_or_else(|| panic!("#1324 oracle field {name} must be a string: {value:?}"))
        .to_utf8()
        .unwrap_or_else(|error| panic!("#1324 oracle field {name} must be UTF-8: {error:?}"))
}

fn boolean(value: &JsValue, name: &str) -> bool {
    value
        .get(name)
        .and_then(JsValue::as_bool)
        .unwrap_or_else(|| panic!("#1324 oracle field {name} must be a boolean: {value:?}"))
}

fn number(value: &JsValue, name: &str) -> f64 {
    value
        .get(name)
        .and_then(JsValue::as_number)
        .unwrap_or_else(|| panic!("#1324 oracle field {name} must be a number: {value:?}"))
}

/// The recorded `afterMs`: a number, or `null` for the NaN of an Invalid
/// `after` Date.
fn number_or_nan(value: &JsValue, name: &str) -> f64 {
    match value.get(name) {
        Some(JsValue::Number(epoch)) => *epoch,
        Some(JsValue::Null) | None => f64::NAN,
        other => panic!("#1324 oracle field {name} must be a number or null: {other:?}"),
    }
}

/// A `null` = `undefined` optional string field (`resolveCronTimezone`
/// arguments and the cron schedule's absent `timezone`).
fn optional_text(value: &JsValue, name: &str) -> Option<String> {
    match value.get(name) {
        Some(JsValue::String(text)) => {
            Some(text.to_utf8().unwrap_or_else(|error| {
                panic!("#1324 oracle field {name} must be UTF-8: {error:?}")
            }))
        }
        Some(JsValue::Null) | None => None,
        other => panic!("#1324 oracle field {name} must be a string or null: {other:?}"),
    }
}

fn unit_of(unit: &str) -> DelayUnit {
    match unit {
        "second" => DelayUnit::Second,
        "minute" => DelayUnit::Minute,
        "hour" => DelayUnit::Hour,
        "day" => DelayUnit::Day,
        other => panic!("#1324 unknown delay unit {other}"),
    }
}

fn schedule_of(entry: &JsValue) -> CronCreateSchedule {
    let name = text(entry, "name");
    let record = entry
        .get("schedule")
        .unwrap_or_else(|| panic!("#1324 {name}: oracle cell has no schedule record"));
    let js_string = |field: &str| {
        record
            .get(field)
            .and_then(JsValue::as_string)
            .unwrap_or_else(|| panic!("#1324 {name}: schedule.{field} must be a string"))
            .clone()
    };
    match text(record, "type").as_str() {
        "once" => CronCreateSchedule::Once {
            run_at: js_string("runAt"),
        },
        "delay" => CronCreateSchedule::Delay {
            amount: number(record, "amount"),
            unit: unit_of(&text(record, "unit")),
        },
        "cron" => CronCreateSchedule::Cron {
            expression: js_string("expression"),
            // Kept as raw UTF-16: the surrogate-timezone cell's value is
            // deliberately not valid UTF-8 (TS passes it straight to Intl).
            timezone: match record.get("timezone") {
                Some(JsValue::String(text)) => Some(text.clone()),
                Some(JsValue::Null) | None => None,
                other => {
                    panic!("#1324 {name}: schedule.timezone must be a string or null: {other:?}")
                }
            },
        },
        other => panic!("#1324 {name}: unknown schedule type {other}"),
    }
}

fn replay_next_run_cell(entry: &JsValue) {
    let name = text(entry, "name");
    let schedule = schedule_of(entry);
    let actual = compute_next_run_at(
        &schedule,
        number_or_nan(entry, "afterMs"),
        &text(entry, "fallback"),
    );
    match entry
        .get("error")
        .filter(|value| !matches!(value, JsValue::Null))
    {
        Some(message) => {
            assert_eq!(
                message
                    .as_string()
                    .and_then(|s| s.to_utf8().ok())
                    .as_deref(),
                Some("RangeError: Invalid time value"),
                "#1324 {name}: oracle error cell must carry the scan's RangeError"
            );
            let error = match actual {
                Err(error) => error,
                Ok(result) => {
                    panic!("#1324 {name}: expected the scan's RangeError, got {result:?}")
                }
            };
            assert_eq!(
                error.to_string(),
                "Invalid time value",
                "#1324 {name}: RangeError message differs from Node"
            );
        }
        None => {
            let actual = actual.unwrap_or_else(|error| panic!("#1324 {name}: scan threw {error}"));
            let expected = match entry.get("result") {
                Some(JsValue::Null) | None => ComputedRunAt::Undefined,
                Some(JsValue::String(text))
                    if text.to_utf8().ok().as_deref() == Some("invalid") =>
                {
                    ComputedRunAt::Invalid
                }
                Some(JsValue::Number(epoch)) => ComputedRunAt::Valid(*epoch),
                other => panic!(
                    "#1324 {name}: oracle result must be null, \"invalid\", or a number: {other:?}"
                ),
            };
            assert_eq!(
                actual, expected,
                "#1324 {name}: computed next run differs from the TS oracle"
            );
        }
    }
}

fn replay_day_or_cell(entry: &JsValue) {
    let name = text(entry, "name");
    let expression = entry
        .get("expression")
        .and_then(JsValue::as_string)
        .unwrap_or_else(|| panic!("#1324 dayOr {name}: expression must be a string"))
        .clone();
    assert_eq!(
        cron_day_fields_use_or(&expression),
        boolean(entry, "expect"),
        "#1324 dayOr {name}: day-field rule differs from the TS oracle"
    );
}

fn replay_timezone_cell(entry: &JsValue) {
    let name = text(entry, "name");
    let timezone = text(entry, "timezone");
    assert_eq!(
        is_valid_cron_timezone(&timezone),
        boolean(entry, "expect"),
        "#1324 timezone {name}: acceptance differs from the TS oracle"
    );
}

fn replay_resolve_cell(entry: &JsValue) {
    let name = text(entry, "name");
    let actual = resolve_cron_timezone(
        optional_text(entry, "scheduleTimezone").as_deref(),
        optional_text(entry, "taskTimezone").as_deref(),
        optional_text(entry, "configTimezone").as_deref(),
    );
    assert_eq!(
        actual,
        text(entry, "expect"),
        "#1324 resolve {name}: resolved timezone differs from the TS oracle"
    );
}

fn replay_delay_cell(entry: &JsValue) {
    let name = text(entry, "name");
    let expected = match entry.get("expect") {
        Some(JsValue::Number(milliseconds)) => Some(*milliseconds),
        Some(JsValue::Null) | None => None,
        other => panic!("#1324 delay {name}: expect must be a number or null: {other:?}"),
    };
    assert_eq!(
        delay_to_milliseconds(number(entry, "amount"), unit_of(&text(entry, "unit"))),
        expected,
        "#1324 delay {name}: delay in milliseconds differs from the TS oracle"
    );
}

/// The matrix shape is frozen: `generate.mts` asserts the same five counts
/// before publishing `oracle.json`, and one named test below replays every
/// cell, so adding or removing a cell must update both sides.
#[test]
fn oracle_matrix_counts_match_generator() {
    for (section, count) in [
        ("nextRuns", 126),
        ("dayOr", 13),
        ("validTimezones", 39),
        ("resolveTimezones", 12),
        ("delayMs", 12),
    ] {
        assert_eq!(
            cells(section).len(),
            count,
            "#1324 oracle section {section} changed cell count"
        );
    }
}

/// `CRON_SCHEDULE_COMPUTATION_VERSION` (CronSchedule.ts:4), recorded by the
/// generator next to the baseline source hashes so the pin moves with the
/// baseline instead of comparing a literal to itself.
#[test]
fn computation_version_matches_baseline_constant() {
    let recorded = match oracle().get("computationVersion") {
        Some(JsValue::Number(version)) => *version,
        other => panic!("#1324 oracle computationVersion must be a number: {other:?}"),
    };
    assert_eq!(
        f64::from(super::CRON_SCHEDULE_COMPUTATION_VERSION),
        recorded,
        "#1324 computation version differs from the TS oracle"
    );
}

/// #1324: a local-wall `once` `runAt` whose process-TZ resolution is
/// unavailable on this host fails closed to `Undefined` — TS never fails
/// there (Node's date parser has ICU-local built in). Registered in
/// `specs/gateway-parity/diffs/A8.3-CRON-ONCE-LOCALWALL-001.md`.
#[test]
fn once_local_wall_unresolvable_fails_closed() {
    let actual = super::schedule::once_computed_run_at(&"2026-03-05T09:30:00".into(), &|_| {
        Err(JsString::from("NODE22_DATE_LOCAL_SYSTEM_TZIF_UNSUPPORTED"))
    });
    assert_eq!(
        actual,
        ComputedRunAt::Undefined,
        "#1324 once local-wall unresolved: must fail closed to undefined, not guess a date"
    );
}

/// #1324: an ICU-valid zone whose resolver construction fails (a legacy
/// alias the host zoneinfo tree lacks) fails closed to `Undefined`, never
/// silently falls back to UTC.
#[test]
fn zone_resolver_failure_fails_closed() {
    let expression: JsString = "0 9 * * *".into();
    let Some(parsed) = super::schedule::parse_cron_expression(&expression) else {
        panic!("#1324 zone resolver failure: the probe expression must parse")
    };
    let actual = super::schedule::compute_next_resolved(
        &parsed,
        0.0,
        Err(JsString::from("Invalid time zone specified")),
    );
    assert_eq!(
        actual,
        Ok(None),
        "#1324 zone resolver failure: must fail closed to undefined, not fall back to UTC"
    );
}

/// TimeClip normalizes `-0` to `+0` after `ToIntegerOrInfinity`, verified on
/// Node v22.22.0 (`new Date(-0.5).getTime()` is `+0`, `Object.is(…, -0)`
/// false); the oracle's JSON cannot distinguish the two zero bits, so this
/// pins the sign bit directly.
#[test]
fn delay_negative_zero_is_positive_zero() {
    let actual = compute_next_run_at(
        &CronCreateSchedule::Delay {
            amount: 0.0005,
            unit: DelayUnit::Second,
        },
        -1.0,
        "UTC",
    )
    .unwrap_or_else(|error| panic!("#1324 delay negative zero: unexpected scan error {error}"));
    let ComputedRunAt::Valid(epoch) = actual else {
        panic!("#1324 delay negative zero: expected a valid date, got {actual:?}")
    };
    assert_eq!(
        epoch.to_bits(),
        0,
        "#1324 delay negative zero: TimeClip must normalize -0 to +0"
    );
}

macro_rules! next_run_tests {
    ($($name:ident => $cell:literal),* $(,)?) => { $(
        #[test]
        fn $name() {
            replay_next_run_cell(cell("nextRuns", $cell));
        }
    )* };
}

macro_rules! day_or_tests {
    ($($name:ident => $cell:literal),* $(,)?) => { $(
        #[test]
        fn $name() {
            replay_day_or_cell(cell("dayOr", $cell));
        }
    )* };
}

macro_rules! timezone_tests {
    ($($name:ident => $cell:literal),* $(,)?) => { $(
        #[test]
        fn $name() {
            replay_timezone_cell(cell("validTimezones", $cell));
        }
    )* };
}

macro_rules! resolve_tests {
    ($($name:ident => $cell:literal),* $(,)?) => { $(
        #[test]
        fn $name() {
            replay_resolve_cell(cell("resolveTimezones", $cell));
        }
    )* };
}

macro_rules! delay_tests {
    ($($name:ident => $cell:literal),* $(,)?) => { $(
        #[test]
        fn $name() {
            replay_delay_cell(cell("delayMs", $cell));
        }
    )* };
}

next_run_tests! {
    once_iso_z => "once/iso-z",
    once_iso_z_no_ms => "once/iso-z-no-ms",
    once_time_clip_max => "once/time-clip-max",
    once_beyond_time_clip => "once/beyond-time-clip",
    once_time_clip_min => "once/time-clip-min",
    once_one_ms_below_max => "once/one-ms-below-max",
    once_fractional_ms_truncates => "once/fractional-ms-truncates",
    once_explicit_plus_8_offset => "once/explicit-plus-8-offset",
    once_explicit_zero_offset => "once/explicit-zero-offset",
    once_date_only => "once/date-only",
    once_year_month_only => "once/year-month-only",
    once_feb_29_nonleap_rolls => "once/feb-29-nonleap-rolls",
    once_apr_31_rolls => "once/apr-31-rolls",
    once_day_32_rejected => "once/day-32-rejected",
    once_day_0_rejected => "once/day-0-rejected",
    once_month_13_rejected => "once/month-13-rejected",
    once_month_0_rejected => "once/month-0-rejected",
    once_hour_24_rolls_to_next_day => "once/hour-24-rolls-to-next-day",
    once_minute_60_rejected => "once/minute-60-rejected",
    once_second_60_rejected => "once/second-60-rejected",
    once_extended_negative_year => "once/extended-negative-year",
    once_year_zero => "once/year-zero",
    once_not_a_date => "once/not-a-date",
    once_empty_string => "once/empty-string",
    once_padded_valid => "once/padded-valid",
    once_lone_surrogate => "once/lone-surrogate",
    delay_one_second_from_epoch => "delay/one-second-from-epoch",
    delay_half_minute => "delay/half-minute",
    delay_negative_after => "delay/negative-after",
    delay_one_day_overflows_clip => "delay/one-day-overflows-clip",
    delay_one_second_at_clip => "delay/one-second-at-clip",
    delay_zero_amount => "delay/zero-amount",
    delay_negative_amount => "delay/negative-amount",
    delay_quarter_day => "delay/quarter-day",
    delay_huge_amount_overflows => "delay/huge-amount-overflows",
    delay_sub_millisecond_truncates => "delay/sub-millisecond-truncates",
    delay_nan_after => "delay/nan-after",
    delay_nan_after_zero_amount => "delay/nan-after-zero-amount",
    delay_two_and_a_half_days => "delay/two-and-a-half-days",
    delay_pre_epoch => "delay/pre-epoch",
    delay_sub_ms_negative_after => "delay/sub-ms-negative-after",
    delay_one_millisecond => "delay/one-millisecond",
    cron_daily_9am_before => "cron/daily-9am-before",
    cron_daily_9am_after => "cron/daily-9am-after",
    cron_daily_9am_exact => "cron/daily-9am-exact",
    cron_every_15_minutes => "cron/every-15-minutes",
    cron_every_15_exact => "cron/every-15-exact",
    cron_minute_list_hour_step => "cron/minute-list-hour-step",
    cron_dow_7_is_sunday => "cron/dow-7-is-sunday",
    cron_dow_0_and_7_normalize => "cron/dow-0-and-7-normalize",
    cron_and_weekly_friday => "cron/and-weekly-friday",
    cron_or_13th_first => "cron/or-13th-first",
    cron_or_next_friday => "cron/or-next-friday",
    cron_or_dom_or_monday => "cron/or-dom-or-monday",
    cron_dow_range => "cron/dow-range",
    cron_hour_range_step => "cron/hour-range-step",
    cron_last_day_of_month => "cron/last-day-of-month",
    cron_month_list => "cron/month-list",
    cron_dom_29_nonfeb => "cron/dom-29-nonfeb",
    cron_spring_forward_gap_skips => "cron/spring-forward-gap-skips",
    cron_spring_forward_hour_skips => "cron/spring-forward-hour-skips",
    cron_fall_back_first_occurrence => "cron/fall-back-first-occurrence",
    cron_fall_back_second_occurrence => "cron/fall-back-second-occurrence",
    cron_berlin_spring_forward => "cron/berlin-spring-forward",
    cron_berlin_fall_back_first => "cron/berlin-fall-back-first",
    cron_berlin_fall_back_second => "cron/berlin-fall-back-second",
    cron_lord_howe_dst_end => "cron/lord-howe-dst-end",
    cron_chatham_quarter_offset => "cron/chatham-quarter-offset",
    cron_sao_paulo_no_dst => "cron/sao-paulo-no-dst",
    cron_kiritimati_skipped_day => "cron/kiritimati-skipped-day",
    cron_ny_pre_1970 => "cron/ny-pre-1970",
    cron_gap_minute_fixed_offset => "cron/gap-minute-fixed-offset",
    cron_ist_9am_same_day => "cron/ist-9am-same-day",
    cron_ist_9am_next_day => "cron/ist-9am-next-day",
    cron_offset_identifier => "cron/offset-identifier",
    cron_offset_hour_only => "cron/offset-hour-only",
    cron_etc_gmt_plus_5 => "cron/etc-gmt-plus-5",
    cron_lowercase_zone => "cron/lowercase-zone",
    cron_fallback_timezone_used => "cron/fallback-timezone-used",
    cron_schedule_timezone_wins => "cron/schedule-timezone-wins",
    cron_empty_explicit_timezone => "cron/empty-explicit-timezone",
    cron_surrogate_timezone => "cron/surrogate-timezone",
    cron_leap_day_2028 => "cron/leap-day-2028",
    cron_leap_day_late_window => "cron/leap-day-late-window",
    cron_leap_day_ny_midnight => "cron/leap-day-ny-midnight",
    cron_leap_day_kiritimati => "cron/leap-day-kiritimati",
    cron_leap_day_dow_breaks_shortcut => "cron/leap-day-dow-breaks-shortcut",
    cron_leap_day_after_year_0050 => "cron/leap-day-after-year-0050",
    cron_leap_day_after_year_0000 => "cron/leap-day-after-year-0000",
    cron_leap_day_after_year_1999 => "cron/leap-day-after-year-1999",
    cron_leap_day_nan_after => "cron/leap-day-nan-after",
    cron_nan_after_throws => "cron/nan-after-throws",
    cron_after_clip_throws => "cron/after-clip-throws",
    cron_scan_past_clip_throws => "cron/scan-past-clip-throws",
    cron_never_matches => "cron/never-matches",
    cron_negative_epoch_alignment => "cron/negative-epoch-alignment",
    cron_pre_epoch_exact => "cron/pre-epoch-exact",
    cron_whitespace_u2028 => "cron/whitespace-u2028",
    cron_tabs_as_separators => "cron/tabs-as-separators",
    cron_trimmed_expression => "cron/trimmed-expression",
    cron_nbsp_inside_field => "cron/nbsp-inside-field",
    cron_surrogate_expression_invalid => "cron/surrogate-expression-invalid",
    cron_minute_out_of_range => "cron/minute-out-of-range",
    cron_hour_out_of_range => "cron/hour-out-of-range",
    cron_dom_out_of_range => "cron/dom-out-of-range",
    cron_month_out_of_range => "cron/month-out-of-range",
    cron_dow_out_of_range => "cron/dow-out-of-range",
    cron_zero_step => "cron/zero-step",
    cron_infinite_step_rejected => "cron/infinite-step-rejected",
    cron_infinite_step_dow_rejected => "cron/infinite-step-dow-rejected",
    cron_huge_finite_step => "cron/huge-finite-step",
    cron_dot_leading_step_rejected => "cron/dot-leading-step-rejected",
    cron_too_many_fields => "cron/too-many-fields",
    cron_too_few_fields => "cron/too-few-fields",
    cron_empty_expression => "cron/empty-expression",
    cron_whitespace_only_expression => "cron/whitespace-only-expression",
    cron_not_cron => "cron/not-cron",
    cron_range_extra_piece_dropped => "cron/range-extra-piece-dropped",
    cron_parse_int_junk_ignored => "cron/parse-int-junk-ignored",
    cron_negative_value_rejected => "cron/negative-value-rejected",
    cron_dot_in_month_field => "cron/dot-in-month-field",
    cron_decimal_step_parses => "cron/decimal-step-parses",
    cron_trailing_comma_rejected => "cron/trailing-comma-rejected",
    cron_three_slash_parts_rejected => "cron/three-slash-parts-rejected",
    cron_leading_plus_allowed => "cron/leading-plus-allowed",
    cron_leading_dash_minute_rejected => "cron/leading-dash-minute-rejected",
}

day_or_tests! {
    day_or_star_both => "star-both",
    day_or_dom_and_dow_lists => "dom-and-dow-lists",
    day_or_dom_star_prefixed_step => "dom-star-prefixed-step",
    day_or_dow_star_prefixed_step => "dow-star-prefixed-step",
    day_or_dom_star_only => "dom-star-only",
    day_or_dow_range => "dow-range",
    day_or_leap_day => "leap-day",
    day_or_leap_day_with_dow => "leap-day-with-dow",
    day_or_dow_7_only => "dow-7-only",
    day_or_range_in_dom => "range-in-dom",
    day_or_unparsable => "unparsable",
    day_or_empty => "empty",
    day_or_too_many_fields => "too-many-fields",
}

timezone_tests! {
    tz_00_utc => "00-UTC",
    tz_01_utc => "01-utc",
    tz_02_gmt => "02-GMT",
    tz_03_etc_utc => "03-Etc-UTC",
    tz_04_etc_gmt_5 => "04-Etc-GMT-5",
    tz_05_etc_gmt_14 => "05-Etc-GMT-14",
    tz_06_etc_gmt_99 => "06-Etc-GMT-99",
    tz_07_america_new_york => "07-America-New-York",
    tz_08_america_new_york => "08-America-new-york",
    tz_09_america_new_york => "09-america-new-york",
    tz_10_america_new_york => "10-America-New-York",
    tz_11_us_pacific => "11-US-Pacific",
    tz_12_est => "12-EST",
    tz_13_est => "13-est",
    tz_14_asia_calcutta => "14-Asia-Calcutta",
    tz_15_asia_kolkata => "15-Asia-Kolkata",
    tz_16_asia_shanghai => "16-Asia-Shanghai",
    tz_17_asia_tokyo => "17-Asia-Tokyo",
    tz_18_europe_berlin => "18-Europe-Berlin",
    tz_19_australia_lord_howe => "19-Australia-Lord-Howe",
    tz_20_pacific_chatham => "20-Pacific-Chatham",
    tz_21_pacific_kiritimati => "21-Pacific-Kiritimati",
    tz_22_america_sao_paulo => "22-America-Sao-Paulo",
    tz_23_america_new_york => "23-america-new-york",
    tz_24_05_30 => "24-05-30",
    tz_25_0530 => "25-0530",
    tz_26_00_00 => "26-00-00",
    tz_27_05_30_00 => "27-05-30-00",
    tz_28_5_30 => "28-5-30",
    tz_29_05_30 => "29-05-30",
    tz_30_99_00 => "30-99-00",
    tz_31_24_00 => "31-24-00",
    tz_32_00_60 => "32-00-60",
    tz_33_blank => "33-blank",
    tz_34_blank => "34-blank",
    tz_35_not_azone => "35-Not-AZone",
    tz_36_z => "36-Z",
    tz_37_plus_05 => "37-05",
    tz_38_plus_00 => "38-00",
}

resolve_tests! {
    resolve_all_undefined => "all-undefined",
    resolve_schedule_valid => "schedule-valid",
    resolve_schedule_empty_falls_to_task => "schedule-empty-falls-to-task",
    resolve_schedule_invalid_falls_to_task => "schedule-invalid-falls-to-task",
    resolve_task_invalid_falls_to_config => "task-invalid-falls-to-config",
    resolve_all_invalid => "all-invalid",
    resolve_all_empty => "all-empty",
    resolve_alias_zone => "alias-zone",
    resolve_lowercase_zone => "lowercase-zone",
    resolve_offset_identifier => "offset-identifier",
    resolve_task_only => "task-only",
    resolve_config_only => "config-only",
}

delay_tests! {
    delay_ms_one_second => "one-second",
    delay_ms_half_minute => "half-minute",
    delay_ms_one_hour => "one-hour",
    delay_ms_one_day => "one-day",
    delay_ms_zero => "zero",
    delay_ms_negative => "negative",
    delay_ms_fractional_second => "fractional-second",
    delay_ms_huge_but_finite => "huge-but-finite",
    delay_ms_sub_millisecond => "sub-millisecond",
    delay_ms_tiny_day => "tiny-day",
    delay_ms_two_and_a_half_days => "two-and-a-half-days",
    delay_ms_five_minutes => "five-minutes",
}
