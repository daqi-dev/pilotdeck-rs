//! Production IM slash-command routing (`FeishuChannel.ts:487-500`, `WeComChannel.ts:526-544`).
use super::native::SharedConfigRoutes;
use std::path::{Path, PathBuf};

/// Outcome shared by `NativeSubmitHost::route_input` and HTTP channel webhooks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductionRouteInputResult {
    /// Non-slash user text must continue through submit (not dropped at HTTP edge).
    pub pass_through: bool,
    pub slash_handled: bool,
    pub replies: Vec<String>,
}

/// Production routing entry used from `submit_host.rs` `route_input` and IM webhooks.
pub fn production_route_input(
    home: &Path,
    cwd: &Path,
    channel_key: &str,
    message_text: &str,
    mut on_reply: impl FnMut(&str),
) -> ProductionRouteInputResult {
    let trimmed = message_text.trim();
    if !trimmed.starts_with('/') {
        return ProductionRouteInputResult {
            pass_through: true,
            slash_handled: false,
            replies: vec![],
        };
    }
    let mut replies = Vec::new();
    let handled = try_dispatch_production_slash_command(home, cwd, channel_key, trimmed, |reply| {
        replies.push(reply.to_owned());
        on_reply(reply);
    });
    ProductionRouteInputResult {
        pass_through: false,
        slash_handled: handled,
        replies,
    }
}

pub fn try_dispatch_production_slash_command(
    home: &Path,
    cwd: &Path,
    channel_key: &str,
    text: &str,
    reply: impl FnMut(&str),
) -> bool {
    if !text.trim().starts_with('/') {
        return false;
    }
    let home = home
        .canonicalize()
        .unwrap_or_else(|_| home.to_path_buf());
    let cwd = cwd
        .canonicalize()
        .unwrap_or_else(|_| cwd.to_path_buf());
    let Some(state) = super::native::installed(&home, &cwd) else {
        return false;
    };
    let routes = state.lock().unwrap();
    routes.try_dispatch_channel_slash_command(channel_key, text, reply)
}

pub fn try_dispatch_with_config(
    routes: &SharedConfigRoutes,
    channel_key: &str,
    text: &str,
    reply: impl FnMut(&str),
) -> bool {
    let routes = routes.lock().unwrap();
    routes.try_dispatch_channel_slash_command(channel_key, text, reply)
}

pub fn production_home_cwd(home: PathBuf, cwd: PathBuf) -> (PathBuf, PathBuf) {
    (home, cwd)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::routes::native::install;
    use crate::config::routes::ConfigRoutes;
    use crate::config::Env;

    const CONFIG: &str = "schemaVersion: 1\nagent: {model: parity/deterministic}\nmodel:\n  providers:\n    parity:\n      protocol: openai\n      url: http://127.0.0.1:1/v1\n      apiKey: mock-only\n      models: {deterministic: {}}\n";

    #[test]
    fn production_dispatch_feishu_after_config_install() {
        let root =
            std::env::temp_dir().join(format!("channel-inbound-prod-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        std::fs::write(root.join("pilotdeck.yaml"), CONFIG).unwrap();
        assert!(!try_dispatch_production_slash_command(
            &root,
            &root,
            "feishu",
            "/update check",
            |_| {},
        ));
        let _owner = install(
            ConfigRoutes::new(
                root.clone(),
                root.clone(),
                Env::from([("PILOT_HOME".into(), root.to_string_lossy().into_owned())]),
            )
            .unwrap(),
        );
        assert!(
            super::super::native::installed(&root, &root).is_some(),
            "CONFIG owner must register home/cwd for production dispatch"
        );
        let mut replies = Vec::new();
        assert!(
            try_dispatch_production_slash_command(
                &root,
                &root,
                "feishu",
                "/update check",
                |reply| replies.push(reply.to_owned()),
            ),
            "replies={replies:?}"
        );
        assert!(replies
            .last()
            .is_some_and(|line| line.contains('✅') || line.contains('❌')));
        assert!(try_dispatch_production_slash_command(
            &root,
            &root,
            "wecom",
            "/update check",
            |reply| replies.push(reply.to_owned()),
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn native_submit_host_route_input_production_path() {
        use crate::metadata::object;
        use crate::runtime::refresh::BoxFuture;
        use crate::runtime::session::{
            LiveSessionHandle, SessionContext, SessionFactory, SessionOwner, SessionOwnerOptions,
        };
        use crate::runtime::project::RuntimeError;
        use crate::runtime::turn::submit::SubmitHost;
        use crate::runtime::turn::submit_host::native_submit_host_for_route_input_tests;
        use pilotdeck_protocol::JsValue;
        use std::sync::Arc;

        struct NoSessionCreation;
        impl SessionFactory for NoSessionCreation {
            fn create(
                &self,
                _: SessionContext,
                _: Option<LiveSessionHandle>,
            ) -> BoxFuture<Result<LiveSessionHandle, RuntimeError>> {
                Box::pin(async { Err(RuntimeError::new("route_input test")) })
            }
        }

        let root =
            std::env::temp_dir().join(format!("channel-route-input-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        std::fs::write(root.join("pilotdeck.yaml"), CONFIG).unwrap();
        let _routes = install(
            ConfigRoutes::new(
                root.clone(),
                root.clone(),
                Env::from([("PILOT_HOME".into(), root.to_string_lossy().into_owned())]),
            )
            .unwrap(),
        );
        let owner = SessionOwner::new(SessionOwnerOptions::new(Arc::new(NoSessionCreation)));
        let host = native_submit_host_for_route_input_tests(root.clone(), owner);
        let mut input = object([
            ("channelKey", "feishu".into()),
            ("message", "/update check".into()),
        ]);
        let events = host.route_input(&mut input).await.unwrap();
        assert!(
            events.iter().any(|event| {
                event
                    .get("type")
                    .and_then(JsValue::as_string)
                    .is_some_and(|kind| kind.equals_str("channel_command_reply"))
            }),
            "route_input must emit production slash replies, events={events:?}"
        );
        let mut plain = object([
            ("channelKey", "feishu".into()),
            ("message", "你好".into()),
        ]);
        assert!(host.route_input(&mut plain).await.unwrap().is_empty());
        let _ = std::fs::remove_dir_all(root);
    }
}
