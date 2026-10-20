//! `web_fetch` and `web_search` (A6.5-pr9), ported from the fixed TS baseline
//! `tool/builtin/{webFetch,webSearch,additionalWebSearch}.ts` and
//! `tool/builtin/web/*`. Network I/O goes through the shared provider
//! transport (`runtime::model::http::ModelHttpTransport`, hyper + rustls).
pub mod cache;
pub mod fetch_tool;
pub mod fetcher;
pub mod network;
pub mod preapproved_hosts;
pub mod search;
pub mod url_validation;

pub use fetch_tool::{
    SecondaryModel, SecondaryModelEvent, WebFetchModelPort, WebFetchTool, WebFetchToolOptions,
};
pub use fetcher::{HtmlToMarkdown, MissingTurndown, UrlFetcher};
pub use search::{CustomProviderConfig, WebSearchOptions, WebSearchTool};

/// `String.prototype.toLowerCase`.
pub(crate) fn js_lower(value: &str) -> String {
    value.to_lowercase()
}

/// `value.length` in UTF-16 units.
pub(crate) fn js_length(value: &str) -> usize {
    value.encode_utf16().count()
}

/// `{ type: "ask", reason, request }` shared by both tools.
pub(crate) fn ask_permission(
    tool: &str,
    message: &str,
    summary: &str,
    allow_label: &str,
) -> pilotdeck_protocol::JsValue {
    use super::{object, string};
    use pilotdeck_protocol::JsValue;
    let reason = || {
        JsValue::Object(object([
            ("type", string("tool")),
            ("toolName", string(tool)),
            ("message", string(message)),
        ]))
    };
    let option = |id: &str, label: &str| {
        JsValue::Object(object([("id", string(id)), ("label", string(label))]))
    };
    JsValue::Object(object([
        ("type", string("ask")),
        ("reason", reason()),
        (
            "request",
            JsValue::Object(object([
                ("toolCallId", string("")),
                ("toolName", string(tool)),
                ("inputSummary", string(summary)),
                ("reason", reason()),
                (
                    "options",
                    JsValue::Array(vec![
                        option("allow_once", allow_label),
                        option("deny", "Deny"),
                    ]),
                ),
            ])),
        ),
    ]))
}
