//! #133 Linux-only end-to-end parity for raw-byte dirent names.
//!
//! macOS volumes cannot hold non-UTF-8 filenames (creating one fails with
//! EILSEQ), so the function-level proof in
//! `native_cstr_dirent_decoding_matches_22_node_byte_sequences` cannot run
//! against a real directory there. This test runs on Linux only, against the
//! pinned Node v22.22.0 baseline, inside an isolated temporary tree:
//!
//! * dirent layer: `native_entries` is compared with Node's own
//!   `fs.readdir(root, { withFileTypes: true })` — the lossy name grouping
//!   (several raw names decoding to one display name), the file/dir types,
//!   and the byte-order sort. Equal decoded names keep each side's own
//!   readdir order (both sorts are stable), so no OS order is presupposed.
//! * wire layer: the full `project_files_list` responses through dispatch are
//!   compared unit by unit. A name whose lossy decoding cannot round-trip is
//!   skipped by both sides (TS `realpath(...).catch(() => undefined)` /
//!   Rust's measured-errno skip); the legal multi-byte names are listed with
//!   identical fields, ordering, cursors, and match ranges.
//!
//! Gated by `PDRS_LINUX_DIRENT_CHECK=1` plus `PDRS_BASELINE` (the pinned TS
//! checkout) so ordinary chains stay green; the Linux runner in
//! `tests/fixtures/project-files-linux-dirent/` sets both. The audit trail
//! (raw names, content hashes, the probe's own output, the receipt) is
//! published to `PDRS_LINUX_DIRENT_OUT`.
use crate::dispatch::{Route, parse, route_with_metadata};
use crate::metadata::{Metadata, NativeFiles};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs::{self, FileTimes, OpenOptions};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant, UNIX_EPOCH};

/// The pinned TS baseline every oracle in this tree is recorded against.
const PINNED_BASELINE: &str = "cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85";
/// The recorded fixtures stamp 1700000000.123456, a fractional-millisecond
/// mtime the byte-for-byte native replay already proved identical in Node.
const STAMP: Duration = Duration::new(1_700_000_000, 123_456_000);
/// Bound on the Node probe child, including reaping after a group kill.
const PROBE_DEADLINE: Duration = Duration::from_secs(180);

/// One intended directory entry: raw path segments, whether it is a
/// directory, and the content of regular files.
type RawEntry = (&'static [&'static [u8]], bool, &'static [u8]);

/// Illegal leading/continuation bytes, overlong forms, UTF-8 surrogates,
/// beyond-Unicode, truncated sequences, the legal U+FFFD and emoji, mixed
/// ASCII, three raw names decoding to one display name, illegal-named
/// directories, the excluded `dist` directory, and a legal multi-byte
/// directory holding a plain file also named `dist` (production excludes
/// only directories, so this file must be listed — the fixture anchors that
/// distinction). NUL and `/` never occur (readdir names cannot hold them).
const RAW_TREE: &[RawEntry] = &[
    (&[b"plain.txt"], false, b"ascii control file\n"),
    (
        &[b"invalid-lead\xff.txt"],
        false,
        b"invalid leading byte 0xff\n",
    ),
    (&[b"cont\xfeext"], false, b"stray continuation byte 0xfe\n"),
    (&[b"overlong\xc0\xaf"], false, b"overlong slash c0 af\n"),
    (
        &[b"nul-overlong\xe0\x80\x80"],
        false,
        b"overlong nul e0 80 80\n",
    ),
    (&[b"surrogate\xed\xa0\x80"], false, b"utf8 surrogate d800\n"),
    (
        &[b"surrogate-tail\xed\xbf"],
        false,
        b"truncated surrogate lead ed bf\n",
    ),
    (
        &[b"beyond\xf4\x90\x80\x80"],
        false,
        b"beyond unicode f4 90 80 80\n",
    ),
    (&[b"trunc2\xd0"], false, b"truncated two byte d0\n"),
    (&[b"trunc3\xe2\x82"], false, b"truncated three byte e2 82\n"),
    (
        &[b"trunc4\xf0\x9f\x98"],
        false,
        b"truncated four byte f0 9f 98\n",
    ),
    (
        &[b"\xef\xbf\xbd-fffd"],
        false,
        b"legal replacement char ef bf bd\n",
    ),
    (
        &[b"emoji\xf0\x9f\x98\x80"],
        false,
        b"legal emoji f0 9f 98 80\n",
    ),
    (&[b"caf\xc3\xa9"], false, b"mixed ascii and two byte e9\n"),
    (&[b"dup-\xff"], false, b"dup: raw ff\n"),
    (&[b"dup-\xfe"], false, b"dup: raw fe, longer\n"),
    (
        &[b"dup-\x80"],
        false,
        b"dup: raw lone continuation 0x80, longest\n",
    ),
    (
        &[b"dir-\xff", b"unreachable.txt"],
        false,
        b"under an illegal dir name\n",
    ),
    (&[b"dir-\xff"], true, b""),
    (
        &[b"dist", b"excluded.txt"],
        false,
        b"inside the excluded dist\n",
    ),
    (&[b"dist"], true, b""),
    (
        &[b"dist\xff", b"also-unreachable.txt"],
        false,
        b"under dist fffd\n",
    ),
    (&[b"dist\xff"], true, b""),
    (
        &[b"nested-\xc3\xa9", b"dist"],
        false,
        b"plain file named dist: excluded only as a directory\n",
    ),
    (
        &[b"nested-\xc3\xa9", b"inner-\xef\xbf\xbd.txt"],
        false,
        b"legal nested\n",
    ),
    (&[b"nested-\xc3\xa9"], true, b""),
];

