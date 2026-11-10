use super::*;
use std::sync::Mutex;
struct Gateway {
    projects: Value,
    requests: Mutex<Vec<Value>>,
    searches: Mutex<Vec<(Option<String>, SearchArgs)>>,
}
impl ChannelGateway for Gateway {
    fn request<'a>(&'a self, method: &'a str, params: Value) -> ChannelCall<'a, Value> {
        Box::pin(async move {
            self.requests.lock().unwrap().push(json!([method, params]));
            Ok(self.projects.clone())
        })
    }
    fn turn(&self, _params: Value) -> ChannelCall<'_, Vec<Value>> {
        Box::pin(async { panic!("commands must not enter turn") })
    }
    fn search<'a>(
        &'a self,
        _home: &'a Path,
        project: Option<&'a str>,
        args: SearchArgs,
    ) -> ChannelCall<'a, String> {
        Box::pin(async move {
            self.searches
                .lock()
                .unwrap()
                .push((project.map(str::to_owned), args));
            Ok("found".into())
        })
    }
}
#[derive(Default)]
struct Reply(Mutex<Vec<String>>);
impl ChannelReply for Reply {
    fn reply<'a>(&'a self, text: &'a str) -> ChannelCall<'a, ()> {
        Box::pin(async move {
            self.0.lock().unwrap().push(text.into());
            Ok(())
        })
    }
}
fn gateway() -> Gateway {
    Gateway {
        projects: json!({"projects":[{"name":"alpha-beta","projectKey":"/a","fullPath":"/a"},{"name":"ALPHA","projectKey":"/b","fullPath":"/b"},{"name":"alpha","projectKey":"/c","fullPath":"/c"}]}),
        requests: Mutex::new(vec![]),
        searches: Mutex::new(vec![]),
    }
}
#[test]
fn registry_preserves_alias_case_ascii_space_and_forwarding_boundaries() {
    assert_eq!(resolve_command(" /GREp  x  "), Some(("search", "x")));
    assert_eq!(
        resolve_command("/切换项目 alpha"),
        Some(("switch-project", "alpha"))
    );
    assert_eq!(resolve_command("/help\targ"), None);
    assert_eq!(resolve_command("/bind alpha"), None);
    assert_eq!(resolve_command("hello"), None);
}
#[test]
fn search_parser_invalid_operands_and_numeric_forms_match_ts() {
    let parsed = parse_search_args(
        "docker --limit nope --role invalid --session s -a -E --case-sensitive -n 0x10",
    );
    assert_eq!(parsed.query, "docker nope invalid");
    assert_eq!(parsed.limit, Some(16));
    assert!(parsed.all_projects && parsed.regex && parsed.case_sensitive);
    assert_eq!(parsed.role, None);
    assert_eq!(parsed.session_id.as_deref(), Some("s"));
    assert_eq!(parse_search_args("--limit 0.5").limit, Some(0));
}
#[tokio::test]
async fn switch_prefers_exact_then_case_then_substring_and_resets_only_bound_chat() {
    let gateway = gateway();
    let reply = Reply::default();
    let mut state = ChannelSessionState::default();
    state
        .active_by_chat_id
        .insert("other".into(), "keep".into());
    let mut ctx = CommandContext {
        gateway: &gateway,
        reply: &reply,
        state: &mut state,
        channel: "weixin",
        chat: "chat",
        home: Path::new("/isolated"),
    };
    assert!(
        execute_channel_command("/切换项目 alpha", &mut ctx)
            .await
            .unwrap()
    );
    assert_eq!(ctx.state.project("chat"), Some("/c"));
    assert!(
        ctx.state
            .session("weixin", "chat")
            .starts_with("weixin:chat=chat:s_")
    );
    assert_eq!(ctx.state.active_by_chat_id["other"], "keep");
    let previous = ctx.state.session("weixin", "chat");
    execute_channel_command("/switch-project absent", &mut ctx)
        .await
        .unwrap();
    assert_eq!(ctx.state.session("weixin", "chat"), previous);
    assert!(!execute_channel_command("/new", &mut ctx).await.unwrap());
    assert!(
        !execute_channel_command("/plan hello", &mut ctx)
            .await
            .unwrap()
    );
    execute_channel_command("/projects", &mut ctx)
        .await
        .unwrap();
    assert!(
        reply
            .0
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .contains("• alpha ✅")
    );
}
#[tokio::test]
async fn search_respects_project_scope_and_all_override() {
    let gateway = gateway();
    let reply = Reply::default();
    let mut state = ChannelSessionState::default();
    state
        .project_by_chat_id
        .insert("chat".into(), "/project".into());
    let mut ctx = CommandContext {
        gateway: &gateway,
        reply: &reply,
        state: &mut state,
        channel: "weixin",
        chat: "chat",
        home: Path::new("/isolated"),
    };
    execute_channel_command("/search hello -n 3 --role user", &mut ctx)
        .await
        .unwrap();
    execute_channel_command("/find hello --all", &mut ctx)
        .await
        .unwrap();
    let calls = gateway.searches.lock().unwrap();
    assert_eq!(calls[0].0.as_deref(), Some("/project"));
    assert_eq!(calls[0].1.limit, Some(3));
    assert_eq!(calls[0].1.role.as_deref(), Some("user"));
    assert_eq!(calls[1].0, None);
}
#[tokio::test]
async fn failed_reply_uses_command_error_fallback_and_reports_second_failure() {
    struct Fail;
    impl ChannelReply for Fail {
        fn reply<'a>(&'a self, _text: &'a str) -> ChannelCall<'a, ()> {
            Box::pin(async { Err(RouteError::new("DELIVERY_FAILED", "reply failed")) })
        }
    }
    let gateway = gateway();
    let mut state = ChannelSessionState::default();
    let mut ctx = CommandContext {
        gateway: &gateway,
        reply: &Fail,
        state: &mut state,
        channel: "weixin",
        chat: "chat",
        home: Path::new("/isolated"),
    };
    assert_eq!(
        execute_channel_command("/help", &mut ctx)
            .await
            .unwrap_err()
            .code,
        "DELIVERY_FAILED"
    );
}
#[test]
fn formatter_preserves_chinese_contract_and_empty_query() {
    assert_eq!(
        format_search_result(&json!({"query":"  "}), false),
        SEARCH_USAGE
    );
    assert_eq!(
        format_search_result(
            &json!({"query":"x","matches":[],"sessionsScanned":3}),
            false
        ),
        "未找到包含「x」的聊天记录。\n已扫描 3 个会话。"
    );
    let output = format_search_result(
        &json!({"query":"x","matches":[{"sessionId":"123456789012345","sessionTitle":"title","role":"user","snippet":"x","createdAt":"","projectKey":"/root/proj"}]}),
        true,
    );
    assert!(output.contains("(`12345678…`) · proj\n   用户 · 未知时间\n   > x"));
}

