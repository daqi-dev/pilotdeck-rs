//! Opt-in, transport-neutral connection boundary. Default pairing stays proxied.
//! Gateway business owners are injected; no live session/abort adapter is fabricated.
pub mod binding;
mod context;

use binding::WsBinding;
use context::{SessionKey, State, js_value, truthy};
use pilotdeck_protocol::{
    GATEWAY_PROTOCOL_VERSION, JsObject, JsString, JsValue, parse_json, stringify_json,
};
use serde_json::Value;
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
};

pub type Call<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub trait GatewayTextConnection: Send + Sync {
    fn send_text(&self, message: String);
    fn close(&self, code: u16, reason: &str);
}
#[derive(Clone, Debug)]
pub struct RequestError {
    pub code: String,
    /// JS strings are UTF-16 code units: an unknown-method message must echo a
    /// lone surrogate exactly as TS does, so no lossy UTF-8 conversion happens.
    pub message: JsString,
    /// Provider error payloads stay JsValue: serde_json::Value sorts keys and
    /// would reorder nested objects relative to the TS JSON.stringify output.
    /// Boxed to keep RequestError small (clippy::result_large_err).
    pub details: Option<Box<JsValue>>,
    pub validation: Option<Box<JsValue>>,
}
impl RequestError {
    pub fn new(message: impl Into<JsString>) -> Self {
        Self {
            code: "gateway_request_failed".into(),
            message: message.into(),
            details: None,
            validation: None,
        }
    }
}
/// Session lifecycle injection point. The pr2 owner adapter is deliberately not
/// implemented here. Every inbound protocol string (method, params, sessionKey,
/// clientName) is a JsString/JsValue so lone UTF-16 surrogate code units pass
/// through without replacement. disconnect must select/remove under its owner
/// lock using the supplied binding. abort receives exactly {sessionKey}, with no
/// runId, preserving A5's unfenced socket-close behavior (including old sockets).
pub trait ConnectionGateway: Send + Sync {
    /// Provider results are JsValue so insertion order survives to JSON.stringify.
    /// serde_json::Value (BTreeMap) would alphabetize nested keys and break the
    /// byte-for-byte frame comparison against TS.
    fn describe_server(&self) -> Call<'_, Result<JsValue, RequestError>>;
    fn request(&self, method: JsString, params: JsValue)
    -> Call<'_, Result<JsValue, RequestError>>;
    fn submit<'a>(
        &'a self,
        params: JsValue,
        emit: &'a (dyn Fn(Value) + Send + Sync),
    ) -> Call<'a, Result<(), RequestError>>;
    fn disconnect(
        &self,
        session: JsValue,
        binding: WsBinding,
    ) -> Call<'_, Result<bool, RequestError>>;
    fn abort_on_close(&self, params: JsValue) -> Call<'_, Result<(), RequestError>>;
}

