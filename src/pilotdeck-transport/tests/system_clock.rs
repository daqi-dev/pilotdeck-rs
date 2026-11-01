//! Source-derived parser tests plus frozen actual Node22 date observations.
//! Fixed local resolver fixtures declare Asia/Shanghai +08:00 for these observed
//! dates; they do not prove arbitrary timezone/ICU historical equivalence.
use pilotdeck_protocol::JsString;
use pilotdeck_protocol::{JsValue, parse_json};
use pilotdeck_transport::server;
use server::ExecutionClock;
#[path = "../src/system_clock.rs"]
mod system_clock;
use system_clock::{
    SystemExecutionClock, format_epoch_iso, parse_node22_date_with_local as parse_with,
};
fn utc(input: &str) -> Option<f64> {
    parse_with(&input.into(), &Ok).unwrap()
}
fn shanghai(input: &JsString) -> Option<f64> {
    parse_with(input, &|wall| Ok(wall - 28_800_000)).unwrap()
}
fn parse(input: &str) -> Option<f64> {
    shanghai(&input.into())
}

#[test]
fn slim2_actual_node22_fixture_preserves_valid_legacy_and_invalid_dates() {
    let fixture = parse_json(include_str!("fixtures/node22-date-parse.json")).unwrap();
    assert_eq!(fixture.get("node"), Some(&JsValue::from("v22.22.0")));
    let Some(JsValue::Array(zones)) = fixture.get("zones") else {
        panic!("missing Node timezone observations");
    };
    let la = la_tzif();
    let mut checked = 0;
    for zone in zones {
        let Some(JsValue::String(tz)) = zone.get("tz") else {
            panic!("missing timezone");
        };
        let tz = tz.to_utf8().unwrap();
        let Some(JsValue::Array(cases)) = zone.get("cases") else {
            panic!("missing cases");
        };
        for case in cases {
            let Some(JsValue::String(input)) = case.get("input") else {
                panic!("missing UTF16 input");
            };
            let expected = match case.get("epoch") {
                Some(JsValue::Number(ms)) => Some(*ms),
                Some(JsValue::Null) => None,
                _ => panic!("missing Node result"),
            };
            // These supplied rules cover the fixture's local dates only;
            // they make no claim about arbitrary system/ICU tzdata parity.
            let actual = parse_with(input, &|wall| match tz.as_str() {
                "UTC" => Ok(wall),
                "Asia/Shanghai" => Ok(wall - 28_800_000),
                "America/Los_Angeles" => system_clock::resolve_tzif_local(wall, &la),
                _ => panic!("unexpected timezone"),
            })
            .unwrap();
            assert_eq!(actual, expected, "{tz}: {:?}", case.get("id"));
            if let Some(ms) = actual {
                assert_eq!(
                    case.get("iso"),
                    Some(&JsValue::from(format_epoch_iso(ms as i64).as_str())),
                    "{tz}: {:?}",
                    case.get("id")
                );
            }
            checked += 1;
        }
    }
    assert_eq!(checked, 117);
}

