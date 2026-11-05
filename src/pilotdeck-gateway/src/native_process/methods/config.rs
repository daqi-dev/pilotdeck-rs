//! S5 session configuration: 7 methods wired through the native request
//! registry. Gate order mirrors InProcessGateway.ts: for the five
//! callback-backed methods (session_model_set, session_model_clear,
//! set_session_thinking, output_styles_list, set_output_style) the TS
//! capability gate is the first statement of the method (IPG.ts:2192,2202,
//! 2365,2375,2380), ahead of every params guard, so a gateway without the
//! callback answers `CAPABILITY_UNAVAILABLE` even for edge inputs.
//! set_permission_mode and seed_read_state have no capability gate ahead of
//! their params guards in TS (IPG.ts:2321-2330,2423-2433), so they validate
//! first. The SDK session registry, permission-mode store and seed-read
//! coordinator are not assembled into the isolated native process, so the
//! capability gates always fire here; the params guards are retained as
//! functions below for the wiring that installs them.
use super::super::requests::Requests;
use crate::runtime::connection::{Call, RequestError};
use pilotdeck_protocol::JsValue;

// Wire names from the gateway frames registry. All 7 pass through admission
// as ordinary session requests; 5 additionally check for an active turn.
pub(super) const METHODS: &[&str] = &[
    "session_model_set",
    "session_model_clear",
    "set_permission_mode",
    "set_session_thinking",
    "output_styles_list",
    "set_output_style",
    "seed_read_state",
];
pub(super) const HANDLER: super::Handler =
    super::Handler::AfterAdmission(Requests::session_config_request);

fn unavailable(message: &str) -> RequestError {
    RequestError {
        code: "CAPABILITY_UNAVAILABLE".into(),
        message: message.into(),
        details: None,
        validation: None,
    }
}

fn error(code: &str, message: &str) -> RequestError {
    RequestError {
        code: code.into(),
        message: message.into(),
        details: None,
        validation: None,
    }
}

/// Return `true` if a JsString value is present and non-empty after JS trim.
fn is_trimmed_non_empty(value: Option<&JsValue>) -> bool {
    value
        .and_then(JsValue::as_string)
        .is_some_and(|s| !crate::metadata::js_trim(&String::from_utf16_lossy(s.units())).is_empty())
}

// Params guards retained for the wiring that installs the SDK session
// registry. Today the capability gate in the request path always fires
// first — matching TS, where the callback check precedes every params
// access (IPG.ts:2192,2202,2365,2380) — so these never run in the request
// path and are unit-tested directly instead. They must be re-verified
// against TS (including the busy gates of IPG.ts:2193-2195,2366-2367,
// 2381-2383, which return with the wiring) when the callbacks land.

/// `sessionKey` must be present and non-empty after JS trim
/// (IPG.ts:2191,2202,2366,2381).
#[cfg_attr(not(test), allow(dead_code))]
fn require_session_key(params: &JsValue) -> Result<(), RequestError> {
    if !is_trimmed_non_empty(params.get("sessionKey")) {
        return Err(error("INVALID_SESSION_KEY", "sessionKey is required."));
    }
    Ok(())
}

/// `name` must be null/absent or a non-blank string (IPG.ts:2383).
#[cfg_attr(not(test), allow(dead_code))]
fn require_output_style_name(params: &JsValue) -> Result<(), RequestError> {
    match params.get("name") {
        None | Some(JsValue::Null) | Some(JsValue::Undefined) => Ok(()),
        Some(name) => {
            let blank = name.as_string().is_some_and(|s| {
                crate::metadata::js_trim(&String::from_utf16_lossy(s.units())).is_empty()
            });
            if blank {
                return Err(error(
                    "INVALID_OUTPUT_STYLE",
                    "name must be non-empty or null.",
                ));
            }
            Ok(())
        }
    }
}

