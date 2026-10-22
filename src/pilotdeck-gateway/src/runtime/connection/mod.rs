//! Opt-in, transport-neutral connection boundary. Default pairing stays proxied.
//! Gateway business owners are injected; no live session/abort adapter is fabricated.
pub mod binding;
mod context;

use binding::WsBinding;
use context::{SessionKey, State, jv_get, jv_str, jv_truthy, truthy};
use pilotdeck_protocol::{GATEWAY_PROTOCOL_VERSION, JsValue, parse_json};
use serde_json::{Value, json};
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
    pub message: String,
    pub details: Option<Value>,
    pub validation: Option<Value>,
}
impl RequestError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            code: "gateway_request_failed".into(),
            message: message.into(),
            details: None,
            validation: None,
        }
    }
}
/// Session lifecycle injection point. The pr2 owner adapter is deliberately not
/// implemented here. Request/submit params use JsValue so JS string spread can
/// pass lone UTF-16 surrogate code units without replacement. disconnect must
/// select/remove under its owner lock using the supplied binding. abort receives
/// exactly {sessionKey}, with no runId, preserving A5's unfenced socket-close behavior (including old sockets).
pub trait ConnectionGateway: Send + Sync {
    fn describe_server(&self) -> Call<'_, Result<Value, RequestError>>;
    fn request(&self, method: String, params: JsValue) -> Call<'_, Result<Value, RequestError>>;
    fn submit<'a>(
        &'a self,
        params: JsValue,
        emit: &'a (dyn Fn(Value) + Send + Sync),
    ) -> Call<'a, Result<(), RequestError>>;
    fn disconnect(
        &self,
        session: Value,
        binding: WsBinding,
    ) -> Call<'_, Result<bool, RequestError>>;
    fn abort_on_close(&self, params: Value) -> Call<'_, Result<(), RequestError>>;
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
    fn send(&self, frame: Value) {
        self.text.send_text(frame.to_string());
    }
    pub fn notify(&self, name: &str, payload: Option<Value>) {
        // Snapshot authorization before invoking a potentially reentrant sink.
        let state = self.state.lock().unwrap();
        if !state.authed {
            return;
        }
        drop(state);
        let mut frame = json!({"type":"notification", "name":name});
        if let Some(payload) = payload {
            frame["payload"] = payload;
        }
        self.send(frame);
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
        // parse_json accepts lone UTF-16 surrogates; serde_json would reject them.
        let Ok(frame) = parse_json(message) else {
            drop(state);
            self.text.close(4002, "invalid_json");
            return Box::pin(async { Ok(()) });
        };
        if !state.authed {
            if jv_str(jv_get(&frame, "type")).as_deref() != Some("hello")
                || !["protocolVersion", "clientName", "clientVersion", "token"]
                    .iter()
                    .all(|key| jv_str(jv_get(&frame, key)).is_some())
            {
                drop(state);
                self.text.close(4001, "hello_required");
                return Box::pin(async { Ok(()) });
            }
            if jv_str(jv_get(&frame, "protocolVersion")).as_deref()
                != Some(GATEWAY_PROTOCOL_VERSION)
            {
                drop(state);
                self.text.close(4001, "protocol_mismatch");
                return Box::pin(async { Ok(()) });
            }
            if jv_str(jv_get(&frame, "token")).as_deref() != Some(self.token.as_str()) {
                drop(state);
                self.text.close(4003, "auth_failed");
                return Box::pin(async { Ok(()) });
            }
            state.authed = true;
            state.client_name = jv_str(jv_get(&frame, "clientName"));
            return Box::pin(async move {
                // As in TS, authentication is published before describe awaits.
                let info = self.gateway.describe_server().await?;
                self.send(
                    json!({"type":"hello_ok", "protocolVersion":GATEWAY_PROTOCOL_VERSION,
                        "serverVersion":self.server_version, "serverInfo":info,
                        "interactionBinding":self.binding.to_json()}),
                );
                Ok(())
            });
        }
        if jv_str(jv_get(&frame, "type")).as_deref() != Some("request")
            || jv_str(jv_get(&frame, "id")).is_none()
            || jv_str(jv_get(&frame, "method")).is_none()
        {
            drop(state);
            self.text.close(4002, "invalid_frame");
            return Box::pin(async { Ok(()) });
        }
        let id = json!(jv_str(jv_get(&frame, "id")).unwrap());
        let method = jv_str(jv_get(&frame, "method")).unwrap();
        // params may contain lone surrogates; keep as JsValue throughout.
        let input: JsValue = match frame {
            JsValue::Object(ref o) => o.get("params").cloned().unwrap_or(JsValue::Null),
            _ => JsValue::Null,
        };
        let session_key_jv = jv_get(&input, "sessionKey");
        let session_key_val: Value = pilotdeck_protocol::stringify_json(session_key_jv)
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(Value::Null);
        let session = jv_truthy(session_key_jv).then(|| SessionKey::new(session_key_val));
        let is_submit = method == "submit_turn";
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
                        if method == "permission_decide" && matches!(input, JsValue::Null) {
                            let kind = match &frame {
                                JsValue::Object(o) if o.contains_key("params") => "null",
                                _ => "undefined",
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
                    if method == "permission_decide"
                        && matches!(input, JsValue::Object(_))
                        && jv_str(jv_get(&input, "sessionKey")).is_some()
                        && jv_str(jv_get(&input, "requestId")).is_some()
                    {
                        let session_key = jv_str(jv_get(&input, "sessionKey")).unwrap();
                        let request_id = jv_str(jv_get(&input, "requestId")).unwrap();
                        let mut payload = json!({"type":"resolved", "sessionKey":session_key, "requestId":request_id});
                        if let Some(pk) = jv_str(jv_get(&input, "projectKey")) {
                            payload["projectKey"] = json!(pk);
                        }
                        self.notify("permission_changed", Some(payload));
                    }
                    self.send(json!({"type":"response", "id":id, "ok":true, "result":result}));
                }
                Ok(None) => {}
                Err(error) => {
                    let mut detail = json!({"code":error.code, "message":error.message});
                    if let Some(validation) = error.validation {
                        detail["validation"] = validation;
                    } else if let Some(details) = error.details {
                        detail["details"] = details;
                    }
                    self.send(json!({"type":"response", "id":id, "ok":false, "error":detail}));
                }
            }
            Ok(())
        })
    }
    async fn submit(
        &self,
        id: &Value,
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
            if event["type"] == "permission_request" && jv_truthy(jv_get(input, "sessionKey")) {
                let sk_raw = pilotdeck_protocol::stringify_json(jv_get(input, "sessionKey"))
                    .ok()
                    .flatten()
                    .unwrap_or_else(|| "null".to_owned());
                let sk_val: Value = serde_json::from_str(&sk_raw).unwrap_or(Value::Null);
                let mut payload = json!({"type":"requested", "sessionKey":sk_val});
                if let Some(pk) = jv_str(jv_get(input, "projectKey")) {
                    payload["projectKey"] = json!(pk);
                }
                if let Some(rid) = event.get("requestId").and_then(Value::as_str) {
                    payload["requestId"] = json!(rid);
                }
                self.notify("permission_changed", Some(payload));
            }
            self.send(
                json!({"type":"event", "id":id, "seq":stream.0, "final":false, "event":event}),
            );
            stream.0 += 1;
        };
        let result = self.gateway.submit(params.clone(), &emit).await;
        // TS finally clears the reservation before terminal/error frames.
        drop(cleanup);
        result?;
        let (seq, completed, error) = stream.into_inner().unwrap();
        let terminal = error.or(completed).unwrap_or_else(|| {
            json!({"type":"error", "code":"result_unknown",
            "message":"Gateway stream ended without a terminal result.", "recoverable":true})
        });
        self.send(json!({"type":"event", "id":id, "seq":seq, "final":true, "event":terminal}));
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
                    let _ = self
                        .gateway
                        .abort_on_close(json!({"sessionKey":session.value()}))
                        .await;
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
