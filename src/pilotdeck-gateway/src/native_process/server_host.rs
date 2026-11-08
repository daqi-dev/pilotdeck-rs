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

pub async fn run_server(options: ServerOptions, env: Env) -> Result<HostExit, ProxyError> {
    let loopback = matches!(options.bind_host.as_str(), "127.0.0.1" | "localhost");
    if !loopback {
        if !options.allow_remote {
            return Err("GatewayServer remote binding requires allowRemoteHost=true.".into());
        }
        if !options.explicit_token || options.token.trim().is_empty() {
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
    let server = match crate::native_ws::NativeWsServer::bind_with_policy(
        options.listen,
        host.clone(),
        embedding.token,
        env!("CARGO_PKG_VERSION").into(),
        embedding.loopback,
    )
    .await
    {
        // #1329: `ANY /feishu/webhook` on the same listener (GatewayServer.ts
        // registers it on the HTTP server, after `/auth/local-token`).
        Ok(mut server) => {
            server = server.with_webhook(feishu_webhook);
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
    );
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
            eprintln!("native switch halted: {error}");
            faces.finish_after_halt().await.map_err(|teardown| {
                crate::ProxyError::from(format!("{error}; teardown: {teardown}"))
            })?;
            Err(crate::ProxyError::from(error.to_string()))
        }
    }
}
