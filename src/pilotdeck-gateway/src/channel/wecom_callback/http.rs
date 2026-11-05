//! `http.createServer(...).listen(port, "0.0.0.0")` plus Node 22
//! `server.close()` drain semantics: stop accepting, let in-flight requests
//! finish, close idle keep-alive connections.

use std::{
    convert::Infallible,
    sync::{Arc, Mutex},
};

use hyper::{server::conn::http1, service::service_fn};
use hyper_util::rt::TokioIo;
use tokio::{net::TcpListener, sync::watch, task::JoinHandle};

use super::Runtime;

/// A listen failure formatted as the thrown JavaScript value would print:
/// `Error: listen EADDRINUSE: address already in use 0.0.0.0:8780` or the
/// synchronous `RangeError [ERR_SOCKET_BAD_PORT]` of a bad port.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ListenError(String);

impl std::fmt::Display for ListenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl ListenError {
    fn bad_port(port: u32) -> Self {
        Self(format!(
            "RangeError [ERR_SOCKET_BAD_PORT]: options.port should be >= 0 and < 65536. \
             Received type number ({port})."
        ))
    }

    fn from_bind(error: &std::io::Error, port: u16) -> Self {
        let described = match error.kind() {
            std::io::ErrorKind::AddrInUse => "EADDRINUSE: address already in use".to_owned(),
            std::io::ErrorKind::PermissionDenied => "EACCES: permission denied".to_owned(),
            _ => error.to_string(),
        };
        Self(format!("Error: listen {described} 0.0.0.0:{port}"))
    }
}

/// Accepts one request at a time per connection and routes it through the
/// adapter's HTTP face. `stop` is idempotent.
pub(crate) struct Listener {
    port: u16,
    shutdown: watch::Sender<bool>,
    task: Mutex<Option<JoinHandle<()>>>,
}

impl Listener {
    pub(crate) fn port(&self) -> u16 {
        self.port
    }

    pub(crate) async fn stop(&self) {
        let _ = self.shutdown.send(true);
        let task = self.task.lock().expect("#1336 listener task").take();
        if let Some(task) = task {
            let _ = task.await;
        }
    }
}

impl Drop for Listener {
    fn drop(&mut self) {
        // A handle discarded without stop must not leave the accept loop
        // running forever. The detach is deliberate: an already accepted
        // request still completes, exactly like Node's closing server.
        let _ = self.shutdown.send(true);
    }
}

/// Bind on `0.0.0.0:port` and start serving. Resolves only after the socket
/// is listening, matching the `await new Promise(... listen ...)` shape.
pub(crate) async fn listen(runtime: Arc<Runtime>, port: u32) -> Result<Listener, ListenError> {
    let port: u16 = port.try_into().map_err(|_| ListenError::bad_port(port))?;
    let listener = TcpListener::bind((std::net::Ipv4Addr::UNSPECIFIED, port))
        .await
        .map_err(|error| ListenError::from_bind(&error, port))?;
    let bound = listener
        .local_addr()
        .map_err(|error| ListenError::from_bind(&error, port))?
        .port();
    let (shutdown, mut shutdown_rx) = watch::channel(false);
    let task = tokio::spawn(async move {
        let mut connections = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                biased;
                _ = shutdown_rx.changed() => break,
                accepted = listener.accept() => {
                    let Ok((stream, _peer)) = accepted else { continue };
                    let runtime = runtime.clone();
                    let mut shutdown = shutdown_rx.clone();
                    connections.spawn(async move {
                        let service = service_fn(move |request| {
                            let runtime = runtime.clone();
                            async move {
                                Ok::<_, Infallible>(runtime.handle_request(request).await)
                            }
                        });
                        let connection = http1::Builder::new()
                            // Node keeps serving a request whose client has
                            // FINed its write side; hyper's default treats the
                            // same half-close as an incomplete-message error.
                            .half_close(true)
                            .serve_connection(TokioIo::new(stream), service);
                        tokio::pin!(connection);
                        tokio::select! {
                            _ = connection.as_mut() => {}
                            _ = shutdown.changed() => {
                                connection.as_mut().graceful_shutdown();
                                let _ = connection.await;
                            }
                        }
                    });
                }
            }
        }
        // In-flight requests finish; idle keep-alive connections were closed
        // by `graceful_shutdown` when the watch flipped.
        while connections.join_next().await.is_some() {}
    });
    Ok(Listener {
        port: bound,
        shutdown,
        task: Mutex::new(Some(task)),
    })
}
