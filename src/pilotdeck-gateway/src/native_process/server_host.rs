//! Both isolated native serving and the legacy CLI own this same lifecycle.
use super::{LocalHostOptions, Options, assembly, switch_wiring};
pub use crate::native_http::control::{ControlHandler, ControlReply, ControlRequest};
use crate::{ProxyError, config::Env};
use std::{future::Future, net::SocketAddr, path::PathBuf, pin::Pin, sync::Arc, time::Duration};
pub type ReadyCallback = Arc<
    dyn Fn(SocketAddr) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>> + Send + Sync,
>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostExit {
    Stopped,
    Restart,
}

/// The caller supplies captured environment, token provenance, and optional
/// native installation management. No JavaScript host or UI API is started.
pub struct ServerOptions {
    pub core: LocalHostOptions,
    pub bind_host: String,
    pub port: u16,
    pub allow_remote: bool,
    pub explicit_token: bool,
    pub token: String,
    pub static_assets: Option<PathBuf>,
    pub control: Option<ControlHandler>,
    pub restart: Option<tokio::sync::watch::Receiver<bool>>,
    pub ready: ReadyCallback,
}

/// Preserve GatewayServer's bind-host policy. A resolved IPv6 localhost is
/// allowed, while an explicitly configured IPv6 literal still needs the
/// explicit remote-host opt-in and token, as in the legacy CLI.
pub fn legacy_local_bind_host(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "localhost")
}

pub async fn run_server(options: ServerOptions, env: Env) -> Result<HostExit, ProxyError> {
    let loopback = legacy_local_bind_host(&options.bind_host);
    if !loopback {
        if !options.allow_remote {
            return Err("GatewayServer remote binding requires allowRemoteHost=true.".into());
        }
        if !options.explicit_token || crate::metadata::js_trim(&options.token).is_empty() {
            return Err("GatewayServer remote binding requires an explicit auth token.".into());
        }
    }
    let address = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::net::lookup_host((options.bind_host.as_str(), options.port)),
    )
    .await??
    .next()
    .ok_or("GatewayServer host did not resolve to an address.")?;
    validate_loopback_resolution(loopback, address)?;
    let mut core = options.core.into_assembly_options()?;
    core.listen = address;
    run_embedding(
        core,
        env,
        Embedding {
            token: options.token,
            loopback: Some(loopback),
            static_assets: options.static_assets,
            control: options.control,
            restart: options.restart,
            ready: options.ready,
            native_diagnostics: false,
        },
    )
    .await
}

