//! Authenticated Web update client (`ui/server/services/webUpdateCommand.js:42-70`).
use super::update_service::{UpdateCommandError, UpdateStatus, UpdateTarget};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use bytes::Bytes;
use http_body_util::{BodyExt, Empty, Full};
use hyper::{Request, header};
use hyper_util::{client::legacy::Client, rt::TokioExecutor};
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub struct OnlineUpdateClient {
    base: String,
    headers: HashMap<String, String>,
    project_root: PathBuf,
}

impl OnlineUpdateClient {
    pub fn connect(
        env: &HashMap<String, String>,
        project_root: Option<&Path>,
    ) -> Result<Self, UpdateCommandError> {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| failure("runtimeUnavailable", error.to_string()))?
            .block_on(connect_online(env, project_root))
    }

    pub fn check(&self) -> Result<UpdateStatus, UpdateCommandError> {
        self.post_json("check", Some("{}".into()))
            .and_then(|value| parse_status(&value))
    }

    pub fn apply(
        &self,
        target: &UpdateTarget,
        output: &mut dyn FnMut(&str),
    ) -> Result<(), UpdateCommandError> {
        let update_id = uuid::Uuid::new_v4().to_string();
        let body = serde_json::json!({
            "target": {
                "tagName": target.tag_name,
                "sourceSha": target.source_sha,
            },
            "updateId": update_id,
        });
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| failure("runtimeUnavailable", error.to_string()))?;
        runtime.block_on(async {
            match self.post_stream("apply", body.to_string(), output).await {
                Ok(()) => Ok(()),
                Err(error) if error.reason == "applyStreamLost" => {
                    output("Update connection interrupted; recovering the server task status...");
                    self.recover_apply(&update_id).await
                }
                Err(error) => Err(error),
            }
        })
    }

    pub fn restart(&self) -> Result<(), UpdateCommandError> {
        let value = self.post_json("restart", Some("{}".into()))?;
        if value.get("status").and_then(Value::as_str) == Some("accepted") {
            Ok(())
        } else {
            Err(failure(
                "restartUnconfirmed",
                "The runtime did not accept the restart request. Restart manually.",
            ))
        }
    }

    async fn verify_context(&mut self, preferred: Option<&Path>) -> Result<(), UpdateCommandError> {
        let value = self.request_json("context", None).await?;
        let remote = value
            .get("projectRoot")
            .and_then(Value::as_str)
            .ok_or_else(|| failure("runtimeUnavailable", "Web update context is invalid."))?;
        let remote_root = fs_canonical(Path::new(remote))?;
        let local_root = match preferred {
            Some(path) => fs_canonical(path)?,
            None => super::update::default_web_update_project_root().ok_or_else(|| {
                failure(
                    "runtimeUnavailable",
                    "Could not resolve the installation directory.",
                )
            })?,
        };
        let local_root = fs_canonical(local_root.as_path())?;
        if local_root != remote_root {
            return Err(failure(
                "differentInstallation",
                "SERVER_PORT points to another installation. Use the matching Web service.",
            ));
        }
        self.project_root = local_root;
        Ok(())
    }

    async fn recover_apply(&self, update_id: &str) -> Result<(), UpdateCommandError> {
        let deadline = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            + 60 * 60 * 1000;
        while SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            < deadline
        {
            let status = self.request_json("status", None).await?;
            if status
                .get("lastUpdateResult")
                .and_then(|value| value.get("updateId"))
                .and_then(Value::as_str)
                == Some(update_id)
            {
                if status
                    .get("lastUpdateResult")
                    .and_then(|value| value.get("success"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    return Ok(());
                }
                return Err(failure(
                    "applyFailed",
                    "The server reported that the update failed.",
                ));
            }
            if !status
                .get("updateInProgress")
                .and_then(Value::as_bool)
                .unwrap_or(false)
                || status
                    .get("currentUpdateId")
                    .and_then(Value::as_str)
                    != Some(update_id)
            {
                return Err(failure(
                    "updateUnconfirmed",
                    "Update outcome is unconfirmed. Check settings before retrying or restarting.",
                ));
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
        Err(failure(
            "updateUnconfirmed",
            "Update is still unconfirmed. Check settings before restarting.",
        ))
    }

    fn post_json(&self, route: &str, body: Option<String>) -> Result<Value, UpdateCommandError> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| failure("runtimeUnavailable", error.to_string()))?;
        runtime.block_on(self.request_json(route, body))
    }

    async fn post_stream(
        &self,
        route: &str,
        body: String,
        output: &mut dyn FnMut(&str),
    ) -> Result<(), UpdateCommandError> {
        let client: Client<hyper_util::client::legacy::connect::HttpConnector, Full<Bytes>> =
            Client::builder(TokioExecutor::new()).build_http();
        let mut request = Request::builder()
            .method("POST")
            .uri(format!("{}/api/update/{route}", self.base))
            .header(header::CONTENT_TYPE, "application/json");
        for (key, value) in &self.headers {
            request = request.header(key.as_str(), value.as_str());
        }
        let response = client
            .request(request.body(Full::new(Bytes::from(body))).map_err(|_| {
                failure("runtimeUnavailable", "Invalid Web update request.")
            })?)
            .await
            .map_err(|error| web_transport_error(error))?;
        if !response.status().is_success() {
            return Err(parse_error_response(response).await);
        }
        let mut body = response.into_body();
        let mut pending = String::new();
        loop {
            let frame = body.frame().await.transpose().map_err(|_| {
                failure("applyStreamLost", "Update stream ended unexpectedly.")
            })?;
            let Some(frame) = frame else { break };
            if let Ok(chunk) = frame.into_data() {
                pending.push_str(&String::from_utf8_lossy(&chunk));
                    while let Some(end) = pending.find('\n') {
                        let line = pending[..end].trim().to_string();
                        pending.drain(..=end);
                        if line.is_empty() {
                            continue;
                        }
                    let event: Value = serde_json::from_str(&line).map_err(|_| {
                        failure("applyFailed", "Update progress payload is invalid.")
                    })?;
                    if let Some(message) = event.get("message").and_then(Value::as_str) {
                        output(message);
                    }
                    if event.get("status").and_then(Value::as_str) == Some("error") {
                        return Err(failure(
                            event
                                .get("reason")
                                .and_then(Value::as_str)
                                .unwrap_or("applyFailed"),
                            event
                                .get("message")
                                .and_then(Value::as_str)
                                .unwrap_or("The server reported that the update failed."),
                        ));
                    }
                    if event.get("stage").and_then(Value::as_str) == Some("complete")
                        && event.get("status").and_then(Value::as_str) == Some("success")
                    {
                        return Ok(());
                    }
                }
            }
        }
        Err(failure(
            "applyStreamLost",
            "Update stream ended before completion.",
        ))
    }

    async fn request_json(
        &self,
        route: &str,
        body: Option<String>,
    ) -> Result<Value, UpdateCommandError> {
        let client: Client<hyper_util::client::legacy::connect::HttpConnector, Full<Bytes>> =
            Client::builder(TokioExecutor::new()).build_http();
        let method = if body.is_some() { "POST" } else { "GET" };
        let mut request = Request::builder()
            .method(method)
            .uri(format!("{}/api/update/{route}", self.base));
        for (key, value) in &self.headers {
            request = request.header(key.as_str(), value.as_str());
        }
        let request = if let Some(body) = body {
            request
                .header(header::CONTENT_TYPE, "application/json")
                .body(Full::new(Bytes::from(body)))
                .map_err(|_| failure("runtimeUnavailable", "Invalid Web update request."))?
        } else {
            request
                .body(Full::new(Bytes::new()))
                .map_err(|_| failure("runtimeUnavailable", "Invalid Web update request."))?
        };
        let response = client
            .request(request)
            .await
            .map_err(|error| web_transport_error(error))?;
        if !response.status().is_success() {
            return Err(parse_error_response(response).await);
        }
        let bytes = response
            .into_body()
            .collect()
            .await
            .map_err(|_| failure("runtimeUnavailable", "Web update response is unreadable."))?
            .to_bytes();
        serde_json::from_slice(&bytes).map_err(|_| {
            failure("runtimeUnavailable", "Web update response is invalid JSON.")
        })
    }
}