#[test]
fn frozen29_actual_node22_shanghai_observations() {
    let cases: &[(&str, &[u16], Option<f64>)] = &[
        ("date-00", &[], None),
        ("date-01", &[32], None),
        (
            "date-02",
            &[49, 57, 55, 48, 45, 48, 49, 45, 48, 49],
            Some(0.0),
        ),
        (
            "date-03",
            &[50, 48, 48, 48, 45, 48, 50, 45, 51, 48],
            Some(951868800000.0),
        ),
        (
            "date-04",
            &[50, 48, 50, 52, 45, 48, 50, 45, 51, 48],
            Some(1709251200000.0),
        ),
        (
            "date-05",
            &[50, 48, 50, 51, 45, 48, 50, 45, 50, 57],
            Some(1677628800000.0),
        ),
        ("date-06", &[50, 48, 50, 52, 45, 49, 51, 45, 48, 49], None),
        (
            "date-07",
            &[
                50, 48, 50, 52, 45, 48, 49, 45, 48, 49, 84, 48, 48, 58, 48, 48, 58, 48, 48, 90,
            ],
            Some(1704067200000.0),
        ),
        (
            "date-08",
            &[
                50, 48, 50, 52, 45, 48, 49, 45, 48, 49, 84, 48, 48, 58, 48, 48, 58, 48, 48, 43, 48,
                56, 58, 48, 48,
            ],
            Some(1704038400000.0),
        ),
        (
            "date-09",
            &[
                50, 48, 50, 52, 45, 48, 49, 45, 48, 49, 84, 48, 48, 58, 48, 48, 58, 48, 48,
            ],
            Some(1704038400000.0),
        ),
        (
            "date-10",
            &[
                84, 104, 117, 44, 32, 48, 49, 32, 74, 97, 110, 32, 49, 57, 55, 48, 32, 48, 48, 58,
                48, 48, 58, 48, 48, 32, 71, 77, 84,
            ],
            Some(0.0),
        ),
        (
            "date-11",
            &[
                74, 97, 110, 117, 97, 114, 121, 32, 49, 44, 32, 50, 48, 48, 48,
            ],
            Some(946656000000.0),
        ),
        (
            "date-12",
            &[
                50, 48, 50, 52, 45, 48, 49, 45, 48, 49, 84, 48, 48, 58, 48, 48, 58, 48, 48, 46, 49,
                50, 51, 52, 53, 54, 90,
            ],
            Some(1704067200123.0),
        ),
        (
            "date-13",
            &[
                50, 48, 50, 52, 45, 48, 49, 45, 48, 49, 84, 50, 52, 58, 48, 48, 58, 48, 48, 90,
            ],
            Some(1704153600000.0),
        ),
        (
            "date-14",
            &[
                50, 48, 50, 52, 45, 48, 49, 45, 48, 49, 84, 50, 52, 58, 48, 49, 58, 48, 48, 90,
            ],
            None,
        ),
        (
            "date-15",
            &[
                43, 48, 49, 48, 48, 48, 48, 45, 48, 49, 45, 48, 49, 84, 48, 48, 58, 48, 48, 58, 48,
                48, 46, 48, 48, 48, 90,
            ],
            Some(253402300800000.0),
        ),
        (
            "date-16",
            &[
                45, 48, 48, 48, 48, 48, 49, 45, 48, 49, 45, 48, 49, 84, 48, 48, 58, 48, 48, 58, 48,
                48, 46, 48, 48, 48, 90,
            ],
            Some(-62198755200000.0),
        ),
        ("date-17", &[48], Some(946656000000.0)),
        ("date-18", &[49], Some(978278400000.0)),
        ("date-19", &[49, 50], Some(1007136000000.0)),
        ("date-20", &[49, 51], None),
        ("date-21", &[57, 57], Some(915120000000.0)),
        ("date-22", &[50, 48, 50, 52], Some(1704067200000.0)),
        (
            "date-23",
            &[50, 48, 50, 52, 45, 49, 45, 50],
            Some(1704124800000.0),
        ),
        (
            "date-24",
            &[110, 111, 116, 32, 97, 32, 100, 97, 116, 101],
            None,
        ),
        ("date-25", &[55296], None),
        (
            "date-26",
            &[48, 49, 47, 48, 50, 47, 50, 48, 50, 52],
            Some(1704124800000.0),
        ),
        (
            "date-27",
            &[48, 50, 47, 51, 48, 47, 50, 48, 50, 52],
            Some(1709222400000.0),
        ),
        (
            "date-28",
            &[
                50, 48, 50, 52, 45, 48, 49, 45, 48, 49, 84, 48, 48, 58, 48, 48, 58, 48, 48, 45, 48,
                53, 58, 51, 48,
            ],
            Some(1704087000000.0),
        ),
    ];
    for (id, units, expected) in cases {
        assert_eq!(
            shanghai(&JsString::from_utf16(units.to_vec())),
            *expected,
            "{id}"
        );
    }
}

