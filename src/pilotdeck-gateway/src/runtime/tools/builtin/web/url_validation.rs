//! URL hygiene for `web_fetch` (baseline `tool/builtin/web/urlValidation.ts`).
//!
//! `new URL(...)` is the WHATWG URL parser, which the `url` crate implements;
//! JS strings reach it as USVStrings (lone surrogates become U+FFFD), so the
//! callers pass the lossy UTF-8 form. Note the baseline's "non-public
//! hostname" check is only "at least two non-empty dot-separated labels":
//! `localhost` is rejected, but `127.0.0.1` or `10.0.0.1` are not, and no
//! private-network or loopback address is blocked. This port keeps that rule.
use url::Url;

/// W1: maximum URL length in UTF-16 units.
pub const MAX_URL_LENGTH: usize = 2000;

/// `new URL(url)`, `None` where it throws.
pub fn parse(url: &str) -> Option<Url> {
    Url::parse(url).ok()
}

/// `new URL(input, base)`, `None` where it throws (`TypeError: Invalid URL`).
pub fn parse_with_base(input: &str, base: &str) -> Option<Url> {
    Url::parse(base).ok()?.join(input).ok()
}

/// WHATWG `url.hostname` (IPv6 keeps its brackets; no host is "").
pub fn hostname(url: &Url) -> String {
    url.host_str().unwrap_or("").to_string()
}

/// `validateURL(url)` (W2).
pub fn validate_url(url: &str) -> bool {
    if url.encode_utf16().count() > MAX_URL_LENGTH {
        return false;
    }
    let Some(parsed) = parse(url) else {
        return false;
    };
    if !parsed.username().is_empty() || parsed.password().is_some_and(|p| !p.is_empty()) {
        return false;
    }
    hostname(&parsed)
        .split('.')
        .filter(|part| !part.is_empty())
        .count()
        >= 2
}

/// `upgradeHttpToHttps(url).upgraded` (W3). `None` where `new URL` throws.
/// Changing the scheme drops a port that becomes the new default.
pub fn upgrade_http_to_https(url: &str) -> Option<String> {
    let mut parsed = parse(url)?;
    if parsed.scheme() == "http" {
        let port = parsed.port();
        parsed
            .set_scheme("https")
            .expect("http and https are both special schemes");
        if port == Some(443) {
            let _ = parsed.set_port(None);
        }
    }
    Some(parsed.to_string())
}

/// `isPermittedRedirect(originalUrl, redirectUrl)` (W7): same scheme and
/// port, no credentials, hostnames equal after one leading `www.` is
/// stripped from each.
pub fn is_permitted_redirect(original: &str, redirect: &str) -> bool {
    let (Some(original), Some(redirect)) = (parse(original), parse(redirect)) else {
        return false;
    };
    if redirect.scheme() != original.scheme() || redirect.port() != original.port() {
        return false;
    }
    if !redirect.username().is_empty() || redirect.password().is_some_and(|p| !p.is_empty()) {
        return false;
    }
    let strip = |host: String| match host.strip_prefix("www.") {
        Some(rest) => rest.to_string(),
        None => host,
    };
    strip(hostname(&original)) == strip(hostname(&redirect))
}
