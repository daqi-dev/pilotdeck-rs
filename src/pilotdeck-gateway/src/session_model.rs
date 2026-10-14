//! Durable session selection, with the same project/config and replay boundaries
//! as the default native TS provider. No active session or Router state is read.
mod path_error;
mod replay;
#[cfg(test)]
pub(crate) mod tests;
use crate::{
    config::{self, Env, Error},
    metadata::{Files, Metadata, object, resolve, text_field},
    project_files::{needs_ts_collation, node_collator, node_locale},
};
use pilotdeck_protocol::{JsString, JsValue, parse_json, stringify_json};
use std::cmp::Ordering;
static UNDEFINED: JsValue = JsValue::Undefined;
fn field<'a>(v: &'a JsValue, k: &str) -> &'a JsValue {
    v.get(k).unwrap_or(&UNDEFINED)
}
fn error(code: &str, message: impl Into<JsString>) -> Error {
    Error {
        code: code.into(),
        message: message.into(),
        diagnostics: vec![],
        name: "Error",
        requires_host: false,
    }
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
fn property<'a>(v: &'a JsValue, k: &str) -> Result<&'a JsValue, Error> {
    if matches!(v, JsValue::Undefined | JsValue::Null) {
        return Err(error(
            "gateway_request_failed",
            format!(
                "Cannot read properties of {} (reading '{k}')",
                if v.is_undefined() {
                    "undefined"
                } else {
                    "null"
                }
            ),
        ));
    }
    Ok(field(v, k))
}
fn is(v: &JsValue, s: &str) -> bool {
    v.as_string().is_some_and(|v| v.equals_str(s))
}
fn requires_host() -> Error {
    let mut e = error("TS_COLLATION_REQUIRED", "");
    e.requires_host = true;
    e
}
pub(crate) fn response<F: Files>(
    request: &JsValue,
    metadata: &Metadata<F>,
    env: &Env,
) -> Option<Vec<u8>> {
    let result = execute(metadata, field(request, "params"), env);
    let id = field(request, "id").clone();
    let frame = match result {
        Ok(value) => object([
            ("type", "response".into()),
            ("id", id),
            ("ok", JsValue::Bool(true)),
            ("result", value),
        ]),
        Err(e) if e.requires_host => return None,
        Err(e) => object([
            ("type", "response".into()),
            ("id", id),
            ("ok", JsValue::Bool(false)),
            (
                "error",
                object([
                    ("code", e.code.as_str().into()),
                    ("message", JsValue::String(e.message)),
                ]),
            ),
        ]),
    };
    stringify_json(&frame)
        .ok()
        .flatten()
        .map(String::into_bytes)
}
fn execute<F: Files>(
    metadata: &Metadata<F>,
    params: &JsValue,
    env: &Env,
) -> Result<JsValue, Error> {
    let project = property(params, "projectKey")?;
    let Some(project_string) = project.as_string() else {
        return Err(path_argument(project));
    };
    let requested = resolve(
        &metadata.cwd,
        &String::from_utf16_lossy(project_string.units()),
    );
    let project = if requested == metadata.home {
        metadata.home.to_string_lossy().into_owned()
    } else {
        let projects = metadata.list_projects();
        let found = match field(&projects, "projects") {
            JsValue::Array(items) => items
                .iter()
                .filter_map(|p| text_field(p, "projectKey"))
                .find(|p| resolve(&metadata.cwd, p) == requested),
            _ => None,
        };
        found.ok_or_else(|| {
            error(
                "PROJECT_NOT_FOUND",
                format!(
                    "Unknown projectKey: {}",
                    String::from_utf16_lossy(project_string.units())
                ),
            )
        })?
    };
    let session = field(params, "sessionKey");
    if matches!(session, JsValue::Undefined | JsValue::Null)
        || session.as_string().is_some_and(|s| {
            crate::metadata::js_trim(&String::from_utf16_lossy(s.units())).is_empty()
        })
    {
        return Err(error("INVALID_SESSION_KEY", "sessionKey is required."));
    }
    let Some(session_string) = session.as_string() else {
        return Err(error(
            "gateway_request_failed",
            "sessionKey?.trim is not a function",
        ));
    };
    let path_session = sanitize_session(session_string);
    let chat_dir = metadata.chat_dir(&project);
    if session_string.units().contains(&0) || chat_dir.to_string_lossy().contains('\0') {
        // path.join accepts NUL; the first fs.stat validates the entire path.
        // Preserve UTF-16 in its error, including lone surrogates in sessionKey.
        let mut path: Vec<_> = format!("{}/", chat_dir.display()).encode_utf16().collect();
        path.extend(sanitize_session_units(session_string));
        path.extend(".jsonl".encode_utf16());
        return Err(path_error::nul(&JsString::from_utf16(path)));
    }
    let path = chat_dir.join(format!("{path_session}.jsonl"));
    let mut entries = vec![];
    match metadata.files.stat(&path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(io_error(&e, &path)),
        Ok(stat) if stat.size > 50 * 1024 * 1024 => {}
        Ok(_) => {
            let bytes = match metadata.files.read(&path) {
                Ok(b) => b,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => vec![],
                Err(e) => return Err(io_error(&e, &path)),
            };
            for line in String::from_utf8_lossy(&bytes).split('\n') {
                if let Ok(entry) = parse_json(line.strip_suffix('\r').unwrap_or(line))
                    && entry.as_object().is_some()
                    && ["type", "sessionId", "turnId", "createdAt"]
                        .iter()
                        .all(|k| field(&entry, k).as_string().is_some())
                    && field(&entry, "sequence").as_number().is_some()
                {
                    entries.push(entry);
                }
            }
        }
    }
    // D6 C: only equal-sequence comparisons call localeCompare. Single entries
    // and non-ASCII timestamps at distinct sequences do not need TS collation.
    let mut counts = std::collections::HashMap::new();
    for e in &entries {
        let n = field(e, "sequence").as_number().unwrap();
        *counts
            .entry(if n == 0.0 { 0 } else { n.to_bits() })
            .or_insert(0usize) += 1;
    }
    let tied = entries
        .iter()
        .filter(|e| {
            let n = field(e, "sequence").as_number().unwrap();
            counts[&if n == 0.0 { 0 } else { n.to_bits() }] > 1
        })
        .filter_map(|e| field(e, "createdAt").as_string());
    if needs_ts_collation(tied) {
        return Err(requires_host());
    }
    if entries.len() > 1 {
        let collator = if counts.values().any(|count| *count > 1) {
            let locale = node_locale(|key| env.get(key).cloned());
            Some(node_collator(&locale).map_err(|(code, msg)| error(code, msg))?)
        } else {
            None
        };
        entries.sort_by(|a, b| {
            let a_seq = field(a, "sequence").as_number().unwrap();
            let b_seq = field(b, "sequence").as_number().unwrap();
            if a_seq != b_seq {
                (a_seq - b_seq).partial_cmp(&0.0).unwrap_or(Ordering::Equal)
            } else {
                collator
                    .as_ref()
                    .expect("equal sequence group")
                    .compare_utf16(
                        field(a, "createdAt").as_string().unwrap().units(),
                        field(b, "createdAt").as_string().unwrap().units(),
                    )
            }
        });
    }
    let saved = replay::selection(&entries)?;
    let saved = if truthy(&saved) {
        restore(metadata, saved, env)?
    } else {
        JsValue::Undefined
    };
    let explicit = is(field(&saved, "mode"), "model");
    let effective = if explicit {
        object([
            ("provider", field(&saved, "provider").clone()),
            ("model", field(&saved, "model").clone()),
            ("source", "session".into()),
            ("reasoning", field(&saved, "reasoning").clone()),
            ("speed", field(&saved, "speed").clone()),
        ])
    } else {
        let c = config::load_project(metadata, env, Some(&project))?;
        object([
            ("provider", JsValue::String(c.selection.0)),
            ("model", JsValue::String(c.selection.1)),
            (
                "source",
                if c.router_enabled {
                    "router"
                } else {
                    "default"
                }
                .into(),
            ),
        ])
    };
    Ok(object([
        ("projectKey", project.as_str().into()),
        ("sessionKey", session.clone()),
        ("saved", saved),
        ("effective", effective),
    ]))
}
fn restore<F: Files>(metadata: &Metadata<F>, saved: JsValue, env: &Env) -> Result<JsValue, Error> {
    if is(field(&saved, "mode"), "auto") {
        return Ok(object([("mode", "auto".into())]));
    }
    if !is(field(&saved, "mode"), "model") {
        return Ok(saved);
    }
    let mut restored = object([
        ("mode", "model".into()),
        ("provider", field(&saved, "provider").clone()),
        ("model", field(&saved, "model").clone()),
        ("reasoning", field(&saved, "reasoning").clone()),
        ("speed", field(&saved, "speed").clone()),
    ]);
    if !field(&saved, "reasoning").is_undefined() {
        // Restore uses the global catalog, including its full validation/sorting.
        let catalog = crate::model_catalog::execute(metadata, &object([]), env)?;
        let item = match field(&catalog, "items") {
            JsValue::Array(items) => items.iter().find(|m| {
                field(m, "provider") == field(&saved, "provider")
                    && field(m, "model") == field(&saved, "model")
            }),
            _ => None,
        };
        let supports = item.is_some_and(|m| {
            match field(field(field(m, "capabilities"), "reasoning"), "values") {
                JsValue::Array(v) => v.contains(field(&saved, "reasoning")),
                _ => false,
            }
        });
        if !supports && let JsValue::Object(o) = &mut restored {
            o.insert("reasoning".into(), JsValue::Undefined);
        }
    }
    Ok(restored)
}
fn sanitize_session(s: &JsString) -> String {
    String::from_utf16_lossy(&sanitize_session_units(s))
}
fn sanitize_session_units(s: &JsString) -> Vec<u16> {
    let mut out = vec![];
    let mut separator = false;
    for &c in s.units() {
        if c == 47 || c == 92 {
            if !separator {
                out.push(45);
            }
            separator = true;
        } else {
            out.push(c);
            separator = false;
        }
    }
    let first = out.iter().position(|c| *c != 45).unwrap_or(out.len());
    let last = out.iter().rposition(|c| *c != 45).map_or(first, |i| i + 1);
    let trimmed = &out[first..last];
    if trimmed.is_empty() {
        "session".encode_utf16().collect()
    } else {
        trimmed.to_vec()
    }
}
fn path_argument(v: &JsValue) -> Error {
    let Some(received) = crate::metadata::argument_received(v) else {
        let mut e = error("JS_HOST_REQUIRED", "");
        e.requires_host = true;
        return e;
    };
    error(
        "ERR_INVALID_ARG_TYPE",
        format!("The \"paths[0]\" argument must be of type string. Received {received}"),
    )
}
fn io_error(e: &std::io::Error, path: &std::path::Path) -> Error {
    let code = config::binding::io_code(e);
    let message = match code {
        "EISDIR" => "EISDIR: illegal operation on a directory, read".into(),
        "ENOENT" => format!(
            "ENOENT: no such file or directory, open '{}'",
            path.display()
        ),
        "EACCES" => format!("EACCES: permission denied, open '{}'", path.display()),
        "ENOTDIR" => format!("ENOTDIR: not a directory, stat '{}'", path.display()),
        _ => e.to_string(),
    };
    error(code, message)
}
