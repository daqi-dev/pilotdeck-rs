use super::*;
use crate::{
    dispatch::{Route, parse, route_with_metadata},
    metadata::{NativeFiles, Stat},
};
use pilotdeck_protocol::JsString;
use serde_json::Value;
use std::{
    collections::HashMap,
    fs, io,
    sync::atomic::{AtomicUsize, Ordering},
};

pub(crate) struct Snapshot {
    pub(crate) files: HashMap<PathBuf, Value>,
}
impl Snapshot {
    fn fact(&self, path: &Path) -> io::Result<&Value> {
        self.files
            .get(path)
            .ok_or(io::Error::from_raw_os_error(libc::ENOENT))
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
                    .ok_or(io::ErrorKind::InvalidInput)?,
            )
            .map_err(io::Error::other)
    }
    fn stat(&self, path: &Path) -> io::Result<Stat> {
        let canonical = self.canonical(path)?;
        let f = self.fact(&canonical)?;
        Ok(Stat {
            directory: f["directory"].as_bool().unwrap(),
            size: f["size"].as_u64().unwrap(),
            birth_ms: f["birth_ms"].as_f64().unwrap(),
            ctime_ms: f["ctime_ms"].as_f64().unwrap(),
            mtime_ms: f["mtime_ms_exact"].as_str().map_or_else(
                || f["mtime_ms"].as_f64().unwrap(),
                |exact| exact.parse().unwrap(),
            ),
        })
    }
    fn canonical(&self, path: &Path) -> io::Result<PathBuf> {
        let mut current = path.to_owned();
        for _ in 0..40 {
            let mut prefix = PathBuf::new();
            let mut replaced = false;
            for component in current.components() {
                prefix.push(component);
                if let Some(link) = self.files.get(&prefix).and_then(|f| f["link"].as_str()) {
                    let suffix = current.strip_prefix(&prefix).unwrap();
                    let target = resolve(prefix.parent().unwrap(), link);
                    // Joining an empty suffix adds a trailing separator, unlike
                    // realpath. A project-root symlink must retain the exact root.
                    current = if suffix.as_os_str().is_empty() {
                        target
                    } else {
                        target.join(suffix)
                    };
                    replaced = true;
                    break;
                }
            }
            if !replaced {
                self.fact(&current)?;
                return Ok(current);
            }
        }
        Err(io::ErrorKind::InvalidInput.into())
    }
    fn project_entries(&self, _root: &Path, path: &Path) -> io::Result<Vec<(String, bool)>> {
        self.entries(path)
    }
    fn project_stat(&self, _root: &Path, path: &Path) -> io::Result<(Stat, bool)> {
        let stat = self.stat(path)?;
        let is_file = !stat.directory;
        Ok((stat, is_file))
    }
    fn project_read(&self, _root: &Path, path: &Path, limit: usize) -> io::Result<Vec<u8>> {
        let mut bytes = self.read(path)?;
        bytes.truncate(limit);
        Ok(bytes)
    }
}
fn fixture() -> (Metadata<Snapshot>, Value) {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/project-files/cases.json"
    ))
    .unwrap();
    (snapshot_metadata(&fixture), fixture)
}
pub(crate) fn snapshot_metadata(fixture: &Value) -> Metadata<Snapshot> {
    let files = fixture["files"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| (PathBuf::from(k), v.clone()))
        .collect();
    Metadata {
        home: fixture["home"].as_str().unwrap().into(),
        cwd: fixture["cwd"].as_str().unwrap().into(),
        files: Snapshot { files },
    }
}
#[test]
fn raw_ts_responses_match_byte_for_byte() {
    let (metadata, fixture) = fixture();
    for case in fixture["cases"].as_array().unwrap() {
        let request = case["request"].as_str().unwrap();
        let Route::LocalResponse(actual) =
            route_with_metadata(&parse(request.as_bytes()).unwrap(), Some(&metadata))
        else {
            panic!("forwarded: {request}")
        };
        assert_eq!(
            String::from_utf8(actual).unwrap(),
            case["response"].as_str().unwrap(),
            "{request}"
        );
    }
}
#[test]
fn normalized_paths_accept_inside_and_reject_outside_like_ts() {
    let (metadata, fixture) = fixture();
    for case in fixture["cases"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["tag"] == "security-comparison")
    {
        let Route::LocalResponse(actual) = route_with_metadata(
            &parse(case["request"].as_str().unwrap().as_bytes()).unwrap(),
            Some(&metadata),
        ) else {
            panic!("forwarded unsafe path")
        };
        assert_eq!(actual, case["response"].as_str().unwrap().as_bytes());
    }
}
#[test]
fn node_runtime_sort_stat_and_decoding_facts() {
    let data: Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/project-files/node-runtime.json"
    ))
    .unwrap();
    let mut entries: Vec<String> = serde_json::from_value(data["readdir"].clone()).unwrap();
    let collator = Collator::try_new(Default::default(), Default::default()).unwrap();
    entries.sort_by(|a, b| collator.compare(a, b));
    assert_eq!(serde_json::to_value(entries).unwrap(), data["localeOrder"]);
    let bytes = [0, 255, 192, 175, 226, 130, 172, 237, 160, 128];
    for case in data["binary"].as_array().unwrap() {
        let n = case["n"].as_u64().unwrap() as usize;
        assert_eq!(
            String::from_utf8_lossy(&bytes[..n]),
            case["utf8"].as_str().unwrap()
        );
        assert_eq!(
            STANDARD.encode(&bytes[..n]),
            case["base64"].as_str().unwrap()
        );
    }
}
struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        static SEQ: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "pdrs-files-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(fs::canonicalize(path).unwrap())
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn native_metadata(temp: &Temp) -> Metadata {
    let home = temp.0.join("home");
    let root = temp.0.join("project");
    fs::create_dir_all(home.join("projects/fixture")).unwrap();
    fs::create_dir_all(&root).unwrap();
    fs::write(home.join("projects/fixture/.cwd"), root.to_str().unwrap()).unwrap();
    Metadata {
        home,
        cwd: temp.0.clone(),
        files: NativeFiles,
    }
}
fn read_native(metadata: &Metadata, path: &str, max: Option<f64>) -> Result<JsValue> {
    let params = object(max.map(|n| ("maxBytes", JsValue::Number(n))));
    read(
        metadata,
        &params,
        metadata.cwd.join("project").to_str().unwrap(),
        path,
    )
}
fn round3() -> Value {
    serde_json::from_str(include_str!(
        "../../tests/fixtures/project-files/node-runtime-round3.json"
    ))
    .unwrap()
}
fn dispatch_read(metadata: &Metadata, path: &str, id: &Value) -> Value {
    let request = serde_json::json!({
        "type": "request", "id": id, "method": "project_file_read",
        "params": {"projectKey": metadata.cwd.join("project"), "path": path, "maxBytes": 7},
    });
    let Route::LocalResponse(response) = route_with_metadata(
        &parse(request.to_string().as_bytes()).unwrap(),
        Some(metadata),
    ) else {
        panic!("measured read must stay local")
    };
    serde_json::from_slice(&response).unwrap()
}
#[test]
fn huge_sparse_file_matches_node_size_error_before_prefix_read() {
    let probe = &round3()["largeFile"];
    let temp = Temp::new();
    let metadata = native_metadata(&temp);
    let file = File::create(temp.0.join("project/huge")).unwrap();
    file.set_len(probe["size"].as_u64().unwrap()).unwrap();
    assert_eq!(
        dispatch_read(&metadata, "huge", &probe["response"]["id"]),
        probe["response"]
    );
    // The adjacent accepted boundary remains bounded in Rust: no 2 GiB buffer.
    file.set_len(2_147_483_647).unwrap();
    assert_eq!(
        text_field(
            &read_native(&metadata, "huge", Some(7.0)).unwrap(),
            "content"
        )
        .unwrap(),
        "\0".repeat(7)
    );
    // Independently retain the maximum-prefix resource guard.
    struct Limited(usize);
    impl Read for Limited {
        fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
            assert!(b.len() <= self.0);
            self.0 -= b.len();
            b.fill(0xff);
            Ok(b.len())
        }
    }
    let bytes = read_prefix(&mut Limited(10_000_000), 10_000_000).unwrap();
    assert_eq!(bytes.len(), 10_000_000);
}
#[cfg(unix)]
#[test]
fn native_path_and_symlink_boundaries() {
    use std::os::unix::fs::symlink;
    let temp = Temp::new();
    let metadata = native_metadata(&temp);
    let root = temp.0.join("project");
    fs::write(root.join("ok"), "\u{feff}hello").unwrap();
    fs::write(temp.0.join("outside"), "SECRET").unwrap();
    symlink("ok", root.join("internal")).unwrap();
    symlink("../outside", root.join("escape")).unwrap();
    symlink("missing", root.join("broken")).unwrap();
    symlink(&temp.0, root.join("dir-escape")).unwrap();
    for path in [
        "internal",
        "./a/../ok",
        "../project/ok",
        root.join("ok").to_str().unwrap(),
        root.join("a/../ok").to_str().unwrap(),
    ] {
        let result = read_native(&metadata, path, None).unwrap();
        assert_eq!(text_field(&result, "content").unwrap(), "\u{feff}hello");
        assert_eq!(text_field(&result, "path").unwrap(), path);
    }
    for path in [
        "../outside",
        "dir-escape/outside",
        "escape",
        "broken",
        "missing",
        temp.0.join("outside").to_str().unwrap(),
        "ok/../../outside",
    ] {
        assert_eq!(
            read_native(&metadata, path, None).unwrap_err().0,
            "PATH_NOT_ALLOWED",
            "{path}"
        );
    }
    assert!(matches!(
        read_native(&metadata, ".", None),
        Ok(JsValue::Null)
    ));
    // Simulate a symlink swap between canonicalization and open, at both leaf
    // and intermediate components. Opening the previously checked path fails.
    let canonical = fs::canonicalize(root.join("ok")).unwrap();
    fs::remove_file(root.join("ok")).unwrap();
    symlink("../outside", root.join("ok")).unwrap();
    assert!(open_beneath(&root, &canonical).is_err());
    fs::create_dir(root.join("dir")).unwrap();
    fs::write(root.join("dir/file"), "inside").unwrap();
    let canonical = fs::canonicalize(root.join("dir/file")).unwrap();
    fs::rename(root.join("dir"), root.join("old-dir")).unwrap();
    symlink(&temp.0, root.join("dir")).unwrap();
    assert!(open_beneath(&root, &canonical).is_err());
}

