//! Host-owned context runtime. `prepare` owns prompt assembly, PILOTDECK.md
//! instruction discovery, memory attachments, cache plans and message projection;
//! `tool_budget` owns tool-result budgeting; `compaction` holds the
//! model-free snip / micro passes (A6.6-pr7).
pub mod cache;
pub mod compaction;
mod instructions;
pub mod memory;
mod prepare;
pub(crate) mod prompt;
pub mod tool_budget;

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
