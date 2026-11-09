//! Native gateway listener and configured channel owners.
use crate::{LocalServer, ProxyError, http_proxy::Proxy};
use hyper_util::{client::legacy::Client, rt::TokioExecutor};
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::net::TcpListener;
pub async fn serve(listen_override: Option<SocketAddr>) -> Result<(), ProxyError> {
    serve_impl(listen_override, false).await
}

/// Independent Web CLI listener (`ui/server/cli.js:190-225`). Its HOST default
/// and deployment contract are separate from GatewayServer's remote opt-in.
pub async fn serve_web(listen: SocketAddr) -> Result<(), ProxyError> {
    serve_impl(Some(listen), true).await
}

async fn serve_impl(listen_override: Option<SocketAddr>, web_cli: bool) -> Result<(), ProxyError> {
    let explicit_token = ["PILOTDECK_GATEWAY_TOKEN", "PILOTDECK_TOKEN"]
        .iter()
        .find_map(|key| {
            std::env::var(key)
                .ok()
                .filter(|value| !value.trim().is_empty())
        });
    let listen = if web_cli {
        listen_override.expect("Web CLI listener")
    } else {
        resolve_listen(
            listen_override,
            std::env::var("PILOTDECK_PROXY_LISTEN").ok().as_deref(),
            std::env::var("PILOTDECK_GATEWAY_HOST").ok().as_deref(),
            std::env::var("PILOTDECK_GATEWAY_PORT").ok().as_deref(),
            std::env::var("PILOTDECK_GATEWAY_ALLOW_REMOTE").as_deref() == Ok("1"),
            explicit_token.is_some(),
        )?
    };
    let proxy = Arc::new(Proxy {
        upstream: "http://127.0.0.1:1".into(),
        client: Client::builder(TokioExecutor::new()).build_http(),
    });
    // Resolve port zero before constructing clients that connect back to us.
    // Binding first also rejects an occupied port before starting automation.
    let listener = TcpListener::bind(listen).await?;
    let listen = listener.local_addr()?;
    let cwd = std::env::current_dir()?;
    let requested = std::env::var("PILOT_HOME")
        .ok()
        .filter(|home| !home.is_empty())
        .unwrap_or_else(|| "~/.pilotdeck".into());
    let home = if requested == "~" || requested.starts_with("~/") {
        std::path::PathBuf::from(std::env::var_os("HOME").ok_or("HOME is required")?)
            .join(requested.strip_prefix("~/").unwrap_or(""))
    } else {
        requested.into()
    };
    let home = crate::metadata::resolve_path(&cwd, &home);
    let token = explicit_token
        .map(Ok)
        .unwrap_or_else(|| gateway_token(&home))?;
    let mut server = LocalServer::with_token(proxy.clone(), token.clone())?
        .without_upstream_fallback()
        .with_home(home.clone(), cwd.clone());
    server.http.loopback = local_token_listener(listen);
    server = server.with_module_host(crate::http::runtime_bindings::bindings(
        home.clone(),
        cwd.clone(),
    ));
    if let Some(root) = std::env::var_os("PILOTDECK_STATIC_ASSETS_PATH") {
        server = server.with_static_assets(root);
    }
    let sidecar = std::env::var_os("PILOTDECK_SIDECAR")
        .or_else(|| std::env::var_os("PILOTDECK_AGENT_SIDECAR"))
        .map(std::path::PathBuf::from)
        .unwrap_or(std::env::current_exe()?.with_file_name("pilotdeck-agent-loop-sidecar"));
    let automation = crate::cron::Automation::new_with_user_dialogs(
        home.clone(),
        cwd.clone(),
        sidecar,
        server.user_dialogs(),
    )
    .await
    .map_err(|e| std::io::Error::other(format!("{}: {}", e.code, e.message)))?;
    server = server.with_automation(automation.clone());
    let mut channels = None;
    let env = std::env::vars().collect();
    if crate::config::has_execution_config(&home, &cwd, &env)? {
        let local_address = if listen.ip().is_unspecified() {
            SocketAddr::new(
                if listen.is_ipv4() {
                    std::net::Ipv4Addr::LOCALHOST.into()
                } else {
                    std::net::Ipv6Addr::LOCALHOST.into()
                },
                listen.port(),
            )
        } else {
            listen
        };
        let gateway = crate::config::channel_commands::WsChannelGateway::new(
            format!("http://{local_address}/ws"),
            token,
        );
        let config = crate::config::routes::bootstrap::ConfigBootstrap::new_with_channel_gateway(
            home.clone(),
            cwd.clone(),
            env.clone(),
            Some(gateway.clone()),
        )
        .map_err(|e| std::io::Error::other(format!("{}: {}", e.code, e.message)))?;
        let owner = crate::channel_runtime::InstalledChannels::new(
            home.clone(),
            cwd,
            env,
            gateway,
            automation.host.clone(),
            config.state.clone(),
            config.weixin.clone(),
            config.weixin_delivery.clone(),
        );
        server = server.with_feishu_webhook(owner.feishu_webhook());
        owner.start();
        channels = Some(owner);
        server = server.with_native_config(&config);
    }
    let server = Arc::new(server);
    server.start_memory_scheduler();
    eprintln!("gateway listening {listen}");
    eprintln!("metadata cwd: {}", std::env::current_dir()?.display());
    let accept_error = loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (socket, _) = match accepted {
                    Ok(connection) => connection,
                    Err(error) => {
                        eprintln!("downstream accept: {error}");
                        if fatal_accept_error(&error) {
                            break Some(error);
                        }
                        // EMFILE and transient network errors must not stop the listener
                        // or spin at full CPU while resources remain exhausted.
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                };
                let server = server.clone();
                tokio::spawn(async move {
                    if let Err(error) = server.serve_connection(socket).await {
                        eprintln!("downstream HTTP: {error}");
                    }
                });
            }
            _ = tokio::signal::ctrl_c() => break None,
        }
    };
    let channel_result = if let Some(channels) = channels {
        channels
            .close()
            .await
            .map_err(|error| std::io::Error::other(error.message))
    } else {
        Ok(())
    };
    let server_result = server.stop().await;
    channel_result?;
    server_result?;
    if let Some(error) = accept_error {
        return Err(error.into());
    }
    Ok(())
}