#[cfg(unix)]
#[test]
fn checked_path_replacements_cannot_read_outside_or_fall_back_to_ts() {
    struct ReplacingFiles {
        target: PathBuf,
        outside: PathBuf,
        parent: bool,
        after_stat: bool,
        swapped: std::cell::Cell<bool>,
    }
    impl ReplacingFiles {
        fn swap(&self) -> io::Result<()> {
            assert!(!self.swapped.replace(true));
            let (from, to) = if self.parent {
                (
                    self.target.parent().unwrap(),
                    self.outside.parent().unwrap(),
                )
            } else {
                (self.target.as_path(), self.outside.as_path())
            };
            fs::rename(from, from.with_extension("saved"))?;
            std::os::unix::fs::symlink(to, from)
        }
    }
    impl Files for ReplacingFiles {
        fn entries(&self, path: &Path) -> io::Result<Vec<(String, bool)>> {
            NativeFiles.entries(path)
        }
        fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
            NativeFiles.read(path)
        }
        fn stat(&self, path: &Path) -> io::Result<Stat> {
            NativeFiles.stat(path)
        }
        fn canonical(&self, path: &Path) -> io::Result<PathBuf> {
            let result = NativeFiles.canonical(path)?;
            if path == self.target && !self.after_stat {
                self.swap()?;
            }
            Ok(result)
        }
        fn project_stat(&self, root: &Path, path: &Path) -> io::Result<(Stat, bool)> {
            NativeFiles.project_stat(root, path)
        }
        fn project_read(&self, root: &Path, path: &Path, limit: usize) -> io::Result<Vec<u8>> {
            if self.after_stat {
                self.swap()?;
            }
            NativeFiles.project_read(root, path, limit)
        }
    }
    // Match all four independently measured TS races, using production Rust IO.
    let probes: Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/project-files/race-probes.json"
    ))
    .unwrap();
    assert_eq!(probes["races"].as_array().unwrap().len(), 4);
    for probe in probes["races"].as_array().unwrap() {
        assert_eq!(probe["result"]["content"], "OUTSIDE_SECRET");
        let temp = Temp::new();
        let native = native_metadata(&temp);
        let root = temp.0.join("project");
        let target = root.join("dir/file");
        let outside = temp.0.join("outside/file");
        fs::create_dir(target.parent().unwrap()).unwrap();
        fs::create_dir(outside.parent().unwrap()).unwrap();
        fs::write(&target, "INSIDE").unwrap();
        fs::write(&outside, "OUTSIDE_SECRET").unwrap();
        let metadata = Metadata {
            home: native.home,
            cwd: native.cwd,
            files: ReplacingFiles {
                target,
                outside,
                parent: probe["component"] == "parent",
                after_stat: probe["stage"] == "after-stat",
                swapped: std::cell::Cell::new(false),
            },
        };
        let request = serde_json::json!({
            "type": "request", "id": "race", "method": "project_file_read",
            "params": {"projectKey": root, "path": "dir/file"},
        })
        .to_string();
        let Route::LocalResponse(bytes) =
            route_with_metadata(&parse(request.as_bytes()).unwrap(), Some(&metadata))
        else {
            panic!("raced request forwarded to vulnerable TS: {probe}");
        };
        assert!(metadata.files.swapped.get());
        let response: Value = serde_json::from_slice(&bytes).unwrap();
        if metadata.files.after_stat {
            assert_eq!(response["ok"], false, "{probe}: {response}");
        } else {
            assert_eq!(response["ok"], true);
            assert_eq!(response["result"], Value::Null);
        }
    }
}
#[test]
fn depth_boundary_and_hidden_filters() {
    let temp = Temp::new();
    let metadata = native_metadata(&temp);
    let root = temp.0.join("project");
    let mut nested = root.clone();
    for _ in 0..64 {
        nested = nested.join("d");
        fs::create_dir(&nested).unwrap();
    }
    fs::write(nested.join("visible"), "yes").unwrap();
    let params = object([]);
    assert!(list(&metadata, &params, root.to_str().unwrap(), "").is_ok());
    nested = nested.join("d");
    fs::create_dir(&nested).unwrap();
    assert_eq!(
        list(&metadata, &params, root.to_str().unwrap(), "")
            .unwrap_err()
            .0,
        "FILE_INDEX_LIMIT"
    );
}
#[test]
fn native_check_against_freshly_recorded_tree_when_requested() {
    if std::env::var_os("PDRS_FILES_NATIVE_CHECK").is_none() {
        return;
    }
    let (_, fixture) = fixture();
    let metadata = Metadata {
        home: fixture["home"].as_str().unwrap().into(),
        cwd: fixture["cwd"].as_str().unwrap().into(),
        files: NativeFiles,
    };
    for case in fixture["cases"].as_array().unwrap() {
        let request = parse(case["request"].as_str().unwrap().as_bytes()).unwrap();
        let Route::LocalResponse(actual) = route_with_metadata(&request, Some(&metadata)) else {
            panic!("forwarded")
        };
        assert_eq!(
            String::from_utf8(actual).unwrap(),
            case["response"].as_str().unwrap(),
            "{}",
            case["request"]
        );
    }
}