fn raw_path(root: &Path, segments: &[&[u8]]) -> PathBuf {
    let mut path = root.to_path_buf();
    for segment in segments {
        path.push(OsString::from_vec(segment.to_vec()));
    }
    path
}

fn hex_path(segments: &[&[u8]]) -> String {
    segments
        .iter()
        .map(|segment| {
            segment
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn create_tree(root: &Path) {
    for (segments, is_directory, content) in RAW_TREE {
        let path = raw_path(root, segments);
        if *is_directory {
            fs::create_dir_all(&path).expect("#133 create raw-byte directory");
            continue;
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("#133 create raw-byte parent directory");
        }
        fs::write(&path, content).expect("#133 create raw-byte file");
        let file = OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("#133 reopen raw-byte file for stamping");
        file.set_times(FileTimes::new().set_modified(UNIX_EPOCH + STAMP))
            .expect("#133 stamp fixed mtime");
    }
}

/// Raw byte names currently on disk, independent of any lossy decoding.
fn observed_raw_names(directory: &Path) -> BTreeSet<Vec<u8>> {
    fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("#133 read {}: {error}", directory.display()))
        .map(|entry| {
            entry
                .expect("#133 read raw directory entry")
                .file_name()
                .as_bytes()
                .to_vec()
        })
        .collect()
}

/// The intended raw name set per directory, from the tree specification.
fn expected_raw_tree() -> BTreeMap<Vec<u8>, BTreeSet<Vec<u8>>> {
    let mut tree: BTreeMap<Vec<u8>, BTreeSet<Vec<u8>>> = BTreeMap::new();
    for (segments, _, _) in RAW_TREE {
        let (last, parents) = segments.split_last().expect("#133 nonempty raw path");
        let mut parent: Vec<u8> = Vec::new();
        for segment in parents {
            if !parent.is_empty() {
                parent.push(b'/');
            }
            parent.extend_from_slice(segment);
            tree.entry(parent.clone()).or_default();
        }
        tree.entry(parent).or_default().insert(last.to_vec());
    }
    tree
}

fn dirent_counts(entries: &[(String, bool)]) -> BTreeMap<String, usize> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for (name, _) in entries {
        *counts.entry(name.clone()).or_default() += 1;
    }
    counts
}

fn dirent_types(entries: &[(String, bool)]) -> BTreeMap<String, Vec<bool>> {
    let mut types: BTreeMap<String, Vec<bool>> = BTreeMap::new();
    for (name, is_directory) in entries {
        types.entry(name.clone()).or_default().push(*is_directory);
    }
    for value in types.values_mut() {
        value.sort();
    }
    types
}

fn dirent_names(entries: &[(String, bool)]) -> Vec<&str> {
    entries.iter().map(|(name, _)| name.as_str()).collect()
}

/// JSON equality with numbers compared as f64: V8 prints an integral
/// `mtimeMs` as `1700000000123` while serde_json prints `1700000000123.0`,
/// both naming the same double.
fn unit_equal(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Number(a), Value::Number(b)) => match (a.as_f64(), b.as_f64()) {
            (Some(x), Some(y)) => x == y,
            _ => a == b,
        },
        (Value::Array(a), Value::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(x, y)| unit_equal(x, y))
        }
        (Value::Object(a), Value::Object(b)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(key, value)| b.get(key).is_some_and(|other| unit_equal(value, other)))
        }
        _ => left == right,
    }
}

