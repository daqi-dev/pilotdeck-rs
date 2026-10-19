use super::*;
use crate::metadata::{NativeFiles, Stat};
use base64::Engine;
use std::{
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
pub(crate) struct SnapshotFiles {
    files: serde_json::Value,
    process: serde_json::Value,
    pub calls: Arc<Mutex<Vec<i32>>>,
}
impl Files for SnapshotFiles {
    fn entries(&self, path: &Path) -> io::Result<Vec<(String, bool)>> {
        let fact = self
            .files
            .get(path.to_str().unwrap())
            .ok_or_else(|| io::Error::from_raw_os_error(libc::ENOENT))?;
        Ok(fact["entries"]
            .as_array()
            .ok_or(io::ErrorKind::NotADirectory)?
            .iter()
            .map(|e| (e[0].as_str().unwrap().into(), e[1].as_bool().unwrap()))
            .collect())
    }
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        let fact = self
            .files
            .get(path.to_str().unwrap())
            .ok_or_else(|| io::Error::from_raw_os_error(libc::ENOENT))?;
        recorded_io_error(fact, "readError")?;
        if fact["directory"].as_bool() == Some(true) {
            return Err(io::Error::from_raw_os_error(libc::EISDIR));
        }
        base64::engine::general_purpose::STANDARD
            .decode(fact["bytes"].as_str().unwrap_or(""))
            .map_err(|_| io::ErrorKind::InvalidData.into())
    }
    fn stat(&self, path: &Path) -> io::Result<Stat> {
        let fact = self
            .files
            .get(path.to_str().unwrap())
            .ok_or_else(|| io::Error::from_raw_os_error(libc::ENOENT))?;
        recorded_io_error(fact, "statError")?;
        Ok(Stat {
            directory: fact["directory"].as_bool().unwrap(),
            size: fact["size"].as_u64().unwrap(),
            birth_ms: 0.0,
            ctime_ms: 0.0,
            mtime_ms: 0.0,
        })
    }
    fn process_alive(&self, pid: i32) -> bool {
        self.calls.lock().unwrap().push(pid);
        // Every numeric fixture targets the oracle's own child. Fail instead of
        // probing an archived PID or an unrelated process in a later test run.
        assert_eq!(i64::from(pid), self.process["pid"].as_i64().unwrap());
        self.process["alive"].as_bool().unwrap() && !self.process["eperm"].as_bool().unwrap()
    }
}
fn recorded_io_error(fact: &serde_json::Value, key: &str) -> io::Result<()> {
    let errno = match fact[key].as_str() {
        Some("EACCES") => libc::EACCES,
        Some("ENOENT") => libc::ENOENT,
        Some("ENOTDIR") => libc::ENOTDIR,
        Some("EISDIR") => libc::EISDIR,
        Some("ENAMETOOLONG") => libc::ENAMETOOLONG,
        Some("ELOOP") => libc::ELOOP,
        None => return Ok(()),
        Some(code) => panic!("unrecorded errno {code}"),
    };
    Err(io::Error::from_raw_os_error(errno))
}
const IO_FIXTURE: &str = include_str!("../../tests/fixtures/session-model/io-errors.json");
const PROJECT_KEY_FIXTURE: &str =
    include_str!("../../tests/fixtures/session-model/project-key-identity.json");
