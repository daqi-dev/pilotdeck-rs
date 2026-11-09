//! SDK callback HTTP hosting (`packages/sdk/src/hook-server.ts`).
//!
//! The host owns the serializable HTTP boundary. Bindings can inject a matcher
//! handler or use the ordered native callback adapter.
mod callbacks;
pub use callbacks::{
    HostedHookCallback, HostedHookCallbackEvents, HostedHookCallbackMatcher, HostedHookCallbacks,
};

use super::exec::Cancellation;
use http_body_util::{BodyExt, Full};
use hyper::{Request, Response, StatusCode, body::Incoming, service::service_fn};
use hyper_util::rt::TokioIo;
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use std::{
    future::Future,
    net::{IpAddr, SocketAddr},
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{net::TcpListener, task::JoinHandle};

const DEFAULT_PATH: &str = "/pilotdeck-sdk-hooks";
const MAX_BODY_BYTES: usize = 1_000_000;
const NATIVE_EVENTS: &[&str] = &[
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "UserPromptSubmit",
    "PreModelRequest",
    "SessionStart",
    "SessionEnd",
    "Stop",
    "StopFailure",
    "SubagentStart",
    "SubagentStop",
    "PreCompact",
    "PostCompact",
    "PermissionRequest",
    "PermissionDenied",
    "Setup",
    "ConfigChange",
    "InstructionsLoaded",
    "FileChanged",
    "Elicitation",
    "ElicitationResult",
];

#[derive(Clone, Debug, PartialEq)]
pub struct HostedHookMatcher {
    pub matcher: Option<JsString>,
    pub timeout_seconds: Option<f64>,
}

pub type HostedHookEvents = Vec<(JsString, Vec<HostedHookMatcher>)>;

#[derive(Clone, Debug, PartialEq)]
pub struct HostedHookConfig {
    pub url: String,
    pub authorization: String,
    pub events: HostedHookEvents,
}

impl HostedHookConfig {
    pub fn to_value(&self) -> JsValue {
        let mut headers = JsObject::new();
        headers.insert("authorization".into(), self.authorization.as_str().into());
        let mut events = JsObject::new();
        for (event, matchers) in &self.events {
            events.insert(
                event.clone(),
                JsValue::Array(
                    matchers
                        .iter()
                        .map(|matcher| {
                            let mut value = JsObject::new();
                            if let Some(matcher) = &matcher.matcher
                                && !matcher.is_empty()
                            {
                                value.insert("matcher".into(), JsValue::String(matcher.clone()));
                            }
                            if let Some(timeout) = matcher.timeout_seconds {
                                value.insert("timeout".into(), JsValue::Number(timeout));
                            }
                            JsValue::Object(value)
                        })
                        .collect(),
                ),
            );
        }
        let mut value = JsObject::new();
        value.insert("url".into(), self.url.as_str().into());
        value.insert("headers".into(), JsValue::Object(headers));
        value.insert("events".into(), JsValue::Object(events));
        JsValue::Object(value)
    }
}

#[derive(Clone, Debug)]
pub struct HostedHookServerOptions {
    pub host: IpAddr,
    pub port: u16,
    pub path: String,
    pub public_url: Option<String>,
}

impl Default for HostedHookServerOptions {
    fn default() -> Self {
        Self {
            host: IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            port: 0,
            path: DEFAULT_PATH.into(),
            public_url: None,
        }
    }
}

pub type HostedHookFuture =
    Pin<Box<dyn Future<Output = Result<JsValue, HostedHookError>> + Send + 'static>>;

pub trait HostedHookHandler: Send + Sync {
    /// Ordered callback adapters apply the configured timeout to each callback.
    /// Legacy matcher handlers keep the existing whole-handler timeout.
    fn manages_callback_timeouts(&self) -> bool {
        false
    }
    fn call(
        &self,
        event: JsString,
        matcher: usize,
        input: JsObject,
        signal: Cancellation,
        async_hook_id: JsString,
    ) -> HostedHookFuture;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostedHookError {
    pub code: &'static str,
    pub message: String,
}

impl HostedHookError {
    pub fn validation(message: impl Into<String>) -> Self {
        Self {
            code: "validation_error",
            message: message.into(),
        }
    }
    fn server(message: impl Into<String>) -> Self {
        Self {
            code: "server_error",
            message: message.into(),
        }
    }
}

struct Running {
    config: HostedHookConfig,
    shutdown: Cancellation,
    task: JoinHandle<()>,
    service: Arc<Service>,
}

#[derive(Default)]
struct State {
    running: Option<Running>,
}

struct Service {
    path: String,
    token: String,
    events: HostedHookEvents,
    handler: Arc<dyn HostedHookHandler>,
    active: Mutex<ActiveCalls>,
}

#[derive(Default)]
struct ActiveCalls {
    closing: bool,
    signals: Vec<Cancellation>,
}

struct ActiveCall<'a> {
    service: &'a Service,
    signal: Cancellation,
}
impl Drop for ActiveCall<'_> {
    fn drop(&mut self) {
        // Hyper drops the request future when the peer disconnects or close
        // cancels its connection. Language callbacks observe that cancellation.
        self.signal.abort();
        self.service
            .active
            .lock()
            .unwrap()
            .signals
            .retain(|active| !active.same_channel(&self.signal));
    }
}

