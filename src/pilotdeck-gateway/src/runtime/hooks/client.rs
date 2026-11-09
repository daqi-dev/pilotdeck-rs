//! Rust SDK callback owner (`packages/sdk/src/client.ts:1153-1221,1751-1779`).
//! Callers supply ordinary submit_turn parameters; this entry point owns the
//! callback endpoint from submission until the wire stream actually ends.
use super::{
    Cancellation, HostedHookCallbackEvents, HostedHookCallbacks, HostedHookEvents,
    HostedHookHandler, HostedHookServer, HostedHookServerOptions,
};
use crate::cron::client::{
    GatewayClient, GatewayClientError, GatewayClientOptions, GatewayOrderedEventStream,
};
use pilotdeck_protocol::{JsObject, JsValue};
use serde_json::Value;
use std::sync::Arc;

struct Owner {
    client: GatewayClient,
    server: HostedHookServer,
    closed: Cancellation,
}
impl Owner {
    async fn close(&self) {
        self.closed.abort();
        // Closing the transport also cancels the active Gateway turn. Explicit
        // close, unlike normal EOF, does not wait for SessionEnd callbacks.
        self.client.close().await;
        self.server.close().await;
    }
}

/// Connected Rust SDK session with native HTTP callback hosting. It forwards
/// public Gateway events without query/message convenience projections.
/// Consume `next` through EOF: a `turn_completed` event can precede SessionEnd.
pub struct HostedHookSession {
    owner: Arc<Owner>,
    gateway_url: String,
    server_options: HostedHookServerOptions,
    has_hooks: bool,
    stream: Option<GatewayOrderedEventStream>,
}
impl HostedHookSession {
    /// Own SDK callback matchers and their HTTP endpoint through each submitted
    /// turn's wire EOF (`packages/sdk/src/client.ts:1153-1221,1761-1779`).
    pub async fn connect_callbacks(
        gateway_url: &str,
        token: &str,
        client_options: GatewayClientOptions,
        callbacks: HostedHookCallbackEvents,
        server_options: HostedHookServerOptions,
    ) -> Result<Self, GatewayClientError> {
        let callbacks = Arc::new(HostedHookCallbacks::new(callbacks).map_err(host_error)?);
        Self::connect(
            gateway_url,
            token,
            client_options,
            callbacks.events(),
            callbacks,
            server_options,
        )
        .await
    }

    pub async fn connect(
        gateway_url: &str,
        token: &str,
        client_options: GatewayClientOptions,
        events: HostedHookEvents,
        handler: Arc<dyn HostedHookHandler>,
        server_options: HostedHookServerOptions,
    ) -> Result<Self, GatewayClientError> {
        // Validate hooks before opening the Gateway connection.
        let has_hooks = !events.is_empty();
        let server = HostedHookServer::new(events, handler).map_err(host_error)?;
        let client =
            GatewayClient::connect_with_options(gateway_url, token, client_options).await?;
        Ok(Self::from_server(
            client,
            gateway_url.into(),
            server,
            has_hooks,
            server_options,
        ))
    }

    pub fn from_connected(
        client: GatewayClient,
        gateway_url: String,
        events: HostedHookEvents,
        handler: Arc<dyn HostedHookHandler>,
        server_options: HostedHookServerOptions,
    ) -> Result<Self, GatewayClientError> {
        let has_hooks = !events.is_empty();
        let server = HostedHookServer::new(events, handler).map_err(host_error)?;
        Ok(Self::from_server(
            client,
            gateway_url,
            server,
            has_hooks,
            server_options,
        ))
    }

    fn from_server(
        client: GatewayClient,
        gateway_url: String,
        server: HostedHookServer,
        has_hooks: bool,
        server_options: HostedHookServerOptions,
    ) -> Self {
        Self {
            owner: Arc::new(Owner {
                client,
                server,
                closed: Cancellation::new(),
            }),
            gateway_url,
            server_options,
            has_hooks,
            stream: None,
        }
    }

    /// Preserve explicit SDK settings and attach the hosted endpoint at the
    /// actual wire key, sdkSessionConfig.hooks (`client.ts:2110-2135`).
    pub async fn submit_turn(&mut self, params: Value) -> Result<(), GatewayClientError> {
        let params = pilotdeck_protocol::parse_json(&params.to_string())
            .map_err(|error| GatewayClientError::new("validation_error", error.to_string()))?;
        self.submit_turn_ordered(params).await
    }

