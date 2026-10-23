//! #563: root-internal dangling symlinks are rejected (registered safe deviation).
//!
//! Real TS follows `global/UserIdentity/user-profile.md -> real.md` even when
//! `real.md` is missing: `getMemoryRecordsByIds` returns `[]`, `upsertUserProfile`
//! creates `real.md` through the link, and `deleteEntries` skips it (see
//! `fixtures/edgeclaw-dangling-links/oracle.json`, generated twice byte-identically
//! from the unmodified baseline). Rust's `within` cannot prove that a dangling
//! target stays inside the store root, so it rejects read / write / delete with
//! the same `Memory entry not found: <id>` text that TS `editEntry` uses, before
//! any IO. The `#[ignore]` test re-runs the real-TS generator and requires a
//! byte-identical oracle.
use pilotdeck_gateway::runtime::memory::edgeclaw::{FileMemoryStore, FileMemoryStoreOptions};
use pilotdeck_protocol::{JsObject, JsValue, stringify_json};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

#[path = "common/baseline.rs"]
mod baseline;

const ORACLE: &str = include_str!("fixtures/edgeclaw-dangling-links/oracle.json");
const NOW: &str = "2026-10-22T00:00:00.000Z";

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("pdrs-dangling-links-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn field<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    value.get(key).unwrap_or_else(|| panic!("missing {key}"))
}

fn arr(value: &JsValue) -> &[JsValue] {
    if let JsValue::Array(items) = value {
        items
    } else {
        panic!("not array {value:?}")
    }
}

fn txt(value: &JsValue) -> String {
    String::from_utf16_lossy(value.as_string().expect("string").units())
}

fn obj(fields: &[(&str, JsValue)]) -> JsObject {
    let mut object = JsObject::new();
    for (key, value) in fields {
        object.insert((*key).into(), value.clone());
    }
    object
}

fn string(value: &str) -> JsValue {
    JsValue::from(value)
}

fn equal(left: &JsValue, right: &JsValue) -> bool {
    match (left, right) {
        (JsValue::Object(left), JsValue::Object(right)) => {
            left.len() == right.len()
                && left
                    .entries()
                    .iter()
                    .all(|(key, value)| right.get_key(key).is_some_and(|other| equal(value, other)))
        }
        (JsValue::Array(left), JsValue::Array(right)) => {
            left.len() == right.len() && left.iter().zip(right).all(|(a, b)| equal(a, b))
        }
        _ => left == right,
    }
}

fn assert_json(actual: &JsValue, expected: &JsValue, label: &str) {
    assert!(
        equal(actual, expected),
        "{label}\nactual: {}\nexpected: {}",
        stringify_json(actual).unwrap().unwrap(),
        stringify_json(expected).unwrap().unwrap()
    );
}

/// Symlink-aware snapshot in the same shape as the generator's `tree()`.
fn tree(root: &Path, dir: &Path) -> Vec<JsValue> {
    let mut entries = fs::read_dir(root.join(dir))
        .unwrap()
        .map(|entry| entry.unwrap())
        .collect::<Vec<_>>();
    entries.sort_by_key(|entry| {
        entry
            .file_name()
            .to_string_lossy()
            .encode_utf16()
            .collect::<Vec<_>>()
    });
    let mut result = vec![];
    for entry in entries {
        let relative = dir.join(entry.file_name());
        let relative_path = relative.to_string_lossy();
        let file_type = entry.file_type().unwrap();
        let mut node = obj(&[("relativePath", string(&relative_path))]);
        if file_type.is_symlink() {
            node.insert("kind".into(), string("symlink"));
            node.insert(
                "target".into(),
                string(&fs::read_link(entry.path()).unwrap().to_string_lossy()),
            );
            result.push(JsValue::Object(node));
        } else if file_type.is_dir() {
            node.insert("kind".into(), string("dir"));
            result.push(JsValue::Object(node));
            result.extend(tree(root, &relative));
        } else {
            node.insert("kind".into(), string("file"));
            node.insert(
                "content".into(),
                string(&String::from_utf8_lossy(&fs::read(entry.path()).unwrap())),
            );
            result.push(JsValue::Object(node));
        }
    }
    result
}

fn scenario<'a>(oracle: &'a JsValue, name: &str) -> &'a JsValue {
    arr(field(oracle, "scenarios"))
        .iter()
        .find(|scenario| txt(field(scenario, "name")) == name)
        .unwrap_or_else(|| panic!("missing scenario {name}"))
}

fn observation(scenario: &JsValue) -> &JsValue {
    &arr(field(scenario, "observations"))[0]
}

fn setup(oracle: &JsValue, root: &Path) -> (String, String) {
    let link = txt(field(field(oracle, "link"), "relativePath"));
    let target = txt(field(field(oracle, "link"), "target"));
    fs::create_dir_all(root.join(&link).parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&target, root.join(&link)).unwrap();
    (link, target)
}

