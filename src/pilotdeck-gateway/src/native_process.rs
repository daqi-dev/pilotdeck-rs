//! Explicit isolated process configuration and production gateway composition.
//! The A4 entry never reads these options unless its first argument is `--native`.
use crate::{ProxyError, config::Env, host::modules::tool_context::TurnEnvironment};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{
    net::{Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    time::Duration,
};
mod assembly;
mod families;
mod management;
mod methods;
pub use methods::registered_methods;
#[cfg(test)]
mod management_tests;
mod requests;
mod services;
mod tools;

/// Start the real serial host, bind WS, then drain on either supported signal.
/// The listener never starts an upstream gateway/helper.
pub async fn run(options: Options) -> Result<(), ProxyError> {
    let token = std::fs::read_to_string(&options.token_file)?;
    let token = token.trim_end_matches(['\r', '\n']).to_owned();
    if token.trim().is_empty() {
        return Err("native token file must contain a nonempty token".into());
    }
    let inherited = capture_environment(std::env::vars_os());
    validate_environment(&inherited)?;
    let env = options.environment(inherited);
    let host = assembly::start(options.clone(), env).await?;
    let server = match crate::native_ws::NativeWsServer::bind(
        options.listen,
        host.clone(),
        token,
        env!("CARGO_PKG_VERSION").into(),
    )
    .await
    {
        Ok(server) => server,
        Err(error) => {
            let _ = host.shutdown(options.drain_budget).await;
            return Err(error);
        }
    };
    #[cfg(unix)]
    let signals = (|| -> std::io::Result<_> {
        Ok((
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?,
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?,
        ))
    })();
    #[cfg(unix)]
    let (mut terminate, mut interrupt) = match signals {
        Ok(signals) => signals,
        Err(error) => {
            let _ = host.shutdown(options.drain_budget).await;
            return Err(error.into());
        }
    };
    let address = match server.local_addr() {
        Ok(address) => address,
        Err(error) => {
            let _ = host.shutdown(options.drain_budget).await;
            return Err(error.into());
        }
    };
    eprintln!("native listening {address}");
    let stop = async move {
        #[cfg(unix)]
        tokio::select! { _ = terminate.recv() => {}, _ = interrupt.recv() => {} }
        #[cfg(not(unix))]
        let _ = tokio::signal::ctrl_c().await;
        eprintln!("native stopping");
    };
    server.run(stop, options.drain_budget).await
}

fn capture_environment(
    vars: impl IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
) -> Env {
    vars.into_iter()
        .map(|(key, value)| {
            (
                key.to_string_lossy().into_owned(),
                value.to_string_lossy().into_owned(),
            )
        })
        .collect()
}

/// All authorities are explicit; native mode has no default home, project,
/// token, upstream, or sidecar command. Port zero is supported for isolation.
#[derive(Clone, Debug)]
pub struct Options {
    pub home: PathBuf,
    pub project: PathBuf,
    pub listen: SocketAddr,
    pub token_file: PathBuf,
    pub sidecar: PathBuf,
    pub sidecar_args: Vec<String>,
    pub drain_budget: Duration,
}

pub fn native_requested(first: Option<&std::ffi::OsStr>) -> bool {
    first == Some(std::ffi::OsStr::new("--native"))
}

impl Options {
    pub fn parse(args: impl IntoIterator<Item = std::ffi::OsString>) -> Result<Self, ProxyError> {
        Self::parse_at(args, Path::new(""))
    }

    fn parse_at(
        args: impl IntoIterator<Item = std::ffi::OsString>,
        cwd: &Path,
    ) -> Result<Self, ProxyError> {
        let mut args = args.into_iter();
        let mut values = std::collections::HashMap::new();
        let mut sidecar_args = Vec::new();
        while let Some(key) = args.next() {
            let key = key
                .into_string()
                .map_err(|_| "native option is not UTF-8")?;
            if ![
                "--home",
                "--project",
                "--listen",
                "--token-file",
                "--sidecar",
                "--sidecar-arg",
                "--drain-ms",
            ]
            .contains(&key.as_str())
            {
                return Err(format!("unknown native option: {key}").into());
            }
            let value = args
                .next()
                .ok_or_else(|| format!("missing value for {key}"))?;
            if key == "--sidecar-arg" {
                sidecar_args.push(
                    value
                        .into_string()
                        .map_err(|_| "sidecar argument is not UTF-8")?,
                );
            } else if values.insert(key.clone(), value).is_some() {
                return Err(format!("duplicate native option: {key}").into());
            }
        }
        let take = |key: &str| {
            values
                .get(key)
                .ok_or_else(|| format!("native mode requires {key}"))
        };
        let home = directory(Path::new(take("--home")?), "home")?;
        // Keep Node path.resolve identity (including symlink spelling). Only
        // the containment check uses the physical path, preserving isolation.
        let project = crate::metadata::resolve_process_path(cwd, Path::new(take("--project")?))?;
        let physical_project = directory(&project, "project")?;
        if home.starts_with(&physical_project) || physical_project.starts_with(&home) {
            return Err("native home and project must be separate directories".into());
        }
        let listen: SocketAddr = take("--listen")?
            .to_str()
            .ok_or("listen is not UTF-8")?
            .parse()?;
        if listen.ip() != Ipv4Addr::LOCALHOST || (listen.port() != 0 && listen.port() < 1024) {
            return Err("native listen requires 127.0.0.1 and a high port (or 0)".into());
        }
        let token_file = file(Path::new(take("--token-file")?), "token file")?;
        let sidecar = file(Path::new(take("--sidecar")?), "sidecar")?;
        let drain_ms = values
            .get("--drain-ms")
            .map(|s| {
                s.to_str()
                    .ok_or("drain budget is not UTF-8")?
                    .parse::<u64>()
                    .map_err(|_| "invalid drain budget")
            })
            .transpose()?
            .unwrap_or(5000);
        if drain_ms == 0 || drain_ms > 300_000 {
            return Err("native drain budget must be 1..300000 milliseconds".into());
        }
        Ok(Self {
            home,
            project,
            listen,
            token_file,
            sidecar,
            sidecar_args,
            drain_budget: Duration::from_millis(drain_ms),
        })
    }

    fn serves_project(&self, project: &str) -> bool {
        crate::metadata::resolve_process_path(Path::new(""), Path::new(project))
            .is_ok_and(|path| path == self.project)
    }

    /// A captured environment for config/children. The isolated home wins over
    /// inherited HOME/PILOT_HOME and no TS gateway upstream is ever consulted.
    pub fn environment(&self, mut inherited: Env) -> Env {
        inherited.insert("HOME".into(), self.home.to_string_lossy().into_owned());
        inherited.insert(
            "PILOT_HOME".into(),
            self.home.to_string_lossy().into_owned(),
        );
        inherited.remove("PILOTDECK_PROXY_UPSTREAM");
        inherited.remove("PILOTDECK_CONFIG_PATH");
        inherited.remove("PILOTDECK_AGENT_LOOP_TCP_HOST");
        inherited.remove("PILOTDECK_AGENT_LOOP_TCP_PORT");
        inherited
    }
}

fn absolute(path: &Path, label: &str) -> Result<PathBuf, ProxyError> {
    if !path.is_absolute() {
        return Err(format!("native {label} must be an absolute path").into());
    }
    Ok(path.canonicalize()?)
}
fn directory(path: &Path, label: &str) -> Result<PathBuf, ProxyError> {
    let path = absolute(path, label)?;
    if !path.is_dir() {
        return Err(format!("native {label} must be an existing directory").into());
    }
    Ok(path)
}
fn file(path: &Path, label: &str) -> Result<PathBuf, ProxyError> {
    let path = absolute(path, label)?;
    if !path.is_file() {
        return Err(format!("native {label} must be an existing file").into());
    }
    Ok(path)
}

fn validate_environment(env: &Env) -> Result<(), ProxyError> {
    for key in [
        "PILOTDECK_MANAGED_CONFIG",
        "PILOTDECK_STORAGE_ROOT",
        "PILOTDECK_SNAPSHOT_ROOT",
    ] {
        if env.get(key).is_some_and(|v| !v.is_empty()) {
            return Err(format!("native isolated mode does not accept inherited {key}").into());
        }
    }
    Ok(())
}

/// Bind the established host environment policy to the production tool port.
/// Capturing the base avoids reading a mutable process environment per tool.
pub struct NativeTurnEnvironment(pub JsObject);
impl TurnEnvironment for NativeTurnEnvironment {
    fn build(
        &self,
        base: Option<&JsValue>,
        cwd: &JsString,
        session: &JsString,
        turn: &JsString,
    ) -> Result<JsObject, crate::host::HostError> {
        let base = match base {
            None | Some(JsValue::Undefined | JsValue::Null) => &self.0,
            Some(JsValue::Object(base)) => base,
            Some(_) => {
                return Err(crate::host::ModuleError::new(
                    "Turn base environment must be an object.",
                )
                .into());
            }
        };
        crate::host::modules::lifecycle::build_turn_environment(Some(base), cwd, session, turn)
    }
}

#[cfg(test)]
mod tests;
