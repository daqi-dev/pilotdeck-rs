//! #1328 weixin-channel fixture provenance: the committed `oracle.json`
//! must regenerate byte-identically from the fixed TS baseline. The per-case
//! replay lives in the library (`channel::weixin::replay_tests`); this guard
//! pins the generator side (U12 pr1 fixture, `weixin-channel/generate.mts`).
#[path = "common/baseline.rs"]
mod baseline;

/// The committed oracle's own shape: one declared case count, matching the
/// scenario definitions the replay iterates. #1823 appended media cases, which
/// carry a `kind` and are declared separately.
#[test]
fn weixin_channel_committed_oracle_shape() {
    let oracle = baseline::oracle(include_str!("fixtures/weixin-channel/oracle.json"));
    let (channel, media) = match oracle.get("cases").expect("#1328 oracle cases") {
        pilotdeck_protocol::JsValue::Array(cases) => {
            cases
                .iter()
                .fold((0usize, 0usize), |(channel, media), case| {
                    if case.get("kind").is_some() {
                        (channel, media + 1)
                    } else {
                        (channel + 1, media)
                    }
                })
        }
        _ => panic!("#1328 oracle cases must be an array"),
    };
    assert_eq!(channel, 20, "#1328 weixin oracle case count");
    assert_eq!(media, 31, "#1823 weixin oracle media case count");
    let declared = oracle
        .get("expectedCaseCount")
        .and_then(|count| count.as_number())
        .expect("#1328 oracle expectedCaseCount");
    assert_eq!(
        channel as f64, declared,
        "#1328 oracle declares its case count"
    );
    let declared_media = oracle
        .get("expectedMediaCaseCount")
        .and_then(|count| count.as_number())
        .expect("#1823 oracle expectedMediaCaseCount");
    assert_eq!(
        media as f64, declared_media,
        "#1823 oracle declares its media case count"
    );
}

// The TS regeneration guard lives in `tests/generator_provenance.rs`
// (`weixin_channel_regenerates_identically`), where every fixture's
// regeneration is registered; this file keeps only the committed-shape guard
// so the fixture is not registered twice.
