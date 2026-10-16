//! Actual pinned Node RPCs are retained separately from candidate safety errors.
use super::*;
use crate::{
    dispatch::{Route, parse, route_with_metadata},
    metadata::{NativeFiles, Stat},
};
use std::{fs, path::PathBuf};

pub(crate) fn oracle() -> serde_json::Value {
    super::tests::assert_fixture_hash(
        include_bytes!("../../tests/fixtures/native-archive/security-node.json"),
        "09fe06e563866f92471e53a2791e8b0e9891b18a11e230b435de944f39e60558",
    );
    let data: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/native-archive/security-node.json"
    ))
    .unwrap();
    let cases = data["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 9);
    let ids: std::collections::HashSet<_> = cases
        .iter()
        .map(|case| case["id"].as_str().filter(|s| !s.is_empty()).unwrap())
        .collect();
    assert_eq!(ids.len(), 9);
    for method in [
        "native_archive_manifest",
        "native_archive_entries",
        "native_archive_artifact",
    ] {
        for suffix in [
            "normal",
            "static-outside-symlink",
            if method == "native_archive_artifact" {
                "before-readFile-open"
            } else {
                "after-stat-before-read"
            },
        ] {
            assert!(ids.contains(format!("{method}-{suffix}").as_str()));
        }
    }
    data
}

pub(crate) struct ProbeFiles {
    target: PathBuf,
    outside: PathBuf,
    swap: std::sync::atomic::AtomicBool,
    pub hits: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}
