use super::*;
use crate::metadata::{NativeFiles, Stat, project_id, string};
use std::{
    collections::HashMap,
    io,
    path::{Path, PathBuf},
};

#[derive(Default)]
pub(crate) struct FixtureFiles {
    directories: HashMap<PathBuf, Vec<(String, bool)>>,
    data: HashMap<PathBuf, Vec<u8>>,
    stats: HashMap<PathBuf, Stat>,
}
impl Files for FixtureFiles {
    fn entries(&self, path: &Path) -> io::Result<Vec<(String, bool)>> {
        let mut entries = self.directories.get(path).cloned().ok_or_else(missing)?;
        // Injected filesystem facts obey the same Node readdir contract as NativeFiles.
        entries.sort_by(|l, r| l.0.as_bytes().cmp(r.0.as_bytes()));
        Ok(entries)
    }
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.data.get(path).cloned().ok_or_else(missing)
    }
    fn stat(&self, path: &Path) -> io::Result<Stat> {
        self.stats.get(path).cloned().ok_or_else(missing)
    }
}
fn missing() -> io::Error {
    io::Error::from_raw_os_error(libc::ENOENT)
}
fn response(raw: &str, metadata: &Metadata<FixtureFiles>) -> Vec<u8> {
    match route_with_metadata(&parse(raw.as_bytes()).unwrap(), Some(metadata)) {
        Route::LocalResponse(bytes) => bytes,
        Route::Forward => panic!("expected local response: {raw}"),
    }
}
fn golden(raw: &str) -> (String, String) {
    let pair = parse_json(raw).unwrap();
    (
        text_field(&pair, "request").unwrap(),
        text_field(&pair, "response").unwrap(),
    )
}
pub(crate) fn fixture() -> Metadata<FixtureFiles> {
    const ROOT: &str =
        "/Users/da/ws/pilotdeck-rs/_build/wt-a2/_tmp/20261014-exp006-r3-live/cur/project";
    let home = PathBuf::from(
        "/Users/da/ws/pilotdeck-rs/_build/wt-a2/_tmp/20261014-exp006-r3-live/cur/home",
    );
    let projects = home.join("projects");
    let id = project_id(Path::new(ROOT));
    let dir = projects.join(&id);
    let chat = dir.join("chats");
    let path = chat.join("web:gateway-parity-text.jsonl");
    let mut files = FixtureFiles::default();
    files.directories.insert(projects, vec![(id, true)]);
    files
        .directories
        .insert(chat, vec![("web:gateway-parity-text.jsonl".into(), false)]);
    files
        .data
        .insert(dir.join(".cwd"), format!("{ROOT}\n").into_bytes());
    let transcript = include_bytes!("../../tests/fixtures/metadata/transcript.jsonl").to_vec();
    files.stats.insert(
        path.clone(),
        Stat {
            directory: false,
            size: transcript.len() as u64,
            birth_ms: 0.0,
            ctime_ms: 0.0,
            mtime_ms: 1_791_944_437_924.0,
        },
    );
    files.data.insert(path, transcript);
    files.stats.insert(
        PathBuf::from(ROOT),
        Stat {
            directory: true,
            size: 0,
            birth_ms: 0.0,
            ctime_ms: 0.0,
            mtime_ms: 0.0,
        },
    );
    files.stats.insert(
        dir,
        Stat {
            directory: true,
            size: 0,
            birth_ms: 1_791_944_435_037.877_4,
            ctime_ms: 0.0,
            mtime_ms: 0.0,
        },
    );
    Metadata {
        home,
        cwd: PathBuf::from(ROOT),
        files,
    }
}

#[test]
fn three_native_methods_match_untouched_a2_ts_raw_bytes() {
    for raw in [
        include_str!("../../tests/fixtures/metadata/supported_agents.json"),
        include_str!("../../tests/fixtures/metadata/list_projects.json"),
        include_str!("../../tests/fixtures/metadata/describe_project.json"),
    ] {
        let (request, expected) = golden(raw);
        assert_eq!(response(&request, &fixture()), expected.as_bytes());
    }
}

