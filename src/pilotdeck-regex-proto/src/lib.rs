//! #1265 prototype: a bounded JS RegExp engine on rquickjs (QuickJS-NG 0.16.2).
//!
//! Human decision #1264 选 C (2026-11-01): prototype a restricted rquickjs
//! RegExp wrapper and track regress #213; production outputSchema integration
//! stays out of scope for this crate.
//!
//! The engine is compiled only behind the non-default `regex-proto` feature,
//! so the product workspace builds, tests and clippy runs without resolving,
//! downloading or compiling rquickjs at all. The accepted input surface is
//! exactly: a pattern as UTF-16 code units, a flag string and a subject as
//! UTF-16 code units. Arbitrary JS, host I/O and module loading are not
//! exposed (see `engine` for the build-level evidence).

#[cfg(feature = "regex-proto")]
pub mod engine;

#[cfg(feature = "regex-proto")]
pub use engine::{
    CompileError, CompiledPattern, EngineLimits, ExecOutcome, MatchBudget, MatchError, MatchResult,
    RegexEngine,
};

/// Why this crate compiles no engine without the `regex-proto` feature.
///
/// The default workspace build must stay independent of rquickjs; tests that
/// assert on this string pin the feature-off state of the crate.
#[cfg(not(feature = "regex-proto"))]
pub const fn engine_disabled_reason() -> &'static str {
    "pilotdeck-regex-proto compiles no engine without the `regex-proto` feature"
}

#[cfg(all(not(feature = "regex-proto"), test))]
mod disabled_tests {
    #[test]
    fn feature_off_crate_compiles_without_rquickjs() {
        assert_eq!(
            super::engine_disabled_reason(),
            "pilotdeck-regex-proto compiles no engine without the `regex-proto` feature"
        );
    }
}