#[test]
fn node_locale_precedence_collation_and_case_mapping() {
    let data: Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/project-files/node-runtime.json"
    ))
    .unwrap();
    for case in data["localeProbe"].as_array().unwrap() {
        let locale = node_locale(|key| case["env"][key].as_str().map(str::to_owned));
        assert_eq!(locale.to_string(), case["locale"].as_str().unwrap());
        let collator = Collator::try_new(locale.clone().into(), Default::default()).unwrap();
        let mut names = vec!["z", "ä", "Z", "a", "A", "你好", "文件", "😀", "_x"];
        names.sort_by(|a, b| collator.compare(a, b));
        assert_eq!(serde_json::to_value(names).unwrap(), case["order"]);
        let lower: Vec<_> = ["Iİıi", "ΟΣ", "İ", "ẞ"]
            .into_iter()
            .map(|s| lowercase(s, &locale.id, false))
            .collect();
        assert_eq!(serde_json::to_value(lower).unwrap(), case["lower"]);
    }
}
#[test]
fn node_compare_utf16_keeps_unpaired_surrogate_weights() {
    // Node v22.22.0 measurement (RESULT「修复轮 2」surrogate-probe.mjs):
    // `['zzz','\ud800a','\ud800b','\ud801','\udfff','\ufffd']
    //   .sort((a,b)=>a.localeCompare(b))`
    // yields this ascending order: unpaired surrogates are distinct, in
    // code-unit order, above "zzz" and below U+FFFD.
    let collator = node_collator(&"en-US".parse().unwrap()).unwrap();
    let units = |units: &[u16]| JsString::from_utf16(units.to_vec());
    let mut mixed = [
        units(&[u16::from(b'z'); 3]),
        units(&[0xD800, u16::from(b'a')]),
        units(&[0xD800, u16::from(b'b')]),
        units(&[0xD801]),
        units(&[0xDFFF]),
        units(&[0xFFFD]),
    ];
    mixed.sort_by(|left, right| node_compare_utf16(&collator, left, right));
    let order: Vec<Vec<u16>> = mixed.iter().map(|value| value.units().to_vec()).collect();
    assert_eq!(
        order,
        vec![
            vec![u16::from(b'z'); 3],
            vec![0xD800, u16::from(b'a')],
            vec![0xD800, u16::from(b'b')],
            vec![0xD801],
            vec![0xDFFF],
            vec![0xFFFD],
        ]
    );
    // All 2048 unpaired surrogates are distinct and strictly increasing under
    // the shared comparator, not folded onto one U+FFFD weight.
    let mut surrogates: Vec<JsString> = (0xD800..=0xDFFF).map(|unit| units(&[unit])).collect();
    surrogates.sort_by(|left, right| node_compare_utf16(&collator, left, right));
    for (index, value) in surrogates.iter().enumerate() {
        assert_eq!(value.units().to_vec(), vec![0xD800 + index as u16]);
    }
    for pair in surrogates.windows(2) {
        assert!(node_compare_utf16(&collator, &pair[0], &pair[1]).is_lt());
    }
    // Valid UTF-16, including surrogate pairs, retains equality.
    for text in ["", "z", "\u{1f600}", "你好", "\u{fffd}"] {
        let value = JsString::from(text);
        assert_eq!(
            node_compare_utf16(&collator, &value, &value),
            std::cmp::Ordering::Equal
        );
    }
}

