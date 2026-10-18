use super::*;
use crate::{
    dispatch::{Route, parse, route_with_metadata},
    metadata::{NativeFiles, Stat},
};
use serde_json::Value;
use std::{
    collections::HashMap,
    fs,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering as AtomicOrdering},
};

pub(crate) struct Snapshot {
    pub(super) files: HashMap<PathBuf, Value>,
}
impl Snapshot {
    fn fact(&self, path: &Path) -> io::Result<&Value> {
        self.files
            .get(path)
            .ok_or_else(|| io::Error::from_raw_os_error(libc::ENOENT))
    }
}
impl Files for Snapshot {
    fn entries(&self, path: &Path) -> io::Result<Vec<(String, bool)>> {
        let mut entries: Vec<(String, bool)> =
            serde_json::from_value(self.fact(path)?["entries"].clone())
                .map_err(io::Error::other)?;
        entries.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
        Ok(entries)
    }
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        STANDARD
            .decode(
                self.fact(path)?["base64"]
                    .as_str()
                    .ok_or(io::ErrorKind::IsADirectory)?,
            )
            .map_err(io::Error::other)
    }
    fn stat(&self, path: &Path) -> io::Result<Stat> {
        let f = self.fact(path)?;
        Ok(Stat {
            directory: f["directory"].as_bool().unwrap(),
            size: f["size"].as_u64().unwrap(),
            birth_ms: 0.0,
            ctime_ms: 0.0,
            mtime_ms: 0.0,
        })
    }
    fn canonical(&self, path: &Path) -> io::Result<PathBuf> {
        self.fact(path)?;
        Ok(path.into())
    }
    fn project_stat(&self, _root: &Path, path: &Path) -> io::Result<(Stat, bool)> {
        let s = self.stat(path)?;
        let file = !s.directory;
        Ok((s, file))
    }
    fn project_read(&self, _root: &Path, path: &Path, limit: usize) -> io::Result<Vec<u8>> {
        let mut b = self.read(path)?;
        b.truncate(limit);
        Ok(b)
    }
}
pub(super) fn assert_fixture_hash(bytes: &[u8], expected: &str) {
    use sha2::{Digest, Sha256};
    assert_eq!(
        format!("{:x}", Sha256::digest(bytes)),
        expected,
        "recorded RPC original changed; review new source evidence before re-admission"
    );
}
pub(crate) fn fixture() -> (Metadata<Snapshot>, Value) {
    assert_fixture_hash(
        include_bytes!("../../tests/fixtures/native-archive/cases.json"),
        "60e878bb9f5433424129822309d1783207cda8c65cbd65d951eabd3a472adef7",
    );
    let data: Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/native-archive/cases.json"
    ))
    .unwrap();
    (metadata_from_data(&data), data)
}
pub(crate) fn metadata_from_data(data: &Value) -> Metadata<Snapshot> {
    let files = data["files"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(p, v)| (PathBuf::from(p), v.clone()))
        .collect();
    Metadata {
        home: PathBuf::from(data["home"].as_str().unwrap()),
        cwd: PathBuf::from(data["cwd"].as_str().unwrap()),
        files: Snapshot { files },
    }
}
pub(crate) fn host_fixture() -> (Metadata<Snapshot>, Value) {
    assert_fixture_hash(
        include_bytes!("../../tests/fixtures/native-archive/host-boundary.json"),
        "2703b8fd3d2944fbb595f97e7268e88726ac732acc74bd9cbae94032ba4fe5bc",
    );
    let data: Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/native-archive/host-boundary.json"
    ))
    .unwrap();
    assert_eq!(data["cases"].as_array().unwrap().len(), 11);
    let ids: std::collections::HashSet<_> = data["cases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|case| case["id"].as_str().filter(|id| !id.is_empty()).unwrap())
        .collect();
    assert_eq!(ids.len(), 11);
    (metadata_from_data(&data), data)
}
pub(crate) fn host_required(case: &Value) -> bool {
    let id = case["id"].as_str().unwrap();
    id.starts_with("ties-lone-") || id.starts_with("zero-tie-")
}
#[test]
fn archive_host_boundary_only_equal_sequence_lone_surrogates_forward() {
    let (metadata, data) = host_fixture();
    let mut local = 0;
    let mut forwarded = 0;
    for case in data["cases"].as_array().unwrap() {
        let route = route_with_metadata(
            &parse(case["request"].as_str().unwrap().as_bytes()).unwrap(),
            Some(&metadata),
        );
        if host_required(case) {
            assert!(matches!(route, Route::Forward), "{}", case["id"]);
            forwarded += 1
        } else {
            let Route::LocalResponse(raw) = route else {
                panic!("unexpected forward {}", case["id"])
            };
            assert_eq!(
                raw,
                case["response"].as_str().unwrap().as_bytes(),
                "{}",
                case["id"]
            );
            local += 1;
        }
    }
    assert_eq!((local, forwarded), (7, 4));
}
#[test]
fn production_default_archive_wire_goldens() {
    let (metadata, data) = fixture();
    let mut local = 0;

    let cases = data["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 138);
    let ids: std::collections::HashSet<_> = cases
        .iter()
        .map(|case| case["id"].as_str().filter(|id| !id.is_empty()).unwrap())
        .collect();
    assert_eq!(ids.len(), 138);
    for case in data["cases"].as_array().unwrap() {
        let raw = case["request"].as_str().unwrap();
        let request = parse(raw.as_bytes()).unwrap();
        assert!(crate::dispatch::is_local(&request));
        assert!(crate::dispatch::needs_files(&request));
        let route = route_with_metadata(&request, Some(&metadata));
        let Route::LocalResponse(response) = route else {
            panic!("unexpected forward: {}", case["id"])
        };
        assert_eq!(
            String::from_utf8(response).unwrap(),
            case["response"].as_str().unwrap(),
            "{}",
            case["id"]
        );
        local += 1;
    }
    assert_eq!(local, 138);
}
#[test]
fn direct_callback_accepts_minus_one_public_entry_rejects_it() {
    let (metadata, data) = fixture();
    let evidence = &data["callbackEvidence"];
    let input = parse_json(&evidence["input"].to_string()).unwrap();
    let result = callback(
        &metadata,
        "native_archive_entries",
        &input,
        session(&input).unwrap(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        stringify_json(&result).unwrap().unwrap(),
        evidence["wire"].as_str().unwrap()
    );
    assert_eq!(
        validate_entries(&input, true).unwrap_err().0,
        "INVALID_ARCHIVE_CURSOR"
    );
    let null_limit = object([("sessionKey", "fixture".into()), ("limit", JsValue::Null)]);
    assert_eq!(validate_entries(&null_limit, false).unwrap(), 100);
    assert_eq!(
        validate_entries(&null_limit, true).unwrap_err().0,
        "INVALID_ARCHIVE_LIMIT"
    );
}
#[test]
fn deep_artifact_name_array_is_an_error_not_a_stack_overflow() {
    let (metadata, _) = fixture();
    let mut name = JsValue::String("a".into());
    for _ in 0..1_000_000 {
        name = JsValue::Array(vec![name]);
    }
    let request = object([
        ("type", "request".into()),
        ("id", "deep".into()),
        ("method", "native_archive_artifact".into()),
        (
            "params",
            object([("sessionKey", "fixture".into()), ("artifactName", name)]),
        ),
    ]);
    let response = response(&request, &metadata).expect("answered locally");
    let oracle = parse_json(include_str!("../../tests/fixtures/runtime-boundaries.json")).unwrap();
    assert_eq!(
        parse_json(std::str::from_utf8(&response).unwrap()).unwrap(),
        oracle
            .get("wireErrors")
            .unwrap()
            .get("nativeArchive")
            .unwrap()
            .clone(),
    );
}
static NEXT: AtomicUsize = AtomicUsize::new(0);
pub(super) fn scratch() -> PathBuf {
    // Short, exclusively-created POSIX test roots keep Node's 128-unit invalid
    // path diagnostic in the same range as the immutable original fixture.
    // A long caller TMPDIR must not change the expected protocol message.
    for _ in 0..100 {
        let dir = Path::new("/tmp").join(format!(
            "pdrs-archive-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, AtomicOrdering::Relaxed)
        ));
        match fs::create_dir(&dir) {
            Ok(()) => return fs::canonicalize(dir).unwrap(),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => panic!("cannot create owned archive fixture: {error}"),
        }
    }
    panic!("no fresh owned archive fixture name");
}

#[test]
fn native_reads_full_artifact_before_truncation_and_missing_transcript_is_independent() {
    let root = scratch();
    let dir = root.join(".pilotdeck/tool-results/fixture");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("body"), [0, 255, 1, 2, 3]).unwrap();
    let metadata = Metadata {
        home: root.clone(),
        cwd: root.clone(),
        files: NativeFiles,
    };
    let params = object([
        ("sessionKey", "fixture".into()),
        ("artifactName", "body".into()),
        ("maxBytes", JsValue::Number(1.0)),
    ]);
    let result = callback(
        &metadata,
        "native_archive_artifact",
        &params,
        session(&params).unwrap(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(field(&result, "bytes").as_number(), Some(5.0));
    assert_eq!(text_field(&result, "content").as_deref(), Some("AA=="));
    assert_eq!(field(&result, "truncated"), &JsValue::Bool(true));
    fs::remove_dir_all(root).unwrap();
}
#[cfg(unix)]
#[test]
fn archive_symlink_containment_and_raced_replacements_fail_closed() {
    use std::os::unix::fs::symlink;
    let root = scratch();
    let project = root.join("project");
    let home = root.join("home");
    let outside = root.join("outside");
    let dir = project.join(".pilotdeck/tool-results/fixture");
    let chat = home.join("projects/registered/chats");
    fs::create_dir_all(&dir).unwrap();
    fs::create_dir_all(&chat).unwrap();
    fs::write(
        home.join("projects/registered/.cwd"),
        project.to_string_lossy().as_bytes(),
    )
    .unwrap();
    fs::write(&outside, b"secret").unwrap();
    let body=b"{\"type\":\"message\",\"sessionId\":\"s\",\"turnId\":\"t\",\"sequence\":0,\"createdAt\":\"x\"}\n";
    fs::write(chat.join("fixture.jsonl"), body).unwrap();
    fs::write(dir.join("body"), b"local").unwrap();
    symlink(&outside, dir.join("escape")).unwrap();
    symlink(&outside, chat.join("escape.jsonl")).unwrap();
    symlink(dir.join("body"), dir.join("inside")).unwrap();
    let metadata = Metadata {
        home: home.clone(),
        cwd: project.clone(),
        files: NativeFiles,
    };
    let params = |name: &str| {
        object([
            ("sessionKey", "fixture".into()),
            ("artifactName", name.into()),
        ])
    };
    let p = params("escape");
    assert_eq!(
        callback(
            &metadata,
            "native_archive_artifact",
            &p,
            session(&p).unwrap()
        )
        .unwrap_err()
        .0,
        "ARCHIVE_ARTIFACT_NOT_FOUND"
    );
    let p = params("inside");
    assert!(
        callback(
            &metadata,
            "native_archive_artifact",
            &p,
            session(&p).unwrap()
        )
        .is_ok()
    );
    assert_eq!(
        transcript(
            &metadata,
            &project,
            &JsString::from("escape"),
            "native_archive_manifest"
        )
        .unwrap_err()
        .0,
        "PATH_NOT_ALLOWED"
    );
    struct Race {
        target: PathBuf,
        outside: PathBuf,
    }
    impl Files for Race {
        fn entries(&self, p: &Path) -> io::Result<Vec<(String, bool)>> {
            NativeFiles.entries(p)
        }
        fn read(&self, p: &Path) -> io::Result<Vec<u8>> {
            NativeFiles.read(p)
        }
        fn stat(&self, p: &Path) -> io::Result<Stat> {
            NativeFiles.stat(p)
        }
        fn canonical(&self, p: &Path) -> io::Result<PathBuf> {
            NativeFiles.canonical(p)
        }
        fn project_stat(&self, r: &Path, p: &Path) -> io::Result<(Stat, bool)> {
            NativeFiles.project_stat(r, p)
        }
        fn project_read(&self, r: &Path, p: &Path, l: usize) -> io::Result<Vec<u8>> {
            if p == self.target {
                fs::remove_file(p)?;
                symlink(&self.outside, p)?;
            }
            NativeFiles.project_read(r, p, l)
        }
    }
    let raced = Metadata {
        home: home.clone(),
        cwd: project.clone(),
        files: Race {
            target: dir.join("body"),
            outside: outside.clone(),
        },
    };
    let p = params("body");
    assert_eq!(
        callback(&raced, "native_archive_artifact", &p, session(&p).unwrap())
            .unwrap_err()
            .0,
        "ARCHIVE_ARTIFACT_NOT_FOUND"
    );
    let raced = Metadata {
        home: home.clone(),
        cwd: project.clone(),
        files: Race {
            target: chat.join("fixture.jsonl"),
            outside,
        },
    };
    assert_eq!(
        transcript(
            &raced,
            &project,
            &JsString::from("fixture"),
            "native_archive_manifest"
        )
        .unwrap_err()
        .0,
        "PATH_NOT_ALLOWED"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn native_sparse_artifact_over_node_limit_is_not_found_without_allocating_it() {
    let root = scratch();
    let dir = root.join(".pilotdeck/tool-results/fixture");
    fs::create_dir_all(&dir).unwrap();
    let file = fs::File::create(dir.join("huge")).unwrap();
    file.set_len(2_147_483_648).unwrap();
    drop(file);
    let metadata = Metadata {
        home: root.clone(),
        cwd: root.clone(),
        files: NativeFiles,
    };
    let params = object([
        ("sessionKey", "fixture".into()),
        ("artifactName", "huge".into()),
    ]);
    assert_eq!(
        callback(
            &metadata,
            "native_archive_artifact",
            &params,
            session(&params).unwrap()
        )
        .unwrap_err()
        .0,
        "ARCHIVE_ARTIFACT_NOT_FOUND"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn unmeasured_transcript_io_diagnostics_defer_but_artifact_io_is_not_found() {
    struct Fault(Snapshot);
    impl Files for Fault {
        fn entries(&self, p: &Path) -> io::Result<Vec<(String, bool)>> {
            self.0.entries(p)
        }
        fn read(&self, p: &Path) -> io::Result<Vec<u8>> {
            self.0.read(p)
        }
        fn stat(&self, p: &Path) -> io::Result<Stat> {
            self.0.stat(p)
        }
        fn canonical(&self, p: &Path) -> io::Result<PathBuf> {
            self.0.canonical(p)
        }
        fn project_stat(&self, r: &Path, p: &Path) -> io::Result<(Stat, bool)> {
            self.0.project_stat(r, p)
        }
        fn project_read(&self, r: &Path, p: &Path, l: usize) -> io::Result<Vec<u8>> {
            if p.file_name().is_some_and(|name| name == ".cwd") {
                return self.0.project_read(r, p, l);
            }
            Err(io::ErrorKind::Other.into())
        }
    }
    let (metadata, data) = fixture();
    let metadata = Metadata {
        home: metadata.home,
        cwd: metadata.cwd,
        files: Fault(metadata.files),
    };
    for id in ["manifest-valid", "artifact-body"] {
        let case = data["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == id)
            .unwrap();
        let request = parse(case["request"].as_str().unwrap().as_bytes()).unwrap();
        let route = route_with_metadata(&request, Some(&metadata));
        if id == "manifest-valid" {
            assert!(matches!(route, Route::Forward));
        } else {
            let Route::LocalResponse(raw) = route else {
                panic!("artifact failures must stay local")
            };
            let response = parse(&raw).unwrap();
            assert_eq!(
                text_field(field(&response, "error"), "code").as_deref(),
                Some("ARCHIVE_ARTIFACT_NOT_FOUND")
            );
        }
    }
}

pub(crate) fn native_fixture() -> (Metadata<NativeFiles>, Value, PathBuf) {
    let (_, data) = fixture();
    let root = scratch();
    let home = root.join("pilot");
    let cwd = root.join("project");
    let old_home = data["home"].as_str().unwrap();
    let old_cwd = data["cwd"].as_str().unwrap();
    let relocate = |value: &str| {
        value
            .replace(old_home, home.to_str().unwrap())
            .replace(old_cwd, cwd.to_str().unwrap())
    };
    for (path, fact) in data["files"].as_object().unwrap() {
        let path = PathBuf::from(relocate(path));
        assert!(path.starts_with(&root));
        if fact["directory"] == true {
            fs::create_dir_all(&path).unwrap();
        } else {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            if let Some(body) = fact["base64"].as_str() {
                let mut bytes = STANDARD.decode(body).unwrap();
                if path.file_name().unwrap() == ".cwd" {
                    bytes = relocate(std::str::from_utf8(&bytes).unwrap()).into_bytes();
                }
                fs::write(path, bytes).unwrap();
            } else {
                let size = fact["size"].as_u64().unwrap();
                assert!(matches!(size, 52_428_801 | 2_147_483_648));
                fs::File::create(path).unwrap().set_len(size).unwrap();
            }
        }
    }
    let mut mapped = data.clone();
    for case in mapped["cases"].as_array_mut().unwrap() {
        for key in ["request", "response"] {
            case[key] = Value::String(relocate(case[key].as_str().unwrap()));
        }
    }
    println!("PDRS_ARCHIVE_NATIVE_ROOT {}", root.display());
    (
        Metadata {
            home,
            cwd,
            files: NativeFiles,
        },
        mapped,
        root,
    )
}

#[test]
fn archive_native_materialized_138_rpcs_match_with_explicit_path_relocation() {
    let (metadata, data, root) = native_fixture();
    let mut count = 0;
    for case in data["cases"].as_array().unwrap() {
        let request = parse(case["request"].as_str().unwrap().as_bytes()).unwrap();
        let Route::LocalResponse(raw) = route_with_metadata(&request, Some(&metadata)) else {
            panic!("Native unexpectedly forwarded {}", case["id"])
        };
        assert_eq!(
            raw,
            case["response"].as_str().unwrap().as_bytes(),
            "{}",
            case["id"]
        );
        count += 1;
    }
    assert_eq!(count, 138);
    fs::remove_dir_all(root).unwrap();
}

pub(crate) fn nul_long_fixture() -> (Metadata<Snapshot>, Value) {
    assert_fixture_hash(
        include_bytes!("../../tests/fixtures/native-archive/nul-long-path.json"),
        "889c1bb192a99c51d15edd0762f081e71b868f6f4895690844fcb95b9d75381f",
    );
    let data: Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/native-archive/nul-long-path.json"
    ))
    .unwrap();
    assert_eq!(data["cases"].as_array().unwrap().len(), 4);
    let expected: std::collections::BTreeSet<_> = [
        "session-nul",
        "session-nul-quotes-0",
        "session-nul-quotes-1",
        "session-nul-quotes-2",
    ]
    .into_iter()
    .collect();
    assert_eq!(
        data["cases"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["id"].as_str().unwrap())
            .collect::<std::collections::BTreeSet<_>>(),
        expected
    );
    (metadata_from_data(&data), data)
}
#[test]
fn archive_nul_long_paths_use_real_node_128_utf16_diagnostics() {
    let (metadata, data) = nul_long_fixture();
    for case in data["cases"].as_array().unwrap() {
        let Route::LocalResponse(raw) = route_with_metadata(
            &parse(case["request"].as_str().unwrap().as_bytes()).unwrap(),
            Some(&metadata),
        ) else {
            panic!("invalid path must terminate locally")
        };
        assert_eq!(
            raw,
            case["response"].as_str().unwrap().as_bytes(),
            "{}",
            case["id"]
        );
    }
}
