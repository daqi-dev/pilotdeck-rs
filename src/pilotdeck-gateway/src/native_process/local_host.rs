//! Process-independent CLI ownership of the existing native Gateway assembly.
//!
//! This facade owns no listener, reads no authentication file, and leaves the
//! caller's captured environment intact. Local transports must bound their
//! text sink and drive request completion concurrently with outgoing events.
use super::{Options, assembly};
use crate::{
    ProxyError,
    config::Env,
    runtime::{
        connection::{GatewayTextConnection, RequestError},
        turn::gateway::{HostedConnection, TurnGateway},
    },
};
use pilotdeck_protocol::{JsValue, stringify_json};
use std::{net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};

/// Explicit local authorities. Network binding and token files belong to the
/// server embedding and are deliberately absent from this API.
#[derive(Clone, Debug)]
pub struct LocalHostOptions {
    pub home: PathBuf,
    pub project: PathBuf,
    pub sidecar: PathBuf,
    pub sidecar_args: Vec<String>,
    pub drain_budget: Duration,
}

impl LocalHostOptions {
    fn into_assembly_options(self) -> Result<Options, ProxyError> {
        for (name, path) in [
            ("home", &self.home),
            ("project", &self.project),
            ("sidecar", &self.sidecar),
        ] {
            if !path.is_absolute() || path.to_str().is_none() {
                return Err(format!("local {name} must be an absolute UTF-8 path").into());
            }
        }
        // Transitional private adaptation: assembly, Requests, Services and
        // SessionTools consume neither listen nor token_file. Do not call
        // Options::parse/environment or native_process::run here. Moving their
        // common fields into a core type can follow without changing this API.
        Ok(Options {
            home: self.home,
            project: self.project,
            listen: SocketAddr::from(([127, 0, 0, 1], 0)),
            token_file: PathBuf::new(),
            sidecar: self.sidecar,
            sidecar_args: self.sidecar_args,
            drain_budget: self.drain_budget,
            storage_root: None,
        })
    }
}

/// A single local owner. Closing a client connection never stops this owner;
/// the embedding closes all its connections, then explicitly awaits shutdown.
pub struct LocalHost {
    host: TurnGateway,
    token: String,
    drain_budget: Duration,
}

impl LocalHost {
    pub async fn start(options: LocalHostOptions, env: Env) -> Result<Self, ProxyError> {
        Self::start_adapter(options.into_assembly_options()?, env).await
    }

    async fn start_adapter(options: Options, env: Env) -> Result<Self, ProxyError> {
        let drain_budget = options.drain_budget;
        let host = assembly::start_local(options, env).await?;
        Ok(Self {
            host,
            token: uuid::Uuid::new_v4().to_string(),
            drain_budget,
        })
    }

    /// Authenticate a real HostedConnection over an embedding-provided sink.
    /// No socket is opened and the process-local token is never persisted.
    ///
    /// The sink must have a bounded frame/byte budget and close on overflow.
    /// After this hello, use enqueue(), then poll its completion concurrently
    /// with sink delivery; waiting for a whole turn before reading would block
    /// permission prompts. The transport owns pending-job and input budgets.
    pub async fn connect(
        &self,
        sink: Arc<dyn GatewayTextConnection>,
        client_name: &str,
    ) -> Result<HostedConnection, RequestError> {
        let connection =
            self.host
                .connect(sink, self.token.clone(), env!("CARGO_PKG_VERSION").into());
        let hello = crate::metadata::object([
            ("type", "hello".into()),
            (
                "protocolVersion",
                pilotdeck_protocol::GATEWAY_PROTOCOL_VERSION.into(),
            ),
            ("clientName", JsValue::from(client_name)),
            ("clientVersion", env!("CARGO_PKG_VERSION").into()),
            ("token", self.token.as_str().into()),
        ]);
        let raw = stringify_json(&hello)
            .map_err(|error| RequestError::new(error.to_string()))?
            .ok_or_else(|| RequestError::new("Local Gateway hello is unavailable."))?;
        if let Err(error) = connection.receive(raw).await {
            let _ = connection.closed().await;
            return Err(error);
        }
        Ok(connection)
    }

    pub fn request_shutdown(&self) {
        self.host.request_shutdown(self.drain_budget);
    }

    pub async fn shutdown(&self) -> Result<(), String> {
        self.host.shutdown(self.drain_budget).await
    }
}