pub(crate) fn email_options(
    snapshot: &pilotdeck_protocol::JsValue,
    env: &crate::config::Env,
) -> Option<crate::config::routes::email::EmailOptions> {
    use pilotdeck_protocol::JsValue;
    let config = enabled_adapter(snapshot, "email")?;
    let extra = config.get("extra");
    let text = |key, variable| {
        channel_text(extra.and_then(|v| v.get(key)))
            .or_else(|| env.get(variable).cloned())
            .unwrap_or_default()
    };
    let number = |key, variable: &str, default| {
        extra
            .and_then(|v| v.get(key))
            .filter(|v| !matches!(v, JsValue::Null | JsValue::Undefined))
            .map(|v| pilotdeck_protocol::js_value::to_number(v).unwrap_or(f64::NAN))
            .or_else(|| {
                env.get(variable)
                    .map(|s| pilotdeck_protocol::js_value::string_to_number(s))
            })
            .unwrap_or(default)
    };
    let imap_port = number("imapPort", "IMAP_PORT", 993.0);
    let smtp_port = number("smtpPort", "SMTP_PORT", 587.0);
    if [imap_port, smtp_port]
        .iter()
        .any(|n| !n.is_finite() || n.fract() != 0.0 || !(0.0..=65535.0).contains(n))
    {
        eprintln!("email: invalid port");
        return None;
    }
    let delay = number("pollIntervalMs", "", 45000.0);
    Some(crate::config::routes::email::EmailOptions {
        address: text("address", "EMAIL_ADDRESS"),
        password: text("password", "EMAIL_PASSWORD"),
        imap_host: text("imapHost", "IMAP_HOST"),
        smtp_host: text("smtpHost", "SMTP_HOST"),
        imap_port: imap_port as u16,
        smtp_port: smtp_port as u16,
        poll_interval_ms: if !delay.is_finite() || delay < 1.0 || delay > 2147483647.0 {
            1
        } else {
            delay.trunc() as u64
        },
        imap_tls: extra.and_then(|v| v.get("imapTls")) != Some(&JsValue::Bool(false)),
        smtp_tls: extra.and_then(|v| v.get("smtpTls")) != Some(&JsValue::Bool(false)),
        default_subject: channel_text(extra.and_then(|v| v.get("defaultSubject")))
            .unwrap_or_else(|| "Message".into()),
    })
}
pub(crate) fn feishu_options(
    snapshot: &pilotdeck_protocol::JsValue,
    home: &std::path::Path,
) -> Option<crate::config::routes::feishu::FeishuOptions> {
    let config = enabled_adapter(snapshot, "feishu")?;
    Some(crate::config::routes::feishu::FeishuOptions {
        app_id: channel_text(config.get("appId")).unwrap_or_default(),
        app_secret: channel_text(config.get("appSecret")).unwrap_or_default(),
        encrypt_key: channel_text(config.get("encryptKey")),
        verify_token: channel_text(config.get("verifyToken")),
        connection_mode: channel_text(config.get("connectionMode"))
            .unwrap_or_else(|| "stream".into()),
        domain_name: channel_text(config.get("domainName")).unwrap_or_else(|| "feishu".into()),
        permission_mode: channel_text(config.get("permissionMode")),
        pilot_home: home.into(),
        ..Default::default()
    })
}
pub(crate) fn wecom_options(
    snapshot: &pilotdeck_protocol::JsValue,
    env: &crate::config::Env,
    home: &std::path::Path,
) -> Option<crate::config::routes::wecom::WeComOptions> {
    use pilotdeck_protocol::JsValue;
    let config = enabled_adapter(snapshot, "wecom")?;
    let extra = config.get("extra");
    let get = |snake, camel| {
        extra
            .and_then(|v| v.get(snake))
            .filter(|v| !matches!(v, JsValue::Null | JsValue::Undefined))
            .or_else(|| {
                extra
                    .and_then(|v| v.get(camel))
                    .filter(|v| !matches!(v, JsValue::Null | JsValue::Undefined))
            })
    };
    let text = |snake, camel, variable| {
        channel_text(get(snake, camel)).or_else(|| env.get(variable).cloned())
    };
    let list = |value: Option<&JsValue>| {
        value
            .and_then(|v| crate::cron::native::from_js(v).ok())
            .map(|v| crate::config::routes::wecom::coerce_list(&v))
            .unwrap_or_default()
    };
    let delay = |snake, camel, default| {
        get(snake, camel)
            .and_then(|v| pilotdeck_protocol::js_value::to_number(v).ok())
            .filter(|v| v.is_finite() && *v >= 0.0)
            .map(|v| v as u64)
            .unwrap_or(default)
    };
    Some(crate::config::routes::wecom::WeComOptions {
        bot_key: channel_text(config.get("token"))
            .or_else(|| text("bot_id", "botId", "WECOM_BOT_ID"))
            .unwrap_or_default(),
        bot_secret: text("botSecret", "secret", "WECOM_SECRET").unwrap_or_default(),
        websocket_url: text("websocket_url", "websocketUrl", "WECOM_WEBSOCKET_URL")
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| "wss://openws.work.weixin.qq.com".into()),
        dm_policy: text("dm_policy", "dmPolicy", "WECOM_DM_POLICY")
            .unwrap_or_else(|| "pairing".into()),
        allow_from: if let Some(value) = get("allow_from", "allowFrom") {
            list(Some(value))
        } else {
            env.get("WECOM_ALLOWED_USERS")
                .map(|s| {
                    crate::config::routes::wecom::coerce_list(&serde_json::Value::String(s.clone()))
                })
                .unwrap_or_default()
        },
        group_policy: text("group_policy", "groupPolicy", "WECOM_GROUP_POLICY")
            .unwrap_or_else(|| "pairing".into()),
        group_allow_from: list(get("group_allow_from", "groupAllowFrom")),
        groups: extra
            .and_then(|v| v.get("groups"))
            .and_then(|v| crate::cron::native::from_js(v).ok())
            .filter(serde_json::Value::is_object)
            .unwrap_or_else(|| serde_json::json!({})),
        group_sessions_per_user: get("group_sessions_per_user", "groupSessionsPerUser")
            .and_then(JsValue::as_bool)
            .unwrap_or(true),
        text_batch_delay_ms: delay("text_batch_delay_ms", "textBatchDelayMs", 600),
        text_batch_split_delay_ms: delay(
            "text_batch_split_delay_ms",
            "textBatchSplitDelayMs",
            2000,
        ),
        pilot_home: home.into(),
    })
}
pub(crate) fn qq_options(
    snapshot: &pilotdeck_protocol::JsValue,
    env: &crate::config::Env,
    store: &std::sync::Arc<crate::config::routes::ChannelStatePersistence>,
) -> Option<crate::config::routes::qq::QQOptions> {
    use pilotdeck_protocol::JsValue;
    let config = enabled_adapter(snapshot, "qq")?;
    let strings = |value: Option<&JsValue>| {
        value
            .and_then(|v| {
                if let JsValue::Array(items) = v {
                    Some(items)
                } else {
                    None
                }
            })
            .map(|items| {
                items
                    .iter()
                    .filter_map(|v| channel_text(Some(v)))
                    .collect::<Vec<_>>()
            })
    };
    let initial_state = store
        .load("qq")
        .unwrap_or_else(|| serde_json::json!({"activeByChatKey":{}}));
    Some(crate::config::routes::qq::QQOptions {
        app_id: channel_text(config.get("appId"))
            .or_else(|| env.get("QQ_BOT_APPID").cloned())
            .unwrap_or_default(),
        client_secret: channel_text(config.get("clientSecret"))
            .or_else(|| env.get("QQ_BOT_SECRET").cloned())
            .unwrap_or_default(),
        allow_groups: strings(config.get("allowGroups"))
            .or_else(|| {
                env.get("QQ_ALLOW_GROUPS").map(|s| {
                    s.split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned)
                        .collect()
                })
            })
            .unwrap_or_else(|| vec!["*".into()]),
        trigger_prefixes: strings(config.get("triggerPrefixes"))
            .unwrap_or_else(|| vec!["/ask".into(), "/chat".into()]),
        max_message_length: config
            .get("maxMessageLength")
            .and_then(JsValue::as_number)
            .map(|v| v.max(1.0) as usize)
            .unwrap_or(2000),
        initial_state,
        on_state_change: Some(store.saver("qq")),
        ..Default::default()
    })
}
pub(crate) fn persist_channel_state(
    path: &std::path::Path,
    state: &serde_json::Value,
) -> std::io::Result<()> {
    let parent = path.parent().expect("channel state directory");
    std::fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(".channel-state-{}.tmp", uuid::Uuid::new_v4()));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    use std::io::Write;
    file.write_all(serde_json::to_string_pretty(state)?.as_bytes())?;
    file.sync_all()?;
    std::fs::rename(temporary, path)?;
    Ok(())
}
pub(crate) fn wecom_callback_options(
    snapshot: &pilotdeck_protocol::JsValue,
    env: &crate::config::Env,
) -> Option<crate::config::routes::wecom_callback::WeComCallbackOptions> {
    let config = enabled_adapter(snapshot, "wecomCallback")?;
    let extra = config.get("extra");
    let text = |key, variable| {
        channel_text(extra.and_then(|v| v.get(key)))
            .or_else(|| env.get(variable).cloned())
            .unwrap_or_default()
    };
    let mut options = crate::config::routes::wecom_callback::WeComCallbackOptions {
        corp_id: text("corpId", "WECOM_CORP_ID"),
        agent_id: text("agentId", "WECOM_AGENT_ID"),
        secret: text("secret", "WECOM_CB_SECRET"),
        token: channel_text(config.get("token"))
            .or_else(|| env.get("WECOM_CB_TOKEN").cloned())
            .unwrap_or_default(),
        encoding_aes_key: text("encodingAesKey", "WECOM_ENCODING_AES_KEY"),
        ..Default::default()
    };
    let port = extra
        .and_then(|v| v.get("port"))
        .filter(|v| {
            !matches!(
                v,
                pilotdeck_protocol::JsValue::Null | pilotdeck_protocol::JsValue::Undefined
            )
        })
        .map(|v| pilotdeck_protocol::js_value::to_number(v).unwrap_or(f64::NAN))
        .or_else(|| {
            env.get("WECOM_CB_PORT")
                .map(|s| pilotdeck_protocol::js_value::string_to_number(s))
        });
    if let Some(port) = port {
        if !port.is_finite() || port.fract() != 0.0 || !(0.0..=65535.0).contains(&port) {
            eprintln!("wecom-callback: invalid port");
            return None;
        }
        options.port = port as u16;
    }
    Some(options)
}

