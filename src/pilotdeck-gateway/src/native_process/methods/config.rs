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
use pilotdeck_protocol::{JsString, JsValue};

// Wire names from the gateway frames registry. All 7 pass through admission
// as ordinary session requests; set_permission_mode and seed_read_state
// additionally check for an active turn (the other 5 answer the capability
// gate before any busy gate, mirroring IPG.ts:2192,2202,2365,2375,2380).
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

/// True when a JS string is non-empty after JS trim — the shared shape of the
/// TS `input.X?.trim()` required-string gates (IPG.ts:2191,2202,2366,2381,
/// 2427). Lone surrogates survive the TS check, so the conversion is lossy:
/// `to_utf8` would reject a key that TS accepts (#1455 probe4, PilotDeck
/// cc7c4e65: `{"label":"thinking-lone-surrogate-key","value":{"applied":"callback"}}`).
/// Session configuration and session directory share this one implementation
/// (#1455 G7); it lives here rather than in `methods/mod.rs` because the
/// sibling #1465/#1316 PRs are rewriting that file's declaration block.
pub(super) fn trimmed_string_is_non_empty(value: &JsString) -> bool {
    !crate::metadata::js_trim(&String::from_utf16_lossy(value.units())).is_empty()
}

/// True when `value` is a present string that is non-empty after JS trim.
fn is_trimmed_non_empty(value: Option<&JsValue>) -> bool {
    value
        .and_then(JsValue::as_string)
        .is_some_and(trimmed_string_is_non_empty)
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

/// `name` must be null/absent or a non-blank string (IPG.ts:2383). TS has no
/// clean error for non-string names — `input.name.trim` throws a bare
/// TypeError, and an absent `name` property also TypeErrors
/// (`input.name !== null && !input.name.trim()`), so this guard only mirrors
/// the one DialogGatewayError the TS method can raise (#1455 probe2, Node
/// v22.22.0 / PilotDeck cc7c4e65). The non-string/absent shapes are
/// registered in A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001-R2.
#[cfg_attr(not(test), allow(dead_code))]
fn require_output_style_name(params: &JsValue) -> Result<(), RequestError> {
    match params.get("name") {
        None | Some(JsValue::Null) | Some(JsValue::Undefined) => Ok(()),
        Some(name) => {
            // Same `?.trim()` as the sessionKey gates (#1455 G7) — an inline
            // copy here would let the ECMAScript WhiteSpace set drift.
            if let Some(s) = name.as_string()
                && !trimmed_string_is_non_empty(s)
            {
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
                // `require_output_style_name` above. session_model_set and
                // session_model_clear carry no hasActiveTurn gate in TS
                // either (IPG.ts:2192-2207; #1455 probe3 busyCalls=0) —
                // their busy semantics is the turn-replacement transcript
                // write reservation, which this process does not assemble
                // (registered as unimplemented in
                // A7-W5-NATIVE-SESSION-CONFIG-CAPABILITY-001-R2).
                "session_model_set"
                | "session_model_clear"
                | "set_session_thinking"
                | "output_styles_list"
                | "set_output_style" => Err(unavailable(&format!("{method} is unavailable."))),

                "set_permission_mode" => {
                    // Mode must be one of the three allowed values before the
                    // busy gate (IPG.ts:2321-2330 checks mode first, then
                    // hasActiveTurn; #1455 probe1 confirms the order under an
                    // active turn). TS never validates sessionKey here: a
                    // missing, blank or non-string key still reaches
                    // `hasActiveTurn(input.sessionKey)` — which receives the
                    // raw value — and then the permission-mode store
                    // (#1455 probe1). The Rust side skips the busy lookup
                    // only when the key is absent or not a string, which no
                    // turn can ever be reserved under, so the observable
                    // outcome matches: no busy, fall through to unavailable.
                    let mode = params.get("mode").and_then(JsValue::as_string);
                    let mode_ok = mode.is_some_and(|m| {
                        m.equals_str("default")
                            || m.equals_str("plan")
                            || m.equals_str("bypassPermissions")
                    });
                    if !mode_ok {
                        // TS interpolates `input.mode` into the message, so an
                        // absent field renders "undefined" (probe1:
                        // missing-mode → "Unsupported permission mode:
                        // undefined") while a non-string renders its JS
                        // ToString form (42 → "42"). A missing key must not
                        // render as the empty string (#1455 r1).
                        let mode_display = String::from_utf16_lossy(
                            &crate::js_value::to_string_units(
                                params.get("mode").unwrap_or(&JsValue::Undefined),
                            )
                            .unwrap_or_default(),
                        );
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
                    // One lookup supplies both the gate and the key the busy
                    // check needs, so no second `expect` can turn a future
                    // gate edit into a panic in the request path (#1455 r1).
                    let session_key = params
                        .get("sessionKey")
                        .and_then(JsValue::as_string)
                        .filter(|s| trimmed_string_is_non_empty(s))
                        .cloned()
                        .ok_or_else(|| error("INVALID_SESSION_KEY", "sessionKey is required."))?;
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
        let err = result.expect_err("#1310 set_permission_mode rejects invalid mode");
        negative_assert_eq!(
            err.code.as_str(),
            "INVALID_PERMISSION_MODE",
            "#1310 set_permission_mode invalid mode"
        );
        r.owner.shutdown().await.expect("#1310 shutdown");
        r.project.dispose().await.expect("#1310 dispose");
    }

    // #1455 r1: TS interpolates `input.mode` into the message, so an absent
    // field renders "undefined" (probe1, missing-mode, all four scenarios)
    // and never the empty string the Rust port used to produce.
    #[tokio::test]
    async fn set_permission_mode_missing_mode_message_matches_ts() {
        let (_s, r) = std::panic::catch_unwind(setup).expect("#1310 fixture");
        let params = object([("sessionKey", "s1".into())]);
        let result = r
            .session_config_request("set_permission_mode".into(), params)
            .await;
        let err = result.expect_err("#1455 set_permission_mode missing mode");
        negative_assert_eq!(
            err.code.as_str(),
            "INVALID_PERMISSION_MODE",
            "#1455 set_permission_mode missing mode"
        );
        negative_assert_eq!(
            err.message
                .equals_str("Unsupported permission mode: undefined"),
            true,
            "#1455 set_permission_mode missing mode renders undefined"
        );
        r.owner.shutdown().await.expect("#1310 shutdown");
        r.project.dispose().await.expect("#1310 dispose");
    }

    // The same interpolation for a non-string value: JS ToString(42) is "42"
    // (probe1, nonstring-mode).
    #[tokio::test]
    async fn set_permission_mode_nonstring_mode_message_matches_ts() {
        let (_s, r) = std::panic::catch_unwind(setup).expect("#1310 fixture");
        let params = object([
            ("sessionKey", "s1".into()),
            ("mode", pilotdeck_protocol::JsValue::Number(42.0)),
        ]);
        let result = r
            .session_config_request("set_permission_mode".into(), params)
            .await;
        let err = result.expect_err("#1455 set_permission_mode non-string mode");
        negative_assert_eq!(
            err.message.equals_str("Unsupported permission mode: 42"),
            true,
            "#1455 set_permission_mode non-string mode renders its JS ToString form"
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
        let err = result.expect_err("#1310 set_permission_mode unavailable");
        negative_assert_eq!(
            err.code.as_str(),
            "CAPABILITY_UNAVAILABLE",
            "#1310 set_permission_mode unavailable"
        );
        r.owner.shutdown().await.expect("#1310 shutdown");
        r.project.dispose().await.expect("#1310 dispose");
    }

    // #1455 G1: TS never validates sessionKey in setPermissionMode — a
    // missing key still reaches hasActiveTurn and then the permission-mode
    // store and answers {applied:true} (probe1: missing-key → applied, no
    // INVALID_SESSION_KEY). The isolated process has no store, so the wire
    // answer is the unavailable shape; it must never become
    // INVALID_SESSION_KEY.
    #[tokio::test]
    async fn set_permission_mode_missing_session_key_hits_unavailable() {
        let (_s, r) = std::panic::catch_unwind(setup).expect("#1310 fixture");
        let params = object([("mode", "plan".into())]);
        let result = r
            .session_config_request("set_permission_mode".into(), params)
            .await;
        let err = result.expect_err("#1455 set_permission_mode missing sessionKey");
        negative_assert_eq!(
            err.code.as_str(),
            "CAPABILITY_UNAVAILABLE",
            "#1455 set_permission_mode missing sessionKey is unavailable, not INVALID_SESSION_KEY"
        );
        negative_assert_eq!(
            err.message.equals_str(
                "Permission mode configuration is not installed on this isolated native process."
            ),
            true,
            "#1455 set_permission_mode terminal message matches the unavailable shape"
        );
        r.owner.shutdown().await.expect("#1310 shutdown");
        r.project.dispose().await.expect("#1310 dispose");
    }

    // #1455 G5/G6: the busy gate must fire under a reserved turn
    // (TS probe3: SESSION_BUSY "Cannot change permission mode during an
    // active turn.").
    #[tokio::test]
    async fn set_permission_mode_busy_during_active_turn() {
        let (_s, r) = std::panic::catch_unwind(setup).expect("#1310 fixture");
        let key: pilotdeck_protocol::JsString = "s1".into();
        let run: pilotdeck_protocol::JsString = "run-1".into();
        let reservation = r
            .owner
            .begin_turn(&key, &run)
            .expect("#1455 begin turn reservation");
        let params = object([("sessionKey", "s1".into()), ("mode", "plan".into())]);
        let result = r
            .session_config_request("set_permission_mode".into(), params)
            .await;
        let err = result.expect_err("#1455 set_permission_mode busy during active turn");
        negative_assert_eq!(
            err.code.as_str(),
            "SESSION_BUSY",
            "#1455 set_permission_mode active turn answers SESSION_BUSY"
        );
        negative_assert_eq!(
            err.message
                .equals_str("Cannot change permission mode during an active turn."),
            true,
            "#1455 set_permission_mode busy message matches the TS shape"
        );
        r.owner.end_turn(&key, Some(&run), Some(&reservation));
        r.owner.shutdown().await.expect("#1310 shutdown");
        r.project.dispose().await.expect("#1310 dispose");
    }

    // #1455 G5: the mode gate precedes the busy gate (probe1: invalid mode
    // under an active turn answers INVALID_PERMISSION_MODE, busyKeys=[]).
    #[tokio::test]
    async fn set_permission_mode_invalid_mode_wins_over_busy() {
        let (_s, r) = std::panic::catch_unwind(setup).expect("#1310 fixture");
        let key: pilotdeck_protocol::JsString = "s1".into();
        let run: pilotdeck_protocol::JsString = "run-1".into();
        let reservation = r
            .owner
            .begin_turn(&key, &run)
            .expect("#1455 begin turn reservation");
        let params = object([("sessionKey", "s1".into()), ("mode", "admin".into())]);
        let result = r
            .session_config_request("set_permission_mode".into(), params)
            .await;
        let err = result.expect_err("#1455 set_permission_mode invalid mode under turn");
        negative_assert_eq!(
            err.code.as_str(),
            "INVALID_PERMISSION_MODE",
            "#1455 set_permission_mode mode gate precedes the busy gate"
        );
        r.owner.end_turn(&key, Some(&run), Some(&reservation));
        r.owner.shutdown().await.expect("#1310 shutdown");
        r.project.dispose().await.expect("#1310 dispose");
    }

    // #1455 G1: a blank "  " key still reaches the busy gate on the TS side
    // (probe1: busyKeys:["  "] → SESSION_BUSY), so the Rust gate must look
    // the trimmed-looking key up instead of treating it as absent.
    #[tokio::test]
    async fn set_permission_mode_blank_session_key_with_active_turn_is_busy() {
        let (_s, r) = std::panic::catch_unwind(setup).expect("#1310 fixture");
        let key: pilotdeck_protocol::JsString = "  ".into();
        let run: pilotdeck_protocol::JsString = "run-1".into();
        let reservation = r
            .owner
            .begin_turn(&key, &run)
            .expect("#1455 begin turn on the blank key");
        let params = object([("sessionKey", "  ".into()), ("mode", "plan".into())]);
        let result = r
            .session_config_request("set_permission_mode".into(), params)
            .await;
        let err = result.expect_err("#1455 set_permission_mode blank key busy");
        negative_assert_eq!(
            err.code.as_str(),
            "SESSION_BUSY",
            "#1455 set_permission_mode blank sessionKey still consults the busy gate"
        );
        r.owner.end_turn(&key, Some(&run), Some(&reservation));
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
        let err = result.expect_err("#1310 seed_read_state missing sessionKey");
        negative_assert_eq!(
            err.code.as_str(),
            "INVALID_SESSION_KEY",
            "#1310 seed_read_state missing sessionKey"
        );
        r.owner.shutdown().await.expect("#1310 shutdown");
        r.project.dispose().await.expect("#1310 dispose");
    }

    // A blank key is rejected by the same `?.trim()` gate the shared helper
    // supplies; the missing-key case above cannot tell the filter apart from
    // an absent key (#1455 r1 negative-control binding).
    #[tokio::test]
    async fn seed_read_state_rejects_blank_session_key() {
        let (_s, r) = std::panic::catch_unwind(setup).expect("#1310 fixture");
        let params = object([
            ("sessionKey", "  ".into()),
            ("path", "/tmp/a.txt".into()),
            ("mtime", pilotdeck_protocol::JsValue::Number(0.0)),
        ]);
        let result = r
            .session_config_request("seed_read_state".into(), params)
            .await;
        let err = result.expect_err("#1455 seed_read_state rejects the blank key");
        negative_assert_eq!(
            err.code.as_str(),
            "INVALID_SESSION_KEY",
            "#1455 seed_read_state rejects the blank key"
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
        let err = result.expect_err("#1310 seed_read_state missing path");
        negative_assert_eq!(
            err.code.as_str(),
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
        let err = result.expect_err("#1310 seed_read_state negative mtime");
        negative_assert_eq!(
            err.code.as_str(),
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
        let err = result.expect_err("#1310 seed_read_state fractional mtime");
        negative_assert_eq!(
            err.code.as_str(),
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
        let err = result.expect_err("#1310 seed_read_state unavailable");
        negative_assert_eq!(
            err.code.as_str(),
            "CAPABILITY_UNAVAILABLE",
            "#1310 seed_read_state unavailable"
        );
        r.owner.shutdown().await.expect("#1310 shutdown");
        r.project.dispose().await.expect("#1310 dispose");
    }

    // #1455 G3/G5: mtime 0.0 is the `n >= 0.0` boundary — valid on the TS
    // side (probe4: mtime-0 → seeded) and must reach the unavailable terminal
    // here, not INVALID_FILE_MTIME.
    #[tokio::test]
    async fn seed_read_state_unavailable_with_zero_mtime() {
        let (_s, r) = std::panic::catch_unwind(setup).expect("#1310 fixture");
        let params = object([
            ("sessionKey", "s1".into()),
            ("path", "/tmp/a.txt".into()),
            ("mtime", pilotdeck_protocol::JsValue::Number(0.0)),
        ]);
        let result = r
            .session_config_request("seed_read_state".into(), params)
            .await;
        let err = result.expect_err("#1455 seed_read_state zero mtime boundary");
        negative_assert_eq!(
            err.code.as_str(),
            "CAPABILITY_UNAVAILABLE",
            "#1455 seed_read_state zero mtime passes the boundary and reaches unavailable"
        );
        negative_assert_eq!(
            err.message
                .equals_str("Seed read state is not installed on this isolated native process."),
            true,
            "#1455 seed_read_state terminal message matches the unavailable shape"
        );
        r.owner.shutdown().await.expect("#1310 shutdown");
        r.project.dispose().await.expect("#1310 dispose");
    }

    // #1455 G5/G6: the busy gate must fire under a reserved turn
    // (TS probe3/probe4: SESSION_BUSY "Cannot seed file read state while a
    // turn is active.").
    #[tokio::test]
    async fn seed_read_state_busy_during_active_turn() {
        let (_s, r) = std::panic::catch_unwind(setup).expect("#1310 fixture");
        let key: pilotdeck_protocol::JsString = "s1".into();
        let run: pilotdeck_protocol::JsString = "run-1".into();
        let reservation = r
            .owner
            .begin_turn(&key, &run)
            .expect("#1455 begin turn reservation");
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
        let err = result.expect_err("#1455 seed_read_state busy during active turn");
        negative_assert_eq!(
            err.code.as_str(),
            "SESSION_BUSY",
            "#1455 seed_read_state active turn answers SESSION_BUSY"
        );
        negative_assert_eq!(
            err.message
                .equals_str("Cannot seed file read state while a turn is active."),
            true,
            "#1455 seed_read_state busy message matches the TS shape"
        );
        r.owner.end_turn(&key, Some(&run), Some(&reservation));
        r.owner.shutdown().await.expect("#1310 shutdown");
        r.project.dispose().await.expect("#1310 dispose");
    }

    // #1455 G5: the params gates precede the busy gate (probe4: active turn
    // with mtime -1 still answers INVALID_FILE_MTIME).
    #[tokio::test]
    async fn seed_read_state_rejects_negative_mtime_during_active_turn() {
        let (_s, r) = std::panic::catch_unwind(setup).expect("#1310 fixture");
        let key: pilotdeck_protocol::JsString = "s1".into();
        let run: pilotdeck_protocol::JsString = "run-1".into();
        let reservation = r
            .owner
            .begin_turn(&key, &run)
            .expect("#1455 begin turn reservation");
        let params = object([
            ("sessionKey", "s1".into()),
            ("path", "/tmp/a.txt".into()),
            ("mtime", pilotdeck_protocol::JsValue::Number(-1.0)),
        ]);
        let result = r
            .session_config_request("seed_read_state".into(), params)
            .await;
        let err = result.expect_err("#1455 seed_read_state negative mtime under turn");
        negative_assert_eq!(
            err.code.as_str(),
            "INVALID_FILE_MTIME",
            "#1455 seed_read_state mtime gate precedes the busy gate"
        );
        r.owner.end_turn(&key, Some(&run), Some(&reservation));
        r.owner.shutdown().await.expect("#1310 shutdown");
        r.project.dispose().await.expect("#1310 dispose");
    }

    // ── retained params guards (unit-tested directly) ────────────────────────

    #[test]
    fn require_session_key_rejects_missing_key() {
        let params = pilotdeck_protocol::JsValue::Object(Default::default());
        let err = require_session_key(&params).expect_err("#1310 require_session_key missing key");
        negative_assert_eq!(
            err.code.as_str(),
            "INVALID_SESSION_KEY",
            "#1310 require_session_key missing sessionKey"
        );
    }

    #[test]
    fn require_session_key_rejects_blank_key() {
        let params = object([("sessionKey", "  ".into())]);
        let err = require_session_key(&params).expect_err("#1310 require_session_key blank key");
        negative_assert_eq!(
            err.code.as_str(),
            "INVALID_SESSION_KEY",
            "#1310 require_session_key blank sessionKey"
        );
    }

    #[test]
    fn require_session_key_rejects_non_string_key() {
        let params = object([("sessionKey", pilotdeck_protocol::JsValue::Number(7.0))]);
        let err =
            require_session_key(&params).expect_err("#1310 require_session_key non-string key");
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
        let err = require_output_style_name(&params)
            .expect_err("#1310 require_output_style_name blank name");
        negative_assert_eq!(
            err.code.as_str(),
            "INVALID_OUTPUT_STYLE",
            "#1310 require_output_style_name blank name"
        );
    }

    // U+FEFF is ECMAScript WhiteSpace but not Rust `str::trim` whitespace, so
    // this case proves the shared `js_trim` helper is actually in the path
    // (#1455 r1).
    #[test]
    fn require_output_style_name_rejects_js_whitespace_name() {
        let params = object([("name", "\u{feff}".into())]);
        let err = require_output_style_name(&params)
            .expect_err("#1455 require_output_style_name rejects the BOM-only name");
        negative_assert_eq!(
            err.code.as_str(),
            "INVALID_OUTPUT_STYLE",
            "#1455 require_output_style_name rejects the BOM-only name"
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
