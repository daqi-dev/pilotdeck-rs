//! The capability list `describe_server` advertises, in one table.
//!
//! TS baseline: `resources/PilotDeck/src/gateway/client/InProcessGateway.ts:2088-2163`
//! builds the array from `this.options.<callback>` presence, and `hello_ok`
//! forwards that same `describeServer()` (`server/GatewayWsConnection.ts:112-115`).
//! The recorded output of the baseline for the three fixture config states is
//! `tests/fixtures/native-management/oracle.json` (`contract.capabilities`,
//! `contract.emptyStorage.capabilities`, `contract.missingStorage.capabilities`
//! — the same 50 names in all three).
//!
//! One entry per advertised name; `methods` is the wire set the name promises
//! and `wired` is the provider probe. A name is advertised only when **every**
//! method it promises is in the #1305 registry (`methods::registered_methods`)
//! **and** its provider is bound in this process. Dropping a method from the
//! registry therefore drops the advertisement instead of promising a method
//! that answers `CAPABILITY_UNAVAILABLE`.
//!
//! Names TS records but this process deliberately does not advertise live in
//! [`NOT_ADVERTISED`], one line of reason each;
//! `capability_table_covers_every_recorded_name` pins advertised == recorded −
//! those exceptions, so silently dropping or inventing a name fails.

use super::methods::registered_methods;
use super::requests::Requests;

/// One advertised capability.
pub(super) struct Capability {
    /// The name that appears in `capabilities`.
    pub(super) name: &'static str,
    /// The wire methods the name promises. Empty means "the method whose name
    /// is the capability name", which is the shape of all but three entries;
    /// the exceptions spell their methods out.
    pub(super) methods: &'static [&'static str],
    /// The provider probe. `false` means the delegate is not bound in this
    /// process, so advertising the name would promise a call that can only
    /// answer `CAPABILITY_UNAVAILABLE`.
    pub(super) wired: fn(&Requests) -> bool,
}

/// A provider with no configuration axis in this process: the delegate is
/// compiled in and always bound (e.g. `upload_gateway`, `mcp`, the registry).
fn compiled_in(_: &Requests) -> bool {
    true
}

/// `options.runRegistry`: reserved runs until the registry is open.
fn runs(requests: &Requests) -> bool {
    requests.runs.is_some()
}

/// `options.snapshotList/snapshotGet/snapshotRestore`: enabled only with an
/// explicit `--storage-root` authority.
fn snapshots(requests: &Requests) -> bool {
    requests.snapshot_enabled
}

/// `options.memoryList && options.memoryWipe`.
fn memory(requests: &Requests) -> bool {
    requests.memory.is_enabled()
}

/// `options.cron`: the six control names need the configured cron runtime.
fn cron(requests: &Requests) -> bool {
    requests.cron.is_some()
}

