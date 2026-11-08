use super::common::*;
use crate::{
    host::HostError,
    module_host::approvals::native_http::{JsonFailure, JsonRequest, JsonTransport},
};
use pilotdeck_protocol::{JsString, JsValue};
use std::{sync::Arc, time::Duration};
pub(crate) struct Discovery {
    endpoint: String,
    agent: JsString,
    key: String,
    path: Option<String>,
    timeout: Duration,
    http: Arc<dyn JsonTransport>,
}
impl Discovery {
    pub(crate) fn from_profile(profile: &JsValue, http: Arc<dyn JsonTransport>) -> Option<Self> {
        let endpoint = field(profile, "discoveryEndpoint")
            .as_string()?
            .to_utf8()
            .ok()?;
        let agent = field(profile, "discoveryAgentId").as_string()?.clone();
        let key = field(profile, "discoveryApiKey")
            .as_string()?
            .to_utf8()
            .ok()?;
        Some(Self {
            endpoint: endpoint.trim_end_matches('/').into(),
            agent,
            key,
            path: field(profile, "discoveryPath")
                .as_string()
                .map(|s| String::from_utf16_lossy(s.units())),
            timeout: field(profile, "discoveryTimeoutMs")
                .as_number()
                .or_else(|| field(profile, "timeoutMs").as_number())
                .map(crate::runtime::network::node_timer_duration)
                .unwrap_or(Duration::from_secs(30)),
            http,
        })
    }
    pub(crate) async fn route(
        &self,
        message: JsString,
        session: JsString,
        signal: &crate::host::Cancellation,
    ) -> Result<Option<JsString>, HostError> {
        let path = match &self.path {
            Some(path) => path.clone(),
            None => format!("/agents/{}/sops:route", encode(&self.agent)?),
        };
        let url = url::Url::parse(&format!("{}{path}", self.endpoint)).map_err(|_| {
            error(
                "SOP_DISCOVERY_UNAVAILABLE",
                "StaffDeck SOP discovery request failed: Failed to parse URL",
            )
        })?;
        let body = crate::metadata::object([
            ("message", JsValue::String(message)),
            ("model_source", "pilotdeck_host".into()),
            ("session_id", JsValue::String(session)),
        ]);
        let pending = self.http.request(JsonRequest {
            method: "POST",
            url,
            bearer: Some(self.key.clone()),
            body: Some(json(&body)),
            parse_error_body: true,
            redirect_error: false,
            timeout: self.timeout,
        });
        let response = tokio::select! {biased;_=signal.cancelled()=>return Err(error("SOP_DISCOVERY_UNAVAILABLE","StaffDeck SOP discovery request failed: This operation was aborted")),v=pending=>v.map_err(|e|{let(JsonFailure::Transport(m)|JsonFailure::Body(m))=e;error("SOP_DISCOVERY_UNAVAILABLE",format!("StaffDeck SOP discovery request failed: {m}").as_str())})?};
        if !(200..300).contains(&response.status) {
            let code = field(&response.body, "code")
                .as_string()
                .map(|s| String::from_utf16_lossy(s.units()))
                .unwrap_or_else(|| "SOP_DISCOVERY_FAILED".into());
            return Err(error(
                &code,
                format!("StaffDeck SOP discovery returned HTTP {}.", response.status).as_str(),
            ));
        }
        if response.body.as_object().is_none()
            || field(&response.body, "decision").as_string().is_none()
        {
            return Err(error(
                "SOP_DISCOVERY_PROTOCOL",
                "StaffDeck SOP discovery returned an invalid route response.",
            ));
        }
        let selected = field(&response.body, "selected_sop_id")
            .as_string()
            .filter(|s| !s.is_empty())
            .cloned();
        if let Some(selected) = &selected {
            if let JsValue::Array(candidates) = field(&response.body, "candidate_sop_ids") {
                if !candidates.iter().any(|v| v.as_string() == Some(selected)) {
                    return Err(error("SOP_DISCOVERY_SELECTION_NOT_VISIBLE",format!("StaffDeck SOP discovery selected '{}', but StaffDeck did not expose it as a candidate.",String::from_utf16_lossy(selected.units())).as_str()));
                }
            }
        }
        Ok(selected)
    }
}
pub(super) fn encode(value: &JsString) -> Result<String, HostError> {
    let s = value
        .to_utf8()
        .map_err(|_| error("gateway_request_failed", "URI malformed"))?;
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
            out.push(char::from(b));
        } else {
            use std::fmt::Write;
            let _ = write!(out, "%{b:02X}");
        }
    }
    Ok(out)
}
