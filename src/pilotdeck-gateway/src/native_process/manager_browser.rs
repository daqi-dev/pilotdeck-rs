//! Optional host-owned browser manager. No browser state is fabricated.
use crate::runtime::manager::{ManagerBrowserPort, ManagerBrowsers, ManagerError};
use pilotdeck_protocol::JsValue;
use std::sync::Arc;

pub(super) fn request_error(error: ManagerError) -> crate::runtime::connection::RequestError {
    crate::runtime::connection::RequestError {
        code: error.code().unwrap_or("gateway_request_failed").into(),
        message: error.message().clone(),
        details: None,
        validation: None,
    }
}

/// Match TS: the optional provider guard runs before inspecting input; when
/// present the host provider receives exactly the original JS value.
pub(super) fn dispatch(
    provider: Option<Arc<dyn ManagerBrowserPort>>,
    input: &JsValue,
) -> Result<JsValue, ManagerError> {
    match provider {
        Some(provider) => ManagerBrowsers::new(provider).list(input),
        None => ManagerBrowsers::unavailable().list(input),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct Capture(Arc<Mutex<Vec<JsValue>>>);
    impl ManagerBrowserPort for Capture {
        fn list(&self, input: &JsValue) -> Result<JsValue, ManagerError> {
            self.0.lock().unwrap().push(input.clone());
            Ok(pilotdeck_protocol::parse_json(
                r#"{"items":[{"id":"provided","extra":{"answer":42}}]}"#,
            )
            .unwrap())
        }
    }

    #[test]
    fn default_guard_precedes_every_input_and_preserves_ts_error() {
        for input in [
            JsValue::Null,
            pilotdeck_protocol::parse_json(r#"{"projectKey":"/foreign"}"#).unwrap(),
        ] {
            let error = dispatch(None, &input).unwrap_err();
            assert_eq!(error.code(), Some("CAPABILITY_UNAVAILABLE"));
            assert_eq!(
                String::from_utf16_lossy(error.message().units()),
                "Gateway manager browser provider is unavailable."
            );
        }
    }

    #[test]
    fn configured_host_receives_original_foreign_and_null_values() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let provider: Arc<dyn ManagerBrowserPort> = Arc::new(Capture(seen.clone()));
        let foreign = pilotdeck_protocol::parse_json(
            r#"{"projectKey":"/foreign","sessionKey":"x","other":[1,null]}"#,
        )
        .unwrap();
        let result = dispatch(Some(provider.clone()), &foreign).unwrap();
        assert_eq!(
            result,
            pilotdeck_protocol::parse_json(
                r#"{"items":[{"id":"provided","extra":{"answer":42}}]}"#
            )
            .unwrap()
        );
        dispatch(Some(provider), &JsValue::Null).unwrap();
        assert_eq!(*seen.lock().unwrap(), vec![foreign, JsValue::Null]);
    }
}
