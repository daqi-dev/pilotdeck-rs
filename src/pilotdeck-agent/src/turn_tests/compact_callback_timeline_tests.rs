use super::*;
fn boundary(input: &str) -> crate::ports::CompactBoundary {
    crate::ports::CompactBoundary {
        boundary: BoundarySnapshot::capture(&pilotdeck_protocol::parse_json(input).unwrap()),
    }
}
#[test]
fn callback_compaction_reuses_position_and_overwrites_stale_timeline() {
    let mut timeline = TurnTimeline::new("turn".into());
    let first = annotate_compaction_boundary(&mut timeline,
            boundary(r#"{"kind":"compact","compactMetadata":{"compactionId":"same","timeline":{"stale":true}}}"#)).unwrap();
    timeline.position("message:later".into(), false);
    let again = annotate_compaction_boundary(
        &mut timeline,
        boundary(r#"{"kind":"compact","compactMetadata":{"compactionId":"same"}}"#),
    )
    .unwrap();
    let position = |b: &crate::ports::CompactBoundary| {
        b.boundary
            .value()
            .get("compactMetadata")
            .unwrap()
            .get("timeline")
            .unwrap()
            .clone()
    };
    assert_eq!(position(&first), position(&again));
    assert_eq!(
        position(&first).get("id"),
        Some(&JsValue::from("compact:same"))
    );
    assert_eq!(position(&first).get("order"), Some(&JsValue::Number(0.0)));
    assert!(position(&first).get("stale").is_none());
}
#[test]
fn falsy_compaction_id_does_not_allocate_or_overwrite_callback_timeline() {
    let mut timeline = TurnTimeline::new("turn".into());
    let original = boundary(
        r#"{"kind":"compact","compactMetadata":{"compactionId":0,"timeline":{"stale":true}}}"#,
    );
    let result = annotate_compaction_boundary(&mut timeline, original.clone()).unwrap();
    assert_eq!(result.boundary.value(), original.boundary.value());
    assert_eq!(timeline.position("next".into(), false).order, 0.0);
}
