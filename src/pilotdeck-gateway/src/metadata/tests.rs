use super::*;
use crate::dispatch::{self, Route};
use serde_json::Value;
use std::collections::{BTreeSet, HashMap};

const SOURCE: &str = include_str!("../../tests/fixtures/metadata/describe-io-errors.json");
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
    let fixture: Value = serde_json::from_str(SOURCE).unwrap();
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
        self.facts
            .get(path.to_str().unwrap())
            .ok_or_else(|| errno("ENOENT"))
    }
    fn error(&self, method: &str, path: &Path) -> io::Result<()> {
        match self
            .errors
            .get(&(method.into(), path.to_str().unwrap().into()))
        {
            Some(code) => Err(errno(code)),
            None => Ok(()),
        }
    }
}
impl Files for Snapshot {
    fn entries(&self, path: &Path) -> io::Result<Vec<(String, bool)>> {
        self.error("entries", path)?;
        let fact = self.fact(path)?;
        if let Some(code) = fact["entriesError"].as_str().or(fact["statError"].as_str()) {
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
        if let Some(code) = fact["readError"].as_str().or(fact["statError"].as_str()) {
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
pub(crate) fn snapshot(row: &Value) -> Metadata<Snapshot> {
    let mut errors = HashMap::new();
    for o in row["observations"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|o| o["ok"] == false)
    {
        let method = match o["api"].as_str().unwrap() {
            "fsp.readdir" | "fs.readdirSync" => "entries",
            "fsp.stat" | "fs.statSync" => "stat",
            "fsp.readFile" | "fs.readFileSync" => "read",
            _ => continue,
        };
        let key = (method.into(), o["path"].as_str().unwrap().into());
        let code = o["code"].as_str().unwrap().to_string();
        if let Some(previous) = errors.insert(key, code.clone()) {
            assert_eq!(previous, code, "no stage race in these retained cases");
        }
    }
    Metadata {
        home: row["home"].as_str().unwrap().into(),
        cwd: row["cwd"].as_str().unwrap().into(),
        files: Snapshot {
            facts: row["files"].clone(),
            errors,
        },
    }
}
fn compare<F: Files>(row: &Value, metadata: &Metadata<F>) {
    let request = dispatch::parse(row["request"].as_str().unwrap().as_bytes()).unwrap();
    let Route::LocalResponse(actual) = dispatch::route_with_metadata(&request, Some(metadata))
    else {
        panic!("describe_project must be local")
    };
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
pub(crate) fn native(row: &Value) -> Metadata<NativeFiles> {
    Metadata {
        home: row["home"].as_str().unwrap().into(),
        cwd: row["cwd"].as_str().unwrap().into(),
        files: NativeFiles,
    }
}
#[test]
#[ignore = "requires freshly retained owned Node fixture, then restore permissions"]
fn metadata_io_native_retained_files_match_full_node_rpc() {
    let fixture = live_fixture();
    let rows = fixture["cases"].as_array().unwrap();
    for row in rows {
        let metadata = native(row);
        if let Some(code) = row["expectedError"].as_str() {
            let error = metadata
                .files
                .entries(Path::new(row["errorTarget"].as_str().unwrap()))
                .unwrap_err();
            assert_eq!(
                crate::node_io::lookup(&error).unwrap().0,
                code,
                "{} actual errno",
                row["id"]
            );
        }
        compare(row, &metadata);
        println!("PDRS_NATIVE_METADATA_OK {}", row["id"].as_str().unwrap());
    }
    assert_eq!(rows.len(), 10);
}
