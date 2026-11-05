//! #1525: a permission-fault contract that could not be exercised must not be
//! reported as a passing test (#1263's three-state leak).
//!
//! Several tests assert behavior that only shows up when the filesystem really
//! refuses a write (mode bits / `EACCES`). On a privileged host (root,
//! `CAP_DAC_OVERRIDE`) the probe succeeds, so the test cannot run and used to
//! `return` — libtest still printed `ok`, and every consumer that looks at the
//! exit code treated the contract as exercised.
//!
//! `PDRS_REQUIRE_PERMISSION_FAULTS=1` (or `true`) flips that skip into a panic:
//! CI and the formal host recipes export the switch, local runs leave it unset
//! and keep the recognizable skip line. Shared by
//! edgeclaw_heartbeat_indexer.rs and write_tools.rs — do not copy the parser.

pub fn require_permission_faults() -> bool {
    matches!(
        std::env::var("PDRS_REQUIRE_PERMISSION_FAULTS").as_deref(),
        Ok("1" | "true")
    )
}
