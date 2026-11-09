//! Channel search reads the same JSONL engine as `pilotdeck chat search`.
use crate::{
    channel::protocol::{BoxFuture, ChannelError, ChannelSearchPort},
    runtime::history::search::{self, SearchOptions},
};
use pilotdeck_protocol::{JsString, JsValue};
use std::path::PathBuf;

pub(super) struct NativeChannelSearch {
    pub home: PathBuf,
    pub cwd: PathBuf,
}
impl ChannelSearchPort for NativeChannelSearch {
    fn search(&self, params: JsValue) -> BoxFuture<Result<JsValue, ChannelError>> {
        let string = |key| params.get(key).and_then(JsValue::as_string).cloned();
        let options = SearchOptions {
            cwd: Some(self.cwd.clone()),
            pilot_home: self.home.clone(),
            project_root: string("projectRoot")
                .map(|s| PathBuf::from(String::from_utf16_lossy(s.units()))),
            query: string("query").unwrap_or_default(),
            limit: match params.get("limit") {
                Some(JsValue::Number(n)) => *n,
                _ => 20.0,
            },
            regex: params.get("regex").is_some_and(truthy),
            case_sensitive: params.get("caseSensitive").is_some_and(truthy),
            role: string("role").unwrap_or_else(|| JsString::from("all")),
            session_id: string("sessionId"),
        };
        Box::pin(async move {
            tokio::task::spawn_blocking(move || search::search(options))
                .await
                .map_err(|e| ChannelError::error(e.to_string()))?
                .map_err(|e| ChannelError::error(e.to_string()))
        })
    }
}
fn truthy(value: &JsValue) -> bool {
    match value {
        JsValue::Undefined | JsValue::Null | JsValue::Bool(false) => false,
        JsValue::Number(n) => *n != 0.0 && !n.is_nan(),
        JsValue::String(s) => !s.is_empty(),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn channel_search_uses_bound_home_and_shared_content() {
        let root =
            std::env::temp_dir().join(format!("pdrs-channel-search-{}", uuid::Uuid::new_v4()));
        let chats = root.join("projects/p/chats");
        std::fs::create_dir_all(&chats).unwrap();
        std::fs::write(chats.join("s.jsonl"), r#"{"type":"accepted_input","sessionId":"s","messages":[{"content":[{"type":"text","text":"bound-home-marker"}]}]}"#).unwrap();
        let port = NativeChannelSearch {
            home: root.clone(),
            cwd: root.clone(),
        };
        let params = pilotdeck_protocol::parse_json(
            r#"{"query":"bound-home-marker","pilotHome":"/must/not/use/caller/home"}"#,
        )
        .unwrap();
        let actual = port.search(params).await.unwrap();
        let expected = search::search(SearchOptions {
            cwd: Some(root.clone()),
            pilot_home: root.clone(),
            query: "bound-home-marker".into(),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(actual, expected);
        assert_eq!(actual.get("sessionsScanned"), Some(&JsValue::Number(1.0)));
        std::fs::remove_dir_all(root).unwrap();
    }
}
