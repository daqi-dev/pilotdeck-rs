//! Host-owned context runtime. `prepare` owns prompt assembly, PILOTDECK.md
//! instruction discovery and message projection; memory attachments land in
//! A6.6-pr5b, tool-result budgeting in A6.6-pr6 and compaction in A6.6-pr7+.
mod instructions;
mod prepare;
mod prompt;

pub use instructions::{
    InstructionDiscovery, Layer as InstructionLayer, Scope as InstructionScope,
};
pub use prepare::{DefaultContextRuntime, Options, RuntimeContextSurface};
pub use prompt::{
    AssembledRuntimeContext, AssembledSection, ContributedCommand, ContributedPrompt,
    ContributedSkill, ContributionContext, ContributionRegistration, DynamicTextProvider,
    ExtensionResolver, McpServerInstruction, NullExtensionResolver, PromptAssembler,
    PromptContributionRegistry, PromptContributionSnapshot, PromptResult, PromptSections,
    ProviderFuture, RuntimeContextContribution, SectionContribution, SnapshotOptions, TextProvider,
    VariableProvider, render_prompt_contribution_sections, render_prompt_runtime_context_sections,
};
