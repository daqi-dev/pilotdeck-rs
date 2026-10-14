//! Inspect the YAML value graph without flattening unused cyclic aliases.
//! Validators only inspect a finite schema. A role selects which child fields
//! are inspected; opaque values retain their JS type and own keys. Arrays used
//! by String(value) retain their elements, with JS's cyclic-array join behavior.
use pilotdeck_protocol::{JsObject, JsValue};
use pilotdeck_yaml::value::{Container, Value, ValueGraph};
#[derive(Clone, Copy)]
enum Role {
    Root,
    SopBundle,
    SopRows,
    Agent,
    Subagents,
    Model,
    Providers,
    Provider,
    Models,
    Definition,
    Fields,
    Extension,
    Memory,
    Router,
    TokenSaver,
    Tiers,
    Stats,
    Pricing,
    Always,
    Projects,
    Tools,
    WebSearch,
    Modules,
    Module,
    RunPolicy,
    Guard,
    Array,
    Opaque,
}
fn child(role: Role, key: &str) -> Role {
    use Role::*;
    match role {
        SopBundle => {
            if key == "sops" {
                SopRows
            } else {
                Opaque
            }
        }
        SopRows => Fields,
        Root => match key {
            "agent" => Agent,
            "__proto__" => Root,
            "model" => Model,
            "extension" => Extension,
            "memory" => Memory,
            "router" => Router,
            "alwaysOn" => Always,
            "tools" => Tools,
            "modules" => Modules,
            "runPolicy" => RunPolicy,
            "gateway" | "cron" | "proxy" => Fields,
            _ => Opaque,
        },
        Agent => {
            if key == "subagents" {
                Subagents
            } else {
                Opaque
            }
        }
        Subagents => Opaque,
        Model => {
            if key == "providers" {
                Providers
            } else {
                Opaque
            }
        }
        Providers => Provider,
        Provider => match key {
            "models" => Models,
            "headers" | "retry" => Fields,
            _ => Opaque,
        },
        Models => Definition,
        Definition => match key {
            "capabilities" | "multimodal" | "thinking" => Fields,
            "aliases" => Array,
            _ => Opaque,
        },
        Extension => {
            if key == "builtinPluginsEnabled" {
                Fields
            } else {
                Opaque
            }
        }
        Memory => {
            if key == "schedule" {
                Fields
            } else {
                Opaque
            }
        }
        Router => match key {
            "tokenSaver" => TokenSaver,
            "stats" => Stats,
            "scenarios" | "zeroUsageRetry" | "transientRetry" | "customRouter"
            | "autoOrchestrate" | "fallback" => Fields,
            _ => Opaque,
        },
        TokenSaver => match key {
            "tiers" => Tiers,
            "subagent" | "cacheAwareSwitching" => Fields,
            "rules" => Array,
            _ => Opaque,
        },
        Tiers => Fields,
        Stats => match key {
            "modelPricing" => Pricing,
            "baselineModel" => Fields,
            _ => Opaque,
        },
        Pricing => Fields,
        Always => match key {
            "trigger" => Fields,
            "projects" => Projects,
            _ => Opaque,
        },
        Projects => Fields,
        Tools => {
            if key == "webSearch" {
                WebSearch
            } else {
                Opaque
            }
        }
        WebSearch => {
            if key == "customProvider" {
                Fields
            } else {
                Opaque
            }
        }
        Modules => Module,
        Module => match key {
            "deployment" => Fields,
            "catalog" => Array,
            "env" => Fields,
            _ => Opaque,
        },
        RunPolicy => {
            if key == "failureGuard" {
                Guard
            } else {
                Opaque
            }
        }
        Guard => match key {
            "toolFailureLimits" | "toolLabels" => Fields,
            _ => Opaque,
        },
        Fields | Array | Opaque => Opaque,
    }
}
pub(super) fn inspect_sops(g: &ValueGraph) -> JsValue {
    convert(g, &g.root, Role::SopBundle, &mut vec![])
}
pub(super) fn inspect(g: &ValueGraph) -> JsValue {
    convert(g, &g.root, Role::Root, &mut vec![])
}
fn convert(g: &ValueGraph, v: &Value, role: Role, active: &mut Vec<usize>) -> JsValue {
    match v {
        Value::Null => JsValue::Null,
        Value::Bool(b) => JsValue::Bool(*b),
        Value::Number(n) => JsValue::Number(*n),
        Value::String(s) => JsValue::String(s.as_ref().clone()),
        Value::Symbol { .. } => JsValue::Undefined,
        Value::Ref(id) => match &g.nodes[*id] {
            Container::Object(entries) => {
                if matches!(role, Role::Root) && active.contains(id) {
                    return JsValue::Object(JsObject::new());
                }
                active.push(*id);
                let mut o = JsObject::new();
                for (k, v) in entries {
                    let value = if matches!(role, Role::Opaque) {
                        // Only typeof/Object.keys is observed on opaque objects.
                        JsValue::Undefined
                    } else {
                        let key = k.to_utf8().unwrap_or_default();
                        convert(g, v, child(role, &key), active)
                    };
                    o.insert(k.clone(), value);
                }
                active.pop();
                JsValue::Object(o)
            }
            Container::Array(items) => {
                if active.contains(id) {
                    return JsValue::Array(vec![]);
                }
                active.push(*id);
                let role = if matches!(role, Role::Array | Role::SopRows) {
                    Role::Fields
                } else {
                    Role::Opaque
                };
                let a = items.iter().map(|v| convert(g, v, role, active)).collect();
                active.pop();
                JsValue::Array(a)
            }
            Container::Map(_) | Container::Set(_) => {
                let mut o = JsObject::new();
                o.set_native_tag(if matches!(g.nodes[*id], Container::Map(_)) {
                    "Map"
                } else {
                    "Set"
                });
                JsValue::Object(o)
            }
        },
    }
}
fn field<'a>(g: &'a ValueGraph, v: &'a Value, key: &str) -> Option<&'a Value> {
    let Value::Ref(id) = v else { return None };
    let Container::Object(entries) = &g.nodes[*id] else {
        return None;
    };
    entries
        .iter()
        .find(|(k, _)| k.equals_str(key))
        .map(|(_, v)| v)
}
fn at<'a>(g: &'a ValueGraph, keys: &[&str]) -> Option<&'a Value> {
    let mut v = &g.root;
    for (index, k) in keys.iter().enumerate() {
        if index == 0 {
            let mut seen = std::collections::HashSet::new();
            while field(g, v, k).is_none() {
                v = field(g, v, "__proto__")?;
                let Value::Ref(id) = v else { return None };
                if !seen.insert(*id) {
                    return None;
                }
            }
        }
        v = field(g, v, k)?;
    }
    Some(v)
}
fn object(g: &ValueGraph, v: &Value) -> bool {
    matches!(v,Value::Ref(id) if matches!(g.nodes[*id],Container::Object(_)))
}
fn cycle(g: &ValueGraph, v: &Value, skip_temperature: bool) -> bool {
    // Iterative DFS: raw schemas may be deeply nested even though validators do
    // not descend into them. Sharing alone is not a cycle.
    let mut active = std::collections::HashSet::new();
    let mut done = std::collections::HashSet::new();
    let mut pending = vec![(v, false, if skip_temperature { 2u8 } else { 0u8 })];
    while let Some((v, exit, skip)) = pending.pop() {
        let Value::Ref(id) = v else { continue };
        if exit {
            active.remove(&(*id, skip));
            done.insert((*id, skip));
            continue;
        }
        if done.contains(&(*id, skip)) {
            continue;
        }
        if !active.insert((*id, skip)) {
            return true;
        }
        pending.push((v, true, skip));
        match &g.nodes[*id] {
            Container::Object(entries) => {
                for (k, v) in entries.iter().rev() {
                    if skip > 0 && k.equals_str("temperature") {
                        continue;
                    }
                    pending.push((
                        v,
                        false,
                        if skip == 2 && k.equals_str("generationConfig") {
                            1
                        } else {
                            0
                        },
                    ));
                }
            }
            Container::Array(items) => {
                for v in items.iter().rev() {
                    pending.push((v, false, 0));
                }
            }
            _ => {}
        }
    }
    false
}
pub(super) fn retained_cycle(
    g: &ValueGraph,
    providers: &[super::model::Provider],
    raw: &JsValue,
) -> bool {
    // sanitizeProviderBody drops temperature at the body and generationConfig
    // levels. Returned bodies, adapter extra data and external ToolPort schemas
    // survive validation and participate in redact/hash/deepFreeze.
    for p in providers {
        let Some(pv) = at(g, &["model", "providers"]).and_then(|v| {
            let Value::Ref(id) = v else { return None };
            let Container::Object(a) = &g.nodes[*id] else {
                return None;
            };
            a.iter().find(|(k, _)| *k == p.id).map(|(_, v)| v)
        }) else {
            continue;
        };
        if let Some(body) = field(g, pv, "extraBody").filter(|v| object(g, v))
            && cycle(g, body, true)
        {
            return true;
        }
    }
    for name in [
        "telegram",
        "discord",
        "slack",
        "matrix",
        "mattermost",
        "signal",
        "whatsapp",
        "bluebubbles",
        "dingtalk",
        "wecom",
        "wecomCallback",
        "email",
        "sms",
        "homeassistant",
        "apiServer",
        "webhook",
    ] {
        if let Some(extra) = at(g, &["adapters", name, "extra"]).filter(|v| object(g, v))
            && cycle(g, extra, false)
        {
            return true;
        }
    }
    let tools = super::value::field(super::value::field(raw, "modules"), "tools");
    if super::value::field(tools, "enabled").as_bool() == Some(true)
        && super::value::field(tools, "provider").is_undefined()
        && super::value::eq(super::value::field(tools, "transport"), "module-http-v2")
        && let Some(Value::Ref(id)) = at(g, &["modules", "tools", "catalog"])
        && let Container::Array(a) = &g.nodes[*id]
    {
        for tool in a {
            if let Some(schema) = field(g, tool, "inputSchema")
                && cycle(g, schema, false)
            {
                return true;
            }
        }
    }
    false
}

pub(super) fn merge_cycle(g: &ValueGraph) -> bool {
    let mut v = &g.root;
    let mut seen = std::collections::HashSet::new();
    while let Some(p) = field(g, v, "__proto__") {
        let Value::Ref(id) = p else { return false };
        if !matches!(g.nodes[*id], Container::Object(_)) {
            return false;
        }
        if !seen.insert(*id) {
            return true;
        }
        v = p;
    }
    false
}
