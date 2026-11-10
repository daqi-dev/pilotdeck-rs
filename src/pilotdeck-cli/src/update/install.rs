use super::process::{Runner, Signals};
use super::{Backend, Error, Release, Status};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, symlink},
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

pub const BINARIES: [&str; 4] = [
    "pilotdeck",
    "pilotdeck-control",
    "pilotdeck-gateway",
    "pilotdeck-agent-loop-sidecar",
];
const MAX_METADATA: u64 = 1024 * 1024;
const MAX_BINARY: u64 = 512 * 1024 * 1024;
fn io(error: impl std::fmt::Display) -> Error {
    Error::new("applyFailed", error.to_string())
}
fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 80 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}
fn valid_sha(sha: &str) -> bool {
    sha.len() == 40
        && sha
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn valid_policy(remote: &str, reference: &str) -> bool {
    !remote.is_empty()
        && !remote.starts_with('-')
        && !remote.chars().any(char::is_control)
        && reference.starts_with("refs/")
        && !reference.contains("..")
        && !reference.chars().any(|c| {
            c.is_control()
                || c.is_whitespace()
                || matches!(c, '~' | '^' | ':' | '?' | '*' | '[' | '\\')
        })
}
fn private_dir(path: &Path) -> Result<(), Error> {
    let m = fs::symlink_metadata(path).map_err(io)?;
    if !m.is_dir()
        || m.file_type().is_symlink()
        || m.uid() != rustix::process::geteuid().as_raw()
        || m.mode() & 0o077 != 0
    {
        return Err(Error::new(
            "unsafeInstallation",
            "Installation directories must be private, owned regular directories.",
        ));
    }
    Ok(())
}
fn mkdir(path: &Path) -> Result<(), Error> {
    fs::DirBuilder::new().mode(0o700).create(path).map_err(io)
}
fn sync_dir(path: &Path) -> Result<(), Error> {
    File::open(path).and_then(|f| f.sync_all()).map_err(io)
}
fn link_generation_skills(generation: &Path, external: bool) -> Result<(), Error> {
    if external {
        // The shared external root remains authoritative, including when it
        // becomes unavailable. Never silently replace it with bundled assets.
        symlink("../skills", generation.join("skills")).map_err(io)?;
    } else {
        match fs::symlink_metadata(generation.join("source/skills")) {
            Ok(metadata) if metadata.is_dir() => {
                symlink("source/skills", generation.join("skills")).map_err(io)?;
            }
            Ok(_) => {
                return Err(Error::new(
                    "invalidSkillsRoot",
                    "Source skills must be a directory.",
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(io(error)),
        }
    }
    Ok(())
}
fn regular(path: &Path) -> Result<File, Error> {
    let f = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
        .open(path)
        .map_err(io)?;
    if !f.metadata().map_err(io)?.is_file() {
        return Err(Error::new(
            "unsafeInstallation",
            "Installation inputs must be regular files.",
        ));
    }
    Ok(f)
}
fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, Error> {
    let mut bytes = Vec::new();
    regular(path)?
        .take(MAX_METADATA + 1)
        .read_to_end(&mut bytes)
        .map_err(io)?;
    if bytes.len() as u64 > MAX_METADATA {
        return Err(Error::new(
            "unsafeInstallation",
            "Installation metadata is too large.",
        ));
    }
    serde_json::from_slice(&bytes).map_err(io)
}
fn write_json(path: &Path, value: &impl Serialize) -> Result<(), Error> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(io)?;
    bytes.push(b'\n');
    let mut f = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(io)?;
    f.write_all(&bytes).and_then(|()| f.sync_all()).map_err(io)
}
fn hash_file(path: &Path) -> Result<String, Error> {
    let mut f = regular(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0; 65536];
    let mut length = 0_u64;
    loop {
        let n = f.read(&mut buf).map_err(io)?;
        if n == 0 {
            break;
        }
        length += n as u64;
        if length > MAX_BINARY {
            return Err(Error::new(
                "buildFailed",
                "Executable exceeds the supported 512MiB limit.",
            ));
        }
        hasher.update(&buf[..n]);
    }
    if length == 0 {
        return Err(Error::new("buildFailed", "An executable is empty."));
    }
    Ok(format!("{:x}", hasher.finalize()))
}
fn copy_executable(source: &Path, target: &Path) -> Result<String, Error> {
    let mut input = regular(source)?;
    if input.metadata().map_err(io)?.mode() & 0o111 == 0 {
        return Err(Error::new("buildFailed", "Build output is not executable."));
    }
    let mut target_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o700)
        .open(target)
        .map_err(io)?;
    let count = std::io::copy(
        &mut Read::by_ref(&mut input).take(MAX_BINARY + 1),
        &mut target_file,
    )
    .map_err(io)?;
    if count == 0 || count > MAX_BINARY {
        return Err(Error::new("buildFailed", "Invalid executable size."));
    }
    target_file.sync_all().map_err(io)?;
    hash_file(target)
}

// Libraries are copied from the newly built target, never from the updater's
// own profile. Every runtime file is a regular bounded file with a recorded hash.
fn valid_library_profile(value: &str) -> bool {
    value.strip_prefix("pilotdeck-libs-").is_some_and(|hash| {
        hash.len() == 64
            && hash
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    })
}
fn valid_library_name(value: &str) -> bool {
    value.starts_with("lib")
        && value.len() <= 200
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._+-".contains(&b))
        && (value.contains(".so") || value.ends_with(".dylib"))
}
fn copy_library(source: &Path, target: &Path) -> Result<String, Error> {
    let mut input = regular(source)?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(target)
        .map_err(io)?;
    let count = std::io::copy(
        &mut Read::by_ref(&mut input).take(MAX_BINARY + 1),
        &mut output,
    )
    .map_err(io)?;
    if count == 0 || count > MAX_BINARY {
        return Err(Error::new("buildFailed", "Invalid library size."));
    }
    output.sync_all().map_err(io)?;
    hash_file(target)
}
fn copy_image_libraries(
    artifacts: &Path,
    bins: &Path,
) -> Result<(Option<String>, BTreeMap<String, String>), Error> {
    let mut marker = String::new();
    regular(&artifacts.join("pilotdeck-image-profile"))?
        .take(128)
        .read_to_string(&mut marker)
        .map_err(io)?;
    let profile = marker
        .strip_suffix('\n')
        .filter(|s| valid_library_profile(s))
        .ok_or_else(|| Error::new("buildFailed", "Invalid image library profile."))?;
    let source = artifacts.join(profile);
    private_dir(&source)?;
    let dest = bins.join(profile);
    mkdir(&dest)?;
    let mut hashes = BTreeMap::new();
    for entry in fs::read_dir(&source).map_err(io)? {
        let entry = entry.map_err(io)?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| Error::new("buildFailed", "Invalid library filename."))?;
        if !valid_library_name(&name) || hashes.len() >= 64 {
            return Err(Error::new(
                "buildFailed",
                "Invalid image library inventory.",
            ));
        }
        hashes.insert(
            name.clone(),
            copy_library(&entry.path(), &dest.join(&name))?,
        );
    }
    if hashes.is_empty() {
        return Err(Error::new(
            "buildFailed",
            "Image library inventory is empty.",
        ));
    }
    sync_dir(&dest)?;
    Ok((Some(profile.to_owned()), hashes))
}
fn verify_image_libraries(bins: &Path, manifest: &Generation) -> Result<(), Error> {
    match (
        &manifest.library_profile,
        manifest.library_hashes.is_empty(),
    ) {
        (None, true) => Ok(()),
        (Some(profile), false)
            if valid_library_profile(profile) && manifest.library_hashes.len() <= 64 =>
        {
            let dir = bins.join(profile);
            private_dir(&dir)?;
            let mut count = 0usize;
            for entry in fs::read_dir(&dir).map_err(io)? {
                let entry = entry.map_err(io)?;
                let name = entry
                    .file_name()
                    .into_string()
                    .map_err(|_| Error::refused("runtimeMismatch"))?;
                if !valid_library_name(&name)
                    || manifest.library_hashes.get(&name) != Some(&hash_file(&entry.path())?)
                {
                    return Err(Error::refused("runtimeMismatch"));
                }
                count += 1;
            }
            if count != manifest.library_hashes.len() {
                return Err(Error::refused("runtimeMismatch"));
            }
            Ok(())
        }
        _ => Err(Error::refused("runtimeMismatch")),
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstallManifest {
    pub schema_version: u32,
    pub install_id: String,
    pub approved_remote: String,
    pub approved_ref: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Generation {
    pub schema_version: u32,
    pub generation: String,
    pub source_sha: String,
    pub source_checkout: PathBuf,
    pub bin_hashes: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub library_profile: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub library_hashes: BTreeMap<String, String>,
}
pub struct Installation {
    pub root: PathBuf,
    pub policy: InstallManifest,
    pub expected_generation: String,
    pub current: Generation,
}
impl Installation {
    pub fn open(root: &Path, expected_generation: &str) -> Result<Self, Error> {
        let root = fs::canonicalize(root).map_err(io)?;
        private_dir(&root)?;
        private_dir(&root.join("generations"))?;
        let policy: InstallManifest = read_json(&root.join("install.json"))?;
        if policy.schema_version != 1
            || !valid_id(&policy.install_id)
            || !valid_policy(&policy.approved_remote, &policy.approved_ref)
            || !valid_id(expected_generation)
        {
            return Err(Error::new(
                "unsafeInstallation",
                "Invalid native installation identity.",
            ));
        }
        let current = Self::read_current(&root)?;
        Ok(Self {
            root,
            policy,
            expected_generation: expected_generation.into(),
            current,
        })
    }
    pub fn discover(executable: &Path) -> Result<Self, Error> {
        let executable = fs::canonicalize(executable).map_err(io)?;
        let bin = executable
            .parent()
            .ok_or_else(|| Error::refused("development"))?;
        let generation = bin.parent().ok_or_else(|| Error::refused("development"))?;
        let generations = generation
            .parent()
            .ok_or_else(|| Error::refused("development"))?;
        if bin.file_name().and_then(|n| n.to_str()) != Some("bin")
            || generations.file_name().and_then(|n| n.to_str()) != Some("generations")
        {
            return Err(Error::refused("development"));
        }
        let id = generation
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| Error::refused("development"))?;
        Self::open(
            generations
                .parent()
                .ok_or_else(|| Error::refused("development"))?,
            id,
        )
    }
    fn read_current(root: &Path) -> Result<Generation, Error> {
        let link = fs::read_link(root.join("current")).map_err(io)?;
        let parts: Vec<_> = link.components().collect();
        if parts.len() != 2 || parts[0].as_os_str() != "generations" {
            return Err(Error::new(
                "unsafeInstallation",
                "Invalid current generation link.",
            ));
        }
        let id = parts[1]
            .as_os_str()
            .to_str()
            .filter(|id| valid_id(id))
            .ok_or_else(|| {
                Error::new("unsafeInstallation", "Invalid current generation identity.")
            })?;
        let directory = root.join(&link);
        private_dir(&directory)?;
        private_dir(&directory.join("bin"))?;
        let manifest: Generation = read_json(&directory.join("manifest.json"))?;
        if manifest.schema_version != 1
            || manifest.generation != id
            || !valid_sha(&manifest.source_sha)
            || manifest.source_checkout != directory.join("source")
            || manifest.bin_hashes.len() != 4
        {
            return Err(Error::new(
                "unsafeInstallation",
                "Invalid generation manifest.",
            ));
        }
        for name in BINARIES {
            if manifest.bin_hashes.get(name) != Some(&hash_file(&directory.join("bin").join(name))?)
            {
                return Err(Error::refused("runtimeMismatch"));
            }
        }
        verify_image_libraries(&directory.join("bin"), &manifest)?;
        Ok(manifest)
    }
    fn unchanged(&self) -> Result<(), Error> {
        private_dir(&self.root)?;
        let policy: InstallManifest = read_json(&self.root.join("install.json"))?;
        if policy.schema_version != self.policy.schema_version
            || policy.install_id != self.policy.install_id
            || policy.approved_remote != self.policy.approved_remote
            || policy.approved_ref != self.policy.approved_ref
        {
            return Err(Error::new(
                "workspaceChanged",
                "Installation policy changed during update.",
            ));
        }
        let current = Self::read_current(&self.root)?;
        if current.generation != self.current.generation
            || current.source_sha != self.current.source_sha
        {
            return Err(Error::new(
                "workspaceChanged",
                "The current installation changed during update.",
            ));
        }
        Ok(())
    }
}

pub trait Builder {
    fn requires_image_libraries(&self) -> bool {
        false
    }
    fn git(&mut self, cwd: &Path, args: &[&str]) -> Result<String, Error>;
    fn build(&mut self, source: &Path, output: &mut dyn FnMut(&str)) -> Result<PathBuf, Error>;
    fn cancelled(&self) -> bool {
        false
    }
}
pub struct NativeBuilder {
    pub git: PathBuf,
    pub cargo: PathBuf,
    signals: Signals,
    progress: bool,
}
impl NativeBuilder {
    /// CLI-scoped owner: keep alive from before check/bootstrap until command
    /// completion so cancellation is not lost between individual subprocesses.
    pub fn new(git: PathBuf, cargo: PathBuf) -> Result<Self, Error> {
        Ok(Self {
            git,
            cargo,
            progress: true,
            signals: Signals::install().map_err(|e| Error::new("processStartFailed", e))?,
        })
    }
    pub fn set_progress(&mut self, enabled: bool) {
        self.progress = enabled;
    }
    pub(super) fn cancellation(&self) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
        self.signals.cancelled.clone()
    }
    fn command(&self, program: &Path, cwd: &Path) -> Command {
        let mut c = Command::new(program);
        c.current_dir(cwd)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_NO_REPLACE_OBJECTS", "1")
            .env("CARGO_TERM_COLOR", "never");
        c
    }
}
impl Builder for NativeBuilder {
    fn requires_image_libraries(&self) -> bool {
        true
    }
    fn git(&mut self, cwd: &Path, args: &[&str]) -> Result<String, Error> {
        let result = Runner {
            timeout: Duration::from_secs(60),
            output_limit: 10 * 1024 * 1024,
            cancelled: Some(&self.signals.cancelled),
        }
        .run(self.command(&self.git, cwd).args(args), false)
        .map_err(command_error)?;
        if !result.success {
            return Err(Error::new(
                if result.exit_code == Some(1) {
                    "gitExit1"
                } else {
                    "checkFailed"
                },
                format!("Git update command failed: {}", result.stderr),
            ));
        }
        if result.truncated {
            return Err(Error::new(
                "checkFailed",
                "Git output exceeded the supported limit.",
            ));
        }
        Ok(result.stdout)
    }
    fn build(&mut self, source: &Path, output: &mut dyn FnMut(&str)) -> Result<PathBuf, Error> {
        output("Building the native CLI, Gateway and sidecar from source.");
        let target = source.join(".update-target");
        if fs::symlink_metadata(&target).is_ok() {
            return Err(Error::new(
                "buildFailed",
                "Refusing to reuse an existing update build target.",
            ));
        }
        let mut c = self.command(&self.cargo, &source.join("src"));
        c.env("CARGO_TARGET_DIR", &target).args([
            "build",
            "--release",
            "--locked",
            "-p",
            "pilotdeck-cli",
            "-p",
            "pilotdeck-gateway",
            "-p",
            "pilotdeck-transport",
            "--bins",
        ]);
        let result = Runner {
            timeout: Duration::from_secs(3600),
            output_limit: 12000,
            cancelled: Some(&self.signals.cancelled),
        }
        .run(&mut c, self.progress)
        .map_err(command_error)?;
        if !result.success {
            return Err(Error::new(
                "buildFailed",
                format!("Source build failed: {}", result.stderr),
            ));
        }
        Ok(target.join("release"))
    }
    fn cancelled(&self) -> bool {
        self.signals
            .cancelled
            .load(std::sync::atomic::Ordering::Relaxed)
    }
}
fn command_error(message: String) -> Error {
    let reason = match message.as_str() {
        "Update command timed out." => "buildTimedOut",
        "Update was cancelled." => "cancelled",
        _ => "processStopFailed",
    };
    Error::new(reason, message)
}

struct Lock {
    path: PathBuf,
    retain: bool,
}
impl Lock {
    fn acquire(root: &Path) -> Result<Self, Error> {
        let path = root.join("update.lock");
        match mkdir(&path) {
            Ok(()) => Ok(Self {
                path,
                retain: false,
            }),
            Err(_) if path.exists() => Err(Error::refused("lockBusy")),
            Err(e) => Err(e),
        }
    }
}
impl Drop for Lock {
    fn drop(&mut self) {
        if !self.retain {
            let _ = fs::remove_dir(&self.path);
        }
    }
}
pub struct Engine<B: Builder> {
    pub installation: Installation,
    pub builder: B,
    target_generation: Option<String>,
}
impl<B: Builder> Engine<B> {
    pub fn new(installation: Installation, builder: B) -> Self {
        Self {
            installation,
            builder,
            target_generation: None,
        }
    }
    fn inspect(&mut self) -> Result<(), Error> {
        let i = &self.installation;
        i.unchanged()?;
        if i.expected_generation != i.current.generation {
            return Err(Error::refused("restartRequired"));
        }
        let source = &i.current.source_checkout;
        if !fs::symlink_metadata(source.join(".git"))
            .map(|m| m.is_dir())
            .unwrap_or(false)
        {
            return Err(Error::refused("notGit"));
        }
        if self.builder.git(source, &["rev-parse", "HEAD"])? != i.current.source_sha {
            return Err(Error::refused("runtimeMismatch"));
        }
        if !self
            .builder
            .git(source, &["status", "--porcelain", "--untracked-files=all"])?
            .is_empty()
        {
            return Err(Error::refused("localChanges"));
        }
        if self
            .builder
            .git(source, &["rev-parse", "--is-shallow-repository"])?
            == "true"
        {
            return Err(Error::refused("shallow"));
        }
        if self.builder.git(source, &["remote", "get-url", "origin"])? != i.policy.approved_remote {
            return Err(Error::refused("unofficial"));
        }
        Ok(())
    }
    fn latest(&mut self) -> Result<Status, Error> {
        self.inspect()?;
        let i = &self.installation;
        self.builder.git(
            &i.current.source_checkout,
            &[
                "fetch",
                "--no-tags",
                &i.policy.approved_remote,
                &i.policy.approved_ref,
            ],
        )?;
        let sha = self.builder.git(
            &i.current.source_checkout,
            &["rev-parse", "FETCH_HEAD^{commit}"],
        )?;
        if !valid_sha(&sha) {
            return Err(Error::new(
                "releaseMismatch",
                "The approved source ref did not identify a commit.",
            ));
        }
        let latest = Release {
            tag_name: i.policy.approved_ref.clone(),
            source_sha: sha.clone(),
        };
        if sha == i.current.source_sha {
            return Ok(Status {
                reason: Some("upToDate".into()),
                latest: Some(latest),
                can_update: false,
            });
        }
        let base = match self.builder.git(
            &i.current.source_checkout,
            &["merge-base", &i.current.source_sha, &sha],
        ) {
            Ok(base) => base,
            Err(error) if error.reason == "gitExit1" => String::new(),
            Err(error) => return Err(error),
        };
        let reason = if base == i.current.source_sha {
            None
        } else if base == sha {
            Some("ahead".into())
        } else {
            Some("diverged".into())
        };
        Ok(Status {
            can_update: reason.is_none(),
            reason,
            latest: Some(latest),
        })
    }
}
impl<B: Builder> Backend for Engine<B> {
    fn check(&mut self) -> Result<Status, Error> {
        let mut lock = Lock::acquire(&self.installation.root)?;
        let result = self.latest();
        if result
            .as_ref()
            .is_err_and(|e| e.reason == "processStopFailed")
        {
            lock.retain = true;
        }
        result
    }
    fn identity(&self) -> (&str, &str) {
        (
            &self.installation.policy.install_id,
            &self.installation.expected_generation,
        )
    }
    fn target_generation(&self) -> Option<&str> {
        self.target_generation.as_deref()
    }
    fn apply(&mut self, target: &Release, output: &mut dyn FnMut(&str)) -> Result<(), Error> {
        let mut lock = Lock::acquire(&self.installation.root)?;
        let status = match self.latest() {
            Ok(status) => status,
            Err(error) => {
                lock.retain = error.reason == "processStopFailed";
                return Err(error);
            }
        };
        if !status.can_update {
            return Err(Error::refused(
                status.reason.as_deref().unwrap_or("checkFailed"),
            ));
        }
        if status.latest.as_ref() != Some(target) {
            return Err(Error::new(
                "targetChanged",
                "The approved source ref changed. Check again before updating.",
            ));
        }
        let id = uuid::Uuid::new_v4().to_string();
        let staging = self.installation.root.join(format!(".prepare-{id}"));
        mkdir(&staging)?;
        let destination = self.installation.root.join("generations").join(&id);
        let result = (|| {
            output("Preparing the published release in a temporary directory.");
            let source = staging.join("source");
            let source_str = source
                .to_str()
                .ok_or_else(|| Error::new("applyFailed", "Installation paths must be UTF-8."))?;
            self.builder.git(
                &staging,
                &[
                    "clone",
                    "--no-hardlinks",
                    "--no-checkout",
                    "--",
                    &self.installation.policy.approved_remote,
                    source_str,
                ],
            )?;
            self.builder
                .git(&source, &["checkout", "--detach", &target.source_sha])?;
            if self.builder.git(&source, &["rev-parse", "HEAD"])? != target.source_sha {
                return Err(Error::new(
                    "releaseMismatch",
                    "Prepared checkout does not match the approved source commit.",
                ));
            }
            let artifacts = self.builder.build(&source, output)?;
            // Cargo's target path is updater-owned; remove it before checking
            // source cleanliness, rather than weakening the untracked check.
            let bins = staging.join("bin");
            mkdir(&bins)?;
            let mut hashes = BTreeMap::new();
            for name in BINARIES {
                hashes.insert(
                    name.into(),
                    copy_executable(&artifacts.join(name), &bins.join(name))?,
                );
            }
            let (library_profile, library_hashes) = if self.builder.requires_image_libraries() {
                copy_image_libraries(&artifacts, &bins)?
            } else {
                (None, BTreeMap::new())
            };
            if artifacts == source.join(".update-target/release") {
                fs::remove_dir_all(source.join(".update-target")).map_err(io)?;
            }
            if !self
                .builder
                .git(&source, &["status", "--porcelain", "--untracked-files=all"])?
                .is_empty()
            {
                return Err(Error::new(
                    "buildFailed",
                    "Build modified the release source.",
                ));
            }
            if self.builder.git(&source, &["rev-parse", "HEAD"])? != target.source_sha {
                return Err(Error::new(
                    "buildFailed",
                    "Build changed the source commit.",
                ));
            }
            let external_skills =
                match fs::symlink_metadata(self.installation.root.join("generations/skills")) {
                    Ok(_) => true,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
                    Err(error) => return Err(io(error)),
                };
            link_generation_skills(&staging, external_skills)?;
            let manifest = Generation {
                schema_version: 1,
                generation: id.clone(),
                source_sha: target.source_sha.clone(),
                source_checkout: destination.join("source"),
                bin_hashes: hashes,
                library_profile,
                library_hashes,
            };
            write_json(&staging.join("manifest.json"), &manifest)?;
            sync_dir(&bins)?;
            sync_dir(&staging)?;
            self.inspect()?;
            if self.builder.cancelled() {
                return Err(Error::new("cancelled", "Update was cancelled."));
            }
            output("Build succeeded. Applying the prepared release.");
            if self.builder.cancelled() {
                return Err(Error::new("cancelled", "Update was cancelled."));
            }
            fs::rename(&staging, &destination).map_err(io)?;
            sync_dir(&self.installation.root.join("generations"))?;
            publish(
                &self.installation.root,
                &id,
                &self.installation.current.generation,
            )?;
            self.target_generation = Some(id.clone());
            Ok(())
        })();
        if let Err(error) = &result {
            if error.reason == "processStopFailed" {
                lock.retain = true;
            } else {
                let _ = fs::remove_dir_all(&staging);
            }
        }
        // A completely prepared but unselected generation is retained after a
        // publication error; never delete a generation that might be current.
        result
    }
}

fn publish(root: &Path, target: &str, expected: &str) -> Result<(), Error> {
    let current = fs::read_link(root.join("current")).map_err(io)?;
    if current != Path::new("generations").join(expected) {
        return Err(Error::new(
            "workspaceChanged",
            "Current generation changed before publication.",
        ));
    }
    let temporary = root.join(format!(".current-{}", uuid::Uuid::new_v4()));
    symlink(Path::new("generations").join(target), &temporary).map_err(io)?;
    let result = fs::rename(&temporary, root.join("current")).map_err(io);
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
        return result;
    }
    if let Err(error) = sync_dir(root) {
        // Durability failed after pointer replacement: restore the previous
        // pointer with a second atomic rename, never unlink the live pointer.
        let rollback = root.join(format!(".rollback-{}", uuid::Uuid::new_v4()));
        if symlink(&current, &rollback)
            .and_then(|()| fs::rename(&rollback, root.join("current")))
            .is_err()
        {
            return Err(Error::new(
                "manualRecovery",
                "Installation durability and rollback could not be confirmed. Inspect current before retrying.",
            ));
        }
        if sync_dir(root).is_err() {
            return Err(Error::new(
                "manualRecovery",
                "Installation rollback durability could not be confirmed. Inspect current before retrying.",
            ));
        }
        return Err(error);
    }
    Ok(())
}

/// First installation from four already-built local executables. No prebuilt
/// downloads and no user-home inference. Root must not already exist.
pub fn bootstrap<B: Builder>(
    root: &Path,
    policy: InstallManifest,
    source: &Path,
    sha: &str,
    builder: &mut B,
) -> Result<Installation, Error> {
    bootstrap_with_external_skills(root, policy, source, sha, builder, None)
}

/// Point the shared install's default bundled skill root at an explicitly
/// selected existing directory without copying or changing its contents.
pub fn bootstrap_with_external_skills<B: Builder>(
    root: &Path,
    policy: InstallManifest,
    source: &Path,
    sha: &str,
    builder: &mut B,
    skills: Option<&Path>,
) -> Result<Installation, Error> {
    bootstrap_with_confirmation_and_skills(
        root,
        policy,
        source,
        sha,
        builder,
        skills,
        |root, generation| {
            sync_dir(
                root.parent().ok_or_else(|| {
                    Error::new("applyFailed", "Installation root needs a parent.")
                })?,
            )?;
            Installation::open(root, generation)
        },
    )
}
#[cfg(test)]
pub(super) fn bootstrap_with_confirmation<
    B: Builder,
    F: FnOnce(&Path, &str) -> Result<Installation, Error>,
>(
    root: &Path,
    policy: InstallManifest,
    source: &Path,
    sha: &str,
    builder: &mut B,
    confirm: F,
) -> Result<Installation, Error> {
    bootstrap_with_confirmation_and_skills(root, policy, source, sha, builder, None, confirm)
}
fn bootstrap_with_confirmation_and_skills<
    B: Builder,
    F: FnOnce(&Path, &str) -> Result<Installation, Error>,
>(
    root: &Path,
    policy: InstallManifest,
    source: &Path,
    sha: &str,
    builder: &mut B,
    skills: Option<&Path>,
    confirm: F,
) -> Result<Installation, Error> {
    let skills = skills
        .map(|path| {
            let path = fs::canonicalize(path)
                .map_err(|e| Error::new("invalidSkillsRoot", e.to_string()))?;
            if !path.is_dir() || path.to_str().is_none() {
                return Err(Error::new(
                    "invalidSkillsRoot",
                    "Bundled skills root must be an existing UTF-8 directory.",
                ));
            }
            Ok(path)
        })
        .transpose()?;
    if !valid_sha(sha)
        || policy.schema_version != 1
        || !valid_id(&policy.install_id)
        || !valid_policy(&policy.approved_remote, &policy.approved_ref)
    {
        return Err(Error::new(
            "unsafeInstallation",
            "Invalid source installation identity.",
        ));
    }
    if builder.git(source, &["rev-parse", "HEAD"])? != sha
        || !builder
            .git(source, &["status", "--porcelain", "--untracked-files=all"])?
            .is_empty()
    {
        return Err(Error::refused("localChanges"));
    }
    if builder.git(source, &["remote", "get-url", "origin"])? != policy.approved_remote {
        return Err(Error::refused("unofficial"));
    }
    let parent = fs::canonicalize(
        root.parent()
            .ok_or_else(|| Error::new("applyFailed", "Installation root needs a parent."))?,
    )
    .map_err(io)?;
    let final_root = parent.join(
        root.file_name()
            .ok_or_else(|| Error::new("applyFailed", "Invalid installation root."))?,
    );
    if fs::symlink_metadata(&final_root).is_ok() {
        return Err(Error::new(
            "alreadyInstalled",
            "Installation root already exists.",
        ));
    }
    let stage = parent.join(format!(".pilotdeck-install-{}", uuid::Uuid::new_v4()));
    mkdir(&stage)?;
    let id = uuid::Uuid::new_v4().to_string();
    let result = (|| {
        mkdir(&stage.join("generations"))?;
        if let Some(skills) = &skills {
            // The real binary resides under generations/<id>/bin, so its
            // second default candidate is this shared link across updates.
            symlink(skills, stage.join("generations/skills")).map_err(io)?;
        }
        let directory = stage.join("generations").join(&id);
        mkdir(&directory)?;
        mkdir(&directory.join("bin"))?;
        let checkout = directory.join("source");
        let checkout_str = checkout
            .to_str()
            .ok_or_else(|| Error::new("applyFailed", "Installation paths must be UTF-8."))?;
        let source_str = source
            .to_str()
            .ok_or_else(|| Error::new("applyFailed", "Source paths must be UTF-8."))?;
        builder.git(
            &stage,
            &[
                "clone",
                "--no-hardlinks",
                "--no-checkout",
                "--",
                source_str,
                checkout_str,
            ],
        )?;
        builder.git(
            &checkout,
            &["remote", "set-url", "origin", &policy.approved_remote],
        )?;
        builder.git(&checkout, &["checkout", "--detach", sha])?;
        if builder.git(&checkout, &["rev-parse", "HEAD"])? != sha {
            return Err(Error::new(
                "releaseMismatch",
                "Prepared checkout does not match the approved source commit.",
            ));
        }
        let artifacts = builder.build(&checkout, &mut |_| {})?;
        let mut hashes = BTreeMap::new();
        for name in BINARIES {
            hashes.insert(
                name.into(),
                copy_executable(&artifacts.join(name), &directory.join("bin").join(name))?,
            );
        }
        let (library_profile, library_hashes) = if builder.requires_image_libraries() {
            copy_image_libraries(&artifacts, &directory.join("bin"))?
        } else {
            (None, BTreeMap::new())
        };
        if artifacts == checkout.join(".update-target/release") {
            fs::remove_dir_all(checkout.join(".update-target")).map_err(io)?;
        }
        if !builder
            .git(
                &checkout,
                &["status", "--porcelain", "--untracked-files=all"],
            )?
            .is_empty()
        {
            return Err(Error::new(
                "buildFailed",
                "Build modified the release source.",
            ));
        }
        if builder.git(&checkout, &["rev-parse", "HEAD"])? != sha {
            return Err(Error::new(
                "buildFailed",
                "Build changed the source commit.",
            ));
        }
        link_generation_skills(&directory, skills.is_some())?;
        let generation = Generation {
            schema_version: 1,
            generation: id.clone(),
            source_sha: sha.into(),
            source_checkout: final_root.join("generations").join(&id).join("source"),
            bin_hashes: hashes,
            library_profile,
            library_hashes,
        };
        write_json(&directory.join("manifest.json"), &generation)?;
        write_json(&stage.join("install.json"), &policy)?;
        symlink(Path::new("generations").join(&id), stage.join("current")).map_err(io)?;
        mkdir(&stage.join("bin"))?;
        for name in BINARIES {
            symlink(
                Path::new("../current/bin").join(name),
                stage.join("bin").join(name),
            )
            .map_err(io)?;
        }
        sync_dir(&directory.join("bin"))?;
        sync_dir(&directory)?;
        sync_dir(&stage.join("generations"))?;
        sync_dir(&stage.join("bin"))?;
        sync_dir(&stage)?;
        // NOREPLACE prevents racing installers from replacing an existing
        // root, including a newly created empty directory or symbolic link.
        rustix::fs::renameat_with(
            rustix::fs::CWD,
            &stage,
            rustix::fs::CWD,
            &final_root,
            rustix::fs::RenameFlags::NOREPLACE,
        )
        .map_err(io)?;
        confirm(&final_root,&id).map_err(|error|Error::new("publicationUnconfirmed",format!("Installation may already be published at '{}', but confirmation failed: {} Do not retry or overwrite this root; inspect the installation first.",final_root.display(),error.message)))
    })();
    if result
        .as_ref()
        .is_err_and(|e| e.reason != "processStopFailed")
    {
        let _ = fs::remove_dir_all(&stage);
    }
    result
}

#[cfg(test)]
mod image_library_tests {
    use super::*;
    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            let path = std::env::temp_dir()
                .join(format!("pilotdeck-image-install-{}", uuid::Uuid::new_v4()));
            mkdir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn copied_libraries_are_manifested_and_tampering_is_rejected() {
        let scratch = Scratch::new();
        let artifacts = scratch.0.join("artifacts");
        let bins = scratch.0.join("bin");
        mkdir(&artifacts).unwrap();
        mkdir(&bins).unwrap();
        let profile = format!("pilotdeck-libs-{}", "a".repeat(64));
        fs::write(
            artifacts.join("pilotdeck-image-profile"),
            format!("{profile}\n"),
        )
        .unwrap();
        mkdir(&artifacts.join(&profile)).unwrap();
        fs::write(
            artifacts.join(&profile).join("libvips.so.42"),
            b"native fixture",
        )
        .unwrap();
        let (library_profile, library_hashes) = copy_image_libraries(&artifacts, &bins).unwrap();
        let manifest = Generation {
            schema_version: 1,
            generation: "generation".into(),
            source_sha: "0".repeat(40),
            source_checkout: scratch.0.join("source"),
            bin_hashes: BTreeMap::new(),
            library_profile,
            library_hashes,
        };
        verify_image_libraries(&bins, &manifest).unwrap();
        fs::write(bins.join(&profile).join("libvips.so.42"), b"modified").unwrap();
        assert!(verify_image_libraries(&bins, &manifest).is_err());
    }

    #[test]
    fn profile_traversal_and_legacy_missing_fields_are_checked() {
        let scratch = Scratch::new();
        let artifacts = scratch.0.join("artifacts");
        let bins = scratch.0.join("bin");
        mkdir(&artifacts).unwrap();
        mkdir(&bins).unwrap();
        fs::write(artifacts.join("pilotdeck-image-profile"), "../escape\n").unwrap();
        assert!(copy_image_libraries(&artifacts, &bins).is_err());
        let legacy = serde_json::json!({"schemaVersion":1,"generation":"old","sourceSha":"0".repeat(40),"sourceCheckout":"/old/source","binHashes":{}});
        let parsed: Generation = serde_json::from_value(legacy).unwrap();
        assert!(parsed.library_profile.is_none());
        assert!(parsed.library_hashes.is_empty());
        verify_image_libraries(&bins, &parsed).unwrap();
    }
}