impl Files for ProbeFiles {
    fn entries(&self, p: &Path) -> io::Result<Vec<(String, bool)>> {
        NativeFiles.entries(p)
    }
    fn read(&self, p: &Path) -> io::Result<Vec<u8>> {
        panic!("archive marker/content must use a contained descriptor: {p:?}")
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
    fn session_summary(&self, _: &Path, _: u64) -> std::result::Result<bool, ()> {
        panic!("archive registration must not read unrelated transcript content")
    }
    fn archive_read(&self, root: &Path, path: &Path) -> io::Result<Vec<u8>> {
        if path == self.target && self.swap.swap(false, std::sync::atomic::Ordering::SeqCst) {
            fs::remove_file(path)?;
            std::os::unix::fs::symlink(&self.outside, path)?;
            self.hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        NativeFiles.archive_read(root, path)
    }
}

pub(crate) fn prepare(
    case: &serde_json::Value,
    data: &serde_json::Value,
) -> (Metadata<ProbeFiles>, String, String, PathBuf, bool) {
    use std::os::unix::fs::symlink;
    let root = super::tests::scratch();
    let project = root.join("project");
    let home = root.join("pilot");
    let chat = home.join("projects/registered/chats");
    let artifacts = project.join(".pilotdeck/tool-results/fixture");
    fs::create_dir_all(&chat).unwrap();
    fs::create_dir_all(&artifacts).unwrap();
    fs::write(
        home.join("projects/registered/.cwd"),
        project.to_str().unwrap(),
    )
    .unwrap();
    let id = case["id"].as_str().unwrap();
    let raw = case["request"]
        .as_str()
        .unwrap()
        .replace(data["cwd"].as_str().unwrap(), project.to_str().unwrap());
    let request = parse(raw.as_bytes()).unwrap();
    let method = text_field(&request, "method").unwrap();
    let artifact = method == "native_archive_artifact";
    let target = if artifact {
        artifacts.join("probe.bin")
    } else {
        chat.join(format!("{method}.jsonl"))
    };
    let normal_oracle = oracle();
    let normal = normal_oracle["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == "native_archive_artifact-normal")
        .unwrap();
    let normal_response: serde_json::Value =
        serde_json::from_str(normal["response"].as_str().unwrap()).unwrap();
    let inside = STANDARD
        .decode(normal_response["result"]["content"].as_str().unwrap())
        .unwrap();
    let secret = String::from_utf8(inside.clone())
        .unwrap()
        .replace("\"sequence\":7", "\"sequence\":4242");
    let outside = root.join("outside-fixture");
    fs::write(&outside, secret).unwrap();
    let is_static = id.ends_with("static-outside-symlink");
    let internal_link = id.ends_with("in-root-symlink");
    let raced = !(is_static || id.ends_with("normal") || internal_link);
    if is_static {
        symlink(&outside, &target).unwrap()
    } else if internal_link {
        let body = target.with_extension("inside");
        fs::write(&body, &inside).unwrap();
        symlink(&body, &target).unwrap();
    } else {
        fs::write(&target, inside).unwrap()
    }
    let expected = if id.ends_with("normal") || internal_link {
        case["response"].as_str().unwrap().to_owned()
    } else {
        let (code, message) = if artifact {
            (
                "ARCHIVE_ARTIFACT_NOT_FOUND",
                "Archive artifact probe.bin was not found.",
            )
        } else {
            (
                "PATH_NOT_ALLOWED",
                "Archive path resolves outside its storage root.",
            )
        };
        format!(
            "{{\"type\":\"response\",\"id\":{},\"ok\":false,\"error\":{{\"code\":{},\"message\":{}}}}}",
            serde_json::to_string(id).unwrap(),
            serde_json::to_string(code).unwrap(),
            serde_json::to_string(message).unwrap()
        )
    };
    let files = ProbeFiles {
        target,
        outside,
        swap: std::sync::atomic::AtomicBool::new(raced),
        hits: Default::default(),
    };
    (
        Metadata {
            home,
            cwd: project,
            files,
        },
        raw,
        expected,
        root,
        raced,
    )
}

#[test]
fn archive_security_nine_real_node_cases_fail_closed_in_native() {
    let data = oracle();
    let mut seen = 0;
    for case in data["cases"].as_array().unwrap() {
        let (metadata, raw, expected, root, raced) = prepare(case, &data);
        let Route::LocalResponse(response) =
            route_with_metadata(&parse(raw.as_bytes()).unwrap(), Some(&metadata))
        else {
            panic!("security refusal must never forward: {}", case["id"])
        };
        assert_eq!(response, expected.as_bytes(), "{}", case["id"]);
        assert_eq!(
            metadata
                .files
                .hits
                .load(std::sync::atomic::Ordering::SeqCst),
            usize::from(raced)
        );
        println!("PDRS_ARCHIVE_SECURITY_OK {}", case["id"].as_str().unwrap());
        seen += 1;
        fs::remove_dir_all(root).unwrap();
    }
    assert_eq!(seen, 9);
}

#[test]
fn archive_real_openat_refuses_leaf_ancestor_and_root_replacements_before_reading() {
    use std::os::unix::fs::symlink;
    for stage in ["leaf", "ancestor", "root"] {
        let area = super::tests::scratch();
        let root = area.join("root");
        let outside = area.join("outside");
        fs::create_dir_all(root.join("nested")).unwrap();
        fs::create_dir_all(outside.join("nested")).unwrap();
        let target = root.join("nested/body");
        fs::write(&target, b"inside").unwrap();
        fs::write(outside.join("nested/body"), b"outside-secret").unwrap();
        fs::write(outside.join("body"), b"outside-secret").unwrap();
        let canonical = fs::canonicalize(&target).unwrap();
        let canonical_root = fs::canonicalize(&root).unwrap();
        match stage {
            "leaf" => {
                fs::remove_file(&target).unwrap();
                symlink(outside.join("body"), &target).unwrap()
            }
            "ancestor" => {
                fs::rename(root.join("nested"), root.join("kept")).unwrap();
                symlink(&outside, root.join("nested")).unwrap()
            }
            "root" => {
                fs::rename(&root, area.join("kept")).unwrap();
                symlink(&outside, &root).unwrap()
            }
            _ => unreachable!(),
        }
        let error = crate::project_files::open_beneath(&canonical_root, &canonical).unwrap_err();
        assert!(
            crate::project_files::nofollow_loop(&error)
                || error.raw_os_error() == Some(libc::ENOTDIR),
            "{stage}: {error:?}"
        );
        let observed = format!("{error:?}");
        assert_eq!(
            secure_fs_error(error, "native_archive_manifest", "open", &target).0,
            "PATH_NOT_ALLOWED"
        );
        // The actual secure open failed before acquiring a readable descriptor.
        // Independent ordinary open confirms this replacement really reaches a secret.
        assert_eq!(fs::read(&target).unwrap(), b"outside-secret");
        println!("PDRS_ARCHIVE_OPENAT_REFUSED {stage} {observed}");
        fs::remove_dir_all(area).unwrap();
    }
}

pub(crate) fn links_fixture() -> (serde_json::Value, serde_json::Value) {
    super::tests::assert_fixture_hash(
        include_bytes!("../../tests/fixtures/native-archive/in-root-links.json"),
        "23191c2634f7c7e5e5fb5dcdb62c880f522ec512800821e8949cf941f839094e",
    );
    let links: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/native-archive/in-root-links.json"
    ))
    .unwrap();
    assert_eq!(links["cases"].as_array().unwrap().len(), 3);
    let ids: std::collections::BTreeSet<_> = links["cases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        [
            "native_archive_manifest-in-root-symlink",
            "native_archive_entries-in-root-symlink",
            "native_archive_artifact-in-root-symlink"
        ]
        .into_iter()
        .collect()
    );
    let mut data = oracle();
    data["cwd"] = links["cwd"].clone();
    (links, data)
}
#[test]
fn archive_ordinary_in_root_links_match_three_real_node_rpcs() {
    let (links, data) = links_fixture();
    for case in links["cases"].as_array().unwrap() {
        let (metadata, raw, expected, root, raced) = prepare(case, &data);
        assert!(!raced);
        let Route::LocalResponse(response) =
            route_with_metadata(&parse(raw.as_bytes()).unwrap(), Some(&metadata))
        else {
            panic!("in-root link must remain local")
        };
        assert_eq!(response, expected.as_bytes(), "{}", case["id"]);
        fs::remove_dir_all(root).unwrap();
    }
}

