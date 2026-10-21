//! `decideScenario.ts`, `subagentDetector.ts` and `mediaRequirements.ts`.
use super::classify::matches_ascii_ci;
use super::{
    RouterConfig, RouterDecisionInput, RouterModelInvocation, RouterModelRef, field, js_text,
    request_messages, trim_end_units, trim_units, truthy,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};

#[derive(Clone, Debug, PartialEq)]
pub struct ScenarioResolution {
    pub scenario_type: &'static str,
    pub selection: Option<RouterModelRef>,
    pub is_subagent: bool,
    pub subagent_model_hint: Option<JsString>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SubagentDetection {
    pub is_subagent: bool,
    pub model_hint: Option<JsString>,
    pub missing_agent_tool: bool,
    pub tagged_in_user_message: bool,
}

const SUBAGENT_TAG_SUFFIX: &[u8] = b"-subagent-model>";
const SUBAGENT_TAG_BRANDS: [&[u8]; 2] = [b"pilotdeck", b"ccr"];

/// Match `<brand-subagent-model>` at `at`, returning the content start.
fn open_subagent_tag_end(units: &[u16], at: usize) -> Option<usize> {
    if units.get(at) != Some(&u16::from(b'<')) {
        return None;
    }
    SUBAGENT_TAG_BRANDS.iter().find_map(|brand| {
        let content_start = at + 1 + brand.len() + SUBAGENT_TAG_SUFFIX.len();
        (matches_ascii_ci(units, at + 1, brand)
            && matches_ascii_ci(units, at + 1 + brand.len(), SUBAGENT_TAG_SUFFIX))
        .then_some(content_start)
    })
}

/// Match `</brand-subagent-model>` at `at`, returning the match end.
fn close_subagent_tag_end(units: &[u16], at: usize) -> Option<usize> {
    if !matches_ascii_ci(units, at, b"</") {
        return None;
    }
    SUBAGENT_TAG_BRANDS.iter().find_map(|brand| {
        let end = at + 2 + brand.len() + SUBAGENT_TAG_SUFFIX.len();
        (matches_ascii_ci(units, at + 2, brand)
            && matches_ascii_ci(units, at + 2 + brand.len(), SUBAGENT_TAG_SUFFIX))
        .then_some(end)
    })
}

/// `/<(?:pilotdeck|ccr)-subagent-model>([\s\S]+?)<\/(?:pilotdeck|ccr)-subagent-model>/i`
/// over UTF-16 code units. TS has no `u` flag, so `i` cannot fold U+017F onto
/// ASCII `s` (the old Rust `(?i)` did); `[\s\S]+?` is any code unit and lazy.
fn find_subagent_tag(units: &[u16], from: usize) -> Option<(usize, usize, usize, usize)> {
    for start in from..units.len() {
        if units[start] != u16::from(b'<') {
            continue;
        }
        let Some(content_start) = open_subagent_tag_end(units, start) else {
            continue;
        };
        let mut content_end = content_start + 1;
        while content_end < units.len() {
            if let Some(end) = close_subagent_tag_end(units, content_end) {
                return Some((start, end, content_start, content_end));
            }
            content_end += 1;
        }
    }
    None
}

/// `/^(?:agent|task|launch[_-]?agent|spawn[_-]?agent)$/i` over UTF-16 units.
fn is_agent_tool_name(units: &[u16]) -> bool {
    if units.len() == 5 && matches_ascii_ci(units, 0, b"agent") {
        return true;
    }
    if units.len() == 4 && matches_ascii_ci(units, 0, b"task") {
        return true;
    }
    [b"launch".as_slice(), b"spawn".as_slice()]
        .iter()
        .any(|prefix| {
            matches_ascii_ci(units, 0, prefix)
                && ((units.len() == prefix.len() + 5
                    && matches_ascii_ci(units, prefix.len(), b"agent"))
                    || (units.len() == prefix.len() + 6
                        && matches!(units[prefix.len()], 45 | 95)
                        && matches_ascii_ci(units, prefix.len() + 1, b"agent")))
        })
}

pub fn decide_scenario(input: &RouterDecisionInput, config: &RouterConfig) -> ScenarioResolution {
    if let Some(explicit) = read_explicit(input) {
        return ScenarioResolution {
            scenario_type: "explicit",
            selection: Some(explicit),
            is_subagent: !input.is_main_agent,
            subagent_model_hint: None,
        };
    }

    let messages = request_messages(&input.request);
    let tools = field(&input.request, "tools");
    let subagent = detect_subagent(&messages, tools, input.is_main_agent);

    if let Some(hint) = subagent
        .model_hint
        .as_ref()
        .filter(|hint| truthy(&JsValue::String((*hint).clone())))
    {
        return ScenarioResolution {
            scenario_type: "subagent",
            selection: None,
            is_subagent: true,
            subagent_model_hint: Some(hint.clone()),
        };
    }

    ScenarioResolution {
        scenario_type: "default",
        selection: config.scenarios_default.clone(),
        is_subagent: subagent.is_subagent,
        subagent_model_hint: subagent.model_hint,
    }
}

fn read_explicit(input: &RouterDecisionInput) -> Option<RouterModelRef> {
    let metadata = input.metadata.as_ref()?;
    let provider = metadata
        .get("explicitProvider")
        .and_then(JsValue::as_string)?;
    let model = metadata.get("explicitModel").and_then(JsValue::as_string)?;
    if truthy(&JsValue::String(provider.clone())) && truthy(&JsValue::String(model.clone())) {
        Some(RouterModelRef::new(provider.clone(), model.clone()))
    } else {
        None
    }
}

pub fn detect_subagent(
    messages: &[JsValue],
    tools: &JsValue,
    is_main_agent: bool,
) -> SubagentDetection {
    let mut model_hint = None;
    let mut tagged_in_user_message = false;
    for message in messages {
        if field(message, "role")
            .as_string()
            .is_none_or(|role| !role.equals_str("user"))
        {
            continue;
        }
        let JsValue::Array(blocks) = field(message, "content") else {
            continue;
        };
        for block in blocks {
            if field(block, "type")
                .as_string()
                .is_none_or(|kind| !kind.equals_str("text"))
            {
                continue;
            }
            let Some(text) = js_text(field(block, "text")) else {
                continue;
            };
            if let Some((_, _, start, end)) = find_subagent_tag(text.units(), 0) {
                model_hint = Some(JsString::from_utf16(
                    trim_units(&text.units()[start..end]).to_vec(),
                ));
                tagged_in_user_message = true;
            }
        }
    }

    let missing_agent_tool = match tools {
        JsValue::Array(items) => !items.iter().any(|tool| {
            js_text(field(tool, "name")).is_some_and(|name| is_agent_tool_name(name.units()))
        }),
        _ => false,
    };

    SubagentDetection {
        is_subagent: !is_main_agent || tagged_in_user_message || missing_agent_tool,
        model_hint,
        missing_agent_tool,
        tagged_in_user_message,
    }
}

/// Strip one subagent tag per user text block and JS `trimEnd` the remainder.
pub fn strip_subagent_tag_from_messages(messages: &[JsValue]) -> Vec<JsValue> {
    messages
        .iter()
        .map(|message| {
            let is_user = field(message, "role")
                .as_string()
                .is_some_and(|role| role.equals_str("user"));
            if !is_user {
                return message.clone();
            }
            let JsValue::Array(blocks) = field(message, "content") else {
                return message.clone();
            };
            let mut changed = false;
            let mut content = Vec::with_capacity(blocks.len());
            for block in blocks {
                let is_text = field(block, "type")
                    .as_string()
                    .is_some_and(|kind| kind.equals_str("text"));
                if !is_text {
                    content.push(block.clone());
                    continue;
                }
                let Some(text) = js_text(field(block, "text")) else {
                    content.push(block.clone());
                    continue;
                };
                let units = strip_tag_units(text.units());
                let trimmed = trim_end_units(&units);
                if trimmed != text.units() {
                    changed = true;
                    let mut copy = block.as_object().cloned().unwrap_or_default();
                    copy.insert(
                        "text".into(),
                        JsValue::String(JsString::from_utf16(trimmed.to_vec())),
                    );
                    content.push(JsValue::Object(copy));
                } else {
                    content.push(block.clone());
                }
            }
            if !changed {
                return message.clone();
            }
            let mut copy = message.as_object().cloned().unwrap_or_default();
            copy.insert("content".into(), JsValue::Array(content));
            JsValue::Object(copy)
        })
        .collect()
}

fn utf16_lossy(value: &JsString) -> String {
    String::from_utf16_lossy(value.units())
}

/// One `String.prototype.replace(tagRegex, "")` over UTF-16 units.
fn strip_tag_units(units: &[u16]) -> Vec<u16> {
    let Some((start, end, _, _)) = find_subagent_tag(units, 0) else {
        return units.to_vec();
    };
    let mut out = units[..start].to_vec();
    out.extend_from_slice(&units[end..]);
    out
}

/// `cleanup` for `rerouteDecisionForMedia`.
pub fn reroute_decision_for_media(
    config: &RouterConfig,
    invoker: &dyn RouterModelInvocation,
    decision: &mut JsObject,
    messages: &[JsValue],
    mutations: &mut JsObject,
) {
    let required = collect_required_input_modalities(messages);
    if required.is_empty() {
        return;
    }
    let provider = decision
        .get("provider")
        .and_then(JsValue::as_string)
        .cloned()
        .unwrap_or_default();
    let model = decision
        .get("model")
        .and_then(JsValue::as_string)
        .cloned()
        .unwrap_or_default();
    let selected = RouterModelRef::new(provider, model);
    if supports_media_requirements(invoker, &selected, &required) {
        return;
    }
    let scenario = decision
        .get("scenarioType")
        .and_then(JsValue::as_string)
        .map(utf16_lossy)
        .unwrap_or_default();
    let Some(replacement) = config
        .fallback
        .candidates(&scenario)
        .into_iter()
        .find(|candidate| supports_media_requirements(invoker, candidate, &required))
    else {
        return;
    };
    decision.insert(
        "provider".into(),
        JsValue::String(replacement.provider.clone()),
    );
    decision.insert("model".into(), JsValue::String(replacement.model.clone()));
    decision.insert("resolvedFrom".into(), "fallback".into());
    let mut mutation = JsObject::new();
    mutation.insert(
        "required".into(),
        JsValue::Array(
            required
                .iter()
                .map(|modality| JsValue::String(modality.clone()))
                .collect(),
        ),
    );
    mutation.insert("from".into(), JsValue::String(selected.id));
    mutation.insert("to".into(), JsValue::String(replacement.id));
    mutations.insert("mediaCapabilityRerouted".into(), JsValue::Object(mutation));
}

pub fn collect_required_input_modalities(messages: &[JsValue]) -> Vec<JsString> {
    let mut image = false;
    let mut pdf = false;
    let mut audio = false;
    for message in messages {
        let JsValue::Array(blocks) = field(message, "content") else {
            continue;
        };
        for block in blocks {
            match field(block, "type").as_string().map(utf16_lossy).as_deref() {
                Some("image") => image = true,
                Some("pdf") => pdf = true,
                Some("audio") => audio = true,
                Some("tool_result") => {
                    let JsValue::Array(items) = field(block, "content") else {
                        continue;
                    };
                    for item in items {
                        match field(item, "type").as_string().map(utf16_lossy).as_deref() {
                            Some("image") => image = true,
                            Some("pdf") => pdf = true,
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
    }
    let mut required = Vec::new();
    if image {
        required.push(JsString::from("image"));
    }
    if pdf {
        required.push(JsString::from("pdf"));
    }
    if audio {
        required.push(JsString::from("audio"));
    }
    required
}

pub(super) fn missing_input_modalities(
    invoker: &dyn RouterModelInvocation,
    reference: &RouterModelRef,
    required: &[JsString],
) -> Vec<JsString> {
    if required.is_empty() {
        return Vec::new();
    }
    let Ok(constraints) = invoker.get_multimodal(&reference.provider, &reference.model) else {
        return required.to_vec();
    };
    let supported = match field(&constraints, "input") {
        JsValue::Array(items) => items
            .iter()
            .filter_map(JsValue::as_string)
            .cloned()
            .collect::<Vec<_>>(),
        _ => Vec::new(),
    };
    required
        .iter()
        .filter(|modality| !supported.iter().any(|value| value == *modality))
        .cloned()
        .collect()
}

pub(super) fn supports_media_requirements(
    invoker: &dyn RouterModelInvocation,
    reference: &RouterModelRef,
    required: &[JsString],
) -> bool {
    missing_input_modalities(invoker, reference, required).is_empty()
}
