//! Shared Node runtime pin for real-TS fixture regeneration (#953).
//!
//! `common/baseline.rs` cannot be extended while #988 owns it, so this module
//! only adds the missing version gate. It deliberately reuses the caller's
//! `Command` (normally `baseline::node_command()`) for the `--version` probe,
//! preserving the one shared LANG/LC_* environment contract.

use std::process::Command;

pub(crate) const NODE_VERSION: &str = "v22.22.0";

/// Require the host Node that produced the fixtures and return `command`.
///
/// Every migration point calls this before spawning a generator; a host with
/// any other Node fails before the oracle is written or compared.
pub(crate) fn pin(command: Command) -> Command {
    let mut probe = Command::new(command.get_program());
    for (key, value) in command.get_envs() {
        match value {
            Some(value) => {
                probe.env(key, value);
            }
            None => {
                probe.env_remove(key);
            }
        }
    }
    let output = probe
        .arg("--version")
        .output()
        .expect("#953 run node --version");
    assert!(
        output.status.success(),
        "#953 node --version failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        NODE_VERSION,
        "#953 pinned Node runtime (see src/PROVENANCE.md)"
    );
    command
}
