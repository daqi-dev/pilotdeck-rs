//! Authenticated management of one native source installation.
use crate::update::{Error, Installation, RestartReply, RestartRequest, RuntimeClient};
use http_body_util::BodyExt;
use pilotdeck_gateway::native_process::{ControlHandler, ControlReply, ControlRequest};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

pub struct RestartManager {
    state: Arc<Mutex<State>>,
    trigger: tokio::sync::watch::Sender<bool>,
    admission: Arc<tokio::sync::Semaphore>,
    record: Arc<crate::runtime_record::RuntimeRecord>,
}
struct State {
    root: PathBuf,
    install_id: String,
    running: String,
    accepted: Option<(String, String)>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Input {
    install_id: String,
    expected_generation: String,
    target_generation: String,
    instance_id: String,
}
const MAX_RESPONSE_BYTES: usize = 16 * 1024;
fn reply(status: u16, value: Value) -> ControlReply {
    let body = value.to_string();
    if body.len() > MAX_RESPONSE_BYTES {
        return ControlReply {
            status: 500,
            body: r#"{"error":"management_response_too_large"}"#.into(),
        };
    }
    ControlReply { status, body }
}
impl RestartManager {
    pub fn new(
        installation: Installation,
    ) -> Result<(Arc<Self>, tokio::sync::watch::Receiver<bool>), Error> {
        let record = Arc::new(crate::runtime_record::RuntimeRecord::lock(&installation)?);
        let (trigger, receiver) = tokio::sync::watch::channel(false);
        Ok((
            Arc::new(Self {
                state: Arc::new(Mutex::new(State {
                    root: installation.root,
                    install_id: installation.policy.install_id,
                    running: installation.expected_generation,
                    accepted: None,
                })),
                trigger,
                admission: Arc::new(tokio::sync::Semaphore::new(1)),
                record,
            }),
            receiver,
        ))
    }
    pub fn publish_address(&self, address: SocketAddr) -> Result<(), Error> {
        self.record.publish(address)
    }
    pub fn handler(self: &Arc<Self>) -> ControlHandler {
        let owner = self.clone();
        Arc::new(move |request| {
            let owner = owner.clone();
            Box::pin(async move { owner.handle(request).await })
        })
    }
    async fn handle(&self, request: ControlRequest) -> ControlReply {
        if request.path == "/_control/context" {
            return match self.state.lock() {
                Ok(state) => reply(
                    200,
                    json!({"installId":state.install_id,"generation":state.running,"instanceId":self.record.instance_id()}),
                ),
                Err(_) => reply(503, json!({"error":"management_unavailable"})),
            };
        }
        let input: Input = match serde_json::from_slice(&request.body) {
            Ok(input) => input,
            Err(_) => return reply(400, json!({"error":"invalid_restart_request"})),
        };
        let Ok(permit) = self.admission.clone().try_acquire_owned() else {
            return reply(409, json!({"error":"restart_busy"}));
        };
        let state = self.state.clone();
        let instance = self.record.instance_id().to_owned();
        let result = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let (root, running, install_id) = {
                let state = state.lock().map_err(|_| "management_unavailable")?;
                if input.instance_id != instance
                    || input.install_id != state.install_id
                    || input.expected_generation != state.running
                    || input.target_generation == state.running
                {
                    return Err("installation_mismatch");
                }
                if let Some((target, id)) = &state.accepted {
                    return if *target == input.target_generation {
                        Ok((target.clone(), id.clone()))
                    } else {
                        Err("restart_already_accepted")
                    };
                }
                (
                    state.root.clone(),
                    state.running.clone(),
                    state.install_id.clone(),
                )
            };
            // The admission permit serializes restarts. Release the state lock
            // before hashing so /context cannot stall the async runtime on IO.
            // Reopen and hash the complete current generation before acknowledging.
            // The running generation remains fixed even after `current` switches.
            let installed =
                Installation::open(&root, &running).map_err(|_| "installation_unconfirmed")?;
            if installed.policy.install_id != install_id
                || installed.current.generation != input.target_generation
            {
                return Err("target_generation_mismatch");
            }
            let accepted = (input.target_generation, uuid::Uuid::new_v4().to_string());
            state.lock().map_err(|_| "management_unavailable")?.accepted = Some(accepted.clone());
            Ok(accepted)
        })
        .await;
        match result {
            Ok(Ok((target, id))) if self.trigger.send(true).is_ok() => reply(
                202,
                json!({"status":"accepted","restartId":id,"targetGeneration":target}),
            ),
            Ok(Ok(_)) => reply(503, json!({"error":"runtime_stopped"})),
            Ok(Err(error)) => reply(409, json!({"error":error})),
            Err(_) => reply(503, json!({"error":"management_unavailable"})),
        }
    }
    /// Called only after the shared host's bounded switch completed successfully.
    pub fn restart_executable(&self) -> Result<PathBuf, Error> {
        let state = self
            .state
            .lock()
            .map_err(|_| Error::new("restartFailed", "Restart state is unavailable."))?;
        let (target, _) = state
            .accepted
            .as_ref()
            .ok_or_else(|| Error::new("restartFailed", "No restart was accepted."))?;
        let installation = Installation::open(&state.root, &state.running)?;
        if installation.policy.install_id != state.install_id
            || installation.current.generation != *target
        {
            return Err(Error::new(
                "restartFailed",
                "Installed generation changed while the host was stopping. Restart manually.",
            ));
        }
        Ok(state
            .root
            .join("generations")
            .join(target)
            .join("bin/pilotdeck"))
    }
}

