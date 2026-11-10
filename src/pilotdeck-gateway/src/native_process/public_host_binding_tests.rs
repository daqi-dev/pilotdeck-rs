//! Real fixed-TS binding/transport corpus; exercise the production selector.
use super::public_host::bound_project;
use crate::{config::Env, metadata::object, module_host::PublicHostPrincipal};
use pilotdeck_protocol::{JsValue, parse_json, stringify_json};
use std::path::{Path, PathBuf};
fn field<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    static U: JsValue = JsValue::Undefined;
    value.get(key).unwrap_or(&U)
}
fn string(value: &JsValue) -> String {
    value.as_string().unwrap().to_utf8().unwrap()
}
fn expand(value: &JsValue, home: &Path, project: &Path) -> JsValue {
    parse_json(
        &stringify_json(value)
            .unwrap()
            .unwrap()
            .replace("<HOME>", home.to_str().unwrap())
            .replace("<PROJECT>", project.to_str().unwrap()),
    )
    .unwrap()
}
struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
#[test]
fn production_public_binding_matches_original_ts_selection_and_error_cells() {
    let oracle = crate::test_baseline::oracle(include_str!(
        "../../tests/fixtures/public-host-binding/oracle.json"
    ));
    let root = std::env::temp_dir().join(format!("pdrs-public-binding-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let _scratch = Scratch(root.clone());
    let root = root.canonicalize().unwrap();
    let home = root.join("home");
    let project = root.join("project");
    std::fs::create_dir_all(home.join("staffdeck")).unwrap();
    std::fs::create_dir(&project).unwrap();
    let file = home.join("staffdeck/project-bindings.json");
    let lock = home.join("staffdeck/project-bindings.json.lock");
    let JsValue::Array(cases) = field(&oracle, "cases") else {
        panic!("binding cases")
    };
    assert_eq!(cases.len(), 21);
    for row in cases {
        if file.exists() {
            std::fs::remove_file(&file).unwrap();
        }
        if lock.exists() {
            std::fs::remove_file(&lock).unwrap();
        }
        let row = expand(row, &home, &project);
        if let Some(store) = row.get("store") {
            std::fs::write(&file, stringify_json(store).unwrap().unwrap()).unwrap();
        }
        if field(&row, "liveLock") == &JsValue::Bool(true) {
            std::fs::write(&lock, std::process::id().to_string()).unwrap();
        }
        let environment = expand(field(&oracle, "environment"), &home, &project);
        let mut env = Env::new();
        for (key, value) in environment.as_object().unwrap().own_entries() {
            env.insert(key.to_utf8().unwrap(), string(value));
        }
        env.insert(
            "STAFFDECK_PROJECT_BINDING_ENABLED".into(),
            if field(&row, "enabled") == &JsValue::Bool(false) {
                "false"
            } else {
                "true"
            }
            .into(),
        );
        if field(&row, "omitOriginEnvironment") == &JsValue::Bool(true) {
            env.remove("STAFFDECK_FORMAL_API_ORIGIN");
        }
        let caller = field(&row, "principal");
        let principal = PublicHostPrincipal {
            pilot_deck_user_id: field(caller, "pilotDeckUserId")
                .as_string()
                .unwrap()
                .clone(),
            tenant_id: field(caller, "tenantId").as_string().unwrap().clone(),
            actor_user_id: field(caller, "actorUserId").as_string().unwrap().clone(),
            agent_id: field(caller, "agentId").as_string().unwrap().clone(),
        };
        let expected = field(&row, "response");
        let name = string(field(&row, "case"));
        match bound_project(&home, &env, &principal) {
            Ok(actual) => {
                assert_eq!(
                    field(expected, "status"),
                    &JsValue::Number(200.0),
                    "{name} unexpectedly admitted"
                );
                let selected = field(field(&row, "selection"), "root");
                match actual {
                    Some(root) => assert_eq!(root.to_str().unwrap(), string(selected), "{name}"),
                    None => assert!(matches!(selected, JsValue::Null), "{name}"),
                }
            }
            Err(error) => {
                let mut body = object([(
                    "code",
                    error
                        .code
                        .unwrap_or_else(|| "PUBLIC_HOST_TRANSPORT_FAILED".into())
                        .as_str()
                        .into(),
                )])
                .as_object()
                .unwrap()
                .clone();
                if let Some(message) = error.message {
                    body.insert("message".into(), message.as_str().into());
                }
                let actual = object([
                    (
                        "status",
                        JsValue::Number(f64::from(error.status.unwrap_or(502))),
                    ),
                    ("body", JsValue::Object(body)),
                ]);
                assert_eq!(&actual, expected, "{name}");
            }
        }
    }
}
#[test]
#[ignore = "requires fixed PDRS_BASELINE checkout and Node toolchain"]
fn fixed_ts_public_binding_oracle_regenerates_identically() {
    crate::test_baseline::regenerates_identically("public-host-binding");
}
