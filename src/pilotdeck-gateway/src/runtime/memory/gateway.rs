//! `memory_list` / `memory_wipe` gateway adapter: the real
//! `InProcessGateway` wrapper validation (`InProcessGateway.ts:2794-2808`)
//! in front of the existing [`MemoryProjection`]
//! (`createLocalGateway.ts:1238-1259`).
//!
//! The adapter is deliberately not registered in the native dispatch table:
//! wiring `memory_*` into `requests.rs` belongs to the A8 entry lane, so the
//! methods stay forwarded to the TS host until then. `dispatch` is the only
//! wire-facing entry and mirrors [`crate::runtime::snapshot::SnapshotGateway`]:
//! unknown methods return `None`, a `requires_host` boundary returns
//! [`MemoryDispatch::Forward`] (the whole request goes to the Node host), and
//! every other outcome is the wire-shaped response or error JSON.
//!
//! Validation order is the TS wrapper's, measured by
//! `tests/fixtures/memory-gateway`: projectKey first, then the optional
//! sessionKey, then (wipe only) the `scope === "session"` sessionKey
//! requirement, then the capability check. The wrapper's TypeError for a
//! null/undefined params object reads like any other own-property access on a
//! JS primitive. Errors reuse [`MemoryProjectionError`] so the wire keeps the
//! `{name, code, message}` shape; the projection layer (including the inner
//! `CAPABILITY_UNAVAILABLE` when `memoryManagement` is absent) is unchanged.
use super::{
    MemoryListInput, MemoryProjection, MemoryProjectionError, MemoryWipeInput,
    ProjectMemoryRegistry, js_trim_units,
};
use pilotdeck_protocol::{JsString, JsValue};
use std::{path::PathBuf, sync::Arc};

static UNDEFINED: JsValue = JsValue::Undefined;

/// One native gateway method outcome. `Forward` is the A8 contract: the
/// projection hit a measured host boundary (`requires_host`), so the WS entry
/// must send the whole request to the Node host instead of surfacing a
/// synthesized error.
#[derive(Clone, Debug, PartialEq)]
pub enum MemoryDispatch {
    Response(JsValue),
    Forward,
}

/// `InProcessGateway.memoryList` / `memoryWipe` adapter over the projection.
/// `None` is the wrapper's absent `options.memoryList` / `options.memoryWipe`
/// seam: after the parameter validation (and the wipe scope check) the method
/// raises `CAPABILITY_UNAVAILABLE` instead of reaching the projection.
pub struct MemoryGateway {
    projection: Option<MemoryProjection>,
}

impl MemoryGateway {
    pub fn new(
        registry: Arc<dyn ProjectMemoryRegistry>,
        fallback_project_root: impl Into<PathBuf>,
    ) -> Self {
        Self {
            projection: Some(MemoryProjection::new(registry, fallback_project_root)),
        }
    }
    /// The wrapper with no `memoryList` / `memoryWipe` capability wired.
    pub fn disabled() -> Self {
        Self { projection: None }
    }
    pub fn from_projection(projection: MemoryProjection) -> Self {
        Self {
            projection: Some(projection),
        }
    }

    /// `memoryList`: projectKey, optional sessionKey, then the capability
    /// check and the projection.
    pub fn list(&self, params: &JsValue) -> Result<JsValue, MemoryProjectionError> {
        let project_key = require_management_project_key(parameter(params, "projectKey")?)?;
        let session_key = require_optional_session_key(parameter(params, "sessionKey")?)?;
        self.projection()?
            .list(&MemoryListInput {
                project_key: Some(project_key),
                session_key,
            })
            .map(|result| result.to_js())
    }

    /// `memoryWipe`: projectKey, optional sessionKey, then the
    /// `scope === "session"` sessionKey requirement, then the capability check
    /// and the projection.
    pub fn wipe(&self, params: &JsValue) -> Result<JsValue, MemoryProjectionError> {
        let project_key = require_management_project_key(parameter(params, "projectKey")?)?;
        let session_key = require_optional_session_key(parameter(params, "sessionKey")?)?;
        if scope_is_session(params)
            && session_key
                .as_ref()
                .map(|key| js_trim_units(key.units()).is_empty())
                .unwrap_or(true)
        {
            return Err(MemoryProjectionError::gateway(
                "INVALID_MEMORY_SCOPE",
                "sessionKey is required for a session-scoped wipe.",
            ));
        }
        self.projection()?
            .wipe(&MemoryWipeInput {
                project_key: Some(project_key),
                session_key,
                // `input.scope === "session"`: only a primitive string matches,
                // so any other wire value (unknown, absent, null, numeric)
                // takes the project branch via `None`.
                scope: params.get("scope").and_then(JsValue::as_string).cloned(),
            })
            .map(|result| result.to_js())
    }