pub fn try_online_client(
    env: &HashMap<String, String>,
    project_root: Option<&Path>,
) -> Result<Option<OnlineUpdateClient>, UpdateCommandError> {
    let env = env.clone();
    let project_root = project_root.map(std::path::Path::to_path_buf);
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|error| failure("runtimeUnavailable", error.to_string()))?
                    .block_on(async move {
                        match connect_online(&env, project_root.as_deref()).await {
                            Ok(client) => Ok(Some(client)),
                            Err(error) if error.reason == "offline" => Ok(None),
                            Err(error) => Err(error),
                        }
                    })
            })
            .join()
            .map_err(|_| failure("runtimeUnavailable", "Online update probe thread panicked."))?
    })
}

async fn connect_online(
    env: &HashMap<String, String>,
    project_root: Option<&Path>,
) -> Result<OnlineUpdateClient, UpdateCommandError> {
    let port = web_port(env)?;
    let base = format!("http://127.0.0.1:{port}");
    probe_health(&base).await?;
    let headers = local_update_headers(env)?;
    let mut client = OnlineUpdateClient {
        base: base.clone(),
        headers,
        project_root: PathBuf::new(),
    };
    client.verify_context(project_root).await?;
    Ok(client)
}

async fn probe_health(base: &str) -> Result<(), UpdateCommandError> {
    tokio::time::timeout(Duration::from_secs(3), probe_health_inner(base))
        .await
        .map_err(|_| {
            failure(
                "runtimeUnavailable",
                "The configured Web port did not answer. Check SERVER_PORT.",
            )
        })?
}