#[test]
fn iso_date_only_defaults_to_utc_but_date_time_defaults_local() {
    assert_eq!(parse("1970"), Some(0.0));
    assert_eq!(parse("1970-01"), Some(0.0));
    assert_eq!(parse("1970-01-01"), Some(0.0));
    assert_eq!(parse("1970-01-01T00:00"), Some(-28_800_000.0));
    assert_eq!(parse("1970-01-01T00:00Z"), Some(0.0));
    // The ISO scanner continues as legacy on trailing whitespace, so the same
    // date-only text with whitespace uses local timezone, not UTC.
    assert_eq!(parse("1970-01-01 "), Some(-28_800_000.0));
    assert_eq!(parse(" 1970-01-01"), Some(-28_800_000.0));
}
#[test]
fn iso_requires_fixed_width_and_irrecoverable_time_grammar() {
    for invalid in [
        "2024-01-01T0:00Z",
        "2024-01-01T00:0Z",
        "2024-01-01T00",
        "2024-01-01T00:00:0Z",
        "2024-01-01T00:00.1Z",
        "2024-01-01T00:00:00.Z",
        "2024-01-01T00:00:00Z ",
        "2024-01-01T00:00:00+8:00",
        "2024-01-01T00:00:00+24:00",
        "2024-01-01T00:00:00+08:60",
    ] {
        assert_eq!(utc(invalid), None, "{invalid}");
    }
    assert_eq!(utc("2024-1-2"), Some(1_704_153_600_000.0)); // valid legacy fallback
    assert_eq!(utc("2024-01-01t00:00z"), Some(1_704_067_200_000.0));
}
#[test]
fn twenty_four_hour_rolls_day_but_rejects_nonzero_tail() {
    assert_eq!(utc("2000-12-31T24:00Z"), Some(978_307_200_000.0));
    assert_eq!(utc("2000-12-31T24:00:00.000000Z"), Some(978_307_200_000.0));
    for invalid in [
        "2000-12-31T24:01Z",
        "2000-12-31T24:00:01Z",
        "2000-12-31T24:00:00.0000000001Z",
        "2000-12-31T25:00Z",
    ] {
        assert_eq!(utc(invalid), None, "{invalid}");
    }
}
#[test]
fn day_overflow_uses_gregorian_rollover_without_rejecting_february31() {
    assert_eq!(utc("2000-02-30"), Some(951_868_800_000.0));
    assert_eq!(utc("2000-02-31"), utc("2000-03-02"));
    assert_eq!(utc("1900-02-29"), utc("1900-03-01"));
    assert_eq!(utc("2000-02-29"), Some(951_782_400_000.0));
    assert_eq!(utc("2100-02-29"), utc("2100-03-01"));
    for invalid in ["2000-02-32", "2000-00-01", "2000-01-00", "2000-13-01"] {
        assert_eq!(utc(invalid), None, "{invalid}");
    }
}
#[test]
fn fraction_scanner_uses_v8_nine_significant_digit_value_and_length() {
    let base = 1_704_067_200_000.0;
    for (tail, ms) in [
        ("1", 100.0),
        ("12", 120.0),
        ("123", 123.0),
        ("123456789999", 123.0),
        ("0000000012345", 0.0),
        ("0001", 0.0),
        ("010", 10.0),
    ] {
        assert_eq!(
            utc(&format!("2024-01-01T00:00:00.{tail}Z")),
            Some(base + ms),
            "{tail}"
        );
    }
}
#[test]
fn extended_year_zero_sign_and_timeclip_limits_are_exact() {
    assert_eq!(utc("0000-01-01T00:00:00Z"), Some(-62_167_219_200_000.0));
    assert_eq!(utc("+000000-01-01T00:00Z"), utc("0000-01-01T00:00Z"));
    assert_eq!(utc("-000000-01-01T00:00Z"), None);
    assert_eq!(utc("-000001-01-01T00:00Z"), Some(-62_198_755_200_000.0));
    assert_eq!(utc("+010000-01-01T00:00Z"), Some(253_402_300_800_000.0));
    assert_eq!(
        utc("+275760-09-13T00:00:00.000Z"),
        Some(8_640_000_000_000_000.0)
    );
    assert_eq!(utc("+275760-09-13T00:00:00.001Z"), None);
    assert_eq!(
        utc("-271821-04-20T00:00:00.000Z"),
        Some(-8_640_000_000_000_000.0)
    );
    assert_eq!(utc("-271821-04-19T23:59:59.999Z"), None);
    assert_eq!(utc("+999999-01-01T00:00Z"), None);
    assert_eq!(utc("999999999"), None);
}
#[test]
fn iso_timezone_extensions_and_offsets_are_subtracted_in_seconds() {
    assert_eq!(utc("1970-01-01T00:00+0800"), Some(-28_800_000.0));
    assert_eq!(utc("1970-01-01T00:00-0530"), Some(19_800_000.0));
    assert_eq!(utc("1970-01-01T00:00+23:59"), Some(-86_340_000.0));
    assert_eq!(utc("1970-01-01T00:00-00:00"), Some(0.0));
}
#[test]
fn legacy_named_month_prefixes_case_and_comments_preserve_composer_order() {
    let expected = Some(0.0);
    for value in [
        "Thu, 01 Jan 1970 00:00:00 GMT",
        "January 1 1970 GMT",
        "1 JANuArY 1970 GMT",
        "1970 January 1 GMT",
        "JanGarbage 1 1970 GMT",
        "junk (nested (comment)) 1 Jan 1970 GMT",
        "1 Jan 1970 GMT (unterminated",
    ] {
        assert_eq!(utc(value), expected, "{value}");
    }
    assert_eq!(
        utc("Jan 1 GMT"),
        Some(978_307_200_000.0),
        "composer pads before assigning legacy missing year"
    );
    assert_eq!(
        utc("junk1 Jan 1970 GMT"),
        Some(0.0),
        "jun prefix is a recognized month, later Jan replaces it"
    );
    for invalid in [
        "garbage1 Jan 1970 GMT",
        "1 Jan 1970 garbage GMT",
        "1 Jan 1970 GMT garbage",
        "1 Jan 1970 GMT)",
    ] {
        assert_eq!(utc(invalid), None, "{invalid}");
    }
}
#[test]
fn legacy_two_digit_years_and_single_numeric_strings_follow_source_padding() {
    for (value, expected) in [
        ("0", 946_684_800_000.0),
        ("1", 978_307_200_000.0),
        ("12", 1_007_164_800_000.0),
        ("49", 2_493_072_000_000.0),
        ("50", -631_152_000_000.0),
        ("99", 915_148_800_000.0),
    ] {
        assert_eq!(utc(value), Some(expected), "{value}");
    }
    assert_eq!(utc("32"), utc("2032-01-01"));
    for invalid in ["13", "31"] {
        assert_eq!(utc(invalid), None, "{invalid}");
    }
    assert_eq!(utc("01/02/49 GMT"), utc("2049-01-02"));
    assert_eq!(utc("01/02/50 GMT"), utc("1950-01-02"));
    assert_eq!(utc("1970/1/1 GMT"), Some(0.0));
}
#[test]
fn legacy_ampm_utc_abbreviations_and_signed_offsets_match_v8_keywords() {
    assert_eq!(utc("1 Jan 1970 12:00 AM GMT"), Some(0.0));
    assert_eq!(utc("1 Jan 1970 12:00 PM GMT"), Some(43_200_000.0));
    assert_eq!(utc("1 Jan 1970 0:00 PM GMT"), Some(43_200_000.0));
    assert_eq!(utc("1 Jan 1970 13:00 PM GMT"), None);
    for (value, ms) in [
        ("EST", 18_000_000.0),
        ("EDT", 14_400_000.0),
        ("CST", 21_600_000.0),
        ("CDT", 18_000_000.0),
        ("MST", 25_200_000.0),
        ("MDT", 21_600_000.0),
        ("PST", 28_800_000.0),
        ("PDT", 25_200_000.0),
        ("UT", 0.0),
        ("UTC", 0.0),
        ("Z", 0.0),
    ] {
        assert_eq!(
            utc(&format!("1 Jan 1970 00:00 {value}")),
            Some(ms),
            "{value}"
        );
    }
    for (value, ms) in [
        ("GMT+8", -28_800_000.0),
        ("GMT-8", 28_800_000.0),
        ("GMT+0800", -28_800_000.0),
        ("GMT-530", 19_800_000.0),
        ("GMT+08:30", -30_600_000.0),
        ("GMT+24", -86_400_000.0),
        ("GMT+01:99", -3_600_000.0),
    ] {
        // GMT+01:99 cannot satisfy timezone minute; 99 occupies the third day
        // slot which named-month Y/D extraction ignores, leaving offset +01.
        assert_eq!(
            utc(&format!("1 Jan 1970 00:00 {value}")),
            Some(ms),
            "{value}"
        );
    }
}
#[test]
fn legacy_double_colon_final_time_separator_and_sign_rejection_are_preserved() {
    assert_eq!(utc("1 Jan 1970 1::02 GMT"), Some(3_602_000.0));
    assert_eq!(utc("1 Jan 1970 00:00:01.2 GMT"), Some(1_200.0));
    assert_eq!(
        utc("1 Jan 1970 00:00:01GMT"),
        None,
        "final time requires separator or single Z"
    );
    assert_eq!(utc("1 Jan 1970 00:00:01Z"), Some(1000.0));
    assert_eq!(utc("1 Jan 1970 00:00:00+0000"), Some(0.0));
    assert_eq!(
        utc("1 Jan 1970 00:00:00:00 GMT"),
        Some(0.0),
        "legacy parser accepts all four TimeComposer slots"
    );
    assert_eq!(
        utc("1 Jan 1970 00:00:00:00:00 GMT"),
        Some(0.0),
        "fifth final number becomes the unused third named-month day slot"
    );
    for invalid in [
        "1 Jan 1970+",
        "1 Jan 1970)",
        "1 Jan 1970 00:00 GMT+12345",
        "1 Jan 1970 00:00:00:00:00:00 GMT",
    ] {
        assert_eq!(utc(invalid), None, "{invalid}");
    }
}
#[test]
fn utf16_nul_terminates_input_and_unpaired_words_are_classified_without_lossy_utf8() {
    let mut units: Vec<u16> = "1970-01-01T00:00Z".encode_utf16().collect();
    units.extend([0, 0xd800, 0xffff, 65]);
    assert_eq!(shanghai(&JsString::from_utf16(units)), Some(0.0));
    assert_eq!(shanghai(&JsString::from_utf16(vec![0xd800])), None);
    let units: Vec<u16> = "\u{feff}1970-01-01".encode_utf16().collect();
    assert_eq!(shanghai(&JsString::from_utf16(units)), Some(-28_800_000.0));
    assert_eq!(utc("garbage 1970/1/1 GMT"), Some(0.0));
}
#[test]
fn explicit_offsets_and_invalid_inputs_never_invoke_local_resolver() {
    let reject = |_: i64| -> Result<i64, JsString> { panic!("local resolution was forbidden") };
    for input in [
        "1970-01-01",
        "1970-01-01T00:00Z",
        "1970-01-01T00:00+08:00",
        "not a date",
        "999999999",
        "+999999-01-01T00:00",
    ] {
        let _ = parse_with(&input.into(), &reject).unwrap();
    }
}
#[test]
fn valid_local_profile_errors_propagate_instead_of_disabling_deadlines() {
    let actual = parse_with(&"1970-01-01T00:00".into(), &|_| {
        Err("local-profile-gap".into())
    });
    assert_eq!(actual, Err(JsString::from("local-profile-gap")));
    assert_eq!(
        parse_with(
            &"1970-01-01".into(),
            &|_| Err("must not resolve UTC".into())
        ),
        Ok(Some(0.0))
    );
    assert_eq!(
        parse_with(&"invalid".into(), &|_| Err(
            "must not resolve invalid".into()
        )),
        Ok(None)
    );
}
#[test]
fn pure_iso_formatter_covers_negative_epoch_extended_years_and_roundtrip() {
    for (ms, expected) in [
        (0, "1970-01-01T00:00:00.000Z"),
        (-1, "1969-12-31T23:59:59.999Z"),
        (-62_167_219_200_000, "0000-01-01T00:00:00.000Z"),
        (-62_198_755_200_000, "-000001-01-01T00:00:00.000Z"),
        (253_402_300_800_000, "+010000-01-01T00:00:00.000Z"),
        (8_640_000_000_000_000, "+275760-09-13T00:00:00.000Z"),
    ] {
        assert_eq!(format_epoch_iso(ms), expected);
        assert_eq!(utc(expected), Some(ms as f64));
    }
}
#[test]
fn production_wall_clock_has_real_epoch_and_parseable_utc_millisecond_iso() {
    let clock = SystemExecutionClock;
    let before = clock.now_epoch_ms();
    let iso = clock.now_iso();
    let after = clock.now_epoch_ms();
    let parsed = clock.parse_date(&iso).unwrap().unwrap();
    assert!(before <= parsed && parsed <= after);
    assert!(before > 1_700_000_000_000.0);
    assert_eq!(before.fract(), 0.0);
    assert!(iso.to_utf8().unwrap().ends_with('Z'));
}

