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
    lookup(error).map(|(code, description)| uv_exception(code, description, error, syscall, path))
}

/// [`diagnostic`] with libuv's `UV_UNKNOWN` fallback for errnos outside the
/// generated table: `UNKNOWN: unknown error`, under the same stage and path
/// rules, formatted by the same code — so a staged error renders the same
/// through either entry point, and callers cannot get an `UNKNOWN` message
/// the shared formatter would never produce.
///
/// The `Stage` override (and the `MkdirError` recursion) is unreachable
/// through this fallback at every current production call point: each caller
/// of `node_fs_message_or_unknown` — CLI `skills::fs_failure`, MCP
/// `read_mcp_config`'s open/read, the MCP client's `create_dir_all` and
/// mkdtemp — feeds an error straight from its own `std::fs` call, which never
/// carries a `Stage` or a `MkdirError`, and the gateway's staged errors reach
/// `diagnostic`/`read_file` callers instead. The override keeps the two entry
/// points from drifting apart for a staged error; the staged case in its test
/// below is defensive, not reachable behavior.
pub(crate) fn diagnostic_or_unknown(
    error: &io::Error,
    syscall: &str,
    path: Option<&str>,
) -> (&'static str, String) {
    if let Some(mkdir) = error
        .get_ref()
        .and_then(|e| e.downcast_ref::<mkdir::MkdirError>())
    {
        return diagnostic_or_unknown(&mkdir.source, "mkdir", Some(&mkdir.path.to_string_lossy()));
    }
    let (code, description) = lookup(error).unwrap_or(("UNKNOWN", "unknown error"));
    uv_exception(code, description, error, syscall, path)
}

