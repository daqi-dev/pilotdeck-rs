//! `getURLMarkdownContent` (baseline `tool/builtin/web/urlFetcher.ts`): URL
//! validation, the cache, the HTTP→HTTPS upgrade, manual redirect handling
//! (same-host hops followed, up to 10; a cross-host target is returned to the
//! caller), the egress-proxy refusal, the 10 MB body cap, error bodies as a
//! whitespace-collapsed preview, and the content conversion by type.
//!
//! HTML→markdown is the npm `turndown` package in the baseline, which parses
//! HTML with the full WHATWG parser `@mixmark-io/domino`. Neither is ported
//! yet: conversion goes through [`HtmlToMarkdown`], and the default
//! [`MissingTurndown`] fails exactly as the baseline does when `turndown`
//! cannot be loaded. A real converter plugs in through the same trait, so
//! callers do not change.
use super::{
    cache::{FetchedEntry, UrlCache},
    network::{BodyError, FetchOptions, HttpRequest, Random, WEB_RETRY, network_fetch, read_body},
    url_validation::{is_permitted_redirect, parse_with_base, upgrade_http_to_https, validate_url},
};
use crate::runtime::{
    model::http::{ModelHttpTransport, NetworkFetchError},
    tools::ToolRunContext,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{
    sync::{Arc, LazyLock},
    time::Duration,
};

pub const MAX_HTTP_CONTENT_LENGTH: usize = 10 * 1024 * 1024;
pub const FETCH_TIMEOUT_MS: f64 = 60_000.0;
pub const MAX_REDIRECTS: usize = 10;
pub const MAX_MARKDOWN_LENGTH: usize = 100_000;
pub const WEB_FETCH_USER_AGENT: &str = "PilotDeck/0.1 (+https://github.com/pilotdeck) WebFetch";

/// What `getTurndown()` rejects with when the `turndown` package cannot be
/// resolved (Node 22 `ERR_MODULE_NOT_FOUND`, `probes/missing.out`). The path
/// is where the baseline module would live; in TS it is the install path.
pub const TURNDOWN_MISSING: &str =
    "Cannot find package 'turndown' imported from /pilotdeck/src/tool/builtin/web/urlFetcher.ts";

/// `new TurndownService().turndown(html)`; `Err` is the thrown message.
pub trait HtmlToMarkdown: Send + Sync {
    fn convert(&self, html: &str) -> Result<String, String>;
}

/// The baseline with `turndown` unavailable: every conversion rejects.
#[derive(Clone, Copy, Debug, Default)]
pub struct MissingTurndown;

impl HtmlToMarkdown for MissingTurndown {
    fn convert(&self, _html: &str) -> Result<String, String> {
        Err(TURNDOWN_MISSING.to_string())
    }
}

/// `WebFetchHttpError`.
#[derive(Clone, Debug, PartialEq)]
pub struct WebFetchHttpError {
    pub url: String,
    pub status: u16,
    pub status_text: String,
    pub content_type: String,
    pub retry_after_ms: Option<f64>,
    pub body_preview: Option<JsString>,
}

/// What `getURLMarkdownContent` throws.
#[derive(Clone, Debug, PartialEq)]
pub enum FetchError {
    Http(WebFetchHttpError),
    /// Any other `Error`; the message only.
    Other(JsString),
}

impl From<NetworkFetchError> for FetchError {
    fn from(error: NetworkFetchError) -> Self {
        Self::Other(error.message)
    }
}

fn other(message: &str) -> FetchError {
    FetchError::Other(JsString::from(message))
}

#[derive(Clone, Debug, PartialEq)]
pub enum FetchResult {
    Fetched {
        entry: FetchedEntry,
        from_cache: bool,
    },
    /// `RedirectInfo`: a hop that `isPermittedRedirect` refused.
    Redirect {
        original_url: String,
        redirect_url: String,
        status_code: u16,
    },
}

/// The process-wide `URL_CACHE`.
pub fn shared_cache() -> Arc<UrlCache> {
    static CACHE: LazyLock<Arc<UrlCache>> = LazyLock::new(|| Arc::new(UrlCache::default()));
    Arc::clone(&CACHE)
}

#[derive(Clone)]
pub struct UrlFetcher {
    pub transport: Arc<dyn ModelHttpTransport>,
    pub cache: Arc<UrlCache>,
    pub html: Arc<dyn HtmlToMarkdown>,
    pub random: Random,
}

struct RawResponse {
    status: u16,
    status_text: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

enum Hop {
    Done(RawResponse),
    Redirect {
        original_url: String,
        redirect_url: String,
        status_code: u16,
    },
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}

/// `isBinaryContentType(contentType)`.
pub fn is_binary_content_type(content_type: &str) -> bool {
    let lower = content_type.to_lowercase();
    if lower.contains("text/")
        || lower.contains("application/json")
        || lower.contains("application/xml")
        || lower.contains("application/javascript")
        || lower.contains("application/x-www-form-urlencoded")
        || (lower.contains("xml") && !lower.contains("octet"))
    {
        return false;
    }
    lower.contains("application/")
        || lower.contains("image/")
        || lower.contains("audio/")
        || lower.contains("video/")
}

fn js_space(unit: u16) -> bool {
    char::from_u32(u32::from(unit)).is_some_and(crate::runtime::tools::js_space)
}

/// `buildBodyPreview(buffer, contentType)`, on UTF-16 units so the 497-unit
/// cut keeps a split surrogate pair exactly as `String#slice` does.
fn body_preview(body: &[u8], content_type: &str) -> Option<JsString> {
    if body.is_empty() {
        return None;
    }
    if is_binary_content_type(content_type) {
        let shown = if content_type.is_empty() {
            "application/octet-stream"
        } else {
            content_type
        };
        return Some(JsString::from(
            format!(
                "[Binary {shown} response body ({} bytes) omitted]",
                body.len()
            )
            .as_str(),
        ));
    }
    let text = String::from_utf8_lossy(body);
    let mut collapsed: Vec<u16> = Vec::new();
    let mut in_space = false;
    for unit in text.encode_utf16().filter(|unit| *unit != 0) {
        if js_space(unit) {
            if !in_space {
                collapsed.push(u16::from(b' '));
            }
            in_space = true;
        } else {
            collapsed.push(unit);
            in_space = false;
        }
    }
    let trimmed = trim_units(&collapsed, true, true);
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.len() > 500 {
        let mut cut = trim_units(&trimmed[..497], false, true).to_vec();
        cut.extend("...".encode_utf16());
        return Some(JsString::from_utf16(cut));
    }
    Some(JsString::from_utf16(trimmed.to_vec()))
}

fn trim_units(units: &[u16], start: bool, end: bool) -> &[u16] {
    let mut from = 0;
    let mut to = units.len();
    if start {
        while from < to && js_space(units[from]) {
            from += 1;
        }
    }
    if end {
        while to > from && js_space(units[to - 1]) {
            to -= 1;
        }
    }
    &units[from..to]
}

/// `truncateMarkdown(markdown)` on UTF-16 units.
pub fn truncate_markdown(markdown: &str) -> JsString {
    let units: Vec<u16> = markdown.encode_utf16().collect();
    if units.len() <= MAX_MARKDOWN_LENGTH {
        return JsString::from_utf16(units);
    }
    let mut out = units[..MAX_MARKDOWN_LENGTH].to_vec();
    out.extend("\n\n[Content truncated due to length...]".encode_utf16());
    JsString::from_utf16(out)
}

impl UrlFetcher {
    pub fn new(transport: Arc<dyn ModelHttpTransport>) -> Self {
        Self {
            transport,
            cache: shared_cache(),
            html: Arc::new(MissingTurndown),
            random: super::network::default_random(),
        }
    }

    /// One `fetchWithRedirects` hop.
    async fn hop(
        &self,
        url: &str,
        parent: Option<&ToolRunContext>,
    ) -> Result<HttpOutcome, FetchError> {
        let request = HttpRequest {
            method: "GET",
            url: url.to_string(),
            headers: vec![
                ("Accept".into(), "text/markdown, text/html, */*".into()),
                ("User-Agent".into(), WEB_FETCH_USER_AGENT.into()),
            ],
            body: None,
        };
        let options = FetchOptions {
            timeout_ms: FETCH_TIMEOUT_MS,
            parent,
            retry: WEB_RETRY,
            random: Arc::clone(&self.random),
        };
        // The hop's own 60 s timer aborts the request as a parent signal, and
        // it is armed before `networkFetch`'s equal timeout, so it wins.
        let response = tokio::select! {
            biased;
            () = tokio::time::sleep(Duration::from_secs_f64(FETCH_TIMEOUT_MS / 1000.0)) => {
                return Err(other("Network request aborted by parent signal."));
            }
            response = network_fetch(self.transport.as_ref(), &request, &options) => response?,
        };
        Ok(HttpOutcome(response))
    }

    async fn fetch_with_redirects(
        &self,
        start: &str,
        parent: Option<&ToolRunContext>,
    ) -> Result<Hop, FetchError> {
        let mut url = start.to_string();
        for _depth in 0..=MAX_REDIRECTS {
            let HttpOutcome(mut response) = self.hop(&url, parent).await?;
            let status = response.status;
            if matches!(status, 301 | 302 | 307 | 308) {
                // `if (!location)`: an empty header value is missing too.
                let Some(location) = response
                    .header("location")
                    .filter(|location| !location.is_empty())
                    .map(str::to_string)
                else {
                    return Err(other("Redirect missing Location header"));
                };
                let redirect_url = parse_with_base(&location, &url)
                    .ok_or_else(|| other("Invalid URL"))?
                    .to_string();
                if is_permitted_redirect(&url, &redirect_url) {
                    url = redirect_url;
                    continue;
                }
                return Ok(Hop::Redirect {
                    original_url: url,
                    redirect_url,
                    status_code: status,
                });
            }
            if status == 403 && response.header("x-proxy-error") == Some("blocked-by-allowlist") {
                let host = super::url_validation::parse(&url)
                    .map(|parsed| super::url_validation::hostname(&parsed))
                    .unwrap_or_default();
                let mut payload = JsObject::new();
                payload.insert("error_type".into(), JsValue::from("EGRESS_BLOCKED"));
                payload.insert("domain".into(), JsValue::from(host.as_str()));
                payload.insert(
                    "message".into(),
                    JsValue::from(
                        format!("Access to {host} is blocked by the network egress proxy.")
                            .as_str(),
                    ),
                );
                let text = crate::js_value::json_stringify(&JsValue::Object(payload))
                    .ok()
                    .flatten()
                    .unwrap_or_default();
                return Err(other(&text));
            }
            let body = match read_body(&mut response.body, MAX_HTTP_CONTENT_LENGTH, parent).await {
                Ok(body) => body,
                Err(BodyError::Terminated) => return Err(other("terminated")),
                // The parent signal fired mid-body; the baseline's message for
                // an aborted `networkFetch` (observed before the headers) is
                // kept so both abort points look the same.
                Err(BodyError::Aborted) => {
                    return Err(other("Network request aborted by parent signal."));
                }
                Err(BodyError::TooLarge) => {
                    return Err(other(&format!(
                        "Response exceeds maximum content length of {MAX_HTTP_CONTENT_LENGTH} bytes"
                    )));
                }
            };
            return Ok(Hop::Done(RawResponse {
                status,
                status_text: std::mem::take(&mut response.status_text),
                headers: std::mem::take(&mut response.headers),
                body,
            }));
        }
        Err(other(&format!(
            "Too many redirects (exceeded {MAX_REDIRECTS})"
        )))
    }

    /// `getURLMarkdownContent(url, signal, { cache })`.
    pub async fn get_url_markdown_content(
        &self,
        url: &str,
        parent: Option<&ToolRunContext>,
    ) -> Result<FetchResult, FetchError> {
        if !validate_url(url) {
            return Err(other("Invalid URL"));
        }
        if let Some(entry) = self.cache.get(url) {
            return Ok(FetchResult::Fetched {
                entry,
                from_cache: true,
            });
        }
        let upgraded = upgrade_http_to_https(url).ok_or_else(|| other("Invalid URL"))?;
        let raw = match self.fetch_with_redirects(&upgraded, parent).await? {
            Hop::Redirect {
                original_url,
                redirect_url,
                status_code,
            } => {
                return Ok(FetchResult::Redirect {
                    original_url,
                    redirect_url,
                    status_code,
                });
            }
            Hop::Done(raw) => raw,
        };
        let content_type = header(&raw.headers, "content-type")
            .unwrap_or("")
            .to_string();
        let bytes = raw.body.len();
        if !(200..300).contains(&raw.status) {
            return Err(FetchError::Http(WebFetchHttpError {
                url: upgraded,
                status: raw.status,
                status_text: raw.status_text,
                retry_after_ms: crate::runtime::model::http::retry_after_header_ms(header(
                    &raw.headers,
                    "retry-after",
                )),
                body_preview: body_preview(&raw.body, &content_type),
                content_type,
            }));
        }
        let (content, content_bytes) = if content_type.contains("text/html") {
            let html = String::from_utf8_lossy(&raw.body);
            let content = self
                .html
                .convert(&html)
                .map_err(|message| FetchError::Other(JsString::from(message.as_str())))?;
            let length = content.len();
            (content, length)
        } else if is_binary_content_type(&content_type) {
            let shown = if content_type.is_empty() {
                "application/octet-stream"
            } else {
                &content_type
            };
            let content = format!("[Binary {shown} content ({bytes} bytes) — not displayed]");
            let length = content.len();
            (content, length)
        } else {
            (String::from_utf8_lossy(&raw.body).into_owned(), bytes)
        };
        let entry = FetchedEntry {
            bytes,
            code: raw.status,
            code_text: raw.status_text,
            content,
            content_type,
        };
        self.cache.set(url, entry.clone(), content_bytes);
        Ok(FetchResult::Fetched {
            entry,
            from_cache: false,
        })
    }
}

struct HttpOutcome(super::network::HttpResponse);