#[test]
fn official_arm64_node_build_smi32_and_unsigned_timezone_wrap_are_source_exact() {
    assert_eq!(utc("1 Jan 1970 GMT+500000:00"), Some(-1_800_000_000_000.0));
    assert_eq!(utc("1 Jan 1970 GMT+600000:00"), None);
    // V8 composer deliberately uses unsigned arithmetic before the Smi bound.
    // 1_193_047 *3600 wraps u32 to1904 seconds.
    assert_eq!(utc("1 Jan 1970 GMT+1193047:00"), Some(-1_904_000.0));
}

#[test]
fn frozen7_exact_additional_original_inputs_distinguish_bare_time_from_date_prefix() {
    let cases: &[(&str, Option<f64>)] = &[
        ("junk1 Jan1970 GMT", Some(0.0)),
        ("00:00:00:00", None),
        ("00:00:00:00:00", Some(946656000000.0)),
        ("00:00:00:00:00:00", None),
        ("1970-01-01 00:00 GMT+500000:00", Some(-1800000000000.0)),
        ("1970-01-01 00:00 GMT+600000:00", None),
        ("1970-01-01 00:00 GMT+1193047:00", Some(-1904000.0)),
    ];
    for (input, expected) in cases {
        assert_eq!(parse(input), *expected, "{input}");
    }
    // Date-prefixed + GMT inputs are a distinct source-valid profile from bare time.
    assert_eq!(utc("1 Jan1970 00:00:00:00 GMT"), Some(0.0));
}