/// One message body for both entry points above: `${code}: ${description},
/// ${syscall}` plus ` 'path'` for a known path, even an empty one. Real fs
/// errors report through the C++ binding formatter, which appends a provided
/// path verbatim (node v22.22.0: `fs.openSync('')` → `ENOENT: no such file
/// or directory, open ''`, likewise `statSync`/`readdirSync`; the JS
/// `UVException` class used to build the table truthiness-drops an empty
/// path — `_tmp/20261025-issue-656/fix-r2/empty-path-probe.out`). A `Stage`
/// overrides the caller's syscall and path.
fn uv_exception(
    code: &'static str,
    description: &'static str,
    error: &io::Error,
    syscall: &str,
    path: Option<&str>,
) -> (&'static str, String) {
    let (syscall, path) = stage(error).map_or((syscall, path), |e| (e.syscall, e.path.as_deref()));
    let suffix = path.map_or(String::new(), |p| format!(" '{p}'"));
    (code, format!("{code}: {description}, {syscall}{suffix}"))
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
/// Node's `os.tmpdir()` on unix (`node:lib/os.js`): the first nonempty value
/// of TMPDIR, then TMP, then TEMP (empty strings are skipped, like JS `||`),
/// else `/tmp`, with exactly one trailing `/` dropped while the result stays
/// longer than the root. Node reads `process.env` as already-decoded strings,
/// so a non-UTF-8 value is decoded lossily (`\xff` becomes U+FFFD) and then
/// used like any other value: it is neither kept as raw bytes nor skipped.
/// Probed against node v22.22.0 (`_tmp/20261025-issue-621-fix-r4/node-probe/`,
/// `_tmp/20261025-issue-621-takeover/node-tmpdir-nonutf8-probe/`): `//`
/// collapses to `/` while `///` only to `//`, relative values pass through,
/// and `TMPDIR=/d\xff` writes into `/d\u{FFFD}`. Shared by the model request
/// dump and the MCP session temp dir.
pub(crate) fn node_tmpdir(var: impl Fn(&str) -> Option<std::ffi::OsString>) -> String {
    let mut path = ["TMPDIR", "TMP", "TEMP"]
        .into_iter()
        .find_map(|key| {
            var(key)
                .map(|value| value.to_string_lossy().into_owned())
                .filter(|value| !value.is_empty())
        })
        .unwrap_or_else(|| "/tmp".into());
    if path.len() > 1 && path.ends_with('/') {
        path.pop();
    }
    path
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
    fn uv_unknown_fallback_shares_the_stage_and_path_rules() {
        // 123456 stays outside libuv's table on both supported targets
        // (macOS, Linux), so every use below exercises the fallback.
        let e = io::Error::from_raw_os_error(123456);
        // A known path stays appended — even an empty one, like the C++
        // binding formatter real fs errors report through — and no path
        // stays absent.
        assert_eq!(
            diagnostic_or_unknown(&e, "open", Some("")).1,
            "UNKNOWN: unknown error, open ''"
        );
        assert_eq!(
            diagnostic_or_unknown(&e, "stat", Some("/file")).1,
            "UNKNOWN: unknown error, stat '/file'"
        );
        assert_eq!(
            diagnostic_or_unknown(&e, "stat", None).1,
            "UNKNOWN: unknown error, stat"
        );
        // A Stage overrides the caller's labels in the fallback too, so a
        // `descriptor_error(e, "write")` keeps reporting `write` with no path
        // even when the caller labels it open — unreachable through today's
        // production call points (see `diagnostic_or_unknown`); the case
        // keeps the fallback from drifting from `diagnostic`.
        let staged = descriptor_error(e, "write");
        assert_eq!(
            diagnostic_or_unknown(&staged, "open", Some("/file")).1,
            "UNKNOWN: unknown error, write"
        );
    }
    #[test]
    fn tmpdir_follows_node_os_tmpdir() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |key: &str| {
                pairs
                    .iter()
                    .find(|(name, _)| *name == key)
                    .map(|(_, value)| OsString::from(*value))
            }
        };
        assert_eq!(node_tmpdir(env(&[])), "/tmp");
        // Empty values fall through to the next variable, like JS `||`.
        assert_eq!(node_tmpdir(env(&[("TMPDIR", "")])), "/tmp");
        assert_eq!(node_tmpdir(env(&[("TMPDIR", ""), ("TEMP", "/t/")])), "/t");
        assert_eq!(node_tmpdir(env(&[("TMPDIR", "/x/"), ("TMP", "/y")])), "/x");
        assert_eq!(
            node_tmpdir(env(&[("TMPDIR", ""), ("TMP", ""), ("TEMP", "")])),
            "/tmp"
        );
        // Exactly one trailing `/` goes away, but only past the root.
        assert_eq!(node_tmpdir(env(&[("TMPDIR", "/tmp/x/")])), "/tmp/x");
        assert_eq!(node_tmpdir(env(&[("TMPDIR", "/tmp/x//")])), "/tmp/x/");
        assert_eq!(node_tmpdir(env(&[("TMPDIR", "///")])), "//");
        assert_eq!(node_tmpdir(env(&[("TMPDIR", "//")])), "/");
        assert_eq!(node_tmpdir(env(&[("TMPDIR", "/")])), "/");
        // Relative values pass through unchanged, like node.
        assert_eq!(node_tmpdir(env(&[("TMPDIR", "relative")])), "relative");
        // A non-UTF-8 TMPDIR is decoded lossily and then used, like node's
        // already-decoded `process.env`: not skipped to TMP, not kept raw.
        let raw = OsString::from_vec(b"/tmp/\xff".to_vec());
        assert_eq!(
            node_tmpdir(|key| match key {
                "TMPDIR" => Some(raw.clone()),
                "TMP" => Some("/next".into()),
                _ => None,
            }),
            "/tmp/\u{FFFD}"
        );
        assert_eq!(
            node_tmpdir(|_| Some(OsString::from_vec(b"/tmp/\xff/".to_vec()))),
            "/tmp/\u{FFFD}"
        );
    }
    #[test]
    fn descriptor_errno_preserves_the_actual_syscall_without_path() {
        let e = descriptor_error(io::Error::from_raw_os_error(libc::EIO), "read");
        assert_eq!(
            read_file(&e, "/file").unwrap(),
            ("EIO", "EIO: i/o error, read".into())
        );
    }
    /// #117: the errno table was generated on macOS, so its libc numbers were
    /// never measured against a real Linux Node. This runs on Linux against a
    /// fresh sample from `tests/fixtures/node-io/linux-errno-verify.mjs`
    /// (same pinned Node, `os.constants` + libuv tables, no repo writes).
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_node_errno_numbers_match_node_os_constants() {
        if std::env::var_os("PDRS_NODE_IO_LINUX_JSON").is_none() {
            return;
        }
        let path = std::path::PathBuf::from(
            std::env::var_os("PDRS_NODE_IO_LINUX_JSON")
                .expect("#117 requires PDRS_NODE_IO_LINUX_JSON (fresh Node errno sample path)"),
        );
        let fresh: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("#117 read the fresh Node errno sample: {error}")),
        )
        .expect("#117 parse the fresh Node errno sample");
        assert_eq!(
            fresh["node"].as_str(),
            Some("v22.22.0"),
            "#117 sample Node version: {}",
            fresh["node"]
        );
        assert_eq!(
            fresh["uv"].as_str(),
            Some("1.52.1"),
            "#117 sample libuv version: {}",
            fresh["uv"]
        );
        assert_eq!(
            fresh["platform"].as_str(),
            Some("linux"),
            "#117 sample platform: {}",
            fresh["platform"]
        );
        // The committed fixture is darwin, so its `uv` numbers are platform
        // data — cb8 measured 38 of 85 rows differing with zero code or
        // description drift. Names and descriptions are the shared data: a
        // fresh Linux sample must agree with the fixture code by code, and a
        // name only one side knows must still be pinned to the compiled
        // table. The numbers themselves are checked below against this
        // Linux's own libc instead of the darwin recording.
        let committed: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/node-io/errors.json"))
                .expect("#117 parse the committed darwin errors.json fixture");
        fn all_by_code(document: &serde_json::Value) -> std::collections::BTreeMap<&str, &str> {
            document["all"]
                .as_array()
                .unwrap_or_else(|| panic!("#117 libuv error map is not an array"))
                .iter()
                .map(|row| {
                    (
                        row["code"]
                            .as_str()
                            .unwrap_or_else(|| panic!("#117 libuv error row without a code")),
                        row["description"].as_str().unwrap_or_else(|| {
                            panic!("#117 libuv error row without a description")
                        }),
                    )
                })
                .collect()
        }
        let fresh_all = all_by_code(&fresh);
        let committed_all = all_by_code(&committed);
        for (code, description) in &fresh_all {
            match committed_all.get(code) {
                Some(recorded) => assert_eq!(
                    description, recorded,
                    "#117 fresh Linux description for {code} differs from the committed table"
                ),
                // A name this Linux's libuv knows but the darwin recording
                // lacks is exactly what the cfg guards in generated.rs exist
                // for; it must not bypass the compiled table.
                None => assert!(
                    generated::ERRORS.iter().any(|(_, known, _)| known == code),
                    "#117 fresh Linux-only libuv code {code} is missing from the generated table"
                ),
            }
        }
        for code in committed_all.keys() {
            assert!(
                fresh_all.contains_key(code),
                "#117 committed darwin libuv code {code} is absent from the fresh Linux sample"
            );
        }
        // The formatting rows for a code only one side's libuv table knows
        // cannot pair, so compare the shared codes only; both tables are
        // generated in the same deterministic order, which filtering keeps.
        fn formatting_codes(document: &serde_json::Value) -> std::collections::BTreeSet<&str> {
            document["formatting"]
                .as_array()
                .unwrap_or_else(|| panic!("#117 formatting table is not an array"))
                .iter()
                .map(|row| {
                    row["code"]
                        .as_str()
                        .unwrap_or_else(|| panic!("#117 formatting row without a code"))
                })
                .collect()
        }
        let fresh_formatting = formatting_codes(&fresh);
        let committed_formatting = formatting_codes(&committed);
        for code in fresh_formatting.difference(&committed_formatting) {
            assert!(
                generated::ERRORS.iter().any(|(_, known, _)| known == code),
                "#117 fresh Linux-only formatting code {code} is missing from the generated table"
            );
        }
        let shared_formatting: std::collections::BTreeSet<&str> = fresh_formatting
            .intersection(&committed_formatting)
            .copied()
            .collect();
        fn shared_rows<'a>(
            document: &'a serde_json::Value,
            shared: &std::collections::BTreeSet<&str>,
        ) -> Vec<&'a serde_json::Value> {
            document["formatting"]
                .as_array()
                .unwrap_or_else(|| panic!("#117 formatting table is not an array"))
                .iter()
                .filter(|row| {
                    row["code"]
                        .as_str()
                        .is_some_and(|code| shared.contains(code))
                })
                .collect()
        }
        assert_eq!(
            shared_rows(&fresh, &shared_formatting),
            shared_rows(&committed, &shared_formatting),
            "#117 fresh Linux UVException formatting differs from the committed table (shared codes)"
        );
        // On unix a libuv system errno is the negated libc errno, so every
        // generated row carries the number Node itself uses on this Linux.
        // Node's own constant table comes first: a table number that only the
        // libuv direction still catches is the Linux-only names below, whose
        // os.constants entry cb8 measured as absent (null), not wrong.
        for (errno, code, _) in generated::ERRORS {
            let entry = fresh["all"]
                .as_array()
                .and_then(|rows| rows.iter().find(|row| row["code"].as_str() == Some(code)))
                .unwrap_or_else(|| panic!("#117 fresh libuv sample lacks {code}"));
            match fresh["errno"][code].as_i64() {
                Some(constant) => assert_eq!(
                    constant,
                    i64::from(*errno),
                    "#117 Node os.constants.errno {code} differs from the generated table"
                ),
                None => assert!(
                    fresh["errno"][code].is_null(),
                    "#117 Node os.constants.errno {code} is neither a number nor absent"
                ),
            }
            assert_eq!(
                -entry["uv"]
                    .as_i64()
                    .unwrap_or_else(|| panic!("#117 fresh libuv {code} uv value is not a number")),
                i64::from(*errno),
                "#117 libuv errno number for {code} differs from the generated table"
            );
        }
        // The lookup direction real errors take: io::Error carries the libc
        // errno, libuv's mapping recovers Node's code name.
        for entry in fresh["all"]
            .as_array()
            .unwrap_or_else(|| panic!("#117 libuv error map is not an array"))
        {
            let uv = entry["uv"]
                .as_i64()
                .unwrap_or_else(|| panic!("#117 fresh libuv uv value is not a number"));
            if (-1024..0).contains(&uv) {
                let expected = entry["code"]
                    .as_str()
                    .unwrap_or_else(|| panic!("#117 fresh libuv code is not a string"));
                let found = lookup(&io::Error::from_raw_os_error(-uv as i32))
                    .unwrap_or_else(|| panic!("#117 lookup found no code for errno {}", -uv))
                    .0;
                assert_eq!(
                    found, expected,
                    "#117 lookup for {expected} recovers the wrong code"
                );
            }
        }
    }
}
