//! Offline Web Release service used when the packaged JavaScript updater is absent.
//!
//! This is the local service half of `webUpdateService.js`: release discovery,
//! deployment eligibility, staged builds, artifact replacement and rollback.
//! Restart ownership stays with a running Web service, so the offline backend
//! deliberately reports that automatic restart is unavailable.
use super::update_service::{UpdateBackend, UpdateCommandError, UpdateStatus, UpdateTarget};
use bytes::Bytes;
use http_body_util::{BodyExt, Empty};
use hyper::{Request, header};
use hyper_util::{client::legacy::Client, rt::TokioExecutor};
use regex::Regex;
use serde_json::Value;
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

const REPOSITORY: &str = "OpenBMB/PilotDeck";
const REPOSITORY_URL: &str = "https://github.com/OpenBMB/PilotDeck.git";
const ARTIFACTS: [&str; 4] = ["node_modules", "ui/node_modules", "dist", "ui/dist"];
const COMMAND_TIMEOUT: Duration = Duration::from_secs(60);
const BUILD_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const MAX_COMMAND_OUTPUT: usize = 10 * 1024 * 1024;
const MAX_MANIFEST: usize = 4 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
struct BuildVersion {
    source_sha: String,
    clean: bool,
}

#[derive(Debug)]
struct ProcessOutput {
    code: i32,
    stdout: String,
}

trait ReleaseRuntime {
    fn latest(&self) -> Result<UpdateTarget, UpdateCommandError>;
    fn run(
        &self,
        program: &str,
        args: &[String],
        cwd: &Path,
        timeout: Duration,
        capture_stdout: bool,
        output: &mut dyn FnMut(&str),
    ) -> Result<ProcessOutput, UpdateCommandError>;
}

struct SystemReleaseRuntime {
    env: HashMap<String, String>,
}

impl ReleaseRuntime for SystemReleaseRuntime {
    fn latest(&self) -> Result<UpdateTarget, UpdateCommandError> {
        latest_release()
    }

    fn run(
        &self,
        program: &str,
        args: &[String],
        cwd: &Path,
        timeout: Duration,
        capture_stdout: bool,
        output: &mut dyn FnMut(&str),
    ) -> Result<ProcessOutput, UpdateCommandError> {
        run_process(
            program,
            args,
            cwd,
            timeout,
            capture_stdout,
            output,
            &self.env,
        )
    }
}

/// Production backend for CLI update and IM update checks outside a running Web service.
pub struct NativeUpdateBackend {
    project_root: Option<PathBuf>,
    env: HashMap<String, String>,
    running: Option<BuildVersion>,
    runtime: Box<dyn ReleaseRuntime>,
    online: Option<super::update_web_client::OnlineUpdateClient>,
    owns_update_lock: bool,
    needs_restart: bool,
}

impl NativeUpdateBackend {
    pub fn from_current_exe() -> Result<Self, UpdateCommandError> {
        let executable = std::env::current_exe().map_err(|error| {
            UpdateCommandError::new(
                "notGit",
                format!("Could not locate this installation: {error}"),
            )
        })?;
        let start = executable.parent().unwrap_or(&executable);
        let git_root = find_git_root(start);
        let install_root = super::update::find_installation_root(start);
        let env: HashMap<String, String> = std::env::vars().collect();
        let backend = Self::new(git_root, env);
        let mut online = super::update_web_client::try_online_client(
            &backend.env,
            install_root.as_deref(),
        )?;
        if online.is_none() {
            if let Err(error) = ensure_offline_service(&backend.env, install_root.clone()) {
                online = super::update_web_client::try_online_client(
                    &backend.env,
                    install_root.as_deref(),
                )?;
                if online.is_none() {
                    return Err(error);
                }
            }
        }
        Ok(Self {
            online,
            ..backend
        })
    }

    pub fn new(project_root: Option<PathBuf>, env: HashMap<String, String>) -> Self {
        let running = project_root
            .as_deref()
            .and_then(read_web_build_version);
        Self {
            project_root,
            runtime: Box::new(SystemReleaseRuntime { env: env.clone() }),
            env,
            running,
            online: None,
            owns_update_lock: false,
            needs_restart: false,
        }
    }

    #[cfg(test)]
    fn injected(
        project_root: PathBuf,
        env: HashMap<String, String>,
        runtime: Box<dyn ReleaseRuntime>,
    ) -> Self {
        let running = read_web_build_version(&project_root);
        Self {
            project_root: Some(project_root),
            env,
            running,
            runtime,
            online: None,
            owns_update_lock: false,
            needs_restart: false,
        }
    }

    fn root(&self) -> Result<&Path, UpdateCommandError> {
        self.project_root
            .as_deref()
            .ok_or_else(|| failure("notGit", "Self-update requires a standard Git deployment."))
    }

    fn git(&self, args: &[&str], cwd: &Path) -> Result<ProcessOutput, UpdateCommandError> {
        let args = args
            .iter()
            .map(|value| (*value).to_owned())
            .collect::<Vec<_>>();
        self.runtime
            .run("git", &args, cwd, COMMAND_TIMEOUT, true, &mut |_| {})
    }

    fn git_required(&self, args: &[&str], cwd: &Path) -> Result<String, UpdateCommandError> {
        let result = self.git(args, cwd)?;
        if result.code == 0 {
            Ok(result.stdout.trim().to_owned())
        } else {
            Err(failure(
                "checkFailed",
                format!("git {} failed ({}).", args.join(" "), result.code),
            ))
        }
    }

    fn ancestor(&self, from: &str, to: &str, root: &Path) -> Result<bool, UpdateCommandError> {
        let result = self.git(&["merge-base", "--is-ancestor", from, to], root)?;
        match result.code {
            0 => Ok(true),
            1 => Ok(false),
            code => Err(failure(
                "checkFailed",
                format!("git merge-base failed ({code})."),
            )),
        }
    }

