//! updateLocalSettings.ts: allowlisted edits, validation before atomic replacement.
use super::{ConfigRoutes, Result, RouteError, field};
use pilotdeck_protocol::{JsString, JsValue, stringify_json};
use pilotdeck_yaml::{
    Document, Options,
    nodes::{NodeKind, ScalarValue},
    parse_document,
};
use std::{fs, io::Write};

type Edit = (Vec<String>, JsValue);
fn invalid(path: &str, suffix: &str) -> RouteError {
    RouteError::new(
        "INVALID_LOCAL_SETTINGS",
        format!("localSettings.{path} {suffix}"),
    )
}
fn unsupported(path: &str) -> RouteError {
    RouteError::new(
        "UNSUPPORTED_LOCAL_SETTING",
        format!("localSettings.{path} is not supported by PilotDeck localSettings."),
    )
}
fn keys(value: &JsValue, path: &str, allowed: &[&str]) -> Result {
    let obj = value
        .as_object()
        .ok_or_else(|| invalid(path, "must be an object."))?;
    for (key, _) in obj.entries() {
        let name = String::from_utf16_lossy(key.units());
        if !allowed.contains(&name.as_str()) {
            return Err(unsupported(
                format!("{path}.{name}").trim_start_matches('.'),
            ));
        }
    }
    Ok(JsValue::Undefined)
}
fn integer(value: &JsValue, min: f64, safe: bool) -> bool {
    value.as_number().is_some_and(|n| {
        n.is_finite() && n.fract() == 0.0 && n >= min && (!safe || n <= 9_007_199_254_740_991.0)
    })
}
fn normalize(settings: &JsValue) -> Result<Vec<Edit>> {
    keys(settings, "", &["agent", "extension", "tools"])?;
    let mut edits = Vec::new();
    let mut add = |path: &str, v: &JsValue| {
        if !v.is_undefined() {
            edits.push((path.split('.').map(str::to_owned).collect(), v.clone()));
        }
    };
    let agent = field(settings, "agent");
    if !agent.is_undefined() {
        keys(
            agent,
            "agent",
            &[
                "maxContextTokens",
                "maxOutputTokens",
                "thinking",
                "subagents",
            ],
        )?;
        for key in ["maxContextTokens", "maxOutputTokens"] {
            let v = field(agent, key);
            if !matches!(v, JsValue::Undefined | JsValue::Null) && !integer(v, 1.0, false) {
                return Err(invalid(
                    &format!("agent.{key}"),
                    "must be a positive integer or null.",
                ));
            }
            add(&format!("agent.{key}"), v);
        }
        let thinking = field(agent, "thinking");
        if !matches!(thinking, JsValue::Undefined | JsValue::Null) {
            if thinking.as_object().is_none() {
                return Err(invalid("agent.thinking", "must be an object or null."));
            }
            keys(thinking, "agent.thinking", &["enabled", "budgetTokens"])?;
            if field(thinking, "enabled").as_bool().is_none() {
                return Err(invalid("agent.thinking.enabled", "must be a boolean."));
            }
            let budget = field(thinking, "budgetTokens");
            if !budget.is_undefined() && !integer(budget, 0.0, false) {
                return Err(invalid(
                    "agent.thinking.budgetTokens",
                    "must be a non-negative integer.",
                ));
            }
        }
        add("agent.thinking", thinking);
        let subs = field(agent, "subagents");
        if matches!(subs, JsValue::Null) {
            add("agent.subagents", subs);
        } else if !subs.is_undefined() {
            if subs.as_object().is_none() {
                return Err(invalid("agent.subagents", "must be an object or null."));
            }
            keys(
                subs,
                "agent.subagents",
                &["default", "timeoutMs", "maxDepth"],
            )?;
            let v = field(subs, "default");
            if !matches!(v, JsValue::Undefined | JsValue::Null) {
                let model = v
                    .as_string()
                    .and_then(|s| s.to_utf8().ok())
                    .filter(|s| !crate::metadata::js_trim(s).is_empty())
                    .ok_or_else(|| {
                        invalid(
                            "agent.subagents.default",
                            "must be a non-empty \"provider/model\" string, \"inherit\", or null.",
                        )
                    })?;
                add(
                    "agent.subagents.default",
                    &crate::metadata::js_trim(&model).into(),
                );
            } else {
                add("agent.subagents.default", v);
            }
            for (key, min, safe, message) in [
                (
                    "timeoutMs",
                    1.0,
                    false,
                    "must be a positive integer or null.",
                ),
                (
                    "maxDepth",
                    0.0,
                    true,
                    "must be a non-negative safe integer or null.",
                ),
            ] {
                let v = field(subs, key);
                if !matches!(v, JsValue::Undefined | JsValue::Null) && !integer(v, min, safe) {
                    return Err(invalid(&format!("agent.subagents.{key}"), message));
                }
                add(&format!("agent.subagents.{key}"), v);
            }
        }
    }
    let extension = field(settings, "extension");
    if !extension.is_undefined() {
        keys(
            extension,
            "extension",
            &["includeHookEvents", "builtinPluginsEnabled"],
        )?;
        let v = field(extension, "includeHookEvents");
        if !matches!(v, JsValue::Undefined | JsValue::Null | JsValue::Bool(_)) {
            return Err(invalid(
                "extension.includeHookEvents",
                "must be a boolean or null.",
            ));
        }
        add("extension.includeHookEvents", v);
        let v = field(extension, "builtinPluginsEnabled");
        if !matches!(v, JsValue::Undefined | JsValue::Null) {
            let map = v.as_object().ok_or_else(|| {
                invalid(
                    "extension.builtinPluginsEnabled",
                    "must be an object of booleans or null.",
                )
            })?;
            if map.entries().iter().any(|(k, v)| {
                crate::metadata::js_trim(&String::from_utf16_lossy(k.units())).is_empty()
                    || v.as_bool().is_none()
            }) {
                return Err(invalid(
                    "extension.builtinPluginsEnabled",
                    "must be an object with non-empty keys and boolean values.",
                ));
            }
        }
        add("extension.builtinPluginsEnabled", v);
    }
    let tools = field(settings, "tools");
    if !tools.is_undefined() {
        keys(tools, "tools", &["webSearch"])?;
        let search = field(tools, "webSearch");
        if !search.is_undefined() {
            keys(search, "tools.webSearch", &["enabled"])?;
            let v = field(search, "enabled");
            if !matches!(v, JsValue::Undefined | JsValue::Null | JsValue::Bool(_)) {
                return Err(invalid(
                    "tools.webSearch.enabled",
                    "must be a boolean or null.",
                ));
            }
            add("tools.webSearch.enabled", v);
        }
    }
    Ok(edits)
}