pub(super) struct Embedding {
    pub token: String,
    pub loopback: Option<bool>,
    pub static_assets: Option<PathBuf>,
    pub control: Option<ControlHandler>,
    pub restart: Option<tokio::sync::watch::Receiver<bool>>,
    pub ready: ReadyCallback,
    pub native_diagnostics: bool,
}
async fn restart_requested(receiver: Option<tokio::sync::watch::Receiver<bool>>) {
    if let Some(mut receiver) = receiver {
        loop {
            if *receiver.borrow_and_update() {
                return;
            }
            if receiver.changed().await.is_err() {
                break;
            }
        }
    }
    std::future::pending::<()>().await;
}
pub(super) async fn run_embedding(
    options: Options,
    env: Env,
    embedding: Embedding,
) -> Result<HostExit, ProxyError> {
    let (host, feishu_webhook, switch_handles) =
        assembly::start_with_webhook(options.clone(), env).await?;
    let api_web = super::production_api_web(embedding.token.clone(), options.home.clone());
    let approvals = crate::module_host::approvals::Config {
        token: embedding.token.clone(),
        bridge: switch_handles.approvals.clone(),
    };
    let public_host = crate::module_host::Config {
        token: embedding.token.clone(),
        capabilities: switch_handles.public_host.clone(),
    };
    let server = match super::production_listener(
        options.listen,
        host.clone(),
        embedding.token,
        feishu_webhook,
        api_web,
        embedding.loopback,
    )
    .await
    {
        // #1329: `ANY /feishu/webhook` on the same listener (GatewayServer.ts
        // registers it on the HTTP server, after `/auth/local-token`).
        Ok(mut server) => {
            server = server
                .with_approvals(approvals)
                .with_public_host(public_host);
            if let Some(root) = embedding.static_assets {
                server = server.with_static_root(root);
            }
            if let Some(handler) = embedding.control {
                server = server.with_control(handler);
            }
            server
        }
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
    // #1353 A8.5-pr2: the signal starts the switch machine instead of the
    // raw WS drain — the matrix §二 sequence (stop automation → stop
    // accepting + drain → flush → stop owner → recovery) owns the order,
    // and a failing or hung step halts the switch before the next one runs
    // (A2/A3). The machine's entry and owner bounds are the `--drain-ms`
    // budget plus slack (a backstop for a hung face, not a second copy of
    // the faces' own internal deadlines).
    //
    // The phased run, the faces, and the machine are constructed *before*
    // the signal wait: the accept loop runs from the run's construction, so
    // steady state is served exactly like the awaited `run()` future was —
    // the signal only triggers the switch (`native_ws.rs::run_phased`).
    let (trigger, trigger_rx) = tokio::sync::watch::channel(false);
    let phased = Arc::new(server.run_phased(
        switch_wiring::trigger_stop(trigger_rx),
        options.drain_budget,
    ));
    let faces = switch_wiring::ProductionFaces::new(
        switch_handles.cron,
        switch_wiring::AdmissionGate::new(switch_handles.channels, trigger),
        phased,
    )
    .with_automation_stop(switch_handles.automation);
    let machine = crate::switch::SwitchMachine::new(
        faces.clone(),
        crate::switch::SwitchTimeouts::production(options.drain_budget),
    );
    if let Err(error) = (embedding.ready)(address).await {
        return match machine.run().await {
            Ok(()) => Err(crate::ProxyError::from(error)),
            Err(cleanup) => {
                let final_cleanup = faces.finish_after_halt().await;
                let detail = match final_cleanup {
                    Ok(()) => format!("{error}; teardown: {cleanup}"),
                    Err(last) => format!("{error}; teardown: {cleanup}; final cleanup: {last}"),
                };
                Err(crate::ProxyError::from(detail))
            }
        };
    }
    #[cfg(unix)]
    let restart = tokio::select! { _ = terminate.recv() => false, _ = interrupt.recv() => false, _ = restart_requested(embedding.restart) => true };
    #[cfg(not(unix))]
    let restart = tokio::select! { _ = tokio::signal::ctrl_c() => false, _ = restart_requested(embedding.restart) => true };
    if embedding.native_diagnostics {
        eprintln!("native stopping");
    }
    match machine.run().await {
        Ok(()) => Ok(if restart {
            HostExit::Restart
        } else {
            HostExit::Stopped
        }),
        Err(error) => {
            // The machine's halt is the reported failure and the exit status:
            // the death march below still terminates the process (a
            // registered difference — the matrix's surviving-owner semantics
            // cannot outlive SIGTERM), but a halt never exits 0 (D1).
            eprintln!("{}: {error}", super::NATIVE_SWITCH_HALTED);
            faces.finish_after_halt().await.map_err(|teardown| {
                crate::ProxyError::from(format!("{error}; teardown: {teardown}"))
            })?;
            Err(crate::ProxyError::from(error.to_string()))
        }
    }
}

// A hostname's spelling cannot authorize token discovery on its resolved
// network interface. Keep the legacy localhost/v4 policy only after checking
// the actual address; a poisoned resolver fails before host side effects.
fn validate_loopback_resolution(loopback: bool, address: SocketAddr) -> Result<(), ProxyError> {
    if loopback && !address.ip().is_loopback() {
        return Err("GatewayServer loopback hostname resolved to a non-loopback address.".into());
    }
    Ok(())
}
#[cfg(test)]
mod resolution_tests {
    use super::*;
    #[test]
    fn local_token_policy_is_checked_against_the_resolved_interface() {
        for address in ["127.0.0.1:0", "127.0.0.2:0", "[::1]:0"] {
            assert!(validate_loopback_resolution(true, address.parse().unwrap()).is_ok());
        }
        for address in ["0.0.0.0:0", "192.0.2.1:0", "[::]:0", "[2001:db8::1]:0"] {
            assert!(validate_loopback_resolution(true, address.parse().unwrap()).is_err());
            assert!(validate_loopback_resolution(false, address.parse().unwrap()).is_ok());
        }
    }
    #[test]
    fn legacy_bind_policy_distinguishes_hostname_from_resolved_address() {
        for host in ["localhost", "127.0.0.1"] {
            assert!(legacy_local_bind_host(host));
        }
        for host in ["::1", "[::1]", "127.0.0.2", "LOCALHOST", "0.0.0.0"] {
            assert!(!legacy_local_bind_host(host));
        }
        assert!(crate::metadata::js_trim("\u{feff} \n").is_empty());
        assert!(!crate::metadata::js_trim("\u{0085}").is_empty());
    }
    #[tokio::test]
    async fn direct_server_rejects_blank_remote_token_before_any_host_side_effect() {
        for token in ["", " ", "\u{feff}", "\u{feff}\n"] {
            let options = ServerOptions {
                core: LocalHostOptions {
                    home: PathBuf::new(),
                    project: PathBuf::new(),
                    sidecar: PathBuf::new(),
                    sidecar_args: vec![],
                    drain_budget: Duration::from_millis(1),
                },
                bind_host: "0.0.0.0".into(),
                port: 0,
                allow_remote: true,
                explicit_token: true,
                token: token.into(),
                static_assets: None,
                control: None,
                restart: None,
                ready: Arc::new(|_| {
                    Box::pin(async { panic!("invalid token must not start a host") })
                }),
            };
            assert_eq!(
                run_server(options, Env::new())
                    .await
                    .unwrap_err()
                    .to_string(),
                "GatewayServer remote binding requires an explicit auth token."
            );
        }
    }
}
