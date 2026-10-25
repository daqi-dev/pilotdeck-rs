//! Persistent hook parsing and matching, following the fixed TS baseline.
use super::{protocol::*, regexp};
use pilotdeck_protocol::{CodecError, JsObject, JsString, JsValue, stringify_json};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ParseHooksConfigResult {
    pub settings: HooksSettings,
    pub diagnostics: Vec<JsString>,
}
impl ParseHooksConfigResult {
    pub fn to_value(&self) -> JsValue {
        let mut settings = JsObject::new();
        for (event, matchers) in &self.settings {
            settings.insert(
                event.clone(),
                JsValue::Array(matchers.iter().map(HookMatcher::to_value).collect()),
            );
        }
        let mut result = JsObject::new();
        result.insert("settings".into(), JsValue::Object(settings));
        result.insert(
            "diagnostics".into(),
            JsValue::Array(
                self.diagnostics
                    .iter()
                    .cloned()
                    .map(JsValue::String)
                    .collect(),
            ),
        );
        JsValue::Object(result)
    }
}

pub fn parse_hooks_config(raw: &JsValue) -> ParseHooksConfigResult {
    let mut result = ParseHooksConfigResult::default();
    if matches!(raw, JsValue::Null | JsValue::Undefined) {
        return result;
    }
    let Some(object) = raw.as_object() else {
        result
            .diagnostics
            .push("Hooks config must be an object.".into());
        return result;
    };
    for (event, raw_matchers) in ordered_entries(object) {
        if !is_hook_event(event) {
            diagnostic(
                &mut result.diagnostics,
                "Unsupported hook event ",
                event,
                ".",
            );
            continue;
        }
        let JsValue::Array(raw_matchers) = raw_matchers else {
            diagnostic(
                &mut result.diagnostics,
                "Hook event ",
                event,
                " must contain an array of matchers.",
            );
            continue;
        };
        let mut matchers = Vec::new();
        for raw in raw_matchers {
            let Some(matcher) = raw.as_object() else {
                diagnostic(
                    &mut result.diagnostics,
                    "Hook matcher for ",
                    event,
                    " must be an object.",
                );
                continue;
            };
            let Some(JsValue::Array(hooks)) = matcher.get_property("hooks") else {
                diagnostic(
                    &mut result.diagnostics,
                    "Hook matcher for ",
                    event,
                    " must contain hooks array.",
                );
                continue;
            };
            matchers.push(HookMatcher {
                matcher: string(matcher, "matcher"),
                plugin_name: string(matcher, "pluginName"),
                plugin_id: string(matcher, "pluginId"),
                plugin_root: string(matcher, "pluginRoot"),
                hooks: hooks
                    .iter()
                    .filter_map(|hook| parse_command(event, hook, &mut result.diagnostics))
                    .collect(),
            });
        }
        if !matchers.is_empty() {
            result.settings.push((event.clone(), matchers));
        }
    }
    result
}
fn parse_command(
    event: &JsString,
    raw: &JsValue,
    diagnostics: &mut Vec<JsString>,
) -> Option<HookCommand> {
    let Some((object, kind)) = raw
        .as_object()
        .and_then(|o| string(o, "type").map(|s| (o, s)))
    else {
        diagnostic(diagnostics, "Hook for ", event, " must contain a type.");
        return None;
    };
    let common = HookCommon {
        condition: string(object, "if"),
        status_message: string(object, "statusMessage"),
        once: boolean(object, "once"),
        timeout: object
            .get_property("timeout")
            .and_then(JsValue::as_number)
            .filter(|n| n.is_finite()),
    };
    let (field, prefix) = if kind.equals_str("command") {
        ("command", "Command")
    } else if kind.equals_str("prompt") {
        ("prompt", "Prompt")
    } else if kind.equals_str("http") {
        ("url", "HTTP")
    } else if kind.equals_str("agent") {
        ("prompt", "Agent")
    } else if kind.equals_str("callback") {
        diagnostic(
            diagnostics,
            "Callback hook for ",
            event,
            " is runtime-only and cannot be loaded from persistent config.",
        );
        return None;
    } else {
        let mut text: Vec<_> = "Unsupported hook type ".encode_utf16().collect();
        text.extend_from_slice(kind.units());
        text.extend(" for ".encode_utf16());
        text.extend_from_slice(event.units());
        text.push(46);
        diagnostics.push(JsString::from_utf16(text));
        return None;
    };
    let Some(required) = string(object, field) else {
        diagnostic(
            diagnostics,
            &format!("{prefix} hook for "),
            event,
            &format!(" must contain {field}."),
        );
        return None;
    };
    Some(match field {
        "command" => HookCommand::Command {
            command: required,
            shell: string(object, "shell").and_then(|s| {
                if s.equals_str("bash") {
                    Some(HookShell::Bash)
                } else if s.equals_str("powershell") {
                    Some(HookShell::Powershell)
                } else {
                    None
                }
            }),
            asynchronous: boolean(object, "async"),
            async_rewake: boolean(object, "asyncRewake"),
            common,
        },
        "url" => HookCommand::Http {
            url: required,
            headers: object
                .get_property("headers")
                .and_then(JsValue::as_object)
                .map(|headers| {
                    let mut out = JsObject::new();
                    for (key, value) in ordered_entries(headers) {
                        if value.as_string().is_some() {
                            out.insert(key.clone(), value.clone());
                        }
                    }
                    out
                }),
            allowed_env_vars: match object.get_property("allowedEnvVars") {
                Some(JsValue::Array(items)) => Some(
                    items
                        .iter()
                        .filter_map(JsValue::as_string)
                        .cloned()
                        .collect(),
                ),
                _ => None,
            },
            common,
        },
        _ if kind.equals_str("prompt") => HookCommand::Prompt {
            prompt: required,
            model: string(object, "model"),
            common,
        },
        _ => HookCommand::Agent {
            prompt: required,
            model: string(object, "model"),
            common,
        },
    })
}
fn string(object: &JsObject, key: &str) -> Option<JsString> {
    object
        .get_property(key)
        .and_then(JsValue::as_string)
        .cloned()
}
fn boolean(object: &JsObject, key: &str) -> Option<bool> {
    object.get_property(key).and_then(JsValue::as_bool)
}
fn diagnostic(out: &mut Vec<JsString>, prefix: &str, event: &JsString, suffix: &str) {
    let mut units: Vec<_> = prefix.encode_utf16().collect();
    units.extend_from_slice(event.units());
    units.extend(suffix.encode_utf16());
    out.push(JsString::from_utf16(units));
}

