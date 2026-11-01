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
    // fix-r2 P3-1: the quick replay pins the oracle's runtime provenance too,
    // not only the TS source — regenerating with a different V8/ICU/tzdata
    // while the cases happen to match must not stay green here (byte identity
    // is separately guarded by the `#[ignore]`d regeneration test).
    assert_eq!(
        oracle["node"].as_str(),
        Some("v22.22.0"),
        "#360-2 oracle node runtime"
    );
    assert_eq!(
        oracle["v8"].as_str(),
        Some("12.4.254.21-node.33"),
        "#360-2 oracle v8 runtime"
    );
    assert_eq!(
        oracle["icu"].as_str(),
        Some("78.2"),
        "#360-2 oracle icu runtime"
    );
    assert_eq!(
        oracle["tzdata"].as_str(),
        Some("2025c"),
        "#360-2 oracle tzdata runtime"
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

/// #360-2 fix-r1 (r2 zones): the timezone-dependent dialects (asctime,
/// zone-less ISO) must also be asserted through the production entry — process
/// `TZ` → the shared admitted system TZif gate — not only through the injected
/// seam, so the real runtime wiring cannot rot while the seam replay stays
/// green. `TZ=localtime` pins the fail-closed alias edge (Node v22.22.0
/// resolves it through the C library's zone search, so it is host-dependent
/// and not an admitted profile), `TZ=:/nonexistent` pins the unreadable
/// path edge without assuming a host zone name is absent (r2 P3-2), and
/// `TZ=Europe/London` is a readable zone outside the allowlist, so the
/// fail-closed None for its local dialects can only come from the gate
/// itself — a kill that does not depend on `/etc/localtime` (r3 P2-3).
/// Each child inherits its `TZ` before any parsing (no in-process
/// `set_var`); the libtest receipt proves the child really ran (#273), and
/// `--test-threads=2` pins the multithreaded receipt layout (#942).
///
/// r3 P2-2: the parent re-prints every child's stdout (the same pattern as
/// `stats_dateparse_tests.rs`), so a degraded SHA-unpaired host cannot look
/// like a Node-parity pass — the child's `#360-2 PARTIAL` marker line stays
/// visible in `cargo test -- --nocapture` output and machine-checkable by the
/// negative controls, which refuse zone-children mutants on such a host.
#[test]
fn retry_after_dialects_production_wiring_matches_node_zone_children() {
    for zone in [
        "UTC",
        "Asia/Shanghai",
        "localtime",
        ":/nonexistent",
        "Europe/London",
    ] {
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
        println!("{stdout}");
    }
}

/// The child half of the zone replay: run only under
/// [`retry_after_dialects_production_wiring_matches_node_zone_children`].
///
/// `TZ=UTC` / `TZ=Asia/Shanghai` children replay the whole oracle row of that
/// zone through `errors::retry_after_header` — but only when this host's
/// `/usr/share/zoneinfo/<zone>` bytes are the SHA-paired
/// `local-profiles.json` profile (r2 P2-2); the 1994/2015 local-resolving
/// dates sit in Shanghai's flat +08:00 era, which is exactly why a byte
/// mismatch would otherwise stay silently green. On an unpaired host the row
/// is not claimed as a pass: the shared gate itself fail-closes, the
/// local-resolving dialects are expected as `None`, and the run prints a
/// machine-checkable `#360-2 PARTIAL` downgrade marker (r3 P2-2; the same
/// convention as `stats_dateparse_tests.rs`). The `localtime`
/// (host-dependent alias, fail-closed by the allowlist),
/// `:/nonexistent` (unreadable path), and `Europe/London` (readable zone
/// outside the allowlist) children pin the edges of the admitted-TZif
/// subset: for the readable aliases the ungated shared parser is asserted to
/// parse the host profile, so the local dialects' `None` comes from the gate
/// and not from a missing file, while zone-independent values keep the UTC
/// column that Node v22.22.0 itself keeps there (it falls back to UTC — the
/// retained difference registered for this consumer by
/// `specs/gateway-parity/diffs/A6.4-PROVIDER-RETRY-AFTER-TZ-001.md`).
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
    let admitted = match &row {
        // Pinned column is the expectation only for the SHA-paired host
        // profile; the production entry applies the very same gate, so an
        // unpaired host must come out fail-closed, not coincidentally green.
        Some(_) => {
            let bytes = std::fs::read(format!("/usr/share/zoneinfo/{zone}"))
                .expect("#360-2 zone child host TZif read");
            let verified = crate::runtime::model::stats::stats_tzif_local(0, &bytes, &zone).is_ok();
            if !verified {
                // r3 P2-2: a machine-checkable PARTIAL marker (the sibling
                // convention of stats_dateparse_tests.rs) — the parent
                // re-prints it, and the negative controls refuse
                // zone-children mutants on a host in this state.
                println!(
                    "#360-2 PARTIAL {zone}: host TZif is not the SHA-paired \
                     local-profiles.json profile; the local-resolving dialects \
                     replay fail-closed (None), not as a Node-parity pass"
                );
            }
            verified
        }
        None => false,
    };
    if row.is_none() {
        // r3 P2-3 (mirrors stats dateparse_zone_child's non-admitted branch):
        // for a pinned zone outside the allowlist, prove the gate — not a
        // missing host file — is what fail-closes the local dialects. The
        // ungated shared parser must read and parse the host profile
        // (Europe/London directly pins the allowlist without depending on
        // /etc/localtime; `:/nonexistent` must stay unreadable), and the
        // production entry must still count the local dialect unparseable.
        let wall = "Sun Nov  6 08:49:37 1994";
        let ungated = pilotdeck_transport::system_clock::parse_node22_date(&JsString::from(wall));
        if zone == ":/nonexistent" {
            assert!(
                ungated.is_err(),
                "#360-2 TZ={zone} must stay an unreadable path, got {ungated:?}"
            );
        } else {
            let parsed = ungated.unwrap_or_else(|error| {
                panic!(
                    "#360-2 TZ={zone} must have a readable host profile so the \
                     allowlist, not a missing file, fail-closes it: {error:?}"
                )
            });
            assert!(
                parsed.is_some(),
                "#360-2 TZ={zone} ungated parser must resolve the local dialect"
            );
        }
        assert_eq!(
            errors::retry_after_header(Some(wall), 0.0),
            None,
            "#360-2 TZ={zone} must fail-close the local dialect at the admitted gate"
        );
    }
    for (index, (utc_case, shanghai_case)) in utc.iter().zip(shanghai).enumerate() {
        let value = utc_case["value"]
            .as_str()
            .expect("#360-2 zone child case value");
        let expected = if admitted {
            row.as_ref().expect("#360-2 zone child admitted oracle row")[index]["retryAfterMs"]
                .as_f64()
        } else if utc_case != shanghai_case {
            // Outside the admitted subset (alias like `TZ=localtime`, an
            // unreadable path, or an unpaired host profile): the
            // local-resolving dialects count as unparseable and the header
            // is ignored; everything else carries no wall-time dependency
            // and keeps the UTC column.
            None
        } else {
            utc_case["retryAfterMs"].as_f64()
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