/// Advertised names, in the order the TS baseline records them.
// One row per name: the table is read as a table (and diffed against
// `oracle.json` row by row), so it is laid out by hand rather than exploded
// into five lines per entry by rustfmt.
#[rustfmt::skip]
pub(super) const CAPABILITIES: &[Capability] = &[
    Capability { name: "upload_create", methods: &[], wired: compiled_in },
    Capability { name: "upload_get", methods: &[], wired: compiled_in },
    Capability { name: "upload_part", methods: &[], wired: compiled_in },
    Capability { name: "upload_complete", methods: &[], wired: compiled_in },
    Capability { name: "upload_cancel", methods: &[], wired: compiled_in },
    Capability { name: "run_get", methods: &[], wired: runs },
    Capability { name: "run_events", methods: &[], wired: runs },
    Capability { name: "run_reattach", methods: &[], wired: runs },
    Capability { name: "project_files_list", methods: &[], wired: compiled_in },
    Capability { name: "commands_list", methods: &[], wired: compiled_in },
    Capability { name: "model_catalog_list", methods: &[], wired: compiled_in },
    Capability { name: "session_model_get", methods: &[], wired: compiled_in },
    Capability { name: "session_model_set", methods: &[], wired: compiled_in },
    Capability { name: "session_model_clear", methods: &[], wired: compiled_in },
    Capability { name: "delete_session", methods: &[], wired: compiled_in },
    // The name covers two requests: the manifest and its entries. TS records
    // this name in every config state (`oracle.json` contract capabilities);
    // both methods are routed by `dispatch::route` from `LOCAL_METHODS`.
    Capability {
        name: "native_transcript_archive",
        methods: &["native_archive_manifest", "native_archive_entries"],
        wired: compiled_in,
    },
    // The artifact name is separate in TS for the same archive: it streams a
    // blob rather than a JSON projection.
    Capability {
        name: "native_transcript_archive_artifact",
        methods: &["native_archive_artifact"],
        wired: compiled_in,
    },
    Capability { name: "memory_list", methods: &[], wired: memory },
    Capability { name: "memory_wipe", methods: &[], wired: memory },
    Capability { name: "manager_sessions", methods: &[], wired: compiled_in },
    Capability { name: "snapshot_list", methods: &[], wired: snapshots },
    Capability { name: "snapshot_get", methods: &[], wired: snapshots },
    Capability { name: "snapshot_restore", methods: &[], wired: snapshots },
    Capability { name: "mcp_server_status", methods: &[], wired: compiled_in },
    Capability { name: "set_mcp_servers", methods: &[], wired: compiled_in },
    Capability { name: "mcp_server_reconnect", methods: &[], wired: compiled_in },
    Capability { name: "mcp_server_toggle", methods: &[], wired: compiled_in },
    Capability { name: "set_mcp_permission_mode_override", methods: &[], wired: compiled_in },
    Capability { name: "project_file_read", methods: &[], wired: compiled_in },
    Capability { name: "set_permission_mode", methods: &[], wired: compiled_in },
    Capability { name: "resolve_settings", methods: &[], wired: compiled_in },
    Capability { name: "set_session_thinking", methods: &[], wired: compiled_in },
    Capability { name: "output_styles_list", methods: &[], wired: compiled_in },
    Capability { name: "set_output_style", methods: &[], wired: compiled_in },
    Capability { name: "usage_snapshot", methods: &[], wired: compiled_in },
    Capability { name: "model_usage_snapshot", methods: &[], wired: compiled_in },
    // The capability name and the wire method differ on purpose: TS advertises
    // `async_hook_result` (`protocol/types.ts`) for the `hook_async_result`
    // request (`protocol/frames.ts:85`).
    Capability {
        name: "async_hook_result",
        methods: &["hook_async_result"],
        wired: compiled_in,
    },
    Capability { name: "user_dialog_list", methods: &[], wired: compiled_in },
    Capability { name: "permission_list", methods: &[], wired: compiled_in },
    Capability { name: "permission_decide", methods: &[], wired: compiled_in },
    Capability { name: "rewind_files", methods: &[], wired: compiled_in },
    // Unix-only in the tools family; the registry check removes them on
    // Windows without a second `#[cfg]` list here.
    Capability { name: "background_task_stop", methods: &[], wired: compiled_in },
    Capability { name: "background_tasks", methods: &[], wired: compiled_in },
    Capability { name: "seed_read_state", methods: &[], wired: compiled_in },
    Capability { name: "supported_agents", methods: &[], wired: compiled_in },
    // TS splices these six right after the snapshot triplet, gated on
    // `options.cron` (`InProcessGateway.ts:2114-2123`); the fixture does not
    // configure cron, which is why they are the only table names the recorded
    // state does not contain. `CRON_METHODS` owns both sides (#1325).
    Capability { name: "cron_create", methods: &[], wired: cron },
    Capability { name: "cron_list", methods: &[], wired: cron },
    Capability { name: "cron_update", methods: &[], wired: cron },
    Capability { name: "cron_delete", methods: &[], wired: cron },
    Capability { name: "cron_stop", methods: &[], wired: cron },
    Capability { name: "cron_run_now", methods: &[], wired: cron },
];

/// TS records these names but this process must not advertise them: the
/// delegate behind them is not bound, so the call would answer
/// `CAPABILITY_UNAVAILABLE` and the advertisement would be a lie.
///
/// `(name, reason)`; each reason names the owner of the missing delegate.
// read by the capability tests only; kept in the shipping build so the
// exception list sits next to the table it excuses
#[allow(dead_code)]
pub(super) const NOT_ADVERTISED: &[(&str, &str)] = &[
    (
        "session_transcript_archive",
        "export/restore_session_transcript are not ported (methods/history.rs answers CAPABILITY_UNAVAILABLE)",
    ),
    (
        "apply_flag_settings",
        "settings delegate not bound in this process; porting tracked by #1671",
    ),
    (
        "update_settings",
        "settings delegate not bound in this process; porting tracked by #1671",
    ),
    (
        "reload_output_styles",
        "settings delegate not bound in this process; porting tracked by #1671",
    ),
    (
        "sdk_session_config",
        "no `set_sdk_session_config` request exists in the wire protocol and no SDK session registry here; the #1305 registry (native_process/methods/mod.rs) routes nothing by that name",
    ),
];