    /// Preserve original JSON property order and every UTF-16 code unit in
    /// the submitted message, SDK settings, and hosted matcher configuration.
    pub async fn submit_turn_ordered(
        &mut self,
        mut params: JsValue,
    ) -> Result<(), GatewayClientError> {
        if self.owner.closed.is_cancelled() {
            return Err(GatewayClientError::new(
                "transport_error",
                "Gateway client closed.",
            ));
        }
        if self.stream.is_some() {
            return Err(GatewayClientError::new(
                "validation_error",
                "A turn stream is already active.",
            ));
        }
        let JsValue::Object(object) = &mut params else {
            return Err(GatewayClientError::new(
                "validation_error",
                "submit_turn parameters must be an object.",
            ));
        };
        if self.has_hooks {
            if object
                .get("sdkSessionConfig")
                .is_some_and(|value| !matches!(value, JsValue::Object(_)))
            {
                return Err(GatewayClientError::new(
                    "validation_error",
                    "sdkSessionConfig must be an object.",
                ));
            }
            let endpoint = self
                .owner
                .server
                .start_for_gateway(self.server_options.clone(), Some(&self.gateway_url))
                .await
                .map_err(host_error)?;
            let mut settings = object
                .get("sdkSessionConfig")
                .and_then(JsValue::as_object)
                .cloned()
                .unwrap_or_else(JsObject::new);
            settings.insert("hooks".into(), endpoint.to_value());
            object.insert("sdkSessionConfig".into(), JsValue::Object(settings));
        }
        match self
            .owner
            .client
            .stream_ordered("submit_turn", &params)
            .await
        {
            Ok(stream) => {
                self.stream = Some(stream);
                Ok(())
            }
            Err(error) => {
                self.owner.server.close().await;
                Err(error)
            }
        }
    }

    pub async fn next(&mut self) -> Option<Result<Value, GatewayClientError>> {
        self.next_ordered()
            .await
            .map(|result| result.map(|value| legacy_view(&value)))
    }

    pub async fn next_ordered(&mut self) -> Option<Result<JsValue, GatewayClientError>> {
        let stream = self.stream.as_mut()?;
        let result = tokio::select! {
            _ = self.owner.closed.cancelled() => None,
            result = stream.next() => result,
        };
        if result.as_ref().is_none_or(Result::is_err) {
            self.stream.take();
            self.owner.server.close().await;
        }
        result
    }

    /// Explicit cancellation closes both owned endpoints. Normal EOF closes
    /// only callbacks, allowing the connected session to submit another turn.
    pub async fn close(&mut self) {
        self.stream.take();
        self.owner.close().await;
    }
}
impl Drop for HostedHookSession {
    fn drop(&mut self) {
        self.owner.closed.abort();
        let owner = self.owner.clone();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                owner.close().await;
            });
        }
    }
}
fn host_error(error: super::HostedHookError) -> GatewayClientError {
    GatewayClientError::new(error.code, error.message)
}

/// Compatibility view for the existing serde API. Exact callers use
/// `next_ordered`; replacement decoding occurs only at this explicit boundary.
fn legacy_view(value: &JsValue) -> Value {
    if let Ok(Some(wire)) = pilotdeck_protocol::stringify_json(value)
        && let Ok(value) = serde_json::from_str(&wire)
    {
        return value;
    }
    legacy_view_lossy(value)
}

fn legacy_view_lossy(value: &JsValue) -> Value {
    match value {
        JsValue::Undefined | JsValue::Null => Value::Null,
        JsValue::Bool(value) => Value::Bool(*value),
        JsValue::Number(value) => serde_json::Number::from_f64(*value)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        JsValue::String(value) => Value::String(String::from_utf16_lossy(value.units())),
        JsValue::Array(values) => Value::Array(values.iter().map(legacy_view_lossy).collect()),
        JsValue::Object(object) => Value::Object(
            object
                .own_entries()
                .into_iter()
                .filter(|(_, value)| !value.is_undefined())
                .map(|(key, value)| {
                    (
                        String::from_utf16_lossy(key.units()),
                        legacy_view_lossy(value),
                    )
                })
                .collect(),
        ),
    }
}

#[cfg(test)]
mod tests;
