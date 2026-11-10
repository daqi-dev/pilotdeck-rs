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
#[derive(Clone)]
pub(super) struct NativeSkills(Arc<[NativeSkill]>, bool);
impl Default for NativeSkills {
    fn default() -> Self {
        Self(Arc::default(), true)
    }
}
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
            true,
        )
    }
    pub(super) fn disabled() -> Self {
        Self(Arc::default(), false)
    }
    pub(super) fn select(
        &self,
        requested: Option<&JsValue>,
    ) -> Result<Self, crate::runtime::project::RuntimeError> {
        let Some(JsValue::Array(names)) = requested else {
            return Ok(self.clone());
        };
        let mut selected = Vec::new();
        for name in names {
            let name = name.as_string().ok_or_else(|| {
                crate::runtime::project::RuntimeError::new(
                    "Native skill selection must contain strings.",
                )
            })?;
            let skill = self
                .0
                .iter()
                .find(|skill| &skill.name == name)
                .ok_or_else(|| {
                    crate::runtime::project::RuntimeError::new(
                        "Native child skill is outside the retained parent catalog.",
                    )
                })?;
            if !selected
                .iter()
                .any(|old: &NativeSkill| old.name == skill.name)
            {
                selected.push(skill.clone());
            }
        }
        Ok(Self(selected.into(), self.1))
    }
    pub(super) fn register(&self, tools: &ToolRegistry) {
        if !self.1 {
            return;
        }
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
            ("description", JsValue::from("Load a skill recipe by name and return its resolved SKILL.md path with the full content. Use this when the system prompt lists an available skill relevant to the current task.")),
            ("kind", JsValue::from("session")),
            ("inputSchema", pilotdeck_protocol::parse_json(r###"{"type":"object","required":["skillName"],"additionalProperties":false,"properties":{"skillName":{"type":"string","description":"The skill name as listed in <available-skills>."}}}"###).expect("static skill schema")),
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

#[cfg(test)]
#[path = "skills_packaged_tests.rs"]
mod packaged_tests;

#[cfg(test)]
mod disabled_tests {
    use super::*;
    #[test]
    fn selected_catalog_retains_bytes_and_cannot_expand_the_parent() {
        let catalog = NativeSkills(
            vec![
                NativeSkill {
                    name: "p:a".into(),
                    description: None,
                    path: "/fixture/a/SKILL.md".into(),
                    content: "SELECTED A".into(),
                },
                NativeSkill {
                    name: "p:b".into(),
                    description: None,
                    path: "/fixture/b/SKILL.md".into(),
                    content: "SELECTED B".into(),
                },
            ]
            .into(),
            true,
        );
        let selected = catalog
            .select(Some(&JsValue::Array(vec!["p:a".into()])))
            .unwrap();
        assert_eq!(selected.list_skills().len(), 1);
        assert_eq!(selected.0[0].content, "SELECTED A".into());
        assert!(
            selected
                .select(Some(&JsValue::Array(vec!["p:b".into()])))
                .is_err()
        );
        assert!(
            NativeSkills::disabled()
                .select(Some(&JsValue::Array(vec![])))
                .unwrap()
                .list_skills()
                .is_empty()
        );
    }
    #[test]
    fn disabled_skills_remove_both_tool_and_prompt_contributions() {
        let disabled = NativeSkills::disabled();
        let registry = ToolRegistry::new(None, None);
        disabled.register(&registry);
        assert!(registry.list().unwrap().is_empty());
        assert!(disabled.list_skills().is_empty());
        let enabled = NativeSkills::default();
        enabled.register(&registry);
        assert!(
            registry
                .list()
                .unwrap()
                .iter()
                .any(|tool| tool.fields().get("name") == Some(&"read_skill".into()))
        );
    }
    #[test]
    fn configured_skill_contributions_are_readable_and_registered() {
        let catalog = NativeSkills::from_contributions(&[PluginSkillContribution {
            name: "fixture:example".into(),
            description: Some("Configured fixture".into()),
            path: std::path::PathBuf::from("/fixture/example/SKILL.md"),
            content: "ORIGINAL-CONTRIBUTION\n".into(),
            namespace: "fixture".into(),
        }]);
        let registry = ToolRegistry::new(None, None);
        catalog.register(&registry);
        let output = catalog.read(&"fixture:example".into());
        let content = output
            .content
            .iter()
            .filter_map(|block| block.get("text").and_then(JsValue::as_string))
            .map(|value| String::from_utf16_lossy(value.units()))
            .collect::<Vec<_>>()
            .join("\n");
        crate::negative_assert!(
            registry.has(&"read_skill".into())
                && catalog.list_skills().len() == 1
                && content.contains("ORIGINAL-CONTRIBUTION\n"),
            "#1071 configured skill contributions reach the native tool"
        );
    }
}

#[cfg(test)]
#[path = "skills_windows_tests.rs"]
mod windows_builtin_tests;
