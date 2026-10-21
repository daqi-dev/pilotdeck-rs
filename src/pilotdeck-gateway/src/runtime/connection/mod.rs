//! Opt-in, transport-neutral connection boundary. Default pairing stays proxied.
//! Gateway business owners are injected; no live session/abort adapter is fabricated.
pub mod binding;
mod context;

use binding::WsBinding;
use context::State;
use pilotdeck_protocol::GATEWAY_PROTOCOL_VERSION;
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
/// implemented here. disconnect must select/remove under its owner lock using
/// the supplied binding. abort receives exactly {sessionKey}, with no runId,
/// preserving A5's unfenced socket-close behavior (including old sockets).
pub trait ConnectionGateway: Send + Sync {
    fn describe_server(&self) -> Call<'_, Result<Value, RequestError>>;
    fn request(&self, method: String, params: Value) -> Call<'_, Result<Value, RequestError>>;
    fn submit<'a>(
        &'a self,
        params: Value,
        emit: &'a (dyn Fn(Value) + Send + Sync),
    ) -> Call<'a, Result<(), RequestError>>;
    fn disconnect(
        &self,
        session: String,
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
        // Sending is inside the same lock as auth/close publication.
        let state = self.state.lock().unwrap();
        if !state.authed {
            return;
        }
        let mut frame = json!({"type":"notification", "name":name});
        if let Some(payload) = payload {
            frame["payload"] = payload;
        }
        self.send(frame);
    }
    /// The synchronous prefix reserves context before returning the future,
    /// like TS before its first await. SDK classification and binding capture
    /// share that lock. No async gateway call holds the context lock.
    pub fn receive<'a>(&'a self, message: &str) -> Call<'a, Result<(), RequestError>> {
        let mut state = self.state.lock().unwrap();
        if state.closed {
            return Box::pin(async { Ok(()) });
        }
        let Ok(frame) = serde_json::from_str::<Value>(message) else {
            self.text.close(4002, "invalid_json");
            return Box::pin(async { Ok(()) });
        };
        if !state.authed {
            if frame["type"] != "hello"
                || !["protocolVersion", "clientName", "clientVersion", "token"]
                    .iter()
                    .all(|key| frame[*key].is_string())
            {
                self.text.close(4001, "hello_required");
                return Box::pin(async { Ok(()) });
            }
            if frame["protocolVersion"] != GATEWAY_PROTOCOL_VERSION {
                self.text.close(4001, "protocol_mismatch");
                return Box::pin(async { Ok(()) });
            }
            if frame["token"] != self.token {
                self.text.close(4003, "auth_failed");
                return Box::pin(async { Ok(()) });
            }
            state.authed = true;
            state.client_name = frame["clientName"].as_str().map(str::to_owned);
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
        if frame["type"] != "request" || !frame["id"].is_string() || !frame["method"].is_string() {
            self.text.close(4002, "invalid_frame");
            return Box::pin(async { Ok(()) });
        }
        let id = frame["id"].clone();
        let method = frame["method"].as_str().unwrap().to_owned();
        let input = frame.get("params").cloned().unwrap_or(Value::Null);
        let session = input["sessionKey"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
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
                self.submit(&id, params, cleanup).await.map(|()| None)
            } else {
                self.gateway
                    .request(method.clone(), params)
                    .await
                    .and_then(|result| {
                        // TS reads the original params only after dispatch succeeds.
                        // Null/undefined throw here even though dispatch saw an object.
                        if method == "permission_decide" && input.is_null() {
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
                    if method == "permission_decide"
                        && input.is_object()
                        && input["sessionKey"].is_string()
                        && input["requestId"].is_string()
                    {
                        let mut payload = json!({"type":"resolved", "sessionKey":input["sessionKey"], "requestId":input["requestId"]});
                        if input["projectKey"].is_string() {
                            payload["projectKey"] = input["projectKey"].clone();
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
        params: Value,
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
            if event["type"] == "permission_request"
                && params["sessionKey"].as_str().is_some_and(|s| !s.is_empty())
            {
                let mut payload = json!({"type":"requested", "sessionKey":params["sessionKey"]});
                for key in ["projectKey", "requestId"] {
                    if let Some(value) = if key == "requestId" {
                        event.get(key)
                    } else {
                        params.get(key)
                    } {
                        payload[key] = value.clone();
                    }
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
    pub fn closed(&self) -> Call<'_, Result<(), RequestError>> {
        let sessions = {
            let mut state = self.state.lock().unwrap();
            if state.closed {
                return Box::pin(async { Ok(()) });
            }
            state.closed = true;
            state.sessions()
        };
        Box::pin(async move {
            for session in sessions {
                let preserve_turn = self
                    .gateway
                    .disconnect(session.clone(), self.binding.clone())
                    .await?;
                let in_flight = self.state.lock().unwrap().in_flight.contains(&session);
                if in_flight && !preserve_turn {
                    // A5 explicitly requires a session-wide abort, without runId.
                    let _ = self
                        .gateway
                        .abort_on_close(json!({"sessionKey":session}))
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
    session: Option<String>,
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
