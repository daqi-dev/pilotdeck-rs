//! `getURLMarkdownContent` (baseline `tool/builtin/web/urlFetcher.ts`): URL
//! validation, the cache, the HTTP→HTTPS upgrade, manual redirect handling
//! (same-host hops followed, up to 10; a cross-host target is returned to the
//! caller), the egress-proxy refusal, the 10 MB body cap, error bodies as a
//! whitespace-collapsed preview, and the content conversion by type.
//!
//! HTML→markdown uses the baseline's default Turndown rules through
//! [`HtmlToMarkdown`]. [`MissingTurndown`] remains an injectable failure for
//! replaying installations where the baseline cannot load the npm package.
use super::{
    cache::{FetchedEntry, UrlCache},
    html_markdown::Turndown,
    html_memory,
    network::{
        BodyError, FetchOptions, HttpRequest, Random, WEB_RETRY,
        network_fetch_with_parent_deadline, node_timer_duration,
    },
    url_validation::{is_permitted_redirect, parse_with_base, upgrade_http_to_https, validate_url},
};
use crate::runtime::{
    model::http::{ModelHttpTransport, NetworkFetchError},
    tools::ToolRunContext,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::sync::{Arc, LazyLock};

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

/// `TurndownService.turndown(html)` result. `text` is the complete trimmed
/// Markdown, or (when `complete` is false) a prefix of it with at least the
/// caller's requested UTF-16 unit count. `utf16_length`/`byte_length` always
/// describe the complete trimmed Markdown, which keeps `rawLength` and the
/// 50 MiB cache budget identical to the baseline.
#[derive(Clone, Debug)]
pub struct MarkdownOutput {
    pub text: String,
    pub utf16_length: usize,
    pub byte_length: usize,
    pub complete: bool,
}

/// `new TurndownService().turndown(html)`; `Err` is the thrown message.
pub trait HtmlToMarkdown: Send + Sync {
    fn convert(&self, html: &str) -> Result<String, String>;

    /// Convert only enough of the result to cover `prefix_units` UTF-16 code
    /// units. The default keeps custom converters exact by converting fully.
    fn convert_bounded(&self, html: &str, _prefix_units: usize) -> Result<MarkdownOutput, String> {
        let text = self.convert(html)?;
        Ok(MarkdownOutput {
            utf16_length: text.encode_utf16().count(),
            byte_length: text.len(),
            text,
            complete: true,
        })
    }
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

/// Raw-mode `truncated`/`rawLength`/`returnedLength` for a fetched entry.
/// HTML entries carry the full trimmed length because `content` is a prefix.
pub fn truncate_entry(entry: &FetchedEntry) -> (JsString, usize, bool) {
    let raw_length = entry
        .raw_markdown_length
        .unwrap_or_else(|| entry.content.encode_utf16().count());
    (
        truncate_markdown(&entry.content),
        raw_length,
        raw_length > MAX_MARKDOWN_LENGTH,
    )
}

impl UrlFetcher {
    pub fn new(transport: Arc<dyn ModelHttpTransport>) -> Self {
        Self {
            transport,
            cache: shared_cache(),
            html: Arc::new(Turndown),
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
        let deadline = tokio::time::Instant::now() + node_timer_duration(FETCH_TIMEOUT_MS);
        let response = network_fetch_with_parent_deadline(
            self.transport.as_ref(),
            &request,
            &options,
            deadline,
        )
        .await?;
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
            let body = match response.read_body(MAX_HTTP_CONTENT_LENGTH, parent).await {
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
        let (content, content_bytes, raw_markdown_length) = if content_type.contains("text/html") {
            let html = String::from_utf8_lossy(&raw.body).into_owned();
            let converter = self.html.clone();
            // TS runs turndown synchronously after the body and ignores an
            // abort delivered during conversion (including raw-mode success).
            // Preserve that result ordering, but keep CPU work off Tokio.
            // Reserve estimated HTML memory before entering the unchanged
            // model CPU pool's four permits. Replacements
            // materialize at most the truncation prefix plus the escaped input
            // text, so #409's depth×content amplification does not build the
            // full Markdown or its UTF-16 copy. Parsing itself remains bounded
            // by the 10 MiB body cap and the existing 256-level depth guard.
            let markdown = html_memory::run(html.len(), move || {
                Ok(converter.convert_bounded(&html, MAX_MARKDOWN_LENGTH + 1))
            })
            .await
            .map_err(|error| other(&error.message))?
            .map_err(|message| other(&message))?;
            (
                markdown.text,
                markdown.byte_length,
                Some(markdown.utf16_length),
            )
        } else if is_binary_content_type(&content_type) {
            let shown = if content_type.is_empty() {
                "application/octet-stream"
            } else {
                &content_type
            };
            let content = format!("[Binary {shown} content ({bytes} bytes) — not displayed]");
            let length = content.len();
            (content, length, None)
        } else {
            (String::from_utf8_lossy(&raw.body).into_owned(), bytes, None)
        };
        let entry = FetchedEntry {
            bytes,
            code: raw.status,
            code_text: raw.status_text,
            content,
            content_type,
            raw_markdown_length,
        };
        self.cache.set(url, entry.clone(), content_bytes);
        Ok(FetchResult::Fetched {
            entry,
            from_cache: false,
        })
    }
}

struct HttpOutcome(super::network::HttpResponse);