#[test]
fn describe_project_keeps_lone_surrogate_distinct_from_literal_fffd() {
    let metadata = ordered_projects(&[("registered", "/fixture/project-\u{fffd}")]);
    let raw = r#"{"type":"request","id":"x","method":"describe_project","params":{"projectKey":"/fixture/project-\ud800"}}"#;
    let bytes = response(raw, &metadata);
    let response = parse_json(std::str::from_utf8(&bytes).unwrap()).unwrap();
    let summary = response.get("result").unwrap();
    let mut expected = "/fixture/project-".encode_utf16().collect::<Vec<_>>();
    expected.push(0xD800);
    let project_key = summary
        .get("projectKey")
        .and_then(JsValue::as_string)
        .unwrap();
    assert_eq!(project_key.units(), expected);
    let full_path = summary
        .get("fullPath")
        .and_then(JsValue::as_string)
        .unwrap();
    assert_eq!(full_path.units(), expected);
    // The registered U+FFFD project has a birth time. A lossy identity would
    // match its marker and surface that `createdAt` for the surrogate key too.
    assert!(summary.get("createdAt").is_none());
}

/// `listProjects.ts:131/154` compares `resolve(marker)` with
/// `resolve(projectKey)`, so the same lexical fold applies to metadata: a `..`
/// after a surrogate-bearing segment drops it before identity is checked.
/// Node measurement: `path.resolve('/fixture/project-\ud800/../project')` and
/// `path.resolve('/fixture','project-\ud800/../project')` both return
/// `/fixture/project`. The second half is the negative control: normalization
/// happens, but a surrogate that survives it can never match a disk marker.
#[test]
fn describe_project_folds_lone_surrogate_segments_before_identity() {
    let metadata = ordered_projects(&[("registered", "/fixture/project")]);
    for key in [
        "/fixture/project-\\ud800/../project",
        "project-\\ud800/../project",
    ] {
        let raw = format!(
            r#"{{"type":"request","id":"x","method":"describe_project","params":{{"projectKey":"{key}"}}}}"#
        );
        let bytes = response(&raw, &metadata);
        let response = parse_json(std::str::from_utf8(&bytes).unwrap()).unwrap();
        let summary = response.get("result").unwrap();
        assert_eq!(
            summary.get("createdAt").and_then(JsValue::as_number),
            Some(1.0),
            "{key} must match the registered project after folding"
        );
        // The wire keeps the caller's original units, not the resolved path.
        let expected = parse_json(&format!("\"{key}\"")).unwrap();
        let units = summary
            .get("projectKey")
            .and_then(JsValue::as_string)
            .unwrap()
            .units();
        assert_eq!(
            units,
            expected.as_string().unwrap().units(),
            "{key} must keep its raw wire units"
        );
    }
    for key in [
        "/fixture/project-\\ud800/../project-\\ud800",
        "project-\\ud800/../project-\\ud800",
    ] {
        let raw = format!(
            r#"{{"type":"request","id":"x","method":"describe_project","params":{{"projectKey":"{key}"}}}}"#
        );
        let bytes = response(&raw, &metadata);
        let response = parse_json(std::str::from_utf8(&bytes).unwrap()).unwrap();
        assert!(
            response
                .get("result")
                .and_then(|summary| summary.get("createdAt"))
                .is_none(),
            "{key} survives normalization and must not alias the registered project"
        );
    }
    // Folded away, but the normalized key is still unregistered. The summary
    // must not alias a registered project, and its wire fields keep the raw
    // units so the surrogate cannot turn into literal U+FFFD.
    for key in [
        "/fixture/project-\\ud800/../missing",
        "project-\\ud800/../missing",
    ] {
        let raw = format!(
            r#"{{"type":"request","id":"x","method":"describe_project","params":{{"projectKey":"{key}"}}}}"#
        );
        let bytes = response(&raw, &metadata);
        let response = parse_json(std::str::from_utf8(&bytes).unwrap()).unwrap();
        let summary = response.get("result").unwrap();
        assert!(
            summary.get("createdAt").is_none(),
            "{key} resolves to an unregistered project"
        );
        let expected = parse_json(&format!("\"{key}\"")).unwrap();
        assert_eq!(
            summary
                .get("projectKey")
                .and_then(JsValue::as_string)
                .unwrap()
                .units(),
            expected.as_string().unwrap().units(),
            "{key} must keep its raw wire units"
        );
    }
}

