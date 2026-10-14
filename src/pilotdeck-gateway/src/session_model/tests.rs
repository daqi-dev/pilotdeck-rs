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
            .ok_or(io::ErrorKind::NotFound)?;
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
            .ok_or(io::ErrorKind::NotFound)?;
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
            .ok_or(io::ErrorKind::NotFound)?;
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
#[test]
fn complete_default_provider_matches_node_wire_fixtures() {
    let fixture = fixture();
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
        if row["forward"].as_bool() == Some(true) {
            assert!(actual.is_none(), "{} must obey D6", row["id"]);
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
    let fixture = fixture();
    let ordered = parse_json(include_str!(
        "../../tests/fixtures/session-model/cases.json"
    ))
    .unwrap();
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
        let loaded = config::load_project(&m, &env(row), project.as_deref());
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
