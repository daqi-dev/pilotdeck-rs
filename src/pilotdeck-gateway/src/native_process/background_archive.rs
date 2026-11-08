//! Native CLI archive: bounded descriptor IO and one writer shared by overlapping
//! project generations. The existing BackgroundTaskRuntime owns child processes.
use crate::runtime::{
    background::{
        BackgroundBashTask, NativeTaskLimits,
        native_output::{NativeOutputFile, NativeOutputStorage},
    },
    background_snapshot::{
        BackgroundTaskSnapshotStore, PersistedBackgroundTask, PersistedOutput,
        TaskSnapshotIdentity, decode_snapshot_text, encode_snapshot_tasks, persist_task,
    },
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json, stringify_json};
use rustix::fs::{AtFlags, FileType, Mode, OFlags, openat, renameat, statat, unlinkat};
use std::{
    collections::HashSet,
    fs::File,
    future::Future,
    io::{self, Read, Write},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, Mutex, Weak},
};
use tokio::sync::watch;

const SNAPSHOT_BYTES: usize = 32 * 1024 * 1024;
fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}
fn lock<T>(value: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    value.lock().unwrap_or_else(|e| e.into_inner())
}
fn id_text(id: &JsString) -> io::Result<String> {
    let id = id
        .to_utf8()
        .map_err(|_| invalid("Native task id must be UTF-8."))?;
    if uuid::Uuid::parse_str(&id).is_err()
        || uuid::Uuid::parse_str(&id).is_ok_and(|uuid| uuid.to_string() != id)
        || id.len() != 36
        || id.contains('/')
        || id.contains('\\')
    {
        return Err(invalid("Native task id must be a UUID."));
    }
    Ok(id)
}
fn regular(file: File) -> io::Result<File> {
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.nlink() != 1 {
        return Err(invalid(
            "Native archive requires a regular unlinked-alias-free file.",
        ));
    }
    Ok(file)
}
fn open_file(dir: &File, name: &str, flags: OFlags) -> io::Result<File> {
    regular(File::from(openat(
        dir,
        name,
        flags | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o600),
    )?))
}
#[derive(Clone)]
struct Owner {
    token: Weak<OwnerToken>,
    seq: u64,
}
struct OwnerToken;
#[derive(Clone)]
struct Record {
    snapshot: PersistedBackgroundTask,
    identity: TaskSnapshotIdentity,
    failure: Option<JsString>,
    owner: Option<Owner>,
}
struct State {
    records: Vec<Record>,
    revision: u64,
    written: u64,
    writing: bool,
    error: Option<String>,
}
#[derive(Clone)]
struct Status {
    written: u64,
    error: Option<String>,
}
pub(super) struct NativeArchive {
    root: File,
    output: File,
    path: PathBuf,
    process_lock: File,
    deferred_owner: Mutex<Weak<DeferredArchive>>,
    state: Mutex<State>,
    status: watch::Sender<Status>,
}
pub(super) struct ArchiveHandle {
    archive: Arc<NativeArchive>,
    token: Arc<OwnerToken>,
    readonly: Mutex<HashSet<String>>,
}
impl NativeArchive {
    #[cfg(test)]
    pub(super) fn open(project: &Path) -> io::Result<Arc<Self>> {
        // The caller selected this project root. Resolve its trusted aliases
        // (including Darwin /var -> /private/var) before pinning the directory.
        // Only descendants of that root are archive-controlled components;
        // their NOFOLLOW checks remain unchanged.
        let project = project.canonicalize()?;
        let selected = crate::project_files::open_directory_beneath(&project, &project)?;
        Self::open_selected(&project, &selected)
    }
    fn open_selected(project: &Path, selected: &File) -> io::Result<Arc<Self>> {
        let path = project.join(".pilotdeck/background-tasks");
        crate::project_files::create_directories_at(selected, Path::new(".pilotdeck/background-tasks/output"), Mode::from_raw_mode(0o700))?;
        let pilot = archive_directory_at(selected, ".pilotdeck")?;
        let root = archive_directory_at(&pilot, "background-tasks")?;
        let output = archive_directory_at(&root, "output")?;
        let process_lock = open_file(&root, ".lock", OFlags::RDWR | OFlags::CREATE)?;
        rustix::fs::flock(
            &process_lock,
            rustix::fs::FlockOperation::NonBlockingLockExclusive,
        )
        .map_err(|_| {
            io::Error::new(
                io::ErrorKind::WouldBlock,
                "Another process owns this project's background archive.",
            )
        })?;
        let records = match open_file(&root, "state.json", OFlags::RDONLY) {
            Ok(file) => {
                if file.metadata()?.len() > SNAPSHOT_BYTES as u64 {
                    return Err(invalid(format!(
                        "Native background snapshot exceeds {} MiB.",
                        SNAPSHOT_BYTES / (1024 * 1024)
                    )));
                }
                let mut bytes = Vec::new();
                file.take(SNAPSHOT_BYTES as u64 + 1)
                    .read_to_end(&mut bytes)?;
                if bytes.len() > SNAPSHOT_BYTES {
                    return Err(invalid(format!(
                        "Native background snapshot grew beyond {} MiB.",
                        SNAPSHOT_BYTES / (1024 * 1024)
                    )));
                }
                decode_records(
                    std::str::from_utf8(&bytes)
                        .map_err(|_| invalid("Native background snapshot is not UTF-8."))?,
                    &path.join("state.json"),
                )?
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => vec![],
            Err(error) => return Err(error),
        };
        let (status, _) = watch::channel(Status {
            written: 0,
            error: None,
        });
        Ok(Arc::new(Self {
            root,
            output,
            path,
            process_lock,
            deferred_owner: Mutex::new(Weak::new()),
            state: Mutex::new(State {
                records,
                revision: 0,
                written: 0,
                writing: false,
                error: None,
            }),
            status,
        }))
    }
    pub(super) fn handle(self: &Arc<Self>) -> Arc<ArchiveHandle> {
        Arc::new(ArchiveHandle {
            archive: self.clone(),
            token: Arc::new(OwnerToken),
            readonly: Mutex::new(HashSet::new()),
        })
    }
    fn failed(&self, message: String) {
        let mut state = lock(&self.state);
        if state.error.is_none() {
            state.error = Some(message);
        }
        self.status.send_replace(Status {
            written: state.written,
            error: state.error.clone(),
        });
    }
    fn update(
        self: &Arc<Self>,
        handle: &ArchiveHandle,
        mut record: Record,
        seq: u64,
    ) -> io::Result<u64> {
        self.verify_owner_file()?;
        validate_record(&record)?;
        let id = record.snapshot.task.task_id.clone();
        // A generation's stale Unknown copy must never replace a still-live
        // owner's final record, even after that owner has released its token.
        if lock(&handle.readonly).contains(&id) {
            return Ok(lock(&self.state).written);
        }
        let mut state = lock(&self.state);
        if let Some(error) = &state.error {
            return Err(io::Error::other(error.clone()));
        }
        if let Some(index) = state
            .records
            .iter()
            .position(|r| r.snapshot.task.task_id == id)
        {
            let current = &state.records[index];
            if let Some(owner) = &current.owner
                && let Some(token) = owner.token.upgrade()
            {
                if !Arc::ptr_eq(&token, &handle.token) {
                    return Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "Another generation owns this background task.",
                    ));
                }
                if seq < owner.seq {
                    return Ok(state.written);
                }
            }
            record.owner = Some(Owner {
                token: Arc::downgrade(&handle.token),
                seq,
            });
            state.records[index] = record;
        } else {
            if state.records.len() >= NativeTaskLimits::CLI.retained_tasks {
                return Err(invalid(format!(
                    "Native background archive retained-task limit ({}) reached; old records were kept.",
                    NativeTaskLimits::CLI.retained_tasks
                )));
            }
            record.owner = Some(Owner {
                token: Arc::downgrade(&handle.token),
                seq,
            });
            state.records.push(record);
        }
        state.revision = state
            .revision
            .checked_add(1)
            .ok_or_else(|| invalid("Native background archive revision exhausted."))?;
        let revision = state.revision;
        let start_writer = !state.writing;
        if start_writer {
            state.writing = true;
        }
        drop(state);
        if start_writer {
            // Guard is constructed before spawn, outside the state lock, so
            // cancellation or a spawn panic cannot deadlock its failure path.
            let guard = WriterGuard {
                archive: self.clone(),
                armed: true,
                _deferred_owner: lock(&self.deferred_owner).upgrade(),
            };
            tokio::spawn(guard.run());
        }
        Ok(revision)
    }
    async fn pump(self: Arc<Self>) {
        loop {
            let (revision, records) = {
                let state = lock(&self.state);
                (state.revision, state.records.clone())
            };
            let archive = self.clone();
            let result =
                tokio::task::spawn_blocking(move || archive.replace(&encode_records(&records)?))
                    .await
                    .unwrap_or_else(|error| {
                        Err(io::Error::other(format!(
                            "Native archive writer failed: {error}"
                        )))
                    });
            let mut state = lock(&self.state);
            if let Err(error) = result {
                state.error.get_or_insert_with(|| error.to_string());
                state.writing = false;
                self.status.send_replace(Status {
                    written: state.written,
                    error: state.error.clone(),
                });
                return;
            }
            state.written = revision;
            self.status.send_replace(Status {
                written: revision,
                error: state.error.clone(),
            });
            if state.revision == revision {
                state.writing = false;
                return;
            }
        }
    }
    fn verify_owner_file(&self) -> io::Result<()> {
        if !same_entry(&self.root, ".lock", &self.process_lock)? {
            return Err(io::Error::other(
                "Native archive owner lock was replaced or unlinked.",
            ));
        }
        Ok(())
    }
    fn replace(&self, bytes: &[u8]) -> io::Result<()> {
        self.verify_owner_file()?;
        if bytes.len() > SNAPSHOT_BYTES {
            return Err(invalid(format!(
                "Native background snapshot exceeds {} MiB.",
                SNAPSHOT_BYTES / (1024 * 1024)
            )));
        }
        let temporary = format!("state.{}.tmp", uuid::Uuid::new_v4());
        let result = (|| {
            let mut file = open_file(
                &self.root,
                &temporary,
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL,
            )?;
            file.write_all(bytes)?;
            file.sync_all()?;
            self.verify_owner_file()?;
            renameat(&self.root, temporary.as_str(), &self.root, "state.json")?;
            self.root.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = unlinkat(&self.root, temporary.as_str(), AtFlags::empty());
        }
        result
    }
    fn wait(
        self: &Arc<Self>,
        revision: u64,
    ) -> Pin<Box<dyn Future<Output = io::Result<()>> + Send + 'static>> {
        let mut status = self.status.subscribe();
        Box::pin(async move {
            loop {
                let current = status.borrow_and_update().clone();
                if let Some(error) = current.error {
                    return Err(io::Error::other(error));
                }
                if current.written >= revision {
                    return Ok(());
                }
                status.changed().await.map_err(|_| {
                    io::Error::other("Native archive writer closed before persistence completed.")
                })?;
            }
        })
    }
}
struct WriterGuard {
    archive: Arc<NativeArchive>,
    armed: bool,
    // Only an in-flight writer holds this strong retain. NativeArchive's
    // back-reference is weak, so no idle archive/deferred-owner cycle forms.
    _deferred_owner: Option<Arc<DeferredArchive>>,
}
impl WriterGuard {
    // An async method taking the whole guard owns it before its first poll.
    // Cancellation/unwind drops it; only normal pump return disarms it.
    async fn run(mut self) {
        self.archive.clone().pump().await;
        self.disarm();
    }
    fn disarm(&mut self) {
        self.armed = false;
    }
}
impl Drop for WriterGuard {
    fn drop(&mut self) {
        if self.armed {
            self.archive.failed(
                "Native archive writer was cancelled or panicked before persistence completed."
                    .to_owned(),
            );
        }
    }
}
impl ArchiveHandle {
    fn write_task_pending(
        &self,
        task: BackgroundBashTask,
        output: PersistedOutput,
        seq: u64,
        failure_reason: Option<JsString>,
    ) -> Pin<Box<dyn Future<Output = io::Result<()>> + Send + 'static>> {
        let record = Record {
            snapshot: PersistedBackgroundTask {
                task: persist_task(&task),
                output,
            },
            identity: TaskSnapshotIdentity::of(&task),
            failure: failure_reason,
            owner: None,
        };
        match self.archive.update(self, record, seq) {
            Ok(revision) => self.archive.wait(revision),
            Err(error) => Box::pin(async move { Err(error) }),
        }
    }
    fn write_pending(
        &self,
        snapshot: PersistedBackgroundTask,
        seq: u64,
    ) -> Pin<Box<dyn Future<Output = io::Result<()>> + Send + 'static>> {
        let identity = identity_from_snapshot(&snapshot);
        match self.archive.update(
            self,
            Record {
                snapshot,
                identity,
                failure: None,
                owner: None,
            },
            seq,
        ) {
            Ok(revision) => self.archive.wait(revision),
            Err(error) => Box::pin(async move { Err(error) }),
        }
    }
    fn flush_pending(&self) -> Pin<Box<dyn Future<Output = io::Result<()>> + Send + 'static>> {
        let revision = lock(&self.archive.state).revision;
        self.archive.wait(revision)
    }
}
impl BackgroundTaskSnapshotStore for ArchiveHandle {
    fn load(&self) -> Vec<PersistedBackgroundTask> {
        let mut readonly = lock(&self.readonly);
        let mut state = lock(&self.archive.state);
        for record in &mut state.records {
            let other = record
                .owner
                .as_ref()
                .and_then(|owner| owner.token.upgrade())
                .is_some_and(|token| !Arc::ptr_eq(&token, &self.token));
            if other {
                readonly.insert(record.snapshot.task.task_id.clone());
            } else {
                record.owner = Some(Owner {
                    token: Arc::downgrade(&self.token),
                    seq: 0,
                });
            }
        }
        state
            .records
            .iter()
            .map(|record| record.snapshot.clone())
            .collect()
    }
    fn original_fields(&self, task_id: &str) -> Option<TaskSnapshotIdentity> {
        lock(&self.archive.state)
            .records
            .iter()
            .find(|r| r.snapshot.task.task_id == task_id)
            .map(|r| r.identity.clone())
    }
    fn failure_reason(&self, task_id: &str) -> Option<JsString> {
        lock(&self.archive.state)
            .records
            .iter()
            .find(|r| r.snapshot.task.task_id == task_id)
            .and_then(|r| r.failure.clone())
    }
    fn report_failure(&self, message: String) {
        self.archive.failed(message);
    }
    fn write_task(
        &self,
        task: BackgroundBashTask,
        output: PersistedOutput,
        seq: u64,
        failure_reason: Option<JsString>,
    ) -> Pin<Box<dyn Future<Output = io::Result<()>> + Send + '_>> {
        self.write_task_pending(task, output, seq, failure_reason)
    }
    fn write(
        &self,
        snapshot: PersistedBackgroundTask,
        seq: u64,
    ) -> Pin<Box<dyn Future<Output = io::Result<()>> + Send + '_>> {
        self.write_pending(snapshot, seq)
    }
    fn flush(&self) -> Pin<Box<dyn Future<Output = io::Result<()>> + Send + '_>> {
        self.flush_pending()
    }
}
#[allow(clippy::unnecessary_cast)] // stat device/inode types vary between Darwin and Linux.
fn same_entry(directory: &File, name: &str, file: &File) -> io::Result<bool> {
    let expected = file.metadata()?;
    Ok(
        statat(directory, name, AtFlags::SYMLINK_NOFOLLOW).is_ok_and(|stat| {
            FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile
                && stat.st_nlink == 1
                && expected.nlink() == 1
                && stat.st_dev as u64 == expected.dev()
                && stat.st_ino as u64 == expected.ino()
        }),
    )
}
fn output_link_check(
    directory: &File,
    name: &str,
    file: &File,
) -> io::Result<Arc<dyn Fn() -> bool + Send + Sync>> {
    let directory = directory.try_clone()?;
    let name = name.to_owned();
    let file = file.try_clone()?;
    Ok(Arc::new(move || {
        same_entry(&directory, &name, &file).unwrap_or(false)
    }))
}
impl NativeOutputStorage for ArchiveHandle {
    fn create(&self, task_id: &JsString) -> io::Result<NativeOutputFile> {
        let name = format!("{}.log", id_text(task_id)?);
        let file = open_file(
            &self.archive.output,
            &name,
            OFlags::RDWR | OFlags::APPEND | OFlags::CREATE | OFlags::EXCL,
        )?;
        let link_check = output_link_check(&self.archive.output, &name, &file)?;
        Ok(NativeOutputFile {
            file,
            path: self.archive.path.join("output").join(name),
            allow_prefix: false,
            link_check,
        })
    }
    fn restore(
        &self,
        task_id: &JsString,
        expected_bytes: u64,
    ) -> io::Result<Option<NativeOutputFile>> {
        if expected_bytes > NativeTaskLimits::CLI.output_bytes as u64 {
            return Err(invalid(format!(
                "Native task output exceeds its {} MiB limit.",
                NativeTaskLimits::CLI.output_bytes / (1024 * 1024)
            )));
        }
        let id = id_text(task_id)?;
        let name = format!("{id}.log");
        match open_file(&self.archive.output, &name, OFlags::RDONLY) {
            Ok(file) => {
                if file.metadata()?.len() > NativeTaskLimits::CLI.output_bytes as u64 {
                    return Err(invalid(format!(
                        "Native output file exceeds {} MiB.",
                        NativeTaskLimits::CLI.output_bytes / (1024 * 1024)
                    )));
                }
                let link_check = output_link_check(&self.archive.output, &name, &file)?;
                Ok(Some(NativeOutputFile {
                    file,
                    path: self.archive.path.join("output").join(name),
                    allow_prefix: lock(&self.readonly).contains(&id),
                    link_check,
                }))
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound && expected_bytes == 0 => {
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }
}

fn archive_directory_at(parent: &File, name: &str) -> io::Result<File> {
    #[cfg(target_os = "linux")]
    let access = OFlags::PATH;
    #[cfg(target_vendor = "apple")]
    let access = OFlags::from_bits_retain(libc::O_SEARCH as u32);
    #[cfg(not(any(target_os = "linux", target_vendor = "apple")))]
    let access = OFlags::RDONLY;
    Ok(File::from(openat(parent, name, access | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW, Mode::empty())?))
}

struct DeferredState {
    archive: Option<Arc<NativeArchive>>,
    error: Option<String>,
}
/// The family keeps this owner in its existing per-project Weak map. A
/// missing archive owns only the selected project fd, never a mkdir/flock.
pub(super) struct DeferredArchive {
    project: PathBuf,
    selected: File,
    state: Mutex<DeferredState>,
}
pub(super) struct DeferredHandle {
    shared: Arc<DeferredArchive>,
    resolved: Mutex<Option<Arc<ArchiveHandle>>>,
}
impl DeferredArchive {
    pub(super) fn new(project: &Path) -> io::Result<Arc<Self>> {
        let project = project.canonicalize()?;
        let selected = crate::project_files::open_directory_beneath(&project, &project)?;
        Ok(Arc::new(Self { project, selected, state: Mutex::new(DeferredState { archive: None, error: None }) }))
    }
    pub(super) fn prepare_existing(self: &Arc<Self>) -> io::Result<()> {
        let mut state = lock(&self.state);
        if let Some(error) = &state.error { return Err(io::Error::other(error.clone())); }
        if state.archive.is_some() { return Ok(()); }
        // Probe relative to the pinned selected project, following no archive
        // component. Only a genuinely missing directory remains dormant.
        let pilot = match archive_directory_at(&self.selected, ".pilotdeck") {
            Ok(directory) => directory,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        match archive_directory_at(&pilot, "background-tasks") {
            Ok(_) => {},
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        }
        // The existing path retains the old lock/read/restore contract.
        let archive = NativeArchive::open_selected(&self.project, &self.selected)?;
        *lock(&archive.deferred_owner) = Arc::downgrade(self);
        state.archive = Some(archive);
        Ok(())
    }
    fn for_write(self: &Arc<Self>) -> io::Result<Arc<NativeArchive>> {
        let mut state = lock(&self.state);
        if let Some(error) = &state.error { return Err(io::Error::other(error.clone())); }
        if let Some(archive) = &state.archive { return Ok(archive.clone()); }
        // Open/flock/recovery completes synchronously before any metadata is
        // queued. A newly created foreign archive cannot be bypassed.
        let archive = NativeArchive::open_selected(&self.project, &self.selected)?;
        *lock(&archive.deferred_owner) = Arc::downgrade(self);
        state.archive = Some(archive.clone());
        Ok(archive)
    }
    pub(super) fn handle(self: &Arc<Self>) -> Arc<DeferredHandle> {
        let initial = lock(&self.state).archive.as_ref().map(|archive| archive.handle());
        Arc::new(DeferredHandle { shared: self.clone(), resolved: Mutex::new(initial) })
    }
    fn failed(&self, message: String) {
        let mut state = lock(&self.state);
        if let Some(archive) = &state.archive {
            let archive = archive.clone();drop(state);archive.failed(message);
        } else if state.error.is_none() { state.error = Some(message); }
    }
}
impl DeferredHandle {
    fn existing(&self) -> Option<Arc<ArchiveHandle>> {
        let mut resolved = lock(&self.resolved);
        if resolved.is_none() {
            // Another live generation may have activated the shared owner.
            *resolved = lock(&self.shared.state).archive.as_ref().map(|archive| archive.handle());
        }
        resolved.clone()
    }
    fn for_write(&self) -> io::Result<Arc<ArchiveHandle>> {
        let mut resolved = lock(&self.resolved);
        if let Some(handle) = &*resolved { return Ok(handle.clone()); }
        let handle = self.shared.for_write()?.handle();
        *resolved = Some(handle.clone());
        Ok(handle)
    }
}
impl BackgroundTaskSnapshotStore for DeferredHandle {
    fn load(&self) -> Vec<PersistedBackgroundTask> {
        // prepare_existing already propagated all existing-archive errors at
        // stage. An absent archive linearizes as empty here without writes.
        self.existing().map(|handle| handle.load()).unwrap_or_default()
    }
    fn original_fields(&self, task_id: &str) -> Option<TaskSnapshotIdentity> {
        self.existing().and_then(|handle| handle.original_fields(task_id))
    }
    fn failure_reason(&self, task_id: &str) -> Option<JsString> {
        self.existing().and_then(|handle| handle.failure_reason(task_id))
    }
    fn report_failure(&self, message: String) { self.shared.failed(message); }
    fn write_task(&self, task: BackgroundBashTask, output: PersistedOutput, seq: u64, failure_reason: Option<JsString>) -> Pin<Box<dyn Future<Output = io::Result<()>> + Send + '_>> {
        match self.for_write() {
            Ok(handle) => handle.write_task_pending(task, output, seq, failure_reason),
            Err(error) => Box::pin(async move { Err(error) }),
        }
    }
    fn write(&self, snapshot: PersistedBackgroundTask, seq: u64) -> Pin<Box<dyn Future<Output = io::Result<()>> + Send + '_>> {
        match self.for_write() {
            Ok(handle) => handle.write_pending(snapshot, seq),
            Err(error) => Box::pin(async move { Err(error) }),
        }
    }
    fn flush(&self) -> Pin<Box<dyn Future<Output = io::Result<()>> + Send + '_>> {
        if let Some(handle) = self.existing() { return handle.flush_pending(); }
        let error = lock(&self.shared.state).error.clone();
        Box::pin(async move { error.map(io::Error::other).map_or(Ok(()), Err) })
    }
}
impl NativeOutputStorage for DeferredHandle {
    fn create(&self, task_id: &JsString) -> io::Result<NativeOutputFile> {
        self.for_write()?.create(task_id)
    }
    fn restore(&self, task_id: &JsString, expected_bytes: u64) -> io::Result<Option<NativeOutputFile>> {
        if let Some(handle) = self.existing() { return handle.restore(task_id, expected_bytes); }
        if expected_bytes > NativeTaskLimits::CLI.output_bytes as u64 {
            return Err(invalid(format!("Native task output exceeds its {} MiB limit.", NativeTaskLimits::CLI.output_bytes / (1024 * 1024))));
        }
        id_text(task_id)?;
        if expected_bytes == 0 { Ok(None) } else { Err(io::Error::new(io::ErrorKind::NotFound, "Native background archive is not initialized.")) }
    }
}

fn identity_from_snapshot(snapshot: &PersistedBackgroundTask) -> TaskSnapshotIdentity {
    let task = &snapshot.task;
    TaskSnapshotIdentity {
        command: JsValue::String(task.command.as_str().into()),
        cwd: JsValue::String(task.cwd.as_str().into()),
        session_id: task
            .session_id
            .as_ref()
            .map_or(JsValue::Undefined, |s| JsValue::String(s.as_str().into())),
        agent_id: task
            .agent_id
            .as_ref()
            .map_or(JsValue::Undefined, |s| JsValue::String(s.as_str().into())),
        kind: JsValue::String(task.kind.as_str().into()),
    }
}
fn validate_record(record: &Record) -> io::Result<()> {
    let snapshot = &record.snapshot;
    if record
        .failure
        .as_ref()
        .is_some_and(|reason| reason.units().len() > 512)
    {
        return Err(invalid("Native failure reason exceeds its limit."));
    }
    id_text(&snapshot.task.task_id.as_str().into())?;
    native_date(&snapshot.task.started_at)?;
    if let Some(date) = &snapshot.task.ended_at {
        native_date(date)?;
    }
    let mut task = crate::runtime::background_snapshot::restore_task(&snapshot.task)?;
    record.identity.apply(&mut task);
    let cwd = task
        .cwd
        .as_string()
        .cloned()
        .ok_or_else(|| invalid("Native task cwd must be a string."))?;
    let spec = crate::runtime::background::StartTaskSpec {
        command: task.command,
        cwd,
        env: None,
        session_id: task.session_id,
        agent_id: task.agent_id,
        kind: task.kind,
    };
    NativeTaskLimits::CLI
        .validate_spec(&spec)
        .map_err(|error| invalid(error.message.to_utf8().unwrap_or_default()))?;
    if snapshot.output.total_bytes > NativeTaskLimits::CLI.output_bytes as u64 {
        return Err(invalid(format!(
            "Native task output exceeds {} MiB.",
            NativeTaskLimits::CLI.output_bytes / (1024 * 1024)
        )));
    }
    Ok(())
}
// Validate the bounded native schema before the legacy date parser can loop
// across an attacker-controlled unbounded year. Normal Date.toISOString output
// produced by the native clock has this exact 24-byte shape.
fn native_date(value: &str) -> io::Result<()> {
    let bytes = value.as_bytes();
    if bytes.len() != 24
        || !bytes[..4].iter().all(u8::is_ascii_digit)
        || &bytes[4..5] != b"-"
        || &bytes[10..11] != b"T"
        || &bytes[23..] != b"Z"
    {
        return Err(invalid(
            "Native task timestamp must be a bounded UTC ISO timestamp.",
        ));
    }
    Ok(())
}
fn depth_preflight(raw: &str) -> io::Result<()> {
    let mut quoted = false;
    let mut escaped = false;
    let mut depth = 0usize;
    for byte in raw.bytes() {
        if quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
        } else {
            match byte {
                b'"' => quoted = true,
                b'{' | b'[' => {
                    depth += 1;
                    if depth > 8 {
                        return Err(invalid("Native task snapshot nesting exceeds its schema."));
                    }
                }
                b'}' | b']' => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
    }
    Ok(())
}
fn decode_records(raw: &str, path: &Path) -> io::Result<Vec<Record>> {
    depth_preflight(raw)?;
    let root = parse_json(raw).map_err(|e| invalid(e.to_string()))?;
    let tasks = match root.get("tasks") {
        Some(JsValue::Array(tasks)) => tasks,
        _ => return Err(invalid("Native background snapshot needs a tasks array.")),
    };
    if tasks.len() > NativeTaskLimits::CLI.retained_tasks {
        return Err(invalid(format!(
            "Native snapshot exceeds its retained-task limit ({}); old records were kept.",
            NativeTaskLimits::CLI.retained_tasks
        )));
    }
    let mut identities = Vec::new();
    let mut failures = Vec::new();
    // A sanitized copy goes only to the existing schema validator. Original
    // JS fields remain in identities and are re-applied by the runtime/store.
    let mut safe_tasks = Vec::new();
    for entry in tasks {
        let task = entry
            .get("task")
            .and_then(JsValue::as_object)
            .ok_or_else(|| invalid("Native task record has no task object."))?;
        let get = |key: &str| task.get(key).cloned().unwrap_or(JsValue::Undefined);
        let identity = TaskSnapshotIdentity {
            command: get("command"),
            cwd: get("cwd"),
            session_id: get("sessionId"),
            agent_id: get("agentId"),
            kind: get("kind"),
        };
        for key in ["startedAt", "endedAt"] {
            if let Some(JsValue::String(value)) = task.get(key) {
                native_date(&String::from_utf16_lossy(value.units()))?;
            }
        }
        identities.push(identity);
        failures.push(match entry.get("nativeFailureReason") {
            None | Some(JsValue::Null) => None,
            Some(JsValue::String(reason)) => Some(reason.clone()),
            _ => return Err(invalid("Native failure reason must be a string.")),
        });
        safe_tasks.push(sanitize(entry));
    }
    let mut safe = root
        .as_object()
        .cloned()
        .ok_or_else(|| invalid("Native snapshot is not an object."))?;
    safe.insert("tasks".into(), JsValue::Array(safe_tasks));
    let safe = stringify_json(&JsValue::Object(safe))
        .map_err(|e| invalid(e.to_string()))?
        .ok_or_else(|| invalid("Native snapshot is undefined."))?;
    let decoded = decode_snapshot_text(&safe, path)?;
    let records: Vec<_> = decoded
        .into_iter()
        .zip(identities)
        .zip(failures)
        .map(|((snapshot, identity), failure)| Record {
            snapshot,
            identity,
            failure,
            owner: None,
        })
        .collect();
    for record in &records {
        validate_record(record)?;
    }
    Ok(records)
}
fn sanitize(value: &JsValue) -> JsValue {
    match value {
        JsValue::String(value) => JsValue::String(String::from_utf16_lossy(value.units()).into()),
        JsValue::Array(values) => JsValue::Array(values.iter().map(sanitize).collect()),
        JsValue::Object(values) => {
            let mut object = JsObject::new();
            for (key, value) in values.own_entries() {
                object.insert(
                    String::from_utf16_lossy(key.units()).into(),
                    sanitize(value),
                );
            }
            JsValue::Object(object)
        }
        _ => value.clone(),
    }
}
fn encode_records(records: &[Record]) -> io::Result<Vec<u8>> {
    let snapshots: Vec<_> = records.iter().map(|r| r.snapshot.clone()).collect();
    let raw = encode_snapshot_tasks(&snapshots)?;
    let value = parse_json(&raw).map_err(|e| invalid(e.to_string()))?;
    let mut root = value
        .as_object()
        .cloned()
        .ok_or_else(|| invalid("Snapshot encoder did not produce an object."))?;
    let tasks = match root.get("tasks") {
        Some(JsValue::Array(tasks)) => tasks,
        _ => return Err(invalid("Snapshot encoder did not produce tasks.")),
    };
    let mut original_tasks = Vec::new();
    for (entry, record) in tasks.iter().zip(records) {
        let mut entry = entry
            .as_object()
            .cloned()
            .ok_or_else(|| invalid("Snapshot entry is not an object."))?;
        let mut task = entry
            .get("task")
            .and_then(JsValue::as_object)
            .cloned()
            .ok_or_else(|| invalid("Snapshot task is not an object."))?;
        for (key, value) in [
            ("command", &record.identity.command),
            ("cwd", &record.identity.cwd),
            ("sessionId", &record.identity.session_id),
            ("agentId", &record.identity.agent_id),
            ("kind", &record.identity.kind),
        ] {
            task.insert(key.into(), value.clone());
        }
        entry.insert("task".into(), JsValue::Object(task));
        if let Some(reason) = &record.failure {
            entry.insert(
                "nativeFailureReason".into(),
                JsValue::String(reason.clone()),
            );
        }
        original_tasks.push(JsValue::Object(entry));
    }
    root.insert("tasks".into(), JsValue::Array(original_tasks));
    let encoded = stringify_json(&JsValue::Object(root))
        .map_err(|e| invalid(e.to_string()))?
        .ok_or_else(|| invalid("Snapshot encoder returned undefined."))?
        .into_bytes();
    if encoded.len() > SNAPSHOT_BYTES {
        return Err(invalid(format!(
            "Native snapshot exceeds {} MiB.",
            SNAPSHOT_BYTES / (1024 * 1024)
        )));
    }
    Ok(encoded)
}

#[cfg(test)]
#[path = "background_archive_tests.rs"]
mod tests;