#[test]
fn missing_params_and_unknown_project_match_ts_error_and_success_bytes() {
    let metadata = Metadata {
        home: PathBuf::from("/empty"),
        cwd: PathBuf::from("/empty"),
        files: FixtureFiles::default(),
    };
    let errors = parse_json(include_str!("../../tests/fixtures/metadata/errors.json")).unwrap();
    let JsValue::Array(cases) = &errors else {
        panic!("fixture array")
    };
    for case in cases {
        let request = text_field(case, "request").unwrap();
        let expected = text_field(case, "response").unwrap();
        assert_eq!(response(&request, &metadata), expected.as_bytes());
    }
}

/// #347 fix-r1 (review P1-3): the `constructor.name` shapes Node converts
/// with `String(name)` (`{}`, `[]`, `null`, `""`, `0` …) produce the local
/// `ERR_INVALID_ARG_TYPE` text instead of forwarding to TS; the shapes whose
/// text Node never produces (a primitive constructor) still forward. Node
/// v22.22.0 matrix: `_tmp/20261026-issue-347/fix-r1/probe/constructor-name
/// {,-edge}.out`.
#[test]
fn describe_project_constructor_name_shapes_match_node() {
    let metadata = Metadata {
        home: PathBuf::from("/empty"),
        cwd: PathBuf::from("/empty"),
        files: FixtureFiles::default(),
    };
    for (label, key_json, received) in [
        (
            "name-object",
            r#"{"constructor":{"name":{}}}"#,
            "an instance of [object Object]",
        ),
        (
            "name-array",
            r#"{"constructor":{"name":[]}}"#,
            "an instance of ",
        ),
        (
            "name-two-element-array",
            r#"{"constructor":{"name":[1,2]}}"#,
            "an instance of 1,2",
        ),
        (
            "name-null",
            r#"{"constructor":{"name":null}}"#,
            "an instance of null",
        ),
        (
            "name-empty-string",
            r#"{"constructor":{"name":""}}"#,
            "an instance of ",
        ),
        (
            "name-zero",
            r#"{"constructor":{"name":0}}"#,
            "an instance of 0",
        ),
        (
            "name-infinity",
            r#"{"constructor":{"name":1e400}}"#,
            "an instance of Infinity",
        ),
    ] {
        let raw = format!(
            r#"{{"type":"request","id":"x","method":"describe_project","params":{{"projectKey":{key_json}}}}}"#
        );
        let parsed = parse_json(std::str::from_utf8(&response(&raw, &metadata)).unwrap()).unwrap();
        let error = parsed.get("error").unwrap();
        assert_eq!(
            text_field(error, "code").as_deref(),
            Some("ERR_INVALID_ARG_TYPE"),
            "#347 fix-r1 {label} keeps Node's error code"
        );
        assert_eq!(
            text_field(error, "message").as_deref(),
            Some(
                format!("The \"paths[0]\" argument must be of type string. Received {received}")
                    .as_str()
            ),
            "#347 fix-r1 {label} message matches Node's String(name) conversion"
        );
    }
    // A primitive constructor makes Node throw `Cannot use 'in' operator …`
    // before any Received text exists; that error shape stays TS-owned here.
    for key_json in [r#"{"constructor":5}"#, r#"{"constructor":"x"}"#] {
        let raw = format!(
            r#"{{"type":"request","id":"x","method":"describe_project","params":{{"projectKey":{key_json}}}}}"#
        );
        assert!(
            matches!(
                route_with_metadata(&parse(raw.as_bytes()).unwrap(), Some(&metadata)),
                Route::Forward
            ),
            "#347 fix-r1 {key_json} still forwards"
        );
    }
}