pub fn match_hook_matcher(matcher: Option<&JsString>, query: Option<&JsString>) -> bool {
    let matcher = matcher.map(JsString::units).unwrap_or_default();
    if matcher.is_empty() || matcher == [42] {
        return true;
    }
    let query = query.map(JsString::units).unwrap_or_default();
    if query.is_empty() {
        return false;
    }
    match_units(matcher, query)
}
fn match_units(matcher: &[u16], query: &[u16]) -> bool {
    if matcher.is_empty() || matcher == [42] {
        return true;
    }
    let alternatives: Vec<_> = matcher
        .split(|c| *c == 124)
        .map(trim)
        .filter(|s| !s.is_empty())
        .collect();
    if alternatives.len() > 1 {
        return alternatives.iter().any(|m| match_units(m, query));
    }
    if matcher.len() > 2 && matcher.first() == Some(&47) && matcher.last() == Some(&47) {
        return regexp::test(&matcher[1..matcher.len() - 1], query);
    }
    matcher == query
}

#[derive(Clone, Copy, Debug, Default)]
pub struct HookConditionInput<'a> {
    pub tool_name: Option<&'a JsString>,
    pub tool_input: Option<&'a JsValue>,
}

// Node 22.22.0: the condition entry serializes 6500 nested containers but
// throws at 7000; the exact first failure wanders in ~6580..6620 because V8's
// remaining stack depends on the caller. 6500 is the largest depth measured
// stable across that entry. The four hook executors (`CommandHookExecutor`,
// `HttpHookExecutor`, `PromptHookExecutor`, `AgentHookExecutor`) measured a
// first failure at 6886 and all accept 6500, so they share this fixed budget
// instead of `js_value::MAX_ARRAY_DEPTH` (2000), which answers a different
// predicate. Fixed-budget gap, not human-approved; see
// specs/gateway-parity/diffs/A6.7-HOOK-RESOURCE-001-R1.md.
pub(crate) const MAX_STRINGIFY_DEPTH: usize = 6500;

