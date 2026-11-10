//! One registration per request family; each family owns its wire names.
use super::requests::Requests;
use crate::runtime::connection::{Call, RequestError};
use pilotdeck_protocol::{JsString, JsValue};

mod active_turn;
mod always_on;
mod config;
mod cron;
mod history;
mod interaction;
mod management;
mod mcp;
mod metadata;
mod permission;
mod runs;
mod session_dir;
mod settings;
// #1320 fix-r2 F10: crate-visible for the runtime-side single-source binding
// test; the value is `crate::runtime::settings::METHODS` verbatim.
#[cfg(test)]
pub(crate) use settings::METHODS;
// #2363 item 6: crate-visible for the switch-side race test, which runs the
// routing seam's own dispatch body against the S1 stop face.
#[cfg(test)]
pub(crate) use cron::dispatch_under_management_gate;
mod sop;
mod steer;
mod tools;
mod transcript_archive;
mod upload;
mod user_dialog;
mod weixin_login;

type Response = Result<JsValue, RequestError>;

// The `Requests.prepare_weixin_login` field's callback type.
pub(super) use weixin_login::PrepareWeixinLogin;
type RequestHandler = for<'a> fn(&'a Requests, String, JsValue) -> Call<'a, Response>;
type EarlyHandler = for<'a> fn(&'a Requests, &'a str, &'a JsValue) -> Call<'a, Option<Response>>;

enum Handler {
    BeforeAdmission(EarlyHandler),
    AfterAdmission(RequestHandler),
}

struct Family {
    methods: &'static [&'static str],
    handler: Handler,
}

// Keep module declarations explicit for the #1166 build-input scanner.
// A new family needs one declaration above and one registration below.
// The same table drives lookup and enumeration; management keeps its boundary.
macro_rules! register_families {
    ($($family:ident: $methods:expr),+ $(,)?) => {
        const FAMILIES: &[Family] = &[
            $(Family { methods: $methods, handler: $family::HANDLER },)+
        ];
    };
}
register_families! {
    management: management::METHODS,
    active_turn: active_turn::METHODS,
    runs: runs::METHODS,
    history: history::METHODS,
    mcp: mcp::METHODS,
    sop: sop::METHODS,
    metadata: metadata::METHODS,
    tools: tools::METHODS,
    session_dir: session_dir::METHODS,
    steer: steer::METHODS,
    permission: permission::METHODS,
    user_dialog: user_dialog::METHODS,
    upload: upload::METHODS,
    // A new family: one declaration above and one registration here.
    config: config::METHODS,
    interaction: interaction::METHODS,
    always_on: always_on::METHODS,
    weixin_login: weixin_login::METHODS,
    cron: cron::METHODS,
    settings: settings::METHODS,
}

/// Methods wired to `Requests::request`, independent of runtime capabilities.
/// Connection-owned methods and `describe_server` are outside this registry.
pub fn registered_methods() -> impl Iterator<Item = &'static str> {
    FAMILIES
        .iter()
        .flat_map(|family| family.methods.iter().copied())
}

fn lookup(method: &str) -> Option<&'static Family> {
    FAMILIES
        .iter()
        .find(|family| family.methods.contains(&method))
}

pub(super) async fn before_admission(
    requests: &Requests,
    method: &str,
    params: &JsValue,
) -> Option<Response> {
    match lookup(method).map(|family| &family.handler) {
        Some(Handler::BeforeAdmission(handler)) => handler(requests, method, params).await,
        _ => None,
    }
}

pub(super) fn request(requests: &Requests, method: String, params: JsValue) -> Call<'_, Response> {
    match lookup(&method).map(|family| &family.handler) {
        Some(Handler::AfterAdmission(handler)) => handler(requests, method, params),
        // Unknown methods retain the original A4 Forward error without
        // dispatching a family that was not selected by the registry.
        _ => Box::pin(async move { metadata::unavailable(method) }),
    }
}

/// `input.<key>` read at the top of a TS method body. The wire `params` reach
/// the method untouched, so only a nullish `params` throws; every other shape
/// yields the property or `undefined` (#1465 TS probes:
/// `_tmp/20261106-issue-1465/ts-probe/output*.txt`).
pub(super) fn input_property(params: &JsValue, key: &str) -> Result<Option<JsValue>, RequestError> {
    match params {
        JsValue::Null => Err(RequestError::new(format!(
            "Cannot read properties of null (reading '{key}')"
        ))),
        JsValue::Undefined => Err(RequestError::new(format!(
            "Cannot read properties of undefined (reading '{key}')"
        ))),
        _ => Ok(params.get(key).cloned()),
    }
}

/// Outcome of the `!input.<key>?.trim()` guard used across the TS methods.
pub(super) enum TrimGuard {
    /// A non-empty string: the guard passes.
    Value(JsString),
    /// null / undefined / empty / blank: the guard fails with a domain error.
    Missing,
    /// Any other non-nullish value: optional chaining does not short-circuit,
    /// `.trim` is not a function, and the boundary serializes the TypeError.
    NotAString,
}

pub(super) fn trim_guard(params: &JsValue, key: &str) -> Result<TrimGuard, RequestError> {
    match input_property(params, key)?.as_ref() {
        None | Some(JsValue::Undefined) | Some(JsValue::Null) => Ok(TrimGuard::Missing),
        Some(JsValue::String(value)) => {
            let text = String::from_utf16_lossy(value.units());
            if crate::metadata::js_trim(&text).is_empty() {
                Ok(TrimGuard::Missing)
            } else {
                Ok(TrimGuard::Value(value.clone()))
            }
        }
        Some(_) => Ok(TrimGuard::NotAString),
    }
}

/// `!input.<key>?.trim()` collapsed to the two failure shapes the TS method
/// bodies produce: a domain error for a missing/blank string, and the
/// boundary's `gateway_request_failed` TypeError for a non-string
/// (`input.<key>?.trim is not a function`).
pub(super) fn require_text(
    params: &JsValue,
    key: &str,
    code: &str,
    message: &str,
) -> Result<JsString, RequestError> {
    match trim_guard(params, key)? {
        TrimGuard::Value(value) => Ok(value),
        TrimGuard::Missing => Err(RequestError {
            code: code.into(),
            message: message.into(),
            details: None,
            validation: None,
        }),
        TrimGuard::NotAString => Err(RequestError::new(format!(
            "input.{key}?.trim is not a function"
        ))),
    }
}

#[cfg(test)]
mod tests;