pub struct HostedHookServer {
    events: HostedHookEvents,
    handler: Arc<dyn HostedHookHandler>,
    state: tokio::sync::Mutex<State>,
}

impl HostedHookServer {
    pub fn from_callbacks(events: HostedHookCallbackEvents) -> Result<Self, HostedHookError> {
        let callbacks = Arc::new(HostedHookCallbacks::new(events)?);
        Self::new(callbacks.events(), callbacks)
    }

    pub fn new(
        events: HostedHookEvents,
        handler: Arc<dyn HostedHookHandler>,
    ) -> Result<Self, HostedHookError> {
        validate_events(&events)?;
        Ok(Self {
            events,
            handler,
            state: tokio::sync::Mutex::new(State::default()),
        })
    }

    /// Concurrent starts share one listener. A failed start leaves no cached
    /// state, so a later call retries like TS's reset `startPromise`.
    pub async fn start(
        &self,
        options: HostedHookServerOptions,
    ) -> Result<HostedHookConfig, HostedHookError> {
        let mut state = self.state.lock().await;
        if let Some(running) = &state.running {
            return Ok(running.config.clone());
        }
        let path = normalize_path(&options.path)?;
        let public_url = options
            .public_url
            .as_deref()
            .map(normalize_public_url)
            .transpose()?;
        let listener = TcpListener::bind(SocketAddr::new(options.host, options.port))
            .await
            .map_err(|error| {
                HostedHookError::server(format!("Unable to start SDK hook server: {error}"))
            })?;
        let address = listener.local_addr().map_err(|error| {
            HostedHookError::server(format!(
                "SDK hook server did not expose a TCP address: {error}"
            ))
        })?;
        let token = uuid::Uuid::new_v4().to_string();
        let local_url = format!(
            "http://{}:{}{path}",
            format_host(address.ip()),
            address.port()
        );
        let config = HostedHookConfig {
            url: public_url.unwrap_or(local_url),
            authorization: format!("Bearer {token}"),
            events: self.events.clone(),
        };
        let service = Arc::new(Service {
            path,
            token,
            events: self.events.clone(),
            handler: self.handler.clone(),
            active: Mutex::new(ActiveCalls::default()),
        });
        let shutdown = Cancellation::new();
        let task_shutdown = shutdown.clone();
        let owned_service = service.clone();
        let task = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    _ = task_shutdown.cancelled() => break,
                    accepted = listener.accept() => {
                        let Ok((stream, _)) = accepted else { break };
                        let service = service.clone();
                        let connection_shutdown = task_shutdown.clone();
                        connections.spawn(async move {
                            let handler = service_fn(move |request| {
                                let service = service.clone();
                                async move { Ok::<_, std::convert::Infallible>(service.handle(request).await) }
                            });
                            tokio::select! {
                                _ = connection_shutdown.cancelled() => {},
                                _ = hyper::server::conn::http1::Builder::new().serve_connection(TokioIo::new(stream), handler) => {},
                            }
                        });
                    }
                    _ = connections.join_next(), if !connections.is_empty() => {},
                }
            }
            for signal in service.active.lock().unwrap().signals.drain(..) {
                signal.abort();
            }
            while connections.join_next().await.is_some() {}
        });
        state.running = Some(Running {
            config: config.clone(),
            shutdown,
            task,
            service: owned_service,
        });
        Ok(config)
    }

    /// SDK query owners call this when attaching callbacks to a Gateway.
    /// An unreachable loopback callback endpoint is closed before rejection
    /// (`packages/sdk/src/client.ts:1761-1779`).
    pub async fn start_for_gateway(
        &self,
        options: HostedHookServerOptions,
        gateway_url: Option<&str>,
    ) -> Result<HostedHookConfig, HostedHookError> {
        let endpoint = self.start(options).await?;
        let loopback = |value: &str| {
            url::Url::parse(value)
                .ok()
                .and_then(|url| {
                    url.host_str()
                        .map(|host| matches!(host, "localhost" | "127.0.0.1" | "::1" | "[::1]"))
                })
                .unwrap_or(false)
        };
        if loopback(&endpoint.url) && !gateway_url.is_some_and(loopback) {
            self.close().await;
            return Err(HostedHookError { code: "unsupported_capability", message: "SDK hooks are bound to loopback but the configured Gateway is remote. Configure hookServer.publicUrl with an endpoint reachable by that Gateway.".into() });
        }
        Ok(endpoint)
    }

    pub async fn close(&self) {
        let running = self.state.lock().await.running.take();
        if let Some(running) = running {
            {
                let mut active = running.service.active.lock().unwrap();
                active.closing = true;
                for signal in &active.signals {
                    signal.abort();
                }
            }
            running.shutdown.abort();
            let _ = running.task.await;
        }
    }
}