#[test]
fn ts_golden_proves_the_link_is_followed() {
    let oracle = baseline::oracle(ORACLE);
    let link = txt(field(field(&oracle, "link"), "relativePath"));

    assert_json(
        observation(scenario(&oracle, "read")),
        &JsValue::Array(vec![]),
        "TS read through a dangling link returns no record",
    );

    let write = scenario(&oracle, "write");
    let written = observation(write);
    assert_eq!(txt(field(written, "relativePath")), link);
    assert_eq!(txt(field(written, "content")), "## 身份背景\n- Rust");
    let files = arr(field(write, "files"));
    assert!(
        files.iter().any(
            |file| txt(field(file, "relativePath")) == "global/UserIdentity/real.md"
                && txt(field(file, "kind")) == "file"
        ),
        "TS write must create real.md through the link"
    );
    assert!(
        arr(field(write, "initialFiles"))
            .iter()
            .all(|file| { txt(field(file, "relativePath")) != "global/UserIdentity/real.md" }),
        "the link must start dangling"
    );

    let deleted = observation(scenario(&oracle, "delete"));
    assert_json(
        field(deleted, "mutatedIds"),
        &JsValue::Array(vec![]),
        "TS delete skips the dangling link",
    );
    let after_delete = arr(field(scenario(&oracle, "delete"), "files"));
    assert!(after_delete.iter().any(|file| {
        txt(field(file, "relativePath")) == link && txt(field(file, "kind")) == "symlink"
    }));
    assert!(
        after_delete
            .iter()
            .all(|file| { txt(field(file, "relativePath")) != "global/UserIdentity/real.md" })
    );

    assert_eq!(
        ts_not_found(&oracle),
        format!("Memory entry not found: {link}")
    );
}

fn ts_not_found(oracle: &JsValue) -> String {
    txt(field(
        observation(scenario(oracle, "edit-anchor")),
        "message",
    ))
}

#[test]
fn dangling_links_are_rejected_before_any_io() {
    let oracle = baseline::oracle(ORACLE);
    let expected = ts_not_found(&oracle);
    let scratch = Scratch::new();
    for scenario in arr(field(&oracle, "scenarios")) {
        let name = txt(field(scenario, "name"));
        let root = scratch.0.join(&name);
        let store = FileMemoryStore::with_clock(
            &root,
            FileMemoryStoreOptions::default(),
            Arc::new(|| NOW.into()),
        )
        .unwrap();
        let (link, target) = setup(&oracle, &root);
        assert_json(
            &JsValue::Array(tree(&root, Path::new(""))),
            field(scenario, "initialFiles"),
            &format!("{name} initial tree"),
        );

        let operation = &arr(field(scenario, "operations"))[0];
        let method = txt(field(operation, "method"));
        let input = field(operation, "input").as_object().unwrap();
        let result = match method.as_str() {
            "getMemoryRecordsByIds" => store.get_memory_records_by_ids(input),
            "upsertUserProfile" => store.upsert_user_profile(input),
            "deleteEntries" => store.delete_entries(input),
            "editEntry" => store.edit_entry(input),
            other => panic!("unknown operation {other}"),
        };
        let error = match result {
            Ok(value) => panic!(
                "{name} must be rejected, got {}",
                stringify_json(&value).unwrap().unwrap()
            ),
            Err(error) => error,
        };
        assert_eq!(error.to_string(), expected, "{name} rejection text");
        assert!(
            !root.join(&link).parent().unwrap().join(&target).exists(),
            "{name} must not create {target}"
        );
        assert_eq!(
            fs::read_link(root.join(&link)).unwrap(),
            PathBuf::from(&target),
            "{name} must leave the link in place"
        );
        assert_json(
            &JsValue::Array(tree(&root, Path::new(""))),
            field(scenario, "initialFiles"),
            &format!("{name} tree after rejection"),
        );
    }
}

/// The rejection is specific to unprovable targets: once TS has created the
/// target through the link, Rust reads it exactly like TS.
#[test]
fn rust_follows_ts_write_through_the_link_once_the_target_exists() {
    let oracle = baseline::oracle(ORACLE);
    let write = scenario(&oracle, "write");
    let scratch = Scratch::new();
    for file in arr(field(write, "files")) {
        let relative = txt(field(file, "relativePath"));
        let path = scratch.0.join(&relative);
        match txt(field(file, "kind")).as_str() {
            "dir" => fs::create_dir_all(&path).unwrap(),
            "symlink" => {
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::os::unix::fs::symlink(txt(field(file, "target")), &path).unwrap();
            }
            "file" => {
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(&path, txt(field(file, "content"))).unwrap();
            }
            other => panic!("unknown node kind {other}"),
        }
    }
    let store = FileMemoryStore::with_clock(
        &scratch.0,
        FileMemoryStoreOptions::default(),
        Arc::new(|| NOW.into()),
    )
    .unwrap();
    let link = txt(field(field(&oracle, "link"), "relativePath"));
    let records = store
        .get_memory_records_by_ids(&obj(&[
            ("ids", JsValue::Array(vec![string(&link)])),
            ("maxLines", JsValue::Number(5000.0)),
        ]))
        .unwrap();
    assert_eq!(arr(&records).len(), 1);
    assert_eq!(
        txt(field(&arr(&records)[0], "content")),
        txt(field(observation(write), "content"))
    );
    assert_json(
        &JsValue::Array(tree(&scratch.0, Path::new(""))),
        field(write, "files"),
        "reading a resolvable link must not change TS bytes",
    );
}

#[test]
#[ignore = "requires fixed TS baseline / Node 22"]
fn real_ts_generator_is_reproducible() {
    let _ = baseline::oracle(ORACLE);
    baseline::regenerates_identically("edgeclaw-dangling-links");
}
