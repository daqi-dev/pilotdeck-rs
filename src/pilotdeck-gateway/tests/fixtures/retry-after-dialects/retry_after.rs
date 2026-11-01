use super::*;

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
