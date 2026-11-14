//! A4 gateway: verified native metadata plus parsed, byte-preserving passthrough.
use hyper_util::{client::legacy::Client, rt::TokioExecutor};
use pilotdeck_gateway::{ProxyError, http_proxy::Proxy};
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::net::TcpListener;
#[tokio::main]
async fn main() -> Result<(), ProxyError> {
    // Node's process-wide SIGXFSZ ignore (a write past RLIMIT_FSIZE reports
    // EFBIG instead of killing the process); register the same disposition
    // at startup through tokio's safe signal API so every fs write in this
    // process matches Node (#621). The registration is held for the
    // process lifetime (a static OnceLock in the library).
    #[cfg(unix)]
    pilotdeck_gateway::ensure_sigxfsz_ignored();
    let mut args = std::env::args_os().skip(1);
    let first = args.next();
    let explicit_native = pilotdeck_gateway::native_process::native_requested(first.as_deref());
    // #1354 A8.5-pr3: the main entry's routing. The explicit `--native`
    // argument (#1071) wins over the `PILOTDECK_GATEWAY_MODE` config item —
    // the per-invocation flag outranks the deployment default, like the TS
    // baseline's `readStringFlag(argv, "--host") ?? env.PILOTDECK_GATEWAY_HOST`
    // (src/cli/pilotdeck.ts:213). The config item is still judged *before*
    // the flag short-circuits anything, so an invalid value fails closed at
    // startup even when `--native` is present (#1369 shape). The config
    // item's default is proxy mode (#1303 option A), so without either
    // explicit switch the entry below is the unchanged A4 proxy: the
    // rollback target stage A keeps.
    let mode_native = pilotdeck_gateway::native_process::gateway_mode(
        std::env::var_os("PILOTDECK_GATEWAY_MODE").as_deref(),
    )?;
    if explicit_native || mode_native {
        return pilotdeck_gateway::native_process::run(
            pilotdeck_gateway::native_process::Options::parse(native_arguments(
                explicit_native,
                first,
                args,
            ))?,
        )
        .await;
    }
    let listen: SocketAddr = std::env::var("PILOTDECK_PROXY_LISTEN")
        .unwrap_or_else(|_| "127.0.0.1:28789".into())
        .parse()?;
    if listen.ip() != std::net::Ipv4Addr::LOCALHOST {
        return Err("proxy must listen on 127.0.0.1".into());
    }
    let upstream = std::env::var("PILOTDECK_PROXY_UPSTREAM")
        .unwrap_or_else(|_| "http://127.0.0.1:28790".into());
    let uri: hyper::Uri = upstream.parse()?;
    if uri.scheme_str() != Some("http") || uri.authority().is_none() || uri.path() != "/" {
        return Err("upstream must be an http origin (no path)".into());
    }
    let proxy = Arc::new(Proxy {
        upstream: upstream.trim_end_matches('/').to_owned(),
        client: Client::builder(TokioExecutor::new()).build_http(),
    });
    let listener = TcpListener::bind(listen).await?;
    let address = listener.local_addr()?;
    // #2558: best-effort console lines, like TS's `console.log` — a raw
    // `println!` here panics when the write fails (std behavior), and these
    // banners run on the startup path inside the main task: a closed stdout
    // would kill the listener right after the bind, before the accept loop
    // answers a connection. The stdout twin of the metadata line below (the
    // stderr half of the same fix, `refresh.rs`'s `log` default shape).
    pilotdeck_gateway::native_process::console_log_line(
        &mut std::io::stdout(),
        &format!("PilotDeck server listening: http://{address}"),
    );
    pilotdeck_gateway::native_process::console_log_line(
        &mut std::io::stdout(),
        &format!("WebSocket: ws://{address}/ws"),
    );
    // #2558: a best-effort console line, like TS's `console.error` — an
    // `eprintln!` here panics when the write fails (std behavior), and this
    // line runs on the startup path inside the main task: an unwritable
    // stderr would kill the listener right after it printed its banner. The
    // `current_dir()` error still propagates; only the write is best-effort.
    pilotdeck_gateway::native_process::console_log_line(
        &mut std::io::stderr(),
        &format!("metadata cwd: {}", std::env::current_dir()?.display()),
    );
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (socket, _) = match accepted {
                    Ok(connection) => connection,
                    Err(error) => {
                        // #2558: a best-effort console line, like TS's
                        // `console.error` — an `eprintln!` here panics when
                        // the write fails (std behavior), and the panic runs
                        // in the accept-loop task itself: an unwritable
                        // stderr would stop the listener instead of merely
                        // losing the diagnostic line.
                        pilotdeck_gateway::native_process::console_log_line(
                            &mut std::io::stderr(),
                            &format!("downstream accept: {error}"),
                        );
                        if fatal_accept_error(&error) {
                            return Err(error.into());
                        }
                        // EMFILE and transient network errors must not stop the listener
                        // or spin at full CPU while resources remain exhausted.
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                };
                let proxy = proxy.clone();
                tokio::spawn(async move {
                    if let Err(error) = proxy.serve_connection(socket).await {
                        // #2558: same non-panicking shape as the accept line
                        // above — a failed stderr write must not kill the
                        // connection task that only owes a diagnostic line.
                        pilotdeck_gateway::native_process::console_log_line(
                            &mut std::io::stderr(),
                            &format!("downstream HTTP: {error}"),
                        );
                    }
                });
            }
            _ = tokio::signal::ctrl_c() => break,
        }
    }
    Ok(())
}

fn fatal_accept_error(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::InvalidInput
            | std::io::ErrorKind::PermissionDenied
            | std::io::ErrorKind::Unsupported
    )
}

/// #1354: `--native` is consumed by the router itself, so only the remaining
/// arguments reach the native options parser; the config item's routing
/// leaves every argument — `first` included — to that parser.
fn native_arguments(
    explicit: bool,
    first: Option<std::ffi::OsString>,
    rest: impl Iterator<Item = std::ffi::OsString>,
) -> impl Iterator<Item = std::ffi::OsString> {
    if explicit { None } else { first }.into_iter().chain(rest)
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
