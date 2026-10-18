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
    let method =
        SkillMethod::from_wire(&format!("skill_{op}")).unwrap_or_else(|| panic!("unknown op {op}"));
    // Through the gateway entry, so errno admission sees every oracle error.
    m.call(method, input)
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
    SkillManager::new(o).unwrap()
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
            "fifo" => {
                fs::create_dir_all(abs.parent().unwrap()).unwrap();
                // The workspace forbids unsafe; mkfifo(1) creates the same node.
                let status = std::process::Command::new("mkfifo")
                    .arg("-m")
                    .arg("644")
                    .arg(&abs)
                    .status()
                    .unwrap();
                assert!(status.success(), "mkfifo");
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
            } else if std::os::unix::fs::FileTypeExt::is_fifo(&meta.file_type()) {
                out.push(object([
                    ("path", path.as_str().into()),
                    ("kind", "fifo".into()),
                ]));
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
fn cursor_bytes_and_slugs_follow_node() {
    // String/Number conversions themselves are covered in crate::js_value.
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

fn oracle() -> JsValue {
    json(&fs::read_to_string(format!("{FIXTURES}/oracle.json")).unwrap())
}

#[test]
fn binding_selection_matches_real_ts_predicates() {
    let oracle = oracle();
    let JsValue::Array(cases) = oracle.get("bindings").unwrap() else {
        panic!("bindings")
    };
    assert!(cases.len() >= 10);
    let undefined = JsValue::Undefined;
    for case in cases {
        let binding = case.get("binding").unwrap_or(&undefined);
        let actual: JsValue = match select_binding(binding) {
            Ok(SkillsBinding::Native) => "native".into(),
            Ok(SkillsBinding::Disabled) => "disabled".into(),
            Ok(SkillsBinding::External) => "external".into(),
            Err(e) => object([("error", e.name.into())]),
        };
        assert_eq!(
            compact(&actual),
            compact(case.get("selected").unwrap()),
            "{binding:?}"
        );
    }
}

/// In-process contract for createLocalGateway.ts:851–870: routing key, the
/// disabled port's single error, and the explicit external-host boundary.
#[test]
fn facade_routes_each_method_to_the_selected_port() {
    use std::cell::RefCell;
    let oracle = oracle();
    let expected = oracle.get("disabledError").unwrap();
    let scratch = Scratch::new();
    let root = scratch.root();
    let seen: RefCell<Vec<JsValue>> = RefCell::new(vec![]);
    let facade = SkillsFacade::new(manager(&root, &json("{}")), |key: Option<&JsValue>| {
        let key = key.cloned().unwrap_or(JsValue::Undefined);
        seen.borrow_mut().push(key.clone());
        Ok(
            match key.as_string().map(|s| String::from_utf16_lossy(s.units())) {
                Some(k) if k == "/p/disabled" => json(r#"{"enabled":false}"#),
                Some(k) if k == "/p/external" => json(
                    r#"{"enabled":true,"implementationId":"x","contract":"skills","transport":"module-http-v2","endpoint":"http://127.0.0.1:1","methods":["list"]}"#,
                ),
                Some(k) if k == "/p/broken" => JsValue::Null,
                _ => JsValue::Undefined,
            },
        )
    });
    for method in SkillMethod::ALL {
        for (project, kind) in [("/p/disabled", "disabled"), ("/p/external", "external")] {
            seen.borrow_mut().clear();
            let input = json(&format!(r#"{{"projectKey":"{project}"}}"#));
            let e = facade.call(method, &input).unwrap_err();
            // validate/scan ignore the input's projectKey (868–869).
            let routed = !matches!(method, SkillMethod::Validate | SkillMethod::Scan);
            let key = if routed {
                project.into()
            } else {
                JsValue::Undefined
            };
            assert_eq!(*seen.borrow(), vec![key], "{method:?}");
            if !routed {
                // Without a key the default project is native.
                assert!(!e.requires_host && e.code != "SKILL_MODULE_DISABLED");
                continue;
            }
            if kind == "disabled" {
                assert_eq!(
                    compact(&object([
                        ("name", e.name.into()),
                        ("code", e.code.as_str().into()),
                        ("message", e.message.as_str().into()),
                    ])),
                    compact(expected),
                    "{method:?}"
                );
                assert!(!e.requires_host);
            } else {
                assert!(e.requires_host, "{method:?}");
                assert_eq!(e.code, "SKILL_MODULE_HOST_REQUIRED");
            }
        }
    }
    // Native passthrough, and `input.projectKey` is read before resolving.
    let listed = facade.call(SkillMethod::List, &json("{}")).unwrap();
    assert_eq!(compact(listed.get("items").unwrap()), "[]");
    seen.borrow_mut().clear();
    let e = facade.call(SkillMethod::List, &JsValue::Null).unwrap_err();
    assert_eq!(e.name, "TypeError");
    assert!(seen.borrow().is_empty());
    // A null binding throws from `"implementationId" in binding`.
    let e = facade
        .call(SkillMethod::Read, &json(r#"{"projectKey":"/p/broken"}"#))
        .unwrap_err();
    assert_eq!(e.name, "TypeError");
}

#[test]
fn errno_admission_equals_the_codes_each_method_recorded() {
    use std::collections::{BTreeMap, BTreeSet};
    let oracle = oracle();
    let mut measured: BTreeMap<String, BTreeSet<String>> = SkillMethod::ALL
        .iter()
        .map(|m| (m.wire().to_owned(), BTreeSet::new()))
        .collect();
    let JsValue::Array(scenarios) = oracle.get("scenarios").unwrap() else {
        panic!("scenarios")
    };
    for scenario in scenarios {
        let JsValue::Array(steps) = scenario.get("steps").unwrap() else {
            panic!("steps")
        };
        for step in steps {
            let Some(error) = step.get("outcome").unwrap().get("error") else {
                continue;
            };
            let code = error
                .get("code")
                .and_then(JsValue::as_string)
                .map(|c| c.to_utf8().unwrap());
            if let Some(code) = code.filter(|c| {
                error.get("name") == Some(&JsValue::from("Error"))
                    && c.starts_with('E')
                    && c.bytes().all(|b| b.is_ascii_uppercase())
            }) {
                let method = format!("skill_{}", str_of(step.get("op").unwrap()));
                measured.get_mut(&method).unwrap().insert(code);
            }
        }
    }
    for (method, codes) in measured {
        let allowed: BTreeSet<String> = crate::node_io::LOCAL_ERRNOS
            .iter()
            .find(|(m, _)| *m == method)
            .unwrap_or_else(|| panic!("{method} has no admission entry"))
            .1
            .iter()
            .map(|c| (*c).to_owned())
            .collect();
        assert_eq!(allowed, codes, "{method}");
    }
}

#[test]
fn unmeasured_errnos_require_the_host() {
    let io = |n| io::Error::from_raw_os_error(n);
    let e = SkillError::io(&io(libc::EACCES), "stat", Some("/x")).admit("skill_list");
    assert!(e.requires_host && e.code == "NODE_IO_REQUIRED" && e.message.is_empty());
    let e = SkillError::io(&io(libc::ENOTDIR), "scandir", Some("/x")).admit("skill_list");
    assert_eq!(
        (e.requires_host, e.message.as_str()),
        (false, "ENOTDIR: not a directory, scandir '/x'")
    );
    // Outside the libuv table: never a Rust Display string.
    assert!(SkillError::io(&io(123456), "stat", None).requires_host);
    assert!(SkillError::io(&io::Error::other("x"), "stat", None).requires_host);
    // Domain and JS errors carry no errno and pass untouched.
    let e = SkillError::manager("conflict", "x").admit("skill_list");
    assert!(!e.requires_host && e.code == "conflict");
    assert_eq!(
        SkillError::io_pair(&io(libc::EEXIST), "symlink", "/a", "/b").message,
        "EEXIST: file already exists, symlink '/a' -> '/b'"
    );
}

/// The depth itself is a known divergence (js_value module docs). Here only
/// the shape is pinned: no Rust stack overflow, and the error lands where V8's
/// RangeError would (caught inside decodeListCursor, uncaught elsewhere).
#[test]
fn deep_arrays_never_overflow_the_stack() {
    let scratch = Scratch::new();
    let m = manager(&scratch.root(), &json("{}"));
    let mut deep = JsValue::Number(1.0);
    for _ in 0..=crate::js_value::MAX_ARRAY_DEPTH {
        deep = JsValue::Array(vec![deep]);
    }
    let with = |key: &str, extra: &str| {
        let mut input = json(&format!("{{{extra}}}"));
        let JsValue::Object(o) = &mut input else {
            unreachable!()
        };
        o.insert(key.into(), deep.clone());
        input
    };
    let e = m.list(&with("cursor", "")).unwrap_err();
    assert_eq!(e.code, "invalid_input");
    let e = m.read(&with("slug", r#""scope":"user""#)).unwrap_err();
    assert_eq!(
        (e.name, e.message.as_str()),
        ("RangeError", "Maximum call stack size exceeded")
    );
    let e = m.list(&with("scope", "")).unwrap_err();
    assert_eq!(e.name, "RangeError");
    let mut file = json(r#"{"relativePath":"SKILL.md"}"#);
    let JsValue::Object(o) = &mut file else {
        unreachable!()
    };
    o.insert("size".into(), deep.clone());
    let input = object([
        ("skillMdContent", "---\nname: x\n---\n".into()),
        ("files", JsValue::Array(vec![file])),
    ]);
    assert_eq!(m.validate(&input).unwrap_err().name, "RangeError");
    assert!(matches!(
        select_binding(&JsValue::Array(vec![deep])),
        Ok(SkillsBinding::Native)
    ));
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