    fn inspect_workspace(&self) -> Result<String, UpdateCommandError> {
        if self.env.get("DOCKER").is_some_and(|value| value == "1")
            || self
                .env
                .get("container")
                .is_some_and(|value| value == "docker")
            || Path::new("/.dockerenv").exists()
            || Path::new("/run/.containerenv").exists()
        {
            return Err(failure(
                "container",
                "Container deployments must update their image manually.",
            ));
        }
        if self
            .env
            .get("PILOTDECK_DESKTOP")
            .is_some_and(|value| value == "1")
            || self
                .env
                .get("PILOTDECK_DESKTOP_VERSION")
                .is_some_and(|value| !value.is_empty())
        {
            return Err(failure(
                "desktop",
                "Use the desktop application settings to update this installation.",
            ));
        }
        if self
            .env
            .get("PILOTDECK_RESTART_MODE")
            .is_some_and(|value| value == "dev")
            || self
                .env
                .get("NODE_ENV")
                .is_some_and(|value| value == "development")
        {
            return Err(development());
        }
        let root = self.root()?;
        let dot_git = root.join(".git");
        if !dot_git.is_dir() {
            return Err(if dot_git.exists() {
                development()
            } else {
                failure("notGit", "Self-update requires a standard Git deployment.")
            });
        }
        let actual = self.git_required(&["rev-parse", "--show-toplevel"], root)?;
        let actual = fs::canonicalize(actual)
            .map_err(|_| failure("notGit", "Self-update requires a standard Git deployment."))?;
        let expected = fs::canonicalize(root)
            .map_err(|_| failure("notGit", "Self-update requires a standard Git deployment."))?;
        if actual != expected {
            return Err(failure(
                "notGit",
                "Self-update requires a standard Git deployment.",
            ));
        }
        if !self.owns_update_lock && dot_git.join("pilotdeck-update.lock").exists() {
            return Err(failure(
                "lockBusy",
                "Another update or an unconfirmed cleanup holds the update lock.",
            ));
        }
        if self.git_required(&["branch", "--show-current"], root)? != "main" {
            return Err(development());
        }
        if !self
            .git_required(&["status", "--porcelain", "--untracked-files=all"], root)?
            .is_empty()
        {
            return Err(failure(
                "localChanges",
                "Local changes are present. Update this workspace manually.",
            ));
        }
        for state in [
            "MERGE_HEAD",
            "CHERRY_PICK_HEAD",
            "REVERT_HEAD",
            "BISECT_LOG",
            "rebase-merge",
            "rebase-apply",
        ] {
            if dot_git.join(state).exists() {
                return Err(development());
            }
        }
        if self.git_required(&["rev-parse", "--is-shallow-repository"], root)? == "true" {
            return Err(failure(
                "shallow",
                "Shallow Git checkouts do not support self-update.",
            ));
        }
        let remotes = self.git_required(&["remote"], root)?;
        let mut official = false;
        for remote in remotes.lines().filter(|line| !line.is_empty()) {
            let url = self.git_required(&["remote", "get-url", remote], root)?;
            official |= matches_official_repository(&url);
        }
        if !official {
            return Err(failure(
                "unofficial",
                "This checkout does not use the official repository.",
            ));
        }
        let running = self.running.as_ref().ok_or_else(|| {
            failure(
                "unknownVersion",
                "The current build version cannot be identified.",
            )
        })?;
        if !running.clean {
            return Err(failure(
                "localChanges",
                "Local changes are present. Update this workspace manually.",
            ));
        }
        let head = self.git_required(&["rev-parse", "HEAD"], root)?;
        if head != running.source_sha {
            return Err(runtime_mismatch());
        }
        let built = read_web_build_version(root).ok_or_else(runtime_mismatch)?;
        if !built.clean || built.source_sha != running.source_sha {
            return Err(runtime_mismatch());
        }
        Ok(head)
    }

    fn checked_status(&self) -> Result<UpdateStatus, UpdateCommandError> {
        let root = self.root()?;
        let head = self.inspect_workspace()?;
        let latest = self.runtime.latest()?;
        validate_target(&latest)?;
        let reference = format!("refs/pilotdeck/releases/{}", latest.tag_name);
        let fetch = format!("refs/tags/{}:{reference}", latest.tag_name);
        self.git_required(&["fetch", "--no-tags", REPOSITORY_URL, &fetch], root)?;
        let commit_ref = format!("{reference}^{{commit}}");
        let release_commit = self.git_required(&["rev-parse", &commit_ref], root)?;
        if release_commit != latest.source_sha {
            return Err(failure(
                "releaseMismatch",
                "The Release manifest does not match its Git tag.",
            ));
        }
        if head == release_commit {
            return Ok(UpdateStatus {
                can_update: false,
                reason: Some("upToDate".into()),
                latest: Some(latest),
            });
        }
        if self.ancestor(&head, &release_commit, root)? {
            return Ok(UpdateStatus {
                can_update: true,
                reason: None,
                latest: Some(latest),
            });
        }
        Ok(UpdateStatus {
            can_update: false,
            reason: Some(
                if self.ancestor(&release_commit, &head, root)? {
                    "ahead"
                } else {
                    "diverged"
                }
                .into(),
            ),
            latest: Some(latest),
        })
    }

    fn build(&self, source: &Path, output: &mut dyn FnMut(&str)) -> Result<(), UpdateCommandError> {
        for args in [
            vec![
                "install".into(),
                "--frozen-lockfile".into(),
                "--filter".into(),
                "pilotdeck".into(),
                "--filter".into(),
                "pilotdeck-ui".into(),
            ],
            vec!["run".into(), "build:web".into()],
        ] {
            let result = self
                .runtime
                .run("pnpm", &args, source, BUILD_TIMEOUT, false, output)?;
            if result.code != 0 {
                return Err(failure(
                    "buildFailed",
                    format!("pnpm failed ({}).", result.code),
                ));
            }
        }
        Ok(())
    }

