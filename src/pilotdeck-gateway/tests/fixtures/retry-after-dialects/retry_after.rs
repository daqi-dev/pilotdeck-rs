use super::*;
use std::process::Command;

/// #360-2: the real TS `parseRetryAfterHeader` (baseline `errors.ts`) with
/// `Date.now()` pinned to 0, recorded in TZ=UTC and TZ=Asia/Shanghai children
/// by `generate.mts`. Every `Date.parse` dialect the header accepts must
/// round-trip through the shared Node22 parser port: delta-seconds `Number`
/// traps, IMF-fixdate edges, RFC 850, asctime, ISO (date-only UTC, no-zone
/// local), named zones, numeric offsets, and the unparseable remainders.
#[test]
fn retry_after_dialects_match_node_in_two_timezones() {
    let oracle: serde_json::Value =
        serde_json::from_str(include_str!("oracle.json")).expect("#360-2 parse oracle");
    // Provenance belongs to the quick replay, not only to the byte-identity
    // regeneration guard: swapping an oracle whose cases still match while
    // the recorded source drifts must not stay green.
    assert_eq!(
        oracle["sourceCommit"].as_str(),
        Some("cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85"),
        "#360-2 oracle source commit"
    );
    assert_eq!(
        oracle["sourceSha256"]["src/model/protocol/errors.ts"].as_str(),
        Some("109e8cd3191d2ae51bec90578148fc06a3f8dc22a2d28c71a96c77fe2e9984bf"),
        "#360-2 oracle source sha256"
    );
    let zones = oracle["zones"].as_array().expect("#360-2 zone table");
    let cases = |index: usize| {
        zones[index]["cases"]
            .as_array()
            .unwrap_or_else(|| panic!("#360-2 cases of zone {index}"))
    };
    let (utc, shanghai) = (cases(0), cases(1));
    assert_eq!(
        utc.len(),
        shanghai.len(),
        "#360-2 zones agree on case count"
    );
    assert_eq!(utc.len(), 62, "#360-2 full dialect table");
    assert_eq!(
        zones[0]["tz"].as_str(),
        Some("UTC"),
        "#360-2 first zone is UTC"
    );
    assert_eq!(
        zones[1]["tz"].as_str(),
        Some("Asia/Shanghai"),
        "#360-2 second zone is Asia/Shanghai"
    );
    // Shanghai has been a flat +08:00 zone across every local-resolving date
    // in the table (1994/2015), so the fixed replay offset is exact here.
    fn utc_wall(wall: i64) -> Result<i64, JsString> {
        Ok(wall)
    }
    fn shanghai_wall(wall: i64) -> Result<i64, JsString> {
        Ok(wall - 28_800_000)
    }
    for (index, (utc_case, shanghai_case)) in utc.iter().zip(shanghai).enumerate() {
        let value = utc_case["value"].as_str().expect("#360-2 case value");
        let expected = |case: &serde_json::Value| case["retryAfterMs"].as_f64();
        assert_eq!(
            errors::retry_after_header_with_local(Some(value), 0.0, &utc_wall),
            expected(utc_case),
            "#360-2 UTC dialect case {index}: {value:?}"
        );
        assert_eq!(
            errors::retry_after_header_with_local(Some(value), 0.0, &shanghai_wall),
            expected(shanghai_case),
            "#360-2 Shanghai dialect case {index}: {value:?}"
        );
        // Where both pinned zones agree, the value carries no wall-time
        // dependency, so the production entry (process-timezone resolution)
        // must agree too — the wiring is the same parser closure.
        if utc_case == shanghai_case {
            assert_eq!(
                errors::retry_after_header(Some(value), 0.0),
                expected(utc_case),
                "#360-2 production wiring for case {index}: {value:?}"
            );
        }
    }
}

/// The dialect table must keep at least one local-resolving case per family
/// (asctime and zone-less ISO); otherwise the two-zone replay silently stops
/// covering timezone-dependent dialects.
#[test]
fn retry_after_dialect_table_keeps_local_resolving_cases() {
    let oracle: serde_json::Value =
        serde_json::from_str(include_str!("oracle.json")).expect("#360-2 parse oracle");
    let zones = oracle["zones"].as_array().expect("#360-2 zone table");
    let utc = zones[0]["cases"].as_array().expect("#360-2 UTC cases");
    let shanghai = zones[1]["cases"].as_array().expect("#360-2 Shanghai cases");
    let differing = utc
        .iter()
        .zip(shanghai)
        .filter(|(utc_case, shanghai_case)| utc_case != shanghai_case)
        .count();
    assert_eq!(differing, 3, "#360-2 local-resolving dialect cases");
}

const ZONE_ENV: &str = "PDRS_RETRY_AFTER_ZONE";
const ZONE_CHILD: &str =
    "runtime::model::http::retry_after_dialect_tests::retry_after_dialect_zone_child";