async fn probe_health_inner(base: &str) -> Result<(), UpdateCommandError> {
    let client: Client<hyper_util::client::legacy::connect::HttpConnector, Empty<Bytes>> =
        Client::builder(TokioExecutor::new()).build_http();
    let request = Request::builder()
        .method("GET")
        .uri(format!("{base}/health"))
        .body(Empty::new())
        .map_err(|_| failure("runtimeUnavailable", "Invalid Web health request."))?;
    match client.request(request).await {
        Err(error) if connection_refused(&error) => Err(failure("offline", "offline")),
        Err(_) => Err(failure(
            "runtimeUnavailable",
            "The configured Web port could not be checked. Check SERVER_PORT.",
        )),
        Ok(response) if !response.status().is_success() => Err(failure(
            "runtimeUnavailable",
            "The configured Web port is not healthy. Check SERVER_PORT.",
        )),
        Ok(_) => Ok(()),
    }
}

fn local_update_headers(env: &HashMap<String, String>) -> Result<HashMap<String, String>, UpdateCommandError> {
    let database = resolved_database_path(env)?;
    let connection = Connection::open_with_flags(&database, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|_| failure("authenticationRequired", "Local Web credentials are unavailable. Use settings to update."))?;
    let (user_id, username): (i64, String) = connection
        .query_row("SELECT id, username FROM users ORDER BY id LIMIT 1", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .map_err(|_| {
            failure(
                "authenticationRequired",
                "Set up the Web user before updating through CLI/IM.",
            )
        })?;
    let secret = env
        .get("JWT_SECRET")
        .cloned()
        .or_else(|| {
            connection
                .query_row(
                    "SELECT value FROM app_config WHERE key = 'jwt_secret'",
                    [],
                    |row| row.get(0),
                )
                .ok()
        })
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            failure(
                "authenticationRequired",
                "Set up the Web user before updating through CLI/IM.",
            )
        })?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let payload = serde_json::json!({
        "userId": user_id,
        "username": username,
        "iat": now,
        "exp": now + 2 * 60 * 60,
    });
    let token = sign_hs256_jwt(&payload.to_string(), &secret)?;
    let mut headers = HashMap::from([
        ("Content-Type".into(), "application/json".into()),
        ("Authorization".into(), format!("Bearer {token}")),
    ]);
    if let Some(api_key) = env.get("API_KEY").filter(|value| !value.is_empty()) {
        headers.insert("x-api-key".into(), api_key.clone());
    }
    Ok(headers)
}

fn sign_hs256_jwt(payload_json: &str, secret: &str) -> Result<String, UpdateCommandError> {
    let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"HS256","typ":"JWT"}"#);
    let payload = URL_SAFE_NO_PAD.encode(payload_json.as_bytes());
    let signing_input = format!("{header}.{payload}");
    let block_size = 64;
    let mut key = secret.as_bytes().to_vec();
    if key.len() > block_size {
        key = Sha256::digest(&key).to_vec();
    }
    key.resize(block_size, 0);
    let mut ipad = vec![0x36u8; block_size];
    let mut opad = vec![0x5cu8; block_size];
    for index in 0..block_size {
        ipad[index] ^= key[index];
        opad[index] ^= key[index];
    }
    let mut inner = Sha256::new();
    inner.update(&ipad);
    inner.update(signing_input.as_bytes());
    let inner = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(&opad);
    outer.update(inner);
    let signature = URL_SAFE_NO_PAD.encode(outer.finalize());
    Ok(format!("{signing_input}.{signature}"))
}