    fn apply_release(
        &mut self,
        target: &UpdateTarget,
        output: &mut dyn FnMut(&str),
    ) -> Result<(), UpdateCommandError> {
        validate_target(target)?;
        if self.needs_restart {
            return Err(failure(
                "restartRequired",
                "An update is already prepared. Restart the service first.",
            ));
        }
        self.inspect_workspace()?;
        let root = self.root()?.to_path_buf();
        let git_dir = root.join(".git");
        let lock = git_dir.join("pilotdeck-update.lock");
        match fs::create_dir(&lock) {
            Ok(()) => self.owns_update_lock = true,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                return Err(failure(
                    "lockBusy",
                    "Another update or an unconfirmed cleanup holds the update lock.",
                ));
            }
            Err(error) => return Err(failure("applyFailed", error.to_string())),
        }
        let staging = git_dir.join(format!("pilotdeck-update-{}", uuid::Uuid::new_v4()));
        let mut preserve = false;
        let result = self.apply_locked(&root, &staging, target, output, &mut preserve);
        if result
            .as_ref()
            .is_err_and(|error| error.reason == "processStopFailed")
        {
            preserve = true;
        }
        if !preserve {
            let _ = fs::remove_dir_all(&staging);
            let _ = fs::remove_dir(&lock);
        }
        self.owns_update_lock = false;
        if result.is_ok() {
            self.needs_restart = true;
        }
        result
    }

    fn apply_locked(
        &mut self,
        root: &Path,
        staging: &Path,
        target: &UpdateTarget,
        output: &mut dyn FnMut(&str),
        preserve: &mut bool,
    ) -> Result<(), UpdateCommandError> {
        let status = self.checked_status()?;
        if !status.can_update {
            let reason = status.reason.as_deref().unwrap_or("checkFailed");
            return Err(failure(
                reason,
                super::update_service::describe_reason(reason),
            ));
        }
        if status.latest.as_ref() != Some(target) {
            return Err(failure("targetChanged", "The selected Release changed."));
        }
        fs::create_dir(staging).map_err(|error| failure("applyFailed", error.to_string()))?;
        output("Preparing the published release in a temporary directory.");
        let source = staging.join("source");
        let root_text = root.to_string_lossy().into_owned();
        let source_text = source.to_string_lossy().into_owned();
        self.git_required(
            &[
                "clone",
                "--shared",
                "--no-checkout",
                &root_text,
                &source_text,
            ],
            root,
        )?;
        self.git_required(&["checkout", "--detach", &target.source_sha], &source)?;
        self.build(&source, output)?;
        let prepared = read_web_build_version(&source)
            .ok_or_else(|| failure("buildFailed", "Prepared build does not match the release."))?;
        if !prepared.clean || prepared.source_sha != target.source_sha {
            return Err(failure(
                "buildFailed",
                "Prepared build does not match the release.",
            ));
        }
        if !self
            .git_required(&["status", "--porcelain", "--untracked-files=all"], &source)?
            .is_empty()
        {
            return Err(failure("buildFailed", "Build modified the release source."));
        }
        for artifact in ARTIFACTS {
            if !source.join(artifact).exists() {
                return Err(failure(
                    "buildFailed",
                    format!("Missing build output: {artifact}"),
                ));
            }
        }
        for directory in ["dist", "ui/dist"] {
            let path = source.join(directory).join("web-build.json");
            let mut metadata: Value = serde_json::from_slice(
                &fs::read(&path).map_err(|error| failure("buildFailed", error.to_string()))?,
            )
            .map_err(|_| failure("buildFailed", "Invalid build metadata."))?;
            metadata["tagName"] = Value::String(target.tag_name.clone());
            fs::write(&path, format!("{}\n", metadata))
                .map_err(|error| failure("buildFailed", error.to_string()))?;
        }
        relocate_dependencies(&source, root)?;
        self.inspect_workspace()?;
        output("Build succeeded. Applying the prepared release.");
        let mut moved = Vec::new();
        let install = (|| {
            for artifact in ARTIFACTS {
                let destination = root.join(artifact);
                let backup = staging.join("backup").join(artifact);
                if let Some(parent) = backup.parent() {
                    fs::create_dir_all(parent)
                        .map_err(|error| failure("applyFailed", error.to_string()))?;
                }
                let existed = destination.exists();
                if existed {
                    fs::rename(&destination, &backup)
                        .map_err(|error| failure("applyFailed", error.to_string()))?;
                }
                moved.push(MovedArtifact {
                    destination: destination.clone(),
                    backup,
                    existed,
                    installed: false,
                });
                fs::rename(source.join(artifact), &destination)
                    .map_err(|error| failure("applyFailed", error.to_string()))?;
                moved.last_mut().expect("moved artifact").installed = true;
            }
            let running = self.running.as_ref().ok_or_else(runtime_mismatch)?;
            if self.git_required(&["branch", "--show-current"], root)? != "main"
                || self.git_required(&["rev-parse", "HEAD"], root)? != running.source_sha
                || !self
                    .git_required(&["status", "--porcelain", "--untracked-files=all"], root)?
                    .is_empty()
            {
                return Err(failure(
                    "workspaceChanged",
                    "The deployment changed during the update.",
                ));
            }
            let hooks = staging
                .join("disabled-hooks")
                .to_string_lossy()
                .into_owned();
            let hook_arg = format!("core.hooksPath={hooks}");
            self.git_required(
                &[
                    "-c",
                    &hook_arg,
                    "merge",
                    "--ff-only",
                    "--no-edit",
                    &target.source_sha,
                ],
                root,
            )?;
            Ok(())
        })();
        if let Err(error) = install {
            if let Err(restore) = restore_artifacts(&mut moved) {
                *preserve = true;
                return Err(failure(
                    "manualRecovery",
                    format!(
                        "Restore failed. Backups retained at {}: {restore}",
                        staging.display()
                    ),
                ));
            }
            return Err(error);
        }
        Ok(())
    }
}

impl UpdateBackend for NativeUpdateBackend {
    fn check(&mut self) -> Result<UpdateStatus, UpdateCommandError> {
        if let Some(client) = &self.online {
            return client.check();
        }
        match self.checked_status() {
            Ok(status) => Ok(status),
            Err(error) => Ok(UpdateStatus {
                can_update: false,
                reason: Some(error.reason),
                latest: None,
            }),
        }
    }

    fn can_restart(&self) -> bool {
        self.online.is_some()
    }

    fn apply(
        &mut self,
        target: &UpdateTarget,
        output: &mut dyn FnMut(&str),
    ) -> Result<(), UpdateCommandError> {
        if let Some(client) = &self.online {
            return client.apply(target, output);
        }
        self.apply_release(target, output)
    }

    fn restart(&mut self) -> Result<(), UpdateCommandError> {
        if let Some(client) = &self.online {
            return client.restart();
        }
        Err(failure(
            "restartUnavailable",
            "No running Web service was found.",
        ))
    }
}

struct MovedArtifact {
    destination: PathBuf,
    backup: PathBuf,
    existed: bool,
    installed: bool,
}

fn restore_artifacts(moved: &mut [MovedArtifact]) -> io::Result<()> {
    for item in moved.iter().rev() {
        if item.installed {
            remove_path(&item.destination)?;
        }
        if item.existed {
            fs::rename(&item.backup, &item.destination)?;
        }
    }
    Ok(())
}

fn remove_path(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            fs::remove_dir_all(path)
        }
        Ok(_) => fs::remove_file(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn read_web_build_version(root: &Path) -> Option<BuildVersion> {
    let read = |directory: &str| -> Option<Value> {
        serde_json::from_slice(&fs::read(root.join(directory).join("web-build.json")).ok()?).ok()
    };
    let gateway = read("dist")?;
    let ui = read("ui/dist")?;
    let source_sha = gateway.get("sourceSha")?.as_str()?;
    if !commit_sha(source_sha) || ui.get("sourceSha")?.as_str()? != source_sha {
        return None;
    }
    Some(BuildVersion {
        source_sha: source_sha.into(),
        clean: gateway.get("clean").and_then(Value::as_bool) == Some(true)
            && ui.get("clean").and_then(Value::as_bool) == Some(true),
    })
}

fn validate_target(target: &UpdateTarget) -> Result<(), UpdateCommandError> {
    if release_tag_syntax(&target.tag_name) && commit_sha(&target.source_sha) {
        Ok(())
    } else {
        Err(failure("targetChanged", "The selected Release is invalid."))
    }
}

fn release_tag_syntax(tag: &str) -> bool {
    Regex::new(r"^v[0-9]{4}\.[0-9]{2}\.[0-9]{2}(?:-r[1-9][0-9]*)?$")
        .unwrap()
        .is_match(tag)
}

fn valid_release_tag(tag: &str) -> bool {
    let pattern = Regex::new(r"^v([0-9]{4})\.([0-9]{2})\.([0-9]{2})(?:-r([1-9][0-9]*))?$").unwrap();
    let Some(captures) = pattern.captures(tag) else {
        return false;
    };
    let year = captures[1].parse::<u32>().unwrap_or(0);
    // Date.UTC treats years 0..99 as 1900..1999.
    let year = if year < 100 { year + 1900 } else { year };
    if captures.get(4).is_some_and(|revision| {
        revision
            .as_str()
            .parse::<u64>()
            .map_or(true, |value| value > 9_007_199_254_740_991)
    }) {
        return false;
    }
    let month = captures[2].parse::<u32>().unwrap_or(0);
    let day = captures[3].parse::<u32>().unwrap_or(0);
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year.is_multiple_of(400) || year.is_multiple_of(4) && !year.is_multiple_of(100) => 29,
        2 => 28,
        _ => 0,
    };
    day > 0 && day <= days
}