pub struct GatewayConnection {
    text: Arc<dyn GatewayTextConnection>,
    gateway: Arc<dyn ConnectionGateway>,
    token: String,
    server_version: String,
    binding: WsBinding,
    state: Mutex<State>,
}
impl GatewayConnection {
    pub fn new(
        text: Arc<dyn GatewayTextConnection>,
        gateway: Arc<dyn ConnectionGateway>,
        token: String,
        server_version: String,
    ) -> Self {
        Self::with_binding(text, gateway, token, server_version, WsBinding::fresh())
    }
    fn with_binding(
        text: Arc<dyn GatewayTextConnection>,
        gateway: Arc<dyn ConnectionGateway>,
        token: String,
        server_version: String,
        binding: WsBinding,
    ) -> Self {
        Self {
            text,
            gateway,
            token,
            server_version,
            binding,
            state: Mutex::new(State::default()),
        }
    }
    pub fn binding(&self) -> &WsBinding {
        &self.binding
    }
    // Outbound frames are built as JsValue too: a response id may contain a
    // lone surrogate and JSON.stringify must re-escape it, not replace it.
    fn send(&self, frame: JsValue) {
        let text = stringify_json(&frame)
            .expect("connection frames are JSON values")
            .expect("connection frames are never undefined");
        self.text.send_text(text);
    }
    pub fn notify(&self, name: &str, payload: Option<JsValue>) {
        // Snapshot authorization before invoking a potentially reentrant sink.
        let state = self.state.lock().unwrap();
        if !state.authed {
            return;
        }
        drop(state);
        let mut frame = JsObject::new();
        frame.insert("type".into(), JsValue::from("notification"));
        frame.insert("name".into(), JsValue::from(name));
        if let Some(payload) = payload {
            frame.insert("payload".into(), payload);
        }
        self.send(JsValue::Object(frame));
    }
    /// The synchronous prefix reserves context before returning the future,
    /// like TS before its first await. SDK classification and binding capture
    /// share that lock. No async gateway call holds the context lock.
    /// Drive the returned future to completion: dropping it cancels the request,
    /// unlike a TS Promise. In particular, adapters must not put it in a losing
    /// select! branch on socket close. Spawn/retain request tasks and drain them
    /// while calling closed(); no synthetic terminal event is sent on cancellation.
    pub fn receive<'a>(&'a self, message: &str) -> Call<'a, Result<(), RequestError>> {
        let mut state = self.state.lock().unwrap();
        // parse_json accepts lone UTF-16 surrogates; serde_json would reject the
        // frame with 4002 while TS JSON.parse accepts it.
        let Ok(frame) = parse_json(message) else {
            drop(state);
            self.text.close(4002, "invalid_json");
            return Box::pin(async { Ok(()) });
        };
        if !state.authed {
            let is_hello = frame
                .get("type")
                .and_then(JsValue::as_string)
                .is_some_and(|kind| kind.equals_str("hello"));
            if !is_hello
                || !["protocolVersion", "clientName", "clientVersion", "token"]
                    .iter()
                    .all(|key| frame.get(key).and_then(JsValue::as_string).is_some())
            {
                drop(state);
                self.text.close(4001, "hello_required");
                return Box::pin(async { Ok(()) });
            }
            let protocol_matches = frame
                .get("protocolVersion")
                .and_then(JsValue::as_string)
                .is_some_and(|value| value.equals_str(GATEWAY_PROTOCOL_VERSION));
            if !protocol_matches {
                drop(state);
                self.text.close(4001, "protocol_mismatch");
                return Box::pin(async { Ok(()) });
            }
            let token_matches = frame
                .get("token")
                .and_then(JsValue::as_string)
                .is_some_and(|value| value.equals_str(&self.token));
            if !token_matches {
                drop(state);
                self.text.close(4003, "auth_failed");
                return Box::pin(async { Ok(()) });
            }
            state.authed = true;
            state.client_name = frame
                .get("clientName")
                .and_then(JsValue::as_string)
                .cloned();
            return Box::pin(async move {
                // As in TS, authentication is published before describe awaits.
                let info = self.gateway.describe_server().await?;
                let mut out = JsObject::new();
                out.insert("type".into(), JsValue::from("hello_ok"));
                out.insert(
                    "protocolVersion".into(),
                    JsValue::from(GATEWAY_PROTOCOL_VERSION),
                );
                out.insert(
                    "serverVersion".into(),
                    JsValue::from(self.server_version.as_str()),
                );
                out.insert("serverInfo".into(), info);
                out.insert(
                    "interactionBinding".into(),
                    js_value(&self.binding.to_json()),
                );
                self.send(JsValue::Object(out));
                Ok(())
            });
        }
        let is_request = frame
            .get("type")
            .and_then(JsValue::as_string)
            .is_some_and(|kind| kind.equals_str("request"));
        if !is_request
            || frame.get("id").and_then(JsValue::as_string).is_none()
            || frame.get("method").and_then(JsValue::as_string).is_none()
        {
            drop(state);
            self.text.close(4002, "invalid_frame");
            return Box::pin(async { Ok(()) });
        }
        let id = frame.get("id").cloned().unwrap();
        let method = frame
            .get("method")
            .and_then(JsValue::as_string)
            .cloned()
            .unwrap();
        let input = frame.get("params").cloned().unwrap_or(JsValue::Null);
        let session_value = input.get("sessionKey").cloned().unwrap_or(JsValue::Null);
        let session = truthy(&session_value).then(|| SessionKey::new(session_value));
        let is_submit = method.equals_str("submit_turn");
        if is_submit && let Some(session) = session.as_ref() {
            state.track_interaction(session);
            state.track_in_flight(session);
        }
        let params = state.inject(&method, input.clone(), &self.binding);
        drop(state);
        let cleanup = is_submit.then(|| SubmitCleanup {
            connection: self,
            session,
        });
        Box::pin(async move {
            let result = if is_submit {
                self.submit(&id, params, &input, cleanup)
                    .await
                    .map(|()| None)
            } else {
                self.gateway
                    .request(method.clone(), params)
                    .await
                    .and_then(|result| {
                        // TS reads the original params only after dispatch succeeds.
                        // Null/undefined throw here even though dispatch saw an object.
                        if method.equals_str("permission_decide") && matches!(input, JsValue::Null)
                        {
                            let kind = if frame.get("params").is_some() {
                                "null"
                            } else {
                                "undefined"
                            };
                            Err(RequestError::new(format!(
                                "Cannot read properties of {kind} (reading 'sessionKey')"
                            )))
                        } else {
                            Ok(Some(result))
                        }
                    })
            };
            match result {
                Ok(Some(result)) => {
                    if method.equals_str("permission_decide")
                        && input
                            .get("sessionKey")
                            .and_then(JsValue::as_string)
                            .is_some()
                        && input
                            .get("requestId")
                            .and_then(JsValue::as_string)
                            .is_some()
                    {
                        let mut payload = JsObject::new();
                        payload.insert("type".into(), JsValue::from("resolved"));
                        payload.insert(
                            "sessionKey".into(),
                            input.get("sessionKey").cloned().unwrap(),
                        );
                        payload
                            .insert("requestId".into(), input.get("requestId").cloned().unwrap());
                        if input
                            .get("projectKey")
                            .and_then(JsValue::as_string)
                            .is_some()
                        {
                            payload.insert(
                                "projectKey".into(),
                                input.get("projectKey").cloned().unwrap(),
                            );
                        }
                        self.notify("permission_changed", Some(JsValue::Object(payload)));
                    }
                    let mut out = JsObject::new();
                    out.insert("type".into(), JsValue::from("response"));
                    out.insert("id".into(), id);
                    out.insert("ok".into(), JsValue::Bool(true));
                    out.insert("result".into(), result);
                    self.send(JsValue::Object(out));
                }
                Ok(None) => {}
                Err(failure) => {
                    let mut detail = JsObject::new();
                    detail.insert("code".into(), JsValue::from(failure.code.as_str()));
                    detail.insert("message".into(), JsValue::String(failure.message));
                    if let Some(validation) = failure.validation {
                        detail.insert("validation".into(), *validation);
                    } else if let Some(details) = failure.details {
                        detail.insert("details".into(), *details);
                    }
                    let mut out = JsObject::new();
                    out.insert("type".into(), JsValue::from("response"));
                    out.insert("id".into(), id);
                    out.insert("ok".into(), JsValue::Bool(false));
                    out.insert("error".into(), JsValue::Object(detail));
                    self.send(JsValue::Object(out));
                }
            }
            Ok(())
        })
    }
    async fn submit(
        &self,
        id: &JsValue,
        params: JsValue,
        input: &JsValue,
        cleanup: Option<SubmitCleanup<'_>>,
    ) -> Result<(), RequestError> {
        let stream = Mutex::new((0u64, None, None));
        let emit = |event: Value| {
            let mut stream = stream.lock().unwrap();
            if event["type"] == "turn_completed" {
                stream.1 = Some(event.clone());
            }
            if event["type"] == "error" {
                stream.2 = Some(event.clone());
            }
            if event["type"] == "permission_request" && input.get("sessionKey").is_some_and(truthy)
            {
                let mut payload = JsObject::new();
                payload.insert("type".into(), JsValue::from("requested"));
                payload.insert(
                    "sessionKey".into(),
                    input.get("sessionKey").cloned().unwrap(),
                );
                if let Some(value) = input.get("projectKey") {
                    payload.insert("projectKey".into(), value.clone());
                }
                if let Some(value) = event.get("requestId") {
                    payload.insert("requestId".into(), js_value(value));
                }
                self.notify("permission_changed", Some(JsValue::Object(payload)));
            }
            let mut out = JsObject::new();
            out.insert("type".into(), JsValue::from("event"));
            out.insert("id".into(), id.clone());
            out.insert("seq".into(), JsValue::Number(stream.0 as f64));
            out.insert("final".into(), JsValue::Bool(false));
            out.insert("event".into(), js_value(&event));
            self.send(JsValue::Object(out));
            stream.0 += 1;
        };
        let result = self.gateway.submit(params.clone(), &emit).await;
        // TS finally clears the reservation before terminal/error frames.
        drop(cleanup);
        result?;
        let (seq, completed, error) = stream.into_inner().unwrap();
        // Build the fallback in TS property order rather than serde's sort.
        let terminal = match error.or(completed) {
            Some(event) => js_value(&event),
            None => {
                let mut fallback = JsObject::new();
                fallback.insert("type".into(), JsValue::from("error"));
                fallback.insert("code".into(), JsValue::from("result_unknown"));
                fallback.insert(
                    "message".into(),
                    JsValue::from("Gateway stream ended without a terminal result."),
                );
                fallback.insert("recoverable".into(), JsValue::Bool(true));
                JsValue::Object(fallback)
            }
        };
        let mut out = JsObject::new();
        out.insert("type".into(), JsValue::from("event"));
        out.insert("id".into(), id.clone());
        out.insert("seq".into(), JsValue::Number(seq as f64));
        out.insert("final".into(), JsValue::Bool(true));
        out.insert("event".into(), terminal);
        self.send(JsValue::Object(out));
        Ok(())
    }
    // TS propagates disconnect rejection immediately; it neither aborts that
    // session nor processes later sessions. Do not invent best-effort cleanup.
    pub fn closed(&self) -> Call<'_, Result<(), RequestError>> {
        let sessions = { self.state.lock().unwrap().sessions() };
        Box::pin(async move {
            for session in sessions {
                let preserve_turn = self
                    .gateway
                    .disconnect(session.value().clone(), self.binding.clone())
                    .await?;
                let in_flight = self.state.lock().unwrap().in_flight.contains(&session);
                if in_flight && !preserve_turn {
                    // A5 explicitly requires a session-wide abort, without runId.
                    let mut params = JsObject::new();
                    params.insert("sessionKey".into(), session.value().clone());
                    let _ = self.gateway.abort_on_close(JsValue::Object(params)).await;
                }
            }
            let mut state = self.state.lock().unwrap();
            state.in_flight.clear();
            state.interactions.clear();
            Ok(())
        })
    }
}
struct SubmitCleanup<'a> {
    connection: &'a GatewayConnection,
    session: Option<SessionKey>,
}
impl Drop for SubmitCleanup<'_> {
    fn drop(&mut self) {
        let Some(session) = &self.session else {
            return;
        };
        self.connection
            .state
            .lock()
            .unwrap()
            .in_flight
            .retain(|s| s != session);
    }
}
#[cfg(test)]
mod tests;
