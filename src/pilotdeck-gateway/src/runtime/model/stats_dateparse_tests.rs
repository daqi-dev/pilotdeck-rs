//! Real TS/Node22 observations: no hand-authored date or retention expectations.
use super::*;
use std::process::Command;

const CHILD: &str = "runtime::model::stats::dateparse_tests::dateparse_zone_child";
const ZONE_ENV: &str = "PDRS_STATS_DATEPARSE_ZONE";
const GOLD: &str = include_str!("../../../tests/fixtures/stats-dateparse/oracle.json");

fn field<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    value.get(key).unwrap_or_else(|| panic!("missing {key}"))
}
fn array(value: &JsValue) -> &[JsValue] {
    let JsValue::Array(values) = value else {
        panic!("expected array");
    };
    values
}
fn string(value: &JsValue) -> &JsString {
    let JsValue::String(value) = value else {
        panic!("expected UTF16 string");
    };
    value
}
fn number_field(value: &JsValue, key: &str) -> f64 {
    field(value, key).as_number().unwrap()
}
fn record(input: &JsString) -> JsValue {
    let mut object = JsObject::new();
    object.insert("endedAt".into(), JsValue::String(input.clone()));
    JsValue::Object(object)
}

/// Each process inherits TZ before any date parsing. No global set_var races
/// with other Rust tests, no injectable fake offset in the actual comparison.
#[test]
fn stats_dateparse_matches_real_ts_in_three_timezones() {
    for zone in [
        "UTC",
        "America/New_York",
        "Asia/Shanghai",
        "unsupported-profile",
        "Europe/London",
        "localtime",
        ":Asia/Shanghai",
        "/etc/localtime",
    ] {
        let output = Command::new(std::env::current_exe().expect("#273 zone child path"))
            .args(["--exact", CHILD, "--nocapture"])
            .env(ZONE_ENV, zone)
            .env("TZ", zone)
            .output()
            .expect("#273 zone child spawn");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        // #273: libtest exits 0 even when --exact matches nothing after a rename;
        // only the libtest receipt proves the zone child really ran.
        assert!(
            crate::test_exact_receipt::single_pass(&output.status, CHILD, &stdout),
            "#273 TZ rerun matched no test: TZ={zone}: {status}\n{stdout}\n{stderr}",
            status = output.status
        );
        println!("{stdout}");
    }
}

#[test]
fn dateparse_zone_child() {
    let Ok(zone) = std::env::var(ZONE_ENV) else {
        return;
    };
    assert_eq!(std::env::var("TZ").unwrap(), zone);
    if !matches!(zone.as_str(), "UTC" | "America/New_York" | "Asia/Shanghai") {
        // The shared parser reports Err for unsupported timezone environments.
        // Stats must preserve the row rather than fabricate an old timestamp.
        let local = JsString::from("2000-01-01T00:00:00");
        let shared = pilotdeck_transport::system_clock::parse_node22_date(&local);
        if zone == "unsupported-profile" {
            assert!(shared.is_err());
        } else {
            assert!(shared.unwrap().is_some());
        }
        assert_eq!(parse_date_ms(&local), None);
        assert!(is_retained_record(Some(0.0), 1.0e15, &record(&local)));
        assert!(parse_date_ms(&"2000-01-01T00:00:00Z".into()).is_some());
        return;
    }
    let fixture = parse_json(GOLD).unwrap();
    assert_eq!(field(&fixture, "node"), &JsValue::from("v22.22.0"));
    assert_eq!(field(&fixture, "v8"), &JsValue::from("12.4.254.21-node.33"));
    assert_eq!(
        field(&fixture, "sourceCommit"),
        &JsValue::from("cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85")
    );
    let observations = array(field(&fixture, "zones"))
        .iter()
        .find(|value| string(field(value, "tz")).to_utf8().unwrap() == zone)
        .unwrap();
    let tzif = fs::read(format!("/usr/share/zoneinfo/{zone}")).unwrap();
    let admitted = stats_tzif_local(0, &tzif, &zone).is_ok();
    let mut unverified_local = 0;
    let inputs = array(field(&fixture, "inputs"));
    let cases = array(field(observations, "cases"));
    assert!(inputs.len() >= 8_000, "corpus must not silently shrink");
    assert_eq!(inputs.len(), cases.len());
    let now = number_field(&fixture, "now");
    let retention = number_field(&fixture, "retentionMs");
    for (index, (input, case)) in inputs.iter().zip(cases).enumerate() {
        let input = string(input);
        let expected = array(case);
        let epoch = expected[0].as_number();
        if !admitted {
            let local = std::cell::Cell::new(false);
            let _ = pilotdeck_transport::system_clock::parse_node22_date_with_local(input, &|_| {
                local.set(true);
                Err("unverified test host profile".into())
            });
            if local.get() {
                assert_eq!(parse_date_ms(input), None, "unverified {zone}: {input:?}");
                assert!(is_retained_record(Some(retention), now, &record(input)));
                unverified_local += 1;
                continue;
            }
        }
        assert_eq!(
            parse_date_ms(input),
            epoch,
            "{zone} case {index}: {input:?}"
        );
        let record = record(input);
        assert_eq!(
            JsValue::Bool(is_retained_record(Some(retention), now, &record)),
            expected[1],
            "retention {zone} case {index}: {input:?}"
        );
        if let Some(epoch) = epoch {
            for (offset, expected) in [-1.0, 0.0, 1.0].into_iter().zip(array(&expected[2])) {
                assert_eq!(
                    JsValue::Bool(is_retained_record(
                        Some(retention),
                        epoch + retention + offset,
                        &record
                    )),
                    *expected,
                    "boundary {zone} case {index} delta={offset}: {input:?}"
                );
            }
        }
    }
    for invalid in array(field(&fixture, "invalidUtf8")) {
        let bytes = array(field(invalid, "bytes"))
            .iter()
            .map(|value| value.as_number().unwrap() as u8)
            .collect::<Vec<_>>();
        assert!(std::str::from_utf8(&bytes).is_err());
        let decoded = String::from_utf8_lossy(&bytes);
        assert_eq!(
            JsString::from(decoded.as_ref()),
            *string(field(invalid, "input"))
        );
        // No unsafe invalid Rust str: journal JSON has already decoded UTF8.
        let _ = parse_date_ms(&decoded.as_ref().into());
    }
    if !admitted {
        println!(
            "PARTIAL {zone}: {unverified_local} local cases conservatively preserved; exact local Date.parse/journal comparison unavailable on this host TZif"
        );
        return;
    }
    assert_compaction_and_reopen(field(observations, "journal"), now, retention);
    println!(
        "{zone}: {} Date.parse values, retention and +/-1ms boundaries; journal compaction/reopen PASS",
        inputs.len()
    );
}

