//! Native standalone skill consumption; the retained plugin generation owns the bytes.
use crate::{
    host::{HostError, modules::capability::ToolDefinition},
    runtime::{
        context::{ContributedSkill, ExtensionResolver},
        plugins::PluginSkillContribution,
        tools::{ExecutableTool, ToolError, ToolFuture, ToolOutput, ToolRegistry, ToolRunContext},
    },
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::sync::Arc;

#[derive(Clone)]
pub(super) struct NativeSkill {
    name: JsString,
    description: Option<String>,
    path: JsString,
    content: JsString,
}
#[derive(Clone, Default)]
pub(super) struct NativeSkills(Arc<[NativeSkill]>);
impl NativeSkills {
    pub(super) fn from_contributions(skills: &[PluginSkillContribution]) -> Self {
        Self(
            skills
                .iter()
                .map(|skill| NativeSkill {
                    name: skill.name.as_str().into(),
                    description: skill.description.clone(),
                    path: skill.path.to_string_lossy().into_owned().into(),
                    content: skill.content.as_str().into(),
                })
                .collect(),
        )
    }
    pub(super) fn register(&self, tools: &ToolRegistry) {
        tools
            .register(Arc::new(ReadSkill::new(self.clone())))
            .expect("native read_skill name is unique");
    }
    fn read(&self, name: &JsString) -> ToolOutput {
        // Like loadSkillPrompt, exact names win before an unqualified suffix.
        let selected = self.0.iter().find(|skill| &skill.name == name).or_else(|| {
            let suffix = join(&[&JsString::from(":"), name]);
            self.0
                .iter()
                .find(|skill| skill.name.units().ends_with(suffix.units()))
        });
        let content = selected.map(|skill| render_skill_content(&skill.content, &skill.path));
        read_response(name, content.as_ref(), &self.0)
    }
}
impl ExtensionResolver for NativeSkills {
    fn list_skills(&self) -> Vec<ContributedSkill> {
        self.0
            .iter()
            .map(|skill| ContributedSkill {
                // Loader-produced catalog entries are valid UTF-8. The pure
                // response/render helpers below retain raw JS string code units.
                name: String::from_utf16_lossy(skill.name.units()),
                description: skill.description.clone(),
                path: String::from_utf16_lossy(skill.path.units()),
            })
            .collect()
    }
}
struct ReadSkill {
    fields: JsObject,
    skills: NativeSkills,
}
impl ReadSkill {
    fn new(skills: NativeSkills) -> Self {
        let mut fields = JsObject::new();
        for (key, value) in [
            ("name", JsValue::from("read_skill")),
            ("aliases", JsValue::Array(vec!["ReadSkill".into()])),
            ("description", JsValue::from("Read the selected skill recipe and its exact SKILL.md path. Use a name listed in available-skills; resolve its references against the returned skill directory.")),
            ("kind", JsValue::from("session")),
            ("inputSchema", pilotdeck_protocol::parse_json(r#"{"type":"object","required":["skillName"],"additionalProperties":false,"properties":{"skillName":{"type":"string","description":"The name shown in available-skills."}}}"#).expect("static skill schema")),
        ] { fields.insert(key.into(), value); }
        Self { fields, skills }
    }
}
impl ToolDefinition for ReadSkill {
    fn fields(&self) -> &JsObject {
        &self.fields
    }
    fn is_read_only(&self, _: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(true))
    }
    fn is_concurrency_safe(&self, _: &JsValue) -> Result<JsValue, HostError> {
        Ok(JsValue::Bool(true))
    }
}
impl ExecutableTool for ReadSkill {
    fn execute(&self, input: JsValue, context: ToolRunContext) -> ToolFuture<ToolOutput> {
        let skills = self.skills.clone();
        Box::pin(async move {
            if context.is_aborted() {
                return Err(ToolError::aborted());
            }
            let name = input
                .get("skillName")
                .and_then(JsValue::as_string)
                .ok_or_else(|| ToolError::new("invalid_input", "skillName must be a string."))?;
            Ok(skills.read(name))
        })
    }
}
fn join(parts: &[&JsString]) -> JsString {
    JsString::from_utf16(
        parts
            .iter()
            .flat_map(|part| part.units().iter().copied())
            .collect(),
    )
}
fn xml(value: &JsString) -> JsString {
    let mut units = Vec::new();
    for &unit in value.units() {
        let escaped = match unit {
            38 => Some("&amp;"),
            60 => Some("&lt;"),
            62 => Some("&gt;"),
            _ => None,
        };
        if let Some(text) = escaped {
            units.extend(text.encode_utf16());
        } else {
            units.push(unit);
        }
    }
    JsString::from_utf16(units)
}
fn read_response(
    name: &JsString,
    content: Option<&JsString>,
    skills: &[NativeSkill],
) -> ToolOutput {
    let text = if let Some(content) = content.filter(|content| !content.is_empty()) {
        if let Some(skill) = skills.iter().find(|skill| &skill.name == name) {
            join(&[
                &"<skill>\n<name>".into(),
                &xml(&skill.name),
                &"</name>\n<path>".into(),
                &xml(&skill.path),
                &"</path>\n".into(),
                content,
                &"\n</skill>".into(),
            ])
        } else {
            content.clone()
        }
    } else {
        let prefix = join(&[&"Skill '".into(), name, &"' not found. ".into()]);
        if skills.is_empty() {
            join(&[&prefix, &"No skills are currently loaded.".into()])
        } else {
            let mut names = Vec::new();
            for skill in skills {
                if !names.is_empty() {
                    names.extend(", ".encode_utf16());
                }
                names.extend_from_slice(skill.name.units());
            }
            join(&[
                &prefix,
                &"Available skills: ".into(),
                &JsString::from_utf16(names),
            ])
        }
    };
    let mut block = JsObject::new();
    block.insert("type".into(), "text".into());
    block.insert("text".into(), JsValue::String(text));
    ToolOutput {
        content: vec![JsValue::Object(block)],
        ..Default::default()
    }
}
fn render_skill_content(content: &JsString, path: &JsString) -> JsString {
    let root = crate::runtime::tools::builtin::dirname(path);
    let mut quoted = vec![39];
    for &unit in root.units() {
        if unit == 39 {
            quoted.extend("'\"'\"'".encode_utf16());
        } else {
            quoted.push(unit);
        }
    }
    quoted.push(39);
    let token: Vec<u16> = "{{SKILL_ROOT_SHELL}}".encode_utf16().collect();
    let escaped: Vec<u16> = "{{!SKILL_ROOT_SHELL}}".encode_utf16().collect();
    let mut output = Vec::new();
    let mut left = content.units();
    while !left.is_empty() {
        if left.starts_with(&escaped) {
            output.extend_from_slice(&token);
            left = &left[escaped.len()..];
        } else if left.starts_with(&token) {
            output.extend_from_slice(&quoted);
            left = &left[token.len()..];
        } else {
            output.push(left[0]);
            left = &left[1..];
        }
    }
    JsString::from_utf16(output)
}

#[cfg(test)]
#[path = "skills_tests.rs"]
mod tests;
