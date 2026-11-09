//! Standalone native Web CLI. Contract: TS ui/server/cli.js:49-84,126-225.
use crate::ProxyError;
use pilotdeck_protocol::{JsString, JsValue};
use std::{
    collections::HashMap,
    io::Write,
    net::SocketAddr,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

const NATIVE_ARGUMENT: &str = "--pilotdeck-web-native-serve";
const NATIVE_ENV: &str = "PILOTDECK_WEB_NATIVE";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Default)]
struct Options {
    command: String,
    port: Option<String>,
    database: Option<String>,
    config: Option<String>,
}
fn parse(args: &[String]) -> Options {
    let mut parsed = Options {
        command: "start".into(),
        ..Default::default()
    };
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        let mut next = || {
            index += 1;
            args.get(index).cloned()
        };
        match arg.as_str() {
            "--port" | "-p" => parsed.port = next(),
            "--database-path" => parsed.database = next(),
            "--config" => parsed.config = next(),
            "--help" | "-h" => parsed.command = "help".into(),
            "--version" | "-v" => parsed.command = "version".into(),
            value if value.starts_with("--port=") => {
                parsed.port = value.split('=').nth(1).map(str::to_owned)
            }
            value if value.starts_with("--database-path=") => {
                parsed.database = value.split('=').nth(1).map(str::to_owned)
            }
            value if value.starts_with("--config=") => {
                parsed.config = value.split('=').nth(1).map(str::to_owned)
            }
            value if !value.starts_with('-') => parsed.command = value.into(),
            _ => {}
        }
        index += 1;
    }
    parsed
}
fn nonempty(value: Option<&String>) -> Option<&str> {
    value.filter(|value| !value.is_empty()).map(String::as_str)
}
fn applied(
    options: &Options,
    mut env: HashMap<String, String>,
) -> Result<HashMap<String, String>, ProxyError> {
    if let Some(port) = nonempty(options.port.as_ref()) {
        env.insert("SERVER_PORT".into(), port.into());
    } else if nonempty(env.get("SERVER_PORT")).is_none()
        && let Some(port) = nonempty(env.get("PORT"))
    {
        env.insert("SERVER_PORT".into(), port.into());
    }
    if let Some(path) = nonempty(options.database.as_ref()) {
        env.insert("DATABASE_PATH".into(), path.into());
    }
    if let Some(path) = nonempty(options.config.as_ref()) {
        env.insert("PILOTDECK_CONFIG_PATH".into(), path.into());
    }
    if nonempty(env.get("DATABASE_PATH")).is_none() {
        env.insert(
            "DATABASE_PATH".into(),
            pilot_home(&env)?
                .join("auth.db")
                .to_string_lossy()
                .into_owned(),
        );
    }
    Ok(env)
}
fn pilot_home(env: &HashMap<String, String>) -> Result<PathBuf, ProxyError> {
    if let Some(home) = nonempty(env.get("PILOT_HOME")) {
        return Ok(home.into());
    }
    Ok(PathBuf::from(
        env.get("HOME")
            .ok_or("HOME is required when PILOT_HOME is unset")?,
    )
    .join(".pilotdeck"))
}
fn config_path(env: &HashMap<String, String>) -> Result<PathBuf, ProxyError> {
    Ok(env
        .get("PILOTDECK_CONFIG_PATH")
        .map(|path| pilotdeck_protocol::js_value::js_trim(path))
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .unwrap_or(pilot_home(env)?.join("pilotdeck.yaml")))
}
fn field<'a>(value: &'a JsValue, key: &str) -> Option<&'a JsValue> {
    value.get(key)
}
fn truthy(value: Option<&JsValue>) -> bool {
    match value {
        None | Some(JsValue::Undefined | JsValue::Null) => false,
        Some(JsValue::Bool(value)) => *value,
        Some(JsValue::Number(value)) => *value != 0.0 && !value.is_nan(),
        Some(JsValue::String(value)) => !value.is_empty(),
        _ => true,
    }
}
fn has_usable_config(path: &Path) -> Result<bool, ProxyError> {
    let raw = match std::fs::read(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    let document = pilotdeck_yaml::parse_document(
        &JsString::from(String::from_utf8_lossy(&raw).as_ref()),
        Default::default(),
    );
    if !document.errors.is_empty() {
        return Ok(false);
    }
    let Ok(config) = document.to_json().and_then(|graph| graph.to_js_value()) else {
        return Ok(false);
    };
    // Keep the fixed baseline's legacy onboarding gate, including its behavior
    // for modern agent/model configurations (cli.js:126-134).
    let main = field(&config, "agents")
        .and_then(|value| field(value, "main"))
        .and_then(|value| field(value, "model"));
    let key = main
        .and_then(JsValue::as_string)
        .and_then(|value| value.to_utf8().ok());
    let models = field(&config, "models");
    let entry = key.as_ref().and_then(|key| {
        models
            .and_then(|value| field(value, "entries"))
            .and_then(|value| field(value, key))
    });
    let provider_id = entry
        .and_then(|value| field(value, "provider"))
        .and_then(JsValue::as_string)
        .and_then(|value| value.to_utf8().ok());
    let provider = provider_id.as_ref().and_then(|key| {
        models
            .and_then(|value| field(value, "providers"))
            .and_then(|value| field(value, key))
    });
    Ok(truthy(main)
        && truthy(entry.and_then(|value| field(value, "name")))
        && truthy(provider.and_then(|value| field(value, "baseUrl")))
        && truthy(provider.and_then(|value| field(value, "apiKey")))
        && ui_config_valid(&config))
}

// UI validation permits an empty modern configuration. The native executable
// loader requires a selected model, so status cannot use it as that predicate.
fn ui_config_valid(config: &JsValue) -> bool {
    let text = |value: Option<&JsValue>| {
        value
            .and_then(JsValue::as_string)
            .map(|value| String::from_utf16_lossy(value.units()))
            .map(|value| pilotdeck_protocol::js_value::js_trim(&value).to_owned())
            .unwrap_or_default()
    };
    let providers = config
        .get("model")
        .and_then(|model| model.get("providers"))
        .and_then(JsValue::as_object);
    if let Some(providers) = providers {
        for (id, provider) in providers.entries() {
            let id = String::from_utf16_lossy(id.units());
            if provider.as_object().is_none() {
                return false;
            }
            let protocol = text(provider.get("protocol")).to_lowercase();
            if !["openai", "openai-responses", "anthropic", "google"].contains(&protocol.as_str()) {
                return false;
            }
            let url = text(provider.get("url"));
            if !url.is_empty()
                && !url::Url::parse(&url).is_ok_and(|url| ["http", "https"].contains(&url.scheme()))
            {
                return false;
            }
            if url.is_empty()
                && ![
                    "anthropic",
                    "openai",
                    "openai-responses",
                    "dashscope",
                    "deepseek",
                    "google",
                    "moonshot",
                    "minimax",
                    "volc_ark",
                    "zhipu",
                    "openrouter",
                    "ollama",
                ]
                .contains(&id.as_str())
            {
                return false;
            }
            if id != "ollama" && text(provider.get("apiKey")).is_empty() {
                return false;
            }
            let Some(models) = provider.get("models").and_then(JsValue::as_object) else {
                return false;
            };
            if models.entries().iter().any(|(id, model)| {
                pilotdeck_protocol::js_value::js_trim(&String::from_utf16_lossy(id.units()))
                    .is_empty()
                    || !matches!(
                        model,
                        JsValue::Null | JsValue::Undefined | JsValue::Object(_)
                    )
                    || !ui_thinking_valid(model.get("thinking"), &protocol)
            }) {
                return false;
            }
        }
    }
    let main = text(config.get("agent").and_then(|agent| agent.get("model")));
    let resolves = |reference: &str| {
        let reference = if reference.is_empty() || reference == "inherit" {
            &main
        } else {
            reference
        };
        let Some((provider, model)) = reference.split_once('/') else {
            return false;
        };
        if provider.is_empty() || model.is_empty() {
            return false;
        }
        providers
            .and_then(|providers| providers.get(provider))
            .and_then(|provider| provider.get("models"))
            .and_then(|models| models.get(model))
            .is_some()
    };
    if !main.is_empty() && !resolves(&main) {
        return false;
    }
    let memory = config.get("memory");
    let memory_model = text(memory.and_then(|memory| memory.get("model")));
    if memory.is_some_and(|memory| {
        memory.as_object().is_some() && memory.get("enabled") != Some(&JsValue::Bool(false))
    }) && !memory_model.is_empty()
        && memory_model != "inherit"
        && !resolves(&memory_model)
    {
        return false;
    }
    let preview = config
        .get("webui")
        .and_then(|webui| webui.get("officePreview"));
    let service = text(preview.and_then(|preview| preview.get("service"))).to_lowercase();
    let legacy_preview =
        !text(preview.and_then(|preview| preview.get("spreadsheetMode"))).is_empty();
    if !legacy_preview
        && !service.is_empty()
        && service != "none"
        && !["builtin", "libreoffice"].contains(&service.as_str())
    {
        return false;
    }
    if preview
        .and_then(|preview| preview.get("binaryPath"))
        .is_some_and(|value| value.as_string().is_none())
    {
        return false;
    }
    crate::config::web_sections_valid(config) && ui_router_valid(config.get("router"), &resolves)
}
fn ui_thinking_valid(raw: Option<&JsValue>, protocol: &str) -> bool {
    let Some(raw) = raw else {
        return true;
    };
    if raw.as_object().is_none() {
        return false;
    }
    let enum_valid = |value: Option<&JsValue>, allowed: &[&str]| {
        value.is_none_or(|value| {
            value
                .as_string()
                .is_some_and(|value| allowed.iter().any(|name| value.equals_str(name)))
        })
    };
    if !enum_valid(raw.get("state"), &["default", "enabled", "disabled"]) {
        return false;
    }
    if let Some(efforts) = raw.get("efforts") {
        let JsValue::Array(efforts) = efforts else {
            return false;
        };
        if efforts
            .iter()
            .any(|effort| !enum_valid(Some(effort), &["low", "medium", "high", "xhigh", "max"]))
        {
            return false;
        }
    }
    let allowed: &[&str] = match protocol {
        "anthropic" => &["provider", "anthropic", "server-default"],
        "google" => &["provider", "google", "server-default"],
        "openai-responses" => &["provider", "openai", "server-default"],
        _ => &[
            "provider",
            "openai",
            "thinking-type",
            "qwen-cloud",
            "qwen-local",
            "openrouter",
            "server-default",
        ],
    };
    enum_valid(raw.get("format"), allowed)
}
fn ui_router_valid(raw: Option<&JsValue>, resolves: &impl Fn(&str) -> bool) -> bool {
    let Some(raw) = raw.filter(|raw| raw.as_object().is_some()) else {
        return true;
    };
    if raw.get("enabled") == Some(&JsValue::Bool(false)) {
        return true;
    }
    let text = |value: Option<&JsValue>| {
        value
            .and_then(JsValue::as_string)
            .map(|value| String::from_utf16_lossy(value.units()))
            .map(|value| pilotdeck_protocol::js_value::js_trim(&value).to_owned())
            .unwrap_or_default()
    };
    let string = |value: Option<&JsValue>| value.is_none_or(|value| value.as_string().is_some());
    let boolean = |value: Option<&JsValue>| value.is_none_or(|value| value.as_bool().is_some());
    let optional_ref = |value: &JsValue| {
        let value = text(Some(value));
        value.is_empty() || resolves(&value)
    };
    let required_ref = |value: Option<&JsValue>| {
        let value = text(value);
        value
            .split_once('/')
            .is_some_and(|(provider, model)| !provider.is_empty() && !model.is_empty())
            && resolves(&value)
    };
    if !boolean(raw.get("enabled")) {
        return false;
    }
    if let Some(scenarios) = raw.get("scenarios").and_then(JsValue::as_object)
        && scenarios
            .entries()
            .iter()
            .any(|(_, value)| !required_ref(Some(value)))
    {
        return false;
    }
    if let Some(fallbacks) = raw.get("fallback").and_then(JsValue::as_object) {
        for (_, value) in fallbacks.entries() {
            if let JsValue::Array(values) = value
                && values.iter().any(|value| !optional_ref(value))
            {
                return false;
            }
        }
    }
    if let Some(stats) = raw.get("stats") {
        if stats.as_object().is_none() || !boolean(stats.get("enabled")) {
            return false;
        }
        if let Some(baseline) = stats.get("baselineModel") {
            let valid = if baseline.as_string().is_some() {
                required_ref(Some(baseline))
            } else if baseline.as_object().is_some() {
                let provider = text(baseline.get("provider"));
                let model = text(baseline.get("model"));
                !provider.is_empty()
                    && !model.is_empty()
                    && resolves(&format!("{provider}/{model}"))
            } else {
                false
            };
            if !valid {
                return false;
            }
        }
        if let Some(pricing) = stats.get("modelPricing") {
            let Some(pricing) = pricing.as_object() else {
                return false;
            };
            for (key, value) in pricing.entries() {
                let key = String::from_utf16_lossy(key.units());
                if !required_ref(Some(&JsValue::from(key.as_str()))) || value.as_object().is_none()
                {
                    return false;
                }
                for field in ["input", "output", "cacheRead"] {
                    if value.get(field).is_some_and(|value| {
                        value
                            .as_number()
                            .is_none_or(|number| !number.is_finite() || number < 0.0)
                    }) {
                        return false;
                    }
                }
                if value.get("unit").is_some_and(|value| {
                    !value.as_string().is_some_and(|value| {
                        value.equals_str("$/百万 Token") || value.equals_str("¥/百万 Token")
                    })
                }) {
                    return false;
                }
            }
        }
    }
    if let Some(token) = raw
        .get("tokenSaver")
        .filter(|value| value.as_object().is_some())
    {
        if !boolean(token.get("enabled")) {
            return false;
        }
        if token.get("enabled") == Some(&JsValue::Bool(false)) {
            return true;
        }
        if !required_ref(token.get("judge")) || !string(token.get("defaultTier")) {
            return false;
        }
        if let Some(tiers) = token.get("tiers") {
            let Some(tiers) = tiers.as_object() else {
                return false;
            };
            if tiers.entries().is_empty() {
                return false;
            }
            for (_, tier) in tiers.entries() {
                if tier.as_object().is_none()
                    || !required_ref(tier.get("model"))
                    || !string(tier.get("label"))
                    || !string(tier.get("description"))
                {
                    return false;
                }
            }
            if let Some(default) = token
                .get("defaultTier")
                .and_then(JsValue::as_string)
                .and_then(|value| value.to_utf8().ok())
                && !tiers.contains_key(&default)
            {
                return false;
            }
        }
        if let Some(subagent) = token.get("subagent") {
            if subagent.as_object().is_none()
                || !subagent
                    .get("policy")
                    .and_then(JsValue::as_string)
                    .is_some_and(|value| value.equals_str("skip") || value.equals_str("judge"))
            {
                return false;
            }
        }
    }
    true
}
fn status(env: &HashMap<String, String>) -> Result<String, ProxyError> {
    let config = config_path(env)?;
    let database = PathBuf::from(env.get("DATABASE_PATH").ok_or("DATABASE_PATH is missing")?);
    let port = nonempty(env.get("SERVER_PORT")).unwrap_or("3001");
    let install = std::env::current_exe()?
        .parent()
        .ok_or("executable directory is missing")?
        .to_path_buf();
    Ok(format!(
        "\npilotdeck - Status\n\n{}\n\n[INFO] Version: {VERSION}\n[INFO] Installation Directory: {}\n[INFO] Server Port: {port}\n[INFO] Config File: {}\n       Status: {}\n       Onboarding: {}\n[INFO] Database: {}\n       Status: {}\n\n{}\n\n[TIP] Start with pilotdeck and open http://localhost:{port}\n\n",
        "═".repeat(60),
        install.display(),
        config.display(),
        if config.exists() {
            "[OK] Exists"
        } else {
            "[WARN] Not found"
        },
        if has_usable_config(&config)? {
            "[OK] Complete"
        } else {
            "[WARN] Required"
        },
        database.display(),
        if database.exists() {
            "[OK] Exists"
        } else {
            "[WARN] Not created yet"
        },
        "═".repeat(60)
    ))
}

const HELP: &str = "\npilotdeck - Command Line Tool\n\nUsage:\n  pilotdeck [command] [options]\n\nCommands:\n  start          Start the PilotDeck web UI (default)\n  status         Show configuration and data locations\n  help           Show this help information\n  version        Show version information\n\nOptions:\n  -p, --port <port>             Set server port (default: 3001)\n  --database-path <path>        Set database location\n  --config <path>               Set pilotdeck.yaml location\n  -h, --help                    Show this help information\n  -v, --version                 Show version information\n\nExamples:\n  pilotdeck\n  pilotdeck --port 8080\n  pilotdeck status\n\nConfiguration:\n  PilotDeck reads ~/.pilotdeck/pilotdeck.yaml by default.\n  First run opens the onboarding UI if no usable config exists.\n\n";

fn native_serve(env: &HashMap<String, String>) -> Result<i32, ProxyError> {
    let host = nonempty(env.get("HOST")).unwrap_or("0.0.0.0");
    let port = nonempty(env.get("SERVER_PORT")).unwrap_or("3001");
    let number = pilotdeck_protocol::js_value::string_to_number(port);
    if !number.is_finite() || number < 0.0 || number > 65535.0 || number.fract() != 0.0 {
        return Err(format!("Invalid server port: {port}").into());
    }
    let address = std::net::ToSocketAddrs::to_socket_addrs(&(host, number as u16))?
        .next()
        .ok_or("HOST did not resolve to an address")?;
    let probe = std::net::TcpListener::bind(address).map_err(|error| {
        if error.kind() == std::io::ErrorKind::AddrInUse {
            std::io::Error::new(
                error.kind(),
                format!(
                    "Port {port} is already in use. Try: pilotdeck --port {}",
                    number + 1.0
                ),
            )
        } else {
            error
        }
    })?;
    drop(probe);
    println!(
        "\npilotdeck starting...\n\n[INFO] Config: {}\n[INFO] Database: {}\n[INFO] Server: http://localhost:{port}\n",
        config_path(env)?.display(),
        env.get("DATABASE_PATH").ok_or("DATABASE_PATH is missing")?
    );
    std::io::stdout().flush()?;
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(crate::application::serve_web(SocketAddr::new(
            address.ip(),
            number as u16,
        )))?;
    Ok(0)
}
fn run(args: Vec<String>) -> Result<i32, ProxyError> {
    let env: HashMap<String, String> = std::env::vars().collect();
    if args == [NATIVE_ARGUMENT] && env.get(NATIVE_ENV).is_some_and(|value| value == "1") {
        return native_serve(&env);
    }
    let options = parse(&args);
    let env = applied(&options, env)?;
    match options.command.as_str() {
        "status" | "info" => {
            print!("{}", status(&env)?);
            Ok(0)
        }
        "help" => {
            print!("{HELP}");
            Ok(0)
        }
        "version" => {
            println!("{VERSION}");
            Ok(0)
        }
        "start" => {
            let mut command = Command::new(std::env::current_exe()?);
            command
                .arg(NATIVE_ARGUMENT)
                .envs(&env)
                .env(NATIVE_ENV, "1")
                .stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit());
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                Err(command.exec().into())
            }
            #[cfg(not(unix))]
            {
                Ok(command.status()?.code().unwrap_or(1))
            }
        }
        command => {
            eprintln!(
                "[ERROR] Unknown command: {command}\nRun pilotdeck help for usage information."
            );
            Ok(1)
        }
    }
}
/// Process entry point. Options are applied by safe process replacement before
/// Tokio starts; this never mutates the process environment or invokes Node.
pub fn main_entry(args: Vec<String>) -> i32 {
    match run(args) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("[ERROR] {error}");
            1
        }
    }
}