impl Drop for HostedHookServer {
    fn drop(&mut self) {
        // A Rust owner may be dropped outside a Tokio enter guard, where an
        // asynchronous close cannot be spawned. Still cancel every owned
        // callback and listener task; explicit close remains the draining API.
        if let Some(running) = self.state.get_mut().running.take() {
            let mut active = running.service.active.lock().unwrap();
            active.closing = true;
            for signal in &active.signals {
                signal.abort();
            }
            drop(active);
            running.shutdown.abort();
            running.task.abort();
        }
    }
}

impl Service {
    async fn handle(&self, request: Request<Incoming>) -> Response<Full<bytes::Bytes>> {
        if request.method() != hyper::Method::POST || request.uri().path() != self.path {
            return json_error(
                StatusCode::NOT_FOUND,
                "PilotDeck SDK hook endpoint not found.",
                None,
            );
        }
        let authorized = request
            .headers()
            .get(hyper::header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value == format!("Bearer {}", self.token));
        if !authorized {
            return json_error(
                StatusCode::UNAUTHORIZED,
                "Unauthorized PilotDeck SDK hook request.",
                None,
            );
        }
        let selector = selector(request.uri(), &self.events);
        let (event, matcher_index, matcher) = match selector {
            Ok(selector) => selector,
            Err(error) => return json_error(StatusCode::BAD_REQUEST, &error.message, None),
        };
        let signal = Cancellation::new();
        {
            let mut active = self.active.lock().unwrap();
            if active.closing {
                return json_error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "PilotDeck SDK hook server is closing.",
                    None,
                );
            }
            active.signals.push(signal.clone());
        }
        let _active = ActiveCall {
            service: self,
            signal: signal.clone(),
        };
        let input = match read_input(request.into_body()).await {
            Ok(input) => input,
            Err(error) => {
                return json_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &error.message,
                    Some(error.code),
                );
            }
        };
        let async_hook_id: JsString = uuid::Uuid::new_v4().to_string().into();
        let future = self.handler.call(
            event.clone(),
            matcher_index,
            input,
            signal.clone(),
            async_hook_id.clone(),
        );
        let result = if self.handler.manages_callback_timeouts() {
            future.await
        } else if let Some(seconds) = matcher.timeout_seconds {
            if !seconds.is_finite() || seconds <= 0.0 {
                Err(HostedHookError::validation(
                    "Hook matcher timeout must be a positive finite number of seconds.",
                ))
            } else {
                // Node setTimeout clamps overflow/sub-ms delays and truncates
                // fractions. In particular finite seconds may overflow when
                // multiplied, so never construct a Duration from the float.
                let delay = pilotdeck_protocol::timer_delay_ms(seconds * 1_000.0);
                match tokio::time::timeout(Duration::from_millis(delay), future).await {
                    Ok(result) => result,
                    Err(_) => {
                        signal.abort();
                        Err(HostedHookError {
                            code: "timeout",
                            message: format!("SDK hook callback timed out after {seconds}s."),
                        })
                    }
                }
            }
        } else {
            future.await
        };
        match result {
            Ok(output) => match native_output(output, &event, async_hook_id) {
                Ok(output) => json_value(StatusCode::OK, &output),
                Err(error) => json_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    &error.message,
                    Some(error.code),
                ),
            },
            Err(error) => json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                &error.message,
                Some(error.code),
            ),
        }
    }
}