// Source-contract TZif fixtures, not a replacement for the independently frozen
// Node/ICU/TZ observations. The builder constructs valid explicit data boundaries.
fn tzif_fixture(initial: i32, after: i32, transitions: &[i64], footer: &str) -> Vec<u8> {
    fn header(times: usize, kinds: usize, chars: usize) -> Vec<u8> {
        let mut bytes = b"TZif2".to_vec();
        bytes.extend([0; 15]);
        for count in [0, 0, 0, times, kinds, chars] {
            bytes.extend((count as u32).to_be_bytes());
        }
        bytes
    }
    let std_name = if let Some(rest) = footer.strip_prefix('<') {
        rest.split('>').next().unwrap()
    } else {
        let end = footer.bytes().take_while(u8::is_ascii_alphabetic).count();
        if end == 0 { "STD" } else { &footer[..end] }
    };
    let std_len = std_name.len() + 1;
    let mut bytes = header(0, 1, std_len);
    bytes.extend(initial.to_be_bytes());
    bytes.extend([0, 0]);
    bytes.extend(std_name.as_bytes());
    bytes.push(0);
    bytes.extend(header(transitions.len(), 2, std_len + 4));
    for time in transitions {
        bytes.extend(time.to_be_bytes());
    }
    for i in 0..transitions.len() {
        bytes.push(if i % 2 == 0 { 1 } else { 0 });
    }
    bytes.extend(initial.to_be_bytes());
    bytes.extend([0, 0]);
    bytes.extend(after.to_be_bytes());
    bytes.extend([1, std_len as u8]);
    bytes.extend(std_name.as_bytes());
    bytes.push(0);
    bytes.extend(b"DST\0");
    bytes.extend(b"\n");
    bytes.extend(footer.as_bytes());
    bytes.extend(b"\n");
    bytes
}
#[test]
fn independent_datefresh_045_extreme_fixed_footer_survives_chrono_range() {
    // Actual frozen independent Node22/Shanghai number: 433eb208c1248c00.
    // This synthetic TZif represents its stationary rule; actual system lookup
    // and data-version equivalence must still be independently executed.
    let bytes = tzif_fixture(28_800, 28_800, &[0, 1], "CST-8");
    let actual = parse_with(&"+275760-09-13T00:00:00".into(), &|wall| {
        system_clock::resolve_tzif_local(wall, &bytes)
    })
    .unwrap();
    assert_eq!(actual.unwrap().to_bits(), 0x433e_b208_c124_8c00);
    assert_eq!(actual, Some(8_639_999_971_200_000.0));
}
#[test]
fn fixed_footer_uses_each_actual_offset_not_a_global_shanghai_constant() {
    for (footer, offset) in [("UTC0", 0), ("NPT-5:45", 20_700), ("<-03>3", -10_800)] {
        let bytes = tzif_fixture(offset, offset, &[0, 1], footer);
        assert_eq!(
            system_clock::resolve_tzif_local(8_640_000_000_000_000, &bytes).unwrap(),
            8_640_000_000_000_000 - i64::from(offset) * 1000
        );
    }
}
#[test]
fn former_gap_offset_preserves_wall_minutes_milliseconds_and_transition_edges() {
    let bytes = tzif_fixture(0, 3600, &[0, 86400], "STD0");
    for wall in [0, 1, 1_800_123, 3_599_999] {
        assert_eq!(
            system_clock::resolve_tzif_local(wall, &bytes).unwrap(),
            wall
        );
    }
    assert_eq!(system_clock::resolve_tzif_local(-1, &bytes).unwrap(), -1);
    assert_eq!(
        system_clock::resolve_tzif_local(3_600_000, &bytes).unwrap(),
        0
    );
    // The duplicate wall interval picks the formerly active +01:00, not later0.
    assert_eq!(
        system_clock::resolve_tzif_local(86_400_123, &bytes).unwrap(),
        82_800_123
    );
    assert_eq!(
        system_clock::resolve_tzif_local(90_000_000, &bytes).unwrap(),
        90_000_000
    );
}
#[test]
fn non_hour_and_full_day_forward_gaps_keep_the_former_rule() {
    for after in [1800, 86_400] {
        let bytes = tzif_fixture(0, after, &[0, 172800], "STD0");
        let middle = i64::from(after) * 500 + 123;
        assert_eq!(
            system_clock::resolve_tzif_local(middle, &bytes).unwrap(),
            middle
        );
    }
}
#[test]
fn former_gap_handles_historical_offset_seconds_without_rounding_to_minutes() {
    let bytes = tzif_fixture(20_476, 20_700, &[0, 86400], "STD-5:41:16");
    let wall = 20_500_987;
    assert_eq!(
        system_clock::resolve_tzif_local(wall, &bytes).unwrap(),
        24_987
    );
}
#[test]
fn missing_footer_remains_a_gap_but_valid_recurring_rules_are_resolved() {
    let missing = tzif_fixture(0, 3600, &[0, 86400], "");
    assert!(system_clock::resolve_tzif_local(8_640_000_000_000_000, &missing).is_err());
    let recurring = tzif_fixture(0, 3600, &[0, 86400], "STD0DST,M3.2.0,M11.1.0");
    // Unlike missing rules, the checked POSIX recurrence owns a real offset.
    // September is in the supplied northern-hemisphere DST season, even at the
    // full JS calendar limit; no representative year is substituted.
    assert_eq!(
        system_clock::resolve_tzif_local(8_640_000_000_000_000, &recurring).unwrap(),
        8_639_999_996_400_000
    );
    assert_eq!(
        system_clock::resolve_tzif_local(1_800_123, &recurring).unwrap(),
        1_800_123
    );
}
#[test]
fn checked_timezone_parser_rejects_truncation_indices_order_and_bad_footer() {
    let valid = tzif_fixture(0, 3600, &[0, 86400], "STD0");
    let block_end = valid.len() - "STD0".len() - 2;
    for end in 0..=block_end {
        assert!(
            system_clock::resolve_tzif_local(1, &valid[..end]).is_err(),
            "truncated data at {end}"
        );
    }
    // tz-rs accepts a single NL as an empty footer. Its explicit historical
    // transition remains usable; a future date must not guess the last offset.
    assert_eq!(
        system_clock::resolve_tzif_local(1, &valid[..block_end + 1]).unwrap(),
        1
    );
    assert!(
        system_clock::resolve_tzif_local(8_640_000_000_000_000, &valid[..block_end + 1]).is_err()
    );
    assert!(system_clock::resolve_tzif_local(1, &valid[..valid.len() - 1]).is_err());
    let mut index = valid.clone();
    // First v2 header+32-bit block=54; second header=44, two64-bit times=16.
    index[54 + 44 + 16] = 255;
    assert!(system_clock::resolve_tzif_local(1, &index).is_err());
    let reversed = tzif_fixture(0, 3600, &[86400, 0], "STD0");
    assert!(system_clock::resolve_tzif_local(1, &reversed).is_err());
    let bad = tzif_fixture(0, 3600, &[0, 86400], "STD0\0");
    assert!(system_clock::resolve_tzif_local(1, &bad).is_err());
}
#[test]
fn tzif_range_projection_still_obeys_before_utc_clip_and_source_time_clip() {
    let east = tzif_fixture(28_800, 28_800, &[0, 1], "CST-8");
    let convert = |wall| system_clock::resolve_tzif_local(wall, &east);
    assert_eq!(
        parse_with(&"+275760-09-13T08:00:00".into(), &convert).unwrap(),
        Some(8_640_000_000_000_000.0)
    );
    assert_eq!(
        parse_with(&"+275760-09-13T08:00:00.001".into(), &convert).unwrap(),
        None
    );
    assert_eq!(
        parse_with(&"+275760-10-14T00:00:00".into(), &|_| panic!(
            "beforeUTCclip must prevent resolution"
        ))
        .unwrap(),
        None
    );
    assert_eq!(parse_with(&"not a date".into(), &convert).unwrap(), None);
}

