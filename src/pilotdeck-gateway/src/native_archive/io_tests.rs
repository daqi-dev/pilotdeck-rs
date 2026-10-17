use super::tests::{Snapshot, metadata_from_data};
use super::*;
use crate::{
    dispatch::{Route, parse, route_with_metadata},
    metadata::Stat,
};
use serde_json::Value;
use std::path::PathBuf;

const KINDS: [(&str, &str, i32); 6] = [
    ("missing", "ENOENT", libc::ENOENT),
    ("directory", "EISDIR", libc::EISDIR),
    ("loop", "ELOOP", libc::ELOOP),
    ("notdir", "ENOTDIR", libc::ENOTDIR),
    ("denied", "EACCES", libc::EACCES),
    ("long", "ENAMETOOLONG", libc::ENAMETOOLONG),
];
fn validate(data: &Value) -> std::result::Result<(), String> {
    let cases = data["cases"].as_array().ok_or("cases")?;
    if cases.len() != 12 {
        return Err("exactly twelve cases".into());
    }
    let mut ids = std::collections::HashSet::new();
    for method in ["native_archive_manifest", "native_archive_entries"] {
        for (kind, code, _) in KINDS {
            let id = format!("{kind}-{method}");
            let row = cases.iter().find(|r| r["id"] == id).ok_or(&id)?;
            if !ids.insert(id.clone()) || row["status"] != "RUN" || row["method"] != method {
                return Err(id);
            }
            let request: Value = serde_json::from_str(row["request"].as_str().ok_or("request")?)
                .map_err(|e| e.to_string())?;
            if request["id"] != id || request["method"] != method {
                return Err(id);
            }
            if !row["observations"]
                .as_array()
                .ok_or("observations")?
                .iter()
                .any(|o| {
                    o["code"] == code
                        && o["path"] == row["target"]
                        && o["api"]
                            == if matches!(kind, "directory" | "denied") {
                                "readFile"
                            } else {
                                "stat"
                            }
                })
            {
                return Err(id);
            }
        }
    }
    Ok(())
}
pub(crate) fn fixture() -> Value {
    super::tests::assert_fixture_hash(
        include_bytes!("../../tests/fixtures/native-archive/io-errors.json"),
        "32b7774bba73d5c6d17bfd4686053c0dc6d6d20af305b0181d6e84deda04f84d",
    );
    let data: Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/native-archive/io-errors.json"
    ))
    .unwrap();
    validate(&data).unwrap();
    data
}
pub(crate) struct IoFiles {
    snapshot: Snapshot,
    target: PathBuf,
    errno: i32,
    stat_error: bool,
}
impl Files for IoFiles {
    fn entries(&self, p: &Path) -> io::Result<Vec<(String, bool)>> {
        self.snapshot.entries(p)
    }
    fn read(&self, p: &Path) -> io::Result<Vec<u8>> {
        self.snapshot.read(p)
    }
    fn stat(&self, p: &Path) -> io::Result<Stat> {
        self.snapshot.stat(p)
    }
    fn canonical(&self, p: &Path) -> io::Result<PathBuf> {
        if p == self.target && self.stat_error {
            Err(io::Error::from_raw_os_error(self.errno))
        } else {
            self.snapshot.canonical(p)
        }
    }
    fn project_stat(&self, r: &Path, p: &Path) -> io::Result<(Stat, bool)> {
        self.snapshot.project_stat(r, p)
    }
    fn archive_read(&self, root: &Path, p: &Path, limit: usize) -> io::Result<Vec<u8>> {
        if p != self.target {
            return self.snapshot.archive_read(root, p, limit);
        }
        Err(io::Error::from_raw_os_error(self.errno))
    }
}
pub(crate) fn metadata(data: &Value, row: &Value) -> Metadata<IoFiles> {
    let home = data["home"].as_str().unwrap();
    let cwd = data["cwd"].as_str().unwrap();
    let target = row["target"].as_str().unwrap();
    let mut files = serde_json::Map::new();
    for path in [
        home.to_owned(),
        cwd.to_owned(),
        format!("{home}/projects/registered"),
        format!("{home}/projects/registered/chats"),
    ] {
        files.insert(
            path,
            serde_json::json!({"directory":true,"size":0,"entries":[]}),
        );
    }
    files.insert(
        format!("{home}/projects"),
        serde_json::json!({"directory":true,"size":0,"entries":[["registered",true]]}),
    );
    files.insert(
        format!("{home}/projects/registered/.cwd"),
        serde_json::json!({"directory":false,"size":cwd.len(),"base64":STANDARD.encode(cwd)}),
    );
    files.insert(
        target.into(),
        serde_json::json!({"directory":row["kind"]=="directory","size":0,"base64":""}),
    );
    let snapshot = metadata_from_data(&serde_json::json!({"home":home,"cwd":cwd,"files":files}));
    let (_, _, errno) = KINDS
        .iter()
        .find(|(kind, _, _)| row["kind"] == *kind)
        .unwrap();
    Metadata {
        home: snapshot.home,
        cwd: snapshot.cwd,
        files: IoFiles {
            snapshot: snapshot.files,
            target: target.into(),
            errno: *errno,
            stat_error: !matches!(row["kind"].as_str().unwrap(), "denied" | "directory"),
        },
    }
}
#[test]
fn archive_io_twelve_own_rpc_errors_match_and_guard_the_method_gate() {
    let data = fixture();
    let mut count = 0;
    for row in data["cases"].as_array().unwrap() {
        let metadata = metadata(&data, row);
        let request = parse(row["request"].as_str().unwrap().as_bytes()).unwrap();
        let Route::LocalResponse(raw) = route_with_metadata(&request, Some(&metadata)) else {
            panic!("unexpected host {}", row["id"])
        };
        assert_eq!(
            raw,
            row["response"].as_str().unwrap().as_bytes(),
            "{}",
            row["id"]
        );
        count += 1;
    }
    assert_eq!(count, 12);
    for method in ["native_archive_manifest", "native_archive_entries"] {
        let actual: std::collections::BTreeSet<_> = crate::node_io::LOCAL_ERRNOS
            .iter()
            .find(|(m, _)| *m == method)
            .unwrap()
            .1
            .iter()
            .copied()
            .collect();
        assert_eq!(actual, KINDS.iter().map(|(_, code, _)| *code).collect());
        for errno in crate::node_io::unmeasured_errnos(method) {
            assert_eq!(
                fs_error(
                    io::Error::from_raw_os_error(errno),
                    method,
                    "stat",
                    Path::new("/fixture")
                )
                .0,
                "TS_IO_REQUIRED"
            );
        }
    }
}
#[test]
fn archive_unmeasured_invalid_input_still_requires_ts() {
    for method in ["native_archive_manifest", "native_archive_entries"] {
        for error in [
            io::Error::from(io::ErrorKind::InvalidInput),
            io::Error::from_raw_os_error(libc::EINVAL),
        ] {
            assert_eq!(
                secure_fs_error(error, method, "open", Path::new("/fixture")).0,
                "TS_IO_REQUIRED"
            );
        }
    }
}
#[test]
fn archive_io_missing_duplicate_wrong_method_or_absent_errno_is_never_pass() {
    let data = fixture();
    for variant in 0..5 {
        let mut broken = data.clone();
        match variant {
            0 => {
                broken["cases"].as_array_mut().unwrap().pop();
            }
            1 => {
                broken["cases"][1] = broken["cases"][0].clone();
            }
            2 => broken["cases"][0]["method"] = "session_model_get".into(),
            3 => broken["cases"][0]["observations"] = serde_json::json!([]),
            4 => broken["cases"][0]["status"] = "NOT_RUN".into(),
            _ => unreachable!(),
        }
        assert!(validate(&broken).is_err(), "variant {variant}");
    }
}