    /// WS method names (`memory_list` / `memory_wipe`). Unknown methods return
    /// `None` so the caller keeps forwarding by default.
    pub fn dispatch(&self, method: &str, params: &JsValue) -> Option<MemoryDispatch> {
        let outcome = match method {
            "memory_list" => self.list(params),
            "memory_wipe" => self.wipe(params),
            _ => return None,
        };
        Some(match outcome {
            Ok(value) => MemoryDispatch::Response(value),
            Err(error) if error.requires_host() => MemoryDispatch::Forward,
            Err(error) => MemoryDispatch::Response(error.to_js()),
        })
    }

    /// `if (!this.options.memoryList) throw CAPABILITY_UNAVAILABLE` — after
    /// the parameter validation, exactly like the TS wrapper.
    fn projection(&self) -> Result<&MemoryProjection, MemoryProjectionError> {
        self.projection.as_ref().ok_or_else(|| {
            MemoryProjectionError::gateway(
                "CAPABILITY_UNAVAILABLE",
                "Gateway memory provider is unavailable.",
            )
        })
    }
}

/// Reads one own property with the TypeError Node raises for a null/undefined
/// params object. Primitive params box like JS and simply have no property.
fn parameter<'a>(params: &'a JsValue, key: &str) -> Result<&'a JsValue, MemoryProjectionError> {
    if let JsValue::Undefined | JsValue::Null = params {
        let kind = if params.is_undefined() {
            "undefined"
        } else {
            "null"
        };
        let mut units = "Cannot read properties of "
            .encode_utf16()
            .collect::<Vec<_>>();
        units.extend(format!("{kind} (reading '{key}')").encode_utf16());
        return Err(MemoryProjectionError {
            code: None,
            name: "TypeError".into(),
            message: JsString::from_utf16(units),
            requires_host: false,
        });
    }
    Ok(params.get(key).unwrap_or(&UNDEFINED))
}

/// `requireManagementProjectKey` (InProcessGateway.ts:3144-3148). Returns the
/// validated key so the projection receives the same JS string identity.
fn require_management_project_key(value: &JsValue) -> Result<JsString, MemoryProjectionError> {
    match value {
        JsValue::String(text) if !js_trim_units(text.units()).is_empty() => Ok(text.clone()),
        _ => Err(MemoryProjectionError::gateway(
            "PROJECT_NOT_FOUND",
            "projectKey is required.",
        )),
    }
}

/// `requireOptionalManagementSessionKey` (InProcessGateway.ts:3150-3154):
/// `undefined` is legal, every other non-string or blank value is rejected.
fn require_optional_session_key(
    value: &JsValue,
) -> Result<Option<JsString>, MemoryProjectionError> {
    match value {
        JsValue::Undefined => Ok(None),
        JsValue::String(text) if !js_trim_units(text.units()).is_empty() => Ok(Some(text.clone())),
        _ => Err(MemoryProjectionError::gateway(
            "INVALID_SESSION_KEY",
            "sessionKey must be non-empty when provided.",
        )),
    }
}

/// `input.scope === "session"` with JS strict-equality semantics: only a
/// primitive string equal by code unit matches.
fn scope_is_session(params: &JsValue) -> bool {
    params
        .get("scope")
        .and_then(JsValue::as_string)
        .is_some_and(|scope| scope.equals_str("session"))
}

#[cfg(test)]
mod tests {
    use super::super::js_is_space_unit;
    use super::*;

    fn js(json: &str) -> JsValue {
        pilotdeck_protocol::parse_json(json).expect("#1214 test JSON must parse")
    }

    /// The complete `String.prototype.trim` whitespace set of the fixed TS
    /// baseline, measured by scanning every UTF-16 code unit on Node v22.22.0
    /// (`String.fromCharCode(unit).trim() === ''`; #1226 delivery evidence
    /// `whitespace-probe.log`). The #1214 replay corpus only pinned 6 of
    /// these 25 units, so a dropped or widened boundary survived it.
    const TS_TRIM_SPACE_UNITS: [u16; 25] = [
        0x0009, 0x000A, 0x000B, 0x000C, 0x000D, 0x0020, 0x00A0, 0x1680, 0x2000, 0x2001, 0x2002,
        0x2003, 0x2004, 0x2005, 0x2006, 0x2007, 0x2008, 0x2009, 0x200A, 0x2028, 0x2029, 0x202F,
        0x205F, 0x3000, 0xFEFF,
    ];

    /// #1226: exhaustive pin of [`js_is_space_unit`] over the whole UTF-16
    /// code-unit range. Any unit removed from or added to the match arms
    /// drifts against the measured TS trim set.
    #[test]
    fn js_is_space_unit_pins_the_ts_trim_whitespace_set() {
        let drifted: Vec<String> = (0u16..=u16::MAX)
            .filter(|&unit| js_is_space_unit(unit) != TS_TRIM_SPACE_UNITS.contains(&unit))
            .map(|unit| format!("U+{unit:04X}"))
            .collect();
        assert!(
            drifted.is_empty(),
            "#1226 js_is_space_unit drifted from the TS trim whitespace set: {drifted:?}"
        );
    }

