use super::*;
use crate::dispatch::{self, Route};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

const SOURCE: &str = include_str!("../../tests/fixtures/metadata/describe-io-errors.json");
// Original Node recording bytes; provenance is indexed beside the fixture docs.
const SOURCE_SHA256: &str = "995400bce07ef1bee5e4e0d6d3766343ae0229e21eac496394b6866e68518c7c";
const FILE_SOURCE: &str =
    include_str!("../../tests/fixtures/metadata/describe-file-io-errors.json");
const FILE_SOURCE_SHA256: &str = "8206d63968542d840326ff1e55501077402b62908e1ffe682e690934355c4fae";
const FILE_IDS: [&str; 3] = ["file-positive", "file-denied", "file-missing"];
const IDS: [&str; 10] = [
    "positive",
    "collision-positive",
    "projects-missing",
    "projects-notdir",
    "projects-denied-legacy",
    "projects-denied-marker",
    "chats-missing",
    "chats-notdir",
    "chats-denied",
    "chats-loop",
];
fn proof(row: &Value, api: &str, path: &str, code: &str, caller: &str) -> bool {
    row["observations"].as_array().is_some_and(|observations| {
        observations.iter().any(|o| {
            o["api"] == api
                && o["path"] == path
                && o["ok"] == false
                && o["code"] == code
                && o["caller"].as_str().is_some_and(|s| s.contains(caller))
        })
    })
}
fn observation_method(api: &str) -> Option<&'static str> {
    match api {
        "fsp.readdir" | "fs.readdirSync" => Some("entries"),
        "fsp.stat" | "fs.statSync" => Some("stat"),
        "fsp.readFile" | "fs.readFileSync" => Some("read"),
        "fsp.open" => Some("session_summary"),
        _ => None,
    }
}
fn success_observation_api(api: &str) -> bool {
    observation_method(api).is_some()
        || matches!(
            api,
            "fsp.open"
                | "fs.existsSync"
                | "FileHandle.stat"
                | "FileHandle.read"
                | "FileHandle.readFile"
        )
}
fn complete(fixture: &Value) -> bool {
    complete_cases(fixture, &IDS)
}
fn complete_cases(fixture: &Value, ids: &[&str]) -> bool {
    let Some(rows) = fixture["cases"].as_array() else {
        return false;
    };
    if rows.len() != ids.len()
        || rows
            .iter()
            .map(|r| r["id"].as_str().unwrap_or(""))
            .collect::<BTreeSet<_>>()
            != ids.iter().copied().collect()
    {
        return false;
    }
    rows.iter().all(|row| {
        let Some(observations) = row["observations"].as_array() else {
            return false;
        };
        if observations.iter().any(|o| match o["ok"].as_bool() {
            Some(false) => o["api"].as_str().and_then(observation_method).is_none(),
            Some(true) => !o["api"].as_str().is_some_and(success_observation_api),
            None => true,
        }) {
            return false;
        }
        let Some(id) = row["id"].as_str() else {
            return false;
        };
        let expected_code = match id {
            "projects-missing" | "chats-missing" | "file-missing" => Some("ENOENT"),
            "projects-notdir" | "chats-notdir" => Some("ENOTDIR"),
            "projects-denied-legacy"
            | "projects-denied-marker"
            | "chats-denied"
            | "file-denied" => Some("EACCES"),
            "chats-loop" => Some("ELOOP"),
            _ => None,
        };
        if row["expectedError"].as_str() != expected_code {
            return false;
        }
        let Some(request) = row["request"]
            .as_str()
            .and_then(|s| serde_json::from_str::<Value>(s).ok())
        else {
            return false;
        };
        let Some(response) = row["response"]
            .as_str()
            .and_then(|s| serde_json::from_str::<Value>(s).ok())
        else {
            return false;
        };
        if row["status"] != "RUN"
            || request["method"] != "describe_project"
            || request["id"] != id
            || request["params"]["projectKey"] != row["project"]
            || response["id"] != id
            || response["ok"] != true
        {
            return false;
        }
        let result = &response["result"];
        let session = matches!(
            id,
            "positive"
                | "collision-positive"
                | "projects-denied-legacy"
                | "projects-denied-marker"
                | "file-positive"
        );
        let created = !matches!(
            id,
            "projects-missing" | "projects-notdir" | "projects-denied-legacy"
        );
        if result["sessionCount"] != i32::from(session)
            || result.get("lastActivity").is_some_and(Value::is_number) != session
            || result.get("createdAt").is_some_and(Value::is_number) != created
        {
            return false;
        }
        let projects = Path::new(row["home"].as_str().unwrap()).join("projects");
        let projects = projects.to_str().unwrap();
        let chats = row["chats"].as_str().unwrap();
        let transcript = Path::new(chats).join("visible.jsonl");
        let error_target = if id.starts_with("file-") {
            transcript.to_str().unwrap()
        } else if id.starts_with("projects-") {
            projects
        } else {
            chats
        };
        if row["errorTarget"].as_str() != Some(error_target) {
            return false;
        }
        match id {
            "file-positive" | "file-denied" | "file-missing" => {
                complete_file_case(row, id, chats, error_target)
            }
            "positive" | "collision-positive" => row["expectedError"].is_null(),
            "projects-missing" => proof(
                row,
                "fsp.readdir",
                projects,
                "ENOENT",
                "resolveProjectStorageDir",
            ),
            "projects-notdir" => {
                proof(
                    row,
                    "fsp.readdir",
                    projects,
                    "ENOTDIR",
                    "resolveProjectStorageDir",
                ) && proof(
                    row,
                    "fs.readdirSync",
                    projects,
                    "ENOTDIR",
                    "findStoredProjectId",
                )
            }
            "projects-denied-legacy" => {
                proof(
                    row,
                    "fsp.readdir",
                    projects,
                    "EACCES",
                    "resolveProjectStorageDir",
                ) && proof(
                    row,
                    "fs.readdirSync",
                    projects,
                    "EACCES",
                    "findStoredProjectId",
                ) && row["observations"].as_array().unwrap().iter().any(|o| {
                    o["api"] == "fsp.stat"
                        && o["path"] == row["legacy"]
                        && o["ok"] == true
                        && o["result"]["directory"] == true
                })
            }
            "projects-denied-marker" => {
                proof(
                    row,
                    "fs.readdirSync",
                    projects,
                    "EACCES",
                    "findStoredProjectId",
                ) && !row["observations"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|o| o["api"] == "fsp.readdir" && o["path"] == projects)
            }
            _ => {
                let code = match id {
                    "chats-missing" => "ENOENT",
                    "chats-notdir" => "ENOTDIR",
                    "chats-denied" => "EACCES",
                    "chats-loop" => "ELOOP",
                    _ => return false,
                };
                proof(row, "fsp.readdir", chats, code, "listProjectSessions")
            }
        }
    })
}
fn complete_file_case(row: &Value, id: &str, chats: &str, transcript: &str) -> bool {
    let observations = row["observations"].as_array().unwrap();
    let listed = observations.iter().position(|o| {
        o["api"] == "fsp.readdir"
            && o["path"] == chats
            && o["ok"] == true
            && o["result"] == serde_json::json!(["visible.jsonl"])
            && o["caller"]
                .as_str()
                .is_some_and(|s| s.contains("listProjectSessions"))
    });
    let Some(listed) = listed else { return false };
    let Some(stat) = observations.iter().position(|o| {
        o["api"] == "fsp.stat"
            && o["path"] == transcript
            && o["caller"]
                .as_str()
                .is_some_and(|s| s.contains("readSessionLite"))
    }) else {
        return false;
    };
    if stat <= listed {
        return false;
    }
    let file = &row["files"][transcript];
    if file["directory"] != false || !file["size"].as_u64().is_some_and(|size| size > 0) {
        return false;
    }
    if id == "file-missing" {
        return proof(row, "fsp.stat", transcript, "ENOENT", "readSessionLite")
            && row["mutations"]
                == serde_json::json!([{
                    "operation": "unlink", "path": transcript,
                    "after": "fsp.readdir", "before": "fsp.stat"
                }])
            && file["bytes"].is_string();
    }
    if row["mutations"] != serde_json::json!([])
        || observations[stat]["ok"] != true
        || observations[stat]["result"]["size"] != file["size"]
    {
        return false;
    }
    let Some(open) = observations.iter().position(|o| {
        o["api"] == "fsp.open"
            && o["path"] == transcript
            && o["caller"]
                .as_str()
                .is_some_and(|s| s.contains("readSessionLite"))
    }) else {
        return false;
    };
    open > stat
        && if id == "file-denied" {
            file["readError"] == "EACCES"
                && proof(row, "fsp.open", transcript, "EACCES", "readSessionLite")
        } else {
            file["bytes"].is_string() && observations[open]["ok"] == true
        }
}
fn file_fixture() -> Value {
    assert_eq!(
        format!("{:x}", Sha256::digest(FILE_SOURCE.as_bytes())),
        FILE_SOURCE_SHA256,
        "file fixture must match original Node bytes"
    );
    let fixture: Value = serde_json::from_str(FILE_SOURCE).unwrap();
    assert!(
        complete_cases(&fixture, &FILE_IDS),
        "three complete file RPC/errno cases required"
    );
    fixture
}
pub(crate) fn fixture() -> Value {
    recorded_fixture(SOURCE)
}
fn recorded_fixture(source: &str) -> Value {
    assert_eq!(
        format!("{:x}", Sha256::digest(source.as_bytes())),
        SOURCE_SHA256,
        "static fixture must match independently pinned original Node bytes"
    );
    let fixture: Value = serde_json::from_str(source).unwrap();
    assert!(
        complete(&fixture),
        "ten complete real RPC/errno cases required"
    );
    fixture
}
pub(crate) struct Snapshot {
    facts: Value,
    errors: HashMap<(String, String), String>,
}
#[derive(Clone, Default)]
pub(crate) struct Requests(Arc<Mutex<Vec<EntryRequest>>>);
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct EntryRequest {
    method: &'static str,
    path: PathBuf,
    result: Result<(), Option<i32>>,
}
impl Requests {
    fn entries(&self, path: &Path, result: &io::Result<Vec<(String, bool)>>) {
        self.io("entries", path, result);
    }
    fn io<T>(&self, method: &'static str, path: &Path, result: &io::Result<T>) {
        self.record(
            method,
            path,
            result.as_ref().map(|_| ()).map_err(io::Error::raw_os_error),
        );
    }
    fn record(&self, method: &'static str, path: &Path, result: Result<(), Option<i32>>) {
        self.0.lock().unwrap().push(EntryRequest {
            method,
            path: path.into(),
            result,
        });
    }
    pub(crate) fn assert_target(&self, row: &Value) {
        let target = PathBuf::from(row["errorTarget"].as_str().unwrap());
        let method = match row["id"].as_str().unwrap() {
            "file-positive" | "file-denied" => "session_summary",
            "file-missing" => "stat",
            _ => "entries",
        };
        // Files::session_summary swallows open/read errors as Ok(false).
        // complete_file_case preserves the raw Node errno at that boundary.
        let result = if method == "session_summary" {
            Ok(())
        } else {
            row["expectedError"]
                .as_str()
                .map_or(Ok(()), |code| Err(errno(code).raw_os_error()))
        };
        assert!(
            self.0.lock().unwrap().contains(&EntryRequest {
                method,
                path: target,
                result
            }),
            "{} production must request the recorded target and errno",
            row["id"]
        );
        let expected = row["observations"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|o| {
                let ok = o["ok"].as_bool().expect("observation ok must be boolean");
                let api = o["api"].as_str().unwrap();
                if ok {
                    assert!(
                        success_observation_api(api),
                        "unsupported success observation API {api}"
                    );
                }
                let method = observation_method(api)?;
                Some(EntryRequest {
                    method,
                    path: o["path"].as_str().unwrap().into(),
                    result: if ok || method == "session_summary" {
                        Ok(())
                    } else {
                        Err(errno(o["code"].as_str().unwrap()).raw_os_error())
                    },
                })
            })
            .collect::<BTreeSet<_>>();
        let actual = self
            .0
            .lock()
            .unwrap()
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        assert_eq!(
            actual, expected,
            "{} production filesystem request contract",
            row["id"]
        );
        println!("PDRS_METADATA_PATH_OK {}", row["id"].as_str().unwrap());
    }
}
pub(crate) struct Observed<F> {
    inner: F,
    pub(crate) requests: Requests,
}
impl<F: Files> Files for Observed<F> {
    fn entries(&self, path: &Path) -> io::Result<Vec<(String, bool)>> {
        let result = self.inner.entries(path);
        self.requests.entries(path, &result);
        result
    }
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        let result = self.inner.read(path);
        self.requests.io("read", path, &result);
        result
    }
    fn stat(&self, path: &Path) -> io::Result<Stat> {
        let result = self.inner.stat(path);
        self.requests.io("stat", path, &result);
        result
    }
    fn session_summary(&self, path: &Path, size: u64) -> Result<bool, ()> {
        let result = self.inner.session_summary(path, size);
        self.requests.record(
            "session_summary",
            path,
            result.as_ref().map(|_| ()).map_err(|_| None),
        );
        result
    }
}
fn errno(code: &str) -> io::Error {
    io::Error::from_raw_os_error(match code {
        "ENOENT" => libc::ENOENT,
        "ENOTDIR" => libc::ENOTDIR,
        "EACCES" => libc::EACCES,
        "ELOOP" => libc::ELOOP,
        other => panic!("unrecorded errno {other}"),
    })
}
impl Snapshot {
    fn fact(&self, path: &Path) -> io::Result<&Value> {
        Ok(self
            .facts
            .get(path.to_str().unwrap())
            .unwrap_or_else(|| panic!("unrecorded filesystem path {}", path.display())))
    }
    fn error(&self, method: &str, path: &Path) -> io::Result<()> {
        match self
            .errors
            .get(&(method.into(), path.to_str().unwrap().into()))
        {
            Some(code) => Err(errno(code)),
            None => {
                let fact = self.fact(path)?;
                let registered = match method {
                    "entries" => fact["entries"].is_array() || fact["entriesError"].is_string(),
                    "read" => fact["bytes"].is_string() || fact["readError"].is_string(),
                    "stat" => fact["directory"].is_boolean() || fact["statError"].is_string(),
                    _ => false,
                };
                assert!(
                    registered,
                    "unrecorded filesystem API {method} at {}",
                    path.display()
                );
                Ok(())
            }
        }
    }
}
impl Files for Snapshot {
    fn session_summary(&self, path: &Path, _size: u64) -> Result<bool, ()> {
        if self
            .errors
            .contains_key(&("session_summary".into(), path.to_str().unwrap().into()))
        {
            return Ok(false);
        }
        match self.read(path) {
            Ok(bytes) => session_has_summary(&bytes),
            Err(_) => Ok(false),
        }
    }
    fn entries(&self, path: &Path) -> io::Result<Vec<(String, bool)>> {
        self.error("entries", path)?;
        let fact = self.fact(path)?;
        if let Some(code) = fact["entriesError"].as_str() {
            return Err(errno(code));
        }
        let mut entries = fact["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| {
                (
                    entry[0].as_str().unwrap().into(),
                    entry[1].as_bool().unwrap(),
                )
            })
            .collect::<Vec<(String, bool)>>();
        entries.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
        Ok(entries)
    }
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        use base64::Engine;
        self.error("read", path)?;
        let fact = self.fact(path)?;
        if let Some(code) = fact["readError"].as_str() {
            return Err(errno(code));
        }
        Ok(base64::engine::general_purpose::STANDARD
            .decode(fact["bytes"].as_str().unwrap())
            .unwrap())
    }
    fn stat(&self, path: &Path) -> io::Result<Stat> {
        self.error("stat", path)?;
        let fact = self.fact(path)?;
        if let Some(code) = fact["statError"].as_str() {
            return Err(errno(code));
        }
        Ok(Stat {
            directory: fact["directory"].as_bool().unwrap(),
            size: fact["size"].as_u64().unwrap(),
            birth_ms: fact["birth_ms"].as_f64().unwrap(),
            ctime_ms: fact["ctime_ms"].as_f64().unwrap(),
            mtime_ms: fact["mtime_ms"].as_f64().unwrap(),
        })
    }
}
pub(crate) fn snapshot(row: &Value) -> Metadata<Observed<Snapshot>> {
    let mut errors = HashMap::new();
    for o in row["observations"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|o| o["ok"] == false)
    {
        let method = observation_method(o["api"].as_str().unwrap())
            .expect("supported error observation API required");
        let key = (method.into(), o["path"].as_str().unwrap().into());
        let code = o["code"].as_str().unwrap().to_string();
        if let Some(previous) = errors.insert(key, code.clone()) {
            assert_eq!(previous, code, "no stage race in these retained cases");
        }
    }
    Metadata {
        home: row["home"].as_str().unwrap().into(),
        cwd: row["cwd"].as_str().unwrap().into(),
        files: Observed {
            inner: Snapshot {
                facts: row["files"].clone(),
                errors,
            },
            requests: Requests::default(),
        },
    }
}
fn compare<F: Files>(row: &Value, metadata: &Metadata<Observed<F>>) {
    let request = dispatch::parse(row["request"].as_str().unwrap().as_bytes()).unwrap();
    let Route::LocalResponse(actual) = dispatch::route_with_metadata(&request, Some(metadata))
    else {
        panic!("describe_project must be local")
    };
    metadata.files.requests.assert_target(row);
    assert_eq!(
        actual,
        row["response"].as_str().unwrap().as_bytes(),
        "{}",
        row["id"]
    );
}
#[test]
fn metadata_io_full_rpc_and_time_fields_match_real_default_node_chain() {
    let fixture = fixture();
    for row in fixture["cases"].as_array().unwrap() {
        compare(row, &snapshot(row));
    }
}
#[test]
fn metadata_file_io_full_rpc_and_paths_match_real_node_chain() {
    let fixture = file_fixture();
    assert!(assert_json_number_bits(FILE_SOURCE) > 0);
    for row in fixture["cases"].as_array().unwrap() {
        compare(row, &snapshot(row));
    }
}
#[test]
fn metadata_file_io_completeness_rejects_missing_cases_and_file_proofs() {
    let fixture = file_fixture();
    assert!(
        !complete(&fixture),
        "file cases cannot replace the directory suite"
    );
    assert!(!complete_cases(&self::fixture(), &FILE_IDS));
    for replacement in [
        vec![],
        fixture["cases"].as_array().unwrap()[..2].to_vec(),
        vec![fixture["cases"][0].clone(); 3],
    ] {
        let mut broken = fixture.clone();
        broken["cases"] = replacement.into();
        assert!(!complete_cases(&broken, &FILE_IDS));
    }
    for index in 0..FILE_IDS.len() {
        for axis in [
            "status", "target", "method", "listing", "proof", "caller", "errno", "expected",
            "facts", "mutation", "result",
        ] {
            let mut broken = fixture.clone();
            let row = &mut broken["cases"][index];
            let target = row["errorTarget"].clone();
            let api = if index == 2 { "fsp.stat" } else { "fsp.open" };
            match axis {
                "status" => row["status"] = "NOT_RUN".into(),
                "target" => row["errorTarget"] = row["chats"].clone(),
                "method" | "result" => {
                    let field = if axis == "method" {
                        "request"
                    } else {
                        "response"
                    };
                    let mut wire: Value =
                        serde_json::from_str(row[field].as_str().unwrap()).unwrap();
                    if axis == "method" {
                        wire["method"] = "list_projects".into();
                    } else {
                        wire["result"]["sessionCount"] = 99.into();
                    }
                    row[field] = wire.to_string().into();
                }
                "listing" => {
                    let chats = row["chats"].clone();
                    let listed = row["observations"]
                        .as_array_mut()
                        .unwrap()
                        .iter_mut()
                        .find(|o| o["api"] == "fsp.readdir" && o["path"] == chats)
                        .unwrap();
                    listed["result"] = serde_json::json!([]);
                }
                "proof" => row["observations"]
                    .as_array_mut()
                    .unwrap()
                    .retain(|o| !(o["api"] == api && o["path"] == target)),
                "caller" | "errno" => {
                    let proof = row["observations"]
                        .as_array_mut()
                        .unwrap()
                        .iter_mut()
                        .find(|o| o["api"] == api && o["path"] == target)
                        .unwrap();
                    if axis == "caller" {
                        proof["caller"] = "unrelatedCaller".into();
                    } else if index == 0 {
                        proof["ok"] = false.into();
                        proof["code"] = "EACCES".into();
                    } else {
                        proof["code"] = "ELOOP".into();
                    }
                }
                "expected" => row["expectedError"] = "ELOOP".into(),
                "facts" => row["files"][target.as_str().unwrap()]["size"] = 0.into(),
                "mutation" => {
                    row["mutations"] = if index == 2 {
                        serde_json::json!([])
                    } else {
                        serde_json::json!([{}])
                    }
                }
                _ => unreachable!(),
            }
            assert!(
                !complete_cases(&broken, &FILE_IDS),
                "{} {axis}",
                FILE_IDS[index]
            );
        }
    }
    let mut wrong_unlink = fixture;
    wrong_unlink["cases"][2]["mutations"][0]["path"] = "/another-file.jsonl".into();
    assert!(!complete_cases(&wrong_unlink, &FILE_IDS));
}
#[test]
fn metadata_numeric_recorded_decimal_matches_node_bits() {
    let raw = "1792123157858.0955";
    let standard: f64 = raw.parse().unwrap();
    assert_eq!(standard.to_bits(), 0x427a142dd3d62187);
    let parsed: Value = serde_json::from_str(raw).unwrap();
    assert_eq!(
        parsed.as_f64().unwrap().to_bits(),
        standard.to_bits(),
        "recorded Node decimal must retain its double bits"
    );
}
fn assert_json_number_bits(source: &str) -> usize {
    use serde_json::value::RawValue;
    fn walk(raw: &RawValue) -> usize {
        let text = raw.get();
        match text.as_bytes()[0] {
            b'{' => serde_json::from_str::<HashMap<String, Box<RawValue>>>(text)
                .unwrap()
                .into_iter()
                .map(|(key, value)| {
                    if matches!(key.as_str(), "request" | "response") {
                        let wire: String = serde_json::from_str(value.get()).unwrap();
                        assert_json_number_bits(&wire)
                    } else {
                        walk(&value)
                    }
                })
                .sum(),
            b'[' => serde_json::from_str::<Vec<Box<RawValue>>>(text)
                .unwrap()
                .iter()
                .map(|value| walk(value))
                .sum(),
            b'-' | b'0'..=b'9' => {
                let standard: f64 = text.parse().unwrap();
                let parsed: Value = serde_json::from_str(text).unwrap();
                assert_eq!(
                    parsed.as_f64().unwrap().to_bits(),
                    standard.to_bits(),
                    "JSON number {text}"
                );
                1
            }
            _ => 0,
        }
    }
    let raw: Box<RawValue> = serde_json::from_str(source).unwrap();
    walk(&raw)
}
#[test]
fn metadata_numeric_static_tokens_preserve_standard_double_bits() {
    let count = assert_json_number_bits(SOURCE);
    assert_eq!(count, 384, "fixed original Node recording numeric coverage");
    println!("PDRS_NUMERIC_STATIC_TOKENS {count}");
}
#[test]
fn metadata_numeric_walker_covers_objects_arrays_and_embedded_wire_numbers() {
    let source = r#"{"object":{"n":0.25},"array":[1,{"n":2},[3]],"request":"{\"n\":4}","response":"[5,{\"n\":6}]"}"#;
    assert_eq!(assert_json_number_bits(source), 7);
}
#[test]
#[ignore = "requires explicit recorded Node oracle; full Snapshot RPC replay"]
fn metadata_numeric_dynamic_snapshot_full_rpc_matches_node() {
    let path =
        std::env::var("PDRS_METADATA_IO_LIVE").expect("explicit recorded Node oracle required");
    let source = std::fs::read_to_string(path).unwrap();
    let count = assert_json_number_bits(&source);
    assert!(count > 0);
    // Exact coverage for these immutable recordings; future recordings may have
    // a different shape and are covered by the independent traversal canary.
    if matches!(
        format!("{:x}", Sha256::digest(source.as_bytes())).as_str(),
        "17b9b53525d13635757bdccd2eb925c134c2b3591f84d42c5d75bd83efcb9846"
            | "ddba8da8635783db8862f878989dbfab3eecc66799b7910729ba080ba870b552"
    ) {
        assert_eq!(count, 386, "known dynamic recording numeric coverage");
    }
    println!("PDRS_NUMERIC_DYNAMIC_TOKENS {count}");
    let fixture: Value = serde_json::from_str(&source).unwrap();
    assert!(complete(&fixture));
    let rows = fixture["cases"].as_array().unwrap();
    assert_eq!(rows.len(), 10);
    for row in rows {
        compare(row, &snapshot(row));
        println!("PDRS_NUMERIC_SNAPSHOT_OK {}", row["id"].as_str().unwrap());
    }
}
fn malformed_observation_ok_cases() -> Vec<Option<Value>> {
    vec![
        None,
        Some(Value::Null),
        Some("true".into()),
        Some(0.into()),
        Some(Value::Array(vec![])),
        Some(serde_json::json!({})),
    ]
}
fn fixture_with_observation_ok(ok: Option<Value>) -> Value {
    let mut fixture = fixture();
    let row = &mut fixture["cases"][0];
    let mut observation =
        serde_json::json!({"api":"fs.existsSync","path":row["chats"],"ok":true,"result":true});
    match ok {
        Some(value) => observation["ok"] = value,
        None => {
            observation.as_object_mut().unwrap().remove("ok");
        }
    }
    row["observations"]
        .as_array_mut()
        .unwrap()
        .push(observation);
    fixture
}
#[test]
fn metadata_observation_ok_complete_requires_boolean() {
    for ok in malformed_observation_ok_cases() {
        assert!(
            !complete(&fixture_with_observation_ok(ok)),
            "observation ok must be boolean"
        );
    }
}
#[test]
fn metadata_observation_ok_consumer_requires_boolean() {
    for ok in malformed_observation_ok_cases() {
        let fixture = fixture_with_observation_ok(ok);
        let row = &fixture["cases"][0];
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            compare(row, &snapshot(row))
        }))
        .unwrap_err();
        assert!(
            panic
                .downcast_ref::<String>()
                .unwrap()
                .contains("observation ok must be boolean")
        );
    }
}
#[test]
fn metadata_success_api_validator_accepts_producer_closure_and_rejects_unknown() {
    for api in [
        "fs.statSync",
        "fs.readdirSync",
        "fs.readFileSync",
        "fs.existsSync",
        "fsp.stat",
        "fsp.readdir",
        "fsp.readFile",
        "fsp.open",
        "FileHandle.stat",
        "FileHandle.read",
        "FileHandle.readFile",
    ] {
        assert!(success_observation_api(api), "{api}");
    }
    let fixture = fixture();
    for api in [
        "fs.existsSync",
        "FileHandle.stat",
        "FileHandle.read",
        "FileHandle.readFile",
        "fsp.unlink",
    ] {
        let mut changed = fixture.clone();
        let row = &mut changed["cases"][0];
        let path = row["chats"].clone();
        row["observations"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::json!({"api":api,"path":path,"ok":true,"result":null}));
        if api == "fsp.unlink" {
            assert!(!complete(&changed));
            let row = &changed["cases"][0];
            let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                compare(row, &snapshot(row))
            }))
            .unwrap_err();
            assert!(
                panic
                    .downcast_ref::<String>()
                    .unwrap()
                    .contains("unsupported success observation API")
            );
        } else {
            assert!(complete(&changed), "{api}");
            compare(&changed["cases"][0], &snapshot(&changed["cases"][0]));
        }
    }
}
#[test]
#[ignore = "requires explicit recorded Node oracle; validator only"]
fn metadata_success_api_recorded_oracle_validation() {
    let fixture = live_fixture();
    let rows = fixture["cases"].as_array().unwrap();
    assert_eq!(rows.len(), 10);
    for row in rows {
        assert!(
            row["observations"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|o| o["ok"] == true)
                .all(|o| o["api"].as_str().is_some_and(success_observation_api))
        );
        println!("PDRS_SUCCESS_API_ROW_OK {}", row["id"].as_str().unwrap());
    }
}
#[test]
fn static_fixture_anchor_rejects_rust_rewritten_facts_and_response() {
    let original = fixture();
    let mut forged = original.clone();
    let row = &mut forged["cases"][0];
    let storage = row["legacy"].as_str().unwrap().to_owned();
    row["files"][&storage]["birth_ms"] =
        (row["files"][&storage]["birth_ms"].as_f64().unwrap() + 1000.0).into();
    let metadata = snapshot(row);
    let request = dispatch::parse(row["request"].as_str().unwrap().as_bytes()).unwrap();
    let Route::LocalResponse(actual) = dispatch::route_with_metadata(&request, Some(&metadata))
    else {
        panic!("local required")
    };
    row["response"] = String::from_utf8(actual).unwrap().into();
    assert!(complete(&forged));
    compare(&forged["cases"][0], &snapshot(&forged["cases"][0]));
    for key in [
        "baselineCommit",
        "node",
        "icu",
        "platform",
        "sources",
        "owner",
        "scratch",
        "permissions",
        "callPath",
    ] {
        assert_eq!(forged[key], original[key]);
    }
    assert_ne!(
        forged["cases"][0]["response"],
        original["cases"][0]["response"]
    );
    assert!(std::panic::catch_unwind(|| recorded_fixture(&forged.to_string())).is_err());
    assert!(std::panic::catch_unwind(|| recorded_fixture(&format!("{SOURCE} "))).is_err());
}
#[test]
fn metadata_path_compare_requires_the_production_audit_call() {
    let mut row = fixture()["cases"][0].clone();
    row["errorTarget"] = "/not-the-production-target".into();
    let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        compare(&row, &snapshot(&row))
    }))
    .unwrap_err();
    assert!(
        panic
            .downcast_ref::<String>()
            .unwrap()
            .contains("production must request")
    );
}
#[test]
fn static_fixture_completeness_rejects_each_named_field_and_proof_axis() {
    let fixture = fixture();
    let mut wrong = fixture.clone();
    let row = &mut wrong["cases"][0];
    let mut request: Value = serde_json::from_str(row["request"].as_str().unwrap()).unwrap();
    request["params"]["projectKey"] = "/wrong-project".into();
    row["request"] = request.to_string().into();
    assert!(!complete(&wrong));
    for (field, value) in [
        ("id", Value::from("wrong-id")),
        ("ok", false.into()),
        ("sessionCount", 0.into()),
        ("lastActivity", Value::Null),
        ("createdAt", Value::Null),
    ] {
        let mut wrong = fixture.clone();
        let row = &mut wrong["cases"][0];
        let mut response: Value = serde_json::from_str(row["response"].as_str().unwrap()).unwrap();
        if matches!(field, "id" | "ok") {
            response[field] = value;
        } else {
            response["result"][field] = value;
        }
        row["response"] = response.to_string().into();
        assert!(!complete(&wrong), "{field}");
    }
    for field in ["caller", "path"] {
        let mut wrong = fixture.clone();
        let row = wrong["cases"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|r| r["id"] == "chats-loop")
            .unwrap();
        for observation in row["observations"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .filter(|o| o["ok"] == false)
        {
            observation[field] = "unrecorded".into();
        }
        assert!(!complete(&wrong), "proof {field}");
    }
    for (id, api) in [
        ("projects-notdir", "fsp.readdir"),
        ("projects-notdir", "fs.readdirSync"),
        ("projects-denied-legacy", "fsp.readdir"),
        ("projects-denied-legacy", "fs.readdirSync"),
    ] {
        let mut wrong = fixture.clone();
        let row = wrong["cases"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|r| r["id"] == id)
            .unwrap();
        row["observations"]
            .as_array_mut()
            .unwrap()
            .retain(|o| o["api"] != api);
        assert!(!complete(&wrong), "{id} missing {api}");
    }
    let mut unknown_api = fixture;
    let row = &mut unknown_api["cases"][0];
    let chats = row["chats"].clone();
    row["observations"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({"api":"fsp.unlink","path":chats,"ok":false,"code":"ENOENT"}));
    assert!(!complete(&unknown_api));
    assert!(std::panic::catch_unwind(|| snapshot(&unknown_api["cases"][0])).is_err());
}
#[test]
fn snapshot_rejects_unregistered_paths_and_api_pairs_without_swallowable_errors() {
    let fixture = fixture();
    let row = &fixture["cases"][0];
    let metadata = snapshot(row);
    let unknown = metadata.home.join("unrecorded");
    for method in ["entries", "read", "stat"] {
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = metadata.files.inner.error(method, &unknown);
        }))
        .unwrap_err();
        let message = panic.downcast_ref::<String>().unwrap();
        assert!(message.contains("unrecorded filesystem path"), "{message}");
    }
    let directory = Path::new(row["chats"].as_str().unwrap());
    for method in ["read", "unlink"] {
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = metadata.files.inner.error(method, directory);
        }))
        .unwrap_err();
        let message = panic.downcast_ref::<String>().unwrap();
        assert!(message.contains("unrecorded filesystem API"), "{message}");
    }
}
#[test]
fn snapshot_path_audit_rejects_unused_wrong_target_and_errno_even_for_empty_rpc() {
    struct WrongDirectory {
        inner: Snapshot,
        target: PathBuf,
    }
    impl Files for WrongDirectory {
        fn entries(&self, path: &Path) -> io::Result<Vec<(String, bool)>> {
            let typo = self.target.join("typo");
            self.inner
                .entries(if path == self.target { &typo } else { path })
        }
        fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
            self.inner.read(path)
        }
        fn stat(&self, path: &Path) -> io::Result<Stat> {
            self.inner.stat(path)
        }
    }
    let fixture = fixture();
    for row in fixture["cases"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["id"] == "projects-missing" || row["id"] == "chats-missing")
    {
        let target = Path::new(row["errorTarget"].as_str().unwrap());
        for wrong in [None, Some(target.join("typo")), Some(target.into())] {
            let requests = Requests::default();
            if let Some(path) = wrong {
                let error = if path == target {
                    io::Error::other("not a recorded errno")
                } else {
                    errno("ENOENT")
                };
                requests.entries(&path, &Err(error));
            }
            assert!(std::panic::catch_unwind(|| requests.assert_target(row)).is_err());
        }
        let original = snapshot(row);
        let wrong = Metadata {
            home: original.home,
            cwd: original.cwd,
            files: Observed {
                inner: WrongDirectory {
                    inner: original.files.inner,
                    target: target.into(),
                },
                requests: original.files.requests,
            },
        };
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| compare(row, &wrong)))
            .unwrap_err();
        assert!(
            panic
                .downcast_ref::<String>()
                .unwrap()
                .contains("unrecorded filesystem path")
        );
        compare(row, &snapshot(row));
    }
}
#[test]
fn metadata_io_completeness_rejects_missing_cases_method_and_each_helper_error() {
    let fixture = fixture();
    for replacement in [
        vec![],
        fixture["cases"].as_array().unwrap()[..9].to_vec(),
        vec![fixture["cases"][0].clone(); 10],
    ] {
        let mut broken = fixture.clone();
        broken["cases"] = replacement.into();
        assert!(!complete(&broken));
    }
    let mut broken = fixture.clone();
    let mut request: Value =
        serde_json::from_str(broken["cases"][0]["request"].as_str().unwrap()).unwrap();
    request["method"] = "list_projects".into();
    broken["cases"][0]["request"] = request.to_string().into();
    assert!(!complete(&broken));
    for (id, api) in [
        ("projects-denied-legacy", "fsp.readdir"),
        ("projects-denied-marker", "fs.readdirSync"),
        ("chats-loop", "fsp.readdir"),
    ] {
        let mut broken = fixture.clone();
        let row = broken["cases"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|r| r["id"] == id)
            .unwrap();
        row["observations"]
            .as_array_mut()
            .unwrap()
            .retain(|o| o["api"] != api);
        assert!(!complete(&broken), "{id} {api}");
    }
    let mut missing_legacy_stat = fixture.clone();
    let row = missing_legacy_stat["cases"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|r| r["id"] == "projects-denied-legacy")
        .unwrap();
    let legacy_path = row["legacy"].clone();
    row["observations"].as_array_mut().unwrap().retain(|o| {
        !(o["api"] == "fsp.stat"
            && o["path"] == legacy_path
            && o["ok"] == true
            && o["result"]["directory"] == true)
    });
    assert!(!complete(&missing_legacy_stat));
    let mut wrong_expected = fixture.clone();
    let row = wrong_expected["cases"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|r| r["id"] == "chats-loop")
        .unwrap();
    row["expectedError"] = "EACCES".into();
    assert!(
        !complete(&wrong_expected),
        "expectedError must be independently anchored"
    );
    let mut wrong_code = fixture.clone();
    let row = wrong_code["cases"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|r| r["id"] == "chats-loop")
        .unwrap();
    row["expectedError"] = "EACCES".into();
    for o in row["observations"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .filter(|o| o["ok"] == false)
    {
        o["code"] = "EACCES".into();
    }
    assert!(!complete(&wrong_code));
    let mut wrong_observation = fixture.clone();
    let row = wrong_observation["cases"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|r| r["id"] == "chats-loop")
        .unwrap();
    for observation in row["observations"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .filter(|o| o["ok"] == false)
    {
        observation["code"] = "EACCES".into();
    }
    assert!(
        !complete(&wrong_observation),
        "observation errno must be independently anchored"
    );
    let mut wrong_target = fixture.clone();
    wrong_target["cases"][0]["errorTarget"] = "/unrecorded-target".into();
    assert!(!complete(&wrong_target));
    let mut broken = fixture;
    broken["cases"][0]["status"] = "NOT_RUN".into();
    assert!(!complete(&broken));
}
pub(crate) fn live_fixture() -> Value {
    let path =
        std::env::var("PDRS_METADATA_IO_LIVE").expect("explicit retained live fixture required");
    let fixture: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    assert!(complete(&fixture));
    fixture
}
pub(crate) fn native(row: &Value) -> Metadata<Observed<NativeFiles>> {
    Metadata {
        home: row["home"].as_str().unwrap().into(),
        cwd: row["cwd"].as_str().unwrap().into(),
        files: Observed {
            inner: NativeFiles,
            requests: Requests::default(),
        },
    }
}
#[test]
#[ignore = "requires freshly retained owned Node fixture, then restore permissions"]
fn metadata_io_native_retained_files_match_full_node_rpc() {
    let fixture = live_fixture();
    let rows = fixture["cases"].as_array().unwrap();
    for row in rows {
        let metadata = native(row);
        compare(row, &metadata);
        println!("PDRS_NATIVE_METADATA_OK {}", row["id"].as_str().unwrap());
    }
    assert_eq!(rows.len(), 10);
}