fn commit_sha(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn matches_official_repository(remote: &str) -> bool {
    remote == REPOSITORY_URL
        || Regex::new(
            r"(?i)^(?:https://github\.com/|git@github\.com:|ssh://git@github\.com/)OpenBMB/PilotDeck(?:\.git)?/?$",
        )
        .unwrap()
        .is_match(remote)
}

pub(crate) fn find_git_root(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|current| current.join(".git").exists())
        .map(Path::to_path_buf)
}

fn failure(reason: impl Into<String>, message: impl Into<String>) -> UpdateCommandError {
    UpdateCommandError::new(reason, message)
}

fn development() -> UpdateCommandError {
    failure(
        "development",
        "Developer branches/worktrees and development runtimes do not support self-update.",
    )
}

fn runtime_mismatch() -> UpdateCommandError {
    failure(
        "runtimeMismatch",
        "The running version does not match the checkout/build. Restart it before checking again.",
    )
}

// webUpdateCommand.js:48-56 only permits offline application after an
// ECONNREFUSED health check. A healthy port continues into localHeaders + /api/update/*.
fn ensure_offline_service(
    env: &HashMap<String, String>,
    install_root: Option<PathBuf>,
) -> Result<(), UpdateCommandError> {
    let port_text = env
        .get("SERVER_PORT")
        .filter(|value| !value.is_empty())
        .map(String::as_str)
        .unwrap_or("3001");
    let port = pilotdeck_protocol::js_value::string_to_number(port_text);
    if !port.is_finite() || port.fract() != 0.0 || !(1.0..=65_535.0).contains(&port) {
        return Err(failure(
            "invalidPort",
            "SERVER_PORT must identify the local Web service.",
        ));
    }
    let port = port as u16;
    let env = env.clone();
    thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| failure("runtimeUnavailable", error.to_string()))?;
        runtime.block_on(probe_offline_service(port, &env, install_root))
    })
    .join()
    .map_err(|_| {
        failure(
            "runtimeUnavailable",
            "Web health worker stopped unexpectedly.",
        )
    })?
}

async fn probe_offline_service(
    port: u16,
    _env: &HashMap<String, String>,
    _install_root: Option<PathBuf>,
) -> Result<(), UpdateCommandError> {
    let result = tokio::time::timeout(Duration::from_secs(3), async {
        let client: Client<hyper_util::client::legacy::connect::HttpConnector, Empty<Bytes>> =
            Client::builder(TokioExecutor::new()).build_http();
        let request = Request::builder().method("GET")
            .uri(format!("http://127.0.0.1:{port}/health"))
            .body(Empty::new()).map_err(|_| failure("runtimeUnavailable", "Invalid Web health request."))?;
        match client.request(request).await {
            Err(error) if connection_refused(&error) => Ok(()),
            Err(_) => Err(failure("runtimeUnavailable", "The configured Web port could not be checked. Check SERVER_PORT.")),
            Ok(response) if !response.status().is_success() => Err(failure("runtimeUnavailable", "The configured Web port is not healthy. Check SERVER_PORT.")),
            Ok(_) => Err(failure(
                "runtimeUnavailable",
                "The configured Web port is healthy. Check SERVER_PORT.",
            )),
        }
    }).await;
    result.map_err(|_| {
        failure(
            "runtimeUnavailable",
            "The configured Web port did not answer. Check SERVER_PORT.",
        )
    })?
}

fn connection_refused(error: &(dyn std::error::Error + 'static)) -> bool {
    let mut current = Some(error);
    while let Some(error) = current {
        if error
            .downcast_ref::<io::Error>()
            .is_some_and(|error| error.kind() == io::ErrorKind::ConnectionRefused)
        {
            return true;
        }
        current = error.source();
    }
    false
}

fn latest_release() -> Result<UpdateTarget, UpdateCommandError> {
    let url = format!("https://github.com/{REPOSITORY}/releases/latest/download/release.json");
    let manifest = thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| failure("checkFailed", error.to_string()))?;
        runtime.block_on(fetch_json(&url))
    })
    .join()
    .map_err(|_| {
        failure(
            "checkFailed",
            "Release request worker stopped unexpectedly.",
        )
    })??;
    validate_manifest(&manifest)
}

async fn fetch_json(url: &str) -> Result<Value, UpdateCommandError> {
    tokio::time::timeout(Duration::from_secs(15), fetch_json_inner(url))
        .await
        .map_err(|_| failure("checkFailed", "Release request timed out."))?
}

async fn fetch_json_inner(url: &str) -> Result<Value, UpdateCommandError> {
    let connector = hyper_rustls::HttpsConnectorBuilder::new()
        .with_webpki_roots()
        .https_only()
        .enable_http1()
        .build();
    let client: Client<_, Empty<Bytes>> = Client::builder(TokioExecutor::new()).build(connector);
    let mut url =
        url::Url::parse(url).map_err(|_| failure("checkFailed", "Invalid Release URL."))?;
    for _ in 0..6 {
        let request = Request::builder()
            .method("GET")
            .uri(url.as_str())
            .header(header::ACCEPT, "application/json")
            .header(header::USER_AGENT, "PilotDeck-Updater")
            .body(Empty::new())
            .map_err(|_| failure("checkFailed", "Invalid Release request."))?;
        let response = tokio::time::timeout(Duration::from_secs(15), client.request(request))
            .await
            .map_err(|_| failure("checkFailed", "Release request timed out."))?
            .map_err(|_| failure("checkFailed", "Release request failed."))?;
        if response.status().is_redirection() {
            let location = response
                .headers()
                .get(header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| failure("checkFailed", "Release redirect is invalid."))?;
            url = url
                .join(location)
                .map_err(|_| failure("checkFailed", "Release redirect is invalid."))?;
            if url.scheme() != "https" {
                return Err(failure("checkFailed", "Release redirect is not HTTPS."));
            }
            continue;
        }
        if !response.status().is_success() {
            return Err(failure(
                "checkFailed",
                format!("Release request failed ({}).", response.status().as_u16()),
            ));
        }
        let bytes = http_body_util::Limited::new(response.into_body(), MAX_MANIFEST)
            .collect()
            .await
            .map_err(|_| failure("checkFailed", "Release response was interrupted."))?
            .to_bytes();
        if bytes.len() > MAX_MANIFEST {
            return Err(failure("checkFailed", "Release manifest is too large."));
        }
        return serde_json::from_slice(&bytes)
            .map_err(|_| failure("releaseUnavailable", "Release manifest is invalid."));
    }
    Err(failure("checkFailed", "Too many Release redirects."))
}