/// The language binding owns callback ordering/merging. The host still owns
/// the shared output boundary (`hook-server.ts:232-267`), including the random
/// deferred id and SDK's `defer` spelling.
fn native_output(
    output: JsValue,
    event: &JsString,
    async_hook_id: JsString,
) -> Result<JsValue, HostedHookError> {
    if output.get("async").and_then(JsValue::as_bool) == Some(true) {
        let timeout = output
            .get("asyncTimeout")
            .filter(|value| !value.is_undefined());
        if timeout.is_some_and(|value| {
            value
                .as_number()
                .is_none_or(|value| !value.is_finite() || value <= 0.0)
        }) {
            return Err(HostedHookError::validation(
                "asyncTimeout must be a positive finite number of seconds.",
            ));
        }
        let mut deferred = JsObject::new();
        deferred.insert("async".into(), JsValue::Bool(true));
        deferred.insert("asyncHookId".into(), JsValue::String(async_hook_id));
        if let Some(timeout) = timeout {
            deferred.insert("asyncTimeout".into(), timeout.clone());
        }
        return Ok(JsValue::Object(deferred));
    }
    let Some(specific) = output.get("hookSpecificOutput").filter(|value| {
        !matches!(
            value,
            JsValue::Null | JsValue::Undefined | JsValue::Bool(false)
        )
    }) else {
        return Ok(output);
    };
    if specific.get("hookEventName").and_then(JsValue::as_string) != Some(event) {
        let actual = specific
            .get("hookEventName")
            .map(|value| {
                String::from_utf16_lossy(
                    &crate::js_value::to_string_units(value).unwrap_or_default(),
                )
            })
            .unwrap_or_else(|| "undefined".into());
        return Err(HostedHookError::validation(format!(
            "Hook {} returned hookSpecificOutput for {actual}.",
            String::from_utf16_lossy(event.units())
        )));
    }
    let mut output = output.as_object().cloned().unwrap_or_default();
    let mut specific = specific.as_object().cloned().unwrap_or_default();
    if specific
        .get("permissionDecision")
        .and_then(JsValue::as_string)
        .is_some_and(|value| value.equals_str("defer"))
    {
        specific.insert("permissionDecision".into(), "passthrough".into());
    }
    output.insert("hookSpecificOutput".into(), JsValue::Object(specific));
    Ok(JsValue::Object(output))
}

fn validate_events(events: &HostedHookEvents) -> Result<(), HostedHookError> {
    for (event, _) in events {
        let supported = NATIVE_EVENTS.iter().any(|native| event.equals_str(native));
        if !supported {
            let name = String::from_utf16_lossy(event.units());
            let detail = match name.as_str() {
                "Notification" => {
                    "PilotDeck has no user-notification lifecycle event; Gateway notifications are infrastructure frames, not hooks."
                }
                "CwdChanged" => {
                    "PilotDeck sessions have a fixed working directory; in-session cwd changes have no native lifecycle event."
                }
                "WorktreeCreate" | "WorktreeRemove" => {
                    "PilotDeck only emits this from the Always-On workspace runtime; it is not attached to a per-query Gateway session."
                }
                _ => "This hook has no native PilotDeck lifecycle event.",
            };
            return Err(HostedHookError {
                code: "unsupported_capability",
                message: format!("hooks.{name} is unsupported: {detail}"),
            });
        }
    }
    Ok(())
}

fn normalize_path(path: &str) -> Result<String, HostedHookError> {
    let path = if path.is_empty() { DEFAULT_PATH } else { path };
    if !path.starts_with('/') {
        return Err(HostedHookError::validation(
            "Hook endpoint path must begin with '/'.",
        ));
    }
    Ok(path.into())
}

fn normalize_public_url(value: &str) -> Result<String, HostedHookError> {
    let url = url::Url::parse(value).map_err(|_| {
        HostedHookError::validation("Hook publicUrl must be an absolute HTTP(S) endpoint URL.")
    })?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(HostedHookError::validation(
            "Hook publicUrl must use http: or https:.",
        ));
    }
    Ok(url.to_string())
}

