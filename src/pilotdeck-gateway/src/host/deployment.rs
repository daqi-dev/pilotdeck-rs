//! Application-owned AgentLoop deployment selection.

use crate::js_value::string_to_number;
use crate::metadata::js_trim;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};

pub const TRANSPORT: &str = "PILOTDECK_AGENT_LOOP_TRANSPORT";
pub const SIDECAR_PATH: &str = "PILOTDECK_AGENT_LOOP_SIDECAR_PATH";
pub const SIDECAR_COMMAND: &str = "PILOTDECK_AGENT_LOOP_SIDECAR_COMMAND";
pub const TCP_HOST: &str = "PILOTDECK_AGENT_LOOP_TCP_HOST";
pub const TCP_PORT: &str = "PILOTDECK_AGENT_LOOP_TCP_PORT";
pub const CONNECT_TIMEOUT_MS: &str = "PILOTDECK_AGENT_LOOP_CONNECT_TIMEOUT_MS";
pub const FACTORY: &str = "PILOTDECK_AGENT_LOOP_FACTORY";
const JS_SIDECAR_PATH: &str = "PILOTDECK_AGENT_LOOP_JS_SIDECAR_PATH";
/// Packaged install layout (`dist/src/cli/...`) when an ancestor ships the Web bundle.
const PACKAGED_JS_SIDECAR: &str = "dist/src/cli/pilotdeck-agent-loop-sidecar.js";
const COMPILED_JS_SIDECAR: &str = "assets/cli/pilotdeck-agent-loop-sidecar.js";
const RUST_SIDECAR_PATH: &str = "PILOTDECK_AGENT_LOOP_RUST_SIDECAR_PATH";
#[allow(dead_code)]
const SIDECAR_LAUNCHER: &str = "pilotdeck-agent-loop-sidecar-launcher.mjs";
#[allow(dead_code)]
const LAUNCHER_CONTENT: &[u8] = include_bytes!(
    "../../../pilotdeck-transport/assets/sidecar/pilotdeck-agent-loop-sidecar-launcher.mjs"
);

// Replace only deployment anchors in fixture tests, without changing NODE or
// installing assets into the checkout. Thread-local scope isolates other tests.
#[cfg(test)]
thread_local! {
    static TEST_MODULE_ROOT: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Profile {
    Native,
    Stdio {
        command: PathBuf,
        args: Vec<String>,
        env: HashMap<String, String>,
    },
    Tcp {
        host: String,
        port: u16,
        connect_timeout: Option<Duration>,
    },
}

pub fn resolve(
    env: &HashMap<String, String>,
    cwd: &Path,
    _default_sidecar: &Path,
) -> Result<Profile, String> {
    let transport = env
        .get(TRANSPORT)
        .map(|value| js_trim(value).to_ascii_lowercase())
        .unwrap_or_default();
    match transport.as_str() {
        "" | "native" => Ok(Profile::Native),
        "tcp" => {
            let host = required_text(
                env.get(TCP_HOST).map(String::as_str).unwrap_or("127.0.0.1"),
                TCP_HOST,
            )?;
            let port = required_port(env.get(TCP_PORT).map(String::as_str))?;
            let connect_timeout =
                optional_positive_integer(env.get(CONNECT_TIMEOUT_MS).map(String::as_str))?
                    .map(Duration::from_millis);
            Ok(Profile::Tcp {
                host,
                port,
                connect_timeout,
            })
        }
        "stdio" => {
            let configured_path = env
                .get(SIDECAR_PATH)
                .filter(|value| !js_trim(value).is_empty())
                .map(|value| resolve_path(cwd, value));
            let configured_command = env
                .get(SIDECAR_COMMAND)
                .map(|value| required_text(value, SIDECAR_COMMAND))
                .transpose()?;
            // AgentLoopDeploymentProfile.ts:62-73 — default sidecar sits beside the compiled profile module.
            let sidecar_path = configured_path.unwrap_or_else(|| {
                if let Some(path) = env
                    .get(JS_SIDECAR_PATH)
                    .filter(|value| !js_trim(value).is_empty())
                {
                    return resolve_path(cwd, path);
                }
                if let Ok(path) = std::env::var("PILOTDECK_AGENT_LOOP_JS_SIDECAR") {
                    if !js_trim(&path).is_empty() {
                        return resolve_path(cwd, &path);
                    }
                }
                find_packaged_js_sidecar().unwrap_or_else(compiled_module_js_sidecar)
            });
            let command = configured_command
                .map(PathBuf::from)
                .unwrap_or_else(|| default_stdio_js_command());
            let args = vec![sidecar_path.to_string_lossy().into_owned()];
            let mut child_env = env.clone();
            child_env.remove(TCP_HOST);
            child_env.remove(TCP_PORT);
            child_env.remove(RUST_SIDECAR_PATH);
            Ok(Profile::Stdio {
                command,
                args,
                env: child_env,
            })
        }
        _ => Err(format!("{TRANSPORT} must be native, stdio, or tcp.")),
    }
}

/// `AgentLoopDeploymentProfile.ts:72` — default sidecar command is `process.execPath`.
pub(crate) fn default_stdio_js_command() -> PathBuf {
    if let Ok(node) = std::env::var("NODE") {
        if !js_trim(&node).is_empty() {
            return PathBuf::from(node);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if exe
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name == "node" || name.starts_with("node"))
        {
            return exe;
        }
    }
    which_node_executable().unwrap_or_else(|| PathBuf::from("node"))
}

pub(crate) fn update_js_executable() -> PathBuf {
    default_stdio_js_command()
}

fn which_node_executable() -> Option<PathBuf> {
    std::process::Command::new("/usr/bin/which")
        .arg("node")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|path| PathBuf::from(path.trim()))
        .filter(|path| !path.as_os_str().is_empty())
}