#[derive(Clone, Default)]
pub(crate) struct WecomWebhookCrypto {
    pub token: String,
    pub encoding_aes_key: String,
    pub corp_id: String,
}

/// Signing and decrypt material for the production WeCom webhook
/// (`channel_webhook.rs`), aligned with WeComCallbackChannel callback options.
pub(crate) fn wecom_webhook_crypto(
    snapshot: &pilotdeck_protocol::JsValue,
    env: &crate::config::Env,
) -> WecomWebhookCrypto {
    if let Some(options) = wecom_callback_options(snapshot, env) {
        return WecomWebhookCrypto {
            token: options.token,
            encoding_aes_key: options.encoding_aes_key,
            corp_id: options.corp_id,
        };
    }
    let Some(config) = enabled_adapter(snapshot, "wecom") else {
        return WecomWebhookCrypto::default();
    };
    let extra = config.get("extra");
    let extra_text = |snake: &str, camel: &str, variable: &str| {
        channel_text(extra.and_then(|v| v.get(snake)))
            .or_else(|| channel_text(extra.and_then(|v| v.get(camel))))
            .or_else(|| env.get(variable).cloned())
            .unwrap_or_default()
    };
    let mut token = channel_text(config.get("token")).unwrap_or_default();
    if token.is_empty() {
        token = extra_text("token", "token", "WECOM_WEBHOOK_TOKEN");
    }
    WecomWebhookCrypto {
        token,
        encoding_aes_key: extra_text(
            "encoding_aes_key",
            "encodingAesKey",
            "WECOM_ENCODING_AES_KEY",
        ),
        corp_id: extra_text("corp_id", "corpId", "WECOM_CORP_ID"),
    }
}