/// #360-2 fix-r1: the timezone-dependent dialects (asctime, zone-less ISO)
/// must also be asserted through the production entry — process `TZ` → the
/// admitted system TZif resolver — not only through the injected seam, so the
/// real runtime wiring cannot rot while the seam replay stays green. Each
/// child inherits its `TZ` before any parsing (no in-process `set_var`); the
/// libtest receipt proves the child really ran (#273), and `--test-threads=2`
/// pins the multithreaded receipt layout (#942).
#[test]
fn retry_after_dialects_production_wiring_matches_node_zone_children() {
    for zone in ["UTC", "Asia/Shanghai", "unsupported-profile"] {
        let output = Command::new(std::env::current_exe().expect("#360-2 zone child executable"))
            .args(["--exact", ZONE_CHILD, "--nocapture", "--test-threads=2"])
            .env(ZONE_ENV, zone)
            .env("TZ", zone)
            .output()
            .expect("#360-2 zone child spawn");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let status = output.status;
        assert!(
            crate::test_exact_receipt::single_pass(&status, ZONE_CHILD, &stdout),
            "#360-2 production wiring replay in TZ={zone} failed: {status}\n{stdout}\n{stderr}",
        );
    }
}

/// The child half of the zone replay: run only under
/// [`retry_after_dialects_production_wiring_matches_node_zone_children`].
///
/// `TZ=UTC` / `TZ=Asia/Shanghai` children replay the whole oracle row of that
/// zone through `errors::retry_after_header` (the 1994/2015 local-resolving
/// dates sit in Shanghai's flat +08:00 era in every tzdata release, so the
/// host TZif cannot disagree with the pinned ICU recording). The
/// `unsupported-profile` child pins the fail-closed edge of the admitted-TZif
/// subset: its system TZif is unreadable, so the local-resolving dialects
/// count as unparseable, while zone-independent values keep the UTC column
/// that Node v22.22.0 itself keeps there (it falls back to UTC — the retained
/// difference recorded under A6.4-STATS-RETENTION-001).
#[test]
fn retry_after_dialect_zone_child() {
    let Ok(zone) = std::env::var(ZONE_ENV) else {
        return;
    };
    assert_eq!(
        std::env::var("TZ").expect("#360-2 zone child TZ env"),
        zone,
        "#360-2 zone child must inherit the pinned TZ"
    );
    let oracle: serde_json::Value =
        serde_json::from_str(include_str!("oracle.json")).expect("#360-2 zone child oracle");
    let zones = oracle["zones"]
        .as_array()
        .expect("#360-2 zone child zone table");
    let utc = zones[0]["cases"]
        .as_array()
        .expect("#360-2 zone child UTC cases");
    let shanghai = zones[1]["cases"]
        .as_array()
        .expect("#360-2 zone child Shanghai cases");
    assert_eq!(
        utc.len(),
        shanghai.len(),
        "#360-2 zone child zone tables agree on case count"
    );
    let row = zones
        .iter()
        .find(|zone_row| zone_row["tz"].as_str() == Some(zone.as_str()))
        .map(|zone_row| {
            zone_row["cases"]
                .as_array()
                .expect("#360-2 zone child oracle row cases")
        });
    if let Some(row) = &row {
        assert_eq!(
            row.len(),
            utc.len(),
            "#360-2 zone child oracle row matches the UTC table"
        );
    }
    for (index, (utc_case, shanghai_case)) in utc.iter().zip(shanghai).enumerate() {
        let value = utc_case["value"]
            .as_str()
            .expect("#360-2 zone child case value");
        let expected = match &row {
            Some(row) => row[index]["retryAfterMs"].as_f64(),
            // Unreadable system TZif: the local-resolving dialects count as
            // unparseable and the header is ignored; everything else carries
            // no wall-time dependency and keeps the UTC column.
            None => {
                if utc_case != shanghai_case {
                    None
                } else {
                    utc_case["retryAfterMs"].as_f64()
                }
            }
        };
        assert_eq!(
            errors::retry_after_header(Some(value), 0.0),
            expected,
            "#360-2 production wiring in TZ={zone} case {index}: {value:?}"
        );
    }
    println!("{zone}: {} production-wiring dialect cases", utc.len());
}

/// #360-2 fix-r1: delta-seconds whose `×1000` overflows f64 cannot live in
/// the JSON oracle — `JSON.stringify(Infinity)` is `null`, indistinguishable
/// from the unparseable null — so the overflow edge pins here. Real TS
/// (Node v22.22.0, `Date.now()` pinned to 0): `parseRetryAfterHeader("1e308")`
/// is `Infinity` and `"1e309"` is `undefined` (`Number("1e309")` is not
/// finite and `Date.parse` rejects the text); `decide` clamps to
/// `maxDelayMs`, so the parsed `Infinity` never reaches a `Duration`.
#[test]
fn retry_after_dialects_huge_delta_seconds_parse_to_infinity_and_clamp() {
    assert_eq!(
        errors::retry_after_header(Some("1e308"), 0.0),
        Some(f64::INFINITY),
        "#360-2 1e308 delta-seconds parse to Infinity milliseconds"
    );
    assert_eq!(
        errors::retry_after_header(Some("1e309"), 0.0),
        None,
        "#360-2 1e309 is not a finite delta-seconds value"
    );
    let (max_retries, delay) = retry_decide(&JsValue::Undefined, 0.0, Some(f64::INFINITY), 0.5);
    assert_eq!(
        max_retries, 2.0,
        "#360-2 default streamMaxRetries for a header-only retry"
    );
    assert_eq!(
        delay, 8_000.0,
        "#360-2 decide clamps the Infinity header to the default maxDelayMs"
    );
    assert_eq!(
        duration(delay),
        Duration::from_millis(8_000),
        "#360-2 the clamped retry delay is a finite Duration"
    );
}