fn validate_manifest(manifest: &Value) -> Result<UpdateTarget, UpdateCommandError> {
    let tag = manifest.get("tag").and_then(Value::as_str).unwrap_or("");
    let source_sha = manifest
        .get("sourceSha")
        .and_then(Value::as_str)
        .unwrap_or("");
    let version = manifest
        .get("version")
        .and_then(Value::as_str)
        .unwrap_or("");
    let date = manifest.get("date").and_then(Value::as_str).unwrap_or("");
    let expected_version = release_version(tag).ok_or_else(|| {
        failure(
            "releaseUnavailable",
            "Release manifest does not match the published release.",
        )
    })?;
    let expected_date = tag.get(1..11).unwrap_or("").replace('.', "-");
    let assets = manifest.get("assets").and_then(Value::as_array);
    if manifest.get("schemaVersion").and_then(Value::as_f64) != Some(1.0)
        || manifest.get("repository").and_then(Value::as_str) != Some(REPOSITORY)
        || !commit_sha(source_sha)
        || version != expected_version
        || date != expected_date
        || assets.is_none_or(Vec::is_empty)
    {
        return Err(failure(
            "releaseUnavailable",
            "Release manifest does not match the published release.",
        ));
    }
    let mut names = HashSet::new();
    for asset in assets.into_iter().flatten() {
        let name = asset.get("name").and_then(Value::as_str).unwrap_or("");
        let sha256 = asset.get("sha256").and_then(Value::as_str).unwrap_or("");
        let sha512 = asset.get("sha512").and_then(Value::as_str).unwrap_or("");
        let safe_name = !name.is_empty()
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
            && !name.bytes().all(|byte| byte == b'.');
        if !safe_name
            || !names.insert(name)
            || sha256.len() != 64
            || !sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
            || sha512.len() != 88
            || !sha512.ends_with("==")
            || !sha512[..86]
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/'))
            || !asset
                .get("size")
                .and_then(Value::as_f64)
                .is_some_and(|size| {
                    size.fract() == 0.0 && (1.0..=9_007_199_254_740_991.0).contains(&size)
                })
            || !asset.get("platform").is_some_and(Value::is_string)
            || !asset.get("arch").is_some_and(Value::is_string)
        {
            return Err(failure("releaseUnavailable", "Invalid installer manifest."));
        }
    }
    Ok(UpdateTarget {
        tag_name: tag.into(),
        source_sha: source_sha.into(),
    })
}

fn release_version(tag: &str) -> Option<String> {
    if !valid_release_tag(tag) {
        return None;
    }
    let captures = Regex::new(r"[0-9]+").unwrap();
    let numbers = captures
        .find_iter(tag)
        .filter_map(|part| part.as_str().parse::<u64>().ok())
        .collect::<Vec<_>>();
    Some(format!(
        "{}.{}.{}",
        numbers[0],
        numbers[1] * 100 + numbers[2],
        numbers.get(3).copied().unwrap_or(1) - 1
    ))
}