pub(crate) fn dingtalk_options(
    snapshot: &pilotdeck_protocol::JsValue,
    env: &crate::config::Env,
) -> Option<crate::config::routes::dingtalk::DingTalkOptions> {
    let config = enabled_adapter(snapshot, "dingtalk")?;
    Some(crate::config::routes::dingtalk::DingTalkOptions {
        client_id: channel_text(config.get("extra").and_then(|v| v.get("clientId")))
            .or_else(|| env.get("DINGTALK_CLIENT_ID").cloned())
            .unwrap_or_default(),
        client_secret: channel_text(config.get("extra").and_then(|v| v.get("clientSecret")))
            .or_else(|| env.get("DINGTALK_CLIENT_SECRET").cloned())
            .unwrap_or_default(),
        ..Default::default()
    })
}
pub(crate) fn whatsapp_options(
    snapshot: &pilotdeck_protocol::JsValue,
    env: &crate::config::Env,
) -> Option<crate::config::routes::whatsapp::WhatsAppOptions> {
    let config = enabled_adapter(snapshot, "whatsapp")?;
    let extra = config.get("extra");
    Some(crate::config::routes::whatsapp::WhatsAppOptions {
        bridge_path: channel_text(extra.and_then(|v| v.get("bridgePath")))
            .or_else(|| env.get("WHATSAPP_BRIDGE_PATH").cloned())
            .unwrap_or_default()
            .into(),
        bridge_url: channel_text(extra.and_then(|v| v.get("bridgeUrl")))
            .or_else(|| env.get("WHATSAPP_BRIDGE_URL").cloned())
            .unwrap_or_else(|| "http://127.0.0.1:3100".into()),
        ..Default::default()
    })
}
pub(crate) fn sms_options(
    snapshot: &pilotdeck_protocol::JsValue,
    env: &crate::config::Env,
) -> Option<crate::config::routes::sms::SmsOptions> {
    let config = enabled_adapter(snapshot, "sms")?;
    let extra = config.get("extra");
    let text = |key| channel_text(extra.and_then(|v| v.get(key)));
    let mut options = crate::config::routes::sms::SmsOptions::default();
    options.account_sid = text("accountSid")
        .or_else(|| env.get("TWILIO_ACCOUNT_SID").cloned())
        .unwrap_or_default();
    options.auth_token = text("authToken")
        .or_else(|| text("apiKey"))
        .or_else(|| env.get("TWILIO_AUTH_TOKEN").cloned())
        .or_else(|| text("token"))
        .unwrap_or_default();
    options.phone_number = text("phoneNumber")
        .or_else(|| env.get("TWILIO_PHONE_NUMBER").cloned())
        .unwrap_or_default();
    options.webhook_host = text("webhookHost").unwrap_or(options.webhook_host);
    options.webhook_path = text("webhookPath").unwrap_or(options.webhook_path);
    options.public_url = text("publicUrl").unwrap_or_default();
    let port = extra
        .and_then(|v| v.get("webhookPort"))
        .filter(|v| {
            !matches!(
                v,
                pilotdeck_protocol::JsValue::Null | pilotdeck_protocol::JsValue::Undefined
            )
        })
        .map(|v| pilotdeck_protocol::js_value::to_number(v).unwrap_or(f64::NAN))
        .or_else(|| {
            env.get("TWILIO_WEBHOOK_PORT")
                .map(|v| pilotdeck_protocol::js_value::string_to_number(v))
        });
    if let Some(port) = port {
        if !port.is_finite() || port.fract() != 0.0 || !(0.0..=65535.0).contains(&port) {
            eprintln!("sms: invalid webhook port");
            return None;
        }
        options.webhook_port = port as u16;
    }
    Some(options)
}

