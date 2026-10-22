//! Node v22 libuv diagnostics. Unknown errno (including non-OS Rust errors) is
//! an explicit host boundary, never a Rust Display string on the wire.
mod generated;
mod mkdir;
pub(crate) use mkdir::mkdir_recursive;
mod path_error;
pub(crate) use path_error::{invalid_arg_value_message, nul_message, nul_message_for};
use std::io;

// Method admission is separate from the complete libuv formatting table.
// Each entry requires a source-oracle fixture for that method's fs boundaries.
// project-files/io-errors-round6.json; session-model/io-errors{,-round5}.json;
// config/catalog-io-errors.json (catalog RPC itself).
// TS cc7c4e6: src/gateway/server/GatewayWsConnection.ts:367-368 routes the RPC
// to src/gateway/dialog/modelCatalog.ts:17-18; its own loadPilotConfig call
// reaches src/pilot/config/loadPilotConfig.ts:214-239 (exists/read boundary).
const PROJECT_FILES: &[&str] = &["ENOENT", "EACCES", "ENOTDIR", "ENAMETOOLONG", "ELOOP"];
const SESSION_CONFIG: &[&str] = &[
    "ENOENT",
    "EACCES",
    "ENOTDIR",
    "EISDIR",
    "ENAMETOOLONG",
    "ELOOP",
];
// Native archives have their own twelve public RPC observations, including
// natural stat ELOOP and descriptor read EISDIR. Do not inherit config's gate.
const ARCHIVE_TRANSCRIPT: &[&str] = &[
    "ENOENT",
    "EACCES",
    "ENOTDIR",
    "EISDIR",
    "ENAMETOOLONG",
    "ELOOP",
];
// skills/oracle.json, per SkillManager method (GatewayWsConnection.ts:542-557);
// skills::tests checks each set equals the errno codes that method recorded.
const SKILL_READ: &[&str] = &["ENOTDIR", "EISDIR"];
const SKILL_WRITE: &[&str] = &["ENOTDIR", "EEXIST", "EISDIR"];
const SKILL_PATH: &[&str] = &["ENOTDIR"];
const SKILL_IMPORT: &[&str] = &["ENOTDIR", "EEXIST"];
// snapshot-projection/oracle.json, per projection method (createLocalGateway.ts:
// 1271-1313). The real TS gateway recorded ENOENT only for snapshotRestore
// (mkdtemp target and object-file open); list/get surfaced no fs errno in the
// corpus, so their sets stay empty like skill_validate.
const SNAPSHOT_TRANSCRIPT: &[&str] = &["ENOENT"];
pub(crate) const LOCAL_ERRNOS: &[(&str, &[&str])] = &[
    ("project_files_list", PROJECT_FILES),
    ("project_file_read", PROJECT_FILES),
    ("session_model_get", SESSION_CONFIG),
    ("model_catalog_list", SESSION_CONFIG),
    ("native_archive_manifest", ARCHIVE_TRANSCRIPT),
    ("native_archive_entries", ARCHIVE_TRANSCRIPT),
    ("skill_list", SKILL_PATH),
    ("skill_read", SKILL_READ),
    ("skill_write", SKILL_WRITE),
    ("skill_create", SKILL_PATH),
    ("skill_delete", SKILL_PATH),
    ("skill_import", SKILL_IMPORT),
    ("skill_validate", &[]),
    ("skill_scan", SKILL_PATH),
    ("snapshot_list", &[]),
    ("snapshot_get", &[]),
    ("snapshot_restore", SNAPSHOT_TRANSCRIPT),
];
pub(crate) fn lookup_for(method: &str, error: &io::Error) -> Option<(&'static str, &'static str)> {
    let found = lookup(error)?;
    LOCAL_ERRNOS
        .iter()
        .find(|(name, _)| *name == method)?
        .1
        .contains(&found.0)
        .then_some(found)
}
/// Code-based admission for a caller that already holds the measured errno code
/// (`WorkspaceSnapshotError::Io`). `lookup_for` remains the entry point when the
/// original `io::Error` is available.
pub(crate) fn admits(method: &str, code: &str) -> bool {
    LOCAL_ERRNOS
        .iter()
        .find(|(name, _)| *name == method)
        .is_some_and(|(_, allowed)| allowed.contains(&code))
}
// Both config consumers have the same measured errno set; changing one requires
// splitting this gate rather than silently inheriting the global format table.
pub(crate) fn config_lookup(error: &io::Error) -> Option<(&'static str, &'static str)> {
    let found = lookup_for("session_model_get", error)?;
    lookup_for("model_catalog_list", error)?;
    Some(found)
}
pub(crate) fn config_read_file(error: &io::Error, path: &str) -> Option<(&'static str, String)> {
    config_lookup(error)?;
    read_file(error, path)
}
#[cfg(test)]
pub(crate) fn unmeasured_errnos(method: &str) -> Vec<i32> {
    generated::ERRORS
        .iter()
        .filter(|(errno, _, _)| lookup_for(method, &io::Error::from_raw_os_error(*errno)).is_none())
        .map(|(errno, _, _)| *errno)
        .chain([123456, libc::EDQUOT])
        .collect()
}