fn parse_status(value: &Value) -> Result<UpdateStatus, UpdateCommandError> {
    Ok(UpdateStatus {
        can_update: value
            .get("canUpdate")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        reason: value
            .get("reason")
            .and_then(Value::as_str)
            .map(str::to_owned),
        latest: value.get("latest").and_then(parse_target),
    })
}

fn parse_target(value: &Value) -> Option<UpdateTarget> {
    Some(UpdateTarget {
        tag_name: value.get("tagName")?.as_str()?.to_owned(),
        source_sha: value.get("sourceSha")?.as_str()?.to_owned(),
    })
}

async fn parse_error_response(
    response: hyper::Response<hyper::body::Incoming>,
) -> UpdateCommandError {
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .map(|body| body.to_bytes())
        .unwrap_or_default();
    let detail: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    failure(
        detail
            .get("reason")
            .and_then(Value::as_str)
            .unwrap_or("runtimeUnavailable"),
        detail
            .get("message")
            .or_else(|| detail.get("error"))
            .and_then(Value::as_str)
            .unwrap_or_else(|| match status.as_u16() {
                401 | 403 => "Local Web credentials are unavailable. Use settings to update.",
                _ => "Web update request failed. Use settings or check SERVER_PORT.",
            }),
    )
}

fn resolved_database_path(env: &HashMap<String, String>) -> Result<PathBuf, UpdateCommandError> {
    let path = env
        .get("DATABASE_PATH")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            failure(
                "authenticationRequired",
                "Local Web credentials are unavailable. Use settings to update.",
            )
        })?;
    let path = PathBuf::from(path);
    if !path.is_file() {
        return Err(failure(
            "authenticationRequired",
            "Local Web credentials are unavailable. Use settings to update.",
        ));
    }
    Ok(path)
}

fn web_port(env: &HashMap<String, String>) -> Result<u16, UpdateCommandError> {
    let port_text = env
        .get("SERVER_PORT")
        .filter(|value| !value.is_empty())
        .map(String::as_str)
        .unwrap_or("3001");
    let port = pilotdeck_protocol::js_value::string_to_number(port_text);
    if port.is_finite() && port.fract() == 0.0 && (1.0..=65_535.0).contains(&port) {
        Ok(port as u16)
    } else {
        Err(failure(
            "invalidPort",
            "SERVER_PORT must identify the local Web service.",
        ))
    }
}

fn fs_canonical(path: &Path) -> Result<PathBuf, UpdateCommandError> {
    std::fs::canonicalize(path).map_err(|_| {
        failure(
            "runtimeUnavailable",
            "Could not resolve the installation directory.",
        )
    })
}

fn web_transport_error(error: hyper_util::client::legacy::Error) -> UpdateCommandError {
    if connection_refused(&error) {
        failure("offline", "offline")
    } else {
        failure(
            "runtimeUnavailable",
            "The configured Web port could not be checked. Check SERVER_PORT.",
        )
    }
}