pub(crate) fn enabled_adapter<'a>(
    snapshot: &'a pilotdeck_protocol::JsValue,
    name: &str,
) -> Option<&'a pilotdeck_protocol::JsValue> {
    let config = snapshot.get("config")?.get("adapters")?.get(name)?;
    (config.get("enabled") == Some(&pilotdeck_protocol::JsValue::Bool(true))).then_some(config)
}
pub(crate) fn channel_text(value: Option<&pilotdeck_protocol::JsValue>) -> Option<String> {
    value
        .and_then(pilotdeck_protocol::JsValue::as_string)
        .map(|s| String::from_utf16_lossy(s.units()))
}
pub(crate) fn home_assistant_options(
    snapshot: &pilotdeck_protocol::JsValue,
    env: &crate::config::Env,
) -> Option<crate::config::routes::home_assistant::HomeAssistantOptions> {
    let config = enabled_adapter(snapshot, "homeassistant")?;
    Some(
        crate::config::routes::home_assistant::HomeAssistantOptions {
            url: channel_text(config.get("extra").and_then(|v| v.get("url")))
                .or_else(|| env.get("HASS_URL").cloned())
                .unwrap_or_else(|| "http://127.0.0.1:8123".into()),
            token: channel_text(config.get("token"))
                .or_else(|| env.get("HASS_TOKEN").cloned())
                .unwrap_or_default(),
            ..Default::default()
        },
    )
}
pub(crate) fn matrix_options(
    snapshot: &pilotdeck_protocol::JsValue,
    env: &crate::config::Env,
    home: &std::path::Path,
) -> Option<crate::config::routes::matrix::MatrixOptions> {
    let config = enabled_adapter(snapshot, "matrix")?;
    Some(crate::config::routes::matrix::MatrixOptions {
        access_token: channel_text(config.get("token"))
            .or_else(|| env.get("MATRIX_ACCESS_TOKEN").cloned())
            .unwrap_or_default(),
        homeserver: channel_text(config.get("extra").and_then(|v| v.get("homeserver")))
            .or_else(|| env.get("MATRIX_HOMESERVER").cloned())
            .unwrap_or_default(),
        user_id: channel_text(config.get("extra").and_then(|v| v.get("userId")))
            .or_else(|| env.get("MATRIX_USER_ID").cloned()),
        storage_path: home.join("matrix-bot-storage.json"),
    })
}
pub(crate) fn mattermost_options(
    snapshot: &pilotdeck_protocol::JsValue,
    env: &crate::config::Env,
) -> Option<crate::config::routes::mattermost::MattermostOptions> {
    let config = enabled_adapter(snapshot, "mattermost")?;
    Some(crate::config::routes::mattermost::MattermostOptions {
        token: channel_text(config.get("token"))
            .or_else(|| env.get("MATTERMOST_TOKEN").cloned())
            .unwrap_or_default(),
        server_url: channel_text(config.get("extra").and_then(|v| v.get("serverUrl")))
            .or_else(|| env.get("MATTERMOST_URL").cloned())
            .unwrap_or_default(),
        team_id: channel_text(config.get("extra").and_then(|v| v.get("teamId")))
            .or_else(|| env.get("MATTERMOST_TEAM_ID").cloned()),
    })
}

pub(crate) fn telegram_options(
    snapshot: &pilotdeck_protocol::JsValue,
    env: &crate::config::Env,
) -> Option<crate::config::routes::telegram::TelegramOptions> {
    use pilotdeck_protocol::JsValue;
    let config = snapshot.get("config")?.get("adapters")?.get("telegram")?;
    if config.get("enabled") != Some(&JsValue::Bool(true)) {
        return None;
    }
    let text = |value: Option<&JsValue>| {
        value
            .and_then(JsValue::as_string)
            .map(|value| String::from_utf16_lossy(value.units()))
    };
    Some(crate::config::routes::telegram::TelegramOptions {
        token: text(config.get("token"))
            .or_else(|| env.get("TELEGRAM_BOT_TOKEN").cloned())
            .unwrap_or_default(),
        webhook_url: text(config.get("webhookUrl")),
        ..Default::default()
    })
}

