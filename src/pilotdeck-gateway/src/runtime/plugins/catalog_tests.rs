use super::*;
use serde_json::{Value, json};
use std::path::PathBuf;
const MESSAGE: &str = "#887 commands catalog oracle";
const LOCALE_MESSAGE: &str = "#887 commands catalog oracle requires en-US; set LANG=en_US.UTF-8 and unset LC_ALL/LC_MESSAGES";
pub(super) fn fixture() -> Value {
    let raw = include_str!("../../../tests/fixtures/commands-catalog/oracle.json");
    std::panic::catch_unwind(|| crate::test_baseline::oracle(raw)).expect(MESSAGE);
    serde_json::from_str(raw).expect(MESSAGE)
}
pub(super) struct Temp(pub(super) PathBuf);
impl Temp {
    pub(super) fn new() -> Self {
        let path = std::env::temp_dir().join(format!("pdrs-887-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).expect(MESSAGE);
        Self(path)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn normalize(value: &mut Value, root: &str) {
    match value {
        Value::String(s) => *s = s.replace(root, "<root>"),
        Value::Array(a) => {
            for v in a {
                normalize(v, root)
            }
        }
        Value::Object(o) => {
            for (key, v) in o {
                if key == "nextCursor" {
                    let bytes = URL_SAFE_NO_PAD
                        .decode(v.as_str().expect(MESSAGE))
                        .expect(MESSAGE);
                    *v = serde_json::from_slice(&bytes).expect(MESSAGE);
                    let bytes = URL_SAFE_NO_PAD
                        .decode(v["signature"].as_str().expect(MESSAGE))
                        .expect(MESSAGE);
                    v["signature"] = serde_json::from_slice(&bytes).expect(MESSAGE);
                }
                normalize(v, root);
            }
        }
        _ => {}
    }
}
async fn replay(id: &str) {
    assert_eq!(
        node_locale(|key| std::env::var(key).ok()).to_string(),
        "en-US",
        "{LOCALE_MESSAGE}"
    );
    let oracle = fixture();
    let case = oracle["cases"]
        .as_array()
        .expect(MESSAGE)
        .iter()
        .find(|c| c["id"] == id)
        .expect(MESSAGE);
    let temp = Temp::new();
    let root = temp.0.to_str().expect(MESSAGE);
    let files = case["files"].as_array().expect(MESSAGE);
    for f in files {
        let path = temp.0.join(f["path"].as_str().expect(MESSAGE));
        fs::create_dir_all(path.parent().expect(MESSAGE)).expect(MESSAGE);
        if f["dir"] == true {
            fs::create_dir_all(&path).expect(MESSAGE);
        } else if let Some(link) = f["link"].as_str() {
            #[cfg(unix)]
            std::os::unix::fs::symlink(temp.0.join(link), &path).expect(MESSAGE);
        } else if f.get("mode").is_none() {
            let bytes = f["bytes"]
                .as_array()
                .map(|a| a.iter().map(|n| n.as_u64().expect(MESSAGE) as u8).collect())
                .unwrap_or_else(|| f["text"].as_str().expect(MESSAGE).as_bytes().to_vec());
            fs::write(&path, bytes).expect(MESSAGE);
        }
        #[cfg(unix)]
        if let Some(mode) = f["mode"].as_u64() {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(mode as u32)).expect(MESSAGE);
        }
    }
    let commands: Vec<_> = case["commands"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|c| PluginCommandContribution {
            name: c["name"].as_str().expect(MESSAGE).into(),
            namespace: c["namespace"].as_str().unwrap_or("").into(),
            description: c["description"].as_str().map(str::to_owned),
            argument_hint: c["argumentHint"].as_str().map(str::to_owned),
            content: String::new(),
        })
        .collect();
    let requests = case["requests"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| vec![json!({})]);
    let results = case["results"].as_array().expect(MESSAGE);
    assert_eq!(
        requests.len(),
        results.len(),
        "{MESSAGE}: {id} request/result count"
    );
    for (request, expected) in requests.iter().zip(results) {
        let project = request["projectKey"]
            .as_str()
            .map(|s| s.replace("<root>", root))
            .unwrap_or_else(|| format!("{root}/project"));
        let mut input = CommandsListInput {
            project_key: project.clone(),
            query: request["query"].as_str().map(str::to_owned),
            limit: request["limit"].as_u64().map(|n| n as u32),
            cursor: request["cursor"].as_str().map(str::to_owned),
        };
        if let Some(offset) = request.get("offset") {
            let q = lower(
                &js_trim(input.query.as_deref().unwrap_or("")).into(),
                &"en-US".parse().expect(MESSAGE),
                false,
            )
            .to_utf8()
            .expect(MESSAGE);
            let signature=URL_SAFE_NO_PAD.encode(serde_json::to_vec(&json!({"projectKey":request["signatureProject"].as_str().unwrap_or(&project),"query":request["signatureQuery"].as_str().unwrap_or(&q)})).expect(MESSAGE));
            input.cursor = Some(URL_SAFE_NO_PAD.encode(
                serde_json::to_vec(&json!({"offset":offset,"signature":signature})).expect(MESSAGE),
            ));
            if request["noisyCursor"] == true {
                input.cursor = input.cursor.map(|s| format!("!!{s}\n"));
            }
        }
        let mut pages = vec![];
        loop {
            match NativeCommandCatalogProvider
                .list(
                    input.clone(),
                    case["home"]
                        .as_str()
                        .map(|s| s.replace("<root>", root))
                        .unwrap_or_else(|| format!("{root}/home")),
                    commands.clone(),
                )
                .await
            {
                Err(error) => {
                    if let Some(boundary) = expected.get("boundary") {
                        assert_eq!(
                            boundary,
                            &json!({"listReturned":true,"selfCycle":true,"jsonSerializable":false}),
                            "{MESSAGE}: TS object graph boundary"
                        );
                        assert_eq!(
                            error.message, "Cyclic alias graph cannot be represented by JsValue",
                            "{MESSAGE}: selected cycle is an explicit interface limitation"
                        );
                        break;
                    }
                    assert_eq!(
                        error.message,
                        expected["error"].as_str().expect(MESSAGE),
                        "{MESSAGE}: {id}"
                    );
                    break;
                }
                Ok(result) => {
                    assert!(
                        expected.get("boundary").is_none(),
                        "{MESSAGE}: selected cycle must not be silently dropped"
                    );
                    assert!(
                        expected.get("error").is_none(),
                        "{MESSAGE}: {id} expected error"
                    );
                    input.cursor = result
                        .value
                        .get("nextCursor")
                        .and_then(JsValue::as_string)
                        .map(|s| s.to_utf8().expect(MESSAGE));
                    let mut value: Value = serde_json::from_str(
                        &stringify_json(&result.value)
                            .expect(MESSAGE)
                            .expect(MESSAGE),
                    )
                    .expect(MESSAGE);
                    normalize(&mut value, root);
                    pages.push(value);
                    if request["pages"] != true || input.cursor.is_none() {
                        assert_eq!(json!(pages), expected["pages"], "{MESSAGE}: {id}");
                        break;
                    }
                    assert!(pages.len() < 100, "{MESSAGE}: pagination termination");
                }
            }
        }
    }
    #[cfg(unix)]
    for f in files {
        if f.get("mode").is_some() {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(
                temp.0.join(f["path"].as_str().expect(MESSAGE)),
                fs::Permissions::from_mode(0o700),
            )
            .expect(MESSAGE);
        }
    }
}
macro_rules! case {
    ($test:ident,$id:literal) => {
        #[tokio::test]
        async fn $test() {
            replay($id).await;
        }
    };
}
case!(catalog_empty, "empty");
case!(catalog_selected_cycle_boundary, "selected-cycle");
case!(catalog_excluded_cycles, "excluded-cycles");
case!(catalog_normalized_paths, "normalized-paths");
case!(catalog_disk, "disk");
case!(catalog_metadata, "metadata");
case!(catalog_precedence, "precedence");
case!(catalog_extensions, "extensions");
case!(catalog_sorting, "sorting");
case!(catalog_query, "query");
case!(catalog_pagination, "pagination");
case!(catalog_validation, "validation");
case!(catalog_read_errors, "read-errors");
#[tokio::test]
async fn catalog_locale_contract() {
    const MSG: &str = "#887 catalog locale contract";
    // Empty pages have no locale-sensitive ordering: bypassing the entry guard
    // must be caught even when the page happens to match the en-US oracle.
    const CHILD: &str = "runtime::plugins::catalog::tests::catalog_empty";
    for (locale, success) in [("en_US.UTF-8", true), ("zh_CN.UTF-8", false)] {
        let mut child = tokio::process::Command::new(std::env::current_exe().expect(MSG));
        child
            .args([CHILD, "--exact", "--nocapture", "--format", "pretty"])
            .env("LANG", locale)
            .env_remove("LC_ALL")
            .env_remove("LC_MESSAGES")
            .kill_on_drop(true);
        // Completion is child exit; a stuck child is killed after 30 seconds.
        let output = tokio::time::timeout(std::time::Duration::from_secs(30), child.output())
            .await
            .expect(MSG)
            .expect(MSG);
        let log = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(log.contains("running 1 test"), "{MSG}: {locale}: {log}");
        if success {
            assert!(output.status.success(), "{MSG}: {locale}: {log}");
            assert!(log.contains("1 passed; 0 failed"), "{MSG}: {locale}: {log}");
        } else {
            if output.status.success() {
                assert!(
                    log.contains("1 passed; 0 failed") && log.contains(&format!("{CHILD} ... ok")),
                    "{MSG}: malformed child success: {log}"
                );
                panic!("#887 catalog locale guard bypassed: zh_CN.UTF-8");
            }
            assert_eq!(output.status.code(), Some(101), "{MSG}: {locale}: {log}");
            assert!(
                log.contains(LOCALE_MESSAGE) && log.contains(&format!("{CHILD} ... FAILED")),
                "{MSG}: {locale}: {log}"
            );
        }
    }
}
#[test]
#[ignore = "requires Node v22.22.0 and PDRS_BASELINE"]
fn catalog_regenerates_identically() {
    crate::test_baseline::regenerates_identically("commands-catalog");
}
