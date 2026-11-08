//! Durable transcript to Gateway history projection. This module owns no writer.
mod entries;
mod page;
mod project;
mod reader;
pub use entries::messages;
pub use page::page;
pub use project::{Context, ProjectionError, flatten_message};
pub use reader::HistoryFiles;