/// Unlike matcher regex errors, TS does not catch condition serialization errors.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HookConditionError {
    StackOverflow,
    Serialization(CodecError),
}
impl HookConditionError {
    pub fn name(&self) -> &'static str {
        match self {
            Self::StackOverflow => "RangeError",
            Self::Serialization(_) => "Error",
        }
    }
    pub fn message(&self) -> &str {
        match self {
            Self::StackOverflow => "Maximum call stack size exceeded",
            Self::Serialization(error) => &error.message,
        }
    }
}
impl std::fmt::Display for HookConditionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.name(), self.message())
    }
}
impl std::error::Error for HookConditionError {}

fn check_stringify_depth(value: &JsValue) -> Result<(), HookConditionError> {
    let mut pending = vec![(value, 0)];
    while let Some((value, parent_depth)) = pending.pop() {
        if matches!(value, JsValue::Array(_) | JsValue::Object(_)) {
            let depth = parent_depth + 1;
            if depth > MAX_STRINGIFY_DEPTH {
                return Err(HookConditionError::StackOverflow);
            }
            match value {
                JsValue::Array(items) => {
                    pending.extend(items.iter().map(|item| (item, depth)));
                }
                JsValue::Object(object) => {
                    // JSON.stringify visits own properties only, never prototypes.
                    pending.extend(object.entries().iter().map(|(_, item)| (item, depth)));
                }
                _ => unreachable!(),
            }
        }
    }
    Ok(())
}

pub fn match_hook_condition(
    condition: Option<&JsString>,
    input: HookConditionInput<'_>,
) -> Result<bool, HookConditionError> {
    let condition = condition.map(JsString::units).unwrap_or_default();
    if condition.is_empty() {
        return Ok(true);
    }
    let text = trim(condition);
    let name_len = text
        .iter()
        .take_while(|c| matches!(c, 65..=90 | 97..=122 | 48..=57 | 95 | 46 | 58 | 45))
        .count();
    if name_len == 0 {
        return Ok(false);
    }
    let tail = &text[name_len..];
    let pattern = if tail.is_empty() {
        &[][..]
    } else if tail.first() == Some(&40)
        && tail.last() == Some(&41)
        && !tail.iter().any(|c| matches!(c, 10 | 13 | 0x2028 | 0x2029))
    {
        &tail[1..tail.len() - 1]
    } else {
        return Ok(false);
    };
    if let Some(name) = input.tool_name.filter(|n| !n.is_empty())
        && normalize(&text[..name_len]) != normalize(name.units())
    {
        return Ok(false);
    }
    if pattern.is_empty() || pattern == [42] {
        return Ok(true);
    }
    let empty = JsValue::Object(JsObject::new());
    let value = input
        .tool_input
        .filter(|v| !matches!(v, JsValue::Null | JsValue::Undefined))
        .unwrap_or(&empty);
    check_stringify_depth(value)?;
    let json = stringify_json(value)
        .map_err(HookConditionError::Serialization)?
        .expect("nullish values were replaced with an object");
    let haystack: Vec<_> = json.encode_utf16().collect();
    let needle: Vec<_> = pattern.iter().copied().filter(|c| *c != 42).collect();
    Ok(needle.is_empty() || haystack.windows(needle.len()).any(|part| part == needle))
}
fn normalize(name: &[u16]) -> Vec<u16> {
    name.iter()
        .copied()
        .filter(|c| matches!(c, 65..=90 | 97..=122 | 48..=57))
        .map(|c| if (65..=90).contains(&c) { c + 32 } else { c })
        .collect()
}
pub(super) fn whitespace(c: u16) -> bool {
    matches!(c, 9..=13 | 32 | 0xa0 | 0x1680 | 0x2000..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff)
}
fn trim(mut text: &[u16]) -> &[u16] {
    while text.first().is_some_and(|c| whitespace(*c)) {
        text = &text[1..];
    }
    while text.last().is_some_and(|c| whitespace(*c)) {
        text = &text[..text.len() - 1];
    }
    text
}