fn dispatch_list(metadata: &Metadata<NativeFiles>, id: &str, params: &Value) -> Value {
    let request = json!({
        "type": "request",
        "id": id,
        "method": "project_files_list",
        "params": params,
    })
    .to_string();
    let Route::LocalResponse(bytes) = route_with_metadata(
        &parse(request.as_bytes()).expect("#133 parse list request"),
        Some(metadata),
    ) else {
        panic!("#133 project_files_list forwarded to TS instead of staying local");
    };
    serde_json::from_slice(&bytes).expect("#133 parse Rust list response")
}

fn drain<T: std::io::Read + Send + 'static>(mut pipe: T) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = pipe.read_to_end(&mut buffer);
        buffer
    })
}

/// Run the Node probe child under a total deadline, draining both pipes; a
/// descendant that escaped the process group can hold them open, so no wait
/// after the deadline is unbounded.
fn run_probe(command: &mut Command) -> (ExitStatus, Vec<u8>, Vec<u8>) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("#133 spawn Node probe");
    let stdout_reader = drain(child.stdout.take().expect("#133 capture probe stdout"));
    let stderr_reader = drain(child.stderr.take().expect("#133 capture probe stderr"));
    let deadline = Instant::now() + PROBE_DEADLINE;
    let mut timed_out = false;
    let status = loop {
        match child.try_wait().expect("#133 poll Node probe") {
            Some(status) => break status,
            None if Instant::now() >= deadline => {
                timed_out = true;
                break kill_group(&mut child);
            }
            None => {}
        }
        thread::sleep(Duration::from_millis(25));
    };
    // A reader still alive here means a descendant holds the pipe open; leak
    // the thread rather than block the test on it (the group was killed).
    let bounded = |reader: thread::JoinHandle<Vec<u8>>| {
        let until = Instant::now() + Duration::from_secs(5);
        while !reader.is_finished() && Instant::now() < until {
            thread::sleep(Duration::from_millis(10));
        }
        if reader.is_finished() {
            reader.join().unwrap_or_default()
        } else {
            Vec::new()
        }
    };
    let stdout = bounded(stdout_reader);
    let stderr = bounded(stderr_reader);
    if timed_out {
        panic!(
            "#133 Node probe exceeded {PROBE_DEADLINE:?}; killed process group {}",
            child.id()
        );
    }
    (status, stdout, stderr)
}

fn kill_group(child: &mut std::process::Child) -> ExitStatus {
    let pgid = child.id();
    let _ = Command::new("kill")
        .args(["-KILL", "--", &format!("-{pgid}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    loop {
        match child.try_wait().expect("#133 reap killed Node probe") {
            Some(status) => return status,
            None => thread::sleep(Duration::from_millis(10)),
        }
    }
}

fn wire_items<'a>(response: &'a Value, index: usize, side: &str) -> &'a Vec<Value> {
    response["result"]["items"]
        .as_array()
        .unwrap_or_else(|| panic!("#133 wire response {index} has no {side} items"))
}