#[test]
fn system_timezone_lookup_never_admits_user_paths_or_parent_traversal() {
    use std::ffi::OsStr;
    assert_eq!(
        system_clock::system_tzif_path(None).unwrap(),
        std::path::Path::new("/etc/localtime")
    );
    assert_eq!(
        system_clock::system_tzif_path(Some(OsStr::new(":Asia/Shanghai"))).unwrap(),
        std::path::Path::new("/usr/share/zoneinfo/Asia/Shanghai")
    );
    for tz in [
        "/Users/example/private.tzif",
        "../../private",
        "Asia/../private",
        "Asia//Shanghai",
        "./UTC",
        "",
        "STD0DST,M3.2.0,M11.1.0",
        "UTC\0hidden",
    ] {
        assert!(
            system_clock::system_tzif_path(Some(OsStr::new(tz))).is_none(),
            "{tz:?}"
        );
    }
}

fn la_tzif() -> Vec<u8> {
    // Exact supplied rule rather than environment mutation: 2024 spring/fall
    // UTC transitions and the source-compatible recurring POSIX wall rules.
    tzif_fixture(
        -28_800,
        -25_200,
        &[1_710_064_800, 1_730_624_400],
        "PST8PDT,M3.2.0,M11.1.0",
    )
}
fn with_tzif(input: &str, bytes: &[u8]) -> Result<Option<f64>, JsString> {
    parse_with(&input.into(), &|wall| {
        system_clock::resolve_tzif_local(wall, bytes)
    })
}
#[test]
fn independent543_fold_end_and_future_gap_counterexamples_exact_original_bits() {
    let bytes = la_tzif();
    // Existing independently executed Node22/ICU77.1 TZ2025b observations.
    // Author must still execute this Rust candidate and independent original
    // must recompare actual system data, rather than treat synthetic data as it.
    for (input, bits) in [
        ("2024-11-03T02:00:00", 0x4279_2f17_7990_0000),
        ("2039-03-13T02:30:00", 0x427f_c6a3_1444_0000),
        ("2500-03-14T02:30:00", 0x42ae_6f34_5838_8000),
    ] {
        assert_eq!(
            with_tzif(input, &bytes).unwrap().unwrap().to_bits(),
            bits,
            "{input}"
        );
    }
    for input in ["+275760-09-13T00:00:00", "+275760-09-13T23:59:59.999"] {
        assert_eq!(with_tzif(input, &bytes).unwrap(), None, "{input}");
    }
}
#[test]
fn former_fold_half_open_endpoints_and_millisecond_neighbors() {
    let bytes = la_tzif();
    for (input, offset) in [
        ("2024-11-03T00:59:59.999", -25_200),
        ("2024-11-03T01:00:00", -25_200),
        ("2024-11-03T01:00:00.001", -25_200),
        ("2024-11-03T01:59:59.999", -25_200),
        ("2024-11-03T02:00:00", -28_800),
        ("2024-11-03T02:00:00.001", -28_800),
    ] {
        let wall = utc(&format!("{input}Z")).unwrap() as i64;
        assert_eq!(
            system_clock::resolve_tzif_local(wall, &bytes).unwrap(),
            wall - i64::from(offset) * 1000,
            "{input}"
        );
    }
}
#[test]
fn former_gap_half_open_endpoints_preserve_wall_milliseconds() {
    let bytes = la_tzif();
    for (input, offset) in [
        ("2024-03-10T01:59:59.999", -28_800),
        ("2024-03-10T02:00:00", -28_800),
        ("2024-03-10T02:00:00.001", -28_800),
        ("2024-03-10T02:59:59.999", -28_800),
        ("2024-03-10T03:00:00", -25_200),
        ("2024-03-10T03:00:00.001", -25_200),
    ] {
        let wall = utc(&format!("{input}Z")).unwrap() as i64;
        assert_eq!(
            system_clock::resolve_tzif_local(wall, &bytes).unwrap(),
            wall - i64::from(offset) * 1000,
            "{input}"
        );
    }
}
#[test]
fn future_recurring_rule_uses_real_year_and_former_policy_at_boundaries() {
    let bytes = la_tzif();
    for (input, offset) in [
        ("2039-03-13T01:59:59.999", -28_800),
        ("2039-03-13T02:00:00", -28_800),
        ("2039-03-13T02:59:59.999", -28_800),
        ("2039-03-13T03:00:00", -25_200),
        ("2039-11-06T01:59:59.999", -25_200),
        ("2039-11-06T02:00:00", -28_800),
        ("2500-03-14T02:30:00.123", -28_800),
        ("2500-03-14T03:00:00", -25_200),
        ("2500-11-07T01:59:59.999", -25_200),
        ("2500-11-07T02:00:00", -28_800),
    ] {
        let wall = utc(&format!("{input}Z")).unwrap() as i64;
        assert_eq!(
            system_clock::resolve_tzif_local(wall, &bytes).unwrap(),
            wall - i64::from(offset) * 1000,
            "{input}"
        );
    }
}
#[test]
fn clip_proof_uses_all_actual_footer_offsets_and_retains_viable_candidates() {
    let negative = la_tzif();
    // Every supplied local type and footer offset is negative. At positive
    // TimeClip end every possible UTC is too late; this is Source NaN proof.
    assert!(
        system_clock::resolve_tzif_local(8_640_000_000_000_000, &negative).unwrap()
            > 8_640_000_000_000_000
    );
    let positive = tzif_fixture(28_800, 32_400, &[0, 1], "CST-8CDT,M3.2.0,M11.1.0");
    assert!(
        system_clock::resolve_tzif_local(8_640_000_000_000_000, &positive).unwrap()
            < 8_640_000_000_000_000
    );
    // A footer offset absent in historical types must still enter the proof.
    // This zone has no historical transitions, so the supplied recurrence is
    // the authority; no global negative historical bound may erase this date.
    let mixed = tzif_fixture(-3600, -3600, &[], "STD1DST-1,M3.2.0,M11.1.0");
    assert_eq!(
        system_clock::resolve_tzif_local(8_640_000_000_000_000, &mixed).unwrap(),
        8_639_999_996_400_000
    );
}
#[test]
fn recurring_nonnorthern_and_nonhour_rules_keep_former_offsets() {
    // Supplied Southern rule: spring start in October, fall end in April.
    let bytes = tzif_fixture(36_000, 37_800, &[], "AEST-10AEDT-10:30,M10.1.0,M4.1.0");
    for (input, offset) in [
        ("2500-10-03T02:00:00", 36_000),
        ("2500-10-03T02:29:59.999", 36_000),
        ("2500-10-03T02:30:00", 37_800),
        ("2500-04-04T01:59:59.999", 37_800),
        ("2500-04-04T02:00:00", 36_000),
    ] {
        let wall = utc(&format!("{input}Z")).unwrap() as i64;
        assert_eq!(
            system_clock::resolve_tzif_local(wall, &bytes).unwrap(),
            wall - i64::from(offset) * 1000,
            "{input}"
        );
    }
}

