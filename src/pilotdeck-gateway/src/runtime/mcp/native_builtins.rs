//! Declarative first-party builtin MCP specs. Only Playwright remains a Node SDK;
//! FunASR uses the same-generation Rust gateway's explicit stdio helper role.
use super::registry::{
    BuiltinMcpPaths, PILOTDECK_FUNASR_INSTALL_COMMAND_MARKER, PILOTDECK_FUNASR_RUNTIME_ROOT_MARKER,
    PILOTDECK_NODE_EXECUTABLE_MARKER, PILOTDECK_PLAYWRIGHT_MCP_ENTRYPOINT_MARKER,
    PILOTDECK_PROJECT_ROOT_MARKER,
};
use crate::{config::Env, metadata::object};
use pilotdeck_protocol::JsValue;
use std::{
    io,
    path::{Path, PathBuf},
};
fn utf8(path: &Path) -> io::Result<String> {
    path.to_str().map(str::to_owned).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "Builtin MCP executable path is not valid UTF-8",
        )
    })
}
/// A CLI/control process must never run itself with gateway-only helper flags.
/// The common installation generation ships the gateway as a real sibling.
pub fn sibling_gateway(executable: &Path) -> PathBuf {
    let executable = executable
        .canonicalize()
        .unwrap_or_else(|_| executable.to_owned());
    executable.with_file_name(if cfg!(windows) {
        "pilotdeck-gateway.exe"
    } else {
        "pilotdeck-gateway"
    })
}
/// Resolve the third-party Node runtime through an explicit host-provided
/// executable or PATH. An absent runtime stays a genuine spawn diagnostic.
pub fn node_from_path(env: &Env) -> PathBuf {
    let node = if cfg!(windows) { "node.exe" } else { "node" };
    if let Some(path) = env.get("PATH") {
        for directory in std::env::split_paths(path) {
            let candidate = directory.join(node);
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    PathBuf::from(node)
}
/// The diagnostic must name the installer shipped by the same source generation,
/// rather than an npm script in the user's unrelated project working directory.
pub(crate) fn install_command(env: &Env, executable: &Path, cwd: &Path) -> io::Result<String> {
    if let Some(command) = env
        .get("PILOTDECK_FUNASR_INSTALL_COMMAND")
        .filter(|value| !value.trim().is_empty())
    {
        return Ok(command.clone());
    }
    let executable = executable
        .canonicalize()
        .unwrap_or_else(|_| executable.to_owned());
    let parent = executable.parent().unwrap_or(cwd);
    let generation = parent.parent().unwrap_or(cwd);
    let mut candidates = vec![generation.join("source/scripts/install-asr.sh")];
    let is_native_source = |directory: &Path| {
        directory.join("src/Cargo.toml").is_file()
            && directory.join("src/pilotdeck-gateway/Cargo.toml").is_file()
    };
    for directory in parent
        .ancestors()
        .filter(|directory| is_native_source(directory))
    {
        candidates.push(directory.join("scripts/install-asr.sh"));
    }
    if is_native_source(cwd) {
        candidates.push(cwd.join("scripts/install-asr.sh"));
    }
    let Some(script) = candidates.iter().find(|path| path.is_file()) else {
        // Absence is diagnosed by the optional tool, not a fictitious shell command.
        return Ok(String::new());
    };
    // POSIX single-quote escaping also protects spaces, dollar signs and backticks.
    Ok(format!("sh '{}'", utf8(script)?.replace('\'', "'\"'\"'")))
}
/// Runtime-root is the installed third-party SDK package root. The fallback
/// probes equivalent generation/source/cwd node_modules layouts, never the
/// retired first-party Gateway or FunASR JS implementation.
pub fn builtin_paths(env: &Env, executable: &Path, cwd: &Path) -> io::Result<BuiltinMcpPaths> {
    let executable = executable
        .canonicalize()
        .unwrap_or_else(|_| executable.to_owned());
    let parent = executable.parent().unwrap_or(cwd);
    let sdk = Path::new("node_modules/@playwright/mcp/cli.js");
    let mut candidates = Vec::new();
    if let Some(root) = env.get("PILOTDECK_RUNTIME_ROOT").filter(|s| !s.is_empty()) {
        candidates.push(PathBuf::from(root).join(sdk));
    }
    candidates.push(parent.join(sdk));
    if let Some(root) = parent.parent() {
        candidates.push(root.join(sdk));
    }
    candidates.push(cwd.join(sdk));
    let entry = candidates
        .iter()
        .find(|path| path.exists())
        .unwrap_or(&candidates[0]);
    Ok(BuiltinMcpPaths {
        node_executable: utf8(&node_from_path(env))?,
        playwright_mcp_entrypoint: utf8(entry)?,
        // Native FunASR declaration below has no entrypoint marker and cannot route
        // through this legacy compatibility field. User plugin marker expansion is
        // retained by the existing registry; no internal JS fallback is configured.
        funasr_mcp_entrypoint: String::new(),
        install_command: install_command(env, &executable, cwd)?,
    })
}
pub fn browser_servers() -> JsValue {
    object([(
        "browser-use",
        object([
            ("command", PILOTDECK_NODE_EXECUTABLE_MARKER.into()),
            (
                "args",
                JsValue::Array(vec![
                    PILOTDECK_PLAYWRIGHT_MCP_ENTRYPOINT_MARKER.into(),
                    "--browser".into(),
                    "chromium".into(),
                    "--allow-unrestricted-file-access".into(),
                ]),
            ),
            ("perSession", JsValue::Bool(true)),
        ]),
    )])
}
pub fn funasr_servers(executable: &Path) -> io::Result<JsValue> {
    let gateway = utf8(&sibling_gateway(executable))?;
    Ok(object([(
        "funasr",
        object([
            ("command", gateway.as_str().into()),
            (
                "args",
                JsValue::Array(vec![
                    "--builtin-mcp".into(),
                    "funasr".into(),
                    "--project-root".into(),
                    PILOTDECK_PROJECT_ROOT_MARKER.into(),
                    "--runtime-root".into(),
                    PILOTDECK_FUNASR_RUNTIME_ROOT_MARKER.into(),
                    "--install-command".into(),
                    PILOTDECK_FUNASR_INSTALL_COMMAND_MARKER.into(),
                ]),
            ),
            ("perSession", JsValue::Bool(true)),
            ("callTimeoutMs", JsValue::Number(300000.0)),
        ]),
    )]))
}
#[cfg(test)]
#[path = "native_builtins_tests.rs"]
mod tests;
