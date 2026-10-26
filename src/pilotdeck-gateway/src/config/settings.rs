//! `resolve_settings` snapshot response projection, mirroring the fixed
//! TypeScript baseline's `toGatewayResolvedSettings`
//! (`src/cli/createLocalGateway.ts`). This is an unenabled native component:
//! default gateway dispatch still routes `resolve_settings` through the
//! TypeScript host, and nothing here is registered anywhere yet.
//!
//! The projection is pure: it reads one already-loaded snapshot and rebuilds
//! the wire response. `config` is redacted by the snapshot's own
//! [`super::snapshot::redact`] (the baseline's `redactConfig`), sources and
//! diagnostics are re-emitted with the baseline's key order, and every
//! optional field follows the baseline's conditional spreads.
use super::store::ConfigStore;
use pilotdeck_protocol::{JsObject, JsValue};

impl ConfigStore {
    /// `resolve_settings` (the baseline binds it at
    /// `src/cli/createLocalGateway.ts:1402`): the current snapshot projected
    /// into the redacted `GatewayResolvedSettingsResult` wire response.
    pub fn resolved_settings(&self) -> JsValue {
        resolved_settings(&self.snapshot())
    }
}

/// `toGatewayResolvedSettings(snapshot)`: project one `loadPilotConfig`
/// snapshot into the response. Top-level key order is `schemaVersion, version,
/// loadedAt, contentHash, config, sources, diagnostics`; a source re-emits
/// `kind, priority, loadedAt, [path], [contentHash], [phase]` and a diagnostic
/// `code, severity, message, [path], [source], [hint], [redactedValue],
/// [recoverable]`, where each bracketed field is the baseline's truthiness
/// spread except `recoverable`, which only requires `!== undefined`.
pub fn resolved_settings(snapshot: &JsValue) -> JsValue {
    let mut out = JsObject::new();
    for key in ["schemaVersion", "version", "loadedAt", "contentHash"] {
        out.insert(key.into(), field(snapshot, key));
    }
    out.insert(
        "config".into(),
        super::snapshot::redact(&field(snapshot, "config")),
    );
    out.insert("sources".into(), projected(snapshot, "sources", source));
    out.insert(
        "diagnostics".into(),
        projected(snapshot, "diagnostics", diagnostic),
    );
    JsValue::Object(out)
}

/// `snapshot.sources.map(...)` / `snapshot.diagnostics.map(...)`: map each
/// element of a snapshot array; the snapshot loader always produces arrays, so
/// a missing one projects to an empty array like an empty `map`.
fn projected(snapshot: &JsValue, key: &str, project: fn(&JsValue) -> JsValue) -> JsValue {
    match field(snapshot, key) {
        JsValue::Array(ref items) => JsValue::Array(items.iter().map(project).collect()),
        _ => JsValue::Array(vec![]),
    }
}

fn source(value: &JsValue) -> JsValue {
    let mut out = JsObject::new();
    for key in ["kind", "priority", "loadedAt"] {
        out.insert(key.into(), field(value, key));
    }
    for key in ["path", "contentHash", "phase"] {
        put_truthy(&mut out, value, key);
    }
    JsValue::Object(out)
}

fn diagnostic(value: &JsValue) -> JsValue {
    let mut out = JsObject::new();
    for key in ["code", "severity", "message"] {
        out.insert(key.into(), field(value, key));
    }
    for key in ["path", "source", "hint", "redactedValue"] {
        put_truthy(&mut out, value, key);
    }
    // `...(diagnostic.recoverable !== undefined ? { recoverable } : {})`:
    // presence, not truthiness, decides; `false` survives.
    let recoverable = field(value, "recoverable");
    if !matches!(recoverable, JsValue::Undefined) {
        out.insert("recoverable".into(), recoverable);
    }
    JsValue::Object(out)
}

/// `...(value.<key> ? { <key>: value.<key> } : {})`: copy the field verbatim
/// only when it is JavaScript-truthy. This drops the snapshot diagnostics'
/// `hint: undefined` own keys, which `Object.entries`-based encoding would
/// otherwise carry into the response.
fn put_truthy(out: &mut JsObject, value: &JsValue, key: &str) {
    let field = field(value, key);
    if truthy(&field) {
        out.insert(key.into(), field);
    }
}

fn truthy(value: &JsValue) -> bool {
    match value {
        JsValue::Undefined | JsValue::Null => false,
        JsValue::Bool(v) => *v,
        JsValue::Number(n) => *n != 0.0 && !n.is_nan(),
        JsValue::String(s) => !s.units().is_empty(),
        _ => true,
    }
}

fn field(value: &JsValue, key: &str) -> JsValue {
    value
        .as_object()
        .and_then(|o| o.get_property(key))
        .cloned()
        .unwrap_or(JsValue::Undefined)
}
