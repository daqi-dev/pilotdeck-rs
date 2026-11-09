use super::*;
#[test]
fn default_api_origin_has_exactly_one_api_prefix() {
    for path in [
        "/api/apps.connections.open",
        "/api/auth.test",
        "/api/chat.postMessage",
    ] {
        assert_eq!(
            endpoint_url(&DEFAULT_BASE_URL.into(), path),
            format!("https://slack.com{path}")
        );
        assert!(!endpoint_url(&DEFAULT_BASE_URL.into(), path).contains("/api/api/"));
    }
}