pub(crate) fn find_packaged_js_sidecar_from(start: &Path) -> Option<PathBuf> {
    for current in start.ancestors() {
        let candidate = current.join(PACKAGED_JS_SIDECAR);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn compiled_module_js_sidecar() -> PathBuf {
    #[cfg(test)]
    if let Some(root) = TEST_MODULE_ROOT.with(|root| root.borrow().clone()) {
        return root.join(COMPILED_JS_SIDECAR);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(COMPILED_JS_SIDECAR)
}

pub(crate) fn compiled_module_js_sidecar_for_tests() -> PathBuf {
    compiled_module_js_sidecar()
}

fn find_packaged_js_sidecar() -> Option<PathBuf> {
    #[cfg(test)]
    if let Some(root) = TEST_MODULE_ROOT.with(|root| root.borrow().clone()) {
        return find_packaged_js_sidecar_from(&root);
    }
    let mut starts = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            starts.push(parent.to_path_buf());
        }
    }
    starts.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")));
    for start in starts {
        if let Some(path) = find_packaged_js_sidecar_from(&start) {
            return Some(path);
        }
    }
    None
}

#[allow(dead_code)]
fn bundled_launcher(default_sidecar: &Path) -> Result<PathBuf, String> {
    let adjacent = default_sidecar.with_file_name(SIDECAR_LAUNCHER);
    match fs::symlink_metadata(&adjacent) {
        Ok(metadata) => {
            if metadata.is_file() && fs::read(&adjacent).ok().as_deref() == Some(LAUNCHER_CONTENT) {
                return Ok(adjacent);
            }
            return Err("The adjacent Sidecar launcher does not match this installation.".into());
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("Could not read the Sidecar launcher: {error}")),
    }
    // The launcher is embedded in release builds as well. Materialize one
    // content-addressed file per user/version, without changing PilotHome or
    // requiring a custom build script or a writable installation directory.
    cache_launcher()
}

fn cache_launcher() -> Result<PathBuf, String> {
    let mut directory = std::env::temp_dir();
    #[cfg(unix)]
    let owner = rustix::process::geteuid().as_raw().to_string();
    #[cfg(not(unix))]
    let owner = std::env::var("USERNAME").unwrap_or_else(|_| "user".into());
    directory.push(format!("pilotdeck-sidecar-launcher-{owner}"));
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    match builder.create(&directory) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(format!(
                "Could not create the Sidecar launcher cache: {error}"
            ));
        }
    }
    let metadata = fs::symlink_metadata(&directory).map_err(|error| error.to_string())?;
    if !metadata.is_dir() {
        return Err("Sidecar launcher cache must be a directory.".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != rustix::process::geteuid().as_raw() || metadata.mode() & 0o077 != 0 {
            return Err(
                "Sidecar launcher cache must be private and owned by the current user.".into(),
            );
        }
    }
    let path = directory.join(format!("{:x}.mjs", Sha256::digest(LAUNCHER_CONTENT)));
    if fs::symlink_metadata(&path).is_ok() {
        return validate_cached_launcher(&path);
    }
    let prepared = directory.join(format!("{}.prepared", uuid::Uuid::new_v4()));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(&prepared) {
        Ok(mut file) => {
            file.write_all(LAUNCHER_CONTENT)
                .map_err(|error| error.to_string())?;
            file.sync_all().map_err(|error| error.to_string())?;
        }
        Err(error) => return Err(format!("Could not write the Sidecar launcher: {error}")),
    }
    // Publish only complete bytes and never replace an existing file. Retain
    // the prepared inode instead of removing files; subsequent calls reuse the
    // final hash path and create no new files. If tmp is cleared, recreate it.
    match fs::hard_link(&prepared, &path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(format!("Could not publish the Sidecar launcher: {error}")),
    }
    validate_cached_launcher(&path)
}