pub struct HttpRuntime {
    pub address: SocketAddr,
    pub token: String,
    pub instance_id: String,
}
struct Driver(tokio::task::JoinHandle<Result<(), hyper::Error>>);
impl Drop for Driver {
    fn drop(&mut self) {
        self.0.abort();
    }
}
impl HttpRuntime {
    fn request(&self, method: &str, path: &str, body: String) -> Result<Value, Error> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| Error::new("runtimeUnavailable", e.to_string()))?;
        runtime.block_on(async {
            let _local = crate::runtime_record::hold_local_address(self.address)?;
            tokio::time::timeout(Duration::from_secs(15), async {
                // Bind the outgoing connection to the same local interface as
                // the discovered listener before transmitting credentials.
                let connector = match self.address {
                    SocketAddr::V4(_) => tokio::net::TcpSocket::new_v4(),
                    SocketAddr::V6(_) => tokio::net::TcpSocket::new_v6(),
                }
                .map_err(|_| {
                    Error::new(
                        "runtimeUnavailable",
                        "Cannot create the native management socket.",
                    )
                })?;
                let mut source = self.address;
                source.set_port(0);
                connector.bind(source).map_err(|_| {
                    Error::new(
                        "runtimeUnavailable",
                        "Native service address is not bound locally.",
                    )
                })?;
                let socket = connector.connect(self.address).await.map_err(|e| {
                    Error::new(
                        if e.kind() == std::io::ErrorKind::ConnectionRefused {
                            "notRunning"
                        } else {
                            "runtimeUnavailable"
                        },
                        "Cannot connect to the running native service.",
                    )
                })?;
                let (mut sender, connection) =
                    hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(socket))
                        .await
                        .map_err(|_| {
                            Error::new(
                                "runtimeUnavailable",
                                "Native service HTTP handshake failed.",
                            )
                        })?;
                let _driver = Driver(tokio::spawn(connection));
                let request = hyper::Request::builder()
                    .method(method)
                    .uri(path)
                    .header("host", self.address.to_string())
                    .header("authorization", format!("Bearer {}", self.token))
                    .header("content-type", "application/json")
                    .header("connection", "close")
                    .body(http_body_util::Full::new(bytes::Bytes::from(body)))
                    .map_err(|_| {
                        Error::new(
                            "runtimeUnavailable",
                            "Invalid native service authentication or request.",
                        )
                    })?;
                let response = sender.send_request(request).await.map_err(|_| {
                    Error::new("runtimeUnavailable", "Native service request failed.")
                })?;
                if !response.status().is_success() {
                    return Err(Error::new(
                        "runtimeUnavailable",
                        format!(
                            "Native service refused management request (HTTP {}).",
                            response.status().as_u16()
                        ),
                    ));
                }
                let mut body = response.into_body();
                let mut bytes = Vec::new();
                while let Some(frame) = body.frame().await {
                    let frame = frame.map_err(|_| {
                        Error::new(
                            "runtimeUnavailable",
                            "Native service response was interrupted.",
                        )
                    })?;
                    if let Ok(data) = frame.into_data() {
                        if data.len() > MAX_RESPONSE_BYTES.saturating_sub(bytes.len()) {
                            return Err(Error::new(
                                "runtimeUnavailable",
                                "Native service response exceeds the management limit.",
                            ));
                        }
                        bytes.extend_from_slice(&data);
                    }
                }
                serde_json::from_slice(&bytes).map_err(|_| {
                    Error::new(
                        "runtimeUnavailable",
                        "Native service returned invalid management JSON.",
                    )
                })
            })
            .await
            .map_err(|_| {
                Error::new(
                    "runtimeUnavailable",
                    "Native service management request timed out.",
                )
            })?
        })
    }
}
impl RuntimeClient for HttpRuntime {
    fn available(&mut self, install_id: &str, expected_generation: &str) -> Result<bool, Error> {
        let value = match self.request("GET", "/_control/context", String::new()) {
            Ok(v) => v,
            Err(e) if e.reason == "notRunning" => return Ok(false),
            Err(e) => return Err(e),
        };
        if value.get("installId").and_then(Value::as_str) != Some(install_id)
            || value.get("generation").and_then(Value::as_str) != Some(expected_generation)
            || value.get("instanceId").and_then(Value::as_str) != Some(self.instance_id.as_str())
        {
            return Err(Error::new(
                "runtimeMismatch",
                "The running service belongs to another installation or generation.",
            ));
        }
        Ok(true)
    }
    fn restart(&mut self, input: &RestartRequest) -> Result<RestartReply, Error> {
        let value = self.request("POST", "/_control/restart", {
            let mut value = serde_json::to_value(input)
                .map_err(|e| Error::new("restartFailed", e.to_string()))?;
            value["instanceId"] = Value::String(self.instance_id.clone());
            value.to_string()
        })?;
        serde_json::from_value(value).map_err(|_| {
            Error::new(
                "restartUnconfirmed",
                "The native service did not confirm a restart.",
            )
        })
    }
}