pub(super) fn update(
    routes: &ConfigRoutes,
    settings: &JsValue,
) -> Result<(Vec<String>, Vec<String>)> {
    let edits = normalize(settings)?;
    let path = routes.home.join("pilotdeck.yaml");
    let mut source = fs::read_to_string(&path).map_err(|e| {
        RouteError::new(
            "LOCAL_SETTINGS_READ_FAILED",
            format!("Unable to read local settings: {e}"),
        )
    })?;
    document(&source)?;
    let mut applied = Vec::new();
    let mut cleared = Vec::new();
    for (path, value) in edits {
        source = edit(&source, &path, &value)?;
        if value == JsValue::Null {
            cleared.push(path.join("."));
        } else {
            applied.push(path.join("."));
        }
    }
    if applied.is_empty() && cleared.is_empty() {
        return Ok((applied, cleared));
    }
    // The sibling is create_new and is removed on every failure; never replace before validation.
    let candidate =
        path.with_file_name(format!(".pilotdeck.yaml.sdk-{}.tmp", uuid::Uuid::new_v4()));
    let outcome = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)?;
        file.set_permissions(fs::metadata(&path)?.permissions())?;
        file.write_all(source.as_bytes())?;
        file.sync_all()?;
        Ok::<_, std::io::Error>(())
    })();
    let result = outcome
        .map_err(|e| {
            RouteError::new(
                "LOCAL_SETTINGS_WRITE_FAILED",
                format!("Unable to write local settings: {e}"),
            )
        })
        .and_then(|()| {
            let mut env = routes.env.clone();
            env.insert(
                "PILOTDECK_CONFIG_PATH".into(),
                candidate.to_string_lossy().into_owned(),
            );
            crate::config::load_project(
                &routes.home,
                &routes.cwd,
                &env,
                Some(&routes.cwd.to_string_lossy()),
            )
            .map_err(|e| {
                RouteError::new(
                    "LOCAL_SETTINGS_INVALID",
                    format!(
                        "localSettings failed PilotDeck validation: {}",
                        String::from_utf16_lossy(e.message.units())
                    ),
                )
            })?;
            fs::rename(&candidate, &path).map_err(|e| {
                RouteError::new(
                    "LOCAL_SETTINGS_WRITE_FAILED",
                    format!("Unable to write local settings: {e}"),
                )
            })
        });
    let _ = fs::remove_file(candidate);
    result?;
    Ok((applied, cleared))
}
fn document(source: &str) -> Result<Document> {
    let doc = parse_document(
        &JsString::from(source),
        Options {
            pretty_errors: false,
            ..Options::default()
        },
    );
    let mapping = doc
        .contents
        .is_some_and(|id| matches!(doc.nodes[id].kind, NodeKind::Map(_)));
    if !doc.errors.is_empty() || !mapping {
        let detail = doc
            .errors
            .first()
            .map(|e| String::from_utf16_lossy(e.message.units()))
            .unwrap_or_else(|| "Config root must be a YAML mapping.".into());
        return Err(RouteError::new(
            "LOCAL_SETTINGS_YAML_INVALID",
            format!("Unable to update local settings: {detail}"),
        ));
    }
    Ok(doc)
}
fn json(v: &JsValue) -> String {
    stringify_json(v)
        .expect("wire value")
        .expect("defined value")
}
fn nested(path: &[String], value: &JsValue) -> String {
    let mut v = value.clone();
    for key in path.iter().rev() {
        let mut obj = pilotdeck_protocol::JsObject::new();
        obj.insert(key.as_str().into(), v);
        v = JsValue::Object(obj);
    }
    json(&v)
}
/// Edit only the affected source ranges: comments, anchors and unrelated values survive.
pub(super) fn edit(source: &str, path: &[String], value: &JsValue) -> Result<String> {
    let doc = document(source)?;
    let mut id = doc.contents.expect("mapping");
    let mut units: Vec<u16> = source.encode_utf16().collect();
    for (depth, key) in path.iter().enumerate() {
        let node = &doc.nodes[id];
        let NodeKind::Map(pairs) = &node.kind else {
            return Err(RouteError::new(
                "LOCAL_SETTINGS_YAML_INVALID",
                "Unable to update local settings: Expected a YAML mapping.",
            ));
        };
        let found = pairs.iter().position(|pair| matches!(&doc.nodes[pair.key].kind, NodeKind::Scalar(ScalarValue::String(s)) if s.equals_str(key)));
        if let Some(index) = found {
            let pair = &pairs[index];
            if depth + 1 < path.len() {
                if let Some(child) = pair.value
                    && matches!(doc.nodes[child].kind, NodeKind::Map(_))
                {
                    id = child;
                    continue;
                }
                if *value == JsValue::Null {
                    return Ok(source.into());
                }
            }
            let replacement = if depth + 1 == path.len() {
                json(value)
            } else {
                nested(&path[depth + 1..], value)
            };
            let key_node = &doc.nodes[pair.key];
            if depth + 1 == path.len() && *value == JsValue::Null {
                if !node.flow && pairs.len() == 1 {
                    // YAML null is not an empty mapping: preserve the parent object's shape.
                    units.splice(node.range[0]..node.range[1], "{}\n".encode_utf16());
                    return Ok(String::from_utf16_lossy(&units));
                }
                let start = key_node.range[0];
                let end = pair
                    .value
                    .map_or(key_node.range[2], |i| doc.nodes[i].range[2]);
                let (start, end) = if node.flow {
                    if index + 1 < pairs.len() {
                        (start, doc.nodes[pairs[index + 1].key].range[0])
                    } else if index > 0 {
                        (
                            pair_end(&doc, &pairs[index - 1]),
                            end.min(node.range[1] - 1),
                        )
                    } else {
                        (start, end.min(node.range[1] - 1))
                    }
                } else {
                    let start = units[..start]
                        .iter()
                        .rposition(|u| *u == 10)
                        .map_or(0, |n| n + 1);
                    (start, end)
                };
                units.splice(start..end, []);
            } else if let Some(child) = pair.value {
                let child = &doc.nodes[child];
                let multiline = units[key_node.range[1]..child.range[0]].contains(&10);
                let text = if multiline {
                    format!("{replacement}\n")
                } else {
                    replacement
                };
                units.splice(child.range[0]..child.range[1], text.encode_utf16());
            } else {
                units.splice(
                    key_node.range[1] + 1..key_node.range[1] + 1,
                    format!(" {replacement}").encode_utf16(),
                );
            }
            return Ok(String::from_utf16_lossy(&units));
        }
        if *value == JsValue::Null {
            return Ok(source.into());
        }
        let text = format!(
            "{}: {}",
            json(&key.as_str().into()),
            if depth + 1 == path.len() {
                json(value)
            } else {
                nested(&path[depth + 1..], value)
            }
        );
        if node.flow {
            let at = node.range[1] - 1;
            units.splice(
                at..at,
                format!("{}{text}", if pairs.is_empty() { "" } else { ", " }).encode_utf16(),
            );
        } else {
            let at = node.range[1];
            let indent = if let Some(first) = pairs.first() {
                let start = doc.nodes[first.key].range[0];
                start
                    - units[..start]
                        .iter()
                        .rposition(|u| *u == 10)
                        .map_or(0, |p| p + 1)
            } else {
                depth * 2
            };
            let prefix = if at > 0 && units[at - 1] != 10 {
                "\n"
            } else {
                ""
            };
            units.splice(
                at..at,
                format!("{prefix}{}{text}\n", " ".repeat(indent)).encode_utf16(),
            );
        }
        return Ok(String::from_utf16_lossy(&units));
    }
    unreachable!("nonempty allowlisted paths")
}
fn pair_end(doc: &Document, pair: &pilotdeck_yaml::nodes::Pair) -> usize {
    pair.value.map_or(doc.nodes[pair.key].range[1] + 1, |id| {
        doc.nodes[id].range[1]
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn edits_preserve_other_yaml_and_validate_allowlist() {
        let source = "# keep\nagent:\n  model: p/m # model\n  maxOutputTokens: 30 # tokens\nmodel: { providers: [] }\n";
        let out = edit(
            source,
            &["agent".into(), "maxOutputTokens".into()],
            &JsValue::Number(40.0),
        )
        .unwrap();
        assert!(out.contains("40 # tokens"));
        assert!(out.contains("model: p/m # model"));
        let out = edit(
            &out,
            &["agent".into(), "maxOutputTokens".into()],
            &JsValue::Null,
        )
        .unwrap();
        assert!(!out.contains("maxOutputTokens"));
        assert!(out.contains("model: { providers: [] }"));
        let out = edit(
            "agent: {model: p/m}\n",
            &["agent".into(), "maxDepth".into()],
            &JsValue::Number(1.0),
        )
        .unwrap();
        assert!(document(&out).is_ok(), "{out}");
        let settings =
            pilotdeck_protocol::parse_json(r#"{"model":{"apiKey":"forbidden"}}"#).unwrap();
        assert_eq!(
            normalize(&settings).unwrap_err().code,
            "UNSUPPORTED_LOCAL_SETTING"
        );
    }
}