#[test]
fn resource_counter_counts_disappearing_entries() {
    let (mut metadata, fixture) = fixture();
    let root = PathBuf::from(fixture["root"].as_str().unwrap());
    let entries: Vec<_> = (0..100_000)
        .map(|i| serde_json::json!([format!("gone-{i}"), false]))
        .collect();
    metadata.files.files.get_mut(&root).unwrap()["entries"] = Value::Array(entries);
    // The success half is about the counter, not the wall clock: an unbounded
    // deadline keeps 100k stat calls deterministic under parallel cargo load.
    // The production deadline is checked independently below (#271).
    let value = list_with_deadline(
        &metadata,
        &object([]),
        root.to_str().unwrap(),
        "",
        Duration::MAX,
    )
    .unwrap();
    assert_eq!(encode(value.get("items").unwrap()), "[]");
    metadata.files.files.get_mut(&root).unwrap()["entries"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!(["gone-extra", false]));
    assert_eq!(
        list_with_deadline(
            &metadata,
            &object([]),
            root.to_str().unwrap(),
            "",
            Duration::MAX,
        )
        .unwrap_err()
        .0,
        "FILE_INDEX_LIMIT"
    );
}

#[test]
fn resource_deadline_fails_closed() {
    let (metadata, fixture) = fixture();
    let root = PathBuf::from(fixture["root"].as_str().unwrap());
    // Test expiry itself with an already-expired clock, without sleeping or
    // making 100k entries fit inside a machine-dependent five-second window.
    let mut scan = Scan {
        metadata: &metadata,
        root: &root,
        collator: Collator::try_new(Default::default(), Default::default()).unwrap(),
        query: "",
        locale: "und".parse().unwrap(),
        include_dirs: true,
        started: Instant::now() - Duration::from_secs(6),
        deadline: FILE_INDEX_DEADLINE,
        count: 0,
        visited: HashSet::new(),
        entries: Vec::new(),
    };
    assert_eq!(scan.walk(&root, 0).unwrap_err().0, "FILE_INDEX_LIMIT");
}
#[cfg(unix)]
#[test]
fn special_files_do_not_block_and_unreadable_files_still_list() {
    use std::os::unix::fs::PermissionsExt;
    let temp = Temp::new();
    let metadata = native_metadata(&temp);
    let root = temp.0.join("project");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(root.join("fifo"))
            .status()
            .unwrap()
            .success()
    );
    assert!(matches!(
        read_native(&metadata, "fifo", None),
        Ok(JsValue::Null)
    ));
    fs::write(root.join("unreadable"), "hidden").unwrap();
    fs::set_permissions(root.join("unreadable"), fs::Permissions::from_mode(0o0)).unwrap();
    let listed = list(&metadata, &object([]), root.to_str().unwrap(), "").unwrap();
    let JsValue::Array(items) = listed.get("items").unwrap() else {
        panic!("items")
    };
    assert_eq!(items.len(), 1);
    assert_eq!(text_field(&items[0], "name").unwrap(), "unreadable");
    // Root can read chmod(0) files; check EACCES only when the OS denies access.
    if File::open(root.join("unreadable")).is_err() {
        assert_eq!(
            read_native(&metadata, "unreadable", None).unwrap_err().0,
            "EACCES"
        );
    }
}

#[cfg(unix)]
#[test]
fn execute_only_parent_still_allows_reading_known_file() {
    use std::os::unix::fs::PermissionsExt;
    let temp = Temp::new();
    let metadata = native_metadata(&temp);
    let root = temp.0.join("project");
    fs::create_dir(root.join("xonly")).unwrap();
    fs::write(root.join("xonly/readable"), "readable").unwrap();
    fs::set_permissions(root.join("xonly"), fs::Permissions::from_mode(0o111)).unwrap();
    let result = read_native(&metadata, "xonly/readable", None);
    fs::set_permissions(root.join("xonly"), fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(text_field(&result.unwrap(), "content").unwrap(), "readable");
}

#[test]
fn utf16_paths_use_normalized_containment_and_preserve_response_string() {
    let temp = Temp::new();
    let metadata = native_metadata(&temp);
    let root = temp.0.join("project");
    fs::write(root.join("\u{fffd}"), "inside").unwrap();
    let probes: Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/project-files/race-probes.json"
    ))
    .unwrap();
    for probe in probes["utf16"].as_array().unwrap() {
        let raw = probe["request"].as_str().unwrap();
        let old_root = text_field(&parse_json(raw).unwrap(), "projectKey").unwrap();
        // Relocate only the oracle's temporary root; path UTF-16 and all response
        // bytes otherwise remain untouched.
        let params = parse_json(&raw.replace(&old_root, root.to_str().unwrap())).unwrap();
        let result = execute(&metadata, "project_file_read", &params).unwrap();
        let response = match result {
            Ok(result) => object([("result", result)]),
            Err((code, message)) => object([(
                "error",
                object([("code", code.into()), ("message", message.as_str().into())]),
            )]),
        };
        assert_eq!(
            encode(&response),
            probe["response"]
                .as_str()
                .unwrap()
                .replace(&old_root, root.to_str().unwrap())
        );
    }
}

