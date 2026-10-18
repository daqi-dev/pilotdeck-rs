//! Catalog projection after the complete global configuration validation chain.
use crate::{
    config::{self, Env},
    metadata::{Files, Metadata, object},
    project_files::{has_unpaired_surrogate, node_collator, node_locale},
};
use pilotdeck_protocol::{JsString, JsValue, stringify_json};
static UNDEFINED: JsValue = JsValue::Undefined;
fn field<'a>(v: &'a JsValue, k: &str) -> &'a JsValue {
    v.get(k).unwrap_or(&UNDEFINED)
}
fn truthy(v: &JsValue) -> bool {
    match v {
        JsValue::Undefined | JsValue::Null => false,
        JsValue::Bool(b) => *b,
        JsValue::Number(n) => *n != 0.0 && !n.is_nan(),
        JsValue::String(s) => !s.is_empty(),
        _ => true,
    }
}
pub(crate) fn lower(
    s: &JsString,
    locale: &icu_locale_core::LanguageIdentifier,
    substring: bool,
) -> JsString {
    // Node's Latin-1 fast path is selected for the complete string, not each
    // Unicode run separated by an isolated surrogate.
    let sliced = substring && s.len_utf16() >= 13;
    let root = icu_locale_core::LanguageIdentifier::UNKNOWN;
    let locale = if !sliced && s.units().iter().all(|u| *u <= 255) {
        &root
    } else {
        locale
    };
    let mapper = icu_casemap::CaseMapper::new();
    let mut output = vec![];
    let mut run = String::new();
    for c in char::decode_utf16(s.units().iter().copied()) {
        match c {
            Ok(c) => run.push(c),
            Err(e) => {
                output.extend(mapper.lowercase_to_string(&run, locale).encode_utf16());
                run.clear();
                output.push(e.unpaired_surrogate());
            }
        }
    }
    output.extend(mapper.lowercase_to_string(&run, locale).encode_utf16());
    JsString::from_utf16(output)
}
fn contains(haystack: &JsString, needle: &JsString) -> bool {
    needle.is_empty()
        || haystack
            .units()
            .windows(needle.units().len())
            .any(|s| s == needle.units())
}
fn join(parts: &[&JsString], separator: u16) -> JsString {
    let mut units = vec![];
    for (i, s) in parts.iter().enumerate() {
        if i > 0 {
            units.push(separator);
        }
        units.extend(s.units());
    }
    JsString::from_utf16(units)
}
pub(crate) fn execute<F: Files>(
    metadata: &Metadata<F>,
    params: &JsValue,
    env: &Env,
) -> Result<JsValue, config::Error> {
    let config = config::load(metadata, env)?;
    // Warnings are retained by the loader for future configuration consumers.
    let _diagnostics = &config.diagnostics;
    if matches!(params, JsValue::Undefined | JsValue::Null) {
        let kind = if params.is_undefined() {
            "undefined"
        } else {
            "null"
        };
        return Err(config::Error {
            code: "gateway_request_failed".into(),
            message: format!("Cannot read properties of {kind} (reading 'query')").into(),
            diagnostics: vec![],
            name: "TypeError",
            requires_host: false,
        });
    }
    let locale = node_locale(|key| env.get(key).cloned());
    let q = field(params, "query");
    let query = match q {
        JsValue::Undefined | JsValue::Null => JsString::default(),
        JsValue::String(s) => {
            let trimmed = trim(s);
            lower(&trimmed, &locale.id, trimmed != *s)
        }
        _ => {
            return Err(config::Error {
                code: "gateway_request_failed".into(),
                message: "input.query?.trim is not a function".into(),
                diagnostics: vec![],
                name: "TypeError",
                requires_host: false,
            });
        }
    };
    let provider_filter = field(params, "provider");
    let mut items = vec![];
    for p in &config.providers {
        if truthy(provider_filter) && provider_filter.as_string() != Some(&p.id) {
            continue;
        }
        for m in &p.models {
            if !m.enumerable {
                continue;
            }
            let display = &m.display_name;
            let search = join(&[&p.id, &m.id, display], 32);
            if !query.is_empty() && !contains(&lower(&search, &locale.id, false), &query) {
                continue;
            }
            let mut capabilities = pilotdeck_protocol::JsObject::new();
            if let Some(values) = &m.reasoning {
                capabilities.insert(
                    "reasoning".into(),
                    object([
                        ("type", "enum".into()),
                        (
                            "values",
                            JsValue::Array(values.iter().copied().map(JsValue::Number).collect()),
                        ),
                    ]),
                );
            }
            if m.speed && p.speed_mapping {
                capabilities.insert(
                    "speed".into(),
                    object([
                        ("type", "enum".into()),
                        (
                            "values",
                            JsValue::Array(vec![JsValue::Number(0.0), JsValue::Number(1.0)]),
                        ),
                    ]),
                );
            }
            let id = join(&[&p.id, &m.id], 47);
            items.push((
                id.clone(),
                object([
                    ("id", JsValue::String(id)),
                    ("provider", JsValue::String(p.id.clone())),
                    ("model", JsValue::String(m.id.clone())),
                    ("displayName", JsValue::String(display.clone())),
                    ("available", JsValue::Bool(p.available)),
                    ("capabilities", JsValue::Object(capabilities)),
                ]),
            ));
        }
    }
    if items.len() > 1 && items.iter().any(|(id, _)| has_unpaired_surrogate(id)) {
        return Err(config::collation_host_error());
    }
    let collator = node_collator(&locale).map_err(|(code, msg)| config::Error {
        code: code.into(),
        message: msg.into(),
        diagnostics: vec![],
        name: "Error",
        requires_host: false,
    })?;
    items.sort_by(|a, b| collator.compare_utf16(a.0.units(), b.0.units()));
    if config.router_enabled
        && field(params, "includeAuto").as_bool() == Some(true)
        && (!truthy(provider_filter)
            || provider_filter
                .as_string()
                .is_some_and(|s| s.equals_str("router")))
        && contains(&"router auto".into(), &query)
    {
        items.insert(
            0,
            (
                "router/auto".into(),
                object([
                    ("id", "router/auto".into()),
                    ("provider", "router".into()),
                    ("model", "auto".into()),
                    ("displayName", "Auto".into()),
                    ("available", JsValue::Bool(true)),
                    ("capabilities", object([])),
                ]),
            ),
        );
    }
    Ok(object([
        (
            "items",
            JsValue::Array(items.into_iter().map(|(_, v)| v).collect()),
        ),
        (
            "defaultSelection",
            object([
                ("mode", "model".into()),
                ("provider", JsValue::String(config.selection.0)),
                ("model", JsValue::String(config.selection.1)),
            ]),
        ),
        (
            "router",
            object([
                ("enabled", JsValue::Bool(config.router_enabled)),
                ("autoAvailable", JsValue::Bool(config.router_enabled)),
            ]),
        ),
    ]))
}
fn trim(s: &JsString) -> JsString {
    let white = |c: &u16| matches!(*c,9..=13|32|160|0x1680|0x2000..=0x200a|0x2028|0x2029|0x202f|0x205f|0x3000|0xfeff);
    let u = s.units();
    let a = u.iter().position(|c| !white(c)).unwrap_or(u.len());
    let b = u.iter().rposition(|c| !white(c)).map_or(a, |i| i + 1);
    JsString::from_utf16(u[a..b].to_vec())
}
pub(crate) fn response<F: Files>(
    request: &JsValue,
    metadata: &Metadata<F>,
    env: &Env,
) -> Option<Vec<u8>> {
    let result = execute(metadata, field(request, "params"), env);
    let id = field(request, "id").clone();
    let frame = match result {
        Ok(result) => object([
            ("type", "response".into()),
            ("id", id),
            ("ok", JsValue::Bool(true)),
            ("result", result),
        ]),
        Err(e) if e.requires_host => return None,
        Err(e) => object([
            ("type", "response".into()),
            ("id", id),
            ("ok", JsValue::Bool(false)),
            (
                "error",
                object([
                    ("code", JsValue::String(e.code.into())),
                    ("message", JsValue::String(e.message)),
                ]),
            ),
        ]),
    };
    Some(stringify_json(&frame).unwrap().unwrap().into_bytes())
}

