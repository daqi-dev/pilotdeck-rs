//! SkillManager contract tests. Scenario outcomes and file trees come from the
//! real TS manager (tests/fixtures/skills/generate.mts).
use super::*;
use base64::engine::general_purpose::STANDARD;
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/skills");

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../_tmp/20261018-a68-skills/test-scratch")
            .join(format!(
                "{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        fs::create_dir_all(&dir).unwrap();
        // Results carry resolved paths; compare against the canonical spelling.
        Self(fs::canonicalize(&dir).unwrap())
    }
    fn root(&self) -> String {
        self.0.to_string_lossy().into_owned()
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn json(text: &str) -> JsValue {
    parse_json(text).unwrap()
}
fn compact(value: &JsValue) -> String {
    stringify_json(value).unwrap().unwrap_or_default()
}
fn str_of(value: &JsValue) -> String {
    String::from_utf16_lossy(value.as_string().unwrap().units())
}
fn replace_units(s: &JsString, from: &str, to: &str) -> JsString {
    let (from, to): (Vec<u16>, Vec<u16>) =
        (from.encode_utf16().collect(), to.encode_utf16().collect());
    let units = s.units();
    let mut out = Vec::with_capacity(units.len());
    let mut i = 0;
    while i < units.len() {
        if !from.is_empty() && units[i..].starts_with(&from) {
            out.extend(&to);
            i += from.len();
        } else {
            out.push(units[i]);
            i += 1;
        }
    }
    JsString::from_utf16(out)
}
fn map_object(o: &JsObject, mut f: impl FnMut(&JsString, &JsValue) -> JsValue) -> JsValue {
    let mut out = JsObject::new();
    for (k, v) in o.entries() {
        out.insert(k.clone(), f(k, v));
    }
    JsValue::Object(out)
}
/// generate.mts `substitute`: "<dir>" → root, "<cursor:K>" → step K's raw cursor.
fn substitute(value: &JsValue, root: &str, cursors: &[Option<String>]) -> JsValue {
    match value {
        JsValue::String(s) => {
            let text = String::from_utf16_lossy(s.units());
            if let Some(k) = text
                .strip_prefix("<cursor:")
                .and_then(|t| t.strip_suffix('>'))
                .and_then(|k| k.parse::<usize>().ok())
            {
                return cursors[k].as_deref().expect("cursor").into();
            }
            JsValue::String(replace_units(s, "<dir>", root))
        }
        JsValue::Array(items) => {
            JsValue::Array(items.iter().map(|v| substitute(v, root, cursors)).collect())
        }
        JsValue::Object(o) => map_object(o, |_, v| substitute(v, root, cursors)),
        other => other.clone(),
    }
}
/// generate.mts `normalize`.
fn normalize(value: &JsValue, root: &str, keep_mtime: bool) -> JsValue {
    match value {
        JsValue::String(s) => JsValue::String(replace_units(s, root, "<dir>")),
        JsValue::Array(items) => JsValue::Array(
            items
                .iter()
                .map(|v| normalize(v, root, keep_mtime))
                .collect(),
        ),
        JsValue::Object(o) => map_object(o, |k, v| match v {
            JsValue::Number(_) if k.equals_str("mtime") && !keep_mtime => "<mtime>".into(),
            JsValue::String(raw) if k.equals_str("nextCursor") => {
                cursor_view(&String::from_utf16_lossy(raw.units()), root)
            }
            _ => normalize(v, root, keep_mtime),
        }),
        other => other.clone(),
    }
}
fn cursor_view(raw: &str, root: &str) -> JsValue {
    let outer = json(&String::from_utf8(URL_SAFE_NO_PAD.decode(raw).unwrap()).unwrap());
    let inner_text = String::from_utf8(
        URL_SAFE_NO_PAD
            .decode(str_of(outer.get("signature").unwrap()))
            .unwrap(),
    )
    .unwrap();
    object([
        (
            "raw",
            if inner_text.contains(root) {
                JsValue::Null
            } else {
                raw.into()
            },
        ),
        ("offset", outer.get("offset").unwrap().clone()),
        ("signature", normalize(&json(&inner_text), root, false)),
    ])
}
fn outcome(result: Result<JsValue>, root: &str, keep_mtime: bool) -> JsValue {
    match result {
        Ok(value) => object([("result", normalize(&value, root, keep_mtime))]),
        Err(e) => {
            let mut error = object([
                ("name", e.name.into()),
                (
                    "code",
                    if e.code.is_empty() {
                        JsValue::Null
                    } else {
                        e.code.as_str().into()
                    },
                ),
            ]);
            let JsValue::Object(fields) = &mut error else {
                unreachable!()
            };
            if e.name != "TypeError" {
                fields.insert(
                    "message".into(),
                    normalize(&e.message.as_str().into(), root, false),
                );
            }
            if let Some(v) = &e.validation {
                fields.insert("validation".into(), normalize(v, root, false));
            }
            object([("error", error)])
        }
    }
}
fn call(m: &SkillManager, op: &str, input: &JsValue) -> Result<JsValue> {
    match op {
        "list" => m.list(input),
        "read" => m.read(input),
        "write" => m.write(input),
        "create" => m.create(input),
        "delete" => m.delete(input),
        "validate" => m.validate(input),
        other => panic!("unknown op {other}"),
    }
}
/// Replays recorded steps and compares every outcome as compact JSON.
fn replay(m: &SkillManager, steps: &JsValue, root: &str, keep_mtime: bool, label: &str) {
    let JsValue::Array(steps) = steps else {
        panic!("{label}: steps")
    };
    let mut cursors: Vec<Option<String>> = vec![];
    for (i, step) in steps.iter().enumerate() {
        let op = str_of(step.get("op").unwrap());
        let input = substitute(
            step.get("input").unwrap_or(&JsValue::Undefined),
            root,
            &cursors,
        );
        let result = call(m, &op, &input);
        cursors.push(
            result
                .as_ref()
                .ok()
                .and_then(|r| r.get("nextCursor"))
                .map(str_of),
        );
        assert_eq!(
            compact(&outcome(result, root, keep_mtime)),
            compact(step.get("outcome").unwrap()),
            "{label} step {i} {op} {:?}",
            step.get("input")
        );
    }
}
fn manager(root: &str, options: &JsValue) -> SkillManager {
    let mut o = SkillManagerOptions::new(format!("{root}/home"));
    if options.get("builtin") != Some(&JsValue::Bool(false)) {
        o.builtin_skills_root = Some(format!("{root}/builtin"));
    }
    let env = substitute(options.get("env").unwrap_or(&JsValue::Null), root, &[]);
    o.env = Some(
        env.as_object()
            .map(|e| {
                e.entries()
                    .iter()
                    .map(|(k, v)| (k.to_utf8().unwrap(), str_of(v)))
                    .collect()
            })
            .unwrap_or_default(),
    );
    if let Some(JsValue::Array(paths)) = options.get("generalCwdPaths") {
        o.general_cwd_paths = Some(
            paths
                .iter()
                .map(|p| str_of(&substitute(p, root, &[])))
                .collect(),
        );
    }
    o.locale = Some("en-US".parse().unwrap());
    o.home_dir = Some(root.into());
    SkillManager::new(o)
}
/// generate.mts `setupTree`.
fn materialize(root: &Path, setup: &JsValue) {
    let JsValue::Array(entries) = setup else {
        panic!("setup")
    };
    for e in entries {
        let abs = root.join(str_of(e.get("path").unwrap()));
        match str_of(e.get("kind").unwrap()).as_str() {
            "dir" => fs::create_dir_all(&abs).unwrap(),
            "symlink" => {
                fs::create_dir_all(abs.parent().unwrap()).unwrap();
                std::os::unix::fs::symlink(str_of(e.get("target").unwrap()), &abs).unwrap();
            }
            "bulk" => {
                fs::create_dir_all(&abs).unwrap();
                for i in 0..e.get("count").unwrap().as_number().unwrap() as usize {
                    fs::write(abs.join(format!("f{i:03}.txt")), "x").unwrap();
                }
            }
            "file" => {
                fs::create_dir_all(abs.parent().unwrap()).unwrap();
                if let Some(size) = e.get("size") {
                    fs::File::create(&abs)
                        .unwrap()
                        .set_len(size.as_number().unwrap() as u64)
                        .unwrap();
                } else if let Some(b64) = e.get("base64") {
                    fs::write(&abs, STANDARD.decode(str_of(b64)).unwrap()).unwrap();
                } else {
                    let text = e.get("text").unwrap().as_string().unwrap();
                    fs::write(&abs, String::from_utf16_lossy(text.units())).unwrap();
                }
            }
            other => panic!("unknown setup kind {other}"),
        }
    }
}
/// generate.mts `snapshotTree`: JS default sort (UTF-16 units), lstat, bytes by hash.
fn snapshot(root: &Path) -> JsValue {
    fn walk(root: &Path, rel: &str, out: &mut Vec<JsValue>) {
        let dir = if rel.is_empty() {
            root.to_path_buf()
        } else {
            root.join(rel)
        };
        let mut names: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort_by(|a, b| a.encode_utf16().cmp(b.encode_utf16()));
        let root_text = root.to_string_lossy();
        for name in names {
            let path = if rel.is_empty() {
                name
            } else {
                format!("{rel}/{name}")
            };
            let abs = root.join(&path);
            let meta = fs::symlink_metadata(&abs).unwrap();
            if meta.is_symlink() {
                let target = fs::read_link(&abs)
                    .unwrap()
                    .to_string_lossy()
                    .replace(&*root_text, "<dir>");
                out.push(object([
                    ("path", path.as_str().into()),
                    ("kind", "symlink".into()),
                    ("target", target.as_str().into()),
                ]));
            } else if meta.is_dir() {
                out.push(object([
                    ("path", path.as_str().into()),
                    ("kind", "dir".into()),
                ]));
                walk(root, &path, out);
            } else {
                let bytes = fs::read(&abs).unwrap();
                let mut entry = object([
                    ("path", path.as_str().into()),
                    ("kind", "file".into()),
                    ("size", JsValue::Number(bytes.len() as f64)),
                    (
                        "sha256",
                        format!("{:x}", Sha256::digest(&bytes)).as_str().into(),
                    ),
                ]);
                if bytes.len() <= 4096 {
                    let JsValue::Object(fields) = &mut entry else {
                        unreachable!()
                    };
                    match String::from_utf8(bytes) {
                        Ok(text) => fields.insert("text".into(), text.as_str().into()),
                        Err(e) => fields.insert(
                            "base64".into(),
                            STANDARD.encode(e.as_bytes()).as_str().into(),
                        ),
                    };
                }
                out.push(entry);
            }
        }
    }
    let mut out = vec![];
    walk(root, "", &mut out);
    JsValue::Array(out)
}

#[test]
fn real_ts_scenarios_match_outcomes_and_file_trees() {
    let text = fs::read_to_string(format!("{FIXTURES}/oracle.json")).unwrap();
    assert!(
        !text.contains(".generate-"),
        "oracle.json leaks a generation path"
    );
    let oracle = json(&text);
    assert_eq!(str_of(oracle.get("locale").unwrap()), "en-US");
    let JsValue::Array(scenarios) = oracle.get("scenarios").unwrap() else {
        panic!("scenarios")
    };
    assert!(scenarios.len() >= 10);
    for scenario in scenarios {
        let name = str_of(scenario.get("name").unwrap());
        let scratch = Scratch::new();
        let root = scratch.root();
        materialize(&scratch.0, scenario.get("setup").unwrap());
        let m = manager(&root, scenario.get("options").unwrap());
        replay(&m, scenario.get("steps").unwrap(), &root, false, &name);
        assert_eq!(
            compact(&snapshot(&scratch.0)),
            compact(scenario.get("tree").unwrap()),
            "{name}: file tree"
        );
    }
}

#[test]
fn error_classes_keep_ts_names_codes_and_messages() {
    let e = SkillError::validation(json(r#"{"ok":false}"#));
    assert_eq!(
        (e.name, e.code.as_str(), e.message.as_str()),
        (
            "SkillValidationError",
            "validation_failed",
            "Validation failed."
        )
    );
    assert!(e.is_manager_error());
    assert_eq!(compact(e.validation.as_deref().unwrap()), r#"{"ok":false}"#);
    let e = SkillError::manager("conflict", "x");
    assert!(e.is_manager_error() && e.validation.is_none());
    assert!(!SkillError::type_error("", "t").is_manager_error());
}

#[test]
fn js_conversions_follow_ecmascript() {
    for (text, expected) in [
        ("", 0.0),
        ("  12  ", 12.0),
        ("0x10", 16.0),
        ("0b101", 5.0),
        ("0o17", 15.0),
        ("1e3", 1000.0),
        (".5", 0.5),
        ("5.", 5.0),
        ("-Infinity", f64::NEG_INFINITY),
        ("+1", 1.0),
    ] {
        assert_eq!(string_to_number(text), expected, "{text:?}");
    }
    for text in [
        "abc",
        "-0x10",
        "inf",
        "nan",
        "1_000",
        "e5",
        ".",
        "0x",
        "1e",
        "Infinityx",
    ] {
        assert!(string_to_number(text).is_nan(), "{text:?}");
    }
    assert_eq!(js_to_number(&json("[5]")), 5.0);
    assert!(js_to_number(&json("[1,2]")).is_nan());
    assert_eq!(js_to_number(&json("[]")), 0.0);
    assert_eq!(js_to_number(&JsValue::Bool(true)), 1.0);
    assert_eq!(number_string(-0.0), "0");
    assert_eq!(number_string(1e21), "1e+21");
    assert_eq!(display(&json(r#"[1,null,"a",[2,3]]"#)), "1,,a,2,3");
    assert_eq!(
        (to_uint8(-1.0), to_uint8(257.9), to_uint8(f64::NAN)),
        (255, 1, 0)
    );
    // Legacy decoder: illegal characters are skipped, "=" stops, units truncate to bytes.
    assert_eq!(
        base64_lenient(&"QU!JD".encode_utf16().collect::<Vec<_>>()),
        b"ABC"
    );
    assert_eq!(
        base64_lenient(&"QQ==QUJD".encode_utf16().collect::<Vec<_>>()),
        b"A"
    );
    assert_eq!(base64_lenient(&[0x141, 0x51, 0x3d]), b"\x01");
    assert_eq!(valid_slug(&"a.b-c_9".into()).as_deref(), Some("a.b-c_9"));
    for bad in ["", ".a", "-a", "a..b", "a/b", "é"] {
        assert_eq!(valid_slug(&bad.into()), None, "{bad:?}");
    }
    assert_eq!(
        valid_slug(&"a".repeat(100).as_str().into()).map(|s| s.len()),
        Some(100)
    );
    assert_eq!(valid_slug(&"a".repeat(101).as_str().into()), None);
}

#[test]
#[ignore = "requires fixed TS baseline and Node/tsx; run with PDRS_BASELINE"]
fn real_ts_and_rust_read_each_others_skill_trees() {
    let baseline = std::env::var("PDRS_BASELINE").expect("set fixed baseline path");
    let scratch = Scratch::new();
    let root = scratch.root();
    let m = manager(&root, &json("{}"));
    let proj = format!("{root}/proj");
    for input in [
        r#"{"scope":"user","slug":"rust-made","name":"Rust Made","description":"Created by the Rust manager for TS to read","body":"Rust body"}"#.to_owned(),
        r#"{"scope":"user","slug":"rust-doomed"}"#.to_owned(),
        format!(r#"{{"scope":"project","slug":"rust-proj","projectKey":"{proj}","description":"Rust project skill"}}"#),
    ] {
        m.create(&json(&input)).unwrap();
    }
    m.write(&json(
        r#"{"scope":"user","slug":"rust-written","content":"---\nname: Rust \ud800 written\ndescription: Lone surrogate encoded by Rust\n---\n"}"#,
    ))
    .unwrap();
    fs::create_dir_all(format!("{root}/builtin/rust-bi")).unwrap();
    fs::write(
        format!("{root}/builtin/rust-bi/SKILL.md"),
        "---\nname: Rust Builtin\ndescription: Bundled skill both sides list\n---\n",
    )
    .unwrap();

    let node = |mode: &str| {
        let output = std::process::Command::new("node")
            .arg("--import")
            .arg(format!("{baseline}/node_modules/tsx/dist/loader.mjs"))
            .arg(format!("{FIXTURES}/generate.mts"))
            .arg(&root)
            .arg("--verify-rust")
            .arg(mode)
            .env("LANG", "en_US.UTF-8")
            .env_remove("LC_ALL")
            .env_remove("LC_MESSAGES")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        json(&String::from_utf8(output.stdout).unwrap())
    };
    // TS reads what Rust wrote: same outcomes, real mtimes included.
    let read = node("read");
    replay(&m, read.get("steps").unwrap(), &root, true, "ts-reads-rust");
    // Rust reads what TS wrote.
    let mutate = node("mutate");
    replay(
        &m,
        mutate.get("after").unwrap(),
        &root,
        true,
        "rust-reads-ts",
    );
    let written = fs::read(format!("{root}/home/skills/ts-written/SKILL.md")).unwrap();
    assert!(written.windows(3).any(|w| w == "\u{fffd}".as_bytes()));
}