fn connection_refused(error: &(dyn std::error::Error + 'static)) -> bool {
    let mut current = Some(error);
    while let Some(error) = current {
        if error
            .downcast_ref::<std::io::Error>()
            .is_some_and(|error| error.kind() == std::io::ErrorKind::ConnectionRefused)
        {
            return true;
        }
        current = error.source();
    }
    false
}

fn failure(reason: impl Into<String>, message: impl Into<String>) -> UpdateCommandError {
    UpdateCommandError::new(reason, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        sync::{Arc, Mutex},
        thread,
    };

    #[test]
    fn online_client_uses_authenticated_update_routes() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let calls = Arc::new(Mutex::new(Vec::<String>::new()));
        let calls_clone = calls.clone();
        let scratch = crate::scratch::Scratch::new("update-web-client");
        let root = scratch.path().canonicalize().unwrap();
        let server = thread::spawn(move || {
            for _ in 0..4 {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(3)))
                    .unwrap();
                let mut buffer = [0u8; 4096];
                let read = stream.read(&mut buffer).unwrap();
                let request = String::from_utf8_lossy(&buffer[..read]);
                let route = request
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap_or("")
                    .to_string();
                calls_clone.lock().unwrap().push(route.clone());
                let body = if route.ends_with("/check") {
                    r#"{"canUpdate":true,"latest":{"tagName":"v2026.09.07","sourceSha":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}}"#.into()
                } else if route.ends_with("/context") {
                    format!(r#"{{"projectRoot":"{}"}}"#, root.display())
                } else if route.ends_with("/restart") {
                    r#"{"status":"accepted"}"#.into()
                } else {
                    "{}".into()
                };
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .unwrap();
            }
        });
        let db = scratch.path().join("auth.db");
        let connection = Connection::open(&db).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE users (id INTEGER PRIMARY KEY, username TEXT NOT NULL);
                 CREATE TABLE app_config (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 INSERT INTO users VALUES (1, 'local');
                 INSERT INTO app_config VALUES ('jwt_secret', '0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef');",
            )
            .unwrap();
        drop(connection);
        let env = HashMap::from([
            ("SERVER_PORT".into(), port.to_string()),
            ("DATABASE_PATH".into(), db.to_string_lossy().into()),
        ]);
        let client = OnlineUpdateClient::connect(&env, Some(scratch.path())).unwrap();
        assert!(client.check().unwrap().can_update);
        client.restart().unwrap();
        server.join().unwrap();
        let routes = calls.lock().unwrap();
        assert!(routes.iter().any(|route| route.ends_with("/health")));
        assert!(routes.iter().any(|route| route.contains("/api/update/context")));
        assert!(routes.iter().any(|route| route.contains("/api/update/check")));
        assert!(routes.iter().any(|route| route.contains("/api/update/restart")));
    }

    #[test]
    fn offline_port_yields_no_online_client() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let env = HashMap::from([("SERVER_PORT".into(), port.to_string())]);
        assert!(try_online_client(&env, None)
            .unwrap()
            .is_none());
    }

    #[test]
    fn health_probe_runs_even_without_installation_root() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let scratch = crate::scratch::Scratch::new("update-health-no-root");
        let expected_root = super::super::update::default_web_update_project_root()
            .expect("repo ships update-web.mjs")
            .canonicalize()
            .unwrap();
        let server = thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(3)))
                    .unwrap();
                let mut buffer = [0u8; 4096];
                let read = stream.read(&mut buffer).unwrap();
                let request = String::from_utf8_lossy(&buffer[..read]);
                let route = request
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap_or("");
                let body = if route.ends_with("/context") {
                    format!(r#"{{"projectRoot":"{}"}}"#, expected_root.display())
                } else {
                    "ok".into()
                };
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .unwrap();
            }
        });
        let db = scratch.path().join("auth.db");
        let connection = Connection::open(&db).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE users (id INTEGER PRIMARY KEY, username TEXT NOT NULL);
                 CREATE TABLE app_config (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 INSERT INTO users VALUES (1, 'local');
                 INSERT INTO app_config VALUES ('jwt_secret', '0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef');",
            )
            .unwrap();
        drop(connection);
        let env = HashMap::from([
            ("SERVER_PORT".into(), port.to_string()),
            ("DATABASE_PATH".into(), db.to_string_lossy().into()),
        ]);
        let client = try_online_client(&env, None).unwrap();
        assert!(
            client.is_some(),
            "healthy web must connect via /api/update/context even without a local root"
        );
        server.join().unwrap();
    }

    #[test]
    fn online_backend_missing_reason_is_never_emitted_for_healthy_web() {
        let error = resolved_database_path(&HashMap::new()).unwrap_err();
        assert_ne!(error.reason, "onlineBackendMissing");
        assert_eq!(
            error.reason, "authenticationRequired",
            "webUpdateCommand.js:31 when DATABASE_PATH is missing"
        );
    }

    #[test]
    fn database_path_must_be_present_and_existing() {
        assert_eq!(
            resolved_database_path(&HashMap::new())
                .unwrap_err()
                .reason,
            "authenticationRequired"
        );
        let scratch = crate::scratch::Scratch::new("update-db-path");
        let missing = scratch.path().join("missing.db");
        assert_eq!(
            resolved_database_path(&HashMap::from([(
                "DATABASE_PATH".into(),
                missing.to_string_lossy().into(),
            )]))
            .unwrap_err()
            .reason,
            "authenticationRequired"
        );
    }
}