/// Unlike project registration, global config does not require explicit
/// PILOT_HOME. Resolve the ordinary ~/.pilotdeck fallback against the host HOME.
/// If the OS home cannot be represented from the captured environment, TS keeps
/// ownership instead of guessing a directory.
pub(crate) fn native_metadata(env: &Env) -> Option<Metadata<crate::metadata::NativeFiles>> {
    metadata_at(env, std::env::current_dir().ok()?)
}
fn metadata_at(
    env: &Env,
    cwd: std::path::PathBuf,
) -> Option<Metadata<crate::metadata::NativeFiles>> {
    let value = env
        .get("PILOT_HOME")
        .map(String::as_str)
        .unwrap_or("~/.pilotdeck");
    let home = if value == "~" || value.starts_with("~/") {
        let system_home = env.get("HOME")?;
        let suffix = value.strip_prefix("~/").unwrap_or("");
        crate::metadata::resolve(
            &cwd,
            &std::path::Path::new(system_home)
                .join(suffix)
                .to_string_lossy(),
        )
    } else {
        crate::metadata::resolve(&cwd, value)
    };
    Some(Metadata {
        home,
        cwd,
        files: crate::metadata::NativeFiles,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn home_resolution_preserves_empty_and_relative_inputs() {
        let cwd = std::path::PathBuf::from("/offline/cwd");
        for (value, expected) in [
            (None, "/offline/user/.pilotdeck"),
            (Some(""), "/offline/cwd"),
            (Some("../home"), "/offline/home"),
            (Some("~"), "/offline/user"),
            (Some("~/isolated"), "/offline/user/isolated"),
            (Some("~/a/../b"), "/offline/user/b"),
        ] {
            let mut env = Env::from([("HOME".into(), "/offline/user".into())]);
            if let Some(value) = value {
                env.insert("PILOT_HOME".into(), value.into());
            }
            assert_eq!(
                metadata_at(&env, cwd.clone()).unwrap().home,
                std::path::PathBuf::from(expected)
            );
        }
    }
}
