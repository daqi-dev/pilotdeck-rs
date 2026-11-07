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
#[derive(Debug)]
pub(super) enum IlinkFailure {
    Abort,
    Error(String),
}
pub(super) trait Fetch {
    fn json(&mut self, method: &str, url: &str, body: Option<&str>) -> Result<Response, String>;
    fn ilink_json(&mut self, url: &str, _poll: bool) -> Result<Response, IlinkFailure> {
        self.json("GET", url, None).map_err(IlinkFailure::Error)
    }
    fn success_json(&mut self, url: &str) -> Result<JsValue, String> {
        let response = self.json("GET", url, None)?;
        if !(200..300).contains(&response.status) {
            return Err(format!("HTTP {}", response.status));
        }
        Ok(response.json)
    }
    fn now_ms(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX)
    }
    fn sleep(&mut self, milliseconds: u64) {
        std::thread::sleep(Duration::from_millis(milliseconds));
    }
}
pub(super) struct Network {
    poll_timeout: Duration,
    ilink_body_timeout: Duration,
    ilink_body_limit: usize,
}
impl Network {
    pub fn new() -> Self {
        Self {
            poll_timeout: Duration::from_secs(35),
            ilink_body_timeout: Duration::from_secs(30),
            ilink_body_limit: 1024 * 1024,
        }
    }
    #[cfg(test)]
    pub(super) fn with_poll_timeout(poll_timeout: Duration) -> Self {
        Self {
            poll_timeout,
            ..Self::new()
        }
    }
    #[cfg(test)]
    pub(super) fn with_ilink_body_limits(limit: usize, timeout: Duration) -> Self {
        Self {
            ilink_body_limit: limit,
            ilink_body_timeout: timeout,
            ..Self::new()
        }
    }
}
impl Fetch for Network {
    fn ilink_json(&mut self, url: &str, poll: bool) -> Result<Response, IlinkFailure> {
        self.exchange("GET", url, None, false, Some(poll))
            .map_err(|error| {
                if poll && error == "This operation was aborted" {
                    IlinkFailure::Abort
                } else {
                    IlinkFailure::Error(error)
                }
            })
    }

    fn json(&mut self, method: &str, url: &str, body: Option<&str>) -> Result<Response, String> {
        self.exchange(method, url, body, false, None)
    }
    fn success_json(&mut self, url: &str) -> Result<JsValue, String> {
        self.exchange("GET", url, None, true, None)
            .map(|response| response.json)
    }
}
impl Network {
    fn exchange(
        &mut self,
        method: &str,
        url: &str,
        body: Option<&str>,
        require_success: bool,
        ilink: Option<bool>,
    ) -> Result<Response, String> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?;
        runtime.block_on(async {
            let client = HyperModelTransport::new();
            let mut url = url::Url::parse(url).map_err(|_| "fetch failed".to_owned())?;
            let mut method = method.to_owned();
            let mut body = body.map(str::to_owned);
            let poll_deadline =
                (ilink == Some(true)).then(|| tokio::time::Instant::now() + self.poll_timeout);
            for redirects in 0..=20 {
                let mut request = Request::builder()
                    .method(method.as_str())
                    .uri(url.as_str())
                    .header(
                        "User-Agent",
                        if require_success {
                            "PilotDeck/1.0"
                        } else {
                            "node"
                        },
                    );
                if ilink == Some(true) {
                    request = request.header("iLink-App-ClientVersion", "1");
                }
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
                let response = tokio::time::timeout_at(
                    poll_deadline
                        .unwrap_or_else(|| tokio::time::Instant::now() + Duration::from_secs(300)),
                    client.send(request),
                )
                .await
                .map_err(|_| {
                    if ilink == Some(true) {
                        "This operation was aborted".to_owned()
                    } else {
                        "fetch failed".to_owned()
                    }
                })?
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
                if require_success && !(200..300).contains(&status) {
                    return Err(format!("HTTP {status}"));
                }
                if ilink.is_some() && !(200..300).contains(&status) {
                    return Ok(Response {
                        status,
                        json: JsValue::Undefined,
                    });
                }
                let mut incoming = response.into_body();
                let mut bytes = Vec::new();
                // iLink alone has a total body deadline and allocation bound.
                // Its SDK polling abort still ends when headers arrive.
                let body_deadline = ilink
                    .is_some()
                    .then(|| tokio::time::Instant::now() + self.ilink_body_timeout);
                loop {
                    let frame = tokio::time::timeout_at(
                        body_deadline.unwrap_or_else(|| {
                            tokio::time::Instant::now() + Duration::from_secs(300)
                        }),
                        incoming.frame(),
                    )
                    .await
                    .map_err(|_| {
                        if ilink.is_some() {
                            "iLink response body exceeded time limit".to_owned()
                        } else {
                            "terminated".to_owned()
                        }
                    })?;
                    let Some(frame) = frame else { break };
                    let frame = frame.map_err(|_| "terminated".to_owned())?;
                    if let Ok(data) = frame.into_data() {
                        if ilink.is_some()
                            && data.len() > self.ilink_body_limit.saturating_sub(bytes.len())
                        {
                            return Err(format!(
                                "iLink response exceeds {}-byte limit",
                                self.ilink_body_limit
                            ));
                        }
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