pub(crate) fn bluebubbles_options(
    snapshot: &pilotdeck_protocol::JsValue,
    env: &crate::config::Env,
) -> Option<crate::config::routes::bluebubbles::BlueBubblesOptions> {
    use pilotdeck_protocol::JsValue;
    let config = snapshot
        .get("config")?
        .get("adapters")?
        .get("bluebubbles")?;
    if config.get("enabled") != Some(&JsValue::Bool(true)) {
        return None;
    }
    let text = |value: Option<&JsValue>| {
        value
            .and_then(JsValue::as_string)
            .map(|value| String::from_utf16_lossy(value.units()))
    };
    Some(crate::config::routes::bluebubbles::BlueBubblesOptions {
        server_url: text(config.get("extra").and_then(|extra| extra.get("serverUrl")))
            .or_else(|| env.get("BLUEBUBBLES_SERVER_URL").cloned())
            .unwrap_or_default(),
        password: text(config.get("token"))
            .or_else(|| text(config.get("extra").and_then(|extra| extra.get("password"))))
            .or_else(|| env.get("BLUEBUBBLES_PASSWORD").cloned())
            .unwrap_or_default(),
    })
}

pub(crate) fn signal_options(
    snapshot: &pilotdeck_protocol::JsValue,
    env: &crate::config::Env,
) -> Option<crate::config::routes::signal::SignalOptions> {
    use pilotdeck_protocol::JsValue;
    let config = snapshot.get("config")?.get("adapters")?.get("signal")?;
    if config.get("enabled") != Some(&JsValue::Bool(true)) {
        return None;
    }
    let text = |key| {
        config
            .get("extra")
            .and_then(|extra| extra.get(key))
            .and_then(JsValue::as_string)
            .map(|value| String::from_utf16_lossy(value.units()))
    };
    Some(crate::config::routes::signal::SignalOptions {
        rest_url: text("restUrl")
            .or_else(|| env.get("SIGNAL_HTTP_URL").cloned())
            .unwrap_or_else(|| "http://127.0.0.1:8080".into()),
        account: text("account")
            .or_else(|| env.get("SIGNAL_ACCOUNT").cloned())
            .unwrap_or_default(),
    })
}

pub(crate) fn slack_options(
    snapshot: &pilotdeck_protocol::JsValue,
    env: &crate::config::Env,
) -> Option<crate::config::routes::slack::SlackOptions> {
    use pilotdeck_protocol::JsValue;
    let config = snapshot.get("config")?.get("adapters")?.get("slack")?;
    if config.get("enabled") != Some(&JsValue::Bool(true)) {
        return None;
    }
    let text = |value: Option<&JsValue>| {
        value
            .and_then(JsValue::as_string)
            .map(|value| String::from_utf16_lossy(value.units()))
    };
    Some(crate::config::routes::slack::SlackOptions {
        bot_token: text(config.get("token"))
            .or_else(|| env.get("SLACK_BOT_TOKEN").cloned())
            .unwrap_or_default(),
        app_token: text(config.get("extra").and_then(|extra| extra.get("appToken")))
            .or_else(|| env.get("SLACK_APP_TOKEN").cloned())
            .unwrap_or_default(),
        ..Default::default()
    })
}

