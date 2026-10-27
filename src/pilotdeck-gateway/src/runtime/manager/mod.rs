//! Manager session/browser projection for U4 (`createLocalGateway.ts:1259-1270`).
//!
//! This component is deliberately **not** registered in the native dispatch
//! table: `dispatch.rs` keeps `manager_sessions` / `manager_browsers` owned by
//! the TS host until A8 takes the WS entry. The projection consumes the same
//! two seams the real local Gateway does:
//!
//! - sessions: `ProjectRuntimeRegistry.listSessions` -> the injected
//!   `SessionCatalogPort`, called as `{ projectRoot, pilotHome, limit: 100,
//!   offset: 0 }`;
//! - browsers: the host-injected manager browser provider, whose successful
//!   result is returned unchanged.
//!
//! `TranscriptSessionCatalog` is the native JSONL bridge to the existing
//! read-only `AsyncTranscriptAdapter::list_sessions`; it never writes and does
//! not change that shared adapter. A store that does not advertise `list`
//! surfaces the TS `ProjectSessionCatalogUnavailableError` instead of falling
//! back to a guessed empty list.
//!
//! The browser success path is provider-driven, not a permanent
//! `CAPABILITY_UNAVAILABLE`: an absent provider produces the TS gateway-method
//! error, while an injected provider owns both values and errors.

mod catalog;
mod projection;
#[cfg(test)]
mod tests;

pub use catalog::{
    GatewaySessionCatalogConsumer, SessionCatalogListInput, SessionCatalogPort,
    TranscriptSessionCatalog,
};
pub use projection::{
    ManagerBrowserPort, ManagerBrowsers, ManagerDispatch, ManagerError, ManagerGateway,
    ManagerProjectResolver, ManagerProjection, ManagerProjects, ManagerSessions,
};

/// Wire method names owned by this component. They are intentionally absent
/// from `dispatch.rs::LOCAL_METHODS` in this package.
pub const MANAGER_METHODS: &[&str] = &["manager_sessions", "manager_browsers"];
