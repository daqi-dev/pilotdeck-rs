//! Golden replay of the webhook channel fixture plus lifecycle and direct
//! tests. The full suite lands with the fixture; the placeholder keeps the
//! module tree compiling while the generator is built.
use super::*;

#[test]
fn placeholder() {
    let channel = WebhookChannel::new(WebhookChannelOptions::default());
    assert_eq!(channel.channel_key().to_string(), "webhook");
}