fn compare_wire(index: usize, rust: &Value, node: &Value) -> usize {
    assert_eq!(
        rust["ok"], node["ok"],
        "#133 wire ok flag differs from Node (response {index}): {rust} vs {node}"
    );
    assert!(
        rust["ok"] == json!(true),
        "#133 wire response {index} is not ok: {rust}"
    );
    let rust_items = wire_items(rust, index, "Rust");
    let node_items = wire_items(node, index, "Node");
    assert_eq!(
        rust_items.len(),
        node_items.len(),
        "#133 wire item count differs from Node (response {index})"
    );
    let sequence = |side: &[Value]| -> Vec<String> {
        side.iter()
            .map(|item| {
                format!(
                    "{}\u{1}{}",
                    item["name"].as_str().unwrap_or(""),
                    item["relativePath"].as_str().unwrap_or("")
                )
            })
            .collect()
    };
    assert_eq!(
        sequence(rust_items),
        sequence(node_items),
        "#133 wire item order differs from Node (response {index})"
    );
    for (item, expected) in rust_items.iter().zip(node_items) {
        assert_eq!(
            item["kind"], expected["kind"],
            "#133 wire item kind differs from Node (response {index})"
        );
        assert_eq!(
            item["size"].as_u64(),
            expected["size"].as_u64(),
            "#133 wire item size differs from Node (response {index})"
        );
        assert_eq!(
            item["mtimeMs"].as_f64(),
            expected["mtimeMs"].as_f64(),
            "#133 wire item mtimeMs differs from Node (response {index})"
        );
        assert_eq!(
            item["id"], expected["id"],
            "#133 wire item id differs from Node (response {index})"
        );
        assert!(
            unit_equal(&item["matches"], &expected["matches"]),
            "#133 wire item matches differ from Node (response {index}): {} vs {}",
            item["matches"],
            expected["matches"]
        );
    }
    assert!(
        unit_equal(&rust["result"]["nextCursor"], &node["result"]["nextCursor"]),
        "#133 wire nextCursor differs from Node (response {index})"
    );
    assert_eq!(
        rust["result"]["projectKey"], node["result"]["projectKey"],
        "#133 wire projectKey differs from Node (response {index})"
    );
    assert!(
        unit_equal(rust, node),
        "#133 wire full response differs from Node (response {index}): {rust} vs {node}"
    );
    rust_items.len()
}