pub(crate) fn lookup(error: &io::Error) -> Option<(&'static str, &'static str)> {
    let errno = if let Some(mkdir) = error
        .get_ref()
        .and_then(|e| e.downcast_ref::<mkdir::MkdirError>())
    {
        mkdir.source.raw_os_error()
    } else {
        stage(error).map_or_else(|| error.raw_os_error(), |e| e.source.raw_os_error())
    }?;
    generated::ERRORS
        .iter()
        .find(|(number, _, _)| *number == errno)
        .map(|(_, code, description)| (*code, *description))
}
pub(crate) fn diagnostic(
    error: &io::Error,
    syscall: &str,
    path: Option<&str>,
) -> Option<(&'static str, String)> {
    if let Some(mkdir) = error
        .get_ref()
        .and_then(|e| e.downcast_ref::<mkdir::MkdirError>())
    {
        return diagnostic(&mkdir.source, "mkdir", Some(&mkdir.path.to_string_lossy()));
    }
    let (code, description) = lookup(error)?;
    let (syscall, path) = stage(error).map_or((syscall, path), |e| (e.syscall, e.path.as_deref()));
    let suffix = path
        .filter(|p| !p.is_empty())
        .map_or(String::new(), |p| format!(" '{p}'"));
    Some((code, format!("{code}: {description}, {syscall}{suffix}")))
}
/// readFile errors before descriptor acquisition include the path. Node's
/// fstat/read descriptor errors have no path. EISDIR fixtures without stage
/// annotations also denote read (opening a directory succeeds on POSIX).
pub(crate) fn read_file(error: &io::Error, path: &str) -> Option<(&'static str, String)> {
    if stage(error).is_none() && lookup(error)?.0 == "EISDIR" {
        diagnostic(error, "read", None)
    } else {
        diagnostic(error, "open", Some(path))
    }
}
#[derive(Debug)]
struct Stage {
    source: io::Error,
    path: Option<String>,
    syscall: &'static str,
}
impl std::fmt::Display for Stage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.source.fmt(f)
    }
}
impl std::error::Error for Stage {}
fn stage(error: &io::Error) -> Option<&Stage> {
    error.get_ref()?.downcast_ref()
}
/// Preserve the actual fs boundary, including failures inside recursive removal.
pub(crate) fn path_error(
    source: io::Error,
    syscall: &'static str,
    path: &std::path::Path,
) -> io::Error {
    if stage(&source).is_some() {
        return source;
    }
    io::Error::new(
        source.kind(),
        Stage {
            source,
            syscall,
            path: Some(path.to_string_lossy().into_owned()),
        },
    )
}
pub(crate) fn descriptor_error(source: io::Error, syscall: &'static str) -> io::Error {
    io::Error::new(
        source.kind(),
        Stage {
            source,
            syscall,
            path: None,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn method_whitelists_equal_source_oracle_errno_sets() {
        use std::collections::BTreeSet;
        let project: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/project-files/io-errors-round6.json"
        ))
        .unwrap();
        for method in ["project_files_list", "project_file_read"] {
            let measured: BTreeSet<_> = project["cases"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|r| r["method"] == method)
                .map(|r| r["observed"][0]["code"].as_str().unwrap())
                .collect();
            let allowed: BTreeSet<_> = LOCAL_ERRNOS
                .iter()
                .find(|(m, _)| *m == method)
                .unwrap()
                .1
                .iter()
                .copied()
                .collect();
            assert_eq!(allowed, measured, "{method}");
        }
        let sources: Vec<serde_json::Value> = [
            include_str!("../tests/fixtures/session-model/io-errors.json"),
            include_str!("../tests/fixtures/session-model/io-errors-round5.json"),
        ]
        .into_iter()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
        let catalog: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/config/catalog-io-errors.json"
        ))
        .unwrap();
        for method in ["session_model_get", "model_catalog_list"] {
            let method_sources = if method == "session_model_get" {
                &sources[..]
            } else {
                std::slice::from_ref(&catalog)
            };
            let measured: BTreeSet<_> = method_sources
                .iter()
                .flat_map(|f| f["cases"].as_array().unwrap())
                .flat_map(|r| r["io"]["observations"].as_array().unwrap())
                .map(|o| o["code"].as_str().unwrap())
                .collect();
            let allowed: BTreeSet<_> = LOCAL_ERRNOS
                .iter()
                .find(|(m, _)| *m == method)
                .unwrap()
                .1
                .iter()
                .copied()
                .collect();
            assert_eq!(allowed, measured, "{method}");
        }
        let raw = include_str!("../tests/fixtures/snapshot-projection/oracle.json");
        crate::test_baseline::oracle(raw);
        let snapshots: serde_json::Value = serde_json::from_str(raw).unwrap();
        for (operation, method) in [
            ("snapshotList", "snapshot_list"),
            ("snapshotGet", "snapshot_get"),
            ("snapshotRestore", "snapshot_restore"),
        ] {
            let measured: BTreeSet<_> = snapshots["scenarios"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|scenario| scenario["operations"].as_array().unwrap())
                .filter(|row| row["op"] == operation)
                .filter_map(|row| row["outcome"]["error"].as_object())
                .filter(|error| error.get("name").and_then(|name| name.as_str()) == Some("Error"))
                .filter_map(|error| error.get("code").and_then(|code| code.as_str()))
                .collect();
            let allowed: BTreeSet<_> = LOCAL_ERRNOS
                .iter()
                .find(|(m, _)| *m == method)
                .unwrap()
                .1
                .iter()
                .copied()
                .collect();
            assert_eq!(allowed, measured, "{method}");
        }
        assert!(admits("snapshot_restore", "ENOENT"));
        assert!(!admits("snapshot_list", "ENOENT"));
        assert!(!admits("snapshot_restore", "EDQUOT"));
        assert!(
            lookup_for(
                "unregistered_method",
                &io::Error::from_raw_os_error(libc::ENOENT)
            )
            .is_none()
        );
    }
    #[test]
    fn every_generated_errno_and_format_matches_node_uv_exception() {
        let oracle: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/node-io/errors.json")).unwrap();
        for (errno, code, _) in generated::ERRORS {
            let error = io::Error::from_raw_os_error(*errno);
            let rows: Vec<_> = oracle["formatting"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|row| row["code"] == *code)
                .collect();
            assert_eq!(rows.len(), 18, "{code}");
            for row in rows {
                let actual = diagnostic(
                    &error,
                    row["syscall"].as_str().unwrap(),
                    row["path"].as_str(),
                )
                .unwrap();
                assert_eq!(actual.0, *code);
                assert_eq!(actual.1, row["message"].as_str().unwrap());
            }
        }
        #[cfg(target_os = "macos")]
        for entry in oracle["all"].as_array().unwrap() {
            let uv = entry["uv"].as_i64().unwrap();
            if (-1024..0).contains(&uv) {
                assert_eq!(
                    lookup(&io::Error::from_raw_os_error(-uv as i32)).unwrap().0,
                    entry["code"].as_str().unwrap()
                );
            }
        }
    }
    #[test]
    fn unknown_errno_and_rust_errors_require_host_including_descriptor_errors() {
        for e in [
            io::Error::from_raw_os_error(123456),
            io::Error::other("not an errno"),
            io::Error::from_raw_os_error(libc::EDQUOT),
        ] {
            assert!(diagnostic(&e, "stat", Some("/file")).is_none());
            assert!(read_file(&e, "/file").is_none());
            assert!(read_file(&descriptor_error(e, "read"), "/file").is_none());
        }
    }
    #[test]
    fn descriptor_errno_preserves_the_actual_syscall_without_path() {
        let e = descriptor_error(io::Error::from_raw_os_error(libc::EIO), "read");
        assert_eq!(
            read_file(&e, "/file").unwrap(),
            ("EIO", "EIO: i/o error, read".into())
        );
    }
}
