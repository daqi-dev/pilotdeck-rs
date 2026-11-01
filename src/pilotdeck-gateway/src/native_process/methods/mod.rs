//! One registration per request family; each family owns its wire names.
use super::requests::Requests;
use crate::runtime::connection::{Call, RequestError};
use pilotdeck_protocol::{JsString, JsValue};

type Response = Result<JsValue, RequestError>;
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

// A new family needs its own module and one line below. The same table drives
// lookup and enumeration; management keeps its original admission boundary.
macro_rules! register_families {
    ($($family:ident: $methods:expr),+ $(,)?) => {
        $(mod $family;)+
        const FAMILIES: &[Family] = &[
            $(Family { methods: $methods, handler: $family::HANDLER },)+
        ];
    };
}
register_families! {
    management: management::METHODS,
    active_turn: active_turn::METHODS,
    runs: runs::METHODS,
    metadata: metadata::METHODS,
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

fn string(value: &JsValue, name: &str) -> Result<JsString, RequestError> {
    value
        .get(name)
        .and_then(JsValue::as_string)
        .cloned()
        .ok_or_else(|| RequestError::new(format!("{name} must be a string.")))
}

#[cfg(test)]
mod tests;
