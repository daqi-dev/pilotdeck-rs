//! Lossless channel input port. TS WebhookChannel.ts:338-363 and
//! packages/sdk/src/transport.ts:281 preserve JavaScript strings on the wire.
use super::*;
pub trait ChannelOrderedEventSink: Send + Sync {
    fn event_ordered(&self, event: JsValue) -> ChannelCall<'_, ()>;
}
pub(super) fn serde_view(value: &JsValue) -> Result<Value, RouteError> {
    let wire = pilotdeck_protocol::stringify_json(value)
        .map_err(|e| RouteError::new("CHANNEL_JSON_INVALID", e.to_string()))?
        .unwrap_or_else(|| "null".into());
    serde_json::from_str(&wire)
        .map_err(|e| RouteError::new("CHANNEL_ORDERED_REQUIRED", e.to_string()))
}
pub(super) struct LegacySink<'a>(pub &'a dyn ChannelOrderedEventSink);
impl ChannelEventSink for LegacySink<'_> {
    fn event(&self, event: Value) -> ChannelCall<'_, ()> {
        Box::pin(async move {
            let wire = serde_json::to_string(&event)
                .map_err(|e| RouteError::new("CHANNEL_JSON_INVALID", e.to_string()))?;
            let event = pilotdeck_protocol::parse_json(&wire)
                .map_err(|e| RouteError::new("CHANNEL_JSON_INVALID", e.to_string()))?;
            self.0.event_ordered(event).await
        })
    }
}
