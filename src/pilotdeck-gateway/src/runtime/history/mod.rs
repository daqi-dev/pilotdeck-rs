//! Durable transcript to Gateway history projection. This module owns no writer.
mod budget;
mod entries;
mod envelope;
mod events;
mod page;
mod portable;
mod project;
mod reader;
mod session;
mod subagent;
mod subagent_reader;
mod usage;
pub use entries::messages;
pub use envelope::{Options as ProjectionOptions, project as project_history};
pub use page::page;
pub(crate) use portable::{export_guarded, restore_guarded};
pub use project::{Context, ProjectionError, flatten_message};
pub(crate) use reader::HistoryMarkerFiles;
pub use reader::{HistoryFiles, NativeHistoryReader};
pub use session::fallback;
pub use usage::token_usage;

pub use subagent_reader::read as read_subagent_messages;
