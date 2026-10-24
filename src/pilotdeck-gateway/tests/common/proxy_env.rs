//! The environment keys the gateway's proxy configuration reads. This mirrors
//! the inline literal in `ProxyConfig::from_settings_lookup`
//! (`src/runtime/model/http.rs`): when production starts reading another
//! variable, this list must follow, or child processes would silently inherit
//! the host's proxy settings.

pub const PROXY_ENV_KEYS: [&str; 7] = [
    "PILOTDECK_PROXY",
    "https_proxy",
    "HTTPS_PROXY",
    "http_proxy",
    "HTTP_PROXY",
    "no_proxy",
    "NO_PROXY",
];
