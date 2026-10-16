use super::*;
use crate::dispatch::{self, Route};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

const SOURCE: &str = include_str!("../../tests/fixtures/metadata/describe-io-errors.json");
// Original Node recording bytes; provenance is indexed beside the fixture docs.
const SOURCE_SHA256: &str = "995400bce07ef1bee5e4e0d6d3766343ae0229e21eac496394b6866e68518c7c";
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
        _ => None,
    }
}
fn complete(fixture: &Value) -> bool {
    let Some(rows) = fixture["cases"].as_array() else {
        return false;
    };
    if rows.len() != 10
        || rows
            .iter()
            .map(|r| r["id"].as_str().unwrap_or(""))
            .collect::<BTreeSet<_>>()
            != IDS.into_iter().collect()
    {
        return false;
    }
    rows.iter().all(|row| {
        let Some(observations) = row["observations"].as_array() else {
            return false;
        };
        if observations
            .iter()
            .filter(|o| o["ok"] == false)
            .any(|o| o["api"].as_str().and_then(observation_method).is_none())
        {
            return false;
        }
        let Some(id) = row["id"].as_str() else {
            return false;
        };
        let expected_code = match id {
            "projects-missing" | "chats-missing" => Some("ENOENT"),
            "projects-notdir" | "chats-notdir" => Some("ENOTDIR"),
            "projects-denied-legacy" | "projects-denied-marker" | "chats-denied" => Some("EACCES"),
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
            "positive" | "collision-positive" | "projects-denied-legacy" | "projects-denied-marker"
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
        let error_target = if id.starts_with("projects-") {
            projects
        } else {
            chats
        };
        if row["errorTarget"].as_str() != Some(error_target) {
            return false;
        }
        match id {
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
        let result = row["expectedError"]
            .as_str()
            .map_or(Ok(()), |code| Err(errno(code).raw_os_error()));
        assert!(
            self.0.lock().unwrap().contains(&EntryRequest {
                method: "entries",
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
                let api = o["api"].as_str().unwrap();
                let method = observation_method(api).or_else(|| {
                    (api == "fsp.open" && o["ok"] == true).then_some("session_summary")
                })?;
                Some(EntryRequest {
                    method,
                    path: o["path"].as_str().unwrap().into(),
                    result: if o["ok"] == true {
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