/// Any `projectKey` containing a lone surrogate stays host-owned even when
/// `..` would fold the segment away: `text_field` rejects the UTF-16 key before
/// `registered`, so this port must not manufacture `Unknown projectKey` from a
/// lossy string. TS resolves the raw units and owns both the fold and wording.
#[test]
fn project_key_with_folded_lone_surrogate_stays_host_owned() {
    let temp = Temp::new();
    let metadata = native_metadata(&temp);
    let params = parse_json(r#"{"projectKey":"project-\ud800/../missing","limit":1}"#).unwrap();
    assert!(execute(&metadata, "project_files_list", &params).is_none());
    let request = parse_json(
        r#"{"type":"request","id":"x","method":"project_files_list","params":{"projectKey":"project-\ud800/../missing","limit":1}}"#,
    )
    .unwrap();
    assert!(matches!(
        route_with_metadata(&request, Some(&metadata)),
        Route::Forward
    ));
}

#[test]
fn deep_max_bytes_array_is_a_local_error_not_a_stack_overflow() {
    let temp = Temp::new();
    let metadata = native_metadata(&temp);
    fs::write(temp.0.join("project/a.txt"), "inside").unwrap();
    let mut max = JsValue::Number(7.0);
    for _ in 0..1_000_000 {
        max = JsValue::Array(vec![max]);
    }
    let params = object([
        (
            "projectKey",
            metadata.cwd.join("project").to_str().unwrap().into(),
        ),
        ("path", "a.txt".into()),
        ("maxBytes", max),
    ]);
    let (code, message) = execute(&metadata, "project_file_read", &params)
        .expect("answered locally, never forwarded")
        .unwrap_err();
    let oracle: serde_json::Value =
        serde_json::from_str(include_str!("../../tests/fixtures/runtime-boundaries.json")).unwrap();
    let expected = &oracle["wireErrors"]["projectFile"]["error"];
    assert_eq!(code, expected["code"].as_str().unwrap());
    assert_eq!(message, expected["message"].as_str().unwrap());
}

#[test]
fn unmeasured_io_diagnostics_are_not_fabricated_locally() {
    let error = fs_error(
        "project_file_read",
        io::Error::other("unknown platform error"),
        "open",
        Path::new("file"),
    );
    assert!(local_result(Err(error)).is_none());
    assert!(local_result(Err(denied(ESCAPE))).is_some());
    let raced_link = fs_error(
        "project_file_read",
        checked_open_error(io::Error::from_raw_os_error(libc::ELOOP)),
        "open",
        Path::new("link"),
    );
    assert_eq!(raced_link.0, "PATH_NOT_ALLOWED");
    assert!(local_result(Err(raced_link)).is_some());
}

#[test]
fn node_round3_fs_error_codes_and_messages() {
    for probe in round3()["fsErrors"].as_array().unwrap() {
        let errno = match probe["nodeError"]["code"].as_str().unwrap() {
            "ENOENT" => libc::ENOENT,
            "EACCES" => libc::EACCES,
            "ENOTDIR" => libc::ENOTDIR,
            other => panic!("unhandled recorded errno: {other}"),
        };
        let (code, message) = fs_error(
            "project_file_read",
            io::Error::from_raw_os_error(errno),
            probe["operation"].as_str().unwrap(),
            Path::new(probe["path"].as_str().unwrap()),
        );
        assert_eq!(
            serde_json::json!({"code": code, "message": message}),
            probe["response"]["error"]
        );
    }
}

#[cfg(unix)]
#[test]
fn native_read_errors_match_node_ws_responses() {
    use std::os::unix::fs::PermissionsExt;
    let temp = Temp::new();
    let metadata = native_metadata(&temp);
    let root = temp.0.join("project");
    fs::write(root.join("bom.txt"), "data").unwrap();
    fs::write(root.join("unreadable"), "hidden").unwrap();
    fs::set_permissions(root.join("unreadable"), fs::Permissions::from_mode(0o0)).unwrap();
    let probes = round3();
    for probe in probes["readErrors"].as_array().unwrap() {
        let path = probe["path"].as_str().unwrap();
        // Root CI cannot exercise EACCES; the checked-in Node record is non-root.
        if path == "unreadable" && File::open(root.join(path)).is_ok() {
            continue;
        }
        let expected: Value = serde_json::from_str(
            &probe["response"]
                .to_string()
                .replace(probes["root"].as_str().unwrap(), root.to_str().unwrap()),
        )
        .unwrap();
        assert_eq!(dispatch_read(&metadata, path, &expected["id"]), expected);
    }
    fs::set_permissions(root.join("unreadable"), fs::Permissions::from_mode(0o600)).unwrap();
}

#[test]
fn node_round3_locale_fallback_and_latin1_case_mapping() {
    for probe in round3()["localeProbe"].as_array().unwrap() {
        let locale = node_locale(|key| probe["env"][key].as_str().map(str::to_owned));
        if !probe["env"]["LANG"].as_str().unwrap().contains("-u-") {
            assert_eq!(locale.to_string(), probe["locale"].as_str().unwrap());
        }
        if matches!(
            locale.to_string().as_str(),
            "vi-VN-u-co-phonebk" | "ee-u-co-phonebk"
        ) {
            // Reproduce the old expect panic's actual failing constructor.
            assert!(Collator::try_new(locale.clone().into(), Default::default()).is_err());
        }
        let collator = node_collator(&locale).unwrap();
        let mut names = vec!["z", "ä", "Z", "a", "A", "你好", "文件", "😀", "_x"];
        names.sort_by(|a, b| collator.compare(a, b));
        assert_eq!(
            serde_json::to_value(names).unwrap(),
            probe["order"],
            "{locale}"
        );
        for lower in probe["lower"].as_array().unwrap() {
            assert_eq!(
                lowercase(lower["input"].as_str().unwrap(), &locale.id, false),
                lower["output"].as_str().unwrap(),
                "{locale}: {lower}"
            );
        }
    }
}

#[test]
fn node_round3_base64_uses_utf16_low_bytes() {
    let probes = round3()["base64Probe"].as_array().unwrap().clone();
    // Real Node outcomes must cover the alphabet swap and '=' termination.
    for needle in ["+", "/", "=", "-", "_"] {
        assert!(
            probes
                .iter()
                .any(|p| p["input"].as_str().unwrap().contains(needle)),
            "no base64 probe with {needle:?}"
        );
    }
    for probe in &probes {
        let input = parse_json(probe["input"].as_str().unwrap()).unwrap();
        let bytes = decode_base64(input.as_string().unwrap().units()).unwrap();
        let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        assert_eq!(hex, probe["hex"].as_str().unwrap(), "{probe}");
    }
}

#[test]
fn node_round3_cursor_dispatch_preserves_surrogates_and_offset() {
    let probes = round3();
    // Relocate the original tree snapshot to the new oracle root. Keep the
    // recorded cursor/signature and complete Node response unchanged.
    let (mut metadata, fixture) = fixture();
    let old_cwd = fixture["cwd"].as_str().unwrap();
    let new_cwd = Path::new(probes["root"].as_str().unwrap())
        .parent()
        .unwrap()
        .to_str()
        .unwrap();
    metadata.cwd = new_cwd.into();
    metadata.home = metadata.cwd.join("home");
    metadata.files.files = metadata
        .files
        .files
        .into_iter()
        .map(|(path, mut value)| {
            if let Some(link) = value["link"].as_str() {
                value["link"] = link.replace(old_cwd, new_cwd).into();
            }
            if path.file_name().is_some_and(|name| name == ".cwd") {
                let content = STANDARD.decode(value["base64"].as_str().unwrap()).unwrap();
                value["base64"] = STANDARD
                    .encode(
                        String::from_utf8(content)
                            .unwrap()
                            .replace(old_cwd, new_cwd),
                    )
                    .into();
            }
            (
                PathBuf::from(path.to_str().unwrap().replace(old_cwd, new_cwd)),
                value,
            )
        })
        .collect();
    let original = parse_json(include_str!(
        "../../tests/fixtures/project-files/node-runtime-round3.json"
    ))
    .unwrap();
    let JsValue::Array(original_probes) = original.get("cursorProbe").unwrap() else {
        panic!("cursorProbe")
    };
    for (probe, original_probe) in probes["cursorProbe"]
        .as_array()
        .unwrap()
        .iter()
        .zip(original_probes)
    {
        let request = format!(
            r#"{{"type":"request","id":{},"method":"project_files_list","params":{{"projectKey":{},"limit":2,"cursor":{}}}}}"#,
            probe["response"]["id"],
            probes["root"],
            probe["cursor"].as_str().unwrap(),
        );
        let Route::LocalResponse(response) =
            route_with_metadata(&parse(request.as_bytes()).unwrap(), Some(&metadata))
        else {
            panic!("cursor forwarded")
        };
        assert_eq!(
            response,
            encode(original_probe.get("response").unwrap()).as_bytes(),
            "{}",
            probe["variant"]
        );
    }
}

fn round4() -> Value {
    serde_json::from_str(include_str!(
        "../../tests/fixtures/project-files/node-runtime-round4.json"
    ))
    .unwrap()
}

#[test]
fn node_round4_sliced_latin1_list_responses() {
    run_node_locale("round4");
}
#[test]
fn node_round5_two_byte_parent_list_responses() {
    run_node_locale("round5");
}
fn locale_fixture(kind: &str) -> Value {
    match kind {
        "round4" => round4(),
        "round5" => serde_json::from_str(include_str!(
            "../../tests/fixtures/project-files/node-runtime-round5.json"
        ))
        .unwrap(),
        _ => panic!("unknown locale fixture {kind}"),
    }
}
fn run_node_locale(kind: &str) {
    let fixture = locale_fixture(kind);
    crate::test_process::run_cases(
        "project_files::tests::node_locale_worker",
        vec![(
            kind.into(),
            vec![
                (
                    "LANG".into(),
                    fixture["env"]["LANG"].as_str().unwrap().into(),
                ),
                ("PDRS_LOCALE_FIXTURE".into(), kind.into()),
            ],
        )],
    );
}
#[test]
#[ignore = "only launched by locale fixture parents"]
fn node_locale_worker() {
    let fixture = locale_fixture(&std::env::var("PDRS_LOCALE_FIXTURE").unwrap());
    let metadata = snapshot_metadata(&fixture);
    for case in fixture["cases"].as_array().unwrap() {
        let request = case["request"].as_str().unwrap();
        let Route::LocalResponse(response) =
            route_with_metadata(&parse(request.as_bytes()).unwrap(), Some(&metadata))
        else {
            panic!("forwarded: {request}")
        };
        assert_eq!(
            String::from_utf8(response).unwrap(),
            case["response"].as_str().unwrap(),
            "{}: {request}",
            case["tag"]
        );
    }
    crate::test_process::receipt(&std::env::var("PDRS_LOCALE_FIXTURE").unwrap());
}

#[cfg(unix)]
#[test]
fn node_round4_unreadable_huge_file_reports_open_error_first() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = round4();
    let temp = Temp::new();
    let metadata = native_metadata(&temp);
    let root = temp.0.join("project");
    let path = root.join("huge");
    File::create(&path)
        .unwrap()
        .set_len(fixture["largeFile"]["size"].as_u64().unwrap())
        .unwrap();
    for (mode, key) in [(0o0, "denied"), (0o600, "readable")] {
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
        // Root cannot reproduce EACCES. Non-root runs exercise both responses.
        if mode == 0 && File::open(&path).is_ok() {
            continue;
        }
        let expected: Value = serde_json::from_str(
            &fixture["largeFile"][key]
                .to_string()
                .replace(fixture["root"].as_str().unwrap(), root.to_str().unwrap()),
        )
        .unwrap();
        assert_eq!(dispatch_read(&metadata, "huge", &expected["id"]), expected);
    }
}