fn assert_compaction_and_reopen(expected: &JsValue, now: f64, retention: f64) {
    let home = std::env::temp_dir().join(format!("pdrs-stats-dateparse-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&home).unwrap();
    let path = home.join("stats.jsonl");
    let bytes = array(field(expected, "inputBytes"))
        .iter()
        .map(|value| value.as_number().unwrap() as u8)
        .collect::<Vec<_>>();
    assert!(
        std::str::from_utf8(&bytes).is_err(),
        "exercise actual malformed UTF8 journal bytes"
    );
    fs::write(&path, bytes).unwrap();
    let config = StatsConfig {
        enabled: true,
        file_path: Some(path.clone()),
        retention_ms: Some(retention),
        model_pricing: None,
        baseline_model: None,
    };
    let mut stats =
        TokenStatsCollector::with_clock(Some(&config), Some(&home), Arc::new(move || now))
            .expect("#879 stats dateparse collector");
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        string(field(expected, "compacted")).to_utf8().unwrap(),
        "compacted journal bytes"
    );
    assert_eq!(stats.snapshot(), *field(expected, "snapshot"));
    drop(stats);
    let mut reopened =
        TokenStatsCollector::with_clock(Some(&config), Some(&home), Arc::new(move || now))
            .expect("#879 stats dateparse reopened collector");
    assert_eq!(reopened.snapshot(), *field(expected, "reopened"));
    drop(reopened);
    fs::remove_dir_all(home).unwrap();
}

/// Bounded fuzz regression independent of the oracle: arbitrary UTF16 (lone
/// surrogates included), malformed UTF8 decoded as journal text and multibyte
/// insertions at every byte/character boundary. Safety is never a panic.
#[test]
fn stats_dateparse_random_and_multibyte_never_panic() {
    let mut state = 0x494_u32;
    let mut next = || {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        state
    };
    for _ in 0..10_000 {
        let length = (next() % 128) as usize;
        let units = (0..length).map(|_| next() as u16).collect::<Vec<_>>();
        let _ = parse_date_ms(&JsString::from_utf16(units));
        let bytes = (0..length).map(|_| next() as u8).collect::<Vec<_>>();
        let _ = parse_date_ms(&String::from_utf8_lossy(&bytes).as_ref().into());
    }
    let seed = "2000-01-01T00:00:00+00:00";
    for at in 0..=seed.len() {
        for insert in ["汉", "é", "😀", "\u{fffd}", "\u{200b}", "\0"] {
            let input = format!("{}{insert}{}", &seed[..at], &seed[at..]);
            let _ = parse_date_ms(&input.into());
        }
    }
}

#[test]
fn stats_dateparse_changed_timezone_data_preserves_local_rows() {
    let bytes = fs::read("/usr/share/zoneinfo/UTC").unwrap();
    assert!(pilotdeck_transport::system_clock::resolve_tzif_local(0, &bytes).is_ok());
    let mut changed = bytes;
    // The abbreviation is display-only; this remains a valid TZif with the
    // same UTC offset, but its bytes no longer match the verified TS profile.
    let index = changed
        .windows(4)
        .position(|part| part == b"UTC\0")
        .unwrap();
    changed[index] = b'X';
    assert!(pilotdeck_transport::system_clock::resolve_tzif_local(0, &changed).is_ok());
    assert!(stats_tzif_local(0, &changed, "UTC").is_err());
}

#[test]
fn stats_dateparse_profile_binds_timezone_name_as_well_as_bytes() {
    let bytes = fs::read("/usr/share/zoneinfo/Asia/Shanghai").unwrap();
    assert!(stats_tzif_local(0, &bytes, "UTC").is_err());
}