#[cfg(test)]
mod source_binding_tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
    };
    #[test]
    fn oversized_management_reply_remains_a_bounded_parseable_error() {
        let response = reply(200, json!({"field":"x".repeat(MAX_RESPONSE_BYTES)}));
        assert_eq!(response.status, 500);
        assert!(response.body.len() <= MAX_RESPONSE_BYTES);
        assert_eq!(
            serde_json::from_str::<Value>(&response.body).unwrap()["error"],
            "management_response_too_large"
        );
    }
    #[test]
    fn management_source_is_bound_to_the_selected_local_interface() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut stream, peer) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut input = Vec::new();
            let mut chunk = [0_u8; 1024];
            while !input.windows(4).any(|part| part == b"\r\n\r\n") {
                let size = stream.read(&mut chunk).unwrap();
                assert!(size > 0 && input.len() + size <= 4096);
                input.extend_from_slice(&chunk[..size]);
            }
            let request = String::from_utf8_lossy(&input).to_ascii_lowercase();
            assert!(request.contains("authorization: bearer probe-token"));
            let body =
                r#"{"installId":"install","generation":"generation","instanceId":"instance"}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
            peer.ip()
        });
        let mut client = HttpRuntime {
            address,
            token: "probe-token".into(),
            instance_id: "instance".into(),
        };
        assert!(client.available("install", "generation").unwrap());
        assert_eq!(server.join().unwrap(), address.ip());
    }
}