pub(crate) fn fixture() -> serde_json::Value {
    serde_json::from_str(include_str!(
        "../../tests/fixtures/session-model/cases.json"
    ))
    .unwrap()
}
pub(crate) fn snapshot(row: &serde_json::Value) -> Metadata<SnapshotFiles> {
    Metadata {
        home: PathBuf::from(row["home"].as_str().unwrap()),
        cwd: PathBuf::from(row["cwd"].as_str().unwrap()),
        files: SnapshotFiles {
            files: row["files"].clone(),
            process: row["process"].clone(),
            calls: Arc::new(Mutex::new(vec![])),
        },
    }
}
fn env(row: &serde_json::Value) -> Env {
    row["env"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), v.as_str().unwrap().into()))
        .collect()
}
pub(crate) fn illformed_fixture() -> serde_json::Value {
    serde_json::from_str(include_str!(
        "../../tests/fixtures/session-model/illformed-collation.json"
    ))
    .unwrap()
}
#[test]
fn illformed_collation_has_precise_host_boundaries_and_node_responses() {
    let fixture = illformed_fixture();
    assert_eq!(fixture["cases"].as_array().unwrap().len(), 12);
    verify_responses(&fixture);
}
#[test]
fn complete_default_provider_matches_node_wire_fixtures() {
    verify_responses(&fixture());
}
/// The U+FFFD project and the lone-surrogate spelling are distinct registry
/// identities. `project-key-lone-surrogate` is the alias negative control: a
/// lossy decode would make it resolve onto the registered U+FFFD project.
/// The folded cases exercise `path.resolve` before that comparison: `..` can
/// drop a surrogate-bearing segment, while a surviving surrogate still fails
/// with its raw units.
#[test]
fn project_key_identity_matches_the_node_registry() {
    let fixture: serde_json::Value = serde_json::from_str(PROJECT_KEY_FIXTURE).unwrap();
    assert_eq!(fixture["cases"].as_array().unwrap().len(), 6);
    verify_responses(&fixture);
}
#[test]
fn filesystem_stages_match_real_node_failures_and_races() {
    for source in [
        IO_FIXTURE,
        include_str!("../../tests/fixtures/session-model/io-errors-round5.json"),
    ] {
        let fixture = serde_json::from_str(source).unwrap();
        verify_responses(&fixture);
        verify_diagnostics(source);
    }
}
fn verify_responses(fixture: &serde_json::Value) {
    assert_eq!(fixture["routerCalls"], 0);
    let mut failures = vec![];
    for row in fixture["cases"].as_array().unwrap() {
        let m = snapshot(row);
        let request = parse_json(row["request"].as_str().unwrap()).unwrap();
        let actual = if row["request"]
            .as_str()
            .unwrap()
            .contains("model_catalog_list")
        {
            crate::model_catalog::response(&request, &m, &env(row))
        } else {
            response(&request, &m, &env(row))
        };
        let expected_calls: Vec<i32> = row["process"]["killCalls"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| i32::try_from(v.as_i64().unwrap()).unwrap())
            .collect();
        assert_eq!(
            *m.files.calls.lock().unwrap(),
            expected_calls,
            "{} kill calls",
            row["id"]
        );
        if row["forward"].as_bool() == Some(true) {
            assert!(actual.is_none(), "{} must forward to TS", row["id"]);
            continue;
        }
        let actual = actual.map(|b| String::from_utf8(b).unwrap());
        let expected = row["response"].as_str().unwrap();
        if actual.as_deref() != Some(expected) {
            failures.push(format!(
                "{}\nactual: {:?}\nexpected: {}",
                row["id"], actual, expected
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}
#[test]
fn real_libc_probe_only_uses_an_owned_child_that_exits_naturally() {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};
    let mut child = Command::new("/bin/sh")
        .args(["-c", "echo ready; read owned_input"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    assert_eq!(line, "ready\n");
    let pid = i32::try_from(child.id()).unwrap();
    assert!(NativeFiles.process_alive(pid));
    drop(child.stdin.take());
    child.wait().unwrap();
    assert!(!NativeFiles.process_alive(pid));
}

#[test]
fn project_configuration_diagnostics_match_the_real_node_loader() {
    verify_diagnostics(include_str!(
        "../../tests/fixtures/session-model/cases.json"
    ));
}
fn verify_diagnostics(source: &str) {
    let fixture: serde_json::Value = serde_json::from_str(source).unwrap();
    let ordered = parse_json(source).unwrap();
    let JsValue::Array(ordered) = field(&ordered, "cases") else {
        panic!("cases")
    };
    let mut failures = vec![];
    for (index, row) in fixture["cases"].as_array().unwrap().iter().enumerate() {
        let m = snapshot(row);
        let request = parse_json(row["request"].as_str().unwrap()).unwrap();
        let project = field(field(&request, "params"), "projectKey")
            .as_string()
            .map(|s| String::from_utf16_lossy(s.units()));
        let loaded = config::load_project_with_files(&m, &env(row), project.as_deref());
        let expected_calls: Vec<i32> = row["process"]["loadKillCalls"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| i32::try_from(v.as_i64().unwrap()).unwrap())
            .collect();
        assert_eq!(
            *m.files.calls.lock().unwrap(),
            expected_calls,
            "{} loader kill calls",
            row["id"]
        );
        if row["loadForward"].as_bool() == Some(true) {
            assert!(
                loaded.err().unwrap().requires_host,
                "{} loader requires host",
                row["id"]
            );
            continue;
        }
        let actual = match loaded {
            Ok(c) => object([
                ("ok", JsValue::Bool(true)),
                ("diagnostics", JsValue::Array(c.diagnostics)),
            ]),
            Err(e) => object([
                ("ok", JsValue::Bool(false)),
                ("code", e.code.as_str().into()),
                ("message", JsValue::String(e.message)),
                ("diagnostics", JsValue::Array(e.diagnostics)),
            ]),
        };
        let expected = field(&ordered[index], "load").clone();
        if actual != expected {
            failures.push(format!(
                "{}\nactual: {}\nexpected: {}",
                row["id"],
                stringify_json(&actual).unwrap().unwrap(),
                stringify_json(&expected).unwrap().unwrap()
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

struct UnknownFiles {
    inner: SnapshotFiles,
    target: PathBuf,
    stat: bool,
    errno: Option<i32>,
}
impl UnknownFiles {
    fn error(&self) -> io::Error {
        self.errno.map_or_else(
            || io::Error::other("unclassified read failure"),
            io::Error::from_raw_os_error,
        )
    }
}
impl Files for UnknownFiles {
    fn entries(&self, path: &Path) -> io::Result<Vec<(String, bool)>> {
        self.inner.entries(path)
    }
    fn stat(&self, path: &Path) -> io::Result<Stat> {
        if self.stat && path == self.target {
            Err(self.error())
        } else {
            self.inner.stat(path)
        }
    }
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        if !self.stat && path == self.target {
            Err(self.error())
        } else {
            self.inner.read(path)
        }
    }
    fn process_alive(&self, _: i32) -> bool {
        panic!("unknown IO must not reach PID probe")
    }
}
#[test]
fn unknown_errno_forwards_entire_session_and_catalog_requests_at_every_fs_boundary() {
    let fixture: serde_json::Value = serde_json::from_str(IO_FIXTURE).unwrap();
    for target in ["transcript", "global", "project", "store", "sops", "lock"] {
        let row = fixture["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == format!("io-{target}-read-missing"))
            .unwrap();
        for stat in [false, true] {
            if target == "lock" && stat {
                continue;
            }
            for errno in crate::node_io::unmeasured_errnos("session_model_get")
                .into_iter()
                .map(Some)
                .chain([None])
            {
                let m = snapshot(row);
                let m = Metadata {
                    home: m.home,
                    cwd: m.cwd,
                    files: UnknownFiles {
                        inner: m.files,
                        target: PathBuf::from(row["io"]["target"].as_str().unwrap()),
                        stat,
                        errno,
                    },
                };
                let request = parse_json(row["request"].as_str().unwrap()).unwrap();
                assert!(
                    response(&request, &m, &env(row)).is_none(),
                    "{target} stat={stat} errno={errno:?}"
                );
                if target != "transcript" {
                    assert!(
                        config::load_project_with_files(
                            &m,
                            &env(row),
                            Some(m.home.to_str().unwrap())
                        )
                        .err()
                        .unwrap()
                        .requires_host,
                        "{target} stat={stat}"
                    );
                }
                if target == "global" {
                    let catalog = object([("id", "catalog".into()), ("params", object([]))]);
                    assert!(crate::model_catalog::response(&catalog, &m, &env(row)).is_none());
                }
            }
        }
    }
}

pub(crate) fn catalog_io_fixture() -> serde_json::Value {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/config/catalog-io-errors.json"
    ))
    .unwrap();
    assert!(catalog_io_cases_complete(&fixture));
    fixture
}
fn catalog_io_cases_complete(fixture: &serde_json::Value) -> bool {
    use std::collections::BTreeSet;
    let expected: BTreeSet<_> = [
        "stat-denied",
        "stat-missing",
        "stat-notdir",
        "read-denied",
        "read-missing",
        "read-notdir",
        "read-directory",
        "stat-longname",
        "read-longname",
        "stat-loop",
        "read-loop",
        "stat-deep",
        "read-deep",
    ]
    .into_iter()
    .map(|scenario| format!("io-global-{scenario}"))
    .collect();
    let Some(rows) = fixture["cases"].as_array() else {
        return false;
    };
    rows.len() == 13
        && rows
            .iter()
            .map(|r| r["id"].as_str().unwrap_or("").to_string())
            .collect::<BTreeSet<_>>()
            == expected
        && rows.iter().all(|row| {
            let request = parse_json(row["request"].as_str().unwrap_or("")).ok();
            request.as_ref().and_then(|r| r.get("method"))
                == Some(&JsValue::from("model_catalog_list"))
                && row["io"]["observations"].as_array().is_some_and(|o| {
                    ["loader", "rpc"]
                        .iter()
                        .all(|phase| o.iter().any(|entry| entry["phase"] == *phase))
                })
        })
}
#[test]
fn catalog_rpc_has_its_own_real_filesystem_errno_oracle() {
    const SOURCE: &str = include_str!("../../tests/fixtures/config/catalog-io-errors.json");
    let fixture = catalog_io_fixture();
    verify_responses(&fixture);
    verify_diagnostics(SOURCE);
    for replacement in [
        vec![],
        fixture["cases"].as_array().unwrap()[..12].to_vec(),
        vec![fixture["cases"][0].clone(); 13],
    ] {
        let mut broken = fixture.clone();
        broken["cases"] = replacement.into();
        assert!(!catalog_io_cases_complete(&broken));
    }
    let mut wrong_method = fixture.clone();
    let mut request: serde_json::Value =
        serde_json::from_str(wrong_method["cases"][0]["request"].as_str().unwrap()).unwrap();
    request["method"] = "session_model_get".into();
    wrong_method["cases"][0]["request"] = request.to_string().into();
    assert!(!catalog_io_cases_complete(&wrong_method));
    println!("PDRS_REJECTED_FIXTURE wrong-method");
    for phase in ["loader", "rpc"] {
        let mut broken = fixture.clone();
        broken["cases"][0]["io"]["observations"]
            .as_array_mut()
            .unwrap()
            .retain(|entry| entry["phase"] != phase);
        assert!(!catalog_io_cases_complete(&broken));
        println!("PDRS_REJECTED_FIXTURE missing-phase-{phase}");
    }
}
#[test]
fn catalog_unknown_errno_keeps_both_global_fs_boundaries_on_host() {
    let fixture = catalog_io_fixture();
    let row = fixture["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == "io-global-read-missing")
        .unwrap();
    for stat in [false, true] {
        for errno in crate::node_io::unmeasured_errnos("model_catalog_list")
            .into_iter()
            .map(Some)
            .chain([None])
        {
            let metadata = snapshot(row);
            let metadata = Metadata {
                home: metadata.home,
                cwd: metadata.cwd,
                files: UnknownFiles {
                    inner: metadata.files,
                    target: PathBuf::from(row["io"]["target"].as_str().unwrap()),
                    stat,
                    errno,
                },
            };
            let request = parse_json(row["request"].as_str().unwrap()).unwrap();
            assert!(
                crate::model_catalog::response(&request, &metadata, &env(row)).is_none(),
                "stat={stat} errno={errno:?}"
            );
        }
    }
}
