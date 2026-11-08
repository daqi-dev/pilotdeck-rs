//! Native installation defaults plus the legacy explicit deployment profile.
use super::Options;
use crate::{
    ProxyError,
    config::Env,
    runtime::turn::host_factory::{Deployment, resolve_deployment},
};
use std::path::{Path, PathBuf};

pub(super) fn resolve(options: &Options, env: &Env) -> Result<Deployment, ProxyError> {
    let profile = resolve_deployment(env, &options.project)?;
    match profile {
        Deployment::Native => Ok(owned_default(options, env)),
        Deployment::Tcp { .. } => Ok(profile),
        Deployment::Stdio {
            mut command,
            mut args,
            cwd,
            env: child_env,
        } => {
            let path = env
                .get("PILOTDECK_AGENT_LOOP_SIDECAR_PATH")
                .filter(|value| !crate::metadata::js_trim(value).is_empty())
                .map(|value| crate::metadata::resolve(&options.project, value));
            if env.contains_key("PILOTDECK_AGENT_LOOP_SIDECAR_COMMAND") {
                // The explicit interpreter/launcher always receives exactly the
                // selected path, like the TS command+[sidecarPath] contract.
                args = vec![
                    path.unwrap_or_else(|| options.sidecar.clone())
                        .to_string_lossy()
                        .into_owned(),
                ];
            } else if let Some(path) = path {
                if matches!(
                    path.extension().and_then(|s| s.to_str()),
                    Some("js" | "mjs" | "cjs")
                ) {
                    command = find_node(env, &options.project)?
                        .to_string_lossy()
                        .into_owned();
                    args = vec![path.to_string_lossy().into_owned()];
                } else {
                    // Native extension: a selected executable is the command,
                    // never an ignored path argument to the installed sidecar.
                    command = path.to_string_lossy().into_owned();
                    args.clear();
                }
            } else {
                return Ok(owned_default(options, env));
            }
            Ok(Deployment::Stdio {
                command,
                args,
                cwd,
                env: child_env,
            })
        }
    }
}
fn owned_default(options: &Options, env: &Env) -> Deployment {
    let mut child_env = env.clone();
    child_env.remove("PILOTDECK_AGENT_LOOP_TCP_HOST");
    child_env.remove("PILOTDECK_AGENT_LOOP_TCP_PORT");
    Deployment::Stdio {
        command: options.sidecar.to_string_lossy().into_owned(),
        args: options.sidecar_args.clone(),
        cwd: options.project.clone(),
        env: child_env,
    }
}
fn find_node(env: &Env, cwd: &Path) -> Result<PathBuf, ProxyError> {
    if let Some(path) = env.get("PATH") {
        for directory in std::env::split_paths(path) {
            let candidate = directory.join(if cfg!(windows) { "node.exe" } else { "node" });
            let candidate = if candidate.is_absolute() {
                candidate
            } else {
                cwd.join(candidate)
            };
            if let Ok(metadata) = std::fs::metadata(&candidate) {
                if !metadata.is_file() {
                    continue;
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if metadata.permissions().mode() & 0o111 == 0 {
                        continue;
                    }
                }
                return Ok(candidate);
            }
        }
    }
    Err("PILOTDECK_AGENT_LOOP_SIDECAR_PATH selects a JavaScript sidecar, but node is not available on PATH. Set PILOTDECK_AGENT_LOOP_SIDECAR_COMMAND explicitly.".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    fn options() -> Options {
        Options {
            home: "/fixture/home".into(),
            project: "/fixture/project".into(),
            listen: "127.0.0.1:0".parse().unwrap(),
            token_file: "/fixture/token".into(),
            sidecar: "/fixture/installed/pilotdeck-agent-loop-sidecar".into(),
            sidecar_args: vec!["--stdio".into()],
            drain_budget: std::time::Duration::from_secs(2),
            storage_root: None,
        }
    }
    #[test]
    fn isolated_options_preserve_explicit_tcp_profile() {
        let options = options();
        let inherited = env(&[
            ("PILOTDECK_AGENT_LOOP_TRANSPORT", "\u{a0}TCP\u{a0}"),
            ("PILOTDECK_AGENT_LOOP_TCP_HOST", "127.0.0.1"),
            ("PILOTDECK_AGENT_LOOP_TCP_PORT", "4321"),
        ]);
        let captured = options.environment(inherited);
        assert!(matches!(resolve(&options, &captured).unwrap(), Deployment::Tcp { port: 4321, .. }));
    }
    fn env(entries: &[(&str, &str)]) -> Env {
        entries
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }
    fn stdio(profile: Deployment) -> (String, Vec<String>, PathBuf, Env) {
        match profile {
            Deployment::Stdio {
                command,
                args,
                cwd,
                env,
            } => (command, args, cwd, env),
            _ => panic!("stdio profile expected"),
        }
    }
    #[test]
    fn native_and_empty_stdio_keep_installed_sidecar_and_remove_tcp_environment() {
        for transport in ["native", "", "stdio"] {
            let env = env(&[
                ("PILOTDECK_AGENT_LOOP_TRANSPORT", transport),
                ("PILOTDECK_AGENT_LOOP_TCP_HOST", "localhost"),
                ("PILOTDECK_AGENT_LOOP_TCP_PORT", "invalid"),
                ("KEEP", "fixture"),
            ]);
            let (command, args, cwd, child) = stdio(resolve(&options(), &env).unwrap());
            assert_eq!(command, "/fixture/installed/pilotdeck-agent-loop-sidecar");
            assert_eq!(args, ["--stdio"]);
            assert_eq!(cwd, options().project);
            assert!(!child.contains_key("PILOTDECK_AGENT_LOOP_TCP_HOST"));
            assert!(!child.contains_key("PILOTDECK_AGENT_LOOP_TCP_PORT"));
            assert_eq!(child.get("KEEP").map(String::as_str), Some("fixture"));
        }
    }
    #[test]
    fn explicit_stdio_command_and_path_are_real_argv_without_shell_or_path_trimming() {
        let env = env(&[
            ("PILOTDECK_AGENT_LOOP_TRANSPORT", "STDIO"),
            ("PILOTDECK_AGENT_LOOP_SIDECAR_COMMAND", " /fixture/runner "),
            ("PILOTDECK_AGENT_LOOP_SIDECAR_PATH", " ./peer.js "),
            ("PILOTDECK_AGENT_LOOP_TCP_HOST", "localhost"),
            ("PILOTDECK_AGENT_LOOP_TCP_PORT", "80"),
            ("KEEP", "fixture"),
        ]);
        let (command, args, cwd, child) = stdio(resolve(&options(), &env).unwrap());
        assert_eq!(command, "/fixture/runner");
        assert_eq!(args, ["/fixture/project/ ./peer.js "]);
        assert_eq!(cwd, options().project);
        assert_eq!(child.get("KEEP").map(String::as_str), Some("fixture"));
        assert!(!child.contains_key("PILOTDECK_AGENT_LOOP_TCP_PORT"));
    }
    #[test]
    fn path_only_native_executable_replaces_command_instead_of_becoming_ignored_argument() {
        let env = env(&[
            ("PILOTDECK_AGENT_LOOP_TRANSPORT", "stdio"),
            ("PILOTDECK_AGENT_LOOP_SIDECAR_PATH", "./custom-sidecar"),
        ]);
        let (command, args, _, _) = stdio(resolve(&options(), &env).unwrap());
        assert_eq!(command, "/fixture/project/custom-sidecar");
        assert!(args.is_empty());
    }
    #[cfg(unix)]
    #[test]
    fn explicit_javascript_uses_only_injected_path_and_missing_node_fails_clearly() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!("pdrs-deployment-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(root.clone());
        let mut env = env(&[
            ("PILOTDECK_AGENT_LOOP_TRANSPORT", "stdio"),
            ("PILOTDECK_AGENT_LOOP_SIDECAR_PATH", "peer.mjs"),
        ]);
        env.insert("PATH".into(), root.to_string_lossy().into_owned());
        assert!(
            resolve(&options(), &env)
                .unwrap_err()
                .to_string()
                .contains("node is not available on PATH")
        );
        let node = root.join("node");
        std::fs::write(&node, "fixture").unwrap();
        std::fs::set_permissions(&node, std::fs::Permissions::from_mode(0o700)).unwrap();
        let (command, args, _, _) = stdio(resolve(&options(), &env).unwrap());
        assert_eq!(command, node.to_string_lossy());
        assert_eq!(args, ["/fixture/project/peer.mjs"]);
    }
    #[test]
    fn real_ts_twenty_two_deployment_profiles_match_parameter_and_error_contract() {
        let oracle: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/native-process/transport-env-oracle.json"
        ))
        .unwrap();
        assert_eq!(oracle["rows"].as_array().unwrap().len(), 22);
        let executable = std::env::current_exe()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let default_path = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .join("pilotdeck-agent-loop-sidecar.js")
            .to_string_lossy()
            .into_owned();
        for row in oracle["rows"].as_array().unwrap() {
            let env: Env = row["env"]
                .as_object()
                .unwrap()
                .iter()
                .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_owned()))
                .collect();
            let actual = resolve_deployment(&env, Path::new(row["cwd"].as_str().unwrap()));
            if row["output"]["ok"] == false {
                assert_eq!(
                    actual.unwrap_err(),
                    row["output"]["error"]["message"].as_str().unwrap(),
                    "{}",
                    row["id"]
                );
                continue;
            }
            let actual = match actual.unwrap() {
                Deployment::Native => json!({"transport":"native"}),
                Deployment::Stdio {
                    command, args, env, ..
                } => {
                    json!({"transport":"stdio","command":if command==executable {"$NODE_EXEC".to_owned()}else{command},"args":args.into_iter().map(|v|if v==default_path {"$DEFAULT_SIDECAR".to_owned()}else{v}).collect::<Vec<_>>(),"env":env})
                }
                Deployment::Tcp {
                    host,
                    port,
                    connect_timeout_ms,
                } => {
                    let mut value = json!({"transport":"tcp","host":host,"port":port});
                    if let Some(ms) = connect_timeout_ms {
                        value["connectTimeoutMs"] = json!(ms);
                    }
                    value
                }
            };
            assert_eq!(actual, row["output"]["profile"], "{}", row["id"]);
        }
    }
}
