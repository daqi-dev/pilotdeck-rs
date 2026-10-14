//! Local gateway method whitelist. Empty until a separately verified migration.

// A handler must return the exact response payload bytes. When enabling the first
// handler, add authenticated hello_ok/binding state before permitting local work.
type Handler = fn(&[u8]) -> Vec<u8>;
const LOCAL_METHODS: &[(&str, Handler)] = &[];

pub(crate) enum Route {
    Forward,
    LocalResponse(Vec<u8>),
}

pub(crate) fn route(payload: &[u8]) -> Route {
    route_with_handlers(payload, LOCAL_METHODS)
}

fn route_with_handlers(payload: &[u8], handlers: &[(&str, Handler)]) -> Route {
    // Do not impose Rust JSON/UTF-8 validation on passthrough traffic. In
    // particular TS's UTF-8 replacement and JSON.parse semantics stay upstream.
    if handlers.is_empty() {
        return Route::Forward;
    }
    let Ok(request) = serde_json::from_slice::<serde_json::Value>(payload) else {
        return Route::Forward;
    };
    if request.get("type").and_then(|v| v.as_str()) != Some("request") {
        return Route::Forward;
    }
    let Some(method) = request.get("method").and_then(|v| v.as_str()) else {
        return Route::Forward;
    };
    match handlers.iter().find(|(name, _)| *name == method) {
        Some((_, handler)) => Route::LocalResponse(handler(payload)),
        None => Route::Forward,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whitelist_is_empty_even_for_all_first_migration_methods() {
        for method in [
            "native_archive_manifest",
            "native_archive_entries",
            "native_archive_artifact",
            "project_files_list",
            "project_file_read",
            "model_catalog_list",
            "session_model_get",
            "supported_agents",
            "list_projects",
            "describe_project",
            "describe_server",
            "submit_turn",
        ] {
            let payload = format!(r#"{{"type":"request","id":"kept","method":"{method}"}}"#);
            assert!(matches!(route(payload.as_bytes()), Route::Forward));
        }
    }

    #[test]
    fn matching_handler_receives_original_payload_and_returns_exact_bytes() {
        fn handler(payload: &[u8]) -> Vec<u8> {
            assert_eq!(
                payload,
                br#"{ "type":"request", "method":"fixture", "id":"kept" }"#
            );
            b"{ \"exact\": true }".to_vec()
        }
        let route = route_with_handlers(
            br#"{ "type":"request", "method":"fixture", "id":"kept" }"#,
            &[("fixture", handler)],
        );
        match route {
            Route::LocalResponse(payload) => assert_eq!(payload, b"{ \"exact\": true }"),
            Route::Forward => panic!("whitelist hit must select local response"),
        }
    }

    #[test]
    fn unknown_nonrequest_or_unparseable_payload_is_never_claimed_by_handler() {
        fn handler(_: &[u8]) -> Vec<u8> {
            panic!("must not claim passthrough payload");
        }
        for payload in [
            br#"{"type":"hello","method":"fixture"}"#.as_slice(),
            br#"{"type":"request","method":"unknown"}"#,
            br#"{"type":"request","method":7}"#,
            b"null",
            b"invalid json",
            &[255],
        ] {
            assert!(matches!(
                route_with_handlers(payload, &[("fixture", handler)]),
                Route::Forward
            ));
        }
    }
}