fn cjk_fixture() -> Value {
    serde_json::from_str(include_str!(
        "../../tests/fixtures/collation/cjk-node22.json"
    ))
    .unwrap()
}

#[test]
fn ascii_and_non_ascii_file_lists_stay_local() {
    let temp = Temp::new();
    let metadata = native_metadata(&temp);
    let root = temp.0.join("project");
    fs::write(root.join("z.txt"), "ASCII").unwrap();
    fs::write(root.join("a.txt"), "ASCII").unwrap();
    let request = |query: &str| {
        parse(
            serde_json::json!({
                "type": "request", "id": "files", "method": "project_files_list",
                "params": {"projectKey": root, "query": query, "limit": 1, "includeDirs": false}
            })
            .to_string()
            .as_bytes(),
        )
        .unwrap()
    };
    let Route::LocalResponse(bytes) = route_with_metadata(&request(""), Some(&metadata)) else {
        panic!("ASCII list must stay local");
    };
    let response: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(response["result"]["items"][0]["name"], "a.txt");
    assert!(response["result"]["nextCursor"].is_string());
    // Query text itself is not a sort key.
    assert!(matches!(
        route_with_metadata(&request("é"), Some(&metadata)),
        Route::LocalResponse(_)
    ));
    for name in ["中", "é", "😀"] {
        // Nested directory names participate in traversal sorting even when
        // includeDirs=false and the query only matches an ASCII file.
        let directory = root.join("nested");
        fs::create_dir(&directory).unwrap();
        fs::write(directory.join(name), "content").unwrap();
        for query in ["", "a.txt", "absent"] {
            assert!(
                matches!(
                    route_with_metadata(&request(query), Some(&metadata)),
                    Route::LocalResponse(_)
                ),
                "{name}: {query}"
            );
        }
        // Reading the same non-ASCII path never consults collation.
        let read = parse(
            serde_json::json!({
                "type": "request", "id": "read", "method": "project_file_read",
                "params": {"projectKey": root, "path": format!("nested/{name}")}
            })
            .to_string()
            .as_bytes(),
        )
        .unwrap();
        let Route::LocalResponse(bytes) = route_with_metadata(&read, Some(&metadata)) else {
            panic!("read forwarded")
        };
        let response: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(response["result"]["content"], "content");
        fs::remove_file(directory.join(name)).unwrap();
        fs::remove_dir(directory).unwrap();
        assert!(matches!(
            route_with_metadata(&request(""), Some(&metadata)),
            Route::LocalResponse(_)
        ));
    }
    #[cfg(unix)]
    {
        // All visited dirents are ASCII, but an ASCII symlink resolves into an
        // excluded directory. Its canonical relativePath enters the final sort.
        fs::create_dir(root.join(".git")).unwrap();
        fs::write(root.join(".git/é"), "content").unwrap();
        std::os::unix::fs::symlink(".git/é", root.join("alias")).unwrap();
        assert!(matches!(
            route_with_metadata(&request("alias"), Some(&metadata)),
            Route::LocalResponse(_)
        ));
        // The non-ASCII target does not participate in either sort when filtered.
        assert!(matches!(
            route_with_metadata(&request("absent"), Some(&metadata)),
            Route::LocalResponse(_)
        ));
    }
}