impl Requests {
    pub(in crate::native_process) fn session_config_request(
        &self,
        method: String,
        params: JsValue,
    ) -> Call<'_, Result<JsValue, RequestError>> {
        Box::pin(async move {
            match method.as_str() {
                // The capability gate is the first statement of each of
                // these TS methods (IPG.ts:2192,2202,2365,2375,2380): `if
                // (!this.options.X) throw new DialogGatewayError(
                // "CAPABILITY_UNAVAILABLE", "<method> is unavailable.")`
                // runs before any params access, so a frame with a missing
                // sessionKey, a blank style name or non-string fields still
                // answers the TS unavailable shape, not a validation error.
                // The isolated native process never wires the callbacks, so
                // the gate always fires here; the params guards are
                // retained in `require_session_key` /
                // `require_output_style_name` above.
                "session_model_set"
                | "session_model_clear"
                | "set_session_thinking"
                | "output_styles_list"
                | "set_output_style" => Err(unavailable(&format!("{method} is unavailable."))),

                "set_permission_mode" => {
                    // Mode must be one of the three allowed values before the
                    // busy gate (IPG.ts:2321-2330 checks mode first, then hasActiveTurn).
                    let mode = params.get("mode").and_then(JsValue::as_string);
                    let mode_ok = mode.is_some_and(|m| {
                        m.equals_str("default")
                            || m.equals_str("plan")
                            || m.equals_str("bypassPermissions")
                    });
                    if !mode_ok {
                        let mode_display = params
                            .get("mode")
                            .map(|v| {
                                String::from_utf16_lossy(
                                    &crate::js_value::to_string_units(v).unwrap_or_default(),
                                )
                            })
                            .unwrap_or_default();
                        return Err(error(
                            "INVALID_PERMISSION_MODE",
                            &format!("Unsupported permission mode: {mode_display}"),
                        ));
                    }
                    let session_key = params
                        .get("sessionKey")
                        .and_then(JsValue::as_string)
                        .cloned();
                    if let Some(ref key) = session_key
                        && self.owner.has_active_turn(key)
                    {
                        return Err(error(
                            "SESSION_BUSY",
                            "Cannot change permission mode during an active turn.",
                        ));
                    }
                    Err(unavailable(
                        "Permission mode configuration is not installed on this isolated native process.",
                    ))
                }

                "seed_read_state" => {
                    // sessionKey, path required; mtime must be a non-negative integer;
                    // then busy check (IPG.ts:2423-2442).
                    if !is_trimmed_non_empty(params.get("sessionKey")) {
                        return Err(error("INVALID_SESSION_KEY", "sessionKey is required."));
                    }
                    if !is_trimmed_non_empty(params.get("path")) {
                        return Err(error("INVALID_FILE_PATH", "path is required."));
                    }
                    // mtime must be a finite non-negative integer (ms).
                    let mtime_ok = params
                        .get("mtime")
                        .and_then(JsValue::as_number)
                        .is_some_and(|n| n.is_finite() && n.fract() == 0.0 && n >= 0.0);
                    if !mtime_ok {
                        return Err(error(
                            "INVALID_FILE_MTIME",
                            "mtime must be a non-negative integer in milliseconds.",
                        ));
                    }
                    let session_key = params
                        .get("sessionKey")
                        .and_then(JsValue::as_string)
                        .cloned()
                        .unwrap();
                    if self.owner.has_active_turn(&session_key) {
                        return Err(error(
                            "SESSION_BUSY",
                            "Cannot seed file read state while a turn is active.",
                        ));
                    }
                    Err(unavailable(
                        "Seed read state is not installed on this isolated native process.",
                    ))
                }

                _ => unreachable!("config family checked against METHODS"),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{require_output_style_name, require_session_key};
    use crate::metadata::object;
    use crate::native_process::management_tests::setup;
    use crate::negative_assert_eq;

    // ── session_model_set ────────────────────────────────────────────────────

    // Missing sessionKey still hits the capability gate first: TS checks
    // `!this.options.sessionModelSet` before any params access (IPG.ts:2192),
    // so the wire answer is the unavailable shape, not INVALID_SESSION_KEY.
    // Mutation: moving a sessionKey guard ahead of the gate flips the code.
    #[tokio::test]
    async fn session_model_set_missing_session_key_hits_capability_gate_first() {
        let (_s, r) = std::panic::catch_unwind(setup).expect("#1310 fixture");
        let params = object([(
            "model",
            pilotdeck_protocol::JsValue::Object(Default::default()),
        )]);
        let result = r
            .session_config_request("session_model_set".into(), params)
            .await;
        let err = result.unwrap_err();
        negative_assert_eq!(
            err.code.as_str(),
            "CAPABILITY_UNAVAILABLE",
            "#1310 session_model_set missing sessionKey hits capability gate first"
        );
        negative_assert_eq!(
            err.message.equals_str("session_model_set is unavailable."),
            true,
            "#1310 session_model_set unavailable message matches TS shape"
        );
        r.owner.shutdown().await.expect("#1310 shutdown");
        r.project.dispose().await.expect("#1310 dispose");
    }

    #[tokio::test]
    async fn session_model_set_unavailable_with_valid_key() {
        let (_s, r) = std::panic::catch_unwind(setup).expect("#1310 fixture");
        let params = object([("sessionKey", "s1".into())]);
        let result = r
            .session_config_request("session_model_set".into(), params)
            .await;
        let err = result.unwrap_err();
        negative_assert_eq!(
            err.code.as_str(),
            "CAPABILITY_UNAVAILABLE",
            "#1310 session_model_set unavailable"
        );
        negative_assert_eq!(
            err.message.equals_str("session_model_set is unavailable."),
            true,
            "#1310 session_model_set unavailable message matches TS shape"
        );
        r.owner.shutdown().await.expect("#1310 shutdown");
        r.project.dispose().await.expect("#1310 dispose");
    }

    // ── session_model_clear ──────────────────────────────────────────────────

    // Missing sessionKey hits the capability gate first (IPG.ts:2202
    // precedes the params access of the reserveTranscriptWrite path).
    // Mutation: moving a sessionKey guard ahead of the gate flips the code.
    #[tokio::test]
    async fn session_model_clear_missing_session_key_hits_capability_gate_first() {
        let (_s, r) = std::panic::catch_unwind(setup).expect("#1310 fixture");
        let params = pilotdeck_protocol::JsValue::Object(Default::default());
        let result = r
            .session_config_request("session_model_clear".into(), params)
            .await;
        let err = result.unwrap_err();
        negative_assert_eq!(
            err.code.as_str(),
            "CAPABILITY_UNAVAILABLE",
            "#1310 session_model_clear missing sessionKey hits capability gate first"
        );
        negative_assert_eq!(
            err.message
                .equals_str("session_model_clear is unavailable."),
            true,
            "#1310 session_model_clear unavailable message matches TS shape"
        );
        r.owner.shutdown().await.expect("#1310 shutdown");
        r.project.dispose().await.expect("#1310 dispose");
    }

    #[tokio::test]
    async fn session_model_clear_unavailable_with_valid_key() {
        let (_s, r) = std::panic::catch_unwind(setup).expect("#1310 fixture");
        let params = object([("sessionKey", "s1".into())]);
        let result = r
            .session_config_request("session_model_clear".into(), params)
            .await;
        let err = result.unwrap_err();
        negative_assert_eq!(
            err.code.as_str(),
            "CAPABILITY_UNAVAILABLE",
            "#1310 session_model_clear unavailable"
        );
        negative_assert_eq!(
            err.message
                .equals_str("session_model_clear is unavailable."),
            true,
            "#1310 session_model_clear unavailable message matches TS shape"
        );
        r.owner.shutdown().await.expect("#1310 shutdown");
        r.project.dispose().await.expect("#1310 dispose");
    }

    // ── set_permission_mode ──────────────────────────────────────────────────

    #[tokio::test]
    async fn set_permission_mode_rejects_invalid_mode() {
        let (_s, r) = std::panic::catch_unwind(setup).expect("#1310 fixture");
        let params = object([("sessionKey", "s1".into()), ("mode", "admin".into())]);
        let result = r
            .session_config_request("set_permission_mode".into(), params)
            .await;
        negative_assert_eq!(
            result.unwrap_err().code.as_str(),
            "INVALID_PERMISSION_MODE",
            "#1310 set_permission_mode invalid mode"
        );
        r.owner.shutdown().await.expect("#1310 shutdown");
        r.project.dispose().await.expect("#1310 dispose");
    }

    #[tokio::test]
    async fn set_permission_mode_unavailable_with_valid_mode() {
        let (_s, r) = std::panic::catch_unwind(setup).expect("#1310 fixture");
        let params = object([("sessionKey", "s1".into()), ("mode", "plan".into())]);
        let result = r
            .session_config_request("set_permission_mode".into(), params)
            .await;
        negative_assert_eq!(
            result.unwrap_err().code.as_str(),
            "CAPABILITY_UNAVAILABLE",
            "#1310 set_permission_mode unavailable"
        );
        r.owner.shutdown().await.expect("#1310 shutdown");
        r.project.dispose().await.expect("#1310 dispose");
    }

    // ── set_session_thinking ─────────────────────────────────────────────────

    // Missing sessionKey hits the capability gate first: TS checks
    // `!this.options.setSessionThinking` before the sessionKey guard
    // (IPG.ts:2365 precedes IPG.ts:2366). Mutation: moving a sessionKey
    // guard ahead of the gate flips the code.
    #[tokio::test]
    async fn set_session_thinking_missing_session_key_hits_capability_gate_first() {
        let (_s, r) = std::panic::catch_unwind(setup).expect("#1310 fixture");
        let params = object([("thinking", pilotdeck_protocol::JsValue::Null)]);
        let result = r
            .session_config_request("set_session_thinking".into(), params)
            .await;
        let err = result.unwrap_err();
        negative_assert_eq!(
            err.code.as_str(),
            "CAPABILITY_UNAVAILABLE",
            "#1310 set_session_thinking missing sessionKey hits capability gate first"
        );
        negative_assert_eq!(
            err.message
                .equals_str("set_session_thinking is unavailable."),
            true,
            "#1310 set_session_thinking unavailable message matches TS shape"
        );
        r.owner.shutdown().await.expect("#1310 shutdown");
        r.project.dispose().await.expect("#1310 dispose");
    }

    #[tokio::test]
    async fn set_session_thinking_unavailable_with_valid_key() {
        let (_s, r) = std::panic::catch_unwind(setup).expect("#1310 fixture");
        let params = object([("sessionKey", "s1".into())]);
        let result = r
            .session_config_request("set_session_thinking".into(), params)
            .await;
        let err = result.unwrap_err();
        negative_assert_eq!(
            err.code.as_str(),
            "CAPABILITY_UNAVAILABLE",
            "#1310 set_session_thinking unavailable"
        );
        negative_assert_eq!(
            err.message
                .equals_str("set_session_thinking is unavailable."),
            true,
            "#1310 set_session_thinking unavailable message matches TS shape"
        );
        r.owner.shutdown().await.expect("#1310 shutdown");
        r.project.dispose().await.expect("#1310 dispose");
    }

    // ── output_styles_list ───────────────────────────────────────────────────

    #[tokio::test]
    async fn output_styles_list_unavailable() {
        let (_s, r) = std::panic::catch_unwind(setup).expect("#1310 fixture");
        let params = pilotdeck_protocol::JsValue::Object(Default::default());
        let result = r
            .session_config_request("output_styles_list".into(), params)
            .await;
        let err = result.unwrap_err();
        negative_assert_eq!(
            err.code.as_str(),
            "CAPABILITY_UNAVAILABLE",
            "#1310 output_styles_list unavailable"
        );
        negative_assert_eq!(
            err.message.equals_str("output_styles_list is unavailable."),
            true,
            "#1310 output_styles_list unavailable message matches TS shape"
        );
        r.owner.shutdown().await.expect("#1310 shutdown");
        r.project.dispose().await.expect("#1310 dispose");
    }

    // ── set_output_style ─────────────────────────────────────────────────────

    // Missing sessionKey hits the capability gate first: TS checks
    // `!this.options.setOutputStyle` before the sessionKey guard
    // (IPG.ts:2380 precedes IPG.ts:2381). Mutation: moving a sessionKey
    // guard ahead of the gate flips the code.
    #[tokio::test]
    async fn set_output_style_missing_session_key_hits_capability_gate_first() {
        let (_s, r) = std::panic::catch_unwind(setup).expect("#1310 fixture");
        let params = object([("name", "compact".into())]);
        let result = r
            .session_config_request("set_output_style".into(), params)
            .await;
        let err = result.unwrap_err();
        negative_assert_eq!(
            err.code.as_str(),
            "CAPABILITY_UNAVAILABLE",
            "#1310 set_output_style missing sessionKey hits capability gate first"
        );
        negative_assert_eq!(
            err.message.equals_str("set_output_style is unavailable."),
            true,
            "#1310 set_output_style unavailable message matches TS shape"
        );
        r.owner.shutdown().await.expect("#1310 shutdown");
        r.project.dispose().await.expect("#1310 dispose");
    }

    // A blank style name also hits the capability gate first: the name
    // guard (IPG.ts:2383) sits behind the capability check (IPG.ts:2380).
    // Mutation: moving the name guard ahead of the gate flips the code.
    #[tokio::test]
    async fn set_output_style_blank_name_hits_capability_gate_first() {
        let (_s, r) = std::panic::catch_unwind(setup).expect("#1310 fixture");
        let params = object([("sessionKey", "s1".into()), ("name", "  ".into())]);
        let result = r
            .session_config_request("set_output_style".into(), params)
            .await;
        let err = result.unwrap_err();
        negative_assert_eq!(
            err.code.as_str(),
            "CAPABILITY_UNAVAILABLE",
            "#1310 set_output_style blank name hits capability gate first"
        );
        negative_assert_eq!(
            err.message.equals_str("set_output_style is unavailable."),
            true,
            "#1310 set_output_style unavailable message matches TS shape"
        );
        r.owner.shutdown().await.expect("#1310 shutdown");
        r.project.dispose().await.expect("#1310 dispose");
    }

    #[tokio::test]
    async fn set_output_style_unavailable_with_null_name() {
        let (_s, r) = std::panic::catch_unwind(setup).expect("#1310 fixture");
        let params = object([
            ("sessionKey", "s1".into()),
            ("name", pilotdeck_protocol::JsValue::Null),
        ]);
        let result = r
            .session_config_request("set_output_style".into(), params)
            .await;
        let err = result.unwrap_err();
        negative_assert_eq!(
            err.code.as_str(),
            "CAPABILITY_UNAVAILABLE",
            "#1310 set_output_style null name unavailable"
        );
        negative_assert_eq!(
            err.message.equals_str("set_output_style is unavailable."),
            true,
            "#1310 set_output_style unavailable message matches TS shape"
        );
        r.owner.shutdown().await.expect("#1310 shutdown");
        r.project.dispose().await.expect("#1310 dispose");
    }

    // ── seed_read_state ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn seed_read_state_requires_session_key() {
        let (_s, r) = std::panic::catch_unwind(setup).expect("#1310 fixture");
        let params = object([
            ("path", "/tmp/a.txt".into()),
            ("mtime", pilotdeck_protocol::JsValue::Number(0.0)),
        ]);
        let result = r
            .session_config_request("seed_read_state".into(), params)
            .await;
        negative_assert_eq!(
            result.unwrap_err().code.as_str(),
            "INVALID_SESSION_KEY",
            "#1310 seed_read_state missing sessionKey"
        );
        r.owner.shutdown().await.expect("#1310 shutdown");
        r.project.dispose().await.expect("#1310 dispose");
    }

    #[tokio::test]
    async fn seed_read_state_requires_path() {
        let (_s, r) = std::panic::catch_unwind(setup).expect("#1310 fixture");
        let params = object([
            ("sessionKey", "s1".into()),
            ("mtime", pilotdeck_protocol::JsValue::Number(0.0)),
        ]);
        let result = r
            .session_config_request("seed_read_state".into(), params)
            .await;
        negative_assert_eq!(
            result.unwrap_err().code.as_str(),
            "INVALID_FILE_PATH",
            "#1310 seed_read_state missing path"
        );
        r.owner.shutdown().await.expect("#1310 shutdown");
        r.project.dispose().await.expect("#1310 dispose");
    }

    #[tokio::test]
    async fn seed_read_state_rejects_negative_mtime() {
        let (_s, r) = std::panic::catch_unwind(setup).expect("#1310 fixture");
        let params = object([
            ("sessionKey", "s1".into()),
            ("path", "/tmp/a.txt".into()),
            ("mtime", pilotdeck_protocol::JsValue::Number(-1.0)),
        ]);
        let result = r
            .session_config_request("seed_read_state".into(), params)
            .await;
        negative_assert_eq!(
            result.unwrap_err().code.as_str(),
            "INVALID_FILE_MTIME",
            "#1310 seed_read_state negative mtime"
        );
        r.owner.shutdown().await.expect("#1310 shutdown");
        r.project.dispose().await.expect("#1310 dispose");
    }

    #[tokio::test]
    async fn seed_read_state_rejects_fractional_mtime() {
        let (_s, r) = std::panic::catch_unwind(setup).expect("#1310 fixture");
        let params = object([
            ("sessionKey", "s1".into()),
            ("path", "/tmp/a.txt".into()),
            ("mtime", pilotdeck_protocol::JsValue::Number(1.5)),
        ]);
        let result = r
            .session_config_request("seed_read_state".into(), params)
            .await;
        negative_assert_eq!(
            result.unwrap_err().code.as_str(),
            "INVALID_FILE_MTIME",
            "#1310 seed_read_state fractional mtime"
        );
        r.owner.shutdown().await.expect("#1310 shutdown");
        r.project.dispose().await.expect("#1310 dispose");
    }

    #[tokio::test]
    async fn seed_read_state_unavailable_with_valid_params() {
        let (_s, r) = std::panic::catch_unwind(setup).expect("#1310 fixture");
        let params = object([
            ("sessionKey", "s1".into()),
            ("path", "/tmp/a.txt".into()),
            (
                "mtime",
                pilotdeck_protocol::JsValue::Number(1_700_000_000.0),
            ),
        ]);
        let result = r
            .session_config_request("seed_read_state".into(), params)
            .await;
        negative_assert_eq!(
            result.unwrap_err().code.as_str(),
            "CAPABILITY_UNAVAILABLE",
            "#1310 seed_read_state unavailable"
        );
        r.owner.shutdown().await.expect("#1310 shutdown");
        r.project.dispose().await.expect("#1310 dispose");
    }

    // ── retained params guards (unit-tested directly) ────────────────────────

    #[test]
    fn require_session_key_rejects_missing_key() {
        let params = pilotdeck_protocol::JsValue::Object(Default::default());
        let err = require_session_key(&params).unwrap_err();
        negative_assert_eq!(
            err.code.as_str(),
            "INVALID_SESSION_KEY",
            "#1310 require_session_key missing sessionKey"
        );
    }

    #[test]
    fn require_session_key_rejects_blank_key() {
        let params = object([("sessionKey", "  ".into())]);
        let err = require_session_key(&params).unwrap_err();
        negative_assert_eq!(
            err.code.as_str(),
            "INVALID_SESSION_KEY",
            "#1310 require_session_key blank sessionKey"
        );
    }

    #[test]
    fn require_session_key_rejects_non_string_key() {
        let params = object([("sessionKey", pilotdeck_protocol::JsValue::Number(7.0))]);
        let err = require_session_key(&params).unwrap_err();
        negative_assert_eq!(
            err.code.as_str(),
            "INVALID_SESSION_KEY",
            "#1310 require_session_key non-string sessionKey"
        );
    }

    #[test]
    fn require_session_key_accepts_present_key() {
        let params = object([("sessionKey", "s1".into())]);
        assert!(
            require_session_key(&params).is_ok(),
            "#1310 require_session_key accepts present sessionKey"
        );
    }

    #[test]
    fn require_output_style_name_rejects_blank_name() {
        let params = object([("name", "  ".into())]);
        let err = require_output_style_name(&params).unwrap_err();
        negative_assert_eq!(
            err.code.as_str(),
            "INVALID_OUTPUT_STYLE",
            "#1310 require_output_style_name blank name"
        );
    }

    #[test]
    fn require_output_style_name_accepts_null_missing_and_nonblank() {
        let null_name = object([("name", pilotdeck_protocol::JsValue::Null)]);
        assert!(
            require_output_style_name(&null_name).is_ok(),
            "#1310 require_output_style_name accepts null name"
        );
        let no_name = pilotdeck_protocol::JsValue::Object(Default::default());
        assert!(
            require_output_style_name(&no_name).is_ok(),
            "#1310 require_output_style_name accepts missing name"
        );
        let named = object([("name", "compact".into())]);
        assert!(
            require_output_style_name(&named).is_ok(),
            "#1310 require_output_style_name accepts non-blank name"
        );
    }
}