fn validate_cached_launcher(path: &Path) -> Result<PathBuf, String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if !metadata.is_file() {
        return Err("Cached Sidecar launcher must be a regular file.".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != rustix::process::geteuid().as_raw() || metadata.mode() & 0o077 != 0 {
            return Err(
                "Cached Sidecar launcher must be private and owned by the current user.".into(),
            );
        }
    }
    if fs::read(path).ok().as_deref() == Some(LAUNCHER_CONTENT) {
        return Ok(path.to_path_buf());
    }
    Err("Cached Sidecar launcher does not match this installation.".into())
}

fn resolve_path(cwd: &Path, value: &str) -> PathBuf {
    let path = Path::new(value);
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    let mut resolved = PathBuf::from("/");
    for component in joined.components() {
        match component {
            std::path::Component::RootDir | std::path::Component::Prefix(_) => {
                resolved = PathBuf::from("/");
            }
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                resolved.pop();
            }
            std::path::Component::Normal(name) => resolved.push(name),
        }
    }
    resolved
}

fn required_text(value: &str, name: &str) -> Result<String, String> {
    let value = js_trim(value);
    if value.is_empty() {
        Err(format!("{name} must not be empty."))
    } else {
        Ok(value.into())
    }
}

fn required_port(value: Option<&str>) -> Result<u16, String> {
    let Some(value) = value.map(js_trim).filter(|value| !value.is_empty()) else {
        return Err(format!("{TCP_PORT} is required when transport is tcp."));
    };
    let value = string_to_number(value);
    if value.is_finite() && value.fract() == 0.0 && (1.0..=65_535.0).contains(&value) {
        Ok(value as u16)
    } else {
        Err(format!(
            "{TCP_PORT} must be an integer between 1 and 65535."
        ))
    }
}