pub(crate) fn assert_cjk_fixture_complete(fixture: &Value) {
    let rows = fixture["rows"].as_array().expect("CJK rows array");
    assert_eq!(rows.len(), 9, "CJK environment rows");
    let mut total = 0;
    for row in rows {
        let count = row["cases"].as_array().expect("CJK cases array").len();
        assert_eq!(count, 12, "CJK requests per environment");
        total += count;
    }
    assert_eq!(total, 108, "CJK total requests");
}

#[test]
fn cjk_fixture_rejects_empty_or_truncated_rows_and_requests() {
    let fixture = cjk_fixture();
    assert_cjk_fixture_complete(&fixture);
    for mutation in 0..3 {
        let mut broken = fixture.clone();
        let rows = broken["rows"].as_array_mut().unwrap();
        match mutation {
            0 => rows.clear(),
            1 => {
                rows.pop();
            }
            _ => {
                rows[0]["cases"].as_array_mut().unwrap().pop();
            }
        }
        assert!(std::panic::catch_unwind(|| assert_cjk_fixture_complete(&broken)).is_err());
    }
}

#[test]
fn cjk_requests_match_node_byte_for_byte() {
    let fixture = cjk_fixture();
    assert_cjk_fixture_complete(&fixture);
    let rows = fixture["rows"].as_array().unwrap();
    crate::test_process::run_cases(
        "project_files::tests::cjk_matrix_worker",
        rows.iter()
            .enumerate()
            .map(|(index, row)| {
                let mut env = crate::test_process::json_env(&row["env"]);
                env.push(("PDRS_FIXTURE_ROW".into(), index.to_string()));
                (format!("cjk-files-{index}"), env)
            })
            .collect(),
    );
}
#[test]
#[ignore = "only launched by complete CJK matrix"]
fn cjk_matrix_worker() {
    let fixture = cjk_fixture();
    assert_cjk_fixture_complete(&fixture);
    let rows = fixture["rows"].as_array().unwrap();
    let index: usize = std::env::var("PDRS_FIXTURE_ROW").unwrap().parse().unwrap();
    let row = &rows[index];
    let locale = node_locale(|key| std::env::var(key).ok());
    assert_eq!(locale.to_string(), row["resolved"]["locale"]);
    let env = row["env"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_owned()))
        .collect();
    let mut metadata = snapshot_metadata(row);
    assert_eq!(row["cases"].as_array().unwrap().len(), 12);
    for case in row["cases"].as_array().unwrap() {
        let tag = case["tag"].as_str().unwrap();
        let request = parse(case["request"].as_str().unwrap().as_bytes()).unwrap();
        let route = if let Some(group) = tag.strip_prefix("catalog-") {
            metadata
                .files
                .files
                .get_mut(&metadata.home.join("pilotdeck.yaml"))
                .unwrap()["base64"] = STANDARD
                .encode(row["yamls"][group].as_str().unwrap())
                .into();
            crate::dispatch::route_catalog(&request, Some(&metadata), &env)
        } else {
            route_with_metadata(&request, Some(&metadata))
        };
        let Route::LocalResponse(bytes) = route else {
            panic!("row {index}: {tag} forwarded");
        };
        assert_eq!(
            bytes,
            case["response"].as_str().unwrap().as_bytes(),
            "row {index}: {tag}"
        );
    }
    crate::test_process::receipt(&format!("cjk-files-{index}"));
}

