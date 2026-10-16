//! Session-specific error wrapper around the shared measured Node formatter.
use super::{Error, JsString, error};
pub(super) fn nul(path: &JsString) -> Error {
    error("ERR_INVALID_ARG_VALUE", crate::node_io::nul_message(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pilotdeck_protocol::{JsValue, parse_json};
    #[test]
    fn invalid_path_messages_match_node_22_bytes() {
        let fixture = parse_json(include_str!(
            "../../tests/fixtures/session-model/invalid-paths.json"
        ))
        .unwrap();
        let JsValue::Array(cases) = fixture.get("cases").unwrap() else {
            panic!("cases")
        };
        for case in cases {
            let path = case.get("path").unwrap().as_string().unwrap();
            let actual = nul(path);
            assert_eq!(
                actual.code,
                case.get("code")
                    .unwrap()
                    .as_string()
                    .unwrap()
                    .to_utf8()
                    .unwrap()
            );
            assert_eq!(
                &actual.message,
                case.get("message").unwrap().as_string().unwrap(),
                "{path:?}"
            );
        }
    }
}