/// Capability names outside this draft. The recorded fixture never configures
/// the delegate behind them, so they are neither advertised nor an exception
/// to the recorded set; they are listed so the boundary is readable in the
/// tree and so adding one without deleting its line fails a test.
#[allow(dead_code)] // read by the capability tests only
pub(super) const OUT_OF_SCOPE: &[(&str, &str)] = &[
    ("always_on_apply", "not this issue: rescoped by #1313"),
    ("always_on_abort", "not this issue: rescoped by #1313"),
    ("always_on_rerun_plan", "not this issue: rescoped by #1313"),
    (
        "manager_browsers",
        "not this issue: the TS CLI only passes the callback through (cli/createLocalGateway.ts:1270) and nothing in the baseline tree calls it, so the baseline never advertises it either",
    ),
    (
        "sdk_session_defaults",
        "not this issue: the wire protocol declares only `sdk_session_config` (protocol/types.ts), so there is no request to serve",
    ),
];

/// The wire methods a capability promises.
fn promised(capability: &Capability) -> impl Iterator<Item = &'static str> {
    let explicit = capability.methods.iter().copied();
    let from_name = std::iter::once(capability.name).filter(move |_| capability.methods.is_empty());
    explicit.chain(from_name)
}

/// Does this process route every method the capability promises?
///
/// Split out from [`advertised`] so a test can drive it with a registry that
/// is missing one method — the "advertised but not wired" failure — without
/// having to unregister anything for real.
fn methods_registered(capability: &Capability, registered: &[&str]) -> bool {
    promised(capability).all(|method| registered.contains(&method))
}

