//! Prompt-facing contributions retained from the session plugin lease.
use super::skills::NativeSkills;
use crate::runtime::{
    context::{ContributedCommand, ContributedSkill, ExtensionResolver, McpServerInstruction},
    plugins::{PluginCommandContribution, PluginMcpInstructionEntry},
};

pub(super) struct NativeExtensions {
    skills: NativeSkills,
    commands: Vec<ContributedCommand>,
    mcp_instructions: Vec<McpServerInstruction>,
}
impl NativeExtensions {
    pub(super) fn new(
        skills: NativeSkills,
        commands: &[PluginCommandContribution],
        mcp_instructions: &[PluginMcpInstructionEntry],
    ) -> Self {
        Self {
            skills,
            commands: commands
                .iter()
                .map(|command| ContributedCommand {
                    name: command.name.clone(),
                    description: command.description.clone(),
                    argument_hint: command.argument_hint.clone(),
                })
                .collect(),
            mcp_instructions: mcp_instructions
                .iter()
                .map(|entry| McpServerInstruction {
                    server_name: entry.server_name.clone(),
                    instructions: Some(entry.instructions.clone()),
                })
                .collect(),
        }
    }
}
impl ExtensionResolver for NativeExtensions {
    fn list_commands(&self) -> Vec<ContributedCommand> {
        self.commands.clone()
    }
    fn list_skills(&self) -> Vec<ContributedSkill> {
        self.skills.list_skills()
    }
    fn list_mcp_instructions(&self) -> Vec<McpServerInstruction> {
        self.mcp_instructions.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pilotdeck_protocol::JsString;
    #[test]
    fn context_projection_freezes_commands_and_raw_mcp_units_with_the_selected_skills() {
        let mut commands = vec![PluginCommandContribution {
            name: "plugin:task".into(),
            description: Some("old description".into()),
            argument_hint: Some("<path>".into()),
            content: "old body".into(),
            namespace: "plugin".into(),
        }];
        let mut mcp = vec![PluginMcpInstructionEntry {
            server_name: JsString::from_utf16(vec![b's' as u16, 0xd800]),
            instructions: JsString::from_utf16(vec![b'i' as u16, 0xdfff]),
        }];
        let extension = NativeExtensions::new(NativeSkills::disabled(), &commands, &mcp);
        commands[0].description = Some("new description".into());
        mcp[0].instructions = "replacement".into();
        assert_eq!(
            extension.list_commands()[0].description.as_deref(),
            Some("old description")
        );
        assert_eq!(
            extension.list_commands()[0].argument_hint.as_deref(),
            Some("<path>")
        );
        assert!(extension.list_skills().is_empty());
        assert_eq!(
            extension.list_mcp_instructions()[0].server_name.units(),
            &[b's' as u16, 0xd800]
        );
        assert_eq!(
            extension.list_mcp_instructions()[0]
                .instructions
                .as_ref()
                .unwrap()
                .units(),
            &[b'i' as u16, 0xdfff]
        );
    }
}
