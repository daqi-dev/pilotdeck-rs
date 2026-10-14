use super::*;
use crate::metadata::{NativeFiles, Stat, project_id};
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
        self.directories.get(path).cloned().ok_or_else(missing)
    }
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.data.get(path).cloned().ok_or_else(missing)
    }
    fn stat(&self, path: &Path) -> io::Result<Stat> {
        self.stats.get(path).cloned().ok_or_else(missing)
    }
}
fn missing() -> io::Error {
    io::ErrorKind::NotFound.into()
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

#[test]
fn deferred_methods_and_invalid_frames_keep_upstream_ownership() {
    for raw in [
        r#"{"type":"request","id":"x","method":"session_model_get"}"#,
        r#"{"type":"request","id":"x","method":"model_catalog_list"}"#,
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
fn native_entries_preserve_os_read_dir_order() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let expected: Vec<_> = std::fs::read_dir(&path)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            (
                entry.file_name().to_string_lossy().into_owned(),
                entry.file_type().unwrap().is_dir(),
            )
        })
        .collect();
    assert_eq!(NativeFiles.entries(&path).unwrap(), expected);
}

#[test]
fn tied_projects_preserve_injected_readdir_order_instead_of_name_order() {
    let metadata = ordered_projects(&[
        ("z-storage", "/projects/zebra"),
        ("a-storage", "/projects/alpha"),
        ("m-storage", "/projects/middle"),
    ]);
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
        ["/projects/zebra", "/projects/alpha", "/projects/middle"]
    );
}

#[test]
fn duplicate_markers_use_first_readdir_match_for_storage_and_chats() {
    let mut metadata = ordered_projects(&[
        ("z-collision", "/projects/shared"),
        ("a-collision", "/projects/shared"),
    ]);
    // Only the first entry has a visible session. Both storage birthtime and
    // the chat catalog must select it, even though its name sorts last.
    let chat = metadata.home.join("projects/z-collision/chats");
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
        Some(1.0)
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