fn format_host(host: IpAddr) -> String {
    match host {
        IpAddr::V4(host) => host.to_string(),
        IpAddr::V6(host) => format!("[{host}]"),
    }
}

fn selector<'a>(
    uri: &hyper::Uri,
    events: &'a HostedHookEvents,
) -> Result<(JsString, usize, &'a HostedHookMatcher), HostedHookError> {
    let url = url::Url::parse(&format!("http://localhost{uri}"))
        .map_err(|_| HostedHookError::validation("Invalid SDK hook endpoint selector."))?;
    let event = url
        .query_pairs()
        .find(|(key, _)| key == "event")
        .map(|(_, value)| JsString::from(value.as_ref()));
    let matcher = url
        .query_pairs()
        .find(|(key, _)| key == "matcher")
        .and_then(|(_, value)| value.parse::<usize>().ok());
    let (event, matcher) = event
        .zip(matcher)
        .ok_or_else(|| HostedHookError::validation("Invalid SDK hook endpoint selector."))?;
    let matchers = events
        .iter()
        .find(|(candidate, _)| candidate == &event)
        .map(|(_, matchers)| matchers)
        .ok_or_else(|| HostedHookError::validation("Invalid SDK hook endpoint selector."))?;
    let selected = matchers
        .get(matcher)
        .ok_or_else(|| HostedHookError::validation("Invalid SDK hook endpoint selector."))?;
    Ok((event, matcher, selected))
}

async fn read_input(mut body: Incoming) -> Result<JsObject, HostedHookError> {
    let mut bytes = Vec::new();
    while let Some(frame) = body.frame().await {
        let frame = frame
            .map_err(|_| HostedHookError::validation("SDK hook payload is not valid JSON."))?;
        if let Ok(data) = frame.into_data() {
            if data.len() > MAX_BODY_BYTES - bytes.len() {
                return Err(HostedHookError::validation("SDK hook payload exceeds 1MB."));
            }
            bytes.extend_from_slice(&data);
        }
    }
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| HostedHookError::validation("SDK hook payload is not valid JSON."))?;
    let value = parse_json(text)
        .map_err(|_| HostedHookError::validation("SDK hook payload is not valid JSON."))?;
    let input = value
        .as_object()
        .ok_or_else(|| HostedHookError::validation("SDK hook payload must be an object."))?;
    let mut snake = JsObject::new();
    for (key, value) in input.entries() {
        snake.insert(camel_to_snake(key), value.clone());
    }
    let hook_event = snake
        .get("hook_event_name")
        .and_then(JsValue::as_string)
        .filter(|event| NATIVE_EVENTS.iter().any(|native| event.equals_str(native)));
    if hook_event.is_none() {
        return Err(HostedHookError::validation(
            "SDK hook payload has no supported native hook_event_name.",
        ));
    }
    if ["session_id", "transcript_path", "cwd"]
        .iter()
        .any(|key| snake.get(key).and_then(JsValue::as_string).is_none())
    {
        return Err(HostedHookError::validation(
            "SDK hook payload is missing session_id, transcript_path, or cwd.",
        ));
    }
    Ok(snake)
}

fn camel_to_snake(value: &JsString) -> JsString {
    let mut units = Vec::with_capacity(value.units().len());
    for unit in value.units() {
        if (65..=90).contains(unit) {
            units.push(u16::from(b'_'));
            units.push(*unit + 32);
        } else {
            units.push(*unit);
        }
    }
    JsString::from_utf16(units)
}

fn json_value(status: StatusCode, value: &JsValue) -> Response<Full<bytes::Bytes>> {
    let text = stringify_json(value)
        .ok()
        .flatten()
        .unwrap_or_else(|| "null".into());
    Response::builder()
        .status(status)
        .header(hyper::header::CONTENT_TYPE, "application/json")
        .body(Full::new(bytes::Bytes::from(text)))
        .expect("static response")
}