pub(crate) fn http_channel_options(
    snapshot: &pilotdeck_protocol::JsValue,
    env: &crate::config::Env,
) -> (
    Option<crate::config::routes::channel_http::ApiServerOptions>,
    Option<crate::config::routes::channel_http::WebhookOptions>,
) {
    use crate::config::routes::channel_http::{ApiServerOptions, WebhookOptions};
    use pilotdeck_protocol::JsValue;
    let adapters = snapshot.get("config").and_then(|v| v.get("adapters"));
    let enabled = |name| {
        adapters
            .and_then(|v| v.get(name))
            .filter(|v| v.get("enabled") == Some(&JsValue::Bool(true)))
    };
    let text = |value: Option<&JsValue>| {
        value
            .and_then(JsValue::as_string)
            .map(|s| String::from_utf16_lossy(s.units()))
    };
    let port = |value: Option<&JsValue>,
                fallback: Option<&String>,
                default: u16|
     -> Result<u16, ProxyError> {
        if let Some(value) = value {
            let number = value.as_number().ok_or("Channel port must be a number")?;
            if number.is_finite() && number.fract() == 0.0 && (0.0..=65535.0).contains(&number) {
                return Ok(number as u16);
            }
            return Err("Invalid channel port".into());
        }
        Ok(match fallback {
            Some(value) => value.parse()?,
            None => default,
        })
    };
    let api = enabled("apiServer")
        .map(|config| -> Result<_, ProxyError> {
            let mut options = ApiServerOptions::default();
            options.port = port(
                config.get("extra").and_then(|v| v.get("port")),
                env.get("API_SERVER_PORT"),
                options.port,
            )?;
            options.host = env.get("API_SERVER_HOST").cloned().unwrap_or(options.host);
            options.api_key = text(config.get("apiKey"))
                .or_else(|| env.get("API_SERVER_KEY").cloned())
                .unwrap_or_default();
            options.model_name = env
                .get("API_SERVER_MODEL_NAME")
                .cloned()
                .unwrap_or(options.model_name);
            options.cors_origins = env
                .get("API_SERVER_CORS_ORIGINS")
                .map(|s| {
                    s.split(',')
                        .map(crate::metadata::js_trim)
                        .filter(|v| !v.is_empty())
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            Ok(options)
        })
        .transpose()
        .unwrap_or_else(|error| {
            eprintln!("api-server: invalid configuration: {error}");
            None
        });
    let webhook = enabled("webhook")
        .map(|config| -> Result<_, ProxyError> {
            let mut options = WebhookOptions::default();
            options.port = port(
                config.get("extra").and_then(|v| v.get("port")),
                None,
                options.port,
            )?;
            options.secret =
                text(config.get("extra").and_then(|v| v.get("secret"))).unwrap_or_default();
            Ok(options)
        })
        .transpose()
        .unwrap_or_else(|error| {
            eprintln!("webhook: invalid configuration: {error}");
            None
        });
    (api, webhook)
}

fn resolve_listen(
    listen_override: Option<SocketAddr>,
    legacy_listen: Option<&str>,
    host: Option<&str>,
    port: Option<&str>,
    allow_remote: bool,
    explicit_token: bool,
) -> Result<SocketAddr, ProxyError> {
    let listen = if let Some(listen) = listen_override {
        listen
    } else if let Some(listen) = legacy_listen.filter(|value| !value.trim().is_empty()) {
        listen.parse()?
    } else {
        let host = host
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("127.0.0.1");
        let port = port
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("18789")
            .parse::<u16>()?;
        let ip = if host.eq_ignore_ascii_case("localhost") {
            std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
        } else {
            host.parse()?
        };
        SocketAddr::new(ip, port)
    };
    if !local_token_listener(listen) {
        if !allow_remote {
            return Err("GatewayServer remote binding requires allowRemoteHost=true.".into());
        }
        if !explicit_token {
            return Err("GatewayServer remote binding requires an explicit auth token.".into());
        }
    }
    Ok(listen)
}

fn local_token_listener(listen: SocketAddr) -> bool {
    // GatewayServer.ts:49 recognizes 127.0.0.1 and localhost only.
    listen.ip() == std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
}

fn configured_listen(
    listen_override: Option<SocketAddr>,
    explicit_listen: Option<&str>,
    runtime_env: &crate::config::Env,
) -> Result<SocketAddr, ProxyError> {
    Ok(match (listen_override, explicit_listen) {
        (Some(listen), _) => listen,
        (None, Some(listen)) => listen.parse()?,
        (None, None) => format!(
            "{}:{}",
            runtime_env
                .get("HOST")
                .map(String::as_str)
                .unwrap_or("0.0.0.0"),
            runtime_env
                .get("SERVER_PORT")
                .map(String::as_str)
                .unwrap_or("3001")
        )
        .parse()?,
    })
}

fn gateway_token(home: &std::path::Path) -> std::io::Result<String> {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use std::{
        fs::{self, OpenOptions},
        io::{Read, Write},
        os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    };
    let path = home.join("server-token");
    match fs::read_to_string(&path) {
        Ok(token) if !token.trim().is_empty() => return Ok(token.trim().to_owned()),
        Ok(_) => return Err(std::io::Error::other("gateway token file is empty")),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(home)?;
    let mut random = [0; 32];
    fs::File::open("/dev/urandom")?.read_exact(&mut random)?;
    let token = URL_SAFE_NO_PAD.encode(random);
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path);
    let mut file = match file {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let existing = fs::read_to_string(path)?;
            return if existing.trim().is_empty() {
                Err(std::io::Error::other("gateway token file is empty"))
            } else {
                Ok(existing.trim().to_owned())
            };
        }
        Err(error) => return Err(error),
    };
    writeln!(file, "{token}")?;
    file.sync_all()?;
    Ok(token)
}

fn fatal_accept_error(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::InvalidInput
            | std::io::ErrorKind::PermissionDenied
            | std::io::ErrorKind::Unsupported
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn webui_runtime_selects_listener_after_explicit_inputs() {
        let runtime = crate::config::Env::from([
            ("HOST".into(), "0.0.0.0".into()),
            ("SERVER_PORT".into(), "4300".into()),
        ]);
        assert_eq!(
            configured_listen(None, None, &runtime).unwrap(),
            "0.0.0.0:4300".parse().unwrap()
        );
        assert_eq!(
            configured_listen(None, Some("127.0.0.1:4400"), &runtime).unwrap(),
            "127.0.0.1:4400".parse().unwrap()
        );
        let explicit = "127.0.0.1:4500".parse().unwrap();
        assert_eq!(
            configured_listen(Some(explicit), Some("127.0.0.1:4400"), &runtime).unwrap(),
            explicit
        );
    }

    #[test]
    fn transient_accept_errors_do_not_stop_listener() {
        for error in [
            std::io::Error::from_raw_os_error(24),
            std::io::ErrorKind::ConnectionAborted.into(),
            std::io::ErrorKind::Interrupted.into(),
        ] {
            assert!(!fatal_accept_error(&error));
        }
        assert!(fatal_accept_error(&std::io::ErrorKind::InvalidInput.into()));
    }
    #[test]
    fn gateway_host_port_and_remote_safety_match_ts_contract() {
        assert_eq!(
            resolve_listen(None, None, None, None, false, false).unwrap(),
            "127.0.0.1:18789".parse().unwrap()
        );
        assert_eq!(
            resolve_listen(None, None, Some("127.0.0.1"), Some("18789"), false, false).unwrap(),
            "127.0.0.1:18789".parse().unwrap()
        );
        for host in ["127.0.0.2", "::1"] {
            assert!(resolve_listen(None, None, Some(host), None, false, true).is_err());
            let listen = resolve_listen(None, None, Some(host), None, true, true).unwrap();
            assert!(!local_token_listener(listen));
        }
        assert_eq!(
            resolve_listen(None, None, Some("localhost"), Some("18789"), false, false).unwrap(),
            "127.0.0.1:18789".parse().unwrap()
        );
        assert!(
            resolve_listen(None, None, Some("0.0.0.0"), Some("18789"), false, true)
                .unwrap_err()
                .to_string()
                .contains("allowRemoteHost")
        );
        assert!(
            resolve_listen(None, None, Some("0.0.0.0"), Some("18789"), true, false)
                .unwrap_err()
                .to_string()
                .contains("explicit auth token")
        );
        assert_eq!(
            resolve_listen(None, None, Some("0.0.0.0"), Some("18789"), true, true).unwrap(),
            "0.0.0.0:18789".parse().unwrap()
        );
        assert_eq!(
            resolve_listen(
                None,
                Some("127.0.0.1:29999"),
                Some("0.0.0.0"),
                None,
                false,
                false
            )
            .unwrap(),
            "127.0.0.1:29999".parse().unwrap()
        );
    }

    #[test]
    fn telegram_loader_preserves_explicit_empty_and_nullish_tokens() {
        use pilotdeck_protocol::parse_json;
        let env = [("TELEGRAM_BOT_TOKEN".into(), "fallback".into())]
            .into_iter()
            .collect();
        let snapshot = parse_json(r#"{"config":{"adapters":{"telegram":{"enabled":true,"token":"","webhookUrl":"https://example.invalid/hook"}}}}"#).unwrap();
        let options = telegram_options(&snapshot, &env).unwrap();
        assert_eq!(options.token, "");
        assert_eq!(
            options.webhook_url.as_deref(),
            Some("https://example.invalid/hook")
        );
        let snapshot =
            parse_json(r#"{"config":{"adapters":{"telegram":{"enabled":true,"token":null}}}}"#)
                .unwrap();
        assert_eq!(telegram_options(&snapshot, &env).unwrap().token, "fallback");
        assert!(
            telegram_options(
                &parse_json(r#"{"config":{"adapters":{"telegram":{"enabled":false}}}}"#).unwrap(),
                &env
            )
            .is_none()
        );
    }

    #[test]
    fn slack_loader_uses_token_and_extra_app_token_with_nullish_defaults() {
        use pilotdeck_protocol::parse_json;
        let env = [
            ("SLACK_BOT_TOKEN".into(), "bot".into()),
            ("SLACK_APP_TOKEN".into(), "app".into()),
        ]
        .into_iter()
        .collect();
        let snapshot = parse_json(r#"{"config":{"adapters":{"slack":{"enabled":true,"token":"","extra":{"appToken":null}}}}}"#).unwrap();
        let options = slack_options(&snapshot, &env).unwrap();
        assert_eq!(options.bot_token, "");
        assert_eq!(options.app_token, "app");
    }

    #[test]
    fn bluebubbles_and_signal_loader_preserve_platform_specific_priority() {
        use pilotdeck_protocol::parse_json;
        let env = [
            ("BLUEBUBBLES_PASSWORD".into(), "env-pass".into()),
            ("SIGNAL_ACCOUNT".into(), "env-account".into()),
        ]
        .into_iter()
        .collect();
        let snapshot = parse_json(r#"{"config":{"adapters":{"bluebubbles":{"enabled":true,"token":"","extra":{"password":"extra","serverUrl":"http://localhost:7777"}},"signal":{"enabled":true,"extra":{"account":""}}}}}"#).unwrap();
        let blue = bluebubbles_options(&snapshot, &env).unwrap();
        assert_eq!(blue.password, "");
        assert_eq!(blue.server_url, "http://localhost:7777");
        let signal = signal_options(&snapshot, &env).unwrap();
        assert_eq!(signal.account, "");
        assert_eq!(signal.rest_url, "http://127.0.0.1:8080");
    }

    #[test]
    fn enabled_http_channels_apply_env_and_isolate_invalid_channel_configuration() {
        use pilotdeck_protocol::parse_json;
        let snapshot = parse_json(r#"{"config":{"adapters":{"apiServer":{"enabled":true,"apiKey":"","extra":{"port":0}},"webhook":{"enabled":true,"extra":{"port":-1}}}}}"#).unwrap();
        let env = [
            ("API_SERVER_PORT".into(), "7777".into()),
            ("API_SERVER_KEY".into(), "ignored".into()),
            ("API_SERVER_HOST".into(), "127.0.0.1".into()),
            (
                "API_SERVER_CORS_ORIGINS".into(),
                "https://a, ,https://b".into(),
            ),
        ]
        .into_iter()
        .collect();
        let (api, webhook) = http_channel_options(&snapshot, &env);
        let api = api.unwrap();
        assert_eq!(api.port, 0);
        assert_eq!(api.api_key, "");
        assert_eq!(api.cors_origins, ["https://a", "https://b"]);
        assert!(webhook.is_none());
        let (api, webhook) = http_channel_options(&parse_json(r#"{"config":{"adapters":{"apiServer":{"enabled":false},"webhook":{"enabled":true}}}}"#).unwrap(), &env);
        assert!(api.is_none());
        assert_eq!(webhook.unwrap().port, 8643);
    }
}