pub(crate) fn round6() -> Value {
    serde_json::from_str(include_str!(
        "../../tests/fixtures/project-files/io-errors-round6.json"
    ))
    .unwrap()
}
pub(crate) struct BoundaryFiles {
    inner: Snapshot,
    operation: String,
    target: PathBuf,
    errno: Option<i32>,
    pub(crate) hits: AtomicUsize,
}
impl BoundaryFiles {
    fn check(&self, operation: &str, path: &Path) -> io::Result<()> {
        if operation == self.operation && path == self.target {
            self.hits.fetch_add(1, Ordering::Relaxed);
            Err(self.errno.map_or_else(
                || io::Error::other("unmeasured error"),
                io::Error::from_raw_os_error,
            ))
        } else {
            Ok(())
        }
    }
}
impl Files for BoundaryFiles {
    fn entries(&self, path: &Path) -> io::Result<Vec<(String, bool)>> {
        self.inner.entries(path)
    }
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.inner.read(path)
    }
    fn stat(&self, path: &Path) -> io::Result<Stat> {
        self.inner.stat(path)
    }
    fn canonical(&self, path: &Path) -> io::Result<PathBuf> {
        self.check("realpath", path)?;
        self.inner.canonical(path)
    }
    fn project_stat(&self, root: &Path, path: &Path) -> io::Result<(Stat, bool)> {
        self.check("stat", path)?;
        self.inner.project_stat(root, path)
    }
    fn project_entries(&self, root: &Path, path: &Path) -> io::Result<Vec<(String, bool)>> {
        self.check("scandir", path)?;
        self.inner.project_entries(root, path)
    }
    fn project_read(&self, root: &Path, path: &Path, limit: usize) -> io::Result<Vec<u8>> {
        self.check("open", path)?;
        self.inner.project_read(root, path, limit)
    }
}
pub(crate) fn boundary_metadata(row: &Value, errno: Option<i32>) -> Metadata<BoundaryFiles> {
    let m = snapshot_metadata(row);
    Metadata {
        home: m.home,
        cwd: m.cwd,
        files: BoundaryFiles {
            inner: m.files,
            operation: row["operation"].as_str().unwrap().into(),
            target: row["target"].as_str().unwrap().into(),
            errno,
            hits: AtomicUsize::new(0),
        },
    }
}
#[test]
fn round6_project_files_call_sites_match_real_node_errors_byte_for_byte() {
    let f = round6();
    assert_eq!(f["cases"].as_array().unwrap().len(), 50);
    for row in f["cases"].as_array().unwrap() {
        let errno = match row["observed"][0]["code"].as_str().unwrap() {
            "ENOENT" => libc::ENOENT,
            "EACCES" => libc::EACCES,
            "ENOTDIR" => libc::ENOTDIR,
            "ENAMETOOLONG" => libc::ENAMETOOLONG,
            "ELOOP" => libc::ELOOP,
            other => panic!("unrecorded errno {other}"),
        };
        let m = boundary_metadata(row, Some(errno));
        let request = parse(row["request"].as_str().unwrap().as_bytes()).unwrap();
        let Route::LocalResponse(actual) = route_with_metadata(&request, Some(&m)) else {
            panic!("forwarded {row}")
        };
        assert_eq!(
            actual,
            row["response"].as_str().unwrap().as_bytes(),
            "{} {} {}",
            row["method"],
            row["boundary"],
            row["observed"][0]
        );
        assert_eq!(m.files.hits.load(Ordering::Relaxed), 1);
    }
}
#[test]
fn every_unmeasured_errno_forwards_entire_project_request_at_every_call_site() {
    let f = round6();
    // One input tree for each of the ten production fs boundaries. The errno
    // injection is a routing control, never advertised as a Node oracle.
    for row in f["cases"].as_array().unwrap().iter().step_by(5) {
        let method = row["method"].as_str().unwrap();
        let request = parse(row["request"].as_str().unwrap().as_bytes()).unwrap();
        for errno in crate::node_io::unmeasured_errnos(method)
            .into_iter()
            .map(Some)
            .chain([None])
        {
            let m = boundary_metadata(row, errno);
            assert!(
                matches!(route_with_metadata(&request, Some(&m)), Route::Forward),
                "{method} {} errno={errno:?}",
                row["boundary"]
            );
            assert_eq!(m.files.hits.load(Ordering::Relaxed), 1);
        }
    }
}

#[test]
fn native_cstr_dirent_decoding_matches_22_node_byte_sequences() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../tests/fixtures/collation/invalid-utf8-node22.json"
    ))
    .unwrap();
    let cases = fixture["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 22);
    for case in cases {
        let bytes: Vec<u8> = case["bytes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| u8::try_from(n.as_u64().unwrap()).unwrap())
            .collect();
        let name = std::ffi::CString::new(bytes).unwrap();
        let actual: Vec<u16> = node_dirent_name(&name).encode_utf16().collect();
        let expected: Vec<u16> = case["utf16"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| u16::try_from(n.as_u64().unwrap()).unwrap())
            .collect();
        assert_eq!(actual, expected, "{case}");
    }
}

#[cfg(unix)]
#[test]
fn rimraf_descends_on_every_nonempty_report() {
    // Table-driven lock of the descent predicate (node_fs.rs measured libuv):
    // a non-empty rmdir descends however it is reported — ENOTEMPTY, EEXIST,
    // or a sticky directory's EPERM — while EACCES never descends.
    let descends = |errno| super::descends_after_rmdir(&io::Error::from_raw_os_error(errno));
    assert!(descends(libc::ENOTEMPTY));
    assert!(descends(libc::EEXIST));
    assert!(descends(libc::EPERM));
    assert!(!descends(libc::EACCES));
}

#[test]
fn node_locale_contract() {
    const MSG: &str = "#887 Node locale contract";
    let data: Value =
        serde_json::from_str(include_str!("../../tests/fixtures/node-locale/oracle.json"))
            .expect(MSG);
    let mut mismatches = vec![];
    for case in data["localeProbe"].as_array().expect(MSG) {
        let locale = node_locale(|key| case["env"][key].as_str().map(str::to_owned));
        let collator = node_collator(&locale).expect(MSG);
        let mut names = vec!["z", "ä", "Z", "a", "A", "你好", "文件", "😀", "_x"];
        names.sort_by(|a, b| collator.compare(a, b));
        println!(
            "#887 Rust locale {} env={} locale={} order={:?}",
            case["id"], case["env"], locale, names
        );
        if locale.to_string() != case["locale"].as_str().expect(MSG)
            || serde_json::to_value(names).expect(MSG) != case["order"]
        {
            mismatches.push(case["id"].clone());
        }
    }
    assert!(mismatches.is_empty(), "{MSG}: mismatches={mismatches:?}");
}

#[test]
#[ignore = "requires Node v22.22.0 and PDRS_BASELINE"]
fn node_locale_regenerates_identically() {
    crate::test_baseline::regenerates_identically("node-locale");
}

#[cfg(target_os = "linux")]
mod linux_dirent;