#[test]
fn linux_dirent_matches_node_v22_on_raw_byte_names() {
    if std::env::var_os("PDRS_LINUX_DIRENT_CHECK").is_none() {
        return;
    }
    let baseline = PathBuf::from(std::env::var_os("PDRS_BASELINE").unwrap_or_else(|| {
        panic!("#133 requires PDRS_BASELINE (the pinned PilotDeck TS checkout)")
    }));
    let out = PathBuf::from(
        std::env::var_os("PDRS_LINUX_DIRENT_OUT").unwrap_or_else(|| {
            panic!("#133 requires PDRS_LINUX_DIRENT_OUT (the audit output directory)")
        }),
    );
    fs::create_dir_all(&out).expect("#133 create audit output directory");
    // The comparison joins locale-aware casing rules that key off an ASCII
    // root (the V8 slice rules in the fixture README); an exotic TMPDIR
    // would silently leave that measured scope.
    let temp = super::Temp::new();
    let root = temp.0.join("project");
    let root_str = root
        .to_str()
        .expect("#133 temp root is valid UTF-8")
        .to_owned();
    assert!(
        root_str.is_ascii(),
        "#133 temp root must be ASCII, found {root_str}"
    );
    let locale = crate::project_files::node_locale(|key| std::env::var(key).ok());
    assert_eq!(
        locale.to_string(),
        "en-US",
        "#133 resolved locale {locale}; run with LANG=C.UTF-8 and no LC_* override"
    );
    let metadata = super::native_metadata(&temp);
    create_tree(&root);

    // Creation self-check, by raw bytes and independent of the decoding under
    // test: every intended name exists on disk and nothing else does.
    for (directory, expected) in expected_raw_tree() {
        let path = if directory.is_empty() {
            root.clone()
        } else {
            let segments: Vec<&[u8]> = directory.split(|byte| *byte == b'/').collect();
            raw_path(&root, &segments)
        };
        assert_eq!(
            observed_raw_names(&path),
            expected,
            "#133 raw names on disk differ from the intended tree under {}",
            String::from_utf8_lossy(&directory)
        );
    }

    // Dirent layer: native_entries against Node's own readdir of the same
    // directory, both under the production byte-order sort.
    let rust_dirents = crate::project_files::native_entries(&root, &root)
        .expect("#133 native_entries on the raw tree root");
    let mut params = vec![
        json!({"projectKey": root_str}),
        json!({"projectKey": root_str, "includeDirs": false}),
        json!({"projectKey": root_str, "query": "\u{FFFD}"}),
        json!({"projectKey": root_str, "limit": 4}),
    ];
    let mut rust_responses: Vec<Value> = params
        .iter()
        .enumerate()
        .map(|(index, parameters)| {
            dispatch_list(
                &metadata,
                &format!("linux-dirent-{}", index + 1),
                parameters,
            )
        })
        .collect();
    let cursor = rust_responses[3]["result"]["nextCursor"]
        .as_str()
        .expect("#133 paginated response carries nextCursor")
        .to_owned();
    params.push(json!({"projectKey": root_str, "limit": 4, "cursor": cursor}));
    rust_responses.push(dispatch_list(
        &metadata,
        &format!("linux-dirent-{}", params.len()),
        &params[params.len() - 1],
    ));

    // One request list is the single source of truth for both sides.
    let requests: Vec<Value> = params
        .iter()
        .enumerate()
        .map(|(index, parameters)| {
            json!({
                "id": format!("linux-dirent-{}", index + 1),
                "method": "project_files_list",
                "params": parameters,
            })
        })
        .collect();
    let requests_path = out.join("requests.json");
    let staged = out.join("requests.json.tmp");
    fs::write(
        &staged,
        serde_json::to_string(&requests).expect("#133 serialize requests"),
    )
    .expect("#133 stage requests.json");
    fs::rename(&staged, &requests_path).expect("#133 publish requests.json");

    let probe = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/project-files-linux-dirent/record.mts");
    let probe_output = out.join("node-probe.json");
    let mut command = Command::new("node");
    command
        .args(["--disable-warning=ExperimentalWarning", "--import"])
        .arg(baseline.join("node_modules/tsx/dist/loader.mjs"))
        .arg(&probe)
        .arg(&baseline)
        .arg(temp.0.join("home"))
        .arg(&root)
        .arg(&requests_path)
        .arg(&probe_output)
        .env("LANG", "C.UTF-8")
        .env_remove("LC_ALL")
        .env_remove("LC_MESSAGES")
        .env_remove("LC_COLLATE")
        .env_remove("LC_CTYPE")
        .env_remove("LANGUAGE")
        .env("HOME", temp.0.join("home"))
        .env("PILOT_HOME", temp.0.join("home"))
        .env("TMPDIR", &temp.0)
        .current_dir(&temp.0);
    let (status, stdout, stderr) = run_probe(&mut command);
    assert!(
        status.success(),
        "#133 Node probe failed ({status}): {}{}",
        String::from_utf8_lossy(&stdout),
        String::from_utf8_lossy(&stderr)
    );
    let probe_document: Value =
        serde_json::from_str(&fs::read_to_string(&probe_output).expect("#133 read probe output"))
            .expect("#133 parse probe output");
    assert_eq!(
        probe_document["node"].as_str(),
        Some("v22.22.0"),
        "#133 probe Node version: {}",
        probe_document["node"]
    );
    assert_eq!(
        probe_document["icu"].as_str(),
        Some("78.2"),
        "#133 probe ICU version: {}",
        probe_document["icu"]
    );
    assert_eq!(
        probe_document["baselineHead"].as_str(),
        Some(PINNED_BASELINE),
        "#133 probe baseline HEAD: {}",
        probe_document["baselineHead"]
    );
    let node_dirents: Vec<(String, bool)> = probe_document["dirent"]["byteSorted"]
        .as_array()
        .expect("#133 probe dirent array")
        .iter()
        .map(|entry| {
            (
                entry["name"]
                    .as_str()
                    .expect("#133 probe dirent name")
                    .to_owned(),
                entry["isDirectory"]
                    .as_bool()
                    .expect("#133 probe dirent type"),
            )
        })
        .collect();

    // Dirent layer comparisons. Grouping first, then types, then the sorted
    // order: equal decoded names keep each side's own readdir order (both
    // sorts are stable), so the order check never presupposes an OS order.
    assert_eq!(
        dirent_counts(&rust_dirents),
        dirent_counts(&node_dirents),
        "#133 dirent name grouping differs from Node readdir"
    );
    assert_eq!(
        dirent_types(&rust_dirents),
        dirent_types(&node_dirents),
        "#133 dirent file/dir types differ from Node readdir"
    );
    assert_eq!(
        dirent_names(&rust_dirents),
        dirent_names(&node_dirents),
        "#133 dirent byte-order sort differs from Node readdir"
    );
    // Positive expectation from the raw specification: the lossy grouping the
    // 22-sequence table already locked at function level, now against the
    // real directory (guards against a silently short tree or probe).
    let expected_counts: BTreeMap<String, usize> = RAW_TREE
        .iter()
        .filter(|(segments, _, _)| segments.len() == 1)
        .map(|(segments, _, _)| (String::from_utf8_lossy(segments[0]).into_owned(), ()))
        .fold(BTreeMap::new(), |mut map, (name, ())| {
            *map.entry(name).or_default() += 1;
            map
        });
    assert_eq!(
        dirent_counts(&rust_dirents),
        expected_counts,
        "#133 dirent decoded names differ from the expected lossy grouping"
    );

    // Wire layer: full response parity, unit by unit.
    let node_responses = probe_document["responses"]
        .as_array()
        .expect("#133 probe responses array");
    assert_eq!(
        node_responses.len(),
        rust_responses.len(),
        "#133 probe response count differs from the request list"
    );
    let mut wire_counts = Vec::new();
    for (index, (rust, node)) in rust_responses.iter().zip(node_responses).enumerate() {
        let node: Value =
            serde_json::from_str(node["response"].as_str().expect("#133 probe response text"))
                .expect("#133 parse probe response");
        wire_counts.push(compare_wire(index, rust, &node));
    }
    // The listed set is exactly the valid-UTF-8 names outside EXCLUDED; the
    // names whose lossy decoding cannot round-trip are skipped by both sides.
    let listed: BTreeSet<String> = RAW_TREE
        .iter()
        .filter(|(segments, is_directory, _)| {
            // Production excludes only directories named in EXCLUDED; every
            // non-final segment is a directory by construction, and the final
            // one only when the entry itself is.
            segments.iter().enumerate().all(|(index, segment)| {
                let this_is_directory = index + 1 < segments.len() || *is_directory;
                std::str::from_utf8(segment).is_ok_and(|name| {
                    !(this_is_directory && super::super::EXCLUDED.contains(&name))
                })
            })
        })
        .map(|(segments, _, _)| {
            segments
                .iter()
                .map(|segment| std::str::from_utf8(segment).expect("#133 valid UTF-8 segment"))
                .collect::<Vec<_>>()
                .join("/")
        })
        .collect();
    let actual: BTreeSet<String> = rust_responses[0]["result"]["items"]
        .as_array()
        .expect("#133 first response items")
        .iter()
        .map(|item| {
            item["relativePath"]
                .as_str()
                .expect("#133 item relativePath")
                .to_owned()
        })
        .collect();
    assert_eq!(
        actual, listed,
        "#133 wire listed set differs from the valid-UTF-8 expectation"
    );

    // Content hashes for the receipt, read while the tree still exists.
    let raw_names: Vec<Value> = RAW_TREE
        .iter()
        .map(|(segments, is_directory, _content)| {
            let path = raw_path(&root, segments);
            let sha = if *is_directory {
                String::new()
            } else {
                format!(
                    "{:x}",
                    Sha256::digest(
                        fs::read(&path).expect("#133 read back raw file for the receipt hash")
                    )
                )
            };
            json!({
                "raw": hex_path(segments),
                "directory": is_directory,
                "contentSha256": sha,
            })
        })
        .collect();
    // Cleanup audit: the owned tree disappears before the receipt is written.
    drop(metadata);
    fs::remove_dir_all(&temp.0).expect("#133 remove owned temporary tree");
    assert!(
        !temp.0.exists(),
        "#133 temporary tree survived cleanup: {}",
        temp.0.display()
    );
    let receipt = json!({
        "issue": "#133",
        "root": root_str,
        "node": probe_document["node"],
        "icu": probe_document["icu"],
        "baselineHead": probe_document["baselineHead"],
        "rawNames": raw_names,
        "direntEntries": rust_dirents.len(),
        "wireItemCounts": wire_counts,
        "responses": rust_responses.len(),
        "cleanup": true,
    });
    let staged = out.join("receipt.json.tmp");
    fs::write(
        &staged,
        serde_json::to_string_pretty(&receipt).expect("#133 serialize receipt"),
    )
    .expect("#133 stage receipt");
    fs::rename(staged, out.join("receipt.json")).expect("#133 publish receipt");
}