#[tokio::test]
async fn update_relays_only_last_line_exit_two_and_usage_without_starting_turn() {
    struct Update;
    impl ChannelGateway for Update {
        fn request<'a>(&'a self, _method: &'a str, _params: Value) -> ChannelCall<'a, Value> {
            Box::pin(async { panic!("unexpected RPC") })
        }
        fn turn(&self, _params: Value) -> ChannelCall<'_, Vec<Value>> {
            Box::pin(async { panic!("unexpected turn") })
        }
        fn update(&self, check: bool) -> ChannelCall<'_, (i32, String)> {
            Box::pin(async move {
                Ok(if check {
                    (2, "internal log".into())
                } else {
                    (1, "internal log\n\nlast outcome\n".into())
                })
            })
        }
    }
    let reply = Reply::default();
    let mut state = ChannelSessionState::default();
    let mut ctx = CommandContext {
        gateway: &Update,
        reply: &reply,
        state: &mut state,
        channel: "weixin",
        chat: "chat",
        home: Path::new("/isolated"),
    };
    execute_channel_command("/update check", &mut ctx)
        .await
        .unwrap();
    assert_eq!(
        reply.0.lock().unwrap().last().unwrap(),
        "✅ 已是最新 Release，无需更新。"
    );
    execute_channel_command("/更新", &mut ctx).await.unwrap();
    assert_eq!(reply.0.lock().unwrap().last().unwrap(), "❌ last outcome");
    let before = reply.0.lock().unwrap().len();
    execute_channel_command("/update invalid", &mut ctx)
        .await
        .unwrap();
    assert_eq!(reply.0.lock().unwrap().len(), before + 1);
    assert!(
        reply
            .0
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .starts_with("用法：/update check")
    );
}

#[tokio::test]
async fn status_failure_uses_status_specific_reply() {
    let reply = Reply::default();
    let mut state = ChannelSessionState::default();
    let gateway = gateway();
    let mut ctx = CommandContext {
        gateway: &gateway,
        reply: &reply,
        state: &mut state,
        channel: "weixin",
        chat: "chat",
        home: Path::new("/isolated"),
    };
    execute_channel_command("/状态", &mut ctx).await.unwrap();
    assert_eq!(
        reply.0.lock().unwrap().last().unwrap(),
        "获取状态失败: 状态提供者当前不可用。"
    );
}

#[tokio::test]
async fn status_reports_the_node_runtime_version_contract() {
    let gateway = WsChannelGateway::new("http://127.0.0.1:1".into(), "token".into());
    let status = gateway
        .status()
        .await
        .expect("status must not require an external Node runtime");
    assert!(status.contains("\nNode: v"), "status={status:?}");
    assert!(status.contains("\nNode: v22.22.0\n"), "status={status:?}");
    assert!(!status.contains("\nRust:"), "status={status:?}");
    assert!(status.starts_with("📊 PilotDeck Status\n\n"));
    assert!(status.contains("分支: "));
    assert!(status.contains("提交: "));
    assert!(status.contains("运行时间: "));
    assert!(status.contains("平台: "));
}

#[cfg(unix)]
#[test]
fn status_succeeds_without_invoking_external_node() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let git = std::env::split_paths(&std::env::var_os("PATH").unwrap())
        .map(|path| path.join("git"))
        .find(|path| path.is_file())
        .expect("git is required for the status fields");
    let dir = std::env::temp_dir().join(format!("pdrs-status-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&dir).unwrap();
    symlink(git, dir.join("git")).unwrap();
    let marker = dir.join("node.called");

    // Only the child test's PATH changes; parallel tests keep their environment.
    for fake_node in [false, true] {
        if fake_node {
            let node = dir.join("node");
            std::fs::write(&node, "#!/bin/sh\nprintf called > \"$0.called\"\nexit 99\n").unwrap();
            std::fs::set_permissions(&node, std::fs::Permissions::from_mode(0o755)).unwrap();
            assert_eq!(
                std::process::Command::new("node")
                    .env("PATH", &dir)
                    .status()
                    .unwrap()
                    .code(),
                Some(99)
            );
            assert!(marker.exists(), "the fake node must record invocation");
            std::fs::remove_file(&marker).unwrap();
        }
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "config::channel_commands::tests::status_reports_the_node_runtime_version_contract",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("PATH", &dir)
            .output()
            .unwrap();
        assert!(
            output.status.success() && String::from_utf8_lossy(&output.stdout).contains("1 passed"),
            "fake_node={fake_node}, output={output:?}"
        );
        assert!(!marker.exists(), "status must never invoke the fake node");
    }
    std::fs::remove_dir_all(dir).unwrap();
}
