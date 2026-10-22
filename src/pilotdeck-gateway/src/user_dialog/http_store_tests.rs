//! Unit-level goldens for the parts of `http_store` that are not reachable
//! through the public wire API: constructor validation and handler creation.
//!
//! TS source: `resources/PilotDeck/src/gateway/user-dialog/HttpGatewayUserDialogStore.ts`
//! at `cc7c4e6`. The oracle is produced by the real TS class (see
//! `tests/fixtures/user-dialog-http-store/generate.mts`); nothing here is
//! hand-written.

use super::*;
use crate::user_dialog::StoreOptions;
use serde_json::Value;

const ORACLE: &str = include_str!("../../tests/fixtures/user-dialog-http-store/oracle.json");

fn oracle() -> Value {
    serde_json::from_str(ORACLE).expect("fixture oracle.json is valid JSON")
}

fn options(url: &str, token: &str, timeout_ms: Option<f64>) -> HttpGatewayUserDialogStoreOptions {
    HttpGatewayUserDialogStoreOptions {
        url: url.into(),
        authorization_token: token.into(),
        timeout_ms,
    }
}

fn number(text: &str) -> f64 {
    match text {
        "NaN" => f64::NAN,
        "Infinity" => f64::INFINITY,
        "-Infinity" => f64::NEG_INFINITY,
        _ => text.parse().expect("fixture number"),
    }
}

/// `store.endpoint.href` / `new URL('clear', store.endpoint).href` for every
/// constructor URL, plus the exact TS error text for rejected URLs.
#[test]
fn constructors_match_typescript_endpoints_and_errors() {
    let oracle = oracle();
    let mut checked = 0;
    for case in oracle["constructors"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        let kind = case["kind"].as_str().unwrap();
        let input = &case["input"];
        if name.starts_with("url ") {
            let url = input.as_str().unwrap();
            let store =
                HttpGatewayUserDialogStore::new(options(url, "test-dialog-store-token", None));
            if kind == "endpoint" {
                let store = store.unwrap_or_else(|error| panic!("{name}: {error}"));
                assert_eq!(
                    store.endpoint.as_str(),
                    case["href"].as_str().unwrap(),
                    "{name}: href"
                );
                assert_eq!(
                    store.endpoint.join("clear").unwrap().as_str(),
                    case["resolved"].as_str().unwrap(),
                    "{name}: resolved URL"
                );
            } else {
                assert_eq!(
                    store.unwrap_err().to_string(),
                    case["message"].as_str().unwrap(),
                    "{name}: error message"
                );
            }
            checked += 1;
            continue;
        }
        if name.starts_with("timeoutMs ") {
            let timeout = match input["kind"].as_str().unwrap() {
                // Rust's `Option<f64>` cannot express a string `timeoutMs`;
                // the TS error is recorded but this state is type-level.
                "string" => continue,
                "undefined" | "null" => None,
                "number" => Some(number(input["text"].as_str().unwrap())),
                other => panic!("{name}: unknown timeout input {other}"),
            };
            let store = HttpGatewayUserDialogStore::new(options(
                "http://h/",
                "test-dialog-store-token",
                timeout,
            ));
            if kind == "timeout" {
                let store = store.unwrap_or_else(|error| panic!("{name}: {error}"));
                let expected = case["ms"].as_f64().unwrap();
                assert_eq!(store.timeout_ms, expected, "{name}: timeoutMs property");
                // Node clamps a setTimeout delay above 2^31-1 to 1 ms; TS still
                // exposes the unclamped property, which is what we compare above.
                let effective = if expected > 2_147_483_647.0 {
                    1
                } else {
                    expected as u64
                };
                assert_eq!(
                    store.timeout,
                    Duration::from_millis(effective),
                    "{name}: effective timer"
                );
            } else {
                assert_eq!(
                    store.unwrap_err().to_string(),
                    case["message"].as_str().unwrap(),
                    "{name}: error message"
                );
            }
            checked += 1;
            continue;
        }
        if name.starts_with("token ") {
            let input = &case["input"];
            // `undefined` is not expressible with `String`.
            if input["kind"].as_str().unwrap() == "undefined" {
                continue;
            }
            let token = input["value"].as_str().unwrap();
            let store = HttpGatewayUserDialogStore::new(options("http://h/", token, None));
            if kind == "token" {
                assert!(store.is_ok(), "{name}: accepted");
            } else {
                assert_eq!(
                    store.unwrap_err().to_string(),
                    case["message"].as_str().unwrap(),
                    "{name}: error message"
                );
            }
            checked += 1;
        }
    }
    // 29 recorded constructor cases; the string timeout and undefined token are
    // type-level skips, the remaining 27 are asserted here.
    assert_eq!(checked, 27, "all expressible constructor cases are covered");
}

/// TS `requireLiveStore` checks all twelve methods at construction. Rust's
/// `LiveGatewayUserDialogStore` trait makes a partial implementation
/// unrepresentable, so only the token guard is exercised at runtime; the
/// `missing-*` recordings document the TS-only runtime check that the type
/// system replaces.
#[test]
fn handler_construction_matches_typescript() {
    let oracle = oracle();
    let store: Arc<dyn LiveGatewayUserDialogStore> = Arc::new(
        FileGatewayUserDialogStore::new(StoreOptions::new("/pdrs-http-store-test"))
            .expect("absolute test directory"),
    );
    let mut checked = 0;
    let mut missing_checked = 0;
    for case in oracle["handlerConstruction"].as_array().unwrap() {
        let name = case["name"].as_str().unwrap();
        if let Some(method) = case["missing"].as_str() {
            assert_eq!(
                case["ok"], false,
                "{name}: TS rejects a store without {method}()"
            );
            assert_eq!(
                case["message"].as_str().unwrap(),
                format!("Gateway user-dialog store HTTP service requires {method}().")
            );
            // A complete Rust store always constructs.
            assert!(
                create_gateway_user_dialog_store_http_handler(
                    store.clone(),
                    "test-dialog-store-token"
                )
                .is_ok()
            );
            missing_checked += 1;
            continue;
        }
        let input = &case["input"];
        if input["kind"].as_str().unwrap() == "undefined" {
            continue;
        }
        let token = input["value"].as_str().unwrap();
        let result = create_gateway_user_dialog_store_http_handler(store.clone(), token);
        match case["ok"].as_bool().unwrap() {
            true => assert!(result.is_ok(), "{name}: accepted"),
            false => assert_eq!(
                result.unwrap_err().to_string(),
                case["message"].as_str().unwrap(),
                "{name}: error message"
            ),
        }
        checked += 1;
    }
    assert_eq!(checked, 3, "expressible handler token cases");
    assert_eq!(missing_checked, 3, "missing-method recordings");
}
