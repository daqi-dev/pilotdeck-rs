//! Native per-process filesystem confinement, selected by the project profile.
//! Process argv is wrapped directly; the profile is never embedded in a shell
//! string. The constrained host provider is macOS Seatbelt. Other platforms
//! reject constrained commands, and full access never probes the provider.
use super::{ToolError, builtin::bash::runner::AbortSignal};
use crate::runtime::project::SandboxMode;
use pilotdeck_protocol::JsString;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::sync::OnceCell;

#[derive(Clone)]
pub struct NativeSandbox {
    mode: SandboxMode,
    temporary_root: PathBuf,
    available: Arc<OnceCell<bool>>,
}
impl NativeSandbox {
    pub fn new(mode: SandboxMode, temporary_root: PathBuf) -> Self {
        Self {
            mode,
            temporary_root,
            available: Arc::new(OnceCell::new()),
        }
    }
    pub fn mode(&self) -> SandboxMode {
        self.mode
    }
    pub fn temporary_root(&self) -> &Path {
        &self.temporary_root
    }
    fn unavailable(&self) -> ToolError {
        ToolError::new(
            "sandbox_unavailable",
            format!(
                "Sandbox mode {} is requested but no enforcing sandbox provider is available.",
                self.mode.as_str()
            )
            .as_str(),
        )
    }
    pub async fn prepare(
        &self,
        executable: JsString,
        args: Vec<JsString>,
        cwd: &JsString,
        signal: Option<&AbortSignal>,
    ) -> Result<(JsString, Vec<JsString>), ToolError> {
        if signal.is_some_and(AbortSignal::aborted) {
            return Err(ToolError::new(
                "permission_cancelled",
                "Sandbox preparation was cancelled.",
            ));
        }
        if self.mode == SandboxMode::DangerFullAccess {
            return Ok((executable, args));
        }
        let available = self.available.get_or_init(probe_seatbelt);
        let available = if let Some(signal) = signal {
            tokio::select! {
                _ = signal.wait() => return Err(ToolError::new("permission_cancelled", "Sandbox preparation was cancelled.")),
                value = available => *value,
            }
        } else {
            *available.await
        };
        if !available {
            return Err(self.unavailable());
        }
        let mut wrapped = vec![
            JsString::from("-p"),
            self.profile(Path::new(&String::from_utf16_lossy(cwd.units())))?
                .into(),
            "--".into(),
            executable,
        ];
        wrapped.extend(args);
        Ok(("/usr/bin/sandbox-exec".into(), wrapped))
    }
    fn roots(&self, workspace: &Path) -> Result<Vec<PathBuf>, ToolError> {
        if self.mode != SandboxMode::WorkspaceWrite {
            return Ok(vec![]);
        }
        let mut roots = Vec::new();
        for root in [workspace, Path::new("/tmp"), self.temporary_root.as_path()] {
            let absolute = absolute(root)?;
            let canonical = std::fs::canonicalize(&absolute).unwrap_or(absolute);
            if !roots.contains(&canonical) {
                roots.push(canonical);
            }
        }
        Ok(roots)
    }
    fn profile(&self, workspace: &Path) -> Result<String, ToolError> {
        let mut profile = String::from(
            "(version 1) (allow default) (deny file-write*) (allow file-write* (literal \"/dev/null\"))",
        );
        let roots = self.roots(workspace)?;
        if !roots.is_empty() {
            profile.push_str(" (allow file-write*");
            for root in roots {
                profile.push_str(" (subpath ");
                profile.push_str(&quote_profile(&root.to_string_lossy())?);
                profile.push(')');
            }
            profile.push(')');
        }
        Ok(profile)
    }
    /// Trusted-process file mutation fence, separate from the process sandbox.
    /// Check stable symlinks, including dangling targets, against the selected
    /// canonical root. Callers retain their original checkpoint/wire spelling;
    /// this is a trusted-process fence like SandboxFsPort, not a kernel sandbox
    /// for host code. Shell/task code uses the separate exact argv wrapper.
    pub fn check_write(&self, workspace: &Path, target: &Path) -> Result<PathBuf, ToolError> {
        if self.mode == SandboxMode::DangerFullAccess {
            return Ok(target.to_path_buf());
        }
        let denied = || {
            ToolError::new(
                "permission_denied",
                format!(
                    "Writing to {} is denied by the {} sandbox policy.",
                    target.display(),
                    self.mode.as_str()
                )
                .as_str(),
            )
        };
        if self.mode == SandboxMode::ReadOnly {
            return Err(denied());
        }
        let target = canonical_target(target).map_err(|_| denied())?;
        let root = std::fs::canonicalize(workspace).map_err(|_| denied())?;
        if target.starts_with(root) {
            Ok(target)
        } else {
            Err(denied())
        }
    }
}
fn absolute(path: &Path) -> Result<PathBuf, ToolError> {
    if path.as_os_str().as_encoded_bytes().contains(&0) {
        return Err(ToolError::new(
            "invalid_tool_input",
            "Sandbox paths cannot contain null bytes.",
        ));
    }
    Ok(if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| ToolError::failed(e.to_string().as_str()))?
            .join(path)
    })
}
fn canonical_target(path: &Path) -> std::io::Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    super::builtin::resolve_path_following_symlinks(&absolute)
}
fn quote_profile(value: &str) -> Result<String, ToolError> {
    if value.contains('\0') {
        return Err(ToolError::new(
            "invalid_tool_input",
            "Sandbox paths cannot contain null bytes.",
        ));
    }
    Ok(format!(
        "\"{}\"",
        value.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}
#[cfg(target_os = "macos")]
async fn probe_seatbelt() -> bool {
    use std::{process::Stdio, time::Duration};
    let mut command = tokio::process::Command::new("/usr/bin/sandbox-exec");
    command.args(["-p", "(version 1) (allow default) (deny file-write*) (allow file-write* (literal \"/dev/null\"))", "--", "/usr/bin/true"])
        .stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).kill_on_drop(true);
    let Ok(mut child) = command.spawn() else {
        return false;
    };
    match tokio::time::timeout(Duration::from_secs(5), child.wait()).await {
        Ok(Ok(status)) => status.success(),
        _ => {
            let _ = child.start_kill();
            let _ = tokio::time::timeout(Duration::from_secs(1), child.wait()).await;
            false
        }
    }
}
#[cfg(not(target_os = "macos"))]
async fn probe_seatbelt() -> bool {
    false
}

#[cfg(test)]
#[path = "sandbox_tests.rs"]
mod tests;