#[test]
fn describe_project_unrepresentable_received_text_forwards() {
    let metadata = Metadata {
        home: PathBuf::from("/empty"),
        cwd: PathBuf::from("/empty"),
        files: FixtureFiles::default(),
    };
    for (label, key_json) in [
        ("surrogate", r#"{"constructor":{"name":"\ud800"}}"#),
        ("primitive-constructor", r#"{"constructor":5}"#),
        (
            "unconvertible-name",
            r#"{"constructor":{"name":{"toString":null,"valueOf":null}}}"#,
        ),
    ] {
        let raw = format!(
            r#"{{"type":"request","id":"x","method":"describe_project","params":{{"projectKey":{key_json}}}}}"#
        );
        let request = parse(raw.as_bytes()).expect("#347 fix-r5b dispatch request must parse");
        assert!(
            matches!(
                route_with_metadata(&request, Some(&metadata)),
                Route::Forward
            ),
            "#347 fix-r5b {label} must forward without rewriting the Node error"
        );
    }
}

#[test]
fn deferred_methods_and_invalid_frames_keep_upstream_ownership() {
    for raw in [
        r#"{"type":"request","id":"x","method":"session_model_set"}"#,
        r#"{"type":"request","id":"x","method":"describe_server"}"#,
        r#"{"type":"hello","method":"supported_agents"}"#,
        r#"{"type":"request","method":"supported_agents"}"#,
        r#"{"type":"request","id":7,"method":"supported_agents"}"#,
    ] {
        assert!(matches!(
            route_with_metadata(&parse(raw.as_bytes()).unwrap(), Some(&fixture())),
            Route::Forward
        ));
    }
    for bytes in [b"invalid json".as_slice(), &[255]] {
        assert!(parse(bytes).is_none());
    }
    assert!(matches!(
        route_with_metadata::<NativeFiles>(
            &parse(br#"{"type":"request","id":"x","method":"list_projects"}"#).unwrap(),
            None
        ),
        Route::Forward
    ));
}

#[test]
fn codec_preserves_js_id_and_replacement_decodes_utf8() {
    let raw = br#"{"type":"request","id":"\ud800","method":"supported_agents","params":null}"#;
    let bytes = response(std::str::from_utf8(raw).unwrap(), &fixture());
    assert!(bytes.starts_with(br#"{"type":"response","id":"\ud800","ok":true,"result":{"#));
    let mut invalid = br#"{"type":"request","id":""#.to_vec();
    invalid.push(255);
    invalid.extend_from_slice(br#"","method":"supported_agents"}"#);
    let parsed = parse(&invalid).unwrap();
    assert_eq!(text_field(&parsed, "id").unwrap(), "\u{fffd}");
}

fn projected_cases() -> JsValue {
    parse_json(include_str!(
        "../../tests/fixtures/metadata/project-cases.json"
    ))
    .unwrap()
}

fn ordered_projects(entries: &[(&str, &str)]) -> Metadata<FixtureFiles> {
    let home = PathBuf::from("/fixture/home");
    let projects = home.join("projects");
    let mut files = FixtureFiles::default();
    files.directories.insert(
        projects.clone(),
        entries.iter().map(|(id, _)| ((*id).into(), true)).collect(),
    );
    for (index, (id, root)) in entries.iter().enumerate() {
        let dir = projects.join(id);
        let stat = Stat {
            directory: true,
            size: 0,
            birth_ms: (index + 1) as f64,
            ctime_ms: 0.0,
            mtime_ms: 0.0,
        };
        files.stats.insert(dir.clone(), stat.clone());
        files.stats.insert(PathBuf::from(root), stat);
        files
            .data
            .insert(dir.join(".cwd"), root.as_bytes().to_vec());
        files.directories.insert(dir.join("chats"), Vec::new());
    }
    Metadata {
        home,
        cwd: PathBuf::from("/fixture"),
        files,
    }
}

#[test]
fn native_entries_are_sorted_by_utf8_bytes() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let entries = NativeFiles.entries(&path).unwrap();
    assert!(entries.len() > 1);
    assert!(
        entries
            .windows(2)
            .all(|pair| { pair[0].0.as_bytes() < pair[1].0.as_bytes() })
    );
}

#[test]
fn native_entries_match_node_mixed_name_byte_order() {
    let path = std::env::temp_dir().join(format!(
        "pilotdeck-readdir-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&path).unwrap();
    for name in ["Beta", "ä", "middle", "zebra", "10", "_x", "alpha", "2"] {
        if name == "_x" {
            std::fs::create_dir(path.join(name)).unwrap();
        } else {
            std::fs::write(path.join(name), []).unwrap();
        }
    }
    let entries = NativeFiles.entries(&path).unwrap();
    std::fs::remove_dir_all(&path).unwrap();
    // Hard-coded Node v22 / APFS readdirSync and promises.readdir oracle.
    assert_eq!(
        entries,
        [
            ("10", false),
            ("2", false),
            ("Beta", false),
            ("_x", true),
            ("alpha", false),
            ("middle", false),
            ("zebra", false),
            ("ä", false),
        ]
        .map(|(name, directory)| (name.to_owned(), directory))
    );
}

#[test]
fn tied_projects_use_byte_order_regardless_of_injected_order() {
    let entries = [
        ("z-storage", "/projects/zebra"),
        ("a-storage", "/projects/alpha"),
        ("m-storage", "/projects/middle"),
    ];
    for order in [
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ] {
        let metadata = ordered_projects(&order.map(|index| entries[index]));
        let result = metadata.list_projects();
        let JsValue::Array(projects) = result.get("projects").unwrap() else {
            panic!("projects array")
        };
        assert!(projects.iter().all(|project| {
            project.get("sessionCount").and_then(JsValue::as_number) == Some(0.0)
                && project.get("lastActivity").unwrap().is_undefined()
        }));
        let roots: Vec<_> = projects
            .iter()
            .map(|project| text_field(project, "fullPath").unwrap())
            .collect();
        assert_eq!(
            roots,
            ["/projects/alpha", "/projects/middle", "/projects/zebra"]
        );
    }
}

#[test]
fn duplicate_markers_use_first_byte_order_match_for_storage_and_chats() {
    let entries = [
        ("z-collision", "/projects/shared"),
        ("a-collision", "/projects/shared"),
    ];
    for order in [[0, 1], [1, 0]] {
        let mut metadata = ordered_projects(&order.map(|index| entries[index]));
        let storage = metadata.home.join("projects/a-collision");
        metadata.files.stats.get_mut(&storage).unwrap().birth_ms = 42.0;
        // Only the byte-order first entry has a visible session. Both storage
        // birthtime and the chat catalog must select it in either injected order.
        let chat = storage.join("chats");
        let transcript = br#"{"type":"session_metadata","metadata":{"title":"first"}}"#;
        metadata
            .files
            .directories
            .insert(chat.clone(), vec![("visible.jsonl".into(), false)]);
        metadata
            .files
            .data
            .insert(chat.join("visible.jsonl"), transcript.to_vec());
        metadata.files.stats.insert(
            chat.join("visible.jsonl"),
            Stat {
                directory: false,
                size: transcript.len() as u64,
                birth_ms: 0.0,
                ctime_ms: 0.0,
                mtime_ms: 1234.0,
            },
        );
        let summary = metadata.describe_project(&"/projects/shared".into());
        assert_eq!(
            summary.get("createdAt").and_then(JsValue::as_number),
            Some(42.0)
        );
        assert_eq!(
            summary.get("sessionCount").and_then(JsValue::as_number),
            Some(1.0)
        );
        assert_eq!(
            summary.get("lastActivity").and_then(JsValue::as_number),
            Some(1234.0)
        );
    }
}

fn files_from_case(case: &JsValue) -> FixtureFiles {
    let mut files = FixtureFiles::default();
    for (path, fact) in case.get("files").unwrap().as_object().unwrap().entries() {
        let path = PathBuf::from(path.to_utf8().unwrap());
        let number = |key| fact.get(key).unwrap().as_number().unwrap();
        files.stats.insert(
            path.clone(),
            Stat {
                directory: fact.get("directory").unwrap().as_bool().unwrap(),
                size: number("size") as u64,
                birth_ms: number("birth_ms"),
                ctime_ms: number("ctime_ms"),
                mtime_ms: number("mtime_ms"),
            },
        );
        if let Some(JsValue::Array(entries)) = fact.get("entries") {
            files.directories.insert(
                path.clone(),
                entries
                    .iter()
                    .map(|entry| {
                        let JsValue::Array(parts) = entry else {
                            panic!("entry")
                        };
                        (string(&parts[0]).unwrap(), parts[1].as_bool().unwrap())
                    })
                    .collect(),
            );
        }
        if let Some(JsValue::Array(parts)) = fact.get("data") {
            let mut bytes = Vec::new();
            for part in parts {
                if let Some(count) = part.as_number() {
                    bytes.extend(std::iter::repeat_n(b'x', count as usize));
                } else {
                    bytes.extend(string(part).unwrap().as_bytes());
                }
            }
            files.data.insert(path, bytes);
        }
    }
    files
}

#[test]
fn project_edge_cases_match_offline_real_ts_projection_and_fs_snapshot() {
    let cases = projected_cases();
    let JsValue::Array(cases) = &cases else {
        panic!("cases")
    };
    assert_eq!(cases.len(), 14);
    for case in cases {
        let metadata = Metadata {
            home: PathBuf::from(text_field(case, "home").unwrap()),
            cwd: PathBuf::from(text_field(case, "cwd").unwrap()),
            files: files_from_case(case),
        };
        let request = text_field(case, "request").unwrap();
        let expected = text_field(case, "response").unwrap();
        assert_eq!(
            response(&request, &metadata),
            expected.as_bytes(),
            "request {request}"
        );
    }
}

#[test]
fn native_stat_and_read_projection_matches_fresh_ts_when_requested() {
    // Optional local source-oracle check: fixture directories must still exist.
    // Portable, mandatory tests above use the captured filesystem input facts.
    if std::env::var_os("PDRS_METADATA_NATIVE_CHECK").is_none() {
        return;
    }
    let cases = projected_cases();
    let JsValue::Array(cases) = &cases else {
        panic!("cases")
    };
    // Later fixture edits deliberately change earlier snapshots; the final
    // describe request still has the current filesystem state and stats.
    let case = cases.last().unwrap();
    let metadata = Metadata {
        home: PathBuf::from(text_field(case, "home").unwrap()),
        cwd: PathBuf::from(text_field(case, "cwd").unwrap()),
        files: NativeFiles,
    };
    let request = parse(text_field(case, "request").unwrap().as_bytes()).unwrap();
    let Route::LocalResponse(actual) = route_with_metadata(&request, Some(&metadata)) else {
        panic!("local")
    };
    assert_eq!(actual, text_field(case, "response").unwrap().as_bytes());
    for (path, fact) in case.get("files").unwrap().as_object().unwrap().entries() {
        let stat = metadata
            .files
            .stat(Path::new(&path.to_utf8().unwrap()))
            .unwrap();
        assert_eq!(
            stat.birth_ms,
            fact.get("birth_ms").unwrap().as_number().unwrap()
        );
        assert_eq!(
            stat.ctime_ms,
            fact.get("ctime_ms").unwrap().as_number().unwrap()
        );
        assert_eq!(
            stat.mtime_ms,
            fact.get("mtime_ms").unwrap().as_number().unwrap()
        );
    }
}

// Actual OS failures matching the fixed Node public-RPC oracle. The guard owns
// only a newly created scratch root and restores permissions even on panic.
struct ListProjectFixture {
    metadata: Metadata<NativeFiles>,
    root: PathBuf,
}
impl ListProjectFixture {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "pdrs-list-projects-{}-{}-{name}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let fixture = Self {
            metadata: Metadata {
                home: root.clone(),
                cwd: root.clone(),
                files: NativeFiles,
            },
            root,
        };
        let projects = fixture.root.join("projects");
        match name {
            "missing" => {}
            "not-directory" => std::fs::write(&projects, b"not a directory").unwrap(),
            "empty" | "denied" => {
                std::fs::create_dir(&projects).unwrap();
                if name == "denied" {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&projects, std::fs::Permissions::from_mode(0o0))
                        .unwrap();
                }
            }
            _ => panic!("unknown fixture {name}"),
        }
        fixture
    }
}
impl Drop for ListProjectFixture {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(
            self.root.join("projects"),
            std::fs::Permissions::from_mode(0o700),
        );
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
fn denied_errno_unavailable<T, U>(observed: &io::Result<T>, native_probe: &io::Result<U>) -> bool {
    match native_probe {
        Ok(_) => {
            assert!(
                observed.is_ok(),
                "wrapper failed although native permission probe succeeded"
            );
            true
        }
        Err(error) => {
            assert_eq!(
                crate::node_io::lookup(error).map(|row| row.0),
                Some("EACCES")
            );
            assert!(
                observed.is_err(),
                "wrapper swallowed independently observed EACCES"
            );
            false
        }
    }
}

#[test]
fn denied_errno_guard_rejects_swallowed_native_error() {
    let success = || Ok::<_, io::Error>(Vec::<()>::new());
    let denied = || Err::<Vec<()>, _>(io::Error::from_raw_os_error(libc::EACCES));
    assert!(!denied_errno_unavailable(&denied(), &denied()));
    assert!(denied_errno_unavailable(&success(), &success()));
    assert!(std::panic::catch_unwind(|| denied_errno_unavailable(&success(), &denied())).is_err());
    assert!(std::panic::catch_unwind(|| denied_errno_unavailable(&denied(), &success())).is_err());
    assert!(
        std::panic::catch_unwind(|| denied_errno_unavailable(
            &denied(),
            &Err::<Vec<()>, _>(io::Error::from_raw_os_error(libc::ENOENT))
        ))
        .is_err()
    );
}

#[test]
fn list_projects_preserves_real_node_scandir_error_suppression() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/metadata/list-project-errors.json"
    ))
    .unwrap();
    let rows = fixture["results"].as_array().unwrap();
    assert_eq!(rows.len(), 4);
    assert_eq!(
        rows.iter()
            .map(|row| row["name"].as_str().unwrap())
            .collect::<std::collections::BTreeSet<_>>(),
        ["missing", "not-directory", "denied", "empty"]
            .into_iter()
            .collect()
    );
    for row in rows {
        let name = row["name"].as_str().unwrap();
        let actual = ListProjectFixture::new(name);
        let scan = actual.metadata.files.entries(&actual.root.join("projects"));
        let unavailable = if name == "denied" {
            let probe = std::fs::read_dir(actual.root.join("projects"))
                .and_then(|entries| entries.collect::<io::Result<Vec<_>>>());
            denied_errno_unavailable(&scan, &probe)
        } else {
            false
        };
        if name == "empty" {
            assert!(scan.unwrap().is_empty());
        } else if unavailable {
            // Root or a permissive filesystem may ignore chmod denial. Keep
            // recorded error replay below mandatory and report this boundary.
            eprintln!(
                "PDRS_UNAVAILABLE real-EACCES: privilege/filesystem permits access; recorded replay required"
            );
        } else {
            let error = scan.unwrap_err();
            assert_eq!(
                crate::node_io::lookup(&error).unwrap().0,
                row["actualError"]["code"].as_str().unwrap()
            );
            assert_eq!(row["actualError"]["syscall"], "scandir");
        }
        let request =
            format!("{{\"type\":\"request\",\"id\":\"{name}\",\"method\":\"list_projects\"}}");
        let Route::LocalResponse(response) =
            route_with_metadata(&parse(request.as_bytes()).unwrap(), Some(&actual.metadata))
        else {
            panic!("must stay local")
        };
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&response).unwrap(),
            row["response"]
        );
        if name == "denied" {
            struct RecordedDenied;
            impl Files for RecordedDenied {
                fn entries(&self, _: &Path) -> io::Result<Vec<(String, bool)>> {
                    Err(io::Error::from_raw_os_error(libc::EACCES))
                }
                fn read(&self, _: &Path) -> io::Result<Vec<u8>> {
                    panic!("scandir failure must not read")
                }
                fn stat(&self, _: &Path) -> io::Result<Stat> {
                    panic!("scandir failure must not stat")
                }
            }
            let recorded = Metadata {
                home: actual.metadata.home.clone(),
                cwd: actual.metadata.cwd.clone(),
                files: RecordedDenied,
            };
            let Route::LocalResponse(response) =
                route_with_metadata(&parse(request.as_bytes()).unwrap(), Some(&recorded))
            else {
                panic!("recorded EACCES must stay local")
            };
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&response).unwrap(),
                row["response"]
            );
        }
    }
}
