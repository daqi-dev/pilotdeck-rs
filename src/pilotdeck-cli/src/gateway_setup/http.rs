use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::Request;
use pilotdeck_gateway::runtime::model::http::{HyperModelTransport, ModelHttpTransport};
use pilotdeck_protocol::{JsValue, json_syntax, parse_json};
use std::time::Duration;

pub(super) struct Response {
    pub status: u16,
    pub json: JsValue,
}
pub(super) trait Fetch {
    fn json(&mut self, method: &str, url: &str, body: Option<&str>) -> Result<Response, String>;
}
pub(super) struct Network;
impl Network {
    pub fn new() -> Self {
        Self
    }
}
impl Fetch for Network {
    fn json(&mut self, method: &str, url: &str, body: Option<&str>) -> Result<Response, String> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;
        runtime.block_on(async {
            let client = HyperModelTransport::new();
            let mut url = url::Url::parse(url).map_err(|_| "fetch failed".to_owned())?;
            let mut method = method.to_owned();
            let mut body = body.map(str::to_owned);
            for redirects in 0..=20 {
                let mut request = Request::builder()
                    .method(method.as_str())
                    .uri(url.as_str())
                    .header("User-Agent", "node");
                if body.is_some() {
                    request = request.header("Content-Type", "application/json; charset=utf-8");
                }
                let request = request
                    .body(Full::new(Bytes::copy_from_slice(
                        body.as_deref().unwrap_or("").as_bytes(),
                    )))
                    .map_err(|_| "fetch failed".to_owned())?;
                // Node fetch's headers deadline is distinct from its body
                // inactivity deadline; receiving a chunk renews the latter.
                let response = tokio::time::timeout(Duration::from_secs(300), client.send(request))
                    .await
                    .map_err(|_| "fetch failed".to_owned())?
                    .map_err(|_| "fetch failed".to_owned())?;
                let status = response.status().as_u16();
                if matches!(status, 301 | 302 | 303 | 307 | 308)
                    && let Some(location) = response.headers().get("location")
                {
                    if redirects == 20 {
                        return Err("fetch failed".into());
                    }
                    let location = location.to_str().map_err(|_| "fetch failed".to_owned())?;
                    let next = url.join(location).map_err(|_| "fetch failed".to_owned())?;
                    if !matches!(next.scheme(), "http" | "https")
                        || !next.username().is_empty()
                        || next.password().is_some()
                    {
                        return Err("fetch failed".into());
                    }
                    if (matches!(status, 301 | 302) && method == "POST")
                        || (status == 303 && method != "GET" && method != "HEAD")
                    {
                        method = "GET".into();
                        body = None;
                    }
                    credential_redirect(&url, &next, body.is_some())?;
                    url = next;
                    continue;
                }
                let mut incoming = response.into_body();
                let mut bytes = Vec::new();
                loop {
                    let frame = tokio::time::timeout(Duration::from_secs(300), incoming.frame())
                        .await
                        .map_err(|_| "terminated".to_owned())?;
                    let Some(frame) = frame else { break };
                    let frame = frame.map_err(|_| "terminated".to_owned())?;
                    if let Ok(data) = frame.into_data() {
                        bytes.extend_from_slice(&data);
                    }
                }
                let raw = String::from_utf8_lossy(&bytes);
                let raw = raw.strip_prefix('\u{feff}').unwrap_or(&raw);
                let json = parse_json(raw)
                    .map_err(|error| json_syntax::parse_error(raw).unwrap_or(error.message))?;
                return Ok(Response { status, json });
            }
            Err("fetch failed".into())
        })
    }
}

pub(super) fn credential_redirect(
    current: &url::Url,
    next: &url::Url,
    has_body: bool,
) -> Result<(), String> {
    if (current.scheme() == "https" && next.scheme() == "http")
        || (has_body && current.origin() != next.origin())
    {
        Err("Refusing unsafe credential redirect".into())
    } else {
        Ok(())
    }
}