fn optional_positive_integer(value: Option<&str>) -> Result<Option<u64>, String> {
    let Some(value) = value.map(js_trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let value = string_to_number(value);
    if value.is_finite() && value.fract() == 0.0 && value > 0.0 {
        // Node clamps out-of-range timer delays to one millisecond.
        Ok(Some(if value > f64::from(i32::MAX) {
            1
        } else {
            value as u64
        }))
    } else {
        Err(format!("{CONNECT_TIMEOUT_MS} must be a positive integer."))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_module_root<T>(root: &Path, run: impl FnOnce() -> T) -> T {
        struct Reset(Option<PathBuf>);
        impl Drop for Reset {
            fn drop(&mut self) {
                TEST_MODULE_ROOT.with(|root| root.replace(self.0.take()));
            }
        }
        let _reset = Reset(TEST_MODULE_ROOT.with(|slot| slot.replace(Some(root.into()))));
        run()
    }

    fn sidecar_fixture(name: &str, relative_path: &str) -> (Scratch, PathBuf) {
        let scratch = Scratch::new(name);
        let sidecar = scratch.path().join(relative_path);
        fs::create_dir_all(sidecar.parent().unwrap()).unwrap();
        fs::write(scratch.path().join("package.json"), r#"{"type":"module"}"#).unwrap();
        // This adapter emits events only after importing and calling the factory
        // and consuming its real async iterator; no canned success response.
        fs::write(
            &sidecar,
            r#"import { createInterface } from 'node:readline';
import { pathToFileURL } from 'node:url';
import { resolve } from 'node:path';
const lines = createInterface({ input: process.stdin });
for await (const line of lines) {
    const request = JSON.parse(line);
    if (request.kind !== 'request' || request.method !== 'execute') throw new Error('expected execute');
    const factory = await import(pathToFileURL(resolve(process.env.PILOTDECK_AGENT_LOOP_FACTORY)).href);
    const execution = await factory.default(request.payload);
    for await (const event of execution.loop.run(execution.input)) {
        console.log(JSON.stringify({ kind: 'notification', method: 'agent.tick', payload: event }));
    }
    console.log(JSON.stringify({ kind: 'response', messageId: request.messageId, outcome: 'success' }));
}
"#,
        )
        .unwrap();
        fs::write(sidecar.with_file_name(SIDECAR_LAUNCHER), LAUNCHER_CONTENT).unwrap();
        (scratch, sidecar)
    }

    /// Local scratch directory: B has a crate-wide `crate::scratch::Scratch`,
    /// this crate keeps one per test module. Same contract — a unique temp
    /// root removed on drop.
    struct Scratch(PathBuf);
    impl Scratch {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir()
                .join(format!("pdrs-deployment-{name}-{}", std::process::id()))
                .join(uuid::Uuid::new_v4().to_string());
            std::fs::create_dir_all(&root).unwrap();
            Self(root)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn env(values: &[(&str, &str)]) -> HashMap<String, String> {
        values
            .iter()
            .map(|(key, value)| ((*key).into(), (*value).into()))
            .collect()
    }

    #[test]
    fn resolves_native_stdio_and_tcp_profiles() {
        assert_eq!(
            resolve(&env(&[]), Path::new("/cwd"), Path::new("/bin/sidecar")).unwrap(),
            Profile::Native
        );
        let stdio = resolve(
            &env(&[
                (TRANSPORT, " StDiO "),
                (SIDECAR_PATH, "tools/sidecar"),
                (TCP_HOST, "leak"),
                (TCP_PORT, "9"),
            ]),
            Path::new("/cwd"),
            Path::new("/bin/default"),
        )
        .unwrap();
        let Profile::Stdio {
            command,
            args,
            env: child_env,
        } = stdio
        else {
            panic!("stdio")
        };
        assert_eq!(command, default_stdio_js_command());
        assert_eq!(args, ["/cwd/tools/sidecar"]);
        assert!(!child_env.contains_key(TCP_HOST));
        assert!(!child_env.contains_key(TCP_PORT));
        assert_eq!(
            resolve(
                &env(&[
                    (TRANSPORT, "tcp"),
                    (TCP_PORT, "28790"),
                    (CONNECT_TIMEOUT_MS, "250")
                ]),
                Path::new("/cwd"),
                Path::new("/bin/default"),
            )
            .unwrap(),
            Profile::Tcp {
                host: "127.0.0.1".into(),
                port: 28790,
                connect_timeout: Some(Duration::from_millis(250))
            }
        );
    }

    #[test]
    fn rejects_invalid_transport_and_tcp_fields() {
        for (values, message) in [
            (vec![(TRANSPORT, "pipe")], "must be native, stdio, or tcp"),
            (
                vec![(TRANSPORT, "tcp")],
                "is required when transport is tcp",
            ),
            (
                vec![(TRANSPORT, "tcp"), (TCP_PORT, "65536")],
                "between 1 and 65535",
            ),
            (
                vec![
                    (TRANSPORT, "tcp"),
                    (TCP_PORT, "1"),
                    (CONNECT_TIMEOUT_MS, "0"),
                ],
                "must be a positive integer",
            ),
        ] {
            assert!(
                resolve(&env(&values), Path::new("/cwd"), Path::new("/bin/default"))
                    .unwrap_err()
                    .contains(message)
            );
        }
    }

    #[test]
    fn javascript_numeric_and_whitespace_profile_inputs() {
        // AgentLoopDeploymentProfile.ts:130-157 uses trim(), Number() and
        // Number.isInteger(); decimal Rust parsers reject valid JS inputs.
        for (text, port) in [
            ("1e3", 1000),
            ("0x10", 16),
            ("+42.0", 42),
            ("\u{feff}9\u{feff}", 9),
        ] {
            let profile = resolve(
                &env(&[
                    (TRANSPORT, "\u{feff}TCP\u{feff}"),
                    (TCP_PORT, text),
                    (CONNECT_TIMEOUT_MS, "0b1000"),
                ]),
                Path::new("/cwd"),
                Path::new("/bin/sidecar"),
            )
            .unwrap();
            assert_eq!(
                profile,
                Profile::Tcp {
                    host: "127.0.0.1".into(),
                    port,
                    connect_timeout: Some(Duration::from_millis(8)),
                }
            );
        }
        for text in ["NaN", "Infinity", "1.5", "0x10000", "\u{0085}9"] {
            assert!(required_port(Some(text)).is_err(), "{text:?}");
        }
        assert_eq!(optional_positive_integer(Some("1e12")).unwrap(), Some(1));
        assert_eq!(optional_positive_integer(Some("\u{feff}")).unwrap(), None);
        assert!(required_text("\u{feff}", "command").is_err());

        // trim() only decides whether a path was supplied; resolve() receives
        // the original filename, including leading/trailing spaces.
        let Profile::Stdio { args, .. } = resolve(
            &env(&[(TRANSPORT, "stdio"), (SIDECAR_PATH, " custom.mjs ")]),
            Path::new("/cwd"),
            Path::new("/bin/sidecar"),
        )
        .unwrap() else {
            panic!("stdio")
        };
        assert_eq!(args, ["/cwd/ custom.mjs "]);
        assert_eq!(
            resolve_path(Path::new("/cwd"), "a/../b"),
            Path::new("/cwd/b")
        );
    }

    #[test]
    fn leaves_factory_loading_to_the_selected_sidecar_runtime() {
        assert_eq!(
            resolve(
                &env(&[(FACTORY, "factory.mjs")]),
                Path::new("/cwd"),
                Path::new("/bin/sidecar")
            )
            .unwrap(),
            Profile::Native
        );

        let profile = resolve(
            &env(&[
                (TRANSPORT, "stdio"),
                (SIDECAR_COMMAND, "/usr/local/bin/node"),
                (SIDECAR_PATH, "sidecar.mjs"),
                (FACTORY, "factory.mjs"),
            ]),
            Path::new("/cwd"),
            Path::new("/bin/sidecar"),
        )
        .unwrap();
        let Profile::Stdio {
            args,
            env: child_env,
            ..
        } = profile
        else {
            panic!("stdio")
        };
        assert_eq!(args, ["/cwd/sidecar.mjs"]);
        assert_eq!(
            child_env.get(FACTORY).map(String::as_str),
            Some("factory.mjs")
        );

        let scratch = crate::scratch::Scratch::new("factory-packaged-sidecar");
        let sidecar = scratch
            .path()
            .join("dist/src/cli/pilotdeck-agent-loop-sidecar.js");
        std::fs::create_dir_all(sidecar.parent().unwrap()).unwrap();
        std::fs::write(&sidecar, "export {}\n").unwrap();
        assert_eq!(
            super::find_packaged_js_sidecar_from(scratch.path()).as_ref(),
            Some(&sidecar)
        );
        let Profile::Stdio {
            command,
            args,
            env: child_env,
            ..
        } = resolve(
            &env(&[
                (TRANSPORT, "stdio"),
                (FACTORY, "factory.mjs"),
                (
                    JS_SIDECAR_PATH,
                    "dist/src/cli/pilotdeck-agent-loop-sidecar.js",
                ),
            ]),
            scratch.path(),
            Path::new("/bin/sidecar"),
        )
        .unwrap() else {
            panic!("stdio")
        };
        assert_eq!(command, default_stdio_js_command());
        assert_eq!(args, [sidecar.to_string_lossy().to_string()]);
        assert_eq!(
            child_env.get(FACTORY).map(String::as_str),
            Some("factory.mjs")
        );
    }

    #[test]
    fn stdio_command_and_path_combinations_do_not_execute_the_wrong_runtime() {
        let default_sidecar = Path::new("/bin/pilotdeck-agent-loop-sidecar");

        let Profile::Stdio { command, args, .. } = resolve(
            &env(&[(TRANSPORT, "stdio"), (SIDECAR_PATH, "custom.mjs")]),
            Path::new("/cwd"),
            default_sidecar,
        )
        .unwrap() else {
            panic!("stdio")
        };
        assert_eq!(command, default_stdio_js_command());
        assert_eq!(args, ["/cwd/custom.mjs"]);

        let Profile::Stdio {
            command,
            args,
            env: child_env,
        } = resolve(
            &env(&[(TRANSPORT, "stdio"), (SIDECAR_COMMAND, "node")]),
            Path::new("/cwd"),
            default_sidecar,
        )
        .unwrap()
        else {
            panic!("stdio")
        };
        // `AgentLoopDeploymentProfile.ts:71` returns an explicit command verbatim.
        assert_eq!(command, Path::new("node"));
        assert_eq!(
            args,
            [super::compiled_module_js_sidecar_for_tests()
                .to_string_lossy()
                .into_owned()]
        );
        assert!(child_env.get(RUST_SIDECAR_PATH).is_none());
    }

    #[test]
    fn default_stdio_sidecar_matches_ts_compile_module_sibling() {
        for layout in [COMPILED_JS_SIDECAR, PACKAGED_JS_SIDECAR] {
            let (scratch, sidecar) = sidecar_fixture("default-stdio", layout);
            with_module_root(scratch.path(), || {
                let Profile::Stdio { command, args, .. } = resolve(
                    &env(&[(TRANSPORT, "stdio")]),
                    Path::new("/cwd"),
                    Path::new("/bin/pilotdeck-agent-loop-sidecar"),
                )
                .unwrap()
                else {
                    panic!("stdio")
                };
                assert_eq!(command, default_stdio_js_command());
                assert_eq!(
                    super::compiled_module_js_sidecar_for_tests(),
                    scratch.path().join(COMPILED_JS_SIDECAR)
                );
                assert_eq!(args, [sidecar.to_string_lossy().into_owned()]);
                assert!(
                    sidecar.is_file(),
                    "packaged sidecar must ship beside the deployment profile anchor"
                );
                let launcher = bundled_launcher(&sidecar).unwrap();
                assert_eq!(launcher, sidecar.with_file_name(SIDECAR_LAUNCHER));
                assert_eq!(fs::read(launcher).unwrap(), LAUNCHER_CONTENT);
            });
        }
    }

    #[test]
    fn stdio_factory_profile_uses_production_node_spawn_chain() {
        use std::process::{Command, Stdio};
        let scratch = crate::scratch::Scratch::new("sidecar-factory-chain");
        let sidecar_js = scratch
            .path()
            .join("dist/src/cli/pilotdeck-agent-loop-sidecar.js");
        std::fs::create_dir_all(sidecar_js.parent().unwrap()).unwrap();
        std::fs::write(
            scratch.path().join("factory.mjs"),
            "export default function createSidecarExecution() {}\n",
        )
        .unwrap();
        std::fs::write(
            &sidecar_js,
            r#"import { pathToFileURL } from "node:url";
import { resolve } from "node:path";
const factoryPath = process.env.PILOTDECK_AGENT_LOOP_FACTORY;
const loaded = await import(pathToFileURL(resolve(process.cwd(), factoryPath)).href);
process.exit(typeof loaded.default === "function" ? 0 : 2);"#,
        )
        .unwrap();
        let profile = resolve(
            &env(&[
                (TRANSPORT, "stdio"),
                (FACTORY, "factory.mjs"),
                (
                    JS_SIDECAR_PATH,
                    "dist/src/cli/pilotdeck-agent-loop-sidecar.js",
                ),
            ]),
            scratch.path(),
            Path::new("/bin/pilotdeck-agent-loop-sidecar"),
        )
        .unwrap();
        let Profile::Stdio { command, args, env } = profile else {
            panic!("stdio")
        };
        assert_eq!(command, default_stdio_js_command());
        assert_eq!(args, [sidecar_js.to_string_lossy().to_string()]);
        assert_eq!(env.get(FACTORY).map(String::as_str), Some("factory.mjs"));
        let output = Command::new(&command)
            .arg(&args[0])
            .current_dir(scratch.path())
            .envs(&env)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("spawn");
        assert!(output.success());
    }

    #[test]
    fn bundled_agent_module_execute_invokes_factory() {
        use std::process::{Command, Stdio};
        let (scratch, sidecar) = sidecar_fixture("sidecar-factory-exec", COMPILED_JS_SIDECAR);
        let factory = scratch.path().join("factory.mjs");
        std::fs::write(
            &factory,
            "import { writeFileSync } from 'node:fs';\n\
             export default async function createSidecarExecution(payload) {\
             writeFileSync(process.env.FACTORY_MARKER, JSON.stringify(payload));\
             return { loop: { run: async function*() { yield { type: 'tick' }; } }, input: {} };\
             }",
        )
        .unwrap();
        let Profile::Stdio { command, args, env } = with_module_root(scratch.path(), || {
            resolve(
                &env(&[(TRANSPORT, "stdio"), (FACTORY, factory.to_str().unwrap())]),
                scratch.path(),
                Path::new("/bin/pilotdeck-agent-loop-sidecar"),
            )
            .unwrap()
        }) else {
            panic!("stdio")
        };
        assert!(sidecar.is_file());
        assert_eq!(command, default_stdio_js_command());
        assert_eq!(args, [sidecar.to_string_lossy().into_owned()]);
        let request = concat!(
            r#"{"kind":"request","method":"execute","messageId":"m-exec","requestId":"r1","#,
            r#""runId":"run1","operationId":"op1","payload":{}}"#,
            "\n",
        );
        let marker = scratch.path().join("factory-called.json");
        let mut child = Command::new(&command)
            .args(&args)
            .envs(&env)
            .env("FACTORY_MARKER", &marker)
            .current_dir(scratch.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn sidecar");
        {
            use std::io::Write;
            let mut stdin = child.stdin.take().expect("stdin");
            stdin.write_all(request.as_bytes()).unwrap();
        }
        let output = child.wait_with_output().expect("sidecar output");
        assert!(
            output.status.success(),
            "sidecar failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(fs::read_to_string(marker).unwrap(), "{}");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            !stdout.contains("SIDEcar_FACTORY_ONLY"),
            "execute must call factory, stdout={stdout}"
        );
        assert!(
            stdout.contains("agent.tick") || stdout.contains("\"outcome\":\"success\""),
            "factory execution should stream or finish, stdout={stdout}"
        );
    }
}