/// Names advertised for this process, in table order.
pub(super) fn advertised(requests: &Requests) -> Vec<&'static str> {
    let registered: Vec<&'static str> = registered_methods().collect();
    CAPABILITIES
        .iter()
        .filter(|capability| (capability.wired)(requests))
        .filter(|capability| methods_registered(capability, &registered))
        .map(|capability| capability.name)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::negative_assert;

    const ORACLE: &str = include_str!("../../tests/fixtures/native-management/oracle.json");

    /// The names the TS baseline recorded in `contract.capabilities`. Held in
    /// a `OnceLock` so they can be compared as `&'static str`.
    fn recorded_names() -> Vec<&'static str> {
        static PARSED: std::sync::OnceLock<serde_json::Value> = std::sync::OnceLock::new();
        PARSED
            .get_or_init(|| serde_json::from_str(ORACLE).expect("#1352 oracle parses"))["contract"]
            ["capabilities"]
            .as_array()
            .expect("#1352 oracle capabilities")
            .iter()
            .map(|value| value.as_str().expect("#1352 oracle capability string"))
            .collect()
    }

    fn table_names() -> Vec<&'static str> {
        CAPABILITIES
            .iter()
            .map(|capability| capability.name)
            .collect()
    }

    /// A name advertised twice would make every set comparison below vacuous
    /// on one of the copies.
    #[test]
    fn capability_table_has_no_duplicate_names() {
        let mut names = table_names();
        names.sort_unstable();
        let total = names.len();
        names.dedup();
        negative_assert!(names.len() == total, "#1352 capability names are unique");
    }

    /// The fail-closed half of #1352 rule 1: an advertised name must promise
    /// only methods the #1305 registry routes. A method that is missing would
    /// otherwise be promised by `describe_server` and then answer
    /// `CAPABILITY_UNAVAILABLE`.
    #[test]
    fn capability_methods_are_registered() {
        let registered: Vec<&str> = registered_methods().collect();
        for capability in CAPABILITIES {
            negative_assert!(
                methods_registered(capability, &registered),
                "#1352 {} promises a method the registry does not route: {:?}",
                capability.name,
                promised(capability).collect::<Vec<_>>()
            );
        }
    }

    /// The other half of the same rule, for the methods that *are* registered
    /// but not served: `native_process/methods/history.rs:50-58` answers
    /// `CAPABILITY_UNAVAILABLE` for both transcript-archive methods, which is
    /// why `session_transcript_archive` sits in [`NOT_ADVERTISED`]. The
    /// registry check alone cannot see that, so without this test a row
    /// promising `export_session_transcript` would advertise a call that can
    /// only fail.
    #[test]
    fn unserved_history_methods_are_not_promised() {
        for method in ["export_session_transcript", "restore_session_transcript"] {
            negative_assert!(
                !CAPABILITIES
                    .iter()
                    .any(|capability| promised(capability).any(|declared| declared == method)),
                "#1352 {method} answers CAPABILITY_UNAVAILABLE and must not be promised"
            );
        }
    }

    /// The same property driven from the other side: drop one promised method
    /// from the registry and the capability must stop being advertised. This
    /// is the assertion a "declared but not wired" regression has to trip, and
    /// it does not depend on the live registry holding the right values.
    #[test]
    fn unregistered_method_suppresses_advertisement() {
        for capability in CAPABILITIES {
            let mut partial: Vec<&str> = promised(capability).collect();
            partial.pop().expect("#1352 capability promises a method");
            negative_assert!(
                !methods_registered(capability, &partial),
                "#1352 {} stays advertised with a promised method missing from the registry",
                capability.name
            );
            let full: Vec<&str> = promised(capability).collect();
            negative_assert!(
                methods_registered(capability, &full),
                "#1352 {} is suppressed even with every promised method registered",
                capability.name
            );
        }
    }

    /// The acceptance-narrowing invariant: advertised == recorded − declared
    /// exceptions. Every recorded name either is advertised or carries a
    /// reason, and no advertised name is invented — the only table entries the
    /// recorded state does not contain are the six cron names, which TS gates
    /// on `options.cron` (a delegate the fixture does not configure) and which
    /// `CRON_METHODS` owns for both sides.
    #[test]
    fn capability_table_covers_every_recorded_name() {
        let recorded = recorded_names();
        let table = table_names();
        for name in &recorded {
            let declared = NOT_ADVERTISED
                .iter()
                .any(|(exception, _)| exception == name);
            negative_assert!(
                table.contains(name) || declared,
                "#1352 recorded TS name {name} is neither advertised nor declared in NOT_ADVERTISED"
            );
        }
        for name in &table {
            negative_assert!(
                recorded.contains(name) || crate::runtime::cron::CRON_METHODS.contains(name),
                "#1352 {name} is advertised but the TS baseline does not record it and it is not a cron name"
            );
        }
        for name in crate::runtime::cron::CRON_METHODS {
            negative_assert!(
                !recorded.contains(&name),
                "#1352 cron name {name} is recorded by the fixture; the cron gate assumption is stale"
            );
        }
    }

    /// #1352 rule 2: no exception without a reason. A bare name would put a
    /// "why" that only exists in a review thread back into the tree, and a
    /// name the baseline never records would silently widen the exception
    /// surface.
    #[test]
    fn exception_entries_carry_a_reason() {
        let recorded = recorded_names();
        for (name, reason) in NOT_ADVERTISED {
            negative_assert!(!reason.trim().is_empty(), "#1352 {name} has no reason");
            negative_assert!(
                reason.contains('#') || reason.contains(".rs"),
                "#1352 exception {name} names no tracker or source file: {reason}"
            );
            negative_assert!(
                recorded.contains(name),
                "#1352 exception {name} is not recorded by the TS baseline"
            );
            negative_assert!(
                !table_names().contains(name),
                "#1352 {name} is both advertised and declared as an exception"
            );
        }
    }

    /// The declared out-of-scope names stay out: none of them may creep into
    /// the advertised table without deleting its line here.
    #[test]
    fn out_of_scope_names_are_not_advertised() {
        let recorded = recorded_names();
        for (name, reason) in OUT_OF_SCOPE {
            negative_assert!(!reason.trim().is_empty(), "#1352 {name} has no reason");
            negative_assert!(
                !table_names().contains(name),
                "#1352 out-of-scope name {name} is advertised"
            );
            negative_assert!(
                !recorded.contains(name),
                "#1352 out-of-scope name {name} is recorded by the fixture; move it to NOT_ADVERTISED"
            );
        }
    }
}