/// #360 fix-r3 (r3 review P3-1): a zone file above the shared 1 MiB cap is
/// rejected, not truncated — the read takes one byte past the cap precisely so
/// `bounded_zone_read` can tell "at most 1 MiB" from "larger" and return
/// `None`, matching `stats_tzif_local`'s explicit post-read rejection. A
/// truncating read would silently feed cut-off bytes to the parser.
#[test]
fn zone_read_above_the_1mib_cap_is_not_admitted() {
    struct RemoveOnDrop(std::path::PathBuf);
    impl Drop for RemoveOnDrop {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let path = std::env::temp_dir().join(format!(
        "pdrs-zone-cap-{}-{:x}.tzif",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or(0),
    ));
    let _guard = RemoveOnDrop(path.clone());
    let fixture = tzif_fixture(0, 0, &[0, 1], "UTC0");
    // At the cap the whole file is admitted, byte for byte.
    let mut capped = fixture.clone();
    capped.resize(1_048_576, 0);
    std::fs::write(&path, &capped).expect("#360 write capped zone file");
    assert_eq!(
        system_clock::bounded_zone_read(&path),
        Some(capped),
        "#360 a 1 MiB zone file must be admitted in full"
    );
    // One byte past the cap the file is not an admitted profile.
    let mut oversized = fixture;
    oversized.resize(1_048_577, 0);
    std::fs::write(&path, &oversized).expect("#360 write oversized zone file");
    assert_eq!(
        system_clock::bounded_zone_read(&path),
        None,
        "#360 a zone file above the 1 MiB cap must not be admitted (nor truncated)"
    );
}