pub(crate) fn marker_fixture() -> serde_json::Value {
    super::tests::assert_fixture_hash(
        include_bytes!("../../tests/fixtures/native-archive/marker-node.json"),
        "e79cc5878070c3f11fd496935a5ecf6f37875dcc53cc6f41bd52271755d32222",
    );
    let data: serde_json::Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/native-archive/marker-node.json"
    ))
    .unwrap();
    assert_eq!(data["cases"].as_array().unwrap().len(), 9);
    for method in [
        "native_archive_manifest",
        "native_archive_entries",
        "native_archive_artifact",
    ] {
        for stage in ["static", "race", "inside"] {
            let id = format!("{method}-marker-{stage}");
            let rows: Vec<_> = data["cases"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|row| row["id"] == id)
                .collect();
            assert_eq!(rows.len(), 1);
            let row = rows[0];
            assert_eq!(row["method"], method);
            assert_eq!(row["stage"], stage);
            assert!(row["trace"].as_array().unwrap().iter().any(|event| {
                event["api"]
                    .as_str()
                    .is_some_and(|api| api == "fsp.readFile" || api == "fs.readFileSync")
                    && event["base64"].as_str().is_some()
            }));
            assert_eq!(
                row["trace"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|e| e["event"] == "marker-replaced-before-real-open")
                    .count(),
                usize::from(stage == "race")
            );
        }
    }
    data
}
pub(crate) fn prepare_marker(
    case: &serde_json::Value,
    data: &serde_json::Value,
) -> (Metadata<ProbeFiles>, String, String, PathBuf, bool) {
    use std::os::unix::fs::symlink;
    let mut plain = case.clone();
    plain["id"] = format!("{}-normal", case["method"].as_str().unwrap()).into();
    let (mut metadata, raw, positive, root, _) = prepare(&plain, data);
    let marker = metadata.home.join("projects/registered/.cwd");
    let outside = root.join("outside-marker");
    fs::write(&outside, format!("{}\n\t", metadata.cwd.display())).unwrap();
    let stage = case["stage"].as_str().unwrap();
    if stage == "static" {
        fs::remove_file(&marker).unwrap();
        symlink(&outside, &marker).unwrap()
    }
    if stage == "inside" {
        let inside = marker.with_extension("inside");
        fs::write(&inside, metadata.cwd.to_str().unwrap()).unwrap();
        fs::remove_file(&marker).unwrap();
        symlink(inside, &marker).unwrap()
    }
    metadata.files.target = marker;
    metadata.files.outside = outside;
    metadata.files.swap = std::sync::atomic::AtomicBool::new(stage == "race");
    let expected = if stage == "inside" {
        positive
    } else {
        let request = parse(raw.as_bytes()).unwrap();
        let id = text_field(&request, "id").unwrap();
        format!(
            "{{\"type\":\"response\",\"id\":{},\"ok\":false,\"error\":{{\"code\":\"PROJECT_NOT_FOUND\",\"message\":{}}}}}",
            serde_json::to_string(&id).unwrap(),
            serde_json::to_string(&format!("Unknown projectKey: {}", metadata.cwd.display()))
                .unwrap()
        )
    };
    (metadata, raw, expected, root, stage == "race")
}
#[test]
fn archive_marker_static_and_checked_open_replacements_do_not_read_outside_contents() {
    let data = marker_fixture();
    let mut count = 0;
    for row in data["cases"].as_array().unwrap() {
        let (metadata, raw, expected, root, raced) = prepare_marker(row, &data);
        let Route::LocalResponse(actual) =
            route_with_metadata(&parse(raw.as_bytes()).unwrap(), Some(&metadata))
        else {
            panic!("marker refusal must never forward")
        };
        assert_eq!(actual, expected.as_bytes(), "{}", row["id"]);
        assert_eq!(
            metadata
                .files
                .hits
                .load(std::sync::atomic::Ordering::SeqCst),
            usize::from(raced)
        );
        println!("PDRS_ARCHIVE_MARKER_OK {}", row["id"].as_str().unwrap());
        count += 1;
        fs::remove_dir_all(root).unwrap();
    }
    assert_eq!(count, 9);
}