fn json_error(
    status: StatusCode,
    message: &str,
    code: Option<&str>,
) -> Response<Full<bytes::Bytes>> {
    let mut value = JsObject::new();
    value.insert("error".into(), message.into());
    if let Some(code) = code {
        value.insert("code".into(), code.into());
    }
    json_value(status, &JsValue::Object(value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyper_util::client::legacy::{Client, connect::HttpConnector};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Unused;
    impl HostedHookHandler for Unused {
        fn call(
            &self,
            _: JsString,
            _: usize,
            _: JsObject,
            _: Cancellation,
            _: JsString,
        ) -> HostedHookFuture {
            Box::pin(async { Ok(JsValue::Object(JsObject::new())) })
        }
    }

    struct Echo(AtomicUsize);
    impl HostedHookHandler for Echo {
        fn call(
            &self,
            event: JsString,
            matcher: usize,
            input: JsObject,
            _: Cancellation,
            async_hook_id: JsString,
        ) -> HostedHookFuture {
            self.0.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move {
                let mut output = JsObject::new();
                output.insert("event".into(), JsValue::String(event));
                output.insert("matcher".into(), JsValue::Number(matcher as f64));
                output.insert("session".into(), input.get("session_id").cloned().unwrap());
                output.insert("asyncHookId".into(), JsValue::String(async_hook_id));
                Ok(JsValue::Object(output))
            })
        }
    }

    #[test]
    fn unsupported_session_hooks_fail_before_binding() {
        let error = match HostedHookServer::new(
            vec![("WorktreeCreate".into(), vec![])],
            Arc::new(Unused),
        ) {
            Ok(_) => panic!("unsupported hook accepted"),
            Err(error) => error,
        };
        assert_eq!(error.code, "unsupported_capability");
        assert!(error.message.contains("Always-On workspace runtime"));
    }

    #[test]
    fn config_preserves_matchers_timeouts_and_bearer_header() {
        let config = HostedHookConfig {
            url: "http://127.0.0.1:1/hooks".into(),
            authorization: "Bearer token".into(),
            events: vec![(
                "PostToolUse".into(),
                vec![HostedHookMatcher {
                    matcher: Some("write_file".into()),
                    timeout_seconds: Some(2.5),
                }],
            )],
        };
        let value = config.to_value();
        assert_eq!(
            value
                .get("headers")
                .and_then(|value| value.get("authorization")),
            Some(&"Bearer token".into())
        );
        let matcher = value
            .get("events")
            .and_then(|value| value.get("PostToolUse"))
            .and_then(|value| match value {
                JsValue::Array(values) => values.first(),
                _ => None,
            })
            .unwrap();
        assert_eq!(matcher.get("matcher"), Some(&"write_file".into()));
        assert_eq!(matcher.get("timeout"), Some(&JsValue::Number(2.5)));
    }

    #[test]
    fn callback_output_uses_native_permission_and_deferred_contracts() {
        let sync = native_output(parse_json(r#"{"hookSpecificOutput":{"hookEventName":"PermissionRequest","permissionDecision":"defer"}}"#).unwrap(), &"PermissionRequest".into(), "generated-id".into()).unwrap();
        assert_eq!(
            sync.get("hookSpecificOutput")
                .unwrap()
                .get("permissionDecision"),
            Some(&"passthrough".into())
        );
        let deferred = native_output(
            parse_json(
                r#"{"async":true,"asyncTimeout":1.5,"asyncHookId":"untrusted","ignored":1}"#,
            )
            .unwrap(),
            &"PreModelRequest".into(),
            "generated-id".into(),
        )
        .unwrap();
        assert_eq!(deferred.get("asyncHookId"), Some(&"generated-id".into()));
        assert_eq!(deferred.get("asyncTimeout"), Some(&JsValue::Number(1.5)));
        assert!(deferred.get("ignored").is_none());
        assert!(
            native_output(
                parse_json(r#"{"async":true,"asyncTimeout":0}"#).unwrap(),
                &"PreModelRequest".into(),
                "id".into()
            )
            .is_err()
        );
        assert!(
            native_output(
                parse_json(r#"{"hookSpecificOutput":{"hookEventName":"Stop"}}"#).unwrap(),
                &"PreModelRequest".into(),
                "id".into()
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn remote_gateway_requires_reachable_public_callback_endpoint() {
        let server = HostedHookServer::new(vec![], Arc::new(Unused)).unwrap();
        let error = server
            .start_for_gateway(Default::default(), Some("wss://gateway.example/ws"))
            .await
            .unwrap_err();
        assert_eq!(error.code, "unsupported_capability");
        assert!(
            server.state.lock().await.running.is_none(),
            "rejection releases listener and cached start"
        );
        let local = server
            .start_for_gateway(Default::default(), Some("ws://127.0.0.1:1234/ws"))
            .await
            .unwrap();
        assert!(local.url.starts_with("http://127.0.0.1:"));
        server.close().await;
        let public = server
            .start_for_gateway(
                HostedHookServerOptions {
                    public_url: Some("https://callback.example/hooks".into()),
                    ..Default::default()
                },
                Some("wss://gateway.example/ws"),
            )
            .await
            .unwrap();
        assert_eq!(public.url, "https://callback.example/hooks");
        server.close().await;
    }

    async fn request(
        method: hyper::Method,
        url: &str,
        authorization: Option<&str>,
        body: &str,
    ) -> Response<Incoming> {
        let client: Client<HttpConnector, Full<bytes::Bytes>> =
            Client::builder(hyper_util::rt::TokioExecutor::new()).build_http();
        let mut request = Request::builder().method(method).uri(url);
        if let Some(authorization) = authorization {
            request = request.header(hyper::header::AUTHORIZATION, authorization);
        }
        client
            .request(
                request
                    .body(Full::new(bytes::Bytes::copy_from_slice(body.as_bytes())))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn hosted_server_authenticates_dispatches_and_restarts() {
        let calls = Arc::new(Echo(AtomicUsize::new(0)));
        let server = HostedHookServer::new(
            vec![(
                "PostToolUse".into(),
                vec![HostedHookMatcher {
                    matcher: Some("write_file".into()),
                    timeout_seconds: None,
                }],
            )],
            calls.clone(),
        )
        .unwrap();
        let config = server
            .start(HostedHookServerOptions::default())
            .await
            .unwrap();
        let second = server
            .start(HostedHookServerOptions::default())
            .await
            .unwrap();
        assert_eq!(
            second, config,
            "concurrent/repeated starts reuse one endpoint"
        );

        let selector = format!("{}?event=PostToolUse&matcher=0", config.url);
        let unauthorized = request(hyper::Method::POST, &selector, None, "{}").await;
        assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

        let body = r#"{"hookEventName":"PostToolUse","sessionId":"s","transcriptPath":"/tmp/t","cwd":"/tmp"}"#;
        let response = request(
            hyper::Method::POST,
            &selector,
            Some(&config.authorization),
            body,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let output = parse_json(std::str::from_utf8(&body).unwrap()).unwrap();
        assert_eq!(output.get("event"), Some(&"PostToolUse".into()));
        assert_eq!(output.get("matcher"), Some(&JsValue::Number(0.0)));
        assert_eq!(output.get("session"), Some(&"s".into()));
        assert_eq!(calls.0.load(Ordering::SeqCst), 1);

        server.close().await;
        let restarted = server
            .start(HostedHookServerOptions::default())
            .await
            .unwrap();
        assert!(!restarted.authorization.is_empty());
        server.close().await;
    }

    #[tokio::test]
    async fn finite_overflow_timeout_uses_node_timer_clamp() {
        struct Pending;
        impl HostedHookHandler for Pending {
            fn call(
                &self,
                _: JsString,
                _: usize,
                _: JsObject,
                _: Cancellation,
                _: JsString,
            ) -> HostedHookFuture {
                Box::pin(std::future::pending())
            }
        }
        let server = HostedHookServer::new(
            vec![(
                "SessionStart".into(),
                vec![HostedHookMatcher {
                    matcher: None,
                    timeout_seconds: Some(1e100),
                }],
            )],
            Arc::new(Pending),
        )
        .unwrap();
        let config = server.start(Default::default()).await.unwrap();
        let response = tokio::time::timeout(Duration::from_secs(2), request(
            hyper::Method::POST,
            &format!("{}?event=SessionStart&matcher=0", config.url),
            Some(&config.authorization),
            r#"{"hookEventName":"SessionStart","sessionId":"s","transcriptPath":"","cwd":"/tmp"}"#,
        )).await.unwrap();
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let output = parse_json(std::str::from_utf8(&bytes).unwrap()).unwrap();
        assert_eq!(output.get("code"), Some(&"timeout".into()));
        server.close().await;
    }

    #[tokio::test]
    async fn chunked_body_is_rejected_before_end_when_limit_is_crossed() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let calls = Arc::new(Echo(AtomicUsize::new(0)));
        let server = HostedHookServer::new(
            vec![(
                "SessionStart".into(),
                vec![HostedHookMatcher {
                    matcher: None,
                    timeout_seconds: None,
                }],
            )],
            calls.clone(),
        )
        .unwrap();
        let config = server.start(Default::default()).await.unwrap();
        let endpoint = url::Url::parse(&config.url).unwrap();
        let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", endpoint.port().unwrap()))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            stream.write_all(format!("POST {}?event=SessionStart&matcher=0 HTTP/1.1\r\nHost: localhost\r\nAuthorization: {}\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n", endpoint.path(), config.authorization).as_bytes()).await.unwrap();
            for size in [600_000, 500_001] {
                stream.write_all(format!("{size:x}\r\n").as_bytes()).await.unwrap();
                stream.write_all(&vec![b'x'; size]).await.unwrap();
                stream.write_all(b"\r\n").await.unwrap();
            }
            // An unbounded collector waits forever here: no final chunk is
            // sent. Bounded reading must reject as soon as the limit is hit.
            let mut response = Vec::new();
            let mut chunk = [0; 4096];
            loop {
                // Closing with unread request bytes may reset TCP after the
                // response. Require the complete error envelope, not clean
                // EOF; a reset before that envelope remains a test failure.
                let count = stream.read(&mut chunk).await.expect("response must arrive before reset");
                assert!(count > 0, "connection closed before complete response");
                response.extend_from_slice(&chunk[..count]);
                if let Some(end) = response.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                    let headers = std::str::from_utf8(&response[..end]).unwrap();
                    assert!(headers.starts_with("HTTP/1.1 500"), "{headers}");
                    let length: usize = headers.lines().find_map(|line| line.split_once(':').filter(|(key, _)| key.eq_ignore_ascii_case("content-length")).map(|(_, value)| value.trim().parse().unwrap())).expect("bounded JSON response has content length");
                    if response.len() >= end + 4 + length {
                        let output = parse_json(std::str::from_utf8(&response[end + 4..end + 4 + length]).unwrap()).unwrap();
                        assert_eq!(output.get("code"), Some(&"validation_error".into()));
                        assert_eq!(output.get("error"), Some(&"SDK hook payload exceeds 1MB.".into()));
                        break;
                    }
                }
            }
        }).await.unwrap();
        assert_eq!(calls.0.load(Ordering::SeqCst), 0);
        server.close().await;
    }

    #[tokio::test]
    async fn closing_host_cancels_active_callback_and_drains_connections() {
        struct Wait(Arc<tokio::sync::Notify>, Arc<std::sync::atomic::AtomicBool>);
        impl HostedHookHandler for Wait {
            fn call(
                &self,
                _: JsString,
                _: usize,
                _: JsObject,
                signal: Cancellation,
                _: JsString,
            ) -> HostedHookFuture {
                let entered = self.0.clone();
                let cancelled = self.1.clone();
                Box::pin(async move {
                    struct Observe(Cancellation, Arc<std::sync::atomic::AtomicBool>);
                    impl Drop for Observe {
                        fn drop(&mut self) {
                            self.1.store(self.0.is_cancelled(), Ordering::SeqCst);
                        }
                    }
                    let _observe = Observe(signal.clone(), cancelled);
                    entered.notify_one();
                    signal.cancelled().await;
                    Ok(JsValue::Object(JsObject::new()))
                })
            }
        }
        let entered = Arc::new(tokio::sync::Notify::new());
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let server = HostedHookServer::new(
            vec![(
                "SessionStart".into(),
                vec![HostedHookMatcher {
                    matcher: None,
                    timeout_seconds: None,
                }],
            )],
            Arc::new(Wait(entered.clone(), cancelled.clone())),
        )
        .unwrap();
        let config = server.start(Default::default()).await.unwrap();
        let caller = tokio::spawn(async move {
            let client: Client<HttpConnector, Full<bytes::Bytes>> =
                Client::builder(hyper_util::rt::TokioExecutor::new()).build_http();
            client.request(Request::builder().method(hyper::Method::POST).uri(format!("{}?event=SessionStart&matcher=0", config.url)).header(hyper::header::AUTHORIZATION, config.authorization).body(Full::new(bytes::Bytes::from_static(br#"{"hookEventName":"SessionStart","sessionId":"s","transcriptPath":"/tmp/t","cwd":"/tmp"}"#))).unwrap()).await
        });
        tokio::time::timeout(Duration::from_secs(2), entered.notified())
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(2), server.close())
            .await
            .unwrap();
        let _ = caller.await.unwrap();
        assert!(cancelled.load(Ordering::SeqCst));
        // A close also resets the single-flight cache.
        server.start(Default::default()).await.unwrap();
        server.close().await;
    }
}