fn run_process(
    program: &str,
    args: &[String],
    cwd: &Path,
    timeout: Duration,
    capture_stdout: bool,
    output: &mut dyn FnMut(&str),
    env: &HashMap<String, String>,
) -> Result<ProcessOutput, UpdateCommandError> {
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .envs(env)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("HUSKY", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
        pilotdeck_child_signals::install_child_signal_reset(&mut command);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = command
        .spawn()
        .map_err(|error| failure("processStartFailed", error.to_string()))?;
    let pid = child.id();
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    #[cfg(unix)]
    for result in [nonblocking(&stdout), nonblocking(&stderr)] {
        if let Err(error) = result {
            let _ = stop_managed_child(&mut child, pid);
            return Err(failure("processStopFailed", error.to_string()));
        }
    }
    let stopped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (sender, receiver) = mpsc::channel();
    let stdout_sender = sender.clone();
    let stop_stdout = stopped.clone();
    let stdout_thread =
        thread::spawn(move || read_chunks(stdout, true, stdout_sender, stop_stdout));
    let stop_stderr = stopped.clone();
    let stderr_thread = thread::spawn(move || read_chunks(stderr, false, sender, stop_stderr));
    let deadline = Instant::now() + timeout;
    let mut stdout_bytes = Vec::new();
    let mut result = loop {
        match receiver.recv_timeout(Duration::from_millis(10)) {
            Ok((is_stdout, chunk)) => {
                output(&String::from_utf8_lossy(&chunk));
                if capture_stdout && is_stdout {
                    if stdout_bytes.len().saturating_add(chunk.len()) > MAX_COMMAND_OUTPUT {
                        break Err(failure("checkFailed", "Command output is too large."));
                    }
                    stdout_bytes.extend_from_slice(&chunk);
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected | mpsc::RecvTimeoutError::Timeout) => {}
        }
        // Keep the Unix leader unreaped until the last group signal. A reused
        // PID must never become a certificate of descendant ownership.
        match child_has_exited(&mut child, pid) {
            Ok(true) => break Ok(()),
            Ok(false) => {}
            Err(error) => break Err(failure("processStopFailed", error.to_string())),
        }
        if Instant::now() >= deadline {
            break Err(failure("buildTimedOut", format!("{program} timed out.")));
        }
    };
    let status = if result.is_ok()
        && capture_stdout
        && stdout_thread.is_finished()
        && stderr_thread.is_finished()
    {
        // TS uses execFile for short Git queries. Complete pipe EOF and an
        // unreaped exit are sufficient; it does not impose build-tree grace.
        child.wait()
    } else {
        stop_managed_child(&mut child, pid)
    };
    if let Err(error) = &status {
        result = Err(failure("processStopFailed", error.to_string()));
    }
    // A process outside our group can retain a pipe. Nonblocking readers stop
    // independently; a pipe must not keep an update and its lock pending.
    let drain_deadline = Instant::now() + Duration::from_millis(250);
    while !(stdout_thread.is_finished() && stderr_thread.is_finished())
        && Instant::now() < drain_deadline
    {
        thread::sleep(Duration::from_millis(5));
    }
    stopped.store(true, std::sync::atomic::Ordering::Release);
    let join_deadline = Instant::now() + Duration::from_millis(250);
    while !(stdout_thread.is_finished() && stderr_thread.is_finished())
        && Instant::now() < join_deadline
    {
        thread::sleep(Duration::from_millis(5));
    }
    for reader in [stdout_thread, stderr_thread] {
        if !reader.is_finished() {
            result = Err(failure("processStopFailed", "Command output did not stop."));
        } else if !matches!(reader.join(), Ok(Ok(()))) {
            result = Err(failure(
                "processStopFailed",
                "Command output reader failed.",
            ));
        }
    }
    for (is_stdout, chunk) in receiver.try_iter() {
        output(&String::from_utf8_lossy(&chunk));
        if capture_stdout && is_stdout {
            if stdout_bytes.len().saturating_add(chunk.len()) > MAX_COMMAND_OUTPUT {
                result = Err(failure("checkFailed", "Command output is too large."));
                break;
            }
            stdout_bytes.extend_from_slice(&chunk);
        }
    }
    result?;
    Ok(ProcessOutput {
        code: status
            .map_err(|error| failure("processStopFailed", error.to_string()))?
            .code()
            .unwrap_or(1),
        stdout: String::from_utf8_lossy(&stdout_bytes).into_owned(),
    })
}

#[cfg(unix)]
fn nonblocking(fd: &impl std::os::fd::AsFd) -> io::Result<()> {
    let flags = rustix::fs::fcntl_getfl(fd)?;
    rustix::fs::fcntl_setfl(fd, flags | rustix::fs::OFlags::NONBLOCK)?;
    Ok(())
}

fn child_has_exited(child: &mut std::process::Child, pid: u32) -> io::Result<bool> {
    #[cfg(unix)]
    {
        let _ = child;
        let pid = rustix::process::Pid::from_raw(pid as i32)
            .ok_or_else(|| io::Error::other("Invalid child identity."))?;
        pilotdeck_process::process_group::leader_exited_unreaped(pid).map_err(Into::into)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        child.try_wait().map(|status| status.is_some())
    }
}

fn read_chunks(
    mut input: impl Read,
    stdout: bool,
    sender: mpsc::Sender<(bool, Vec<u8>)>,
    stopped: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> io::Result<()> {
    let mut buffer = [0; 4096];
    loop {
        match input.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(read) => {
                if sender.send((stdout, buffer[..read].to_vec())).is_err() {
                    return Ok(());
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if stopped.load(std::sync::atomic::Ordering::Acquire) {
                    return Ok(());
                }
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => return Err(error),
        }
    }
}

fn stop_managed_child(
    child: &mut std::process::Child,
    pid: u32,
) -> io::Result<std::process::ExitStatus> {
    #[cfg(unix)]
    {
        let group = rustix::process::Pid::from_raw(pid as i32)
            .ok_or_else(|| io::Error::other("Invalid child identity."))?;
        pilotdeck_process::process_group::kill(Some(group), rustix::process::Signal::TERM);
        // Match processTree.js: descendants receive the grace period even if
        // the leader has exited. The leader remains unreaped throughout it.
        thread::sleep(Duration::from_millis(1500));
        pilotdeck_process::process_group::kill(Some(group), rustix::process::Signal::KILL);
        let deadline = Instant::now() + Duration::from_secs(3);
        while !pilotdeck_process::process_group::leader_exited_unreaped(group)? {
            if Instant::now() >= deadline {
                return Err(io::Error::other("Command termination is unconfirmed."));
            }
            thread::sleep(Duration::from_millis(10));
        }
        let status = child.wait()?;
        while pilotdeck_process::process_group::group_exists(group) {
            if Instant::now() >= deadline {
                return Err(io::Error::other("Command descendants did not stop."));
            }
            thread::sleep(Duration::from_millis(10));
        }
        Ok(status)
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        let _ = child.kill();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(status) = child.try_wait()? {
                return Ok(status);
            }
            if Instant::now() >= deadline {
                return Err(io::Error::other("Command termination is unconfirmed."));
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}

fn relocate_dependencies(source: &Path, destination: &Path) -> Result<(), UpdateCommandError> {
    for directory in ["node_modules", "ui/node_modules"] {
        relocate_tree(&source.join(directory), source, destination)?;
    }
    Ok(())
}

fn relocate_tree(
    directory: &Path,
    source: &Path,
    destination: &Path,
) -> Result<(), UpdateCommandError> {
    for entry in
        fs::read_dir(directory).map_err(|error| failure("relocationFailed", error.to_string()))?
    {
        let entry = entry.map_err(|error| failure("relocationFailed", error.to_string()))?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| failure("relocationFailed", error.to_string()))?;
        if metadata.file_type().is_symlink() {
            relocate_symlink(&path, source, destination)?;
        } else if metadata.is_dir() {
            relocate_tree(&path, source, destination)?;
        } else if path
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            == Some(".bin")
            || matches!(
                path.file_name().and_then(|name| name.to_str()),
                Some(".modules.yaml" | ".pnpm-workspace-state-v1.json")
            )
        {
            let original = fs::read_to_string(&path)
                .map_err(|error| failure("relocationFailed", error.to_string()))?;
            let source_text = source.to_string_lossy();
            let destination_text = destination.to_string_lossy();
            let escaped_source = serde_json::to_string(source_text.as_ref()).unwrap();
            let escaped_destination = serde_json::to_string(destination_text.as_ref()).unwrap();
            let rewritten = original
                .replace(
                    &escaped_source[1..escaped_source.len() - 1],
                    &escaped_destination[1..escaped_destination.len() - 1],
                )
                .replace(
                    &source_text.replace('\\', "/"),
                    &destination_text.replace('\\', "/"),
                )
                .replace(source_text.as_ref(), destination_text.as_ref());
            if rewritten != original {
                fs::write(&path, rewritten)
                    .map_err(|error| failure("relocationFailed", error.to_string()))?;
            }
        }
    }
    Ok(())
}

fn lexical_path(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                result.pop();
            }
            _ => result.push(component.as_os_str()),
        }
    }
    result
}

fn relocate_symlink(
    path: &Path,
    source: &Path,
    destination: &Path,
) -> Result<(), UpdateCommandError> {
    let target =
        fs::read_link(path).map_err(|error| failure("relocationFailed", error.to_string()))?;
    let resolved = lexical_path(&path.parent().unwrap_or(source).join(&target));
    let relative = resolved.strip_prefix(source).map_err(|_| {
        failure(
            "relocationFailed",
            "Dependency link escapes the staged source.",
        )
    })?;
    if !target.is_absolute() {
        return Ok(());
    }
    let replacement = destination.join(relative);
    fs::remove_file(path).map_err(|error| failure("relocationFailed", error.to_string()))?;
    #[cfg(unix)]
    std::os::unix::fs::symlink(replacement, path)
        .map_err(|error| failure("relocationFailed", error.to_string()))?;
    #[cfg(windows)]
    {
        let is_directory = target.is_dir();
        if is_directory {
            std::os::windows::fs::symlink_dir(replacement, path)
        } else {
            std::os::windows::fs::symlink_file(replacement, path)
        }
        .map_err(|error| failure("relocationFailed", error.to_string()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Mutex;

    struct MockRuntime {
        latest: Result<UpdateTarget, UpdateCommandError>,
        commands: Mutex<Vec<(String, Vec<String>)>>,
        replies: Mutex<Vec<ProcessOutput>>,
    }

    impl ReleaseRuntime for MockRuntime {
        fn latest(&self) -> Result<UpdateTarget, UpdateCommandError> {
            self.latest.clone()
        }

        fn run(
            &self,
            program: &str,
            args: &[String],
            _cwd: &Path,
            _timeout: Duration,
            _capture_stdout: bool,
            _output: &mut dyn FnMut(&str),
        ) -> Result<ProcessOutput, UpdateCommandError> {
            self.commands
                .lock()
                .unwrap()
                .push((program.into(), args.to_vec()));
            Ok(self.replies.lock().unwrap().remove(0))
        }
    }

    #[derive(Clone, Copy)]
    enum Relation {
        Newer,
        Same,
        Ahead,
        Diverged,
    }

    struct DeploymentRuntime {
        root: PathBuf,
        relation: Relation,
        fail_merge: bool,
        fail_build: bool,
        calls: std::sync::Arc<Mutex<Vec<Vec<String>>>>,
    }

    fn target() -> UpdateTarget {
        UpdateTarget {
            tag_name: "v2026.09.07".into(),
            source_sha: "b".repeat(40),
        }
    }

    fn write_build(root: &Path, sha: &str, marker: &str) {
        for artifact in ARTIFACTS {
            fs::create_dir_all(root.join(artifact)).unwrap();
            fs::write(root.join(artifact).join("marker"), marker).unwrap();
        }
        for directory in ["dist", "ui/dist"] {
            fs::write(
                root.join(directory).join("web-build.json"),
                json!({"sourceSha":sha, "clean":true}).to_string(),
            )
            .unwrap();
        }
    }

    impl ReleaseRuntime for DeploymentRuntime {
        fn latest(&self) -> Result<UpdateTarget, UpdateCommandError> {
            Ok(target())
        }

        fn run(
            &self,
            program: &str,
            args: &[String],
            cwd: &Path,
            _timeout: Duration,
            _capture: bool,
            output: &mut dyn FnMut(&str),
        ) -> Result<ProcessOutput, UpdateCommandError> {
            let mut call = vec![program.to_owned()];
            call.extend_from_slice(args);
            self.calls.lock().unwrap().push(call);
            let mut code = 0;
            let stdout = match (
                program,
                args.iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .as_slice(),
            ) {
                ("git", ["rev-parse", "--show-toplevel"]) => {
                    self.root.to_string_lossy().into_owned()
                }
                ("git", ["branch", "--show-current"]) => "main".into(),
                ("git", ["status", "--porcelain", "--untracked-files=all"]) => String::new(),
                ("git", ["rev-parse", "--is-shallow-repository"]) => "false".into(),
                ("git", ["remote"]) => "origin".into(),
                ("git", ["remote", "get-url", "origin"]) => REPOSITORY_URL.into(),
                ("git", ["rev-parse", "HEAD"]) => {
                    if matches!(self.relation, Relation::Same) {
                        "b".repeat(40)
                    } else {
                        "a".repeat(40)
                    }
                }
                ("git", ["fetch", "--no-tags", _, _]) => String::new(),
                ("git", ["rev-parse", reference])
                    if reference.starts_with("refs/pilotdeck/releases/") =>
                {
                    "b".repeat(40)
                }
                ("git", ["merge-base", "--is-ancestor", from, _]) => {
                    let ancestor = match self.relation {
                        Relation::Newer => from.starts_with('a'),
                        Relation::Ahead => from.starts_with('b'),
                        Relation::Diverged | Relation::Same => false,
                    };
                    code = if ancestor { 0 } else { 1 };
                    String::new()
                }
                ("git", ["clone", "--shared", "--no-checkout", _, destination]) => {
                    fs::create_dir(destination).unwrap();
                    String::new()
                }
                ("git", ["checkout", "--detach", _]) => String::new(),
                ("git", ["-c", _, "merge", "--ff-only", "--no-edit", _]) => {
                    code = if self.fail_merge { 1 } else { 0 };
                    String::new()
                }
                (
                    "pnpm",
                    [
                        "install",
                        "--frozen-lockfile",
                        "--filter",
                        "pilotdeck",
                        "--filter",
                        "pilotdeck-ui",
                    ],
                ) => String::new(),
                ("pnpm", ["run", "build:web"]) => {
                    if self.fail_build {
                        code = 1;
                    } else {
                        write_build(cwd, &"b".repeat(40), "new");
                    }
                    output("mock build progress");
                    String::new()
                }
                _ => panic!("Unexpected command in isolated deployment: {program} {args:?}"),
            };
            Ok(ProcessOutput { code, stdout })
        }
    }

    type Calls = std::sync::Arc<Mutex<Vec<Vec<String>>>>;
    fn deployment(
        relation: Relation,
        fail_merge: bool,
        fail_build: bool,
    ) -> (crate::scratch::Scratch, NativeUpdateBackend, Calls) {
        let scratch = crate::scratch::Scratch::new("update-deployment");
        fs::create_dir(scratch.path().join(".git")).unwrap();
        let sha = if matches!(relation, Relation::Same) {
            "b"
        } else {
            "a"
        }
        .repeat(40);
        write_build(scratch.path(), &sha, "old");
        let calls = std::sync::Arc::new(Mutex::new(Vec::new()));
        let runtime = DeploymentRuntime {
            root: scratch.path().to_owned(),
            relation,
            fail_merge,
            fail_build,
            calls: calls.clone(),
        };
        let backend =
            NativeUpdateBackend::injected(scratch.path().into(), HashMap::new(), Box::new(runtime));
        (scratch, backend, calls)
    }

    #[test]
    fn release_check_distinguishes_fast_forward_noop_ahead_and_divergence() {
        for (relation, reason, available) in [
            (Relation::Newer, None, true),
            (Relation::Same, Some("upToDate"), false),
            (Relation::Ahead, Some("ahead"), false),
            (Relation::Diverged, Some("diverged"), false),
        ] {
            let (_scratch, mut backend, _) = deployment(relation, false, false);
            let status = backend.check().unwrap();
            assert_eq!(status.reason.as_deref(), reason);
            assert_eq!(status.can_update, available);
            assert_eq!(status.latest, Some(target()));
        }
    }

    #[test]
    fn isolated_apply_installs_validated_artifacts_and_requires_restart() {
        let (scratch, mut backend, calls) = deployment(Relation::Newer, false, false);
        let mut progress = Vec::new();
        backend
            .apply(&target(), &mut |text| progress.push(text.to_owned()))
            .unwrap();
        for artifact in ARTIFACTS {
            assert_eq!(
                fs::read_to_string(scratch.path().join(artifact).join("marker")).unwrap(),
                "new"
            );
        }
        assert_eq!(
            read_web_build_version(scratch.path()).unwrap().source_sha,
            "b".repeat(40)
        );
        let manifest: Value =
            serde_json::from_slice(&fs::read(scratch.path().join("dist/web-build.json")).unwrap())
                .unwrap();
        assert_eq!(manifest["tagName"], target().tag_name);
        assert!(!scratch.path().join(".git/pilotdeck-update.lock").exists());
        assert!(progress.iter().any(|line| line.contains("Build succeeded")));
        assert!(
            calls
                .lock()
                .unwrap()
                .iter()
                .any(|call| call.iter().any(|arg| arg == "--ff-only"))
        );
        assert_eq!(
            backend.apply(&target(), &mut |_| {}).unwrap_err().reason,
            "restartRequired"
        );
    }

    #[test]
    fn isolated_apply_rolls_back_and_refuses_changed_or_failed_preparation() {
        for (fail_merge, fail_build, reason) in
            [(true, false, "checkFailed"), (false, true, "buildFailed")]
        {
            let (scratch, mut backend, _) = deployment(Relation::Newer, fail_merge, fail_build);
            assert_eq!(
                backend.apply(&target(), &mut |_| {}).unwrap_err().reason,
                reason
            );
            for artifact in ARTIFACTS {
                assert_eq!(
                    fs::read_to_string(scratch.path().join(artifact).join("marker")).unwrap(),
                    "old"
                );
            }
            assert!(!scratch.path().join(".git/pilotdeck-update.lock").exists());
            assert!(!backend.needs_restart);
        }
        let (scratch, mut backend, calls) = deployment(Relation::Newer, false, false);
        let mut changed = target();
        changed.source_sha = "c".repeat(40);
        assert_eq!(
            backend.apply(&changed, &mut |_| {}).unwrap_err().reason,
            "targetChanged"
        );
        assert!(
            !calls
                .lock()
                .unwrap()
                .iter()
                .any(|call| call.first().is_some_and(|program| program == "pnpm"))
        );
        assert!(!scratch.path().join(".git/pilotdeck-update.lock").exists());
    }

    #[cfg(unix)]
    #[test]
    fn build_command_stops_pipe_holding_descendants_after_leader_exit_and_timeout() {
        for (script, timeout, expected) in [
            (
                "(trap '' TERM; sleep 30) & exit 0",
                Duration::from_secs(5),
                None,
            ),
            (
                "trap '' TERM; (trap '' TERM; sleep 30) & wait",
                Duration::from_millis(60),
                Some("buildTimedOut"),
            ),
        ] {
            let scratch = crate::scratch::Scratch::new("update-process");
            let started = Instant::now();
            let result = run_process(
                "/bin/sh",
                &["-c".into(), script.into()],
                scratch.path(),
                timeout,
                false,
                &mut |_| {},
                &HashMap::new(),
            );
            assert!(
                started.elapsed() < Duration::from_secs(6),
                "command exceeded its cleanup bound"
            );
            if let Some(reason) = expected {
                assert_eq!(result.unwrap_err().reason, reason);
            } else {
                assert_eq!(result.unwrap().code, 0);
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn dependency_relocation_rejects_escaping_relative_links() {
        let scratch = crate::scratch::Scratch::new("update-links");
        let source = scratch.path().join("source");
        fs::create_dir(&source).unwrap();
        let link = source.join("package");
        std::os::unix::fs::symlink("../outside", &link).unwrap();
        assert_eq!(
            relocate_symlink(&link, &source, &scratch.path().join("live"))
                .unwrap_err()
                .reason,
            "relocationFailed"
        );
    }

    #[test]
    fn offline_health_guard_never_applies_against_a_running_web_service() {
        use std::net::TcpListener;
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = thread::spawn(move || {
            use std::io::Write;
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut request = [0; 1024];
            let received = stream.read(&mut request).unwrap();
            assert!(received > 0);
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
                .unwrap();
        });
        let env = HashMap::from([("SERVER_PORT".into(), port.to_string())]);
        let error = ensure_offline_service(&env, None).unwrap_err();
        assert_ne!(
            error.reason, "onlineBackendMissing",
            "webUpdateCommand.js has no onlineBackendMissing; healthy /health continues to localHeaders (line 56)"
        );
        assert_eq!(
            error.reason, "runtimeUnavailable",
            "healthy port blocks offline(); from_current_exe retries try_online_client"
        );
        server.join().unwrap();
        // The same isolated port is now closed, so only this path may go offline.
        ensure_offline_service(&env, None).unwrap();
        assert_eq!(
            ensure_offline_service(&HashMap::from([("SERVER_PORT".into(), "0".into())]), None)
                .unwrap_err()
                .reason,
            "invalidPort"
        );
    }

    #[test]
    fn release_tag_and_manifest_validation_match_ts() {
        assert!(valid_release_tag("v2026.02.28"));
        assert!(valid_release_tag("v2024.02.29-r10"));
        for tag in [
            "v2026.02.29",
            "v2026.13.01",
            "v2026.09.07-r0",
            "desktop-v2026.09.07",
            "v0000.02.29",
            "v2026.09.07-r9007199254740992",
        ] {
            assert!(!valid_release_tag(tag), "{tag}");
        }
        let asset = json!({
            "name":"PilotDeck.zip","platform":"darwin","arch":"arm64","size":42,
            "sha256":"a".repeat(64),"sha512":format!("{}==", "A".repeat(86))
        });
        let manifest = json!({
            "schemaVersion":1,"repository":REPOSITORY,"tag":"v2026.09.07-r10",
            "sourceSha":"a".repeat(40),"version":"2026.907.9","date":"2026-09-07",
            "assets":[asset]
        });
        assert_eq!(
            validate_manifest(&manifest).unwrap(),
            UpdateTarget {
                tag_name: "v2026.09.07-r10".into(),
                source_sha: "a".repeat(40)
            }
        );
        assert!(!release_tag_syntax("v２０２６.09.07"));
        let mut numeric = manifest.clone();
        numeric["schemaVersion"] = json!(1.0);
        numeric["assets"][0]["size"] = json!(42.0);
        assert!(validate_manifest(&numeric).is_ok());
        for size in [0.0, 1.5, 9_007_199_254_740_992.0] {
            numeric["assets"][0]["size"] = json!(size);
            assert!(validate_manifest(&numeric).is_err(), "size={size}");
        }
        let mut bad = manifest;
        bad["assets"][0]["name"] = "../bad".into();
        assert_eq!(
            validate_manifest(&bad).unwrap_err().reason,
            "releaseUnavailable"
        );
    }

    #[test]
    fn build_metadata_requires_two_matching_clean_outputs() {
        let scratch = crate::scratch::Scratch::new("update-build-version");
        for directory in ["dist", "ui/dist"] {
            fs::create_dir_all(scratch.path().join(directory)).unwrap();
            fs::write(
                scratch.path().join(directory).join("web-build.json"),
                json!({"sourceSha":"a".repeat(40),"clean":true,"tagName":null}).to_string(),
            )
            .unwrap();
        }
        assert_eq!(
            read_web_build_version(scratch.path()).unwrap().source_sha,
            "a".repeat(40)
        );
        fs::write(
            scratch.path().join("ui/dist/web-build.json"),
            json!({"sourceSha":"b".repeat(40),"clean":true}).to_string(),
        )
        .unwrap();
        assert!(read_web_build_version(scratch.path()).is_none());
    }

    #[test]
    fn injected_backend_rejects_before_release_discovery() {
        let scratch = crate::scratch::Scratch::new("update-eligibility");
        fs::write(scratch.path().join(".git"), "gitdir: elsewhere").unwrap();
        let runtime = MockRuntime {
            latest: Ok(UpdateTarget {
                tag_name: "v2026.09.07".into(),
                source_sha: "a".repeat(40),
            }),
            commands: Mutex::new(Vec::new()),
            replies: Mutex::new(Vec::new()),
        };
        let mut backend =
            NativeUpdateBackend::injected(scratch.path().into(), HashMap::new(), Box::new(runtime));
        assert_eq!(
            backend.check().unwrap().reason.as_deref(),
            Some("development")
        );
    }
}