    /// #1226: `requireManagementProjectKey` (InProcessGateway.ts:3144-3148)
    /// only trims to check emptiness; the validated key keeps its raw units.
    #[test]
    fn require_management_project_key_only_trims_for_emptiness() {
        for &unit in &TS_TRIM_SPACE_UNITS {
            let mut padded = vec![unit];
            padded.extend("/pdrs-fixture/project".encode_utf16());
            padded.push(unit);
            let key = require_management_project_key(&JsValue::String(JsString::from_utf16(
                padded.clone(),
            )))
            .unwrap_or_else(|_| {
                panic!("#1226 U+{unit:04X}-padded projectKey must pass validation")
            });
            assert_eq!(
                key.units(),
                padded.as_slice(),
                "#1226 U+{unit:04X}-padded projectKey must keep its raw units"
            );
            let blank =
                require_management_project_key(&JsValue::String(JsString::from_utf16(vec![unit])))
                    .expect_err("#1226 whitespace-only projectKey must be rejected");
            assert_eq!(
                blank.code(),
                Some("PROJECT_NOT_FOUND"),
                "#1226 whitespace-only projectKey rejection code"
            );
        }
    }

    #[test]
    fn gateway_validators_match_inprocess_order() {
        let gateway = MemoryGateway::disabled();
        // projectKey before sessionKey before the scope check before the
        // capability check (InProcessGateway.ts:2800-2808).
        let error = gateway
            .wipe(&js(
                r#"{"projectKey":"  ","scope":"session","sessionKey":42}"#,
            ))
            .expect_err("#1214 blank projectKey must fail first");
        assert_eq!(error.code(), Some("PROJECT_NOT_FOUND"), "#1214 key first");
        let error = gateway
            .wipe(&js(
                r#"{"projectKey":"/p","scope":"session","sessionKey":42}"#,
            ))
            .expect_err("#1214 non-string sessionKey must fail before the scope check");
        assert_eq!(
            error.code(),
            Some("INVALID_SESSION_KEY"),
            "#1214 session code"
        );
        let error = gateway
            .wipe(&js(r#"{"projectKey":"/p","scope":"session"}"#))
            .expect_err("#1214 scope check must run before the capability check");
        assert_eq!(
            error.code(),
            Some("INVALID_MEMORY_SCOPE"),
            "#1214 scope before capability"
        );
        let error = gateway
            .wipe(&js(r#"{"projectKey":"/p","scope":"project"}"#))
            .expect_err("#1214 absent capability must fail");
        assert_eq!(
            error.code(),
            Some("CAPABILITY_UNAVAILABLE"),
            "#1214 capability after validation"
        );
        let error = gateway
            .list(&js(r#"{"projectKey":"/p"}"#))
            .expect_err("#1214 absent capability must fail on list");
        assert_eq!(
            error.code(),
            Some("CAPABILITY_UNAVAILABLE"),
            "#1214 list capability"
        );
        let error = gateway
            .list(&js("{}"))
            .expect_err("#1214 empty params must fail before the capability check");
        assert_eq!(error.code(), Some("PROJECT_NOT_FOUND"), "#1214 validation");
    }

    #[test]
    fn params_object_typeerror_keeps_node_shape() {
        let gateway = MemoryGateway::disabled();
        let error = gateway
            .list(&JsValue::Undefined)
            .expect_err("#1214 undefined params must TypeError");
        assert_eq!(error.name(), "TypeError", "#1214 TypeError name");
        assert_eq!(error.code(), None, "#1214 TypeError has no code");
        assert_eq!(
            String::from_utf16_lossy(error.message().units()),
            "Cannot read properties of undefined (reading 'projectKey')",
            "#1214 TypeError message"
        );
        let error = gateway
            .list(&JsValue::Null)
            .expect_err("#1214 null params must TypeError");
        assert_eq!(
            String::from_utf16_lossy(error.message().units()),
            "Cannot read properties of null (reading 'projectKey')",
            "#1214 null TypeError message"
        );
    }

    #[test]
    fn dispatch_routes_only_memory_methods() {
        let gateway = MemoryGateway::disabled();
        assert_eq!(
            gateway.dispatch("memory_list", &js(r#"{"projectKey":"/p"}"#)),
            Some(MemoryDispatch::Response(js(
                r#"{"name":"DialogGatewayError","code":"CAPABILITY_UNAVAILABLE","message":"Gateway memory provider is unavailable."}"#
            ))),
            "#1214 memory_list dispatch"
        );
        assert_eq!(
            gateway.dispatch("memory_wipe", &js(r#"{"projectKey":"/p"}"#)),
            Some(MemoryDispatch::Response(js(
                r#"{"name":"DialogGatewayError","code":"CAPABILITY_UNAVAILABLE","message":"Gateway memory provider is unavailable."}"#
            ))),
            "#1214 memory_wipe dispatch"
        );
        assert_eq!(
            gateway.dispatch("snapshot_list", &js("{}")),
            None,
            "#1214 unknown method forwards"
        );
    }
}
