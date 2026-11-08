//! Durable transcript to Gateway history projection. This module owns no writer.
mod budget;
mod entries;
mod envelope;
mod events;
mod page;
mod project;
mod reader;
mod session;
mod subagent;
mod usage;
pub use entries::messages;
pub use envelope::{Options as ProjectionOptions, project as project_history};
pub use page::page;
pub use project::{Context, ProjectionError, flatten_message};
pub use reader::{HistoryFiles, NativeHistoryReader};
pub use session::fallback;
pub use usage::token_usage;
