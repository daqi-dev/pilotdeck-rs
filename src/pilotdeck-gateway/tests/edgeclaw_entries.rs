//! Real TS transcripts compare each return and the entire residual tree/SQL.
use base64::{Engine, engine::general_purpose::STANDARD};
use pilotdeck_gateway::runtime::memory::edgeclaw::{
    EdgeClawFileStores, FileMemoryError, StorageError,
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
};
#[path = "common/baseline.rs"]
mod baseline;
const ORACLE: &str = include_str!("fixtures/edgeclaw-entries/oracle.json");
const NOW: &str = "2026-10-22T00:00:00.000Z";
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("pdrs-entries-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}
fn writable(p: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(p, fs::Permissions::from_mode(0o700));
    }
    if let Ok(entries) = fs::read_dir(p) {
        for e in entries.flatten() {
            if e.file_type().unwrap().is_dir() {
                writable(&e.path());
            }
        }
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        writable(&self.0);
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn js(v: &Value) -> JsValue {
    parse_json(&v.to_string()).unwrap()
}
fn obj(v: &Value) -> JsObject {
    js(v).as_object().unwrap().clone()
}
fn value(v: JsValue) -> Value {
    stringify_json(&v)
        .unwrap()
        .map(|s| serde_json::from_str(&s).unwrap())
        .unwrap_or(Value::Null)
}
fn oracle() -> Value {
    baseline::oracle(ORACLE);
    serde_json::from_str(ORACLE).unwrap()
}
// Node v22.22.0: deps/uv/src/unix/fs.c:1635,1643 forwards unlink errno;
// src/api/exceptions.cc:94-112 formats uv_err_name/uv_strerror and the path.
// https://docs.libuv.org/en/v1.x/errors.html#error-constants
// https://man7.org/linux/man-pages/man2/unlink.2.html#ERRORS (EISDIR vs EPERM).
// The committed real-TS transcript was recorded on macOS. These are exact
// per-platform expectations, never a set of acceptable actual errors.
const DIRECTORY_UNLINK_ERRORS: [(&str, &str, &str); 2] = [
    ("macos", "EPERM", "operation not permitted"),
    ("linux", "EISDIR", "illegal operation on a directory"),
];
fn expected_observation(op: &Value, recorded: &Value, platform: &str) -> Value {
    let mut expected = recorded.clone();
    // Only the empty entry id resolving to the memory directory has this
    // platform difference. Permission failures on files must remain untouched.
    if op["method"] != "deleteMemoryEntries"
        || !op["ids"]
            .as_array()
            .is_some_and(|ids| ids.iter().any(|id| id == ""))
        || !recorded["disk"].as_array().is_some_and(|disk| {
            disk.iter()
                .any(|entry| entry["relativePath"] == "memory" && entry["dir"] == true)
        })
        || recorded["result"]
            != json!({"error":"EPERM","message":"EPERM: operation not permitted, unlink '$ROOT/memory'"})
    {
        return expected;
    }
    if let Some((_, code, description)) = DIRECTORY_UNLINK_ERRORS
        .iter()
        .find(|(os, _, _)| *os == platform)
    {
        expected["result"]["error"] = json!(code);
        expected["result"]["message"] =
            json!(format!("{code}: {description}, unlink '$ROOT/memory'"));
    }
    expected
}
#[test]
fn directory_unlink_expectation_is_platform_specific_and_strict() {
    let o = oracle();
    let c = o["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "invalid-entry-operations")
        .unwrap();
    let op = &c["ops"][7];
    let recorded = &c["observations"][7];
    assert_eq!(expected_observation(op, recorded, "macos"), *recorded);
    let linux = expected_observation(op, recorded, "linux");
    assert_eq!(linux["result"]["error"], "EISDIR");
    assert_eq!(
        linux["result"]["message"],
        "EISDIR: illegal operation on a directory, unlink '$ROOT/memory'"
    );
    let mut unchanged = linux.clone();
    unchanged["result"] = recorded["result"].clone();
    assert_eq!(unchanged, *recorded); // Every other byte/field stays strict.
    assert_ne!(linux, *recorded); // A macOS result must fail the Linux criterion.
    for c in o["cases"].as_array().unwrap() {
        for (i, op) in c["ops"].as_array().unwrap().iter().enumerate() {
            if c["name"] == "invalid-entry-operations" && i == 7 {
                continue;
            }
            let recorded = &c["observations"][i];
            assert_eq!(expected_observation(op, recorded, "linux"), *recorded);
        }
    }
    // Even a near-match must not be rewritten: path, syscall, errno, extra
    // result fields, operation and directory evidence all remain significant.
    for (pointer, replacement) in [
        ("/result/error", json!("EACCES")),
        (
            "/result/message",
            json!("EPERM: operation not permitted, unlink '$ROOT/other'"),
        ),
        (
            "/result/message",
            json!("EPERM: operation not permitted, rmdir '$ROOT/memory'"),
        ),
        ("/disk", json!([])),
    ] {
        let mut changed = recorded.clone();
        *changed.pointer_mut(pointer).unwrap() = replacement;
        assert_eq!(expected_observation(op, &changed, "linux"), changed);
    }
    let mut extra = recorded.clone();
    extra["result"]["extra"] = json!(true);
    assert_eq!(expected_observation(op, &extra, "linux"), extra);
    let mut wrong_op = op.clone();
    wrong_op["method"] = json!("restoreMemoryEntries");
    assert_eq!(
        expected_observation(&wrong_op, recorded, "linux"),
        *recorded
    );
    wrong_op = op.clone();
    wrong_op["ids"] = json!(["missing"]);
    assert_eq!(
        expected_observation(&wrong_op, recorded, "linux"),
        *recorded
    );
}
fn clean(v: &Value, root: &Path) -> Value {
    match v {
        Value::String(s) => json!(s.replace(root.to_str().unwrap(), "$ROOT")),
        Value::Array(a) => json!(a.iter().map(|v| clean(v, root)).collect::<Vec<_>>()),
        Value::Object(o) => {
            Value::Object(o.iter().map(|(k, v)| (k.clone(), clean(v, root))).collect())
        }
        v => v.clone(),
    }
}
fn open(root: &Path, general: bool) -> EdgeClawFileStores {
    let home = root.join("home/.pilotdeck");
    EdgeClawFileStores::with_workspace_clock(
        root.join("control.sqlite"),
        root.join(if general { "current/memory" } else { "memory" }),
        root.join("global"),
        if general {
            home.as_path()
        } else {
            Path::new("/fixture/workspace")
        },
        &home,
        Arc::new(|| NOW.into()),
    )
    .unwrap()
}
fn query(c: &Connection, sql: &str) -> Value {
    let mut stmt = c.prepare(sql).unwrap();
    let names = stmt
        .column_names()
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>();
    let out = stmt
        .query_map([], |row| {
            let mut o = serde_json::Map::new();
            for (i, name) in names.iter().enumerate() {
                let v = match row.get_ref(i)? {
                    rusqlite::types::ValueRef::Null => Value::Null,
                    rusqlite::types::ValueRef::Integer(n) => json!(n),
                    rusqlite::types::ValueRef::Real(n) => json!(n),
                    rusqlite::types::ValueRef::Text(s) => json!(std::str::from_utf8(s).unwrap()),
                    _ => panic!("unexpected blob"),
                };
                o.insert(name.clone(), v);
            }
            Ok(Value::Object(o))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    json!(out)
}
fn disk(root: &Path) -> Value {
    fn walk(root: &Path, p: &Path, out: &mut Vec<Value>) {
        for e in fs::read_dir(p).unwrap() {
            let e = e.unwrap();
            let p = e.path();
            let rel = p.strip_prefix(root).unwrap().to_str().unwrap();
            let name = e.file_name().to_string_lossy().into_owned();
            if e.file_type().unwrap().is_symlink() {
                out.push(json!({"relativePath":rel,"link":fs::read_link(&p).unwrap()}));
            } else if e.file_type().unwrap().is_dir() {
                out.push(json!({"relativePath":rel,"dir":true}));
                walk(root, &p, out);
            } else if name.contains("control.sqlite") {
                if !name.ends_with("-wal") && !name.ends_with("-shm") {
                    let c =
                        Connection::open_with_flags(&p, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
                            .unwrap();
                    let mut rows = query(&c, "SELECT * FROM pipeline_state ORDER BY state_key");
                    for r in rows.as_array_mut().unwrap() {
                        let t = r["updated_at"].as_str().unwrap();
                        assert_eq!(t.len(), 24);
                        assert!(t.ends_with('Z'));
                        r["updated_at"] = json!("<CLOCK>");
                    }
                    out.push(json!({"relativePath":rel,"rows":rows,"l0Rows":query(&c,"SELECT * FROM l0_sessions ORDER BY l0_index_id"),"schema":query(&c,"SELECT name,type,sql FROM sqlite_master WHERE name NOT LIKE 'sqlite_%' ORDER BY name,type")}));
                }
            } else {
                out.push(json!({"relativePath":rel,"content":fs::read_to_string(p).unwrap()}));
            }
        }
    }
    let mut out = vec![];
    walk(root, root, &mut out);
    out.sort_by(|a, b| a["relativePath"].as_str().cmp(&b["relativePath"].as_str()));
    clean(&json!(out), root)
}
fn seed_ts(root: &Path, c: &Value) {
    for e in c["seedDisk"].as_array().unwrap() {
        let p = root.join(e["relativePath"].as_str().unwrap());
        if e["dir"] == true {
            fs::create_dir_all(p).unwrap();
        } else if let Some(text) = e["content"].as_str() {
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, text).unwrap();
        }
    }
    fs::write(
        root.join("control.sqlite"),
        STANDARD.decode(c["database"].as_str().unwrap()).unwrap(),
    )
    .unwrap();
}
fn seed_rust(root: &Path, general: bool) {
    use pilotdeck_gateway::runtime::memory::edgeclaw::L0SessionRecord;
    let r = open(root, general);
    r.storage
        .insert_l0_session(&L0SessionRecord {
            l0_index_id: "retained-l0".into(),
            session_key: "retained-session".into(),
            timestamp: NOW.into(),
            messages: parse_json(r#"[{"role":"user","content":"retained"}]"#).unwrap(),
            source: "openclaw".into(),
            indexed: false,
            created_at: NOW.into(),
        })
        .unwrap();
    r.workspace_memory
        .upsert_project_meta(&obj(&json!({"projectName":"Snapshot project"})))
        .unwrap();
    for kind in ["project", "feedback", "user"] {
        let c = obj(
            &json!({"type":kind,"scope":if kind=="user"{"global"}else{"project"},"name":kind,"description":format!("{kind} description"),"summary":format!("{kind} summary"),"profile":format!("{kind} profile"),"capturedAt":NOW,"sourceSessionKey":format!("session-{kind}")}),
        );
        (if kind == "user" {
            r.global_user_memory.as_ref()
        } else {
            &r.workspace_memory
        })
        .upsert_candidate(&c)
        .unwrap();
    }
    for (key, v) in [
        ("workspaceDir", json!(" /fixture/workspace ")),
        ("lastDreamAt", json!(" 2026-before ")),
        ("lastDreamStatus", json!("success")),
        ("lastDreamSummary", json!(" before summary ")),
        ("unrelated", json!({"keep":true})),
        (
            "indexingSettings",
            json!({"reasoningMode":"low","autoIndexIntervalMinutes":17,"autoDreamIntervalMinutes":33}),
        ),
        ("null-state", Value::Null),
    ] {
        let v = if key == "indexingSettings" {
            parse_json(r#"{"reasoningMode":"low","autoIndexIntervalMinutes":17,"autoDreamIntervalMinutes":33}"#).unwrap()
        } else {
            js(&v)
        };
        r.storage.set_pipeline_state(key, &v).unwrap();
    }
    r.close().unwrap();
}
fn error(e: FileMemoryError) -> Value {
    let io_code = e.io_code();
    match e {
        FileMemoryError::Io(e) => {
            let message = FileMemoryError::Io(e).js_message();
            json!({"error":io_code.expect("known Node errno"),"message":String::from_utf16_lossy(message.units())})
        }
        e @ FileMemoryError::Storage(StorageError::Sqlite(_)) => {
            json!({"error":"ERR_SQLITE_ERROR","message":String::from_utf16_lossy(e.js_message().units())})
        }
        e @ FileMemoryError::Type(_) => {
            json!({"error":"TypeError","message":String::from_utf16_lossy(e.js_message().units())})
        }
        e => json!({"error":"Error","message":String::from_utf16_lossy(e.js_message().units())}),
    }
}
fn entries(r: &EdgeClawFileStores) -> Vec<Value> {
    let options = obj(&json!({"includeDeprecated":true,"limit":5000}));
    let mut a = value(r.workspace_memory.list_memory_entries(&options).unwrap())
        .as_array()
        .unwrap()
        .clone();
    a.extend(
        value(r.list_global_memory_entries(&options).unwrap())
            .as_array()
            .unwrap()
            .clone(),
    );
    a
}
fn replay(root: &Path, c: &Value) -> Value {
    let general = c["general"] == true;
    let r = open(root, general);
    let mut second = None;
    let mut ids = HashMap::from([("missing".to_string(), "missing".to_string())]);
    for e in entries(&r) {
        ids.insert(
            e["type"].as_str().unwrap().into(),
            e["relativePath"].as_str().unwrap().into(),
        );
    }
    let mut observations = vec![];
    for (i, op) in c["ops"].as_array().unwrap().iter().enumerate() {
        if op["method"] == "second" {
            second = Some(open(root, general));
        }
        let repo = if op["second"] == true {
            second.as_ref().unwrap()
        } else {
            &r
        };
        let selected = op["select"]
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|k| {
                        ids.get(k.as_str().unwrap())
                            .cloned()
                            .unwrap_or("missing".into())
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_else(|| {
                op["ids"]
                    .as_array()
                    .map(|a| a.iter().map(|v| v.as_str().unwrap().to_string()).collect())
                    .unwrap_or_default()
            });
        let selected = selected
            .iter()
            .map(|s| JsString::from(s.as_str()))
            .collect::<Vec<_>>();
        let selected_ids = JsValue::Array(selected.iter().cloned().map(JsValue::String).collect());
        let input = if op["input"].is_object() {
            obj(&op["input"])
        } else {
            JsObject::new()
        };
        let result: Result<JsValue, FileMemoryError> = (|| match op["method"].as_str().unwrap() {
            "peer" => {
                use sha1::{Digest, Sha1};
                let peer = EdgeClawFileStores::with_workspace_clock(
                    root.join("peer/control.sqlite"),
                    root.join("peer/memory"),
                    root.join("peer-global"),
                    "/fixture/external",
                    root.join("home/.pilotdeck"),
                    Arc::new(|| NOW.into()),
                )?;
                peer.storage
                    .set_pipeline_state("workspaceDir", &JsValue::from("/fixture/external"))?;
                let name = "external-record";
                let record=value(peer.workspace_memory.upsert_candidate(&obj(&json!({"type":"project","scope":"project","name":name,"description":format!("{name} description"),"summary":format!("{name} summary"),"profile":format!("{name} profile"),"capturedAt":NOW,"sourceSessionKey":format!("session-{name}")})))?);
                ids.insert(
                    "peer".into(),
                    format!(
                        "external:{}:{}",
                        &format!("{:x}", Sha1::digest(b"/fixture/external"))[..10],
                        record["relativePath"].as_str().unwrap()
                    ),
                );
                peer.close()?;
                Ok(JsValue::Null)
            }
            "records" => repo
                .get_memory_records_by_ids(&selected_ids, op["maxLines"].as_f64().unwrap_or(80.0)),
            "editMemoryEntry" => {
                let mut input = input.clone();
                if op["select"].is_array() {
                    input.insert("id".into(), JsValue::String(selected[0].clone()));
                }
                repo.edit_memory_entry(&input)
            }
            "deleteMemoryEntries" => repo.delete_memory_entries(&selected_ids),
            "deprecateMemoryEntries" => repo.deprecate_memory_entries(&selected_ids),
            "restoreMemoryEntries" => repo.restore_memory_entries(&selected_ids),
            "getProjectMeta" => repo.get_project_meta(),
            "ensureProjectMeta" => repo.ensure_project_meta(&input),
            "editProjectMeta" => repo.edit_project_meta(&input),
            "archiveTmpEntries" => repo.archive_tmp_entries(&input),
            "countMemoryEntries" => repo.count_memory_entries(&input),
            "getSnapshotVersion" => repo
                .get_snapshot_version()
                .map(|s| JsValue::from(s.as_str())),
            "clearAllMemoryData" => repo.clear_all_memory_data(),
            "clearCurrentWorkspaceMemoryData" => repo.clear_current_workspace_memory_data(),
            "clearSessionMemoryData" => {
                repo.clear_session_memory_data(op["session"].as_str().unwrap_or("session-project"))
            }
            "state" => {
                repo.storage
                    .set_pipeline_state(op["key"].as_str().unwrap(), &js(&op["value"]))?;
                Ok(JsValue::Null)
            }
            "sql" => {
                Connection::open(root.join("control.sqlite"))
                    .unwrap()
                    .execute_batch(op["sql"].as_str().unwrap())
                    .unwrap();
                Ok(JsValue::Null)
            }
            "raw" => {
                let p = root.join(op["path"].as_str().unwrap());
                fs::create_dir_all(p.parent().unwrap())?;
                fs::write(p, op["content"].as_str().unwrap())?;
                Ok(JsValue::Null)
            }
            "chmodEntry" => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let id = selected[0].to_utf8().unwrap();
                    let p = root.join(if id.starts_with("global/") {
                        id
                    } else {
                        format!("memory/{id}")
                    });
                    fs::set_permissions(
                        p,
                        fs::Permissions::from_mode(op["mode"].as_u64().unwrap() as u32),
                    )?;
                }
                Ok(JsValue::Null)
            }
            "chmod" => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(
                        root.join(op["path"].as_str().unwrap()),
                        fs::Permissions::from_mode(op["mode"].as_u64().unwrap() as u32),
                    )?;
                }
                Ok(JsValue::Null)
            }
            "replaceWithLink" => {
                let p = root.join(op["path"].as_str().unwrap());
                fs::remove_dir_all(&p)?;
                #[cfg(unix)]
                std::os::unix::fs::symlink(op["target"].as_str().unwrap(), p)?;
                Ok(JsValue::Null)
            }
            "snapshot" => {
                fs::create_dir_all(root.join("last_dream/workspace"))?;
                fs::write(root.join("last_dream/workspace/saved"), "snapshot")?;
                fs::write(root.join("last_dream/metadata.json"), "{}")?;
                Ok(JsValue::Null)
            }
            "second" => Ok(JsValue::Null),
            "upsert" => {
                let kind = op["type"].as_str().unwrap();
                let name = op["name"].as_str().unwrap();
                let candidate = obj(
                    &json!({"type":kind,"scope":if kind=="user"{"global"}else{"project"},"name":name,"description":format!("{name} description"),"summary":format!("{name} summary"),"profile":format!("{name} profile"),"capturedAt":NOW,"sourceSessionKey":format!("session-{name}")}),
                );
                if kind == "user" {
                    repo.global_user_memory.upsert_candidate(&candidate)
                } else {
                    repo.workspace_memory.upsert_candidate(&candidate)
                }
            }
            m => panic!("unknown method {m}"),
        })();
        let actual = json!({"result":clean(&result.map(value).unwrap_or_else(error),root),"disk":disk(root)});
        let expected = expected_observation(op, &c["observations"][i], std::env::consts::OS);
        if actual != expected {
            let evidence = std::env::temp_dir().join(format!(
                "pdrs-entries-mismatch-{}.json",
                uuid::Uuid::new_v4()
            ));
            let diff = serde_json::to_string_pretty(&json!({"actual":actual,"expected":expected}))
                .unwrap();
            fs::write(&evidence, &diff).unwrap();
            // Keep return/error differences visible before the potentially large
            // disk tree, and bound CI output without splitting a UTF-8 character.
            let preview = format!(
                "actual result: {}\nexpected result: {}\n{diff}",
                actual["result"], expected["result"]
            );
            let mut end = preview.len().min(4096);
            while !preview.is_char_boundary(end) {
                end -= 1;
            }
            panic!(
                "scenario {} observation {i} {op}; diff {}\n{}{}",
                c["name"],
                evidence.display(),
                &preview[..end],
                if end < preview.len() {
                    "\n[diff preview truncated at 4 KiB; full diff in file]"
                } else {
                    ""
                }
            );
        }
        observations.push(actual);
    }
    if let Some(r) = second {
        r.close().unwrap();
    }
    r.close().unwrap();
    json!(observations)
}
fn locale(test: &str) -> bool {
    if std::env::var("PDRS_ENTRIES_CHILD").as_deref() == Ok(test) {
        return true;
    }
    assert!(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test, "--include-ignored", "--nocapture"])
            .env("PDRS_ENTRIES_CHILD", test)
            .env("LANG", "en_US.UTF-8")
            .env_remove("LC_ALL")
            .env_remove("LC_MESSAGES")
            .status()
            .unwrap()
            .success()
    );
    false
}
#[test]
fn malformed_archive_ids_match_real_ts() {
    let o = oracle();
    let s = Scratch::new();
    seed_rust(&s.0, false);
    let r = open(&s.0, false);
    let before = disk(&s.0);
    let mut n = 0;
    for audit in o["idAudit"].as_array().unwrap() {
        if audit["method"] != "archiveTmpEntries" {
            continue;
        }
        let id = js(&audit["input"])
            .get("id")
            .cloned()
            .unwrap_or(JsValue::Undefined);
        let mut input = JsObject::new();
        input.insert(
            "ids".into(),
            if audit["container"] == true {
                id
            } else {
                JsValue::Array(vec![id])
            },
        );
        let actual = r
            .archive_tmp_entries(&input)
            .map(value)
            .unwrap_or_else(error);
        assert_eq!(actual, audit["result"], "{audit}");
        assert_eq!(
            disk(&s.0) == before,
            audit["diskUnchanged"].as_bool().unwrap()
        );
        n += 1;
    }
    assert_eq!(n, 11);
    r.close().unwrap();
}
#[test]
fn malformed_batch_ids_match_real_ts() {
    let o = oracle();
    let s = Scratch::new();
    seed_rust(&s.0, false);
    let r = open(&s.0, false);
    let before = disk(&s.0);
    let mut n = 0;
    for audit in o["idAudit"].as_array().unwrap() {
        let method = audit["method"].as_str().unwrap();
        if !matches!(
            method,
            "getMemoryRecordsByIds"
                | "deleteMemoryEntries"
                | "deprecateMemoryEntries"
                | "restoreMemoryEntries"
        ) {
            continue;
        }
        let id = js(&audit["input"])
            .get("id")
            .cloned()
            .unwrap_or(JsValue::Undefined);
        let ids = if audit["container"] == true {
            id
        } else {
            JsValue::Array(vec![id])
        };
        let actual = match method {
            "getMemoryRecordsByIds" => r.get_memory_records_by_ids(&ids, 80.0),
            "deleteMemoryEntries" => r.delete_memory_entries(&ids),
            "deprecateMemoryEntries" => r.deprecate_memory_entries(&ids),
            _ => r.restore_memory_entries(&ids),
        }
        .map(value)
        .unwrap_or_else(error);
        assert_eq!(actual, audit["result"], "{audit}");
        assert_eq!(
            disk(&s.0) == before,
            audit["diskUnchanged"].as_bool().unwrap()
        );
        n += 1;
    }
    assert_eq!(n, 44);
    r.close().unwrap();
}
#[test]
fn malformed_batch_ids_multi_element_match_real_ts() {
    let o = oracle();
    let mut n = 0;
    for audit in o["idBatchAudit"].as_array().unwrap() {
        // A fresh seed per case: `[]` mutations repair manifests, so a shared
        // repository would make a later no-write case look dirty.
        let s = Scratch::new();
        seed_rust(&s.0, false);
        let r = open(&s.0, false);
        let before = disk(&s.0);
        let ids = js(&audit["ids"]);
        let actual = match audit["method"].as_str().unwrap() {
            "getMemoryRecordsByIds" => r.get_memory_records_by_ids(&ids, 80.0),
            "deleteMemoryEntries" => r.delete_memory_entries(&ids),
            "deprecateMemoryEntries" => r.deprecate_memory_entries(&ids),
            "restoreMemoryEntries" => r.restore_memory_entries(&ids),
            "archiveTmpEntries" => {
                let mut input = JsObject::new();
                input.insert("ids".into(), ids.clone());
                r.archive_tmp_entries(&input)
            }
            method => panic!("unexpected method {method}"),
        }
        .map(value)
        .unwrap_or_else(error);
        assert_eq!(clean(&actual, &s.0), audit["result"], "{audit}");
        assert_eq!(
            disk(&s.0) == before,
            audit["diskUnchanged"].as_bool().unwrap(),
            "{audit}"
        );
        if let Some(expected) = audit.get("diskAfter") {
            // `[]` mutations repair the manifest: compare the complete
            // residual tree, not only the boolean "disk changed".
            assert_eq!(clean(&disk(&s.0), &s.0), *expected, "{audit}");
        }
        n += 1;
        r.close().unwrap();
    }
    assert_eq!(n, 15);
}
#[test]
fn ts_entries_and_failure_residue_replay() {
    if !locale("ts_entries_and_failure_residue_replay") {
        return;
    }
    let o = oracle();
    let filter = std::env::var("PDRS_ENTRIES_CASE").ok();
    let mut n = 0;
    for c in o["cases"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| filter.as_ref().is_none_or(|f| c["name"] == f.as_str()))
    {
        let s = Scratch::new();
        seed_ts(&s.0, c);
        replay(&s.0, c);
        n += 1;
    }
    assert!(n > 0);
}
fn node(script: &str, args: &[&Path]) {
    let base = PathBuf::from(std::env::var("PDRS_BASELINE").unwrap());
    let mut child = Command::new("node")
        .arg("--import")
        .arg(base.join("node_modules/tsx/dist/loader.mjs"))
        .arg(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/edgeclaw-entries")
                .join(script),
        )
        .args(args)
        .env("LANG", "en_US.UTF-8")
        .env_remove("LC_ALL")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let start = std::time::Instant::now();
    while child.try_wait().unwrap().is_none() {
        if start.elapsed() > std::time::Duration::from_secs(55) {
            child.kill().unwrap();
            let _ = child.wait();
            panic!("TS verifier timed out");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
#[test]
#[ignore = "requires fixed real TS and Node 22"]
fn real_ts_reopens_rust_entries_and_failure_residue() {
    if !locale("real_ts_reopens_rust_entries_and_failure_residue") {
        return;
    }
    let o = oracle();
    let s = Scratch::new();
    let mut expected = vec![];
    for c in o["cases"].as_array().unwrap() {
        let root = s.0.join(c["name"].as_str().unwrap());
        fs::create_dir_all(&root).unwrap();
        seed_rust(&root, c["general"] == true);
        assert_eq!(disk(&root), c["seedDisk"], "Rust seed {}", c["name"]);
        replay(&root, c);
        let r = open(&root, c["general"] == true);
        let meta = clean(
            &r.get_project_meta().map(value).unwrap_or_else(error),
            &root,
        );
        let version = clean(
            &r.get_snapshot_version()
                .map(|v| json!(v))
                .unwrap_or_else(error),
            &root,
        );
        r.close().unwrap();
        expected.push(json!({"meta":meta,"version":version,"disk":disk(&root)}));
    }
    let output = s.0.join("inspect.json");
    node("verify.mts", &[&s.0, &output, Path::new("inspect")]);
    let actual: Value = serde_json::from_slice(&fs::read(output).unwrap()).unwrap();
    assert_eq!(actual, json!(expected));
}
#[test]
#[ignore = "regenerates golden using fixed real TS"]
fn real_ts_entries_regenerates_identically() {
    if locale("real_ts_entries_regenerates_identically") {
        baseline::regenerates_identically("edgeclaw-entries");
    }
}

fn signal(root: &Path, name: &str) {
    fs::write(root.join("signals").join(name), "go").unwrap();
}
fn wait_signal(root: &Path, name: &str) {
    let start = std::time::Instant::now();
    while !root.join("signals").join(name).exists() {
        assert!(
            start.elapsed() < std::time::Duration::from_secs(20),
            "barrier timeout {name}"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}
struct Children(Vec<std::process::Child>);
impl Drop for Children {
    fn drop(&mut self) {
        for c in &mut self.0 {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}
#[test]
fn concurrent_clear_and_writes_match_real_ts() {
    if !locale("concurrent_clear_and_writes_match_real_ts") {
        return;
    }
    if let Ok(root) = std::env::var("PDRS_ENTRIES_WORKER_ROOT") {
        let root = PathBuf::from(root);
        let id = std::env::var("PDRS_ENTRIES_WORKER_ID").unwrap();
        let mut r = open(&root, false);
        signal(&root, &format!("boot-{id}"));
        wait_signal(&root, &format!("start-{id}"));
        let result = if id == "one" {
            let root2 = root.clone();
            r.set_snapshot_io_hook(Arc::new(move|op,p,_|{
                if op==pilotdeck_gateway::runtime::memory::edgeclaw::storage::snapshots::SnapshotIoOperation::Remove&&p==root2.join("memory"){signal(&root2,"paused");wait_signal(&root2,"release");}Ok(())
            }));
            value(r.clear_all_memory_data().unwrap())
        } else {
            let candidate = |kind: &str, name: &str| {
                obj(
                    &json!({"type":kind,"scope":"project","name":name,"description":format!("{name} description"),"summary":format!("{name} summary"),"profile":format!("{name} profile"),"capturedAt":NOW,"sourceSessionKey":format!("session-{name}")}),
                )
            };
            r.workspace_memory
                .upsert_candidate(&candidate("project", "overlap-before"))
                .unwrap();
            let cleared = value(r.clear_current_workspace_memory_data().unwrap());
            let written = value(
                r.workspace_memory
                    .upsert_candidate(&candidate("feedback", "overlap-after"))
                    .unwrap(),
            );
            json!({"cleared":cleared,"written":written})
        };
        r.close().unwrap();
        fs::write(
            root.join("signals").join(format!("result-{id}")),
            clean(&result, &root).to_string(),
        )
        .unwrap();
        return;
    }
    let o = oracle();
    let s = Scratch::new();
    seed_ts(&s.0, &o["cases"][0]);
    fs::create_dir(s.0.join("signals")).unwrap();
    let mut children = Children(vec![]);
    for id in ["one", "two"] {
        children.0.push(
            Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "concurrent_clear_and_writes_match_real_ts",
                    "--nocapture",
                ])
                .env("PDRS_ENTRIES_WORKER_ROOT", &s.0)
                .env("PDRS_ENTRIES_WORKER_ID", id)
                .stdout(std::process::Stdio::null())
                .spawn()
                .unwrap(),
        );
        wait_signal(&s.0, &format!("boot-{id}"));
    }
    signal(&s.0, "start-one");
    wait_signal(&s.0, "paused");
    signal(&s.0, "start-two");
    wait_signal(&s.0, "result-two");
    signal(&s.0, "release");
    wait_signal(&s.0, "result-one");
    for child in &mut children.0 {
        assert!(child.wait().unwrap().success());
    }
    let results = ["one", "two"].map(|id| {
        serde_json::from_slice::<Value>(
            &fs::read(s.0.join("signals").join(format!("result-{id}"))).unwrap(),
        )
        .unwrap()
    });
    let disk = disk(&s.0)
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| !e["relativePath"].as_str().unwrap().starts_with("signals"))
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(json!({"results":results,"disk":disk}), o["concurrent"]);
}
#[test]
fn same_process_clear_write_and_manifest_provider_do_not_deadlock() {
    if !locale("same_process_clear_write_and_manifest_provider_do_not_deadlock") {
        return;
    }
    let o = oracle();
    let s = Scratch::new();
    seed_ts(&s.0, &o["cases"][0]);
    let b = Arc::new(std::sync::Barrier::new(4));
    let (tx, rx) = std::sync::mpsc::channel();
    let mut workers = vec![];
    for id in 0..3 {
        let r = open(&s.0, false);
        let b = b.clone();
        let tx = tx.clone();
        workers.push(std::thread::spawn(move || {
            b.wait();
            for n in 0..20 {
                match id {
                    0 => { r.clear_all_memory_data().unwrap(); }
                    1 => { r.clear_current_workspace_memory_data().unwrap(); }
                    _ => {
                        let input=json!({"type":"user","scope":"global","name":format!("writer-{n}"),"description":"writer","profile":"profile"});
                        r.global_user_memory.upsert_candidate(&obj(&input)).unwrap();
                        r.repair_workspace_manifest().unwrap();
                    }
                }
            }
            r.close().unwrap();
            tx.send(()).unwrap();
        }));
    }
    b.wait();
    for _ in 0..3 {
        rx.recv_timeout(std::time::Duration::from_secs(20))
            .expect("clear/provider deadlock");
    }
    for w in workers {
        w.join().unwrap();
    }
    let r = open(&s.0, false);
    let files = r.workspace_memory.export_snapshot_files().unwrap();
    assert!(matches!(files, JsValue::Array(_)));
    r.repair_workspace_manifest().unwrap();
    r.close().unwrap();
}

// The error value is measured from real TS editEntry(missing), rather than
// inventing a security-only wire error. Only the supplied path is substituted.
#[test]
#[cfg(unix)]
fn entry_paths_reject_escape_without_touching_outside_files() {
    use std::os::unix::fs::symlink;
    let oracle = oracle();
    let missing = &oracle["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "invalid-entry-operations")
        .unwrap()["observations"][0]["result"];
    for global in [false, true] {
        for attack in [
            "parent",
            "absolute",
            "leaf-link",
            "directory-link",
            "inside-parent",
        ] {
            for operation in [
                "read",
                "store-read",
                "edit",
                "delete",
                "deprecate",
                "restore",
            ] {
                if std::env::var("PDRS_ENTRY_SECURITY_OPERATION")
                    .is_ok_and(|only| only != operation)
                {
                    continue;
                }
                let scratch = Scratch::new();
                let r = open(&scratch.0, false);
                let store = if global {
                    r.global_user_memory.as_ref()
                } else {
                    &r.workspace_memory
                };
                let root = store.get_root_dir();
                let outside = scratch.0.join("victim.md");
                let content =
                    "---\ntype: project\nname: victim\nscope: project\n---\nvaluable contents\n";
                fs::write(&outside, content).unwrap();
                symlink(&outside, root.join("escape.md")).unwrap();
                symlink(&scratch.0, root.join("escape-dir")).unwrap();
                fs::write(root.join("safe.md"), content).unwrap();
                let path = match attack {
                    "parent" => "../victim.md".to_owned(),
                    "absolute" => outside.to_str().unwrap().to_owned(),
                    "leaf-link" => "escape.md".to_owned(),
                    "directory-link" => "escape-dir/victim.md".to_owned(),
                    _ => "Project/../safe.md".to_owned(),
                };
                let id = if global {
                    format!("global/{path}")
                } else {
                    path.clone()
                };
                let ids = JsValue::Array(vec![JsValue::from(id.as_str())]);
                let before = disk(&scratch.0);
                let result = match operation {
                    "read" => r.get_memory_records_by_ids(&ids, 80.0),
                    "store-read" => store.get_memory_records_by_ids(&obj(&json!({"ids":[path]}))),
                    "edit" => r.edit_memory_entry(&obj(&json!({"id":id,"name":"changed"}))),
                    "delete" => r.delete_memory_entries(&ids),
                    "deprecate" => r.deprecate_memory_entries(&ids),
                    _ => r.restore_memory_entries(&ids),
                };
                let expected = clean(
                    &json!({"error":missing["error"],"message":missing["message"].as_str().unwrap().replace("missing", &path)}),
                    &scratch.0,
                );
                assert_eq!(
                    clean(
                        &error(result.expect_err("unsafe entry accepted")),
                        &scratch.0
                    ),
                    expected,
                    "{global} {attack} {operation}"
                );
                assert_eq!(fs::read_to_string(&outside).unwrap(), content);
                assert_eq!(
                    disk(&scratch.0),
                    before,
                    "{global} {attack} {operation} changed disk"
                );
            }
        }
    }
}

#[test]
fn snapshot_version_waits_for_clear_to_finish() {
    use pilotdeck_gateway::runtime::memory::edgeclaw::storage::snapshots::SnapshotIoOperation;
    use std::sync::{Barrier, mpsc};
    use std::time::Duration;
    let s = Scratch::new();
    seed_ts(&s.0, &oracle()["cases"][0]);
    let mut clearing = open(&s.0, false);
    let reader = open(&s.0, false);
    let state_writer = open(&s.0, false);
    let paused = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let p = paused.clone();
    let r = release.clone();
    let root = s.0.join("memory");
    clearing.set_snapshot_io_hook(Arc::new(move |op, path, _| {
        if op == SnapshotIoOperation::Remove && path == root {
            p.wait();
            r.wait();
        }
        Ok(())
    }));
    let clear = std::thread::spawn(move || clearing.clear_all_memory_data());
    paused.wait();
    let (ready_tx, ready_rx) = mpsc::channel();
    let (tx, rx) = mpsc::channel();
    let read = std::thread::spawn(move || {
        ready_tx.send(()).unwrap();
        tx.send(reader.get_snapshot_version()).unwrap();
    });
    ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let early = rx.recv_timeout(Duration::from_millis(200));
    // Without the repository lock the reader has already captured lastDreamAt
    // before blocking on the store's export lock. A state update exposes that
    // mixed view even though the store lock prevents an early return.
    state_writer
        .storage
        .set_pipeline_state("lastDreamAt", &JsValue::from("after-clear-state"))
        .unwrap();
    release.wait();
    clear.join().unwrap().unwrap();
    read.join().unwrap();
    assert!(
        matches!(early, Err(mpsc::RecvTimeoutError::Timeout)),
        "snapshot observed an unfinished clear: {early:?}"
    );
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(5)).unwrap().unwrap(),
        open(&s.0, false).get_snapshot_version().unwrap()
    );
}
