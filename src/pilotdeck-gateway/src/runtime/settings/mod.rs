//! U7 global settings write & reload gateway adapter (#1318). Mirrors the TS
//! gateway layers the five A8.1 methods cross: the
//! `GatewayWsConnection.ts` dispatch (`apply_flag_settings` /
//! `update_settings` / `reload_output_styles` / `reload_config` /
//! `reload_extensions`), then the `InProcessGateway.ts` wrapper validation
//! (field checks, the per-key `hasActiveTurn` SESSION_BUSY gate, the
//! capability seam, and `router.close(sessionKey)` after a mutating
//! `apply_flag_settings`), then the delegated bodies:
//!
//! * `applyFlagSettingsForSdk` / `reloadOutputStylesForSdk` are registry
//!   seams (the product registry binding is a wiring-layer concern);
//! * `createLocalGateway.ts`'s `updateSettings` closure is composed here
//!   over the real [`ConfigStore`]: organization policy check, the
//!   [`LocalSettingsWriter`] port, then the `sdk-update-settings` reload with
//!   the store event's `changedPaths` replacing the writer's (gap U7: the
//!   write, the in-memory store reload and the resulting broadcasts stay one
//!   chain — a YAML-only write is not a completed update);
//! * `reloadConfig` / `reloadExtensions` bind the existing
//!   [`RefreshBundle`](super::refresh::RefreshBundle).
//!
//! This is a component, not a wiring change: `dispatch.rs`, the native
//! process request table and the default routing are untouched; wiring
//! belongs to #1320. A caller assembling this component answers for the
//! capability gate exactly like the TS assembly: the gateway assembled
//! without an option answers `CAPABILITY_UNAVAILABLE` per method (or the
//! `{reloaded: false, reason: "unsupported"}` projection for the two
//! reloads), and any future wiring must check the capability before routing
//! to this adapter instead of mounting it unconditionally.
//!
//! ## JS semantics
//!
//! `input.sessionKey?.trim()` short-circuits only on a nullish key: a
//! non-string key throws the raw `input.sessionKey?.trim is not a function`
//! TypeError (wrapped as `gateway_request_failed` by the WS layer), reading
//! any property of a nullish params object throws the property-read
//! TypeError, `Object.entries` walks own keys in JS order (array indices
//! first, ascending), and `String(value)` conversions go through
//! [`crate::js_value`]. Wire-reachable divergences are documented on the
//! mapping helpers: a non-string `reload_extensions` `projectKey` uses the
//! coerced `String(value)` form (TS passes the raw value through to its
//! per-project branch), and non-string `changedPaths` entries likewise;
//! both cells are outside the golden's string-only contract.

mod gateway;
#[cfg(test)]
#[path = "gateway_tests.rs"]
mod tests;

pub use gateway::*;
