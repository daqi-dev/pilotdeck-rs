//! Source-built native installation updater. Installation policy is recorded
//! by the installer; the TypeScript repository is never a Rust release source.
#[cfg(unix)]
#[path = "update/install.rs"]
mod install;
#[cfg(unix)]
#[path = "update/process.rs"]
mod process;
#[cfg(unix)]
pub use install::{
    BINARIES, Builder, Engine, Generation, InstallManifest, Installation, NativeBuilder, bootstrap,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Release {
    pub tag_name: String,
    pub source_sha: String,
}
#[derive(Debug)]
pub struct Error {
    pub reason: String,
    pub message: String,
}
impl Error {
    pub fn new(reason: &str, message: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            message: message.into(),
        }
    }
    pub fn refused(reason: &str) -> Self {
        Self::new(reason, format!("{reason}: {}", describe(reason)))
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for Error {}

pub fn describe(reason: &str) -> &'static str {
    match reason {
        "development" => {
            "Developer branches/worktrees and development runtimes do not support self-update."
        }
        "localChanges" => "Local changes are present. Update this workspace manually.",
        "container" => "Container deployments must update their image manually.",
        "desktop" => "Use the desktop application settings to update this installation.",
        "notGit" => "Self-update requires a standard Git deployment.",
        "unofficial" => "This checkout does not use the official repository.",
        "shallow" => "Shallow Git checkouts do not support self-update.",
        "unknownVersion" => "The current build version cannot be identified.",
        "runtimeMismatch" => {
            "The running version does not match the checkout/build. Restart it before checking again."
        }
        "ahead" => {
            "This deployment is ahead of the latest Release. Automatic downgrades are disabled."
        }
        "diverged" => "This deployment has diverged from the Release. Update it manually.",
        "lockBusy" => "Another update or an unconfirmed cleanup holds the update lock.",
        "restartRequired" => "An update is already prepared. Restart the service first.",
        _ => "The Release update is unavailable. See settings or update manually.",
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Flags {
    pub check_only: bool,
    pub restart: bool,
}
impl Flags {
    pub fn parse(args: &[String]) -> Result<Self, Error> {
        let mut flags = Self::default();
        for arg in args {
            match arg.as_str() {
                "--check" => flags.check_only = true,
                "--restart" => flags.restart = true,
                _ => return Err(Error::new("usage", "Usage: update [--check] [--restart]")),
            }
        }
        Ok(flags)
    }
}
pub struct Status {
    pub reason: Option<String>,
    pub latest: Option<Release>,
    pub can_update: bool,
}

/// Backend owns source inspection and the atomic installation transaction.
pub trait Backend {
    fn check(&mut self) -> Result<Status, Error>;
    fn apply(&mut self, target: &Release, output: &mut dyn FnMut(&str)) -> Result<(), Error>;
    fn identity(&self) -> (&str, &str);
    fn target_generation(&self) -> Option<&str>;
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestartRequest {
    pub install_id: String,
    pub expected_generation: String,
    pub target_generation: String,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestartReply {
    pub status: String,
    pub restart_id: String,
    pub target_generation: String,
}
pub trait RuntimeClient {
    /// An authenticated same-installation context probe, performed before apply.
    /// Authentication, unhealthy service, or mismatched identity must be errors;
    /// only an actual refused connection may mean no running service.
    fn available(&mut self, install_id: &str, expected_generation: &str) -> Result<bool, Error>;
    fn restart(&mut self, request: &RestartRequest) -> Result<RestartReply, Error>;
}

/// Main-bin adapter; the coordinator wires the authenticated runtime client
/// from the native server's configured loopback address and server token.
#[cfg(unix)]
pub fn run_current(
    args: &[String],
    runtime: Option<&mut dyn RuntimeClient>,
) -> Result<u8, Error> {
    let flags = Flags::parse(args)?;
    if std::env::var_os("DOCKER").as_deref() == Some(std::ffi::OsStr::new("1"))
        || std::env::var_os("container").as_deref() == Some(std::ffi::OsStr::new("docker"))
        || std::path::Path::new("/.dockerenv").exists()
        || std::path::Path::new("/run/.containerenv").exists()
    {
        return Err(Error::refused("container"));
    }
    if std::env::var_os("PILOTDECK_DESKTOP").as_deref() == Some(std::ffi::OsStr::new("1"))
        || std::env::var_os("PILOTDECK_DESKTOP_VERSION").is_some_and(|s| !s.is_empty())
    {
        return Err(Error::refused("desktop"));
    }
    if std::env::var_os("PILOTDECK_RESTART_MODE").as_deref() == Some(std::ffi::OsStr::new("dev"))
        || std::env::var_os("NODE_ENV").as_deref() == Some(std::ffi::OsStr::new("development"))
    {
        return Err(Error::refused("development"));
    }
    let executable =
        std::env::current_exe().map_err(|e| Error::new("unknownVersion", e.to_string()))?;
    let installation = Installation::discover(&executable)?;
    let builder = NativeBuilder::new("git".into(), "cargo".into())?;
    let cancelled=builder.cancellation();
    execute_stdout(
        flags,
        &mut Engine::new(installation, builder),
        runtime,
        cancelled,
    )
}

#[cfg(unix)]
fn execute_stdout(flags:Flags,backend:&mut dyn Backend,runtime:Option<&mut dyn RuntimeClient>,cancelled:std::sync::Arc<std::sync::atomic::AtomicBool>)->Result<u8,Error>{
    // Keep the outer guard for the whole transaction: a nested Runner restores
    // NONBLOCK, so a full progress pipe cannot hang a subsequent status line.
    let output=process::Progress::new().map_err(|e|Error::new("output_error",e))?;
    let mut failure=None;
    let result={
        let mut line=|text:&str|{
            if failure.is_none(){
                let mut bytes=Vec::with_capacity(text.len()+1);bytes.extend_from_slice(text.as_bytes());bytes.push(b'\n');
                if let Err(error)=output.write(&bytes){cancelled.store(true,std::sync::atomic::Ordering::Relaxed);failure=Some(error);}
            }
        };
        execute(flags,backend,runtime,&mut line)
    };
    let restored=output.restore();
    if result.as_ref().is_err_and(|error|error.reason=="processStopFailed"){return result;}
    if let Some(error)=failure.or_else(||restored.err()){
        return Err(Error::new("output_error",format!("Update output failed: {error} The installation outcome may be unknown; inspect the installation before retrying.")));
    }
    result
}

pub fn execute(
    flags: Flags,
    backend: &mut dyn Backend,
    runtime: Option<&mut dyn RuntimeClient>,
    output: &mut dyn FnMut(&str),
) -> Result<u8, Error> {
    let status = backend.check()?;
    if status.reason.as_deref() == Some("upToDate") {
        output("Already at the latest Release.");
        return Ok(2);
    }
    if !status.can_update {
        return Err(Error::refused(
            status.reason.as_deref().unwrap_or("checkFailed"),
        ));
    }
    let target = status.latest.ok_or_else(|| {
        Error::new(
            "checkFailed",
            "The Release update is unavailable. See settings or update manually.",
        )
    })?;
    output(&format!(
        "Release available: {} ({})",
        target.tag_name,
        target.source_sha.get(..8).unwrap_or(&target.source_sha)
    ));
    if flags.check_only {
        return Ok(0);
    }
    let mut runtime = runtime;
    if flags.restart {
        let (id, expected) = backend.identity();
        let available = match runtime.as_mut() {
            Some(client) => client.available(id, expected)?,
            None => false,
        };
        if !available {
            return Err(Error::new(
                "restartUnavailable",
                "No running Web service was found. Run update without --restart, then start PilotDeck manually.",
            ));
        }
    }
    backend.apply(&target, output)?;
    if flags.restart {
        let (id, expected) = backend.identity();
        let generation=backend.target_generation().ok_or_else(||Error::new("restartFailed","Update prepared, but the installed generation could not be confirmed. Restart manually; do not repeat the update."))?;
        let request = RestartRequest {
            install_id: id.into(),
            expected_generation: expected.into(),
            target_generation: generation.into(),
        };
        let reply = runtime
            .as_mut()
            .expect("restart availability checked")
            .restart(&request);
        let accepted = reply.and_then(|reply| {
            if reply.status == "accepted"
                && !reply.restart_id.is_empty()
                && reply.target_generation == request.target_generation
            {
                Ok(())
            } else {
                Err(Error::new(
                    "restartUnconfirmed",
                    "The runtime did not accept the restart request. Restart manually.",
                ))
            }
        });
        if let Err(error) = accepted {
            return Err(Error::new(
                "restartFailed",
                format!(
                    "Update prepared, but restart was not confirmed: {} Restart manually; do not repeat the update.",
                    error.message
                ),
            ));
        }
        output("Update prepared; the running Web service accepted the restart request.");
    } else {
        output("Update prepared. Restart PilotDeck manually to load the new version.");
    }
    Ok(0)
}

#[cfg(test)]
#[path = "update/tests.rs"]
mod tests;