impl Drop for LocalHost {
    fn drop(&mut self) {
        self.request_shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use std::{net::TcpListener, sync::Mutex};

    struct Scratch(PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    #[derive(Default)]
    struct Text(Mutex<Vec<Value>>);
    impl GatewayTextConnection for Text {
        fn send_text(&self, raw: String) {
            assert!(raw.len() < 64 * 1024);
            let mut frames = self.0.lock().unwrap();
            assert!(frames.len() < 16);
            frames.push(serde_json::from_str(&raw).unwrap());
        }
        fn close(&self, code: u16, reason: &str) {
            panic!("unexpected local close {code}: {reason}");
        }
    }
    fn fixture() -> (Scratch, LocalHostOptions, Env) {
        let root = std::env::temp_dir().join(format!("pdrs-local-host-{}", uuid::Uuid::new_v4()));
        // A CLI project can be nested beneath its explicit home. This must not
        // accidentally acquire the isolated server's separate-directory rule.
        let home = root.join("home");
        let project = home.join("project");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(home.join("pilotdeck.yaml"), "schemaVersion: 1\nagent: {model: fixture/mock}\nmodel:\n  providers:\n    fixture:\n      protocol: openai\n      url: http://127.0.0.1:1/v1\n      apiKey: mock-only\n      models:\n        mock: {}\ncron: {enabled: true}\nadapters:\n  qq: {enabled: true}\n  weixin: {enabled: true}\n").unwrap();
        let options = LocalHostOptions {
            home: home.clone(),
            project,
            // No turn is run in this wiring test, so no executable is needed.
            sidecar: root.join("unused-sidecar"),
            sidecar_args: vec![],
            drain_budget: Duration::from_secs(2),
        };
        let mut env = Env::new();
        env.insert(
            "HOME".into(),
            root.join("account-home").to_string_lossy().into_owned(),
        );
        env.insert("PILOT_HOME".into(), home.to_string_lossy().into_owned());
        (Scratch(root), options, env)
    }

    #[tokio::test]
    async fn local_uses_memory_connection_without_server_authorities_or_automation() {
        let (_scratch, options, env) = fixture();
        let home = options.home.clone();
        // Both tripwires are real: binding this address fails, and reading the
        // token authority as a file fails. The local host must use neither.
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let mut adapter = options.clone().into_assembly_options().unwrap();
        adapter.listen = listener.local_addr().unwrap();
        adapter.token_file = home.clone();
        let host = tokio::time::timeout(
            Duration::from_secs(10),
            LocalHost::start_adapter(adapter, env.clone()),
        )
        .await
        .unwrap()
        .unwrap();
        let sink = Arc::new(Text::default());
        let connection = host.connect(sink.clone(), "cli").await.unwrap();
        connection
            .receive(
                json!({"type":"request","id":"describe","method":"describe_server","params":{}})
                    .to_string(),
            )
            .await
            .unwrap();
        connection
            .receive(
                json!({"type":"request","id":"weixin","method":"prepare_weixin_login","params":{}})
                    .to_string(),
            )
            .await
            .unwrap();
        {
            let frames = sink.0.lock().unwrap();
            assert!(frames.iter().any(|f| f["type"] == "hello_ok"));
            let describe = frames.iter().find(|f| f["id"] == "describe").unwrap();
            let capabilities = describe["result"]["capabilities"].as_array().unwrap();
            for method in crate::runtime::cron::CRON_METHODS {
                assert!(!capabilities.iter().any(|c| c.as_str() == Some(method)));
            }
            let login = frames.iter().find(|f| f["id"] == "weixin").unwrap();
            assert_eq!(login["result"]["requested"], false);
            assert_eq!(login["result"]["reason"], "unsupported");
        }
        assert!(
            !home.join("channels").exists(),
            "local startup activated IM state"
        );
        assert!(!home.join("server-token").exists());
        connection.closed().await.unwrap();
        host.shutdown().await.unwrap();
        assert!(
            !home.join("channels").exists(),
            "local cleanup created IM state"
        );
        // The same enabled, unimplemented QQ adapter still refuses a server
        // start. Local mode must not weaken the existing server startup gate.
        let server_options = options.into_assembly_options().unwrap();
        let result = assembly::start(server_options, env).await;
        assert!(
            result.is_err(),
            "server no longer rejects unsupported adapters"
        );
    }
}
